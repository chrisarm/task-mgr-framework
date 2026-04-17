/// Closed-loop learning feedback for the autonomous agent loop.
///
/// After each iteration, correlates shown learnings with the task outcome.
/// On successful completion, records each shown learning as "applied" via the
/// UCB bandit system (`record_learning_applied`). On failure/non-completion,
/// learnings are not recorded — they weren't demonstrably helpful.
///
/// This closes the feedback loop that makes the UCB bandit actually learn
/// from iteration outcomes rather than just showing learnings blindly.
use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::learnings::bandit;
use crate::loop_engine::config::IterationOutcome;

/// Record feedback for learnings shown during an iteration based on its outcome.
///
/// - On `Completed`: calls `record_learning_applied()` for each shown learning ID
/// - On all other outcomes: no-op (learning wasn't demonstrably helpful)
/// - Empty `shown_learning_ids`: no-op (no DB calls)
///
/// Errors from individual `record_learning_applied` calls are logged to stderr
/// but do not propagate — a feedback recording failure should never crash the loop.
pub fn record_iteration_feedback(
    conn: &Connection,
    shown_learning_ids: &[i64],
    outcome: &IterationOutcome,
) -> TaskMgrResult<()> {
    if shown_learning_ids.is_empty() {
        return Ok(());
    }

    if *outcome != IterationOutcome::Completed {
        return Ok(());
    }

    let mut applied_count = 0;
    for &learning_id in shown_learning_ids {
        match bandit::record_learning_applied(conn, learning_id) {
            Ok(()) => applied_count += 1,
            Err(e) => {
                eprintln!(
                    "Warning: failed to record learning {} as applied: {}",
                    learning_id, e
                );
            }
        }
    }

    if applied_count > 0 {
        eprintln!("Feedback: {} learnings marked as applied", applied_count);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learnings::bandit::get_window_stats;
    use crate::loop_engine::test_utils::{insert_test_learning, setup_test_db};

    /// Insert a test learning and initialize its window stats by calling
    /// `record_learning_shown`. Feedback tests require this because
    /// `record_learning_applied` updates window stats that must already exist.
    fn insert_learning_with_shown(conn: &Connection, title: &str) -> i64 {
        let id = insert_test_learning(conn, title);
        bandit::record_learning_shown(conn, id, 1).unwrap();
        id
    }

    // --- AC: Completed outcome + shown learning IDs -> calls record_learning_applied for each ---

    #[test]
    fn test_completed_outcome_records_applied_for_each_learning() {
        let (_temp_dir, conn) = setup_test_db();
        let id1 = insert_learning_with_shown(&conn, "Learning 1");
        let id2 = insert_learning_with_shown(&conn, "Learning 2");

        let shown_ids = vec![id1, id2];
        record_iteration_feedback(&conn, &shown_ids, &IterationOutcome::Completed).unwrap();

        // Verify both learnings had their window_applied incremented
        let stats1 = get_window_stats(&conn, id1).unwrap();
        assert_eq!(stats1.window_applied, 1, "Learning 1 should have 1 applied");

        let stats2 = get_window_stats(&conn, id2).unwrap();
        assert_eq!(stats2.window_applied, 1, "Learning 2 should have 1 applied");
    }

    // --- AC: Failed outcome + shown learning IDs -> does NOT call record_learning_applied ---

    #[test]
    fn test_blocked_outcome_does_not_record_applied() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Learning 1");

        let shown_ids = vec![id];
        record_iteration_feedback(&conn, &shown_ids, &IterationOutcome::Blocked).unwrap();

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 0,
            "Blocked outcome should not record applied"
        );
    }

    #[test]
    fn test_crash_outcome_does_not_record_applied() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Learning 1");

        let shown_ids = vec![id];
        let crash = IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError);
        record_iteration_feedback(&conn, &shown_ids, &crash).unwrap();

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 0,
            "Crash outcome should not record applied"
        );
    }

    #[test]
    fn test_rate_limit_outcome_does_not_record_applied() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Learning 1");

        let shown_ids = vec![id];
        record_iteration_feedback(&conn, &shown_ids, &IterationOutcome::RateLimit).unwrap();

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 0,
            "RateLimit outcome should not record applied"
        );
    }

    #[test]
    fn test_reorder_outcome_does_not_record_applied() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Learning 1");

        let shown_ids = vec![id];
        let reorder = IterationOutcome::Reorder("FEAT-001".to_string());
        record_iteration_feedback(&conn, &shown_ids, &reorder).unwrap();

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 0,
            "Reorder outcome should not record applied"
        );
    }

    #[test]
    fn test_no_eligible_tasks_outcome_does_not_record_applied() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Learning 1");

        let shown_ids = vec![id];
        record_iteration_feedback(&conn, &shown_ids, &IterationOutcome::NoEligibleTasks).unwrap();

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 0,
            "NoEligibleTasks outcome should not record applied"
        );
    }

    #[test]
    fn test_empty_outcome_does_not_record_applied() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Learning 1");

        let shown_ids = vec![id];
        record_iteration_feedback(&conn, &shown_ids, &IterationOutcome::Empty).unwrap();

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 0,
            "Empty outcome should not record applied"
        );
    }

    // --- AC: Empty shown_learnings list -> no-op (no DB calls) ---

    #[test]
    fn test_empty_shown_learnings_is_noop() {
        let (_temp_dir, conn) = setup_test_db();

        // Should not error even with no learnings in DB
        let result = record_iteration_feedback(&conn, &[], &IterationOutcome::Completed);
        assert!(result.is_ok(), "Empty learning IDs should be a no-op");
    }

    #[test]
    fn test_empty_shown_learnings_with_failure_outcome() {
        let (_temp_dir, conn) = setup_test_db();

        let result = record_iteration_feedback(&conn, &[], &IterationOutcome::Blocked);
        assert!(
            result.is_ok(),
            "Empty learning IDs with failure should be a no-op"
        );
    }

    // --- Additional edge cases ---

    #[test]
    fn test_multiple_completions_accumulate_applied_count() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Learning 1");

        // Simulate multiple successful iterations showing the same learning
        for _ in 0..3 {
            record_iteration_feedback(&conn, &[id], &IterationOutcome::Completed).unwrap();
        }

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 3,
            "Three completions should give 3 applied"
        );
    }

    #[test]
    fn test_nonexistent_learning_id_degrades_gracefully() {
        let (_temp_dir, conn) = setup_test_db();

        // Use an ID that doesn't exist in the DB
        let result = record_iteration_feedback(&conn, &[99999], &IterationOutcome::Completed);
        // Should not error -- graceful degradation (the UPDATE affects 0 rows)
        assert!(
            result.is_ok(),
            "Non-existent learning ID should degrade gracefully"
        );
    }

    // === Comprehensive tests (TEST-005) ===

    // --- AC: Feedback across multiple iterations ---
    // Shown in iteration 1, failure; shown again in iteration 3, success.
    // Only iteration 3's completion should record applied.

    #[test]
    fn test_multi_iteration_only_completed_records() {
        let (_temp_dir, conn) = setup_test_db();
        let id1 = insert_learning_with_shown(&conn, "Learning A");
        let id2 = insert_learning_with_shown(&conn, "Learning B");

        // Iteration 1: show both, outcome = Crash → no applied
        record_iteration_feedback(
            &conn,
            &[id1, id2],
            &IterationOutcome::Crash(crate::loop_engine::config::CrashType::OomOrKilled),
        )
        .unwrap();

        let stats1 = get_window_stats(&conn, id1).unwrap();
        assert_eq!(stats1.window_applied, 0, "Crash should not record applied");

        // Iteration 2: show only id1, outcome = Blocked → no applied
        record_iteration_feedback(&conn, &[id1], &IterationOutcome::Blocked).unwrap();
        let stats1 = get_window_stats(&conn, id1).unwrap();
        assert_eq!(
            stats1.window_applied, 0,
            "Blocked should not record applied"
        );

        // Iteration 3: show only id2, outcome = Completed → id2 recorded, id1 NOT
        record_iteration_feedback(&conn, &[id2], &IterationOutcome::Completed).unwrap();
        let stats1 = get_window_stats(&conn, id1).unwrap();
        let stats2 = get_window_stats(&conn, id2).unwrap();
        assert_eq!(
            stats1.window_applied, 0,
            "id1 not shown in completed iteration"
        );
        assert_eq!(stats2.window_applied, 1, "id2 shown in completed iteration");
    }

    // --- AC: Mixed valid/invalid learning IDs → partial success ---

    #[test]
    fn test_mixed_valid_invalid_ids_partial_success() {
        let (_temp_dir, conn) = setup_test_db();
        let valid_id = insert_learning_with_shown(&conn, "Valid Learning");

        // Mix of valid ID + non-existent IDs
        let ids = vec![99990, valid_id, 99991, 99992];
        let result = record_iteration_feedback(&conn, &ids, &IterationOutcome::Completed);
        assert!(result.is_ok(), "Should not fail on mixed valid/invalid IDs");

        // Valid ID should still have been applied
        let stats = get_window_stats(&conn, valid_id).unwrap();
        assert_eq!(
            stats.window_applied, 1,
            "Valid learning should be recorded even with invalid siblings"
        );
    }

    // --- AC: All CrashType variants don't record ---

    #[test]
    fn test_oom_crash_does_not_record() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "OOM test");
        let crash = IterationOutcome::Crash(crate::loop_engine::config::CrashType::OomOrKilled);
        record_iteration_feedback(&conn, &[id], &crash).unwrap();
        assert_eq!(get_window_stats(&conn, id).unwrap().window_applied, 0);
    }

    #[test]
    fn test_segfault_crash_does_not_record() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Segfault test");
        let crash = IterationOutcome::Crash(crate::loop_engine::config::CrashType::Segfault);
        record_iteration_feedback(&conn, &[id], &crash).unwrap();
        assert_eq!(get_window_stats(&conn, id).unwrap().window_applied, 0);
    }

    #[test]
    fn test_ratelimit_crash_does_not_record() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "RateLimit crash test");
        let crash = IterationOutcome::Crash(crate::loop_engine::config::CrashType::RateLimit);
        record_iteration_feedback(&conn, &[id], &crash).unwrap();
        assert_eq!(get_window_stats(&conn, id).unwrap().window_applied, 0);
    }

    // --- AC: Large batch of learning IDs ---

    #[test]
    fn test_large_batch_of_learning_ids() {
        let (_temp_dir, conn) = setup_test_db();
        let mut ids = Vec::new();
        for i in 0..20 {
            ids.push(insert_learning_with_shown(
                &conn,
                &format!("Learning {}", i),
            ));
        }

        record_iteration_feedback(&conn, &ids, &IterationOutcome::Completed).unwrap();

        for &id in &ids {
            let stats = get_window_stats(&conn, id).unwrap();
            assert_eq!(
                stats.window_applied, 1,
                "Each learning in batch should be applied once"
            );
        }
    }

    // --- Sequence: failure then success on same learning ---

    #[test]
    fn test_failure_then_success_records_only_on_success() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Resilient Learning");

        // 5 failures
        for _ in 0..5 {
            record_iteration_feedback(&conn, &[id], &IterationOutcome::NoEligibleTasks).unwrap();
        }
        assert_eq!(get_window_stats(&conn, id).unwrap().window_applied, 0);

        // Then success
        record_iteration_feedback(&conn, &[id], &IterationOutcome::Completed).unwrap();
        assert_eq!(get_window_stats(&conn, id).unwrap().window_applied, 1);
    }

    // --- Duplicate learning IDs in one call ---

    #[test]
    fn test_duplicate_ids_in_single_call() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Dup Learning");

        // Same ID appears 3 times
        record_iteration_feedback(&conn, &[id, id, id], &IterationOutcome::Completed).unwrap();

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 3,
            "Duplicate IDs should each increment applied count"
        );
    }

    // --- All outcome types exhaustively verified (completeness check) ---

    #[test]
    fn test_every_non_completed_outcome_is_noop() {
        let (_temp_dir, conn) = setup_test_db();
        let id = insert_learning_with_shown(&conn, "Exhaustive test");

        let non_completed_outcomes = vec![
            IterationOutcome::Blocked,
            IterationOutcome::Reorder("FEAT-999".to_string()),
            IterationOutcome::RateLimit,
            IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError),
            IterationOutcome::Crash(crate::loop_engine::config::CrashType::OomOrKilled),
            IterationOutcome::Crash(crate::loop_engine::config::CrashType::Segfault),
            IterationOutcome::Crash(crate::loop_engine::config::CrashType::RateLimit),
            IterationOutcome::NoEligibleTasks,
            IterationOutcome::Empty,
        ];

        for outcome in &non_completed_outcomes {
            record_iteration_feedback(&conn, &[id], outcome).unwrap();
        }

        let stats = get_window_stats(&conn, id).unwrap();
        assert_eq!(
            stats.window_applied, 0,
            "No non-Completed outcome should record applied, but got {}",
            stats.window_applied
        );
    }
}
