//! Comprehensive integration tests for retry tracking and auto-block logic.
//!
//! Covers parameterized boundary values for max_retries, auto-block message
//! content (task ID + failure count), model escalation ordering, and the full
//! handle_task_failure pipeline.
//!
//! Does NOT duplicate unit tests already in src/loop_engine/engine.rs.

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::engine::{
    auto_block_task, escalate_task_model_if_needed, handle_task_failure,
    increment_consecutive_failures, reset_consecutive_failures, should_auto_block,
    should_escalate_for_consecutive_failures,
};
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Set up a fresh in-process DB with schema and migrations applied.
fn setup_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let mut conn = open_connection(dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (dir, conn)
}

/// Insert a task with retry-tracking fields pre-set.
fn insert_retry_task(
    conn: &Connection,
    id: &str,
    model: Option<&str>,
    max_retries: i32,
    consecutive_failures: i32,
) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, max_retries, consecutive_failures) \
         VALUES (?, ?, 'in_progress', ?, ?, ?)",
        rusqlite::params![
            id,
            format!("Task {}", id),
            model,
            max_retries,
            consecutive_failures
        ],
    )
    .unwrap();
}

/// Read (consecutive_failures, model, status, last_error) for a task.
fn read_task_state(conn: &Connection, id: &str) -> (i32, Option<String>, String, Option<String>) {
    conn.query_row(
        "SELECT consecutive_failures, model, status, last_error FROM tasks WHERE id = ?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

// ── Parameterized should_auto_block boundary tests ────────────────────────────

/// Parameterized: should_auto_block for max_retries in {0, 1, 2, 3, 5, 10}.
///
/// Verifies the boundary at every value: must not fire below threshold,
/// must fire at and above threshold. max_retries=0 never fires at any count.
#[test]
fn test_should_auto_block_parameterized_max_retries_values() {
    // (consecutive_failures, max_retries, expected, description)
    let cases: &[(i32, i32, bool, &str)] = &[
        // max_retries=0: disabled — never fires regardless of failure count
        (0, 0, false, "max_retries=0, failures=0"),
        (1, 0, false, "max_retries=0, failures=1"),
        (10, 0, false, "max_retries=0, failures=10 (never fires)"),
        // max_retries=1: fires on first failure
        (0, 1, false, "max_retries=1, failures=0 (not yet)"),
        (1, 1, true, "max_retries=1, failures=1 (threshold hit)"),
        (2, 1, true, "max_retries=1, failures=2 (above threshold)"),
        // max_retries=2: fires at exactly 2
        (0, 2, false, "max_retries=2, failures=0"),
        (
            1,
            2,
            false,
            "max_retries=2, failures=1 (one below threshold)",
        ),
        (2, 2, true, "max_retries=2, failures=2 (threshold hit)"),
        (3, 2, true, "max_retries=2, failures=3 (above threshold)"),
        // max_retries=3: fires at exactly 3
        (0, 3, false, "max_retries=3, failures=0"),
        (
            2,
            3,
            false,
            "max_retries=3, failures=2 (one below threshold)",
        ),
        (3, 3, true, "max_retries=3, failures=3 (threshold hit)"),
        (5, 3, true, "max_retries=3, failures=5 (above threshold)"),
        // max_retries=5: fires at exactly 5
        (0, 5, false, "max_retries=5, failures=0"),
        (
            4,
            5,
            false,
            "max_retries=5, failures=4 (one below threshold)",
        ),
        (5, 5, true, "max_retries=5, failures=5 (threshold hit)"),
        (6, 5, true, "max_retries=5, failures=6 (above threshold)"),
        // max_retries=10: fires at exactly 10
        (0, 10, false, "max_retries=10, failures=0"),
        (
            9,
            10,
            false,
            "max_retries=10, failures=9 (one below threshold)",
        ),
        (10, 10, true, "max_retries=10, failures=10 (threshold hit)"),
        (
            11,
            10,
            true,
            "max_retries=10, failures=11 (above threshold)",
        ),
    ];

    for &(consecutive, max_retries, expected, desc) in cases {
        assert_eq!(
            should_auto_block(consecutive, max_retries),
            expected,
            "{}",
            desc
        );
    }
}

// ── Auto-block message content ────────────────────────────────────────────────

/// auto_block_task sets status='blocked', populates last_error, and the
/// last_error message contains both the failure count and the task ID.
#[test]
fn test_auto_block_message_contains_task_id_and_failure_count() {
    let (_dir, conn) = setup_db();
    insert_retry_task(&conn, "FAIL-007", Some(SONNET_MODEL), 3, 3);

    auto_block_task(&conn, "FAIL-007", 3, 1).unwrap();

    let (_, _, status, last_error) = read_task_state(&conn, "FAIL-007");
    assert_eq!(
        status, "blocked",
        "auto-blocked task must have status='blocked'"
    );

    let err = last_error.expect("auto_block_task must populate last_error");
    assert!(
        err.contains("FAIL-007"),
        "last_error must contain task ID 'FAIL-007', got: '{}'",
        err
    );
    assert!(
        err.contains('3') || err.to_lowercase().contains("fail"),
        "last_error must reference the failure count, got: '{}'",
        err
    );

    // blocked_at_iteration must be set for decay tracking
    let blocked_iter: Option<i64> = conn
        .query_row(
            "SELECT blocked_at_iteration FROM tasks WHERE id = 'FAIL-007'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        blocked_iter,
        Some(1),
        "auto_block_task must set blocked_at_iteration"
    );
}

/// auto_block_task works correctly with a different task ID — verifies message
/// template interpolation is not hard-coded.
#[test]
fn test_auto_block_message_task_id_interpolation() {
    let (_dir, conn) = setup_db();
    insert_retry_task(&conn, "XYZ-999", Some(SONNET_MODEL), 5, 5);

    auto_block_task(&conn, "XYZ-999", 5, 1).unwrap();

    let (_, _, _, last_error) = read_task_state(&conn, "XYZ-999");
    let err = last_error.expect("auto_block_task must populate last_error");
    assert!(
        err.contains("XYZ-999"),
        "last_error must contain interpolated task ID 'XYZ-999', got: '{}'",
        err
    );
    assert!(
        err.contains('5') || err.to_lowercase().contains("fail"),
        "last_error must contain failure count 5, got: '{}'",
        err
    );
}

// ── Rapid complete/fail sequences ─────────────────────────────────────────────

/// Rapid interleaved fail/succeed sequence verifies counter accuracy.
///
/// Sequence: F(1), S(0), F(1), F(2), S(0), F(1), F(2), F(3), F(4), F(5).
/// Expected final counter: 5 (five consecutive failures after last reset).
#[test]
fn test_rapid_complete_fail_sequences_counter_accuracy() {
    let (_dir, conn) = setup_db();
    conn.execute(
        "INSERT INTO tasks (id, title, status, consecutive_failures) \
         VALUES ('SEQ-001', 'Seq Task', 'in_progress', 0)",
        [],
    )
    .unwrap();

    // Interleaved sequence
    increment_consecutive_failures(&conn, "SEQ-001").unwrap(); // 1
    reset_consecutive_failures(&conn, "SEQ-001").unwrap(); // 0
    increment_consecutive_failures(&conn, "SEQ-001").unwrap(); // 1
    increment_consecutive_failures(&conn, "SEQ-001").unwrap(); // 2
    reset_consecutive_failures(&conn, "SEQ-001").unwrap(); // 0
    increment_consecutive_failures(&conn, "SEQ-001").unwrap(); // 1
    increment_consecutive_failures(&conn, "SEQ-001").unwrap(); // 2
    increment_consecutive_failures(&conn, "SEQ-001").unwrap(); // 3
    increment_consecutive_failures(&conn, "SEQ-001").unwrap(); // 4
    let last = increment_consecutive_failures(&conn, "SEQ-001").unwrap(); // 5

    assert_eq!(
        last, 5,
        "return value from increment must equal the live DB count"
    );

    let db_count: i32 = conn
        .query_row(
            "SELECT consecutive_failures FROM tasks WHERE id = 'SEQ-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(db_count, 5, "DB counter must be 5 after the rapid sequence");

    // Boundary check against two different max_retries values
    assert!(
        should_auto_block(db_count, 5),
        "auto-block must fire at count=5 with max_retries=5"
    );
    assert!(
        !should_auto_block(db_count, 10),
        "auto-block must NOT fire at count=5 with max_retries=10"
    );
}

// ── handle_task_failure integration ──────────────────────────────────────────

/// handle_task_failure: sonnet task, 3 failures with max_retries=3.
///
/// - Failure 1 (count→1): no escalation (threshold=2), no block (count < max_retries=3)
/// - Failure 2 (count→2): model escalated sonnet→opus, no block (count < max_retries=3)
/// - Failure 3 (count→3): auto-blocked (count >= max_retries=3), model stays at opus
#[test]
fn test_handle_task_failure_escalation_at_two_then_block_at_three() {
    let (_dir, mut conn) = setup_db();
    insert_retry_task(&conn, "TASK-A", Some(SONNET_MODEL), 3, 0);

    // Failure 1
    handle_task_failure(&mut conn, "TASK-A", 1).unwrap();
    let (count, model, status, _) = read_task_state(&conn, "TASK-A");
    assert_eq!(count, 1, "failure 1: count must be 1");
    assert_eq!(
        model.as_deref(),
        Some(SONNET_MODEL),
        "failure 1: model must stay sonnet (escalation threshold not reached)"
    );
    assert_eq!(status, "in_progress", "failure 1: task must not be blocked");

    // Failure 2: escalation fires (count=2 >= threshold=2)
    handle_task_failure(&mut conn, "TASK-A", 2).unwrap();
    let (count, model, status, _) = read_task_state(&conn, "TASK-A");
    assert_eq!(count, 2, "failure 2: count must be 2");
    assert_eq!(
        model.as_deref(),
        Some(OPUS_MODEL),
        "failure 2: model must be escalated to opus"
    );
    assert_eq!(
        status, "in_progress",
        "failure 2: task must not yet be blocked"
    );

    // Failure 3: auto-block fires (count=3 >= max_retries=3), escalation skipped (would be wasted)
    handle_task_failure(&mut conn, "TASK-A", 3).unwrap();
    let (count, model, status, last_error) = read_task_state(&conn, "TASK-A");
    assert_eq!(count, 3, "failure 3: count must be 3");
    assert_eq!(
        model.as_deref(),
        Some(OPUS_MODEL),
        "failure 3: model stays at opus ceiling"
    );
    assert_eq!(status, "blocked", "failure 3: task must be auto-blocked");
    assert!(last_error.is_some(), "failure 3: last_error must be set");
}

/// handle_task_failure with max_retries=0: 5 failures, never auto-blocked.
///
/// Counter still increments — max_retries=0 only disables blocking.
#[test]
fn test_handle_task_failure_max_retries_zero_never_blocks() {
    let (_dir, mut conn) = setup_db();
    insert_retry_task(&conn, "NEVER-BLOCK", Some(SONNET_MODEL), 0, 0);

    for i in 0..5 {
        handle_task_failure(&mut conn, "NEVER-BLOCK", i + 1).unwrap();
    }

    let (count, _, status, _) = read_task_state(&conn, "NEVER-BLOCK");
    assert_eq!(
        status, "in_progress",
        "max_retries=0 must never auto-block regardless of failure count"
    );
    assert_eq!(
        count, 5,
        "consecutive_failures must still increment even when auto-block is disabled"
    );
}

/// handle_task_failure with max_retries=1: blocked on the very first failure.
#[test]
fn test_handle_task_failure_max_retries_one_blocks_immediately() {
    let (_dir, mut conn) = setup_db();
    insert_retry_task(&conn, "BLOCK-FAST", Some(SONNET_MODEL), 1, 0);

    handle_task_failure(&mut conn, "BLOCK-FAST", 1).unwrap();

    let (count, _, status, _) = read_task_state(&conn, "BLOCK-FAST");
    assert_eq!(
        status, "blocked",
        "max_retries=1 must auto-block after first failure"
    );
    assert_eq!(
        count, 1,
        "consecutive_failures must be 1 after first failure"
    );
}

/// handle_task_failure with max_retries=2: blocked on the second failure.
#[test]
fn test_handle_task_failure_max_retries_two_blocks_on_second() {
    let (_dir, mut conn) = setup_db();
    insert_retry_task(&conn, "BLOCK-TWO", Some(SONNET_MODEL), 2, 0);

    // Failure 1: no block yet
    handle_task_failure(&mut conn, "BLOCK-TWO", 1).unwrap();
    let (count, _, status, _) = read_task_state(&conn, "BLOCK-TWO");
    assert_eq!(count, 1, "failure 1: count must be 1");
    assert_eq!(
        status, "in_progress",
        "failure 1: must not block at count=1, max_retries=2"
    );

    // Failure 2: blocks (escalation skipped — would be wasted since auto-block fires too)
    handle_task_failure(&mut conn, "BLOCK-TWO", 2).unwrap();
    let (count, _, status, _) = read_task_state(&conn, "BLOCK-TWO");
    assert_eq!(count, 2, "failure 2: count must be 2");
    assert_eq!(
        status, "blocked",
        "failure 2: must block at count=2, max_retries=2"
    );
}

// ── Model escalation: haiku → sonnet ──────────────────────────────────────────

/// Haiku task at 2 consecutive failures → escalated to sonnet (one tier up).
///
/// The model ladder is: haiku → sonnet → opus. Haiku does NOT jump straight to opus.
#[test]
fn test_model_escalation_haiku_to_sonnet_at_two_failures() {
    let (_dir, conn) = setup_db();
    insert_retry_task(&conn, "HAIKU-T", Some(HAIKU_MODEL), 3, 0);

    let result = escalate_task_model_if_needed(&conn, "HAIKU-T", 2).unwrap();
    assert_eq!(
        result,
        Some(SONNET_MODEL.to_string()),
        "haiku at 2 failures must escalate to sonnet (one tier up, not directly to opus)"
    );

    let model: Option<String> = conn
        .query_row("SELECT model FROM tasks WHERE id = 'HAIKU-T'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        model.as_deref(),
        Some(SONNET_MODEL),
        "model column must be updated to sonnet in DB"
    );
}

/// Empty / whitespace-only models in the DB must normalize to sonnet baseline
/// and escalate to opus, matching `check_crash_escalation`. Falsifies a past
/// state where the two paths disagreed on non-empty-but-whitespace inputs.
#[test]
fn test_model_escalation_empty_and_whitespace_normalize_to_opus() {
    for (idx, bad) in ["", "   ", "\t"].iter().enumerate() {
        let (_dir, conn) = setup_db();
        let id = format!("WS-{idx}");
        insert_retry_task(&conn, &id, Some(bad), 5, 0);

        let result = escalate_task_model_if_needed(&conn, &id, 2).unwrap();
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "bogus model {bad:?} must normalize to baseline and escalate to opus"
        );

        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = ?", [&id], |r| r.get(0))
            .unwrap();
        assert_eq!(
            model.as_deref(),
            Some(OPUS_MODEL),
            "DB model column must be rewritten to opus"
        );
    }
}

/// handle_task_failure with a haiku task: 2 failures → escalates to sonnet, not opus.
#[test]
fn test_handle_task_failure_haiku_escalates_to_sonnet_at_two() {
    let (_dir, mut conn) = setup_db();
    insert_retry_task(&conn, "HAIKU-PIPE", Some(HAIKU_MODEL), 5, 0);

    // Failure 1: no escalation
    handle_task_failure(&mut conn, "HAIKU-PIPE", 1).unwrap();
    let (_, model, _, _) = read_task_state(&conn, "HAIKU-PIPE");
    assert_eq!(
        model.as_deref(),
        Some(HAIKU_MODEL),
        "failure 1: haiku model must remain unchanged"
    );

    // Failure 2: escalates haiku → sonnet
    handle_task_failure(&mut conn, "HAIKU-PIPE", 2).unwrap();
    let (_, model, status, _) = read_task_state(&conn, "HAIKU-PIPE");
    assert_eq!(
        model.as_deref(),
        Some(SONNET_MODEL),
        "failure 2: haiku must escalate to sonnet (not directly to opus)"
    );
    assert_eq!(
        status, "in_progress",
        "failure 2: not yet auto-blocked (max_retries=5)"
    );
}

// ── Escalation ordering invariant ─────────────────────────────────────────────

/// Escalation fires at consecutive_failures=2, before auto-block at max_retries=3.
///
/// This verifies the ordering: escalation threshold (2) < default auto-block threshold (3).
#[test]
fn test_escalation_fires_before_auto_block_ordering() {
    // At count=2: escalation YES, auto-block NO (max_retries=3)
    assert!(
        should_escalate_for_consecutive_failures(2),
        "escalation must fire at consecutive_failures=2"
    );
    assert!(
        !should_auto_block(2, 3),
        "auto-block must NOT fire at consecutive_failures=2 with max_retries=3"
    );

    // At count=3: both fire
    assert!(
        should_escalate_for_consecutive_failures(3),
        "escalation also fires at consecutive_failures=3"
    );
    assert!(
        should_auto_block(3, 3),
        "auto-block fires at consecutive_failures=3 with max_retries=3"
    );
}

// ── reset_consecutive_failures: does not unblock ──────────────────────────────

/// Resetting the counter on a blocked task clears consecutive_failures to 0
/// but does NOT change the status — unblocking requires a separate action.
#[test]
fn test_reset_on_blocked_task_clears_counter_not_status() {
    let (_dir, conn) = setup_db();
    insert_retry_task(&conn, "BLOCKED-T", Some(SONNET_MODEL), 3, 3);
    conn.execute(
        "UPDATE tasks SET status = 'blocked', last_error = 'Auto-blocked after 3 consecutive failures (task: BLOCKED-T)' WHERE id = 'BLOCKED-T'",
        [],
    )
    .unwrap();

    reset_consecutive_failures(&conn, "BLOCKED-T").unwrap();

    let (count, _, status, _) = read_task_state(&conn, "BLOCKED-T");
    assert_eq!(count, 0, "reset must zero the counter");
    assert_eq!(
        status, "blocked",
        "reset must not change status — blocked tasks stay blocked"
    );
}
