//! Integration tests: full multi-PRD lifecycle.
//!
//! Verifies that two PRDs can coexist in one database with full isolation:
//! - prd_metadata has one row per prefix
//! - Task counts scoped per prefix
//! - select_next_task with P1 prefix returns only P1 tasks
//! - Force-reinit P1 deletes P1 tasks, leaves P2 tasks and learnings intact
//! - Signal files: .stop-P1 / global .stop fallback
//! - Lock files: loop-P1.lock and loop-P2.lock are independently acquirable
//! - Single-PRD DB without prefix: backwards-compatible behaviour

use std::fs;
use tempfile::TempDir;

use task_mgr::commands::init::{self, PrefixMode};
use task_mgr::commands::next::selection::select_next_task;
use task_mgr::db::{open_connection, LockGuard};
use task_mgr::loop_engine::signals::check_stop_signal;

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Import both PRDs into a single temp directory.
fn setup_dual_prd_db() -> (TempDir, rusqlite::Connection) {
    let temp_dir = TempDir::new().unwrap();

    // Import P1 (alpha) — first import, fresh DB
    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        false, // force
        false, // append
        false, // update_existing
        false, // dry_run
        PrefixMode::Auto,
    )
    .unwrap();

    // Import P2 (beta) — append to existing DB
    init::init(
        temp_dir.path(),
        &[&fixture("prd_p2_beta.json")],
        false, // force
        true,  // append
        false, // update_existing
        false, // dry_run
        PrefixMode::Auto,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    (temp_dir, conn)
}

// ============================================================================
// AC: prd_metadata has 2 rows, one per prefix
// ============================================================================

#[test]
fn test_dual_prd_metadata_has_two_rows() {
    let (_temp_dir, conn) = setup_dual_prd_db();

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM prd_metadata", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2, "Should have exactly 2 prd_metadata rows");

    // Both prefixes are present
    let p1_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix = 'P1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        > 0;
    let p2_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix = 'P2'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        > 0;
    assert!(p1_exists, "prd_metadata should have P1 row");
    assert!(p2_exists, "prd_metadata should have P2 row");
}

// ============================================================================
// AC: task counts scoped correctly per prefix
// ============================================================================

#[test]
fn test_dual_prd_task_counts_scoped_per_prefix() {
    let (_temp_dir, conn) = setup_dual_prd_db();

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(total, 4, "Total tasks across both PRDs should be 4");

    let p1_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id LIKE 'P1-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(p1_count, 2, "P1 should have 2 tasks");

    let p2_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id LIKE 'P2-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(p2_count, 2, "P2 should have 2 tasks");
}

// ============================================================================
// AC: select_next_task with P1 prefix returns only P1 tasks
// ============================================================================

#[test]
fn test_select_next_task_scoped_to_prefix() {
    let (_temp_dir, conn) = setup_dual_prd_db();

    // P1: TASK-001 has no deps (eligible), TASK-002 depends on TASK-001 (not yet)
    let p1_result = select_next_task(&conn, &[], &[], Some("P1")).unwrap();
    let p1_task = p1_result.task.expect("P1 should have an eligible task");
    assert!(
        p1_task.task.id.starts_with("P1-"),
        "select_next_task(P1) returned non-P1 task: {}",
        p1_task.task.id
    );
    assert_eq!(p1_task.task.id, "P1-TASK-001");

    // P2: TASK-001 is todo with no deps; TASK-002 is done
    let p2_result = select_next_task(&conn, &[], &[], Some("P2")).unwrap();
    let p2_task = p2_result.task.expect("P2 should have an eligible task");
    assert!(
        p2_task.task.id.starts_with("P2-"),
        "select_next_task(P2) returned non-P2 task: {}",
        p2_task.task.id
    );
    assert_eq!(p2_task.task.id, "P2-TASK-001");
}

// ============================================================================
// AC: force-reinit P1 → P1 tasks deleted, P2 tasks intact, learnings intact
// ============================================================================

#[test]
fn test_force_reinit_p1_preserves_p2_and_learnings() {
    let (temp_dir, conn) = setup_dual_prd_db();

    // Insert a learning before force-reinit
    conn.execute(
        "INSERT INTO learnings (outcome, title, content, confidence) \
         VALUES ('pattern', 'Test learning', 'Keep things simple.', 'high')",
        [],
    )
    .unwrap();
    let learning_id: i64 = conn.last_insert_rowid();
    drop(conn);

    // Force-reinit only P1 (PrefixMode::Auto reads taskPrefix from the JSON)
    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        true,  // force — deletes P1 tasks
        false, // append
        false,
        false,
        PrefixMode::Auto,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();

    // P1 tasks should be reimported (still present, fresh)
    let p1_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id LIKE 'P1-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(p1_count, 2, "P1 tasks should be present after force-reinit");

    // P2 tasks must be intact
    let p2_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE id LIKE 'P2-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        p2_count, 2,
        "P2 tasks should be untouched by P1 force-reinit"
    );

    // Learning must still exist
    let learning_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings WHERE id = ?",
            [learning_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        > 0;
    assert!(
        learning_exists,
        "Learnings must be preserved after scoped force-reinit"
    );
}

// ============================================================================
// AC: signal files — .stop-P1 checked first, global .stop fallback
// ============================================================================

#[test]
fn test_stop_signal_prefix_specific_and_fallback() {
    let temp_dir = TempDir::new().unwrap();
    let tasks_dir = temp_dir.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();

    // No signal files — both return false
    assert!(!check_stop_signal(&tasks_dir, Some("P1")));
    assert!(!check_stop_signal(&tasks_dir, Some("P2")));
    assert!(!check_stop_signal(&tasks_dir, None));

    // Create .stop-P1 — only P1 is signaled
    fs::write(tasks_dir.join(".stop-P1"), "").unwrap();
    assert!(
        check_stop_signal(&tasks_dir, Some("P1")),
        ".stop-P1 should signal P1"
    );
    assert!(
        !check_stop_signal(&tasks_dir, Some("P2")),
        ".stop-P1 must not affect P2"
    );
    assert!(
        !check_stop_signal(&tasks_dir, None),
        ".stop-P1 must not affect global check"
    );
    fs::remove_file(tasks_dir.join(".stop-P1")).unwrap();

    // Create global .stop — all prefixed and unscoped sessions see it
    fs::write(tasks_dir.join(".stop"), "").unwrap();
    assert!(
        check_stop_signal(&tasks_dir, Some("P1")),
        "Global .stop must be visible as fallback for P1"
    );
    assert!(
        check_stop_signal(&tasks_dir, Some("P2")),
        "Global .stop must be visible as fallback for P2"
    );
    assert!(
        check_stop_signal(&tasks_dir, None),
        "Global .stop must be visible without prefix"
    );
}

// ============================================================================
// AC: lock files — loop-P1.lock and loop-P2.lock are independently acquirable
// ============================================================================

#[test]
fn test_per_prefix_locks_are_independent() {
    let temp_dir = TempDir::new().unwrap();

    // Acquire P1 lock
    let guard1 =
        LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock").expect("Should acquire P1 lock");

    // P2 lock is independent — should succeed while P1 is held
    let guard2 = LockGuard::acquire_named(temp_dir.path(), "loop-P2.lock")
        .expect("Should acquire P2 lock independently of P1");

    // Both lock files exist simultaneously
    assert!(temp_dir.path().join("loop-P1.lock").exists());
    assert!(temp_dir.path().join("loop-P2.lock").exists());

    // Attempting to re-acquire P1 while held must fail
    let p1_retry = LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock");
    assert!(
        p1_retry.is_err(),
        "Acquiring P1 lock again while held should fail"
    );

    drop(guard1);
    drop(guard2);

    // After release both locks can be re-acquired
    let _reacquire = LockGuard::acquire_named(temp_dir.path(), "loop-P1.lock")
        .expect("Should re-acquire P1 after release");
}

// ============================================================================
// AC: single-PRD DB with no prefix — backwards-compatible behaviour
// ============================================================================

#[test]
fn test_single_prd_no_prefix_backwards_compat() {
    let temp_dir = TempDir::new().unwrap();

    // Import using PrefixMode::Disabled (legacy behaviour)
    let result = init::init(
        temp_dir.path(),
        &[&fixture("sample_prd.json")],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();
    assert!(result.tasks_imported > 0, "Should import tasks");

    let conn = open_connection(temp_dir.path()).unwrap();

    // With PrefixMode::Disabled the resolved prefix is None, so no prefix is
    // prepended to task IDs. prd_metadata may still store the JSON's taskPrefix
    // but tasks are stored without a prefix prepended. The key invariant is that
    // select_next_task(None) works correctly — no prefix filtering is applied.

    // select_next_task with None prefix works (no filter applied)
    let sel = select_next_task(&conn, &[], &[], None).unwrap();
    assert!(
        sel.task.is_some(),
        "select_next_task(None) should work without prefix"
    );
}
