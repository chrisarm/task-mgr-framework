//! Tests for curate retire, curate unretire, and curate dedup merge commands.
//!
//! TEST-INIT-001: Dedup cluster merge logic (merge_cluster function).
//! TEST-INIT-002: Retirement candidate identification (three criteria, dry-run, thresholds).
//! TEST-INIT-003: Unretire (restore retired learnings, error handling).
//!
//! TEST-INIT-001 tests are #[ignore] until FEAT-004 is implemented.
//! TEST-INIT-002/003 tests are #[ignore] until the following are implemented:
//!   FEAT-001: retired_at column migration (v8)
//!   FEAT-003: CLI scaffolding + types
//!   FEAT-004: curate_retire() implementation
//!   FEAT-005: curate_unretire() implementation

use rusqlite::Connection;

use crate::db::{create_schema, open_connection};
use crate::learnings::{record_learning, RecordLearningParams};
use crate::models::{Confidence, LearningOutcome};

use super::output::{format_retire_text, format_unretire_text};
use super::{
    build_dedup_prompt, curate_retire, curate_unretire, merge_cluster, parse_dedup_response,
    DeduplicateLearningItem, MergeClusterParams, RetireParams,
};

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

use super::enrich::{build_enrich_prompt, curate_enrich, parse_enrich_response, EnrichBatchItem};

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

// ──────────────────────────────────────────────────────────────────────────────
// TEST-002: curate_enrich handler and output formatting
// ──────────────────────────────────────────────────────────────────────────────

use super::output::format_enrich_text;
use super::types::{EnrichProposal, EnrichResult};

/// Helper: inserts a learning with all three metadata fields set (fully enriched).
fn insert_fully_enriched_learning(conn: &Connection, title: &str) -> i64 {
    let id = insert_learning(conn, title, Confidence::High, LearningOutcome::Pattern);
    set_applies_to_files(conn, id, Some("[\"src/**/*.rs\"]"));
    set_applies_to_task_types(conn, id, Some("[\"FEAT-\"]"));
    set_applies_to_errors(conn, id, Some("[\"E0001\"]"));
    id
}

/// Helper: constructs an EnrichProposal with the given learning_id and title.
fn make_enrich_proposal(learning_id: i64, title: &str) -> EnrichProposal {
    EnrichProposal {
        learning_id,
        learning_title: title.to_string(),
        proposed_files: vec!["src/**/*.rs".to_string()],
        proposed_task_types: vec!["FEAT-".to_string()],
        proposed_errors: vec![],
        proposed_tags: vec![],
    }
}

#[test]
fn test_curate_enrich_zero_candidates_returns_immediately() {
    // AC: curate_enrich with 0 candidates short-circuits and returns an empty result.
    // Invariant: no LLM call is made (batches_processed=0, llm_errors=0).
    let (_dir, conn) = setup_db();

    // Only a fully-enriched learning present — no candidates
    insert_fully_enriched_learning(&conn, "Fully enriched");

    let result = curate_enrich(
        &conn,
        EnrichParams {
            dry_run: false,
            batch_size: 20,
            field_filter: None,
        },
    )
    .expect("curate_enrich with 0 candidates");

    assert_eq!(result.total_candidates, 0, "must report 0 candidates");
    assert_eq!(result.batches_processed, 0, "no batches processed");
    assert_eq!(result.learnings_enriched, 0, "nothing enriched");
    assert_eq!(result.llm_errors, 0, "no LLM errors");
    assert!(result.proposals.is_empty(), "no proposals");
}

#[test]
fn test_curate_enrich_empty_database_zero_candidates() {
    // Edge: empty DB → 0 candidates, returns immediately without error.
    let (_dir, conn) = setup_db();

    let result = curate_enrich(&conn, EnrichParams::default()).expect("curate_enrich on empty db");

    assert_eq!(result.total_candidates, 0);
    assert!(result.proposals.is_empty());
}

#[test]
fn test_curate_enrich_rerun_excludes_already_enriched() {
    // AC: after partial enrichment, re-run only processes learnings with remaining NULL fields.
    // This tests that find_enrichment_candidates correctly excludes already-enriched learnings
    // so curate_enrich won't process them again.
    let (_dir, conn) = setup_db();

    // Learning 1: fully enriched — should be excluded
    let id_enriched = insert_fully_enriched_learning(&conn, "Already enriched");

    // Learning 2: still missing files — should be found
    let id_partial = insert_learning(&conn, "Partial", Confidence::High, LearningOutcome::Pattern);
    set_applies_to_task_types(&conn, id_partial, Some("[\"FEAT-\"]"));
    set_applies_to_errors(&conn, id_partial, Some("[\"E0001\"]"));
    // applies_to_files is NULL

    let candidates =
        find_enrichment_candidates(&conn, &EnrichParams::default()).expect("candidates");

    assert!(
        candidates.iter().all(|c| c.id != id_enriched),
        "already-enriched learning must be excluded from candidates"
    );
    assert!(
        candidates.iter().any(|c| c.id == id_partial),
        "partially-enriched learning with NULL files must remain a candidate"
    );
}

#[test]
fn test_curate_enrich_field_filter_zero_candidates_when_all_have_field() {
    // AC: --field=applies_to_files with all learnings having applies_to_files set → 0 candidates.
    let (_dir, conn) = setup_db();

    let id = insert_learning(
        &conn,
        "Has files",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    set_applies_to_files(&conn, id, Some("[\"src/**/*.rs\"]"));
    // task_types and errors are NULL, but field_filter restricts to files only

    let result = curate_enrich(
        &conn,
        EnrichParams {
            dry_run: false,
            batch_size: 20,
            field_filter: Some(EnrichFieldFilter::AppliesToFiles),
        },
    )
    .expect("curate_enrich with field filter");

    assert_eq!(
        result.total_candidates, 0,
        "--field=applies_to_files must find 0 candidates when all learnings have files set"
    );
    assert_eq!(result.field_filter.as_deref(), Some("applies_to_files"));
}

#[test]
fn test_format_enrich_text_dry_run_no_proposals() {
    // AC: dry-run output with no proposals says "no enrichment candidates found"
    let result = EnrichResult {
        dry_run: true,
        field_filter: None,
        total_candidates: 0,
        batches_processed: 0,
        learnings_enriched: 0,
        llm_errors: 0,
        proposals: vec![],
    };
    let text = format_enrich_text(&result);

    assert!(
        text.contains("Dry run"),
        "dry-run text must say 'Dry run': {text}"
    );
    assert!(
        text.contains("no enrichment candidates found"),
        "empty dry-run must say 'no enrichment candidates found': {text}"
    );
}

#[test]
fn test_format_enrich_text_dry_run_with_proposals() {
    // AC: dry-run output lists per-proposal detail (ID, title, proposed metadata).
    let result = EnrichResult {
        dry_run: true,
        field_filter: None,
        total_candidates: 2,
        batches_processed: 1,
        learnings_enriched: 0,
        llm_errors: 0,
        proposals: vec![
            make_enrich_proposal(10, "Test learning A"),
            make_enrich_proposal(20, "Test learning B"),
        ],
    };
    let text = format_enrich_text(&result);

    assert!(
        text.contains("Dry run"),
        "dry-run text must contain 'Dry run': {text}"
    );
    assert!(
        text.contains("no changes made"),
        "dry-run text must say 'no changes made': {text}"
    );
    assert!(
        text.contains("2"),
        "dry-run text must include proposal count: {text}"
    );
    assert!(
        text.contains("Test learning A"),
        "dry-run text must list first proposal title: {text}"
    );
    assert!(
        text.contains("Test learning B"),
        "dry-run text must list second proposal title: {text}"
    );
    // Should include proposed_files since it's non-empty
    assert!(
        text.contains("files"),
        "dry-run text must list proposed files when present: {text}"
    );
    assert!(
        text.contains("task_types"),
        "dry-run text must list proposed task_types when present: {text}"
    );
}

#[test]
fn test_format_enrich_text_actual_no_candidates() {
    // AC: non-dry-run with total_candidates=0 says "No enrichment candidates found."
    let result = EnrichResult {
        dry_run: false,
        field_filter: None,
        total_candidates: 0,
        batches_processed: 0,
        learnings_enriched: 0,
        llm_errors: 0,
        proposals: vec![],
    };
    let text = format_enrich_text(&result);

    assert_eq!(
        text, "No enrichment candidates found.",
        "must return exact string for 0 candidates"
    );
}

#[test]
fn test_format_enrich_text_actual_with_results() {
    // AC: non-dry-run output summarizes enriched count and batch count.
    let result = EnrichResult {
        dry_run: false,
        field_filter: None,
        total_candidates: 5,
        batches_processed: 2,
        learnings_enriched: 4,
        llm_errors: 0,
        proposals: vec![],
    };
    let text = format_enrich_text(&result);

    assert!(
        text.contains("Enriched"),
        "actual output must say 'Enriched': {text}"
    );
    assert!(text.contains("4"), "must include enriched count: {text}");
    assert!(text.contains("2"), "must include batch count: {text}");
    assert!(
        !text.contains("error"),
        "must not mention errors when llm_errors=0: {text}"
    );
}

#[test]
fn test_format_enrich_text_actual_with_llm_errors() {
    // AC: non-dry-run output includes LLM error count when errors occurred.
    let result = EnrichResult {
        dry_run: false,
        field_filter: None,
        total_candidates: 10,
        batches_processed: 2,
        learnings_enriched: 3,
        llm_errors: 1,
        proposals: vec![],
    };
    let text = format_enrich_text(&result);

    assert!(text.contains("1"), "must include LLM error count: {text}");
    assert!(
        text.to_lowercase().contains("error"),
        "must mention errors when llm_errors > 0: {text}"
    );
}

// LLM-dependent tests: marked #[ignore] until a mock or integration harness is available.
// These define expected behavior but require spawn_claude to be injectable or stubbed.

#[ignore = "requires LLM stub — curate_enrich calls spawn_claude directly"]
#[test]
fn test_enrich_single_batch_when_candidates_lt_batch_size() {
    // AC: candidates < batch_size processes in a single batch (batches_processed=1).
    let (_dir, conn) = setup_db();
    let _ = insert_learning(
        &conn,
        "Needs enrich",
        Confidence::High,
        LearningOutcome::Pattern,
    );

    let result = curate_enrich(
        &conn,
        EnrichParams {
            batch_size: 20,
            dry_run: true,
            field_filter: None,
        },
    )
    .expect("curate_enrich single batch");

    // Single candidate < batch_size=20 → exactly 1 batch attempted
    assert_eq!(result.total_candidates, 1);
    assert_eq!(result.batches_processed, 1);
}

#[ignore = "requires LLM stub — curate_enrich calls spawn_claude directly"]
#[test]
fn test_enrich_multiple_batches_when_candidates_gt_batch_size() {
    // AC: 3 candidates with batch_size=1 → 3 batches processed.
    let (_dir, conn) = setup_db();
    for i in 0..3 {
        insert_learning(
            &conn,
            &format!("Learning {i}"),
            Confidence::High,
            LearningOutcome::Pattern,
        );
    }

    let result = curate_enrich(
        &conn,
        EnrichParams {
            batch_size: 1,
            dry_run: true,
            field_filter: None,
        },
    )
    .expect("curate_enrich multiple batches");

    assert_eq!(result.total_candidates, 3);
    // With LLM stub returning valid JSON per batch, all 3 batches processed
    assert_eq!(result.batches_processed, 3);
}

#[ignore = "requires LLM stub — curate_enrich calls spawn_claude directly"]
#[test]
fn test_enrich_dry_run_makes_no_db_changes() {
    // AC: dry_run=true generates proposals but leaves the database unchanged.
    let (_dir, conn) = setup_db();
    let id = insert_learning(
        &conn,
        "Needs enrich",
        Confidence::High,
        LearningOutcome::Pattern,
    );

    let result = curate_enrich(
        &conn,
        EnrichParams {
            batch_size: 20,
            dry_run: true,
            field_filter: None,
        },
    )
    .expect("curate_enrich dry_run");

    assert!(result.dry_run);
    assert_eq!(result.learnings_enriched, 0, "dry_run must not enrich");

    // Verify DB unchanged: applies_to_files must still be NULL
    let files: Option<String> = conn
        .query_row(
            "SELECT applies_to_files FROM learnings WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .expect("query applies_to_files");
    assert!(
        files.is_none(),
        "applies_to_files must remain NULL after dry_run"
    );
}

#[ignore = "requires LLM stub — curate_enrich calls spawn_claude directly"]
#[test]
fn test_enrich_llm_error_for_one_batch_other_batches_succeed() {
    // AC: when one batch fails (LLM error), other batches still succeed (partial success).
    // Invariant: llm_errors incremented, batches_processed < total_batches.
    //
    // To test this deterministically, spawn_claude would need to fail for one specific batch.
    // With a stub that alternates success/failure, we'd verify:
    // - result.llm_errors == 1
    // - result.batches_processed == total_batches - 1
    let (_dir, conn) = setup_db();
    for i in 0..3 {
        insert_learning(
            &conn,
            &format!("Learning {i}"),
            Confidence::High,
            LearningOutcome::Pattern,
        );
    }
    // (stub setup would go here)
    let _result = curate_enrich(
        &conn,
        EnrichParams {
            batch_size: 1,
            dry_run: false,
            field_filter: None,
        },
    );
    // assertions depend on stub behavior
}

#[ignore = "requires LLM stub — curate_enrich calls spawn_claude directly"]
#[test]
fn test_enrich_batch_failure_previous_batches_committed_and_durable() {
    // AC: if a later batch's apply step fails, earlier batch results are already committed.
    // Invariant: per-batch transactions mean partial progress is durable on failure.
    //
    // With a stub: first batch succeeds + commits, second batch fails.
    // After failure: verify first batch's changes are still in the DB.
    let (_dir, conn) = setup_db();
    for i in 0..2 {
        insert_learning(
            &conn,
            &format!("Learning {i}"),
            Confidence::High,
            LearningOutcome::Pattern,
        );
    }
    // (stub setup: first batch succeeds, second fails apply_proposals_in_transaction)
    // assertions depend on stub behavior
}

// ──────────────────────────────────────────────────────────────────────────────
// TEST-INIT-001: merge_cluster() — dedup cluster merge logic
//
// All tests are #[ignore] until FEAT-004 (merge_cluster implementation).
// ──────────────────────────────────────────────────────────────────────────────

/// Helper: insert a learning with full metadata and bandit stats for merge tests.
///
/// Returns the new learning ID.
fn insert_learning_full(
    conn: &Connection,
    title: &str,
    confidence: Confidence,
    applies_to_files: Option<Vec<&str>>,
    applies_to_task_types: Option<Vec<&str>>,
    applies_to_errors: Option<Vec<&str>>,
    tags: Option<Vec<&str>>,
    times_shown: i32,
    times_applied: i32,
) -> i64 {
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: title.to_string(),
        content: format!("Content for {title}"),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: applies_to_files.map(|v| v.into_iter().map(str::to_string).collect()),
        applies_to_task_types: applies_to_task_types
            .map(|v| v.into_iter().map(str::to_string).collect()),
        applies_to_errors: applies_to_errors.map(|v| v.into_iter().map(str::to_string).collect()),
        tags: tags.map(|v| v.into_iter().map(str::to_string).collect()),
        confidence,
    };
    let id = record_learning(conn, params)
        .expect("insert_learning_full")
        .learning_id;
    // record_learning always initialises bandit stats to 0 — patch them now
    conn.execute(
        "UPDATE learnings SET times_shown = ?1, times_applied = ?2 WHERE id = ?3",
        rusqlite::params![times_shown, times_applied, id],
    )
    .expect("set bandit stats");
    id
}

/// Reads a single column value from the learnings table for the given learning.
fn get_learning_col_str(conn: &Connection, id: i64, col: &str) -> Option<String> {
    let sql = format!("SELECT {col} FROM learnings WHERE id = ?1");
    conn.query_row(&sql, [id], |row| row.get::<_, Option<String>>(0))
        .expect("get_learning_col_str: learning must exist")
}

fn get_learning_col_i64(conn: &Connection, id: i64, col: &str) -> i64 {
    let sql = format!("SELECT {col} FROM learnings WHERE id = ?1");
    conn.query_row(&sql, [id], |row| row.get::<_, i64>(0))
        .expect("get_learning_col_i64: learning must exist")
}

fn get_learning_col_i32(conn: &Connection, id: i64, col: &str) -> i32 {
    let sql = format!("SELECT {col} FROM learnings WHERE id = ?1");
    conn.query_row(&sql, [id], |row| row.get::<_, i32>(0))
        .expect("get_learning_col_i32: learning must exist")
}

/// Reads the sorted tags list for a learning from the `learning_tags` table.
fn get_tags(conn: &Connection, id: i64) -> Vec<String> {
    use crate::learnings::get_learning_tags;
    let mut tags = get_learning_tags(conn, id).expect("get_tags");
    tags.sort();
    tags
}

/// Parses the JSON array column (applies_to_files etc.) into a sorted Vec.
fn parse_json_array_col(conn: &Connection, id: i64, col: &str) -> Vec<String> {
    match get_learning_col_str(conn, id, col) {
        Some(json) => {
            let mut v: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
            v.sort();
            v
        }
        None => vec![],
    }
}

// ── Core merge tests ──────────────────────────────────────────────────────────

#[test]
fn test_merge_two_learnings_creates_one_merged() {
    // Merging 2 source learnings must produce exactly 1 new learning whose
    // title and content come from the MergeClusterParams (not from the sources).
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(
        &conn,
        "Learning A",
        Confidence::Medium,
        None,
        None,
        None,
        None,
        0,
        0,
    );
    let id2 = insert_learning_full(
        &conn,
        "Learning B",
        Confidence::Medium,
        None,
        None,
        None,
        None,
        0,
        0,
    );

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "Merged Title".to_string(),
        merged_content: "Merged Content".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    // Verify the merged learning has the LLM-supplied title/content
    let title = get_learning_col_str(&conn, result.merged_learning_id, "title")
        .expect("merged title must exist");
    let content = get_learning_col_str(&conn, result.merged_learning_id, "content")
        .expect("merged content must exist");
    assert_eq!(title, "Merged Title");
    assert_eq!(content, "Merged Content");
    assert_eq!(result.retired_source_ids.len(), 2);
}

#[test]
fn test_merge_union_applies_to_files_no_duplicates() {
    // merged learning.applies_to_files = union(source1, source2) with no dupes
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(
        &conn,
        "A",
        Confidence::Medium,
        Some(vec!["src/lib.rs", "src/main.rs"]),
        None,
        None,
        None,
        0,
        0,
    );
    let id2 = insert_learning_full(
        &conn,
        "B",
        Confidence::Medium,
        Some(vec!["src/main.rs", "src/foo.rs"]), // "src/main.rs" is a duplicate
        None,
        None,
        None,
        0,
        0,
    );

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let files = parse_json_array_col(&conn, result.merged_learning_id, "applies_to_files");
    assert_eq!(
        files,
        vec!["src/foo.rs", "src/lib.rs", "src/main.rs"],
        "union of files, sorted, no duplicates"
    );
}

#[test]
fn test_merge_union_applies_to_task_types() {
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(
        &conn,
        "A",
        Confidence::Medium,
        None,
        Some(vec!["FEAT-", "FIX-"]),
        None,
        None,
        0,
        0,
    );
    let id2 = insert_learning_full(
        &conn,
        "B",
        Confidence::Medium,
        None,
        Some(vec!["FIX-", "TEST-"]), // "FIX-" is a duplicate
        None,
        None,
        0,
        0,
    );

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let task_types =
        parse_json_array_col(&conn, result.merged_learning_id, "applies_to_task_types");
    assert_eq!(task_types, vec!["FEAT-", "FIX-", "TEST-"]);
}

#[test]
fn test_merge_union_applies_to_errors() {
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(
        &conn,
        "A",
        Confidence::Medium,
        None,
        None,
        Some(vec!["E0001", "E0002"]),
        None,
        0,
        0,
    );
    let id2 = insert_learning_full(
        &conn,
        "B",
        Confidence::Medium,
        None,
        None,
        Some(vec!["E0002", "E0003"]), // "E0002" is a duplicate
        None,
        0,
        0,
    );

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let errors = parse_json_array_col(&conn, result.merged_learning_id, "applies_to_errors");
    assert_eq!(errors, vec!["E0001", "E0002", "E0003"]);
}

#[test]
fn test_merge_union_tags_no_duplicates() {
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(
        &conn,
        "A",
        Confidence::Medium,
        None,
        None,
        None,
        Some(vec!["alpha", "beta"]),
        0,
        0,
    );
    let id2 = insert_learning_full(
        &conn,
        "B",
        Confidence::Medium,
        None,
        None,
        None,
        Some(vec!["beta", "gamma"]), // "beta" is a duplicate
        0,
        0,
    );

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let tags = get_tags(&conn, result.merged_learning_id);
    assert_eq!(tags, vec!["alpha", "beta", "gamma"]);
}

// ── Bandit stat tests ─────────────────────────────────────────────────────────

#[test]
fn test_merge_times_shown_is_sum_of_sources() {
    // Known-bad discriminator: naive impl may forget to sum stats (leaves at 0).
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 5, 2);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 7, 3);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let times_shown = get_learning_col_i32(&conn, result.merged_learning_id, "times_shown");
    assert_eq!(
        times_shown, 12,
        "times_shown must be 5+7=12, not 0 (naive bug)"
    );
}

#[test]
fn test_merge_times_applied_is_sum_of_sources() {
    // Known-bad discriminator: naive impl may forget to sum stats.
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 5, 2);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 7, 3);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let times_applied = get_learning_col_i32(&conn, result.merged_learning_id, "times_applied");
    assert_eq!(
        times_applied, 5,
        "times_applied must be 2+3=5, not 0 (naive bug)"
    );
}

// ── Confidence tests ──────────────────────────────────────────────────────────

#[test]
fn test_merge_confidence_is_highest_from_sources() {
    // high > medium > low; merged learning takes the highest, not from LLM
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Low, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::High, None, None, None, None, 0, 0);
    let id3 = insert_learning_full(&conn, "C", Confidence::Medium, None, None, None, None, 0, 0);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2, id3],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let confidence = get_learning_col_str(&conn, result.merged_learning_id, "confidence")
        .expect("confidence must exist");
    assert_eq!(confidence, "high", "highest confidence from cluster wins");
}

#[test]
fn test_merge_confidence_not_from_llm_response() {
    // Confidence must be computed from source confidences, not accepted from
    // the LLM merged content.  We verify it equals "medium" (max of low+medium),
    // regardless of what the LLM-produced content might claim.
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Low, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 0, 0);

    // The merged_content deliberately embeds "confidence: high" to tempt a naive
    // implementation into parsing the LLM response for confidence.
    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "Merged insight. confidence: high".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let confidence = get_learning_col_str(&conn, result.merged_learning_id, "confidence")
        .expect("confidence must exist");
    assert_eq!(
        confidence, "medium",
        "confidence must come from sources (medium), not from LLM content"
    );
}

// ── Retirement / lifecycle tests ──────────────────────────────────────────────

#[test]
fn test_merge_sources_are_retired_after_merge() {
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 0, 0);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    merge_cluster(&conn, params).expect("merge_cluster");

    // Both sources must have retired_at set
    for id in [id1, id2] {
        let retired_at = get_learning_col_str(&conn, id, "retired_at");
        assert!(
            retired_at.is_some(),
            "source learning {id} must have retired_at set after merge"
        );
    }
}

#[test]
fn test_merge_merged_learning_is_active() {
    // The merged learning itself must NOT be retired (retired_at IS NULL)
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 0, 0);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let retired_at = get_learning_col_str(&conn, result.merged_learning_id, "retired_at");
    assert!(
        retired_at.is_none(),
        "merged learning must have retired_at = NULL (is active)"
    );
}

#[test]
fn test_merge_window_stats_reset_to_zero() {
    // window_shown and window_applied on the merged learning must be 0
    // (per US-003: window stats are NOT carried from source learnings)
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(
        &conn,
        "A",
        Confidence::Medium,
        None,
        None,
        None,
        None,
        10,
        4,
    );
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 8, 3);
    // Manually set window_shown / window_applied on sources to non-zero values
    conn.execute(
        "UPDATE learnings SET window_shown = 6, window_applied = 2 WHERE id = ?1",
        [id1],
    )
    .expect("set window stats on source 1");
    conn.execute(
        "UPDATE learnings SET window_shown = 4, window_applied = 1 WHERE id = ?1",
        [id2],
    )
    .expect("set window stats on source 2");

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let window_shown = get_learning_col_i32(&conn, result.merged_learning_id, "window_shown");
    let window_applied = get_learning_col_i32(&conn, result.merged_learning_id, "window_applied");
    assert_eq!(
        window_shown, 0,
        "window_shown must be 0 (reset, not summed)"
    );
    assert_eq!(
        window_applied, 0,
        "window_applied must be 0 (reset, not summed)"
    );
}

// ── Cross-cluster dedup test ───────────────────────────────────────────────────

#[test]
fn test_already_merged_learning_skipped_in_second_cluster() {
    // If learning A appears in cluster 1 (merged → M1) and cluster 2 also
    // lists A as a source, the second cluster should skip A (already retired)
    // and still create a merged learning from the remaining active sources.
    let (_dir, conn) = setup_db();

    let id_a = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 3, 1);
    let id_b = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 4, 2);
    let id_c = insert_learning_full(&conn, "C", Confidence::Medium, None, None, None, None, 5, 1);

    // Cluster 1: merge A + B → M1
    let params1 = MergeClusterParams {
        source_ids: vec![id_a, id_b],
        merged_title: "M1".to_string(),
        merged_content: "Content M1".to_string(),
    };
    let result1 = merge_cluster(&conn, params1).expect("cluster 1 merge");
    assert_eq!(result1.retired_source_ids.len(), 2);

    // Cluster 2: A (already retired) + C → M2; A must be skipped
    let params2 = MergeClusterParams {
        source_ids: vec![id_a, id_c],
        merged_title: "M2".to_string(),
        merged_content: "Content M2".to_string(),
    };
    let result2 = merge_cluster(&conn, params2).expect("cluster 2 merge");

    // A was already retired — it must appear in skipped, not retired
    assert!(
        result2.skipped_source_ids.contains(&id_a),
        "already-merged learning A must be skipped in second cluster"
    );
    assert!(
        !result2.retired_source_ids.contains(&id_a),
        "already-merged learning A must NOT appear in retired list"
    );
    // C must be retired
    assert!(
        result2.retired_source_ids.contains(&id_c),
        "active learning C must be retired in second cluster"
    );
    // M2 must be active
    let retired_at = get_learning_col_str(&conn, result2.merged_learning_id, "retired_at");
    assert!(
        retired_at.is_none(),
        "merged learning M2 must be active (retired_at IS NULL)"
    );
}

// ── TEST-001: Additional merge_cluster edge cases ─────────────────────────────

#[test]
fn test_merge_large_cluster_5_sources_sums_all_stats_and_unions_metadata() {
    // AC: merging 5 learnings — all stats summed, all metadata unioned
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(
        &conn,
        "A",
        Confidence::Low,
        Some(vec!["src/a.rs"]),
        Some(vec!["FEAT-"]),
        Some(vec!["E001"]),
        Some(vec!["tag-a"]),
        3,
        1,
    );
    let id2 = insert_learning_full(
        &conn,
        "B",
        Confidence::Medium,
        Some(vec!["src/b.rs"]),
        Some(vec!["FIX-"]),
        Some(vec!["E002"]),
        Some(vec!["tag-b"]),
        4,
        2,
    );
    let id3 = insert_learning_full(
        &conn,
        "C",
        Confidence::High,
        Some(vec!["src/c.rs"]),
        Some(vec!["TEST-"]),
        Some(vec!["E003"]),
        Some(vec!["tag-c"]),
        5,
        3,
    );
    let id4 = insert_learning_full(
        &conn,
        "D",
        Confidence::Low,
        Some(vec!["src/d.rs"]),
        Some(vec!["REFACTOR-"]),
        Some(vec!["E004"]),
        None,
        6,
        4,
    );
    let id5 = insert_learning_full(
        &conn,
        "E",
        Confidence::Medium,
        Some(vec!["src/e.rs"]),
        Some(vec!["FEAT-"]), // duplicate with id1
        Some(vec!["E005"]),
        Some(vec!["tag-a"]), // duplicate with id1
        7,
        5,
    );

    let params = MergeClusterParams {
        source_ids: vec![id1, id2, id3, id4, id5],
        merged_title: "Big Merge".to_string(),
        merged_content: "Merged from 5 sources".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster 5 sources");

    // All 5 sources retired
    assert_eq!(
        result.retired_source_ids.len(),
        5,
        "all 5 sources must be retired"
    );
    assert!(result.skipped_source_ids.is_empty(), "no skipped sources");

    // Stats summed: 3+4+5+6+7=25 shown, 1+2+3+4+5=15 applied
    let shown = get_learning_col_i32(&conn, result.merged_learning_id, "times_shown");
    let applied = get_learning_col_i32(&conn, result.merged_learning_id, "times_applied");
    assert_eq!(shown, 25, "times_shown must be sum of all 5 sources");
    assert_eq!(applied, 15, "times_applied must be sum of all 5 sources");

    // Metadata union: deduplicated
    let files = parse_json_array_col(&conn, result.merged_learning_id, "applies_to_files");
    assert_eq!(
        files,
        vec!["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs"]
    );

    let task_types =
        parse_json_array_col(&conn, result.merged_learning_id, "applies_to_task_types");
    assert_eq!(task_types, vec!["FEAT-", "FIX-", "REFACTOR-", "TEST-"]);

    let errors = parse_json_array_col(&conn, result.merged_learning_id, "applies_to_errors");
    assert_eq!(errors, vec!["E001", "E002", "E003", "E004", "E005"]);

    // Tags deduped: tag-a appears in id1 and id5 — must appear once
    let tags = get_tags(&conn, result.merged_learning_id);
    assert_eq!(tags, vec!["tag-a", "tag-b", "tag-c"]);

    // Highest confidence (High from id3)
    let confidence = get_learning_col_str(&conn, result.merged_learning_id, "confidence")
        .expect("confidence must exist");
    assert_eq!(confidence, "high");
}

#[test]
fn test_merge_all_null_metadata_produces_null_fields() {
    // AC: when ALL sources have NULL metadata, merged learning has NULL fields (not empty arrays)
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 2, 1);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 3, 2);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "All Null Merge".to_string(),
        merged_content: "No metadata".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster all-null");

    // All metadata fields must be NULL, not empty arrays
    let files = get_learning_col_str(&conn, result.merged_learning_id, "applies_to_files");
    let task_types =
        get_learning_col_str(&conn, result.merged_learning_id, "applies_to_task_types");
    let errors = get_learning_col_str(&conn, result.merged_learning_id, "applies_to_errors");

    assert!(
        files.is_none(),
        "applies_to_files must be NULL (not '[]') when all sources are NULL"
    );
    assert!(
        task_types.is_none(),
        "applies_to_task_types must be NULL (not '[]') when all sources are NULL"
    );
    assert!(
        errors.is_none(),
        "applies_to_errors must be NULL (not '[]') when all sources are NULL"
    );
}

#[test]
fn test_merge_confidence_medium_plus_high_gives_high() {
    // AC: confidence ordering edge case — medium + high = high
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::High, None, None, None, None, 0, 0);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let confidence = get_learning_col_str(&conn, result.merged_learning_id, "confidence")
        .expect("confidence must exist");
    assert_eq!(confidence, "high", "medium + high = high");
}

#[test]
fn test_merge_confidence_low_plus_low_gives_low() {
    // AC: confidence ordering edge case — low + low = low
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Low, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::Low, None, None, None, None, 0, 0);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let confidence = get_learning_col_str(&conn, result.merged_learning_id, "confidence")
        .expect("confidence must exist");
    assert_eq!(confidence, "low", "low + low = low");
}

#[test]
fn test_merge_outcome_is_pattern_not_computed_from_sources() {
    // AC: merged outcome is "pattern" (LLM-proposed default), not computed from source outcomes
    // Even if sources have failure/success/workaround outcomes, merge always uses pattern
    let (_dir, conn) = setup_db();

    // Insert with non-pattern outcomes via raw SQL after creation
    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 0, 0);
    conn.execute(
        "UPDATE learnings SET outcome = 'failure' WHERE id = ?1",
        [id1],
    )
    .expect("set outcome to failure");
    conn.execute(
        "UPDATE learnings SET outcome = 'success' WHERE id = ?1",
        [id2],
    )
    .expect("set outcome to success");

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let outcome = get_learning_col_str(&conn, result.merged_learning_id, "outcome")
        .expect("outcome must exist");
    assert_eq!(
        outcome, "pattern",
        "merged outcome must be 'pattern', not derived from source outcomes"
    );
}

#[test]
fn test_merge_root_cause_and_solution_not_propagated() {
    // AC: root_cause and solution from sources are NOT propagated to merged learning
    // The merged content captures this information instead.
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 0, 0);
    // Set root_cause and solution on source learnings
    conn.execute(
        "UPDATE learnings SET root_cause = 'Source root cause A', solution = 'Source solution A' WHERE id = ?1",
        [id1],
    )
    .expect("set root_cause/solution on id1");
    conn.execute(
        "UPDATE learnings SET root_cause = 'Source root cause B', solution = 'Source solution B' WHERE id = ?1",
        [id2],
    )
    .expect("set root_cause/solution on id2");

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "Merged content captures root cause and solution".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    let root_cause = get_learning_col_str(&conn, result.merged_learning_id, "root_cause");
    let solution = get_learning_col_str(&conn, result.merged_learning_id, "solution");
    assert!(
        root_cause.is_none(),
        "root_cause must be NULL on merged learning (not propagated from sources)"
    );
    assert!(
        solution.is_none(),
        "solution must be NULL on merged learning (not propagated from sources)"
    );
}

#[test]
fn test_merge_created_at_is_now_not_copied_from_sources() {
    // AC: merged learning has created_at set to now, not copied from oldest source
    let (_dir, conn) = setup_db();

    let id1 = insert_learning_full(&conn, "A", Confidence::Medium, None, None, None, None, 0, 0);
    let id2 = insert_learning_full(&conn, "B", Confidence::Medium, None, None, None, None, 0, 0);

    // Age both sources by 100 days
    set_age_days(&conn, id1, 100);
    set_age_days(&conn, id2, 100);

    let params = MergeClusterParams {
        source_ids: vec![id1, id2],
        merged_title: "M".to_string(),
        merged_content: "MC".to_string(),
    };
    let result = merge_cluster(&conn, params).expect("merge_cluster");

    // Merged learning's created_at must be recent (within the last minute), not 100 days ago.
    // We measure age using the same julianday approach as the retirement criteria.
    let age_days: f64 = conn
        .query_row(
            "SELECT julianday('now') - julianday(created_at) FROM learnings WHERE id = ?1",
            [result.merged_learning_id],
            |row| row.get(0),
        )
        .expect("query merged created_at age");

    assert!(
        age_days < 1.0,
        "merged learning created_at must be recent (< 1 day old), not copied from 100-day-old sources; age={age_days:.3}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// TEST-INIT-002: dedup LLM prompt building and response parsing
//
// All tests are #[ignore] until FEAT-004 (build_dedup_prompt / parse_dedup_response).
// ──────────────────────────────────────────────────────────────────────────────

/// Helper: build a minimal batch of DeduplicateLearningItem for prompt tests.
fn make_dedup_items(pairs: &[(i64, &str, &str)]) -> Vec<DeduplicateLearningItem> {
    pairs
        .iter()
        .map(|(id, title, content)| DeduplicateLearningItem {
            id: *id,
            title: title.to_string(),
            content: content.to_string(),
        })
        .collect()
}

#[test]

fn test_dedup_prompt_contains_uuid_boundary_delimiter() {
    // AC: prompt contains a random UUID boundary delimiter (injection protection)
    let items = make_dedup_items(&[(1, "Title A", "Content A"), (2, "Title B", "Content B")]);
    let prompt = build_dedup_prompt(&items, 0.85);

    // The delimiter must contain "===BOUNDARY_" followed by a UUID fragment
    assert!(
        prompt.contains("===BOUNDARY_"),
        "prompt must contain ===BOUNDARY_<uuid> delimiter for injection protection"
    );
    // The delimiter must appear at least twice (wrapping untrusted content)
    let count = prompt.matches("===BOUNDARY_").count();
    assert!(
        count >= 2,
        "delimiter must appear at least twice to wrap the untrusted content block; found {count}"
    );
}

#[test]

fn test_dedup_prompt_contains_untrusted_warning() {
    // AC: prompt contains UNTRUSTED warning for learning content
    let items = make_dedup_items(&[(1, "Title", "Content")]);
    let prompt = build_dedup_prompt(&items, 0.85);

    assert!(
        prompt.contains("UNTRUSTED"),
        "prompt must contain UNTRUSTED warning to guard against prompt injection"
    );
}

#[test]

fn test_dedup_prompt_includes_learning_ids_titles_content() {
    // AC: prompt includes all learning IDs, titles, and content
    let items = make_dedup_items(&[
        (42, "Caching pattern", "Use Redis for hot paths"),
        (
            99,
            "Database indexing",
            "Add composite indexes for slow queries",
        ),
    ]);
    let prompt = build_dedup_prompt(&items, 0.85);

    assert!(prompt.contains("42"), "prompt must include learning ID 42");
    assert!(
        prompt.contains("Caching pattern"),
        "prompt must include title for ID 42"
    );
    assert!(
        prompt.contains("Use Redis for hot paths"),
        "prompt must include content for ID 42"
    );
    assert!(prompt.contains("99"), "prompt must include learning ID 99");
    assert!(
        prompt.contains("Database indexing"),
        "prompt must include title for ID 99"
    );
    assert!(
        prompt.contains("Add composite indexes for slow queries"),
        "prompt must include content for ID 99"
    );
}

#[test]

fn test_dedup_prompt_includes_threshold_value() {
    // AC: prompt includes threshold value as guidance
    let items = make_dedup_items(&[(1, "Title", "Content")]);
    let prompt = build_dedup_prompt(&items, 0.85);

    assert!(
        prompt.contains("0.85"),
        "prompt must include the similarity threshold value 0.85 as guidance"
    );
}

#[test]

fn test_parse_dedup_response_valid_json() {
    // AC: valid JSON response parses to Vec<RawDedupCluster> correctly
    let response = r#"[{"source_ids": [1, 2, 3]}, {"source_ids": [4, 5]}]"#;
    let valid_ids = vec![1, 2, 3, 4, 5];

    let clusters = parse_dedup_response(response, &valid_ids).expect("parse should succeed");

    assert_eq!(clusters.len(), 2, "should parse 2 clusters");
    let c0_ids = clusters[0]
        .source_ids
        .as_ref()
        .expect("source_ids must be present");
    assert!(c0_ids.contains(&1) && c0_ids.contains(&2) && c0_ids.contains(&3));
    let c1_ids = clusters[1]
        .source_ids
        .as_ref()
        .expect("source_ids must be present");
    assert!(c1_ids.contains(&4) && c1_ids.contains(&5));
}

#[test]

fn test_parse_dedup_response_filters_nonexistent_ids() {
    // AC: response with non-existent learning IDs — invalid IDs filtered out, valid clusters preserved
    // Cluster 1: all IDs valid → kept
    // Cluster 2: contains ID 999 which is not in valid_ids → that cluster is filtered out
    let response = r#"[{"source_ids": [1, 2]}, {"source_ids": [3, 999]}]"#;
    let valid_ids = vec![1, 2, 3];

    let clusters = parse_dedup_response(response, &valid_ids).expect("parse should succeed");

    // Cluster with valid IDs [1,2] must be preserved
    let has_valid = clusters.iter().any(|c| {
        let ids = c.source_ids.as_ref().map(|v| v.as_slice()).unwrap_or(&[]);
        ids.contains(&1) && ids.contains(&2)
    });
    assert!(has_valid, "cluster with valid IDs [1, 2] must be preserved");

    // Cluster containing hallucinated ID 999 must be filtered out
    let has_invalid = clusters.iter().any(|c| {
        c.source_ids
            .as_ref()
            .map(|v| v.contains(&999))
            .unwrap_or(false)
    });
    assert!(
        !has_invalid,
        "cluster containing non-existent ID 999 must be filtered out"
    );
}

#[test]

fn test_parse_dedup_response_first_cluster_wins_on_duplicate_id() {
    // AC: response with same learning in multiple clusters — first cluster wins, later skipped
    // Learning ID 2 appears in both clusters; the first cluster should be kept, second skipped.
    let response = r#"[{"source_ids": [1, 2]}, {"source_ids": [2, 3]}]"#;
    let valid_ids = vec![1, 2, 3];

    let clusters = parse_dedup_response(response, &valid_ids).expect("parse should succeed");

    // First cluster [1,2] must be present
    let first_kept = clusters.iter().any(|c| {
        let ids = c.source_ids.as_ref().map(|v| v.as_slice()).unwrap_or(&[]);
        ids.contains(&1) && ids.contains(&2)
    });
    assert!(first_kept, "first cluster [1, 2] must be kept");

    // Second cluster [2,3] must NOT be present (ID 2 already claimed)
    let second_present = clusters.iter().any(|c| {
        c.source_ids
            .as_ref()
            .map(|v| v.contains(&3))
            .unwrap_or(false)
    });
    assert!(
        !second_present,
        "second cluster containing already-claimed ID 2 must be skipped"
    );
}

#[test]

fn test_parse_dedup_response_non_json_returns_empty() {
    // AC: non-JSON response returns empty clusters (best-effort, no crash)
    let response = "Sorry, I cannot help with that.";
    let valid_ids = vec![1, 2, 3];

    let clusters = parse_dedup_response(response, &valid_ids).expect("parse should not error");

    assert!(
        clusters.is_empty(),
        "non-JSON response must return empty clusters without crashing"
    );
}

#[test]

fn test_parse_dedup_response_markdown_wrapped_json() {
    // AC: markdown-wrapped JSON (```json ... ```) is extracted correctly
    let response = "```json\n[{\"source_ids\": [10, 20]}]\n```";
    let valid_ids = vec![10, 20];

    let clusters = parse_dedup_response(response, &valid_ids).expect("parse should succeed");

    assert_eq!(
        clusters.len(),
        1,
        "should extract 1 cluster from markdown-wrapped JSON"
    );
    let ids = clusters[0]
        .source_ids
        .as_ref()
        .expect("source_ids must be present");
    assert!(
        ids.contains(&10) && ids.contains(&20),
        "extracted cluster must contain IDs 10 and 20"
    );
}

#[test]

fn test_parse_dedup_response_empty_array() {
    // AC: empty array response returns 0 clusters
    let response = "[]";
    let valid_ids = vec![1, 2, 3];

    let clusters = parse_dedup_response(response, &valid_ids).expect("parse should succeed");

    assert_eq!(
        clusters.len(),
        0,
        "empty array response must return 0 clusters"
    );
}

#[test]

fn test_parse_dedup_response_single_id_cluster_rejected() {
    // Known-bad discriminator: cluster with only 1 ID is not a merge — must be rejected
    let response = r#"[{"source_ids": [42]}, {"source_ids": [1, 2]}]"#;
    let valid_ids = vec![1, 2, 42];

    let clusters = parse_dedup_response(response, &valid_ids).expect("parse should succeed");

    // Single-ID cluster must be filtered out
    let has_singleton = clusters
        .iter()
        .any(|c| c.source_ids.as_deref() == Some(&[42_i64][..]));
    assert!(
        !has_singleton,
        "cluster with only 1 ID must be rejected (requires at least 2 for a merge)"
    );

    // Valid 2-ID cluster must be kept
    let has_valid_pair = clusters.iter().any(|c| {
        let ids = c.source_ids.as_ref().map(|v| v.as_slice()).unwrap_or(&[]);
        ids.contains(&1) && ids.contains(&2)
    });
    assert!(has_valid_pair, "valid 2-ID cluster must be preserved");
}

// ──────────────────────────────────────────────────────────────────────────────
// TEST-INIT-003: Dedup orchestration — dry-run, re-run idempotency, short-circuit
//
// All tests are #[ignore] until FEAT-004 implements curate_dedup().
// ──────────────────────────────────────────────────────────────────────────────

use super::types::DedupResult;
use super::{curate_dedup, DedupParams};

/// Returns the number of active (non-retired) learnings in the DB.
fn count_active_learnings(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )
    .expect("count_active_learnings")
}

/// Returns the total number of learnings in the DB (active + retired).
fn count_all_learnings(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM learnings", [], |row| {
        row.get::<_, i64>(0)
    })
    .expect("count_all_learnings")
}

#[test]
fn test_dedup_zero_active_learnings_returns_empty_result() {
    // AC: 0 active learnings returns empty DedupResult immediately (no LLM invocation).
    // An empty DB means there is nothing to deduplicate; the function must short-circuit
    // without calling the LLM (no CLAUDE_BINARY required for this test).
    let (_dir, conn) = setup_db();

    let result = curate_dedup(&conn, DedupParams::default()).expect("curate_dedup empty db");

    assert_eq!(
        result.clusters_found, 0,
        "no clusters expected for empty db"
    );
    assert_eq!(
        result.learnings_merged, 0,
        "no merges expected for empty db"
    );
    assert_eq!(
        result.learnings_created, 0,
        "no creations expected for empty db"
    );
    assert_eq!(result.llm_errors, 0, "no LLM errors expected for empty db");
    assert!(result.clusters.is_empty(), "clusters vec must be empty");
}

#[test]
fn test_dedup_dry_run_makes_no_db_changes() {
    // AC: dry_run=true returns DedupResult with clusters but no DB changes.
    // Known-bad discriminator: learning count before and after must be equal.
    let (_dir, conn) = setup_db();

    // Insert two learnings that are semantically similar (for documentation only —
    // with a real LLM mock they would form a cluster).
    insert_learning(
        &conn,
        "Use cargo fmt before committing",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    insert_learning(
        &conn,
        "Run cargo fmt prior to commit",
        Confidence::High,
        LearningOutcome::Pattern,
    );

    let before_count = count_all_learnings(&conn);

    let result = curate_dedup(
        &conn,
        DedupParams {
            dry_run: true,
            ..DedupParams::default()
        },
    )
    .expect("curate_dedup dry_run=true");

    let after_count = count_all_learnings(&conn);

    // Dry-run must never write to the DB.
    assert_eq!(
        before_count, after_count,
        "dry_run=true must not change total learning count"
    );
    assert_eq!(
        result.learnings_merged, 0,
        "dry_run must not retire any learnings"
    );
    assert_eq!(
        result.learnings_created, 0,
        "dry_run must not create merged learnings"
    );
    assert!(result.dry_run, "result must reflect dry_run=true");

    // Verify no retired_at was set on existing learnings.
    let retired_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings WHERE retired_at IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("retired count query");
    assert_eq!(
        retired_count, 0,
        "dry_run=true must not set retired_at on any learning"
    );
}

#[test]
fn test_dedup_rerun_excludes_already_retired_learnings() {
    // AC: after merging cluster A, re-running excludes cluster A's source IDs
    // (they're retired) — only remaining active learnings are processed.
    //
    // Setup: 4 learnings. We manually retire learnings 1 and 2 (simulating a prior
    // merge). On re-run, curate_dedup should only see learnings 3 and 4.
    let (_dir, conn) = setup_db();

    let id1 = insert_learning(
        &conn,
        "Learning A1",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    let id2 = insert_learning(
        &conn,
        "Learning A2",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    let _id3 = insert_learning(
        &conn,
        "Learning B1",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    let _id4 = insert_learning(
        &conn,
        "Learning B2",
        Confidence::High,
        LearningOutcome::Pattern,
    );

    // Simulate prior merge: retire learnings 1 and 2.
    retire_learning(&conn, id1);
    retire_learning(&conn, id2);

    // Two active learnings remain.
    assert_eq!(
        count_active_learnings(&conn),
        2,
        "precondition: 2 active learnings"
    );

    // With dry_run=true, curate_dedup must only consider the 2 active learnings.
    let result = curate_dedup(
        &conn,
        DedupParams {
            dry_run: true,
            ..DedupParams::default()
        },
    )
    .expect("curate_dedup re-run");

    // The already-retired learnings must not appear in any cluster's source_ids.
    for cluster in &result.clusters {
        assert!(
            !cluster.source_ids.contains(&id1),
            "retired learning {} must not appear in dedup clusters",
            id1
        );
        assert!(
            !cluster.source_ids.contains(&id2),
            "retired learning {} must not appear in dedup clusters",
            id2
        );
    }
}

#[test]
#[ignore = "FEAT-004: curate_dedup not yet implemented — requires LLM stub for per-cluster failure"]
fn test_dedup_per_cluster_transaction_partial_failure() {
    // AC: per-cluster transaction — if cluster 2 merge fails, cluster 1 is already committed.
    //
    // This test defines expected behavior: each cluster merge is committed independently,
    // so a failure in cluster 2 does not roll back cluster 1.
    //
    // Requires LLM stub (CLAUDE_BINARY) returning 2 clusters and a mechanism to inject
    // a failure for the second cluster merge (e.g., a corrupted source ID).
    //
    // For now the test documents the invariant. Full coverage requires:
    //   1. An LLM stub returning two clusters
    //   2. One cluster containing a non-existent ID (forces merge failure)
    //   3. Verification that the first cluster's merged learning was committed
    let (_dir, conn) = setup_db();

    let id1 = insert_learning(
        &conn,
        "Cluster1 A",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    let id2 = insert_learning(
        &conn,
        "Cluster1 B",
        Confidence::High,
        LearningOutcome::Pattern,
    );
    // id3/id4 would belong to cluster 2 (one with a bad ID to force failure)
    let _id3 = insert_learning(
        &conn,
        "Cluster2 A",
        Confidence::High,
        LearningOutcome::Pattern,
    );

    // curate_dedup processes clusters individually; each uses its own transaction.
    // After a successful cluster-1 merge, cluster-1 source IDs are retired.
    let result = curate_dedup(&conn, DedupParams::default()).expect("curate_dedup partial");

    // If cluster 1 merged successfully, its sources are retired.
    // (This assertion only holds when the LLM stub returns [id1, id2] as cluster 1.)
    if result.learnings_created >= 1 {
        assert!(
            is_retired(&conn, id1),
            "cluster-1 source id1 must be retired after successful merge"
        );
        assert!(
            is_retired(&conn, id2),
            "cluster-1 source id2 must be retired after successful merge"
        );
    }
}
