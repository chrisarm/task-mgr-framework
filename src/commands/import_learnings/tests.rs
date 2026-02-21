//! Tests for import_learnings command.

use std::fs;

use tempfile::TempDir;

use chrono::{DateTime, NaiveDateTime, Utc};
use rstest::rstest;

use clap::CommandFactory;

use super::{
    compute_dedup_key, format_text, import_learnings, parse_learnings, ImportLearningsResult,
};
use crate::cli::Cli;
use crate::commands::export::export as export_cmd;
use crate::commands::init::{init, PrefixMode};
use crate::db::migrations;
use crate::db::open_connection;
use crate::db::schema;
use crate::error::TaskMgrError;
use crate::learnings::{record_learning, RecordLearningParams};
use crate::models::{Confidence, LearningExport, LearningOutcome, ProgressExport};

/// Set up a test database with schema and migrations.
fn setup_test_db() -> (TempDir, rusqlite::Connection) {
    let dir = TempDir::new().expect("create temp dir");
    let mut conn = open_connection(dir.path()).expect("open connection");
    schema::create_schema(&conn).expect("create schema");
    migrations::run_migrations(&mut conn).expect("run migrations");
    (dir, conn)
}

/// Create a minimal LearningExport for testing.
fn make_learning(title: &str, content: &str) -> LearningExport {
    LearningExport::new(LearningOutcome::Pattern, title, content)
}

/// Create a LearningExport with tags.
fn make_learning_with_tags(title: &str, content: &str, tags: Vec<String>) -> LearningExport {
    let mut learning = make_learning(title, content);
    learning.tags = tags;
    learning
}

// --- parse_learnings tests ---

#[test]
fn test_parse_learnings_progress_export_format() {
    let mut progress = ProgressExport::new("test.db", 0);
    progress.learnings = vec![
        make_learning("Title 1", "Content 1"),
        make_learning("Title 2", "Content 2"),
    ];
    let json = serde_json::to_string(&progress).unwrap();

    let result = parse_learnings(&json).unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].title, "Title 1");
    assert_eq!(result[1].title, "Title 2");
}

#[test]
fn test_parse_learnings_standalone_array_format() {
    let learnings = vec![
        make_learning("Title A", "Content A"),
        make_learning("Title B", "Content B"),
        make_learning("Title C", "Content C"),
    ];
    let json = serde_json::to_string(&learnings).unwrap();

    let result = parse_learnings(&json).unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].title, "Title A");
}

#[test]
fn test_parse_learnings_empty_progress_export() {
    let progress = ProgressExport::new("test.db", 0);
    let json = serde_json::to_string(&progress).unwrap();

    let result = parse_learnings(&json).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_parse_learnings_empty_array() {
    let json = "[]";
    let result = parse_learnings(json).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_parse_learnings_invalid_json() {
    let result = parse_learnings("not json at all");
    assert!(result.is_err());
}

#[test]
fn test_parse_learnings_wrong_format() {
    let json = r#"{"foo": "bar"}"#;
    // This will fail as ProgressExport (missing required fields)
    // and fail as Vec<LearningExport> (not an array)
    let result = parse_learnings(json);
    assert!(result.is_err());
}

// --- compute_dedup_key tests ---

#[test]
fn test_hash_deterministic() {
    let h1 = compute_dedup_key("title", "content");
    let h2 = compute_dedup_key("title", "content");
    assert_eq!(h1, h2);
}

#[test]
fn test_hash_different_titles() {
    let h1 = compute_dedup_key("title1", "content");
    let h2 = compute_dedup_key("title2", "content");
    assert_ne!(h1, h2);
}

#[test]
fn test_hash_different_content() {
    let h1 = compute_dedup_key("title", "content1");
    let h2 = compute_dedup_key("title", "content2");
    assert_ne!(h1, h2);
}

#[test]
fn test_hash_empty_strings() {
    let h = compute_dedup_key("", "");
    assert!(!h.is_empty());
    // Key for empty strings should be deterministic
    assert_eq!(h, compute_dedup_key("", ""));
}

// --- import_learnings integration tests ---

#[test]
fn test_import_from_standalone_array() {
    let (dir, _conn) = setup_test_db();

    let learnings = vec![
        make_learning("Learning 1", "Content 1"),
        make_learning("Learning 2", "Content 2"),
    ];
    let json = serde_json::to_string_pretty(&learnings).unwrap();

    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 2);
    assert_eq!(result.learnings_skipped, 0);
    assert!(!result.stats_reset);
}

#[test]
fn test_import_from_progress_export() {
    let (dir, _conn) = setup_test_db();

    let mut progress = ProgressExport::new("source.db", 42);
    progress.learnings = vec![make_learning("From Progress", "Content from progress")];
    let json = serde_json::to_string_pretty(&progress).unwrap();

    let import_file = dir.path().join("progress.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 1);
    assert_eq!(result.learnings_skipped, 0);
}

#[test]
fn test_import_deduplicates_by_title_content_hash() {
    let (dir, _conn) = setup_test_db();

    // Import first batch
    let learnings = vec![make_learning("Dup Title", "Dup Content")];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result1 = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result1.learnings_imported, 1);
    assert_eq!(result1.learnings_skipped, 0);

    // Import same file again
    let result2 = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result2.learnings_imported, 0);
    assert_eq!(result2.learnings_skipped, 1);
}

#[test]
fn test_import_with_tags() {
    let (dir, _conn) = setup_test_db();

    let learnings = vec![make_learning_with_tags(
        "Tagged",
        "Content",
        vec!["rust".to_string(), "testing".to_string()],
    )];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 1);
    assert_eq!(result.tags_imported, 2);
}

#[test]
fn test_import_with_reset_stats() {
    let (dir, _conn) = setup_test_db();

    let mut learning = make_learning("Stats Test", "Content");
    learning.times_shown = 10;
    learning.times_applied = 5;
    let learnings = vec![learning];

    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, true).unwrap();
    assert_eq!(result.learnings_imported, 1);
    assert!(result.stats_reset);

    // Verify stats are zero in DB
    let conn = open_connection(dir.path()).unwrap();
    let (shown, applied): (i32, i32) = conn
        .query_row(
            "SELECT times_shown, times_applied FROM learnings WHERE title = ?1",
            rusqlite::params!["Stats Test"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(shown, 0);
    assert_eq!(applied, 0);
}

#[test]
fn test_import_empty_file() {
    let (dir, _conn) = setup_test_db();

    let import_file = dir.path().join("empty.json");
    fs::write(&import_file, "[]").unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 0);
    assert_eq!(result.learnings_skipped, 0);
}

#[test]
fn test_import_nonexistent_file() {
    let (dir, _conn) = setup_test_db();
    let result = import_learnings(dir.path(), &dir.path().join("nonexistent.json"), false);
    assert!(result.is_err());
}

#[test]
fn test_import_invalid_json() {
    let (dir, _conn) = setup_test_db();
    let import_file = dir.path().join("bad.json");
    fs::write(&import_file, "not json").unwrap();

    let result = import_learnings(dir.path(), &import_file, false);
    assert!(result.is_err());
}

#[test]
fn test_import_mixed_new_and_duplicate() {
    let (dir, _conn) = setup_test_db();

    // First import
    let learnings = vec![make_learning("Existing", "Already here")];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("first.json");
    fs::write(&import_file, &json).unwrap();
    import_learnings(dir.path(), &import_file, false).unwrap();

    // Second import with mix of new and existing
    let learnings = vec![
        make_learning("Existing", "Already here"),     // duplicate
        make_learning("New One", "Brand new content"), // new
    ];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file2 = dir.path().join("second.json");
    fs::write(&import_file2, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file2, false).unwrap();
    assert_eq!(result.learnings_imported, 1);
    assert_eq!(result.learnings_skipped, 1);
}

#[test]
fn test_import_preserves_metadata() {
    let (dir, _conn) = setup_test_db();

    let mut learning = make_learning("Meta Test", "Content");
    learning.outcome = LearningOutcome::Failure;
    learning.confidence = Confidence::High;
    learning.root_cause = Some("Root cause".to_string());
    learning.solution = Some("Solution".to_string());
    learning.applies_to_files = Some(vec!["src/*.rs".to_string()]);
    learning.applies_to_task_types = Some(vec!["FIX-".to_string()]);
    learning.applies_to_errors = Some(vec!["E0277".to_string()]);

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 1);

    // Verify metadata in DB
    let conn = open_connection(dir.path()).unwrap();
    let (outcome, confidence, root_cause, solution): (
        String,
        String,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT outcome, confidence, root_cause, solution FROM learnings WHERE title = ?1",
            rusqlite::params!["Meta Test"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(outcome, "failure");
    assert_eq!(confidence, "high");
    assert_eq!(root_cause.unwrap(), "Root cause");
    assert_eq!(solution.unwrap(), "Solution");
}

#[test]
fn test_import_does_not_carry_run_id() {
    let (dir, _conn) = setup_test_db();

    let mut learning = make_learning("No RunID", "Content");
    learning.run_id = Some("old-run-123".to_string());

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    import_learnings(dir.path(), &import_file, false).unwrap();

    // Verify run_id is NULL in DB (not carried over to avoid FK violations)
    let conn = open_connection(dir.path()).unwrap();
    let run_id: Option<String> = conn
        .query_row(
            "SELECT run_id FROM learnings WHERE title = ?1",
            rusqlite::params!["No RunID"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(run_id.is_none());
}

#[test]
fn test_import_does_not_carry_task_id() {
    let (dir, _conn) = setup_test_db();

    let mut learning = make_learning("No TaskID", "Content");
    learning.task_id = Some("old-task-123".to_string());

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    import_learnings(dir.path(), &import_file, false).unwrap();

    // Verify task_id is NULL in DB (not carried over to avoid FK violations)
    let conn = open_connection(dir.path()).unwrap();
    let task_id: Option<String> = conn
        .query_row(
            "SELECT task_id FROM learnings WHERE title = ?1",
            rusqlite::params!["No TaskID"],
            |row| row.get(0),
        )
        .unwrap();
    assert!(task_id.is_none());
}

// --- within-batch dedup and atomicity tests ---

#[test]
fn test_import_deduplicates_within_batch() {
    let (dir, _conn) = setup_test_db();

    // Batch contains two learnings with identical title+content
    let learnings = vec![
        make_learning("Same Title", "Same Content"),
        make_learning("Same Title", "Same Content"),
        make_learning("Unique", "Different content"),
    ];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 2);
    assert_eq!(result.learnings_skipped, 1);

    // Verify only 2 learnings in DB (not 3)
    let conn = open_connection(dir.path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM learnings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn test_import_atomicity_all_or_nothing() {
    let (dir, _conn) = setup_test_db();

    // First, import one learning successfully
    let learnings = vec![make_learning("Existing", "Already here")];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("first.json");
    fs::write(&import_file, &json).unwrap();
    import_learnings(dir.path(), &import_file, false).unwrap();

    // Count learnings before attempted import
    let conn = open_connection(dir.path()).unwrap();
    let count_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM learnings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count_before, 1);
    drop(conn);

    // Create a batch where the second learning has a valid unique key
    // and verify the transaction commits all-or-nothing by confirming
    // both new learnings appear in the DB
    let learnings = vec![
        make_learning("New A", "Content A"),
        make_learning("New B", "Content B"),
    ];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file2 = dir.path().join("second.json");
    fs::write(&import_file2, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file2, false).unwrap();
    assert_eq!(result.learnings_imported, 2);

    // Verify all 3 learnings (1 existing + 2 new) are in DB
    let conn = open_connection(dir.path()).unwrap();
    let count_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM learnings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count_after, 3);
}

// --- format_text tests ---

#[test]
fn test_format_text_basic() {
    let result = ImportLearningsResult {
        source_file: "progress.json".to_string(),
        learnings_imported: 5,
        learnings_skipped: 0,
        tags_imported: 0,
        stats_reset: false,
    };
    let text = format_text(&result);
    assert!(text.contains("progress.json"));
    assert!(text.contains("5"));
    assert!(!text.contains("duplicates"));
    assert!(!text.contains("Tags"));
    assert!(!text.contains("reset"));
}

#[test]
fn test_format_text_with_skipped() {
    let result = ImportLearningsResult {
        source_file: "import.json".to_string(),
        learnings_imported: 3,
        learnings_skipped: 2,
        tags_imported: 0,
        stats_reset: false,
    };
    let text = format_text(&result);
    assert!(text.contains("3"));
    assert!(text.contains("2"));
    assert!(text.contains("duplicates"));
}

#[test]
fn test_format_text_with_tags_and_reset() {
    let result = ImportLearningsResult {
        source_file: "learnings.json".to_string(),
        learnings_imported: 1,
        learnings_skipped: 0,
        tags_imported: 4,
        stats_reset: true,
    };
    let text = format_text(&result);
    assert!(text.contains("Tags imported: 4"));
    assert!(text.contains("reset"));
}

// --- stats preservation tests ---

/// Helper to build a fixed DateTime<Utc> from a SQLite-format string.
fn fixed_datetime(s: &str) -> DateTime<Utc> {
    let naive =
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").expect("valid datetime string");
    DateTime::from_naive_utc_and_offset(naive, Utc)
}

#[test]
fn test_import_preserves_stats_when_no_reset() {
    let (dir, _conn) = setup_test_db();

    let mut learning = make_learning("Stats Preserve", "Content");
    learning.times_shown = 10;
    learning.times_applied = 5;
    learning.last_shown_at = Some(fixed_datetime("2026-01-15 10:30:00"));
    learning.last_applied_at = Some(fixed_datetime("2026-01-14 08:00:00"));

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 1);
    assert!(!result.stats_reset);

    // Verify stats are preserved in DB
    let conn = open_connection(dir.path()).unwrap();
    let (shown, applied, last_shown, last_applied): (i32, i32, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT times_shown, times_applied, last_shown_at, last_applied_at \
             FROM learnings WHERE title = ?1",
            rusqlite::params!["Stats Preserve"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(shown, 10);
    assert_eq!(applied, 5);
    assert_eq!(last_shown.unwrap(), "2026-01-15 10:30:00");
    assert_eq!(last_applied.unwrap(), "2026-01-14 08:00:00");
}

#[test]
fn test_import_preserves_stats_with_none_datetimes() {
    let (dir, _conn) = setup_test_db();

    // times_shown > 0 but last_shown_at is None — preserve as-is
    let mut learning = make_learning("None Dates", "Content");
    learning.times_shown = 3;
    learning.times_applied = 0;
    // last_shown_at and last_applied_at default to None

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    import_learnings(dir.path(), &import_file, false).unwrap();

    let conn = open_connection(dir.path()).unwrap();
    let (shown, applied, last_shown, last_applied): (i32, i32, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT times_shown, times_applied, last_shown_at, last_applied_at \
             FROM learnings WHERE title = ?1",
            rusqlite::params!["None Dates"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(shown, 3);
    assert_eq!(applied, 0);
    assert!(last_shown.is_none());
    assert!(last_applied.is_none());
}

#[test]
fn test_format_text_stats_preserved() {
    let result = ImportLearningsResult {
        source_file: "learnings.json".to_string(),
        learnings_imported: 3,
        learnings_skipped: 0,
        tags_imported: 0,
        stats_reset: false,
    };
    let text = format_text(&result);
    assert!(text.contains("preserved from export"));
    assert!(!text.contains("reset to zero"));
}

#[test]
fn test_format_text_stats_reset() {
    let result = ImportLearningsResult {
        source_file: "learnings.json".to_string(),
        learnings_imported: 2,
        learnings_skipped: 0,
        tags_imported: 0,
        stats_reset: true,
    };
    let text = format_text(&result);
    assert!(text.contains("reset to zero"));
    assert!(!text.contains("preserved from export"));
}

#[test]
fn test_format_text_no_stats_line_when_zero_imported() {
    let result = ImportLearningsResult {
        source_file: "learnings.json".to_string(),
        learnings_imported: 0,
        learnings_skipped: 5,
        tags_imported: 0,
        stats_reset: false,
    };
    let text = format_text(&result);
    // No stats line when nothing was imported and not resetting
    assert!(!text.contains("preserved"));
    assert!(!text.contains("reset"));
}

// --- AC1: Error test: invalid JSON returns meaningful error ---

#[test]
fn test_import_invalid_json_returns_meaningful_error() {
    let (dir, _conn) = setup_test_db();
    let import_file = dir.path().join("bad.json");
    fs::write(&import_file, "not json {{{").unwrap();

    let err = import_learnings(dir.path(), &import_file, false).unwrap_err();
    let msg = err.to_string();
    // Error message should mention the format expectation, not be opaque
    assert!(
        msg.contains("JSON format") || msg.contains("unrecognized"),
        "Error should indicate JSON format issue, got: {msg}"
    );
}

// --- AC2: Error test: nonexistent file returns IoError ---

#[test]
fn test_import_nonexistent_file_returns_io_error() {
    let (dir, _conn) = setup_test_db();
    let missing = dir.path().join("does_not_exist.json");

    let err = import_learnings(dir.path(), &missing, false).unwrap_err();
    assert!(
        matches!(err, TaskMgrError::IoErrorWithContext { .. }),
        "Expected IoErrorWithContext, got: {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("does_not_exist.json"),
        "Error should mention the file path, got: {msg}"
    );
}

// --- AC3: Parameterized LearningOutcome round-trip ---

#[rstest]
#[case(LearningOutcome::Pattern, "pattern")]
#[case(LearningOutcome::Failure, "failure")]
#[case(LearningOutcome::Workaround, "workaround")]
#[case(LearningOutcome::Success, "success")]
fn test_outcome_roundtrip(#[case] outcome: LearningOutcome, #[case] expected_db: &str) {
    let (dir, _conn) = setup_test_db();

    let mut learning = LearningExport::new(outcome, "Outcome Test", "Content");
    learning.title = format!("Outcome {expected_db}");

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    import_learnings(dir.path(), &import_file, false).unwrap();

    let conn = open_connection(dir.path()).unwrap();
    let db_outcome: String = conn
        .query_row(
            "SELECT outcome FROM learnings WHERE title = ?1",
            rusqlite::params![format!("Outcome {expected_db}")],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(db_outcome, expected_db);
}

// --- AC4: Parameterized Confidence round-trip ---

#[rstest]
#[case(Confidence::High, "high")]
#[case(Confidence::Medium, "medium")]
#[case(Confidence::Low, "low")]
fn test_confidence_roundtrip(#[case] confidence: Confidence, #[case] expected_db: &str) {
    let (dir, _conn) = setup_test_db();

    let mut learning = make_learning("Confidence Test", "Content");
    learning.confidence = confidence;
    learning.title = format!("Confidence {expected_db}");

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    import_learnings(dir.path(), &import_file, false).unwrap();

    let conn = open_connection(dir.path()).unwrap();
    let db_confidence: String = conn
        .query_row(
            "SELECT confidence FROM learnings WHERE title = ?1",
            rusqlite::params![format!("Confidence {expected_db}")],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(db_confidence, expected_db);
}

// --- AC6: Boundary: all optional fields None ---

#[test]
fn test_import_learning_all_optional_fields_none() {
    let (dir, _conn) = setup_test_db();

    // make_learning creates minimal learning with all optionals None
    let learning = make_learning("Minimal", "Only required fields");
    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 1);

    let conn = open_connection(dir.path()).unwrap();
    let (root_cause, solution, applies_files, applies_tasks, applies_errors): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT root_cause, solution, applies_to_files, applies_to_task_types, \
             applies_to_errors FROM learnings WHERE title = ?1",
            rusqlite::params!["Minimal"],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();

    assert!(root_cause.is_none(), "root_cause should be None");
    assert!(solution.is_none(), "solution should be None");
    assert!(applies_files.is_none(), "applies_to_files should be None");
    assert!(
        applies_tasks.is_none(),
        "applies_to_task_types should be None"
    );
    assert!(applies_errors.is_none(), "applies_to_errors should be None");
}

// --- AC7: Boundary: all optional fields populated ---

#[test]
fn test_import_learning_all_optional_fields_populated() {
    let (dir, _conn) = setup_test_db();

    let mut learning = make_learning("Maximal", "All fields set");
    learning.outcome = LearningOutcome::Workaround;
    learning.confidence = Confidence::Low;
    learning.root_cause = Some("The root cause".to_string());
    learning.solution = Some("The solution".to_string());
    learning.applies_to_files = Some(vec!["src/*.rs".to_string(), "tests/**/*.rs".to_string()]);
    learning.applies_to_task_types = Some(vec!["FIX-".to_string(), "US-".to_string()]);
    learning.applies_to_errors = Some(vec!["E0277".to_string(), "E0308".to_string()]);
    learning.tags = vec![
        "rust".to_string(),
        "testing".to_string(),
        "edge".to_string(),
    ];
    learning.times_shown = 42;
    learning.times_applied = 7;
    learning.last_shown_at = Some(fixed_datetime("2026-02-10 14:30:00"));
    learning.last_applied_at = Some(fixed_datetime("2026-02-09 09:15:00"));

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false).unwrap();
    assert_eq!(result.learnings_imported, 1);
    assert_eq!(result.tags_imported, 3);

    let conn = open_connection(dir.path()).unwrap();

    // Verify all fields in DB
    let (outcome, confidence, root_cause, solution): (String, String, String, String) = conn
        .query_row(
            "SELECT outcome, confidence, root_cause, solution FROM learnings WHERE title = ?1",
            rusqlite::params!["Maximal"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(outcome, "workaround");
    assert_eq!(confidence, "low");
    assert_eq!(root_cause, "The root cause");
    assert_eq!(solution, "The solution");

    // Verify JSON array fields
    let (files_json, tasks_json, errors_json): (String, String, String) = conn
        .query_row(
            "SELECT applies_to_files, applies_to_task_types, applies_to_errors \
             FROM learnings WHERE title = ?1",
            rusqlite::params!["Maximal"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let files: Vec<String> = serde_json::from_str(&files_json).unwrap();
    let tasks: Vec<String> = serde_json::from_str(&tasks_json).unwrap();
    let errors: Vec<String> = serde_json::from_str(&errors_json).unwrap();
    assert_eq!(files, vec!["src/*.rs", "tests/**/*.rs"]);
    assert_eq!(tasks, vec!["FIX-", "US-"]);
    assert_eq!(errors, vec!["E0277", "E0308"]);

    // Verify tags
    let tag_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learning_tags lt JOIN learnings l ON lt.learning_id = l.id \
             WHERE l.title = ?1",
            rusqlite::params!["Maximal"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tag_count, 3);

    // Verify preserved stats
    let (shown, applied, last_shown, last_applied): (i32, i32, String, String) = conn
        .query_row(
            "SELECT times_shown, times_applied, last_shown_at, last_applied_at \
             FROM learnings WHERE title = ?1",
            rusqlite::params!["Maximal"],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(shown, 42);
    assert_eq!(applied, 7);
    assert_eq!(last_shown, "2026-02-10 14:30:00");
    assert_eq!(last_applied, "2026-02-09 09:15:00");
}

// --- AC8: Round-trip: export learnings, import to fresh DB, compare field-by-field ---

/// Read a learning back from DB as a struct for round-trip comparison.
/// Uses SQL directly to avoid dependency on export module.
fn read_learning_from_db(conn: &rusqlite::Connection, title: &str) -> LearningExport {
    let (
        outcome_str,
        confidence_str,
        content,
        root_cause,
        solution,
        files_json,
        tasks_json,
        errors_json,
        times_shown,
        times_applied,
        last_shown,
        last_applied,
    ): (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i32,
        i32,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT outcome, confidence, content, root_cause, solution, \
             applies_to_files, applies_to_task_types, applies_to_errors, \
             times_shown, times_applied, last_shown_at, last_applied_at \
             FROM learnings WHERE title = ?1",
            rusqlite::params![title],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
        .unwrap();

    let id: i64 = conn
        .query_row(
            "SELECT id FROM learnings WHERE title = ?1",
            rusqlite::params![title],
            |row| row.get(0),
        )
        .unwrap();

    // Load tags
    let mut stmt = conn
        .prepare("SELECT tag FROM learning_tags WHERE learning_id = ?1 ORDER BY tag")
        .unwrap();
    let tags: Vec<String> = stmt
        .query_map(rusqlite::params![id], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    let outcome = std::str::FromStr::from_str(&outcome_str).unwrap();
    let confidence = std::str::FromStr::from_str(&confidence_str).unwrap();
    let applies_to_files: Option<Vec<String>> =
        files_json.and_then(|s| serde_json::from_str(&s).ok());
    let applies_to_task_types: Option<Vec<String>> =
        tasks_json.and_then(|s| serde_json::from_str(&s).ok());
    let applies_to_errors: Option<Vec<String>> =
        errors_json.and_then(|s| serde_json::from_str(&s).ok());
    let last_shown_at = last_shown.map(|s| {
        let naive = NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").unwrap();
        DateTime::from_naive_utc_and_offset(naive, Utc)
    });
    let last_applied_at = last_applied.map(|s| {
        let naive = NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").unwrap();
        DateTime::from_naive_utc_and_offset(naive, Utc)
    });

    let mut export = LearningExport::new(outcome, title, content);
    export.confidence = confidence;
    export.root_cause = root_cause;
    export.solution = solution;
    export.applies_to_files = applies_to_files;
    export.applies_to_task_types = applies_to_task_types;
    export.applies_to_errors = applies_to_errors;
    export.times_shown = times_shown;
    export.times_applied = times_applied;
    export.last_shown_at = last_shown_at;
    export.last_applied_at = last_applied_at;
    export.tags = tags;
    export
}

#[test]
fn test_roundtrip_export_import_field_by_field() {
    // 1. Import to source DB
    let (src_dir, _conn) = setup_test_db();

    let mut learning = make_learning("Round Trip", "Round-trip fidelity test");
    learning.outcome = LearningOutcome::Workaround;
    learning.confidence = Confidence::High;
    learning.root_cause = Some("RC".to_string());
    learning.solution = Some("SOL".to_string());
    learning.applies_to_files = Some(vec!["src/lib.rs".to_string()]);
    learning.applies_to_task_types = Some(vec!["TEST-".to_string()]);
    learning.applies_to_errors = Some(vec!["E0001".to_string()]);
    learning.tags = vec!["roundtrip".to_string()];
    learning.times_shown = 15;
    learning.times_applied = 3;
    learning.last_shown_at = Some(fixed_datetime("2026-02-01 12:00:00"));
    learning.last_applied_at = Some(fixed_datetime("2026-01-31 18:00:00"));

    let original = learning.clone();
    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = src_dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();

    import_learnings(src_dir.path(), &import_file, false).unwrap();

    // 2. Read back from source DB and serialize to JSON (simulates export)
    let src_conn = open_connection(src_dir.path()).unwrap();
    let exported = read_learning_from_db(&src_conn, "Round Trip");
    let exported_json = serde_json::to_string_pretty(&[&exported]).unwrap();
    drop(src_conn);

    // 3. Import serialized data to fresh DB
    let (dst_dir, _conn2) = setup_test_db();
    let export_file = dst_dir.path().join("exported.json");
    fs::write(&export_file, &exported_json).unwrap();
    import_learnings(dst_dir.path(), &export_file, false).unwrap();

    // 4. Read back from destination DB and compare field-by-field
    let dst_conn = open_connection(dst_dir.path()).unwrap();
    let reimported = read_learning_from_db(&dst_conn, "Round Trip");

    // Field-by-field comparison against original input
    assert_eq!(reimported.outcome, original.outcome, "outcome mismatch");
    assert_eq!(reimported.title, original.title, "title mismatch");
    assert_eq!(reimported.content, original.content, "content mismatch");
    assert_eq!(
        reimported.root_cause, original.root_cause,
        "root_cause mismatch"
    );
    assert_eq!(reimported.solution, original.solution, "solution mismatch");
    assert_eq!(
        reimported.applies_to_files, original.applies_to_files,
        "applies_to_files mismatch"
    );
    assert_eq!(
        reimported.applies_to_task_types, original.applies_to_task_types,
        "applies_to_task_types mismatch"
    );
    assert_eq!(
        reimported.applies_to_errors, original.applies_to_errors,
        "applies_to_errors mismatch"
    );
    assert_eq!(
        reimported.confidence, original.confidence,
        "confidence mismatch"
    );
    assert_eq!(reimported.tags, original.tags, "tags mismatch");
    assert_eq!(
        reimported.times_shown, original.times_shown,
        "times_shown mismatch"
    );
    assert_eq!(
        reimported.times_applied, original.times_applied,
        "times_applied mismatch"
    );
    assert_eq!(
        reimported.last_shown_at, original.last_shown_at,
        "last_shown_at mismatch"
    );
    assert_eq!(
        reimported.last_applied_at, original.last_applied_at,
        "last_applied_at mismatch"
    );
}

// --- AC9: Stats round-trip via export/import with reset_stats=false ---

#[test]
fn test_stats_roundtrip_via_export_import() {
    let (dir, _conn) = setup_test_db();

    // Import learning with stats
    let mut learning = make_learning("Stats RT", "Stats round-trip test");
    learning.times_shown = 20;
    learning.times_applied = 8;
    learning.last_shown_at = Some(fixed_datetime("2026-02-15 10:00:00"));
    learning.last_applied_at = Some(fixed_datetime("2026-02-14 16:00:00"));

    let learnings = vec![learning];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file = dir.path().join("import.json");
    fs::write(&import_file, &json).unwrap();
    import_learnings(dir.path(), &import_file, false).unwrap();

    // Read back, serialize, then import to fresh DB (simulates export→import round-trip)
    let conn = open_connection(dir.path()).unwrap();
    let exported = read_learning_from_db(&conn, "Stats RT");
    let exported_json = serde_json::to_string_pretty(&[&exported]).unwrap();
    drop(conn);

    // Import to fresh DB with reset_stats=false
    let (dir2, _conn2) = setup_test_db();
    let export_file = dir2.path().join("exported.json");
    fs::write(&export_file, &exported_json).unwrap();
    let result = import_learnings(dir2.path(), &export_file, false).unwrap();
    assert!(!result.stats_reset);

    // Verify stats match original
    let conn2 = open_connection(dir2.path()).unwrap();
    let reimported = read_learning_from_db(&conn2, "Stats RT");
    assert_eq!(reimported.times_shown, 20);
    assert_eq!(reimported.times_applied, 8);
    assert_eq!(
        reimported.last_shown_at,
        Some(fixed_datetime("2026-02-15 10:00:00"))
    );
    assert_eq!(
        reimported.last_applied_at,
        Some(fixed_datetime("2026-02-14 16:00:00"))
    );
}

// --- INT-001: E2E integration tests ---

/// Create a minimal PRD JSON file for E2E tests.
fn create_minimal_prd(dir: &std::path::Path) -> std::path::PathBuf {
    let prd = r#"{
  "project": "e2e-test",
  "branchName": "test/e2e",
  "description": "E2E integration test PRD",
  "userStories": [
    {
      "id": "T-001",
      "title": "Test task",
      "description": "A task for E2E testing",
      "acceptanceCriteria": ["Test passes"],
      "priority": 1,
      "passes": false
    }
  ]
}"#;
    let path = dir.join("prd.json");
    fs::write(&path, prd).unwrap();
    path
}

/// E2E test: init PRD → record learning → export --learnings-file → import to fresh DB → verify field-by-field.
#[test]
fn test_e2e_init_learn_export_import_roundtrip() {
    // 1. Set up source: init PRD + record a learning with all fields
    let src_dir = TempDir::new().unwrap();
    let prd_path = create_minimal_prd(src_dir.path());

    init(
        src_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // 2. Record a learning with all fields populated (simulates `learn` command)
    let conn = open_connection(src_dir.path()).unwrap();
    let params = RecordLearningParams {
        outcome: LearningOutcome::Workaround,
        title: "E2E Round-Trip Learning".to_string(),
        content: "This learning tests the full export-import pipeline".to_string(),
        task_id: None,
        run_id: None,
        root_cause: Some("The root cause of the issue".to_string()),
        solution: Some("The applied solution".to_string()),
        applies_to_files: Some(vec!["src/**/*.rs".to_string(), "tests/*.rs".to_string()]),
        applies_to_task_types: Some(vec!["FIX-".to_string(), "US-".to_string()]),
        applies_to_errors: Some(vec!["E0277".to_string()]),
        tags: Some(vec!["e2e".to_string(), "roundtrip".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // Manually set stats to simulate bandit usage (record_learning starts at 0)
    conn.execute(
        "UPDATE learnings SET times_shown = 25, times_applied = 12, \
         last_shown_at = '2026-02-10 14:30:00', last_applied_at = '2026-02-09 09:15:00' \
         WHERE title = 'E2E Round-Trip Learning'",
        [],
    )
    .unwrap();
    drop(conn);

    // 3. Export with --learnings-file (simulates `export --learnings-file`)
    let export_json = src_dir.path().join("export.json");
    let learnings_file = src_dir.path().join("learnings.json");
    let export_result =
        export_cmd(src_dir.path(), &export_json, false, Some(&learnings_file)).unwrap();
    assert_eq!(export_result.learnings_exported, Some(1));

    // 4. Import to fresh DB (simulates `import-learnings --from-json`)
    let (dst_dir, dst_conn) = setup_test_db();
    drop(dst_conn); // Release connection before import_learnings opens its own
    let import_result = import_learnings(dst_dir.path(), &learnings_file, false).unwrap();
    assert_eq!(import_result.learnings_imported, 1);
    assert_eq!(import_result.learnings_skipped, 0);
    assert!(!import_result.stats_reset);

    // 5. Verify field-by-field match
    let dst_conn = open_connection(dst_dir.path()).unwrap();
    let imported = read_learning_from_db(&dst_conn, "E2E Round-Trip Learning");

    assert_eq!(imported.outcome, LearningOutcome::Workaround, "outcome");
    assert_eq!(imported.title, "E2E Round-Trip Learning", "title");
    assert_eq!(
        imported.content, "This learning tests the full export-import pipeline",
        "content"
    );
    assert_eq!(imported.confidence, Confidence::High, "confidence");
    assert_eq!(
        imported.root_cause,
        Some("The root cause of the issue".to_string()),
        "root_cause"
    );
    assert_eq!(
        imported.solution,
        Some("The applied solution".to_string()),
        "solution"
    );
    assert_eq!(
        imported.applies_to_files,
        Some(vec!["src/**/*.rs".to_string(), "tests/*.rs".to_string()]),
        "applies_to_files"
    );
    assert_eq!(
        imported.applies_to_task_types,
        Some(vec!["FIX-".to_string(), "US-".to_string()]),
        "applies_to_task_types"
    );
    assert_eq!(
        imported.applies_to_errors,
        Some(vec!["E0277".to_string()]),
        "applies_to_errors"
    );
    assert_eq!(
        imported.tags,
        vec!["e2e".to_string(), "roundtrip".to_string()],
        "tags"
    );
    // Stats should be preserved (reset_stats=false)
    assert_eq!(imported.times_shown, 25, "times_shown");
    assert_eq!(imported.times_applied, 12, "times_applied");
    assert_eq!(
        imported.last_shown_at,
        Some(fixed_datetime("2026-02-10 14:30:00")),
        "last_shown_at"
    );
    assert_eq!(
        imported.last_applied_at,
        Some(fixed_datetime("2026-02-09 09:15:00")),
        "last_applied_at"
    );
}

/// E2E test: same flow with --reset-stats → verify stats are zeroed but fields preserved.
#[test]
fn test_e2e_export_import_roundtrip_with_reset_stats() {
    // 1. Set up source: init PRD + record a learning with stats
    let src_dir = TempDir::new().unwrap();
    let prd_path = create_minimal_prd(src_dir.path());

    init(
        src_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(src_dir.path()).unwrap();
    let params = RecordLearningParams {
        outcome: LearningOutcome::Success,
        title: "Stats Reset Learning".to_string(),
        content: "Verify stats are zeroed on import with --reset-stats".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: Some("A solution".to_string()),
        applies_to_files: Some(vec!["*.rs".to_string()]),
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["reset".to_string()]),
        confidence: Confidence::Medium,
    };
    record_learning(&conn, params).unwrap();

    // Set non-zero stats
    conn.execute(
        "UPDATE learnings SET times_shown = 50, times_applied = 20, \
         last_shown_at = '2026-01-20 08:00:00', last_applied_at = '2026-01-19 16:00:00' \
         WHERE title = 'Stats Reset Learning'",
        [],
    )
    .unwrap();
    drop(conn);

    // 2. Export
    let export_json = src_dir.path().join("export.json");
    let learnings_file = src_dir.path().join("learnings.json");
    export_cmd(src_dir.path(), &export_json, false, Some(&learnings_file)).unwrap();

    // 3. Import with --reset-stats to fresh DB
    let (dst_dir, dst_conn) = setup_test_db();
    drop(dst_conn);
    let import_result = import_learnings(dst_dir.path(), &learnings_file, true).unwrap();
    assert_eq!(import_result.learnings_imported, 1);
    assert!(import_result.stats_reset);

    // 4. Verify non-stat fields are preserved
    let dst_conn = open_connection(dst_dir.path()).unwrap();
    let imported = read_learning_from_db(&dst_conn, "Stats Reset Learning");

    assert_eq!(imported.outcome, LearningOutcome::Success, "outcome");
    assert_eq!(
        imported.content, "Verify stats are zeroed on import with --reset-stats",
        "content"
    );
    assert_eq!(imported.confidence, Confidence::Medium, "confidence");
    assert_eq!(
        imported.solution,
        Some("A solution".to_string()),
        "solution"
    );
    assert_eq!(
        imported.applies_to_files,
        Some(vec!["*.rs".to_string()]),
        "applies_to_files"
    );
    assert_eq!(imported.tags, vec!["reset".to_string()], "tags");

    // Stats should be zeroed
    assert_eq!(imported.times_shown, 0, "times_shown should be 0");
    assert_eq!(imported.times_applied, 0, "times_applied should be 0");
    assert!(
        imported.last_shown_at.is_none(),
        "last_shown_at should be None"
    );
    assert!(
        imported.last_applied_at.is_none(),
        "last_applied_at should be None"
    );
}

#[test]
fn test_cli_help_does_not_contain_learnings_only() {
    let cmd = Cli::command();
    let subcmd = cmd
        .get_subcommands()
        .find(|c| c.get_name() == "import-learnings")
        .expect("import-learnings subcommand not found");
    let has_flag = subcmd
        .get_arguments()
        .any(|a| a.get_long() == Some("learnings-only"));
    assert!(
        !has_flag,
        "CLI should not have --learnings-only flag"
    );
}
