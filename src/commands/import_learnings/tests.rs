//! Tests for import_learnings command.

use std::fs;

use tempfile::TempDir;

use super::{
    compute_dedup_key, format_text, import_learnings, parse_learnings, ImportLearningsResult,
};
use crate::db::migrations;
use crate::db::open_connection;
use crate::db::schema;
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

    let result = import_learnings(dir.path(), &import_file, true, false).unwrap();
    assert_eq!(result.learnings_imported, 2);
    assert_eq!(result.learnings_skipped, 0);
    assert!(!result.stats_reset);
    assert!(result.learnings_only);
}

#[test]
fn test_import_from_progress_export() {
    let (dir, _conn) = setup_test_db();

    let mut progress = ProgressExport::new("source.db", 42);
    progress.learnings = vec![make_learning("From Progress", "Content from progress")];
    let json = serde_json::to_string_pretty(&progress).unwrap();

    let import_file = dir.path().join("progress.json");
    fs::write(&import_file, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file, false, false).unwrap();
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

    let result1 = import_learnings(dir.path(), &import_file, true, false).unwrap();
    assert_eq!(result1.learnings_imported, 1);
    assert_eq!(result1.learnings_skipped, 0);

    // Import same file again
    let result2 = import_learnings(dir.path(), &import_file, true, false).unwrap();
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

    let result = import_learnings(dir.path(), &import_file, true, false).unwrap();
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

    let result = import_learnings(dir.path(), &import_file, true, true).unwrap();
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

    let result = import_learnings(dir.path(), &import_file, true, false).unwrap();
    assert_eq!(result.learnings_imported, 0);
    assert_eq!(result.learnings_skipped, 0);
}

#[test]
fn test_import_nonexistent_file() {
    let (dir, _conn) = setup_test_db();
    let result = import_learnings(
        dir.path(),
        &dir.path().join("nonexistent.json"),
        true,
        false,
    );
    assert!(result.is_err());
}

#[test]
fn test_import_invalid_json() {
    let (dir, _conn) = setup_test_db();
    let import_file = dir.path().join("bad.json");
    fs::write(&import_file, "not json").unwrap();

    let result = import_learnings(dir.path(), &import_file, true, false);
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
    import_learnings(dir.path(), &import_file, true, false).unwrap();

    // Second import with mix of new and existing
    let learnings = vec![
        make_learning("Existing", "Already here"),     // duplicate
        make_learning("New One", "Brand new content"), // new
    ];
    let json = serde_json::to_string_pretty(&learnings).unwrap();
    let import_file2 = dir.path().join("second.json");
    fs::write(&import_file2, &json).unwrap();

    let result = import_learnings(dir.path(), &import_file2, true, false).unwrap();
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

    let result = import_learnings(dir.path(), &import_file, true, false).unwrap();
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

    import_learnings(dir.path(), &import_file, true, false).unwrap();

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

    import_learnings(dir.path(), &import_file, true, false).unwrap();

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

// --- format_text tests ---

#[test]
fn test_format_text_basic() {
    let result = ImportLearningsResult {
        source_file: "progress.json".to_string(),
        learnings_imported: 5,
        learnings_skipped: 0,
        tags_imported: 0,
        stats_reset: false,
        learnings_only: true,
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
        learnings_only: false,
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
        learnings_only: true,
    };
    let text = format_text(&result);
    assert!(text.contains("Tags imported: 4"));
    assert!(text.contains("reset"));
}
