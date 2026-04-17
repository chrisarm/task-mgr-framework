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

use task_mgr::commands::init::{self, PrefixMode, generate_prefix};
use task_mgr::commands::next::selection::select_next_task;
use task_mgr::db::prefix::validate_prefix;
use task_mgr::db::{LockGuard, open_connection};
use task_mgr::loop_engine::signals::check_stop_signal;
use task_mgr::loop_engine::status_queries::read_prd_hints;

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
        PrefixMode::Explicit("P1".to_string()),
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
        PrefixMode::Explicit("P2".to_string()),
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

    // Force-reinit only P1 with explicit prefix
    init::init(
        temp_dir.path(),
        &[&fixture("prd_p1_alpha.json")],
        true,  // force — deletes P1 tasks
        false, // append
        false,
        false,
        PrefixMode::Explicit("P1".to_string()),
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

// ============================================================================
// AC: auto-generated prefix is consistent between engine pre-lock path and init()
// ============================================================================

/// Verifies that the engine's pre-lock prefix computation (read_prd_hints + generate_prefix)
/// produces the same prefix that init(PrefixMode::Auto) writes back to the JSON file.
/// This is the critical invariant for lock file naming to match task IDs.
#[test]
fn test_autogen_prefix_consistent_between_engine_and_init() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = temp_dir.path().join("auto-prefix-test.json");

    // PRD with branchName but NO taskPrefix — forces auto-generation
    let prd_json = serde_json::json!({
        "project": "prefix-consistency-test",
        "branchName": "feat/auto-prefix-test",
        "userStories": [
            {
                "id": "TASK-001",
                "title": "Auto prefix test task",
                "description": "Verifies prefix consistency",
                "priority": 10,
                "status": "todo",
                "passes": false,
                "acceptanceCriteria": ["prefix matches"],
                "dependsOn": [],
                "batchWith": [],
                "conflictsWith": []
            }
        ]
    });
    fs::write(&prd_path, serde_json::to_string_pretty(&prd_json).unwrap()).unwrap();

    // Step 1: Simulate engine pre-lock computation
    let hints = read_prd_hints(&prd_path);
    assert!(
        hints.task_prefix.is_none(),
        "PRD should have no taskPrefix before init"
    );
    assert_eq!(
        hints.branch_name.as_deref(),
        Some("feat/auto-prefix-test"),
        "PRD should have branchName"
    );

    let filename = prd_path.file_name().and_then(|f| f.to_str()).unwrap();
    let engine_prefix = generate_prefix(hints.branch_name.as_deref(), filename);
    assert!(
        validate_prefix(&engine_prefix).is_ok(),
        "Engine-generated prefix must be valid: {}",
        engine_prefix
    );

    // Step 2: Run init with PrefixMode::Auto
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        PrefixMode::Auto,
    )
    .unwrap();

    // Step 3: Read back the taskPrefix that init wrote to JSON
    let updated_hints = read_prd_hints(&prd_path);
    let init_prefix = updated_hints
        .task_prefix
        .expect("init should have written taskPrefix back to JSON");
    assert_eq!(
        engine_prefix, init_prefix,
        "Engine prefix and init-written prefix must match"
    );

    // Step 4: Verify task IDs in DB use the same prefix
    let conn = open_connection(temp_dir.path()).unwrap();
    let task_id: String = conn
        .query_row("SELECT id FROM tasks LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(
        task_id.starts_with(&format!("{}-", engine_prefix)),
        "Task ID '{}' should start with '{}-'",
        task_id,
        engine_prefix
    );

    // Step 5: Verify the lock file name is acquirable and consistent
    let lock_name = format!("loop-{}.lock", engine_prefix);
    let _guard = LockGuard::acquire_named(temp_dir.path(), &lock_name)
        .expect("Should acquire lock with engine-computed prefix name");
    assert!(temp_dir.path().join(&lock_name).exists());
}

/// Edge case: PRD with no branchName AND no taskPrefix.
/// Both engine and init must still produce the same deterministic prefix
/// derived solely from the filename.
#[test]
fn test_autogen_prefix_consistent_without_branch_name() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = temp_dir.path().join("no-branch-prd.json");

    // PRD with neither branchName nor taskPrefix
    let prd_json = serde_json::json!({
        "project": "no-branch-prefix-test",
        "userStories": [
            {
                "id": "TASK-001",
                "title": "No-branch test task",
                "description": "Tests prefix when branchName is absent",
                "priority": 10,
                "status": "todo",
                "passes": false,
                "acceptanceCriteria": ["prefix matches"],
                "dependsOn": [],
                "batchWith": [],
                "conflictsWith": []
            }
        ]
    });
    fs::write(&prd_path, serde_json::to_string_pretty(&prd_json).unwrap()).unwrap();

    // Engine pre-lock computation with no branch name
    let hints = read_prd_hints(&prd_path);
    assert!(hints.task_prefix.is_none());
    assert!(hints.branch_name.is_none());

    let filename = prd_path.file_name().and_then(|f| f.to_str()).unwrap();
    let engine_prefix = generate_prefix(None, filename);
    assert!(
        validate_prefix(&engine_prefix).is_ok(),
        "Prefix from None branch must be valid: {}",
        engine_prefix
    );

    // Run init
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        PrefixMode::Auto,
    )
    .unwrap();

    // Verify consistency
    let updated_hints = read_prd_hints(&prd_path);
    let init_prefix = updated_hints
        .task_prefix
        .expect("init should have written taskPrefix back to JSON");
    assert_eq!(
        engine_prefix, init_prefix,
        "Engine prefix (no branch) and init-written prefix must match"
    );

    // Verify task ID uses the prefix
    let conn = open_connection(temp_dir.path()).unwrap();
    let task_id: String = conn
        .query_row("SELECT id FROM tasks LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(
        task_id.starts_with(&format!("{}-", engine_prefix)),
        "Task ID '{}' should start with '{}-'",
        task_id,
        engine_prefix
    );

    // Verify lock file consistency
    let lock_name = format!("loop-{}.lock", engine_prefix);
    let _guard = LockGuard::acquire_named(temp_dir.path(), &lock_name)
        .expect("Should acquire lock with no-branch prefix name");
}

/// Regression test: loop→batch transition must reuse the same prefix.
///
/// Simulates: (1) loop run with Auto writes prefix + tasks, (2) batch run with
/// Auto + append + update_existing must find existing tasks, not create duplicates.
#[test]
fn test_loop_to_batch_prefix_continuity() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = temp_dir.path().join("04-predictive-intelligence.json");

    let prd_json = serde_json::json!({
        "project": "loop-batch-continuity",
        "branchName": "feat/predictive-intelligence",
        "userStories": [
            {
                "id": "FEAT-501b",
                "title": "Predictive task",
                "description": "Tests loop to batch prefix continuity",
                "priority": 10,
                "status": "todo",
                "passes": false,
                "acceptanceCriteria": ["prefix is consistent"],
                "dependsOn": [],
                "batchWith": [],
                "conflictsWith": []
            }
        ]
    });
    fs::write(&prd_path, serde_json::to_string_pretty(&prd_json).unwrap()).unwrap();

    // Step 1: Simulate loop run — init with PrefixMode::Auto
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false, // force
        false, // append
        false, // update_existing
        false, // dry_run
        PrefixMode::Auto,
    )
    .unwrap();

    // Capture the prefix and task ID from the first run
    let hints_after_loop = read_prd_hints(&prd_path);
    let loop_prefix = hints_after_loop
        .task_prefix
        .expect("loop run should have written taskPrefix");

    let conn = open_connection(temp_dir.path()).unwrap();
    let task_count_after_loop: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        task_count_after_loop, 1,
        "should have exactly 1 task after loop"
    );

    let task_id_after_loop: String = conn
        .query_row("SELECT id FROM tasks LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(
        task_id_after_loop.starts_with(&format!("{}-", loop_prefix)),
        "Task ID '{}' should be prefixed with '{}-'",
        task_id_after_loop,
        loop_prefix
    );

    // Step 2: Simulate batch run — init again with PrefixMode::Auto (append + update)
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false, // force
        true,  // append
        true,  // update_existing
        false, // dry_run
        PrefixMode::Auto,
    )
    .unwrap();

    // Step 3: Verify no duplicate tasks were created
    let task_count_after_batch: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        task_count_after_batch, 1,
        "batch run with Auto prefix must NOT create duplicates (got {} tasks)",
        task_count_after_batch
    );

    // Step 4: Verify the task ID is unchanged
    let task_id_after_batch: String = conn
        .query_row("SELECT id FROM tasks LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        task_id_after_loop, task_id_after_batch,
        "Task ID must remain the same after loop to batch transition"
    );

    // Step 5: Verify the prefix in JSON is unchanged
    let hints_after_batch = read_prd_hints(&prd_path);
    let batch_prefix = hints_after_batch
        .task_prefix
        .expect("batch run should preserve taskPrefix");
    assert_eq!(
        loop_prefix, batch_prefix,
        "Prefix must be identical between loop and batch runs"
    );
}
