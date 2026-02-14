//! Tests for the recall module.
//!
//! These tests verify the backward-compatible `recall_learnings()` function
//! which now delegates to the composite retrieval backend.

use rusqlite::Connection;
use tempfile::TempDir;

use super::{format_text, recall_learnings, RecallParams, RecallResult};
use crate::db::{create_schema, open_connection};
use crate::learnings::crud::{record_learning, RecordLearningParams};
use crate::models::{Confidence, Learning, LearningOutcome};

fn setup_db() -> (TempDir, Connection) {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    (temp_dir, conn)
}

fn setup_db_with_fts5() -> (TempDir, Connection) {
    use crate::db::migrations::run_migrations;

    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (temp_dir, conn)
}

fn create_test_learning(
    conn: &Connection,
    title: &str,
    content: &str,
    outcome: LearningOutcome,
) -> i64 {
    let params = RecordLearningParams {
        outcome,
        title: title.to_string(),
        content: content.to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    record_learning(conn, params).unwrap().learning_id
}

#[test]
fn test_recall_empty_database() {
    let (_temp_dir, conn) = setup_db();

    let params = RecallParams::default();
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 0);
    assert!(result.learnings.is_empty());
}

#[test]
fn test_recall_all_learnings() {
    let (_temp_dir, conn) = setup_db();

    create_test_learning(&conn, "Learning 1", "Content 1", LearningOutcome::Failure);
    create_test_learning(&conn, "Learning 2", "Content 2", LearningOutcome::Success);

    let params = RecallParams {
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 2);
}

#[test]
fn test_recall_with_text_query() {
    let (_temp_dir, conn) = setup_db();

    create_test_learning(
        &conn,
        "Database error",
        "SQLite crashed",
        LearningOutcome::Failure,
    );
    create_test_learning(
        &conn,
        "API success",
        "REST worked well",
        LearningOutcome::Success,
    );

    // Search for "database" — uses LIKE fallback since no FTS5
    let params = RecallParams {
        query: Some("database".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Database error");
}

#[test]
fn test_recall_with_outcome_filter() {
    let (_temp_dir, conn) = setup_db();

    create_test_learning(&conn, "Failure 1", "Content about failure", LearningOutcome::Failure);
    create_test_learning(&conn, "Success 1", "Content about success", LearningOutcome::Success);
    create_test_learning(&conn, "Failure 2", "Another failure story", LearningOutcome::Failure);

    let params = RecallParams {
        query: Some("Content".to_string()),
        outcome: Some(LearningOutcome::Failure),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert!(result
        .learnings
        .iter()
        .all(|l| l.outcome == LearningOutcome::Failure));
}

#[test]
fn test_recall_with_tags_filter() {
    let (_temp_dir, conn) = setup_db();

    // Create learning with tags
    let params1 = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Rust pattern".to_string(),
        content: "Use Result type for error handling".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["rust".to_string(), "patterns".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params1).unwrap();

    // Create learning without matching tags
    let params2 = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Python pattern".to_string(),
        content: "Use exceptions".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["python".to_string()]),
        confidence: Confidence::Medium,
    };
    record_learning(&conn, params2).unwrap();

    // Filter by rust tag + text query
    let params = RecallParams {
        query: Some("pattern".to_string()),
        tags: Some(vec!["rust".to_string()]),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Rust pattern");
}

#[test]
fn test_recall_with_limit() {
    let (_temp_dir, conn) = setup_db();

    for i in 1..=10 {
        create_test_learning(
            &conn,
            &format!("Learning {}", i),
            "Same content for searching",
            LearningOutcome::Pattern,
        );
    }

    let params = RecallParams {
        query: Some("content".to_string()),
        limit: 3,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert!(result.count <= 3);
}

#[test]
fn test_recall_updates_times_shown() {
    let (_temp_dir, conn) = setup_db();

    let learning_id = create_test_learning(&conn, "Test search target", "Searchable content", LearningOutcome::Pattern);

    // Verify initial times_shown is 0
    let initial: i32 = conn
        .query_row(
            "SELECT times_shown FROM learnings WHERE id = ?1",
            [learning_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(initial, 0);

    // Recall the learning
    let params = RecallParams {
        query: Some("search target".to_string()),
        limit: 10,
        ..Default::default()
    };
    recall_learnings(&conn, params).unwrap();

    // Verify times_shown was incremented
    let after: i32 = conn
        .query_row(
            "SELECT times_shown FROM learnings WHERE id = ?1",
            [learning_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, 1);
}

#[test]
fn test_recall_for_task_file_matching() {
    let (_temp_dir, conn) = setup_db();

    // Create a task with files
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('US-001', 'src/db/schema.rs')",
        [],
    )
    .unwrap();

    // Create a learning that matches
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "DB pattern".to_string(),
        content: "Use transactions".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/db/*.rs".to_string()]),
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // Create a learning that doesn't match
    let params2 = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "CLI pattern".to_string(),
        content: "Use clap".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/cli.rs".to_string()]),
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    record_learning(&conn, params2).unwrap();

    // Recall for task
    let recall_params = RecallParams {
        for_task: Some("US-001".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "DB pattern");
}

#[test]
fn test_recall_for_task_type_matching() {
    let (_temp_dir, conn) = setup_db();

    // Create a task
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();

    // Create a learning matching US- tasks
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "US pattern".to_string(),
        content: "For user stories".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: Some(vec!["US-".to_string()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // Create a learning matching FIX- tasks
    let params2 = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "FIX pattern".to_string(),
        content: "For bug fixes".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: Some(vec!["FIX-".to_string()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    record_learning(&conn, params2).unwrap();

    // Recall for US-001 task
    let recall_params = RecallParams {
        for_task: Some("US-001".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "US pattern");
}

#[test]
fn test_recall_for_nonexistent_task() {
    let (_temp_dir, conn) = setup_db();

    // Create a learning with applicability
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Test".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["*.rs".to_string()]),
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    record_learning(&conn, params).unwrap();

    // Recall for nonexistent task - should return empty
    let recall_params = RecallParams {
        for_task: Some("NONEXISTENT".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    // Should find no matches because task has no files to match against
    assert_eq!(result.count, 0);
}

#[test]
fn test_format_text_empty() {
    let result = RecallResult {
        learnings: vec![],
        count: 0,
        query: None,
        for_task: None,
        outcome_filter: None,
        tags_filter: None,
    };

    let text = format_text(&result);
    assert!(text.contains("No matching learnings found"));
}

#[test]
fn test_format_text_with_learnings() {
    let mut learning = Learning::new(
        LearningOutcome::Failure,
        "Test failure",
        "Detailed content here",
    );
    learning.id = Some(1);

    let result = RecallResult {
        learnings: vec![learning],
        count: 1,
        query: None,
        for_task: None,
        outcome_filter: None,
        tags_filter: None,
    };

    let text = format_text(&result);
    assert!(text.contains("Found 1 learning"));
    assert!(text.contains("Test failure"));
    assert!(text.contains("failure"));
}

// ========== FTS5 integration tests ==========

#[test]
fn test_fts5_basic_search() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    create_test_learning(
        &conn,
        "Database error handling",
        "SQLite crashed when inserting",
        LearningOutcome::Failure,
    );
    create_test_learning(
        &conn,
        "API success pattern",
        "REST worked well with JSON",
        LearningOutcome::Success,
    );

    let params = RecallParams {
        query: Some("database".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Database error handling");
}

#[test]
fn test_fts5_search_in_content() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    create_test_learning(
        &conn,
        "Error handling",
        "SQLite database crashed",
        LearningOutcome::Failure,
    );

    let params = RecallParams {
        query: Some("SQLite".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Error handling");
}

#[test]
fn test_fts5_no_match() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    create_test_learning(
        &conn,
        "Database error",
        "SQLite crashed",
        LearningOutcome::Failure,
    );

    let params = RecallParams {
        query: Some("nonexistent".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 0);
}

#[test]
fn test_fts5_fallback_to_like_without_migration() {
    let (_temp_dir, conn) = setup_db();

    create_test_learning(
        &conn,
        "Database error",
        "SQLite crashed",
        LearningOutcome::Failure,
    );

    let params = RecallParams {
        query: Some("database".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Database error");
}
