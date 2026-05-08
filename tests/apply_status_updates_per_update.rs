//! Contract tests for `apply_status_updates` per-update success result (M2 fix).
//!
//! # TDD-bootstrap
//!
//! These tests pin the CODE-FIX-002 API BEFORE the implementation lands:
//!
//! - **One type-level guard** (no `#[ignore]`): assigns the return value to
//!   `Vec<(String, TaskStatusChange, bool)>`. Today this file will NOT compile
//!   because `apply_status_updates` still returns `u32` — that compile failure
//!   IS the "fail today" signal. CODE-FIX-002 changes the return type; once it
//!   does, the file compiles and the guard runs clean.
//!
//! - **`#[ignore]`'d body tests**: define the behavioral contract for
//!   CODE-FIX-002. CODE-FIX-002's final step is to remove the `#[ignore]`
//!   attributes so these tests run in CI.
//!
//! # Known-bad discriminator
//!
//! A test that only checks `applied > 0` (the legacy global counter) would pass
//! both before and after CODE-FIX-002 — it would NOT catch the M2 bug. The
//! tests here are designed to fail against the old `u32` return type and pass
//! only once the per-(task_id, status, success) shape lands.
//!
//! # Setup note
//!
//! DB setup uses `open_connection` + `create_schema` + `run_migrations` (same
//! pattern as `tests/retry_tracking.rs`) so the full schema is available.

use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
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

fn write_minimal_prd(dir: &std::path::Path, ids: &[&str]) -> PathBuf {
    use serde_json::json;
    let stories: Vec<_> = ids
        .iter()
        .map(|id| json!({"id": id, "title": "t", "priority": 50, "passes": false}))
        .collect();
    let doc = json!({"userStories": stories});
    let path = dir.join("test-prd.json");
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    path
}

// ── Type-level guard (runs unconditionally — NOT #[ignore]'d) ────────────────

/// Compile-time assertion that `apply_status_updates` returns
/// `Vec<(String, TaskStatusChange, bool)>` — the per-update shape required by
/// the M2 fix.
///
/// This test does NOT carry `#[ignore]`. If `apply_status_updates` still
/// returns `u32` this file fails to compile, which is the intended signal that
/// CODE-FIX-002 has not yet landed. Once it lands:
/// - the file compiles
/// - this test runs (empty slice → empty vec, no panic)
/// - the `#[ignore]`'d body tests below can be un-ignored
#[test]
fn apply_status_updates_return_type_is_per_update_vec() {
    let (_dir, mut conn) = setup_db();

    // The explicit type binding is the assertion. If the return type is not
    // Vec<(String, TaskStatusChange, bool)> this line is a compile error.
    let result: Vec<(String, TaskStatusChange, bool)> =
        apply_status_updates(&mut conn, &[], None, None, None, None, None);

    // Empty updates slice → empty result vec (not None, not 0, not a panic).
    assert!(
        result.is_empty(),
        "empty updates must yield empty result vec"
    );
}

// ── Behavioral contract (body tests, #[ignore]'d until CODE-FIX-002) ─────────

/// All updates that dispatch successfully must appear with `true` in the result
/// vec, preserving (task_id, status) identity for each entry.
#[test]
fn apply_status_updates_all_success_returns_all_true() {
    let (dir, mut conn) = setup_db();
    insert_task(&conn, "FEAT-A", "in_progress");
    insert_task(&conn, "FEAT-B", "in_progress");
    let prd = write_minimal_prd(dir.path(), &["FEAT-A", "FEAT-B"]);

    let updates = vec![
        TaskStatusUpdate {
            task_id: "FEAT-A".to_string(),
            status: TaskStatusChange::Done,
        },
        TaskStatusUpdate {
            task_id: "FEAT-B".to_string(),
            status: TaskStatusChange::Done,
        },
    ];
    let result: Vec<(String, TaskStatusChange, bool)> =
        apply_status_updates(&mut conn, &updates, None, Some(&prd), None, None, None);

    assert_eq!(result.len(), 2, "one result entry per update");

    let a = result
        .iter()
        .find(|(id, _, _)| id == "FEAT-A")
        .expect("FEAT-A must appear in result");
    assert!(a.2, "FEAT-A dispatch succeeded → true");

    let b = result
        .iter()
        .find(|(id, _, _)| id == "FEAT-B")
        .expect("FEAT-B must appear in result");
    assert!(b.2, "FEAT-B dispatch succeeded → true");
}

/// When one dispatch is rejected (e.g. task absent from DB), that entry carries
/// `false`; peer entries whose dispatch succeeded carry `true`.
///
/// This is the core M2 scenario: the old global `applied > 0` flag would be
/// `true` (peer succeeded), silently marking the claimed task done even though
/// its own dispatch failed.
///
/// Failure trigger: a `Done` dispatch for a non-existent task ID raises
/// "task not found" from `complete_single_task` (mirrors the existing
/// `test_apply_status_update_continues_past_failed_dispatch` test in
/// `src/loop_engine/engine.rs`). `complete()` is idempotent for already-done
/// tasks, so "already done" is NOT a failure trigger.
#[test]
fn apply_status_updates_rejected_dispatch_returns_false_for_that_entry() {
    let (dir, mut conn) = setup_db();
    // CLAIMED is intentionally absent from the DB so its Done dispatch fails.
    insert_task(&conn, "PEER", "in_progress");
    let prd = write_minimal_prd(dir.path(), &["CLAIMED", "PEER"]);

    let updates = vec![
        TaskStatusUpdate {
            task_id: "CLAIMED".to_string(),
            status: TaskStatusChange::Done,
        },
        TaskStatusUpdate {
            task_id: "PEER".to_string(),
            status: TaskStatusChange::Done,
        },
    ];
    let result: Vec<(String, TaskStatusChange, bool)> =
        apply_status_updates(&mut conn, &updates, None, Some(&prd), None, None, None);

    assert_eq!(result.len(), 2, "one result entry per update");

    let claimed = result
        .iter()
        .find(|(id, _, _)| id == "CLAIMED")
        .expect("CLAIMED must appear in result");
    assert!(
        !claimed.2,
        "CLAIMED dispatch failed (task absent from DB) → false"
    );

    let peer = result
        .iter()
        .find(|(id, _, _)| id == "PEER")
        .expect("PEER must appear in result");
    assert!(peer.2, "PEER dispatch succeeded → true");
}

/// Same-task-id appearing twice with different statuses: each entry is tracked
/// independently. Edge case: the pipeline must not collapse them.
#[test]
fn apply_status_updates_same_task_different_statuses_tracked_independently() {
    let (dir, mut conn) = setup_db();
    insert_task(&conn, "TASK-X", "in_progress");
    let prd = write_minimal_prd(dir.path(), &["TASK-X"]);

    // Done first (succeeds), then Skip (fails — task is now done).
    let updates = vec![
        TaskStatusUpdate {
            task_id: "TASK-X".to_string(),
            status: TaskStatusChange::Done,
        },
        TaskStatusUpdate {
            task_id: "TASK-X".to_string(),
            status: TaskStatusChange::Skipped,
        },
    ];
    let result: Vec<(String, TaskStatusChange, bool)> =
        apply_status_updates(&mut conn, &updates, None, Some(&prd), None, None, None);

    assert_eq!(result.len(), 2, "each update gets its own result entry");

    let done_entry = result
        .iter()
        .find(|(id, s, _)| id == "TASK-X" && matches!(s, TaskStatusChange::Done))
        .expect("Done entry must be present");
    assert!(done_entry.2, "Done dispatch succeeded → true");

    let skip_entry = result
        .iter()
        .find(|(id, s, _)| id == "TASK-X" && matches!(s, TaskStatusChange::Skipped))
        .expect("Skipped entry must be present");
    assert!(
        !skip_entry.2,
        "Skipped dispatch failed (task already done) → false"
    );
}

/// The iteration_pipeline status-tag completion gate (step 4a) must use the
/// claimed task's per-entry success result, NOT the global
/// `status_updates_applied > 0` flag.
///
/// Scenario: peer task's Done dispatch succeeds but claimed task's Done dispatch
/// fails (already done). The old gate uses `applied > 0` which is `true` when
/// the peer succeeds — a false positive that marks the claimed task done. The
/// new gate must check `results.iter().find(claimed_id).map(|e| e.2)`.
///
/// This test drives `process_iteration_output` directly to verify end-to-end
/// gate behavior.
#[test]
fn pipeline_gate_uses_per_entry_success_not_global_count() {
    use task_mgr::loop_engine::config::IterationOutcome;
    use task_mgr::loop_engine::engine::IterationContext;
    use task_mgr::loop_engine::iteration_pipeline::{ProcessingParams, process_iteration_output};
    use task_mgr::loop_engine::signals::SignalFlag;

    unsafe {
        std::env::set_var("TASK_MGR_NO_EXTRACT_LEARNINGS", "1");
    }

    let (dir, mut conn) = setup_db();

    // CLAIMED-001 is intentionally absent from the DB so its Done dispatch fails
    // (complete() is idempotent for already-done tasks; "task not found" is the
    // failure trigger).
    insert_task(&conn, "PEER-001", "in_progress");
    let prd = write_minimal_prd(dir.path(), &["CLAIMED-001", "PEER-001"]);

    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES ('run-gate', 'active')",
        [],
    )
    .unwrap();

    // Claude output contains Done tags for both tasks.
    let output = "<task-status>CLAIMED-001:done</task-status>\n\
                  <task-status>PEER-001:done</task-status>";

    let mut outcome = IterationOutcome::Completed;
    let signal_flag = SignalFlag::new();
    let mut ctx = IterationContext::new(3);
    let progress_path = dir.path().join("progress.txt");
    std::fs::write(&progress_path, "").unwrap();

    let result = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "run-gate",
        iteration: 1,
        task_id: Some("CLAIMED-001"),
        output,
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: dir.path(),
        git_scan_depth: 0,
        skip_git_completion_detection: true,
        prd_path: &prd,
        task_prefix: None,
        progress_path: &progress_path,
        db_dir: dir.path(),
        signal_flag: &signal_flag,
        ctx: &mut ctx,
        files_modified: &[],
        effective_model: None,
        effective_effort: None,
        slot_index: None,
    });

    // With per-entry gate: CLAIMED-001's dispatch failed → NOT in completed_task_ids.
    assert!(
        !result
            .completed_task_ids
            .iter()
            .any(|id| id == "CLAIMED-001"),
        "CLAIMED-001 must NOT be marked done via 4a gate when its own dispatch failed"
    );

    // PEER-001 succeeded → it IS marked done.
    assert!(
        result.completed_task_ids.iter().any(|id| id == "PEER-001"),
        "PEER-001 must be marked done (its dispatch succeeded)"
    );
}
