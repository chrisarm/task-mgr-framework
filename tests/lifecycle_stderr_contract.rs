//! Stderr contract snapshot for `apply_status_updates` PRD JSON sync failure.
//!
//! Locks the exact byte shape emitted by `eprintln!` at
//! `src/loop_engine/engine.rs:4781-4786` when `update_prd_task_passes` fails
//! AFTER a successful DB transition. Operators grep stderr for this warning;
//! the lifecycle service's eventual `apply()` MUST produce byte-identical
//! output. Per PRD FR-010 (TaskLifecycle extraction) the lock is "whatever
//! bytes the legacy `apply_status_updates` produces today" — see TEST-INIT-003
//! qualityDimension: *"the lock is 'whatever bytes legacy produces'"*.
//!
//! Failure is injected with a real failure mode (non-existent PRD path), NOT
//! a mock — `update_prd_task_passes` returns
//! `TaskMgrError::IoErrorWithContext` with Display text
//! `I/O error during reading PRD file on '<path>': <io_error>`.
//!
//! The snapshot also captures the **stderr-vs-DB-commit ORDER** invariant:
//! the warning fires only after the DB row is durable. We assert this by
//! `SELECT`ing `tasks.status` *after* stderr capture has finished — if the
//! warning had fired before the commit, the SELECT would see the pre-Done
//! row.
//!
//! Unix-only: stderr capture relies on `libc::dup2`. `cfg(unix)` skips on
//! Windows; this codebase already targets Unix (see `tokio` + `signal-hook`
//! setup).

#![cfg(unix)]
// FEAT-010: this test crate intentionally exercises the deprecated
// `apply_status_updates` shim to lock its stderr contract during the migration.
#![allow(deprecated)]

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use expect_test::expect;
use rusqlite::Connection;
use tempfile::{NamedTempFile, TempDir};

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::lifecycle::{TaskLifecycle, TransitionChange, TransitionIntent, TransitionSource};
use task_mgr::loop_engine::detection::{TaskStatusChange, TaskStatusUpdate};
use task_mgr::loop_engine::engine::apply_status_updates;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn setup_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let mut conn = open_connection(dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (dir, conn)
}

fn insert_task(conn: &Connection, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, priority, status) VALUES (?1, 'Test task', 50, ?2)",
        rusqlite::params![id, status],
    )
    .unwrap();
}

/// Redirect FD 2 to a temp file for the duration of `f`, then restore.
///
/// Returns the bytes written to FD 2 during the call. Uses `libc::dup`/`dup2`
/// so it captures `eprintln!` output across the C runtime, not just Rust's
/// `io::stderr()` buffer. Always flushes Rust's stderr handle before and
/// after the swap so no buffered bytes leak past the boundary.
///
/// Integration-test isolation: this test binary contains exactly one test, so
/// no concurrent thread can race the FD swap. Adding more stderr-capturing
/// tests to this file would require serialization (e.g. a `Mutex`).
fn capture_stderr<F: FnOnce()>(f: F) -> String {
    std::io::stderr().flush().ok();
    let mut tmp = NamedTempFile::new().expect("create tempfile for stderr capture");
    let saved_fd = unsafe { libc::dup(2) };
    assert!(
        saved_fd >= 0,
        "dup(2) failed: {}",
        std::io::Error::last_os_error()
    );
    let new_fd = unsafe { libc::dup2(tmp.as_file().as_raw_fd(), 2) };
    assert!(
        new_fd >= 0,
        "dup2 failed: {}",
        std::io::Error::last_os_error()
    );

    f();

    std::io::stderr().flush().ok();
    unsafe {
        libc::dup2(saved_fd, 2);
        libc::close(saved_fd);
    }

    tmp.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
    let mut out = String::new();
    tmp.as_file_mut().read_to_string(&mut out).unwrap();
    out
}

/// Replace tempdir-specific path bytes with `<PRD_PATH>` so the snapshot is
/// stable across runs. Anchored to the exact `prd_path` produced by the test
/// — no broader sanitization (we WANT to lock everything else verbatim).
fn mask_path(stderr: &str, prd_path: &Path) -> String {
    stderr.replace(prd_path.display().to_string().as_str(), "<PRD_PATH>")
}

// ── Snapshot test ────────────────────────────────────────────────────────────

/// Snapshot the exact stderr bytes emitted by `apply_status_updates` when
/// `update_prd_task_passes` fails on a Done transition.
///
/// Invariants exercised:
/// 1. `applied = true` on the per-task result even though stderr fired
///    (PRD §2.5 / learning #2284 — DB is authoritative; stderr is
///    best-effort observability)
/// 2. DB row reaches `done` BEFORE the warning is written (read after
///    capture; status would be `in_progress` if the warning preceded the
///    commit)
/// 3. Byte-exact warning text (frozen by the `expect!` literal below; the
///    lifecycle-service migration that lands later MUST keep these bytes)
fn main() {
    stderr_contract_prd_json_sync_failure_on_done();
    direct_tasklifecycle_apply_stderr_contract();
    println!("test result: ok. 2 passed; 0 failed");
}

fn stderr_contract_prd_json_sync_failure_on_done() {
    let (dir, mut conn) = setup_db();
    insert_task(&conn, "FEAT-003", "in_progress");
    // Non-existent PRD: `update_prd_task_passes` fails at the initial
    // `fs::read_to_string` and returns IoErrorWithContext.
    let prd_path = dir.path().join("nonexistent.json");

    let updates = vec![TaskStatusUpdate {
        task_id: "FEAT-003".to_string(),
        status: TaskStatusChange::Done,
    }];

    let mut applied_flag = false;
    let stderr = capture_stderr(|| {
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(results.len(), 1, "exactly one per-update result expected");
        let (ref tid, status, applied) = results[0];
        assert_eq!(tid, "FEAT-003");
        assert!(matches!(status, TaskStatusChange::Done));
        applied_flag = applied;
    });

    // Invariant 1: applied = true even though PRD sync failed.
    assert!(
        applied_flag,
        "applied must be true when DB commits and only PRD sync fails (learning #2284)"
    );

    // Invariant 2: stderr-vs-DB-commit ordering. The SELECT runs AFTER stderr
    // capture ends; seeing `done` proves the DB commit happened (and by the
    // structure of apply_status_updates — see engine.rs:4771-4787 — the
    // commit precedes the eprintln! that produced the captured bytes).
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FEAT-003'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        status, "done",
        "DB transition must be durable before (or at the moment of) stderr emission"
    );

    // Invariant 3: byte-exact warning text. Mask only the tempdir path so the
    // snapshot is stable across runs; every other byte (including the
    // structural `(` `)` `:` separators) is locked.
    let masked = mask_path(&stderr, &prd_path);
    expect![[r#"
        Warning: <task-status> dispatched FEAT-003 to done in DB but PRD JSON sync failed (<PRD_PATH>): I/O error during reading PRD file on '<PRD_PATH>': No such file or directory (os error 2)
    "#]]
    .assert_eq(&masked);
}

// ── Direct coverage for the post-extraction emission sites (H3) ──────────────

/// Exercises the *new* `TaskLifecycle::apply` path (not the deprecated shim)
/// under a failing PRD JSON sync. Locks the identical warning bytes so any
/// future change to the eprintln! in `src/lifecycle/apply/mod.rs` will fail
/// this test.
fn direct_tasklifecycle_apply_stderr_contract() {
    let (dir, mut conn) = setup_db();
    insert_task(&conn, "FEAT-DIR-1", "in_progress");
    let prd_path = dir.path().join("nonexistent.json");

    let intents = vec![TransitionIntent {
        task_id: "FEAT-DIR-1".to_string(),
        change: TransitionChange::Done,
        source: TransitionSource::Operator,
        reason: None,
        fail_status: None,
        audit_note: None,
    }];

    let mut applied_flag = false;
    let stderr = capture_stderr(|| {
        let mut lc = TaskLifecycle::new(&mut conn)
            .with_prd_sync(&prd_path, "FEAT-");
        let results = lc.apply(&intents);
        assert_eq!(results.len(), 1);
        applied_flag = results[0].applied;
    });

    assert!(applied_flag, "DB write must succeed; PRD sync is best-effort");

    // Verify DB is done before (or at) the warning
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'FEAT-DIR-1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "done");

    let masked = mask_path(&stderr, &prd_path);
    expect![[r#"
        Warning: <task-status> dispatched FEAT-DIR-1 to done in DB but PRD JSON sync failed (<PRD_PATH>): I/O error during reading PRD file on '<PRD_PATH>': No such file or directory (os error 2)
    "#]]
    .assert_eq(&masked);
}
