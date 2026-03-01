//! Tests for curate retire and curate unretire commands.
//!
//! TEST-INIT-002: Retirement candidate identification (three criteria, dry-run, thresholds).
//! TEST-INIT-003: Unretire (restore retired learnings, error handling).
//!
//! All tests are #[ignore] until the following are implemented:
//!   FEAT-001: retired_at column migration (v8)
//!   FEAT-003: CLI scaffolding + types
//!   FEAT-004: curate_retire() implementation
//!   FEAT-005: curate_unretire() implementation

use rusqlite::Connection;

use crate::db::{create_schema, open_connection};
use crate::learnings::{record_learning, RecordLearningParams};
use crate::models::{Confidence, LearningOutcome};

use super::{curate_retire, curate_unretire, RetireParams};

// ──────────────────────────────────────────────────────────────────────────────
// Test helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Creates an in-memory test database with schema and all migrations applied.
///
/// Uses a temp-file-backed connection (not `:memory:`) so that `open_connection`
/// can apply migrations via the normal path.
fn setup_db() -> (tempfile::TempDir, Connection) {
    use crate::db::migrations::run_migrations;
    let temp_dir = tempfile::TempDir::new().expect("create temp dir");
    let mut conn = open_connection(temp_dir.path()).expect("open connection");
    create_schema(&conn).expect("create schema");
    run_migrations(&mut conn).expect("run migrations");
    (temp_dir, conn)
}

/// Inserts a learning with default fields and returns its ID.
fn insert_learning(
    conn: &Connection,
    title: &str,
    confidence: Confidence,
    outcome: LearningOutcome,
) -> i64 {
    let params = RecordLearningParams {
        outcome,
        title: title.to_string(),
        content: "Test content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence,
    };
    record_learning(conn, params)
        .expect("insert learning")
        .learning_id
}

/// Sets `created_at` to `days` days ago on a learning (for age-based tests).
fn set_age_days(conn: &Connection, id: i64, days: u32) {
    conn.execute(
        "UPDATE learnings SET created_at = datetime('now', ?1) WHERE id = ?2",
        rusqlite::params![format!("-{} days", days), id],
    )
    .expect("set_age_days: requires FEAT-001 (retired_at column) and valid learning id");
}

/// Sets `times_shown` and `times_applied` on a learning.
fn set_show_stats(conn: &Connection, id: i64, times_shown: i32, times_applied: i32) {
    conn.execute(
        "UPDATE learnings SET times_shown = ?1, times_applied = ?2 WHERE id = ?3",
        rusqlite::params![times_shown, times_applied, id],
    )
    .expect("set_show_stats");
}

/// Sets `retired_at` to now on a learning (simulates a prior retirement).
fn retire_learning(conn: &Connection, id: i64) {
    conn.execute(
        "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
        [id],
    )
    .expect("retire_learning: requires FEAT-001 (retired_at column)");
}

/// Returns true if `retired_at` IS NOT NULL for the given learning.
fn is_retired(conn: &Connection, id: i64) -> bool {
    conn.query_row(
        "SELECT retired_at IS NOT NULL FROM learnings WHERE id = ?1",
        [id],
        |row| row.get::<_, bool>(0),
    )
    .expect("is_retired query")
}

// ──────────────────────────────────────────────────────────────────────────────
// TEST-INIT-002: Retirement candidate identification
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_criterion1_old_low_confidence_unapplied_is_candidate() {
    // AC: learning matching criterion 1 (age >= 90 days, confidence=low, times_applied=0) is a candidate
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Old low-conf learning",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 91); // 91 days old — over the 90-day threshold

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert_eq!(result.candidates_found, 1, "one candidate expected");
    assert!(
        result.candidates.iter().any(|c| c.id == id),
        "criterion-1 learning must be a candidate"
    );
}

#[test]
fn test_criterion1_high_confidence_not_candidate() {
    // Known-bad discriminator: high-confidence learning with 0 applications must NOT match criterion 1
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Old high-conf learning",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 91);
    // times_applied is already 0 by default

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert!(
        result.candidates.iter().all(|c| c.id != id),
        "high-confidence learning must NOT be retired by criterion 1 (confidence=low required)"
    );
}

#[test]
fn test_criterion1_boundary_89_days_not_candidate() {
    // Edge case: 89 days old is below the 90-day threshold — must NOT be a candidate
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Almost-old low-conf",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 89); // one day short

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert!(
        result.candidates.iter().all(|c| c.id != id),
        "89-day-old learning must NOT be a candidate (threshold is >= 90)"
    );
}

#[test]
fn test_criterion2_shown_never_applied_is_candidate() {
    // AC: learning matching criterion 2 (times_shown >= 10, times_applied=0) is a candidate
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Shown-never-applied",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_show_stats(&conn, id, 10, 0); // exactly at threshold

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert!(
        result.candidates.iter().any(|c| c.id == id),
        "criterion-2 learning (times_shown=10, times_applied=0) must be a candidate"
    );
}

#[test]
fn test_criterion2_shown_9_times_not_candidate() {
    // Edge case: times_shown=9 is below min_shows=10 — must NOT be a candidate
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Shown-9-times",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_show_stats(&conn, id, 9, 0);

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert!(
        result.candidates.iter().all(|c| c.id != id),
        "times_shown=9 must NOT be a candidate (threshold is >= 10)"
    );
}

#[test]
fn test_criterion3_low_application_rate_is_candidate() {
    // AC: learning matching criterion 3 (times_shown >= 20, application rate < 0.05) is a candidate
    // Rate = 1/20 = 0.05 — NOT < 0.05 (must be strictly less), so use 1/21 ≈ 0.0476
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Low-rate learning",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_show_stats(&conn, id, 21, 1); // rate ≈ 0.0476 < 0.05

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert!(
        result.candidates.iter().any(|c| c.id == id),
        "criterion-3 learning (rate ≈ 0.0476) must be a candidate"
    );
}

#[test]
fn test_criterion3_rate_exactly_at_threshold_not_candidate() {
    // Edge case: rate = 0.05 exactly is NOT < 0.05, so must NOT be a candidate
    // 1/20 = 0.05 exactly
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Exactly-threshold rate",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_show_stats(&conn, id, 20, 1); // rate = 1/20 = 0.05

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert!(
        result.candidates.iter().all(|c| c.id != id),
        "rate exactly 0.05 must NOT be a candidate (criterion requires strictly < 0.05)"
    );
}

#[test]
fn test_non_matching_learning_not_candidate() {
    // AC: learning NOT matching any criterion is NOT a candidate
    let (_dir, conn) = setup_db();

    // Recent, high confidence, applied frequently
    let id = insert_learning(
        &conn,
        "Healthy learning",
        Confidence::High,
        LearningOutcome::Success,
    );
    set_show_stats(&conn, id, 10, 8); // high application rate, criterion 2 won't match (applied > 0)
                                      // created_at is recent by default (criterion 1 won't match)

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert!(
        result.candidates.iter().all(|c| c.id != id),
        "healthy learning must NOT be a candidate"
    );
}

#[test]
fn test_dry_run_true_does_not_set_retired_at() {
    // AC: dry_run=true identifies candidates but does NOT set retired_at
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Dry-run candidate",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 91);

    let params = RetireParams {
        dry_run: true,
        ..RetireParams::default()
    };
    let result = curate_retire(&conn, params).expect("curate_retire dry_run=true");

    assert!(result.dry_run, "result must reflect dry_run=true");
    assert!(
        result.candidates_found > 0,
        "must identify at least one candidate"
    );
    assert_eq!(
        result.learnings_retired, 0,
        "dry_run must not retire any learnings"
    );
    assert!(
        !is_retired(&conn, id),
        "retired_at must remain NULL after dry run"
    );
}

#[test]
fn test_dry_run_false_sets_retired_at() {
    // AC: dry_run=false sets retired_at on all candidates
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "To be retired",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 91);

    let params = RetireParams {
        dry_run: false,
        ..RetireParams::default()
    };
    let result = curate_retire(&conn, params).expect("curate_retire dry_run=false");

    assert!(!result.dry_run, "result must reflect dry_run=false");
    assert!(
        result.learnings_retired > 0,
        "must retire at least one learning"
    );
    assert!(
        is_retired(&conn, id),
        "learning must have retired_at set after dry_run=false"
    );
}

#[test]
fn test_already_retired_excluded_from_candidates() {
    // Edge case: already-retired learning must not appear as candidate again
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Already retired",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 91);
    retire_learning(&conn, id); // manually retire first

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    assert!(
        result.candidates.iter().all(|c| c.id != id),
        "already-retired learning must NOT appear as a candidate"
    );
}

#[test]
fn test_zero_candidates_returns_empty_result() {
    // AC: 0 candidates returns empty result, no errors
    let (_dir, conn) = setup_db();

    // Insert a healthy learning that won't match any criterion
    insert_learning(&conn, "Healthy", Confidence::High, LearningOutcome::Success);

    let result =
        curate_retire(&conn, RetireParams::default()).expect("curate_retire with 0 candidates");

    assert_eq!(result.candidates_found, 0);
    assert_eq!(result.learnings_retired, 0);
    assert!(result.candidates.is_empty());
}

#[test]
fn test_zero_candidates_empty_database() {
    // Edge case: 0 learnings in database — return empty result, no error
    let (_dir, conn) = setup_db();

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire on empty db");

    assert_eq!(result.candidates_found, 0);
    assert!(result.candidates.is_empty());
}

#[test]
fn test_custom_thresholds_change_candidate_set() {
    // AC: custom thresholds (min_age_days, min_shows, max_rate) change candidate set
    let (_dir, conn) = setup_db();

    // This learning is 45 days old — only a candidate if min_age_days=30
    let id = insert_learning(
        &conn,
        "Moderate age low-conf",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 45);

    // With default threshold (90 days), it's NOT a candidate
    let result_default = curate_retire(&conn, RetireParams::default()).expect("default thresholds");
    assert!(
        result_default.candidates.iter().all(|c| c.id != id),
        "at 45 days with default 90-day threshold, must NOT be a candidate"
    );

    // With custom threshold (30 days), it IS a candidate
    let custom_params = RetireParams {
        min_age_days: 30,
        ..RetireParams::default()
    };
    let result_custom = curate_retire(&conn, custom_params).expect("custom thresholds");
    assert!(
        result_custom.candidates.iter().any(|c| c.id == id),
        "at 45 days with 30-day threshold, must be a candidate"
    );
}

#[test]
fn test_each_candidate_has_reason_string() {
    // Invariant: each candidate must have a human-readable reason string
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Candidate with reason",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 91);

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire");

    let candidate = result
        .candidates
        .iter()
        .find(|c| c.id == id)
        .expect("candidate must be present");
    assert!(
        !candidate.reason.is_empty(),
        "candidate must have a non-empty reason string"
    );
}

#[test]
fn test_candidates_found_matches_learnings_retired() {
    // Invariant: candidate count in result must match actual retired_at updates when dry_run=false
    let (_dir, conn) = setup_db();

    let id1 = insert_learning(
        &conn,
        "Candidate 1",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id1, 91);
    let id2 = insert_learning(
        &conn,
        "Candidate 2",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_show_stats(&conn, id2, 10, 0);

    let params = RetireParams {
        dry_run: false,
        ..RetireParams::default()
    };
    let result = curate_retire(&conn, params).expect("curate_retire dry_run=false");

    assert_eq!(
        result.candidates_found, result.learnings_retired,
        "candidates_found must equal learnings_retired when dry_run=false"
    );
    assert!(is_retired(&conn, id1), "id1 must be retired");
    assert!(is_retired(&conn, id2), "id2 must be retired");
}

// ──────────────────────────────────────────────────────────────────────────────
// TEST-INIT-003: Unretire
// ──────────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "requires FEAT-001 (retired_at migration), FEAT-003 (types), FEAT-005 (curate_unretire impl)"]
fn test_unretire_sets_retired_at_null() {
    // AC: unretire sets retired_at = NULL on a retired learning
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "To be unretired",
        Confidence::Medium,
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);
    assert!(
        is_retired(&conn, id),
        "pre-condition: learning must be retired"
    );

    let result = curate_unretire(&conn, vec![id]).expect("curate_unretire");

    assert!(result.restored.contains(&id), "id must be in restored list");
    assert!(
        result.errors.is_empty(),
        "no errors expected for valid retired learning"
    );
    assert!(
        !is_retired(&conn, id),
        "retired_at must be NULL after unretire"
    );
}

#[test]
#[ignore = "requires FEAT-001 (retired_at migration), FEAT-002 (filters), FEAT-003 (types), FEAT-005 (curate_unretire impl)"]
fn test_unretired_learning_reappears_in_list() {
    // AC: unretired learning reappears in recall/list queries
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Should reappear",
        Confidence::Medium,
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);

    // Verify excluded while retired
    let count_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("count before");

    curate_unretire(&conn, vec![id]).expect("curate_unretire");

    let count_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("count after");

    assert_eq!(
        count_after,
        count_before + 1,
        "unretired learning must reappear in active count"
    );
}

#[test]
#[ignore = "requires FEAT-001 (retired_at migration), FEAT-003 (types), FEAT-005 (curate_unretire impl)"]
fn test_unretire_nonexistent_id_returns_error() {
    // AC: unretire on non-existent ID returns appropriate error
    let (_dir, conn) = setup_db();

    let result =
        curate_unretire(&conn, vec![99999]).expect("curate_unretire returns Ok with errors");

    assert!(result.restored.is_empty(), "nothing should be restored");
    assert!(
        !result.errors.is_empty(),
        "must have an error for non-existent ID"
    );
    assert!(
        result.errors.iter().any(|e| e.contains("99999")),
        "error must identify the missing ID"
    );
}

#[test]
#[ignore = "requires FEAT-001 (retired_at migration), FEAT-003 (types), FEAT-005 (curate_unretire impl)"]
fn test_unretire_already_active_returns_error() {
    // AC: unretire on already-active learning returns error/no-op (must not silently succeed)
    // Known-bad discriminator: unretiring an already-active learning should not succeed silently
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Active learning",
        Confidence::Medium,
        LearningOutcome::Pattern,
    );
    // Do NOT retire it — it's active

    let result = curate_unretire(&conn, vec![id]).expect("curate_unretire returns Ok with errors");

    assert!(
        !result.restored.contains(&id),
        "active learning must not appear in restored"
    );
    assert!(
        !result.errors.is_empty(),
        "must return an error when trying to unretire an already-active learning"
    );
}

#[test]
#[ignore = "requires FEAT-001 (retired_at migration), FEAT-003 (types), FEAT-005 (curate_unretire impl)"]
fn test_unretire_multiple_ids() {
    // AC: unretire multiple IDs in one call
    let (_dir, conn) = setup_db();

    let id1 = insert_learning(
        &conn,
        "Retired 1",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    let id2 = insert_learning(
        &conn,
        "Retired 2",
        Confidence::Medium,
        LearningOutcome::Success,
    );
    retire_learning(&conn, id1);
    retire_learning(&conn, id2);

    let result = curate_unretire(&conn, vec![id1, id2]).expect("curate_unretire multiple");

    assert!(result.restored.contains(&id1), "id1 must be restored");
    assert!(result.restored.contains(&id2), "id2 must be restored");
    assert!(result.errors.is_empty(), "no errors for valid retired IDs");
    assert!(!is_retired(&conn, id1), "id1 must have retired_at = NULL");
    assert!(!is_retired(&conn, id2), "id2 must have retired_at = NULL");
}

#[test]
#[ignore = "requires FEAT-001 (retired_at migration), FEAT-003 (types), FEAT-005 (curate_unretire impl)"]
fn test_unretire_only_modifies_retired_at() {
    // Invariant: unretire must not modify any field other than retired_at
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Title preserved",
        Confidence::High,
        LearningOutcome::Failure,
    );
    set_show_stats(&conn, id, 5, 3);
    retire_learning(&conn, id);

    curate_unretire(&conn, vec![id]).expect("curate_unretire");

    let (title, times_shown, times_applied, confidence): (String, i32, i32, String) = conn
        .query_row(
            "SELECT title, times_shown, times_applied, confidence FROM learnings WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("query post-unretire state");

    assert_eq!(title, "Title preserved", "title must not change");
    assert_eq!(times_shown, 5, "times_shown must not change");
    assert_eq!(times_applied, 3, "times_applied must not change");
    assert_eq!(confidence, "high", "confidence must not change");
}
