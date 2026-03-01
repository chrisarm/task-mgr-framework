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

use super::output::{format_retire_text, format_unretire_text};
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

// ──────────────────────────────────────────────────────────────────────────────
// TEST-002: Additional edge cases
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_retire_threshold_zero_min_age_retires_recent() {
    // Edge: min_age_days=0 means even brand-new low-conf learnings are candidates
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "New low-conf",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    // No set_age_days — learning was just created (0 days old)

    let params = RetireParams {
        min_age_days: 0,
        ..RetireParams::default()
    };
    let result = curate_retire(&conn, params).expect("curate_retire min_age=0");

    assert!(
        result.candidates.iter().any(|c| c.id == id),
        "with min_age_days=0, even brand-new low-conf unapplied learnings must be candidates"
    );
}

#[test]
fn test_retire_threshold_zero_min_shows_retires_unshown() {
    // Edge: min_shows=0 — criterion 2 matches anything with times_applied=0
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Never shown",
        Confidence::High,
        LearningOutcome::Success,
    );
    // times_shown=0, times_applied=0 by default

    let params = RetireParams {
        min_shows: 0,
        ..RetireParams::default()
    };
    let result = curate_retire(&conn, params).expect("curate_retire min_shows=0");

    assert!(
        result.candidates.iter().any(|c| c.id == id),
        "with min_shows=0, any unapplied learning must match criterion 2"
    );
}

#[test]
fn test_retire_all_learnings_are_candidates() {
    // All learnings match at least one criterion — all should be retired
    let (_dir, conn) = setup_db();

    let id1 = insert_learning(&conn, "C1", Confidence::Low, LearningOutcome::Pattern);
    let id2 = insert_learning(&conn, "C2", Confidence::Low, LearningOutcome::Pattern);
    set_age_days(&conn, id1, 91);
    set_age_days(&conn, id2, 91);

    let params = RetireParams {
        dry_run: false,
        ..RetireParams::default()
    };
    let result = curate_retire(&conn, params).expect("curate_retire all candidates");

    assert_eq!(
        result.candidates_found, 2,
        "both learnings must be candidates"
    );
    assert_eq!(result.learnings_retired, 2, "both must be retired");
    assert!(is_retired(&conn, id1), "id1 must be retired");
    assert!(is_retired(&conn, id2), "id2 must be retired");
}

#[test]
fn test_retire_database_has_only_retired_learnings() {
    // When all learnings are already retired, candidates_found must be 0
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Already retired",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 91);
    retire_learning(&conn, id);

    let result = curate_retire(&conn, RetireParams::default()).expect("curate_retire only retired");

    assert_eq!(
        result.candidates_found, 0,
        "no active learnings = 0 candidates"
    );
    assert_eq!(result.learnings_retired, 0);
    assert!(result.candidates.is_empty());
}

#[test]
fn test_unretire_empty_id_list_is_noop() {
    // unretire([]) must return empty restored and empty errors
    let (_dir, conn) = setup_db();

    let result = curate_unretire(&conn, vec![]).expect("curate_unretire empty list");

    assert!(
        result.restored.is_empty(),
        "restored must be empty for empty input"
    );
    assert!(
        result.errors.is_empty(),
        "errors must be empty for empty input"
    );
}

#[test]
fn test_unretire_mix_valid_and_invalid_ids_partial_success() {
    // unretire with one valid retired ID and one invalid ID — partial success
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Valid retired",
        Confidence::Medium,
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);

    let result = curate_unretire(&conn, vec![id, 99999]).expect("curate_unretire partial");

    assert!(
        result.restored.contains(&id),
        "valid retired ID must be restored"
    );
    assert!(
        !result.errors.is_empty(),
        "must have error for invalid ID 99999"
    );
    assert!(
        result.errors.iter().any(|e| e.contains("99999")),
        "error must identify the missing ID"
    );
    assert!(!is_retired(&conn, id), "valid ID must be unretired");
}

#[test]
fn test_retire_dry_run_text_format() {
    // Dry-run text output must include candidate count and "no changes made"
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Dry candidate",
        Confidence::Low,
        LearningOutcome::Pattern,
    );
    set_age_days(&conn, id, 91);

    let params = RetireParams {
        dry_run: true,
        ..RetireParams::default()
    };
    let result = curate_retire(&conn, params).expect("curate_retire dry run");
    let text = format_retire_text(&result);

    assert!(
        text.contains("Dry run"),
        "dry-run text must start with 'Dry run'"
    );
    assert!(
        text.contains("no changes made"),
        "dry-run text must say 'no changes made'"
    );
    assert!(
        text.contains("1"),
        "dry-run text must include candidate count"
    );
    assert!(
        text.contains("Dry candidate"),
        "dry-run text must list the candidate title"
    );
}

#[test]
fn test_unretire_text_format_restored() {
    // AC5: unretire text output mentions restored IDs
    use super::types::UnretireResult;

    let result = UnretireResult {
        restored: vec![42, 99],
        errors: vec![],
    };
    let text = format_unretire_text(&result);
    assert!(
        text.contains("Restored"),
        "unretire text must say 'Restored': {text}"
    );
    assert!(text.contains("2"), "must include count of restored: {text}");
}

#[test]
fn test_unretire_text_format_error() {
    // AC5: unretire text output includes error messages when present
    use super::types::UnretireResult;

    let result = UnretireResult {
        restored: vec![],
        errors: vec!["Learning 999 not found".to_string()],
    };
    let text = format_unretire_text(&result);
    assert!(
        text.contains("Error"),
        "unretire text must show errors: {text}"
    );
    assert!(
        text.contains("999"),
        "error text must identify the missing ID: {text}"
    );
}

#[test]
fn test_retire_result_json_serialization() {
    // RetireResult must serialize to JSON with all expected fields
    use super::types::RetireResult;
    use super::types::RetirementCandidate;

    let result = RetireResult {
        dry_run: true,
        candidates_found: 1,
        learnings_retired: 0,
        candidates: vec![RetirementCandidate {
            id: 42,
            title: "Test learning".to_string(),
            reason: "Some reason".to_string(),
        }],
    };

    let json = serde_json::to_string(&result).expect("serialize RetireResult");
    assert!(json.contains("\"dry_run\""), "must have dry_run field");
    assert!(
        json.contains("\"candidates_found\""),
        "must have candidates_found field"
    );
    assert!(
        json.contains("\"learnings_retired\""),
        "must have learnings_retired field"
    );
    assert!(
        json.contains("\"candidates\""),
        "must have candidates field"
    );
    assert!(json.contains("\"id\""), "candidate must have id field");
    assert!(
        json.contains("\"title\""),
        "candidate must have title field"
    );
    assert!(
        json.contains("\"reason\""),
        "candidate must have reason field"
    );
}

#[test]
fn test_unretire_result_json_serialization() {
    use super::types::UnretireResult;

    let result = UnretireResult {
        restored: vec![1, 2, 3],
        errors: vec!["Learning 99 not found".to_string()],
    };

    let json = serde_json::to_string(&result).expect("serialize UnretireResult");
    assert!(json.contains("\"restored\""), "must have restored field");
    assert!(json.contains("\"errors\""), "must have errors field");
    assert!(json.contains("99"), "errors content must be present");
}

// ──────────────────────────────────────────────────────────────────────────────
// TEST-INIT-002: Enrich metadata query and field filter
//
// All tests are #[ignore] until FEAT-003 implements find_enrichment_candidates.
// ──────────────────────────────────────────────────────────────────────────────

use super::{find_enrichment_candidates, EnrichParams};
use crate::commands::curate::types::EnrichFieldFilter;

/// Sets `applies_to_files` on a learning to a JSON array (simulates enriched field).
fn set_applies_to_files(conn: &Connection, id: i64, value: Option<&str>) {
    conn.execute(
        "UPDATE learnings SET applies_to_files = ?1 WHERE id = ?2",
        rusqlite::params![value, id],
    )
    .expect("set_applies_to_files");
}

/// Sets `applies_to_task_types` on a learning to a JSON array.
fn set_applies_to_task_types(conn: &Connection, id: i64, value: Option<&str>) {
    conn.execute(
        "UPDATE learnings SET applies_to_task_types = ?1 WHERE id = ?2",
        rusqlite::params![value, id],
    )
    .expect("set_applies_to_task_types");
}

/// Sets `applies_to_errors` on a learning to a JSON array.
fn set_applies_to_errors(conn: &Connection, id: i64, value: Option<&str>) {
    conn.execute(
        "UPDATE learnings SET applies_to_errors = ?1 WHERE id = ?2",
        rusqlite::params![value, id],
    )
    .expect("set_applies_to_errors");
}

#[test]
fn test_enrich_query_returns_learning_with_null_files() {
    // AC: query returns learnings where applies_to_files IS NULL
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Missing files",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    // applies_to_files is NULL by default

    let params = EnrichParams::default();
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates");

    assert!(
        candidates.iter().any(|c| c.id == id),
        "learning with NULL applies_to_files must be a candidate"
    );
}

#[test]
fn test_enrich_query_returns_learning_with_null_task_types() {
    // AC: query returns learnings where applies_to_task_types IS NULL
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Missing task types",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    // Set files so only task_types is NULL
    set_applies_to_files(&conn, id, Some("[\"src/**/*.rs\"]"));

    let params = EnrichParams::default();
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates");

    assert!(
        candidates.iter().any(|c| c.id == id),
        "learning with NULL applies_to_task_types must be a candidate"
    );
}

#[test]
fn test_enrich_query_returns_learning_with_null_errors() {
    // AC: query returns learnings where applies_to_errors IS NULL
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Missing errors",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    // Set files and task_types so only errors is NULL
    set_applies_to_files(&conn, id, Some("[\"src/**/*.rs\"]"));
    set_applies_to_task_types(&conn, id, Some("[\"FEAT-\"]"));

    let params = EnrichParams::default();
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates");

    assert!(
        candidates.iter().any(|c| c.id == id),
        "learning with NULL applies_to_errors must be a candidate"
    );
}

#[test]
fn test_enrich_query_excludes_retired_learnings() {
    // AC: query excludes retired learnings (retired_at IS NOT NULL)
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Retired with nulls",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    // All metadata fields are NULL — but learning is retired
    retire_learning(&conn, id);

    let params = EnrichParams::default();
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates");

    assert!(
        candidates.iter().all(|c| c.id != id),
        "retired learning must NOT be returned even if metadata fields are NULL"
    );
}

#[test]
fn test_enrich_query_excludes_fully_enriched_learnings() {
    // AC: query excludes learnings where all 3 fields are populated
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Fully enriched",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_applies_to_files(&conn, id, Some("[\"src/**/*.rs\"]"));
    set_applies_to_task_types(&conn, id, Some("[\"FEAT-\"]"));
    set_applies_to_errors(&conn, id, Some("[\"E0001\"]"));

    let params = EnrichParams::default();
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates");

    assert!(
        candidates.iter().all(|c| c.id != id),
        "fully-enriched learning (all 3 fields set) must NOT be a candidate"
    );
}

#[test]
fn test_enrich_field_filter_files_restricts_to_missing_files() {
    // AC: --field=applies_to_files restricts to learnings missing only that field
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Missing files only",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    // applies_to_files is NULL, task_types and errors set
    set_applies_to_task_types(&conn, id, Some("[\"FEAT-\"]"));
    set_applies_to_errors(&conn, id, Some("[\"E0001\"]"));

    let params = EnrichParams {
        field_filter: Some(EnrichFieldFilter::AppliesToFiles),
        ..EnrichParams::default()
    };
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates");

    assert!(
        candidates.iter().any(|c| c.id == id),
        "--field=applies_to_files must return learning with NULL applies_to_files"
    );
}

#[test]
fn test_enrich_field_filter_files_known_bad_discriminator() {
    // Known-bad discriminator: --field=applies_to_files must NOT return a learning
    // that has applies_to_files set but applies_to_task_types NULL
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Has files, missing task_types",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_applies_to_files(&conn, id, Some("[\"src/**/*.rs\"]"));
    // applies_to_task_types is NULL, applies_to_errors is NULL

    let params = EnrichParams {
        field_filter: Some(EnrichFieldFilter::AppliesToFiles),
        ..EnrichParams::default()
    };
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates");

    assert!(
        candidates.iter().all(|c| c.id != id),
        "--field=applies_to_files must NOT return learning that has applies_to_files set (even if task_types is NULL)"
    );
}

#[test]
fn test_enrich_zero_candidates_returns_empty_vec() {
    // AC: 0 matching learnings returns empty vec (no error)
    let (_dir, conn) = setup_db();

    // Insert only a fully-enriched learning (no candidates)
    let id = insert_learning(
        &conn,
        "Fully enriched",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_applies_to_files(&conn, id, Some("[\"src/**/*.rs\"]"));
    set_applies_to_task_types(&conn, id, Some("[\"FEAT-\"]"));
    set_applies_to_errors(&conn, id, Some("[\"E0001\"]"));

    let params = EnrichParams::default();
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates");

    assert!(
        candidates.is_empty(),
        "no candidates expected when all learnings are fully enriched"
    );
}

#[test]
fn test_enrich_empty_database_returns_empty_vec() {
    // Edge case: 0 learnings in database — return empty vec, no error
    let (_dir, conn) = setup_db();

    let params = EnrichParams::default();
    let candidates =
        find_enrichment_candidates(&conn, &params).expect("find_enrichment_candidates on empty db");

    assert!(
        candidates.is_empty(),
        "empty db must return empty candidates"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// TEST-INIT-003: Enrich LLM prompt building and response parsing
//
// All tests are #[ignore] until FEAT-004 (build_enrich_prompt) and
// FEAT-005 (parse_enrich_response) are implemented.
// ──────────────────────────────────────────────────────────────────────────────

use super::enrich::{build_enrich_prompt, parse_enrich_response, EnrichBatchItem};

/// Returns a minimal batch of two items for prompt/parser tests.
fn make_batch() -> Vec<EnrichBatchItem> {
    vec![
        EnrichBatchItem {
            id: 1,
            title: "SQLite busy error".to_string(),
            content: "Concurrent writes cause SQLITE_BUSY due to missing busy_timeout.".to_string(),
            existing_tags: vec!["sqlite".to_string(), "concurrency".to_string()],
        },
        EnrichBatchItem {
            id: 2,
            title: "Use Result for fallible ops".to_string(),
            content: "Always return Result from functions that can fail.".to_string(),
            existing_tags: vec![],
        },
    ]
}

#[test]
fn test_enrich_prompt_contains_uuid_delimiter() {
    // AC: prompt wraps learning content with a random UUID-based delimiter
    let batch = make_batch();
    let prompt = build_enrich_prompt(&batch);

    assert!(
        prompt.contains("===BOUNDARY_"),
        "prompt must contain a UUID-based boundary delimiter for injection protection"
    );
}

#[test]
fn test_enrich_prompt_contains_untrusted_warning() {
    // AC: prompt includes UNTRUSTED warning to guard against prompt injection
    let batch = make_batch();
    let prompt = build_enrich_prompt(&batch);

    assert!(
        prompt.contains("UNTRUSTED"),
        "prompt must contain UNTRUSTED warning for injection protection"
    );
}

#[test]
fn test_enrich_prompt_includes_learning_id_title_content_tags() {
    // AC: prompt includes ID, title, content, and existing tags for each batch item
    let batch = make_batch();
    let prompt = build_enrich_prompt(&batch);

    // Item 1
    assert!(prompt.contains("1"), "prompt must include learning ID 1");
    assert!(
        prompt.contains("SQLite busy error"),
        "prompt must include learning title"
    );
    assert!(
        prompt.contains("SQLITE_BUSY"),
        "prompt must include learning content"
    );
    assert!(
        prompt.contains("sqlite"),
        "prompt must include existing tags"
    );

    // Item 2
    assert!(prompt.contains("2"), "prompt must include learning ID 2");
    assert!(
        prompt.contains("Use Result for fallible ops"),
        "prompt must include second learning title"
    );
}

#[test]
fn test_enrich_prompt_requests_json_with_expected_field_names() {
    // AC: prompt requests a JSON response with specific field names
    let batch = make_batch();
    let prompt = build_enrich_prompt(&batch);

    assert!(
        prompt.contains("learning_id"),
        "prompt must request 'learning_id' field in JSON response"
    );
    assert!(
        prompt.contains("applies_to_files"),
        "prompt must request 'applies_to_files' field"
    );
    assert!(
        prompt.contains("applies_to_task_types"),
        "prompt must request 'applies_to_task_types' field"
    );
    assert!(
        prompt.contains("applies_to_errors"),
        "prompt must request 'applies_to_errors' field"
    );
}

#[test]
fn test_parse_enrich_valid_json_array() {
    // AC: parser returns proposals from a valid JSON array
    let batch_ids = vec![1i64, 2];
    let response = r#"[
        {
            "learning_id": 1,
            "applies_to_files": ["src/db/*.rs"],
            "applies_to_task_types": ["FEAT-", "FIX-"],
            "applies_to_errors": ["SQLITE_BUSY"],
            "applies_to_tags": ["sqlite"]
        },
        {
            "learning_id": 2,
            "applies_to_files": ["src/**/*.rs"],
            "applies_to_task_types": [],
            "applies_to_errors": [],
            "applies_to_tags": []
        }
    ]"#;

    let result = parse_enrich_response(response, &batch_ids).unwrap();

    assert_eq!(result.len(), 2, "must return 2 proposals");
    let p1 = result
        .iter()
        .find(|p| p.learning_id == 1)
        .expect("proposal for id=1");
    assert!(
        p1.proposed_files.contains(&"src/db/*.rs".to_string()),
        "proposal must include proposed files"
    );
    assert!(
        p1.proposed_task_types.contains(&"FEAT-".to_string()),
        "proposal must include proposed task types"
    );
}

#[test]
fn test_parse_enrich_garbage_response_returns_empty() {
    // AC: parser returns empty vec on non-JSON garbage (no crash)
    let batch_ids = vec![1i64, 2, 3];
    let result = parse_enrich_response("not json at all", &batch_ids).unwrap();

    assert!(
        result.is_empty(),
        "garbage response must return empty vec, not crash"
    );
}

#[test]
fn test_parse_enrich_markdown_code_block_json() {
    // AC: parser handles JSON wrapped in markdown code block
    let batch_ids = vec![1i64];
    let response = r#"Here are my suggestions:

```json
[{"learning_id": 1, "applies_to_files": ["src/**/*.rs"], "applies_to_task_types": ["TEST-"], "applies_to_errors": [], "applies_to_tags": []}]
```

Let me know if you need anything else."#;

    let result = parse_enrich_response(response, &batch_ids).unwrap();

    assert_eq!(
        result.len(),
        1,
        "must parse proposal from markdown code block"
    );
    assert_eq!(result[0].learning_id, 1);
}

#[test]
fn test_parse_enrich_rejects_hallucinated_id() {
    // Known-bad discriminator: parser must reject a response that references
    // learning ID 999 when the batch only contains IDs [1, 2, 3].
    let batch_ids = vec![1i64, 2, 3];
    let response = r#"[
        {"learning_id": 1, "applies_to_files": ["src/**/*.rs"], "applies_to_task_types": [], "applies_to_errors": [], "applies_to_tags": []},
        {"learning_id": 999, "applies_to_files": ["src/**/*.rs"], "applies_to_task_types": [], "applies_to_errors": [], "applies_to_tags": []}
    ]"#;

    let result = parse_enrich_response(response, &batch_ids).unwrap();

    assert!(
        result.iter().all(|p| p.learning_id != 999),
        "hallucinated ID 999 (not in batch [1,2,3]) must be rejected"
    );
    assert_eq!(
        result.len(),
        1,
        "only the valid proposal (id=1) should be returned"
    );
}
