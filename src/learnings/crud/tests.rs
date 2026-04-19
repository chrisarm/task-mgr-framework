//! Tests for learnings CRUD operations.

#![cfg(test)]

use rusqlite::Connection;
use tempfile::TempDir;

use super::create::record_learning;
use super::delete::delete_learning;
use super::output::{format_delete_text, format_edit_text};
use super::read::{get_learning, get_learning_tags};
use super::types::{
    DeleteLearningResult, EditLearningParams, EditLearningResult, RecordLearningParams,
};
use super::update::edit_learning;
use crate::db::{create_schema, open_connection};
use crate::models::{Confidence, LearningOutcome};

fn setup_db() -> (TempDir, Connection) {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    (temp_dir, conn)
}

#[test]
fn test_record_learning_minimal() {
    let (_temp_dir, conn) = setup_db();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "Test failure".to_string(),
        content: "Something went wrong".to_string(),
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

    let result = record_learning(&conn, params).unwrap();

    assert!(result.learning_id > 0);
    assert_eq!(result.title, "Test failure");
    assert_eq!(result.outcome, LearningOutcome::Failure);
    assert_eq!(result.tags_added, 0);
}

#[test]
fn test_record_learning_with_all_fields() {
    let (_temp_dir, conn) = setup_db();

    // Create task and run for foreign key references
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Success,
        title: "Successful pattern".to_string(),
        content: "This worked well".to_string(),
        task_id: Some("US-001".to_string()),
        run_id: Some("run-001".to_string()),
        root_cause: Some("Root cause".to_string()),
        solution: Some("Applied fix".to_string()),
        applies_to_files: Some(vec!["src/*.rs".to_string()]),
        applies_to_task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
        applies_to_errors: Some(vec!["E0001".to_string()]),
        tags: Some(vec!["rust".to_string(), "database".to_string()]),
        confidence: Confidence::High,
    };

    let result = record_learning(&conn, params).unwrap();

    assert!(result.learning_id > 0);
    assert_eq!(result.title, "Successful pattern");
    assert_eq!(result.outcome, LearningOutcome::Success);
    assert_eq!(result.tags_added, 2);
}

#[test]
fn test_record_learning_with_tags() {
    let (_temp_dir, conn) = setup_db();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Tagged learning".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec![
            "rust".to_string(),
            "cli".to_string(),
            "error".to_string(),
        ]),
        confidence: Confidence::Medium,
    };

    let result = record_learning(&conn, params).unwrap();
    assert_eq!(result.tags_added, 3);

    // Verify tags are stored
    let tags = get_learning_tags(&conn, result.learning_id).unwrap();
    assert_eq!(tags, vec!["cli", "error", "rust"]); // sorted alphabetically
}

#[test]
fn test_record_learning_invalid_task_id() {
    let (_temp_dir, conn) = setup_db();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "Test".to_string(),
        content: "Content".to_string(),
        task_id: Some("NONEXISTENT".to_string()),
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };

    let result = record_learning(&conn, params);
    assert!(result.is_err(), "Should fail with invalid task_id");
}

#[test]
fn test_record_learning_invalid_run_id() {
    let (_temp_dir, conn) = setup_db();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "Test".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: Some("nonexistent-run".to_string()),
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };

    let result = record_learning(&conn, params);
    assert!(result.is_err(), "Should fail with invalid run_id");
}

#[test]
fn test_get_learning() {
    let (_temp_dir, conn) = setup_db();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Workaround,
        title: "Workaround title".to_string(),
        content: "Workaround content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: Some("Root cause".to_string()),
        solution: Some("Solution".to_string()),
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Low,
    };

    let result = record_learning(&conn, params).unwrap();
    let learning = get_learning(&conn, result.learning_id).unwrap().unwrap();

    assert_eq!(learning.id, Some(result.learning_id));
    assert_eq!(learning.title, "Workaround title");
    assert_eq!(learning.content, "Workaround content");
    assert_eq!(learning.outcome, LearningOutcome::Workaround);
    assert_eq!(learning.root_cause, Some("Root cause".to_string()));
    assert_eq!(learning.solution, Some("Solution".to_string()));
    assert_eq!(learning.confidence, Confidence::Low);
    assert_eq!(learning.times_shown, 0);
    assert_eq!(learning.times_applied, 0);
}

#[test]
fn test_get_learning_not_found() {
    let (_temp_dir, conn) = setup_db();

    let learning = get_learning(&conn, 999).unwrap();
    assert!(learning.is_none());
}

#[test]
fn test_get_learning_with_json_arrays() {
    let (_temp_dir, conn) = setup_db();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "JSON arrays".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()]),
        applies_to_task_types: Some(vec!["US-".to_string()]),
        applies_to_errors: Some(vec!["E0001".to_string(), "E0002".to_string()]),
        tags: None,
        confidence: Confidence::High,
    };

    let result = record_learning(&conn, params).unwrap();
    let learning = get_learning(&conn, result.learning_id).unwrap().unwrap();

    assert_eq!(
        learning.applies_to_files,
        Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()])
    );
    assert_eq!(
        learning.applies_to_task_types,
        Some(vec!["US-".to_string()])
    );
    assert_eq!(
        learning.applies_to_errors,
        Some(vec!["E0001".to_string(), "E0002".to_string()])
    );
}

#[test]
fn test_get_learning_tags_empty() {
    let (_temp_dir, conn) = setup_db();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "No tags".to_string(),
        content: "Content".to_string(),
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

    let result = record_learning(&conn, params).unwrap();
    let tags = get_learning_tags(&conn, result.learning_id).unwrap();

    assert!(tags.is_empty());
}

#[test]
fn test_record_learning_all_outcomes() {
    let (_temp_dir, conn) = setup_db();

    let outcomes = [
        LearningOutcome::Failure,
        LearningOutcome::Success,
        LearningOutcome::Workaround,
        LearningOutcome::Pattern,
    ];

    for outcome in outcomes {
        let params = RecordLearningParams {
            outcome,
            title: format!("{} test", outcome),
            content: "Content".to_string(),
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

        let result = record_learning(&conn, params).unwrap();
        assert_eq!(result.outcome, outcome);
    }
}

#[test]
fn test_record_learning_all_confidences() {
    let (_temp_dir, conn) = setup_db();

    let confidences = [Confidence::High, Confidence::Medium, Confidence::Low];

    for confidence in confidences {
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: format!("{} confidence", confidence),
            content: "Content".to_string(),
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

        let result = record_learning(&conn, params).unwrap();
        let learning = get_learning(&conn, result.learning_id).unwrap().unwrap();
        assert_eq!(learning.confidence, confidence);
    }
}

#[test]
fn test_delete_learning() {
    let (_temp_dir, conn) = setup_db();

    // Create a learning
    let params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "To be deleted".to_string(),
        content: "This will be deleted".to_string(),
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

    let record_result = record_learning(&conn, params).unwrap();
    let learning_id = record_result.learning_id;

    // Verify it exists
    assert!(get_learning(&conn, learning_id).unwrap().is_some());

    // Delete it
    let delete_result = delete_learning(&conn, learning_id).unwrap();

    assert_eq!(delete_result.learning_id, learning_id);
    assert_eq!(delete_result.title, "To be deleted");
    assert_eq!(delete_result.tags_deleted, 0);

    // Verify it no longer exists
    assert!(get_learning(&conn, learning_id).unwrap().is_none());
}

#[test]
fn test_delete_learning_with_tags() {
    let (_temp_dir, conn) = setup_db();

    // Create a learning with tags
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Tagged learning".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec![
            "rust".to_string(),
            "cli".to_string(),
            "test".to_string(),
        ]),
        confidence: Confidence::High,
    };

    let record_result = record_learning(&conn, params).unwrap();
    let learning_id = record_result.learning_id;

    // Verify tags exist
    let tags = get_learning_tags(&conn, learning_id).unwrap();
    assert_eq!(tags.len(), 3);

    // Delete it
    let delete_result = delete_learning(&conn, learning_id).unwrap();

    assert_eq!(delete_result.learning_id, learning_id);
    assert_eq!(delete_result.tags_deleted, 3);

    // Verify learning no longer exists
    assert!(get_learning(&conn, learning_id).unwrap().is_none());

    // Verify tags are cascade deleted
    let remaining_tags = get_learning_tags(&conn, learning_id).unwrap();
    assert!(remaining_tags.is_empty());
}

#[test]
fn test_delete_learning_not_found() {
    let (_temp_dir, conn) = setup_db();

    // Try to delete non-existent learning
    let result = delete_learning(&conn, 999);
    assert!(result.is_err());
}

#[test]
fn test_format_delete_text() {
    let result = DeleteLearningResult {
        learning_id: 42,
        title: "Test learning".to_string(),
        tags_deleted: 2,
    };

    let text = format_delete_text(&result);
    assert!(text.contains("Deleted learning #42"));
    assert!(text.contains("Test learning"));
}

// ============ Edit learning tests ============

fn create_test_learning(conn: &Connection) -> i64 {
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Original Title".to_string(),
        content: "Original Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: Some("Original Root Cause".to_string()),
        solution: Some("Original Solution".to_string()),
        applies_to_files: Some(vec!["src/old.rs".to_string()]),
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["original-tag".to_string()]),
        confidence: Confidence::Medium,
    };
    record_learning(conn, params).unwrap().learning_id
}

#[test]
fn test_edit_learning_title() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        title: Some("New Title".to_string()),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert_eq!(result.learning_id, learning_id);
    assert_eq!(result.title, "New Title");
    assert!(result.updated_fields.contains(&"title".to_string()));

    // Verify in database
    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert_eq!(learning.title, "New Title");
}

#[test]
fn test_edit_learning_content() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        content: Some("New Content".to_string()),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(result.updated_fields.contains(&"content".to_string()));

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert_eq!(learning.content, "New Content");
}

#[test]
fn test_edit_learning_solution_and_root_cause() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        solution: Some("New Solution".to_string()),
        root_cause: Some("New Root Cause".to_string()),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(result.updated_fields.contains(&"solution".to_string()));
    assert!(result.updated_fields.contains(&"root_cause".to_string()));

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert_eq!(learning.solution, Some("New Solution".to_string()));
    assert_eq!(learning.root_cause, Some("New Root Cause".to_string()));
}

#[test]
fn test_edit_learning_confidence() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        confidence: Some(Confidence::High),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(result.updated_fields.contains(&"confidence".to_string()));

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert_eq!(learning.confidence, Confidence::High);
}

#[test]
fn test_edit_learning_add_tags() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        add_tags: Some(vec!["new-tag1".to_string(), "new-tag2".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert_eq!(result.tags_added, 2);
    assert!(result.updated_fields.contains(&"tags".to_string()));

    let tags = get_learning_tags(&conn, learning_id).unwrap();
    assert!(tags.contains(&"new-tag1".to_string()));
    assert!(tags.contains(&"new-tag2".to_string()));
    assert!(tags.contains(&"original-tag".to_string()));
}

#[test]
fn test_edit_learning_remove_tags() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        remove_tags: Some(vec!["original-tag".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert_eq!(result.tags_removed, 1);
    assert!(result.updated_fields.contains(&"tags".to_string()));

    let tags = get_learning_tags(&conn, learning_id).unwrap();
    assert!(!tags.contains(&"original-tag".to_string()));
}

#[test]
fn test_edit_learning_add_and_remove_tags() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        add_tags: Some(vec!["new-tag".to_string()]),
        remove_tags: Some(vec!["original-tag".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert_eq!(result.tags_added, 1);
    assert_eq!(result.tags_removed, 1);

    let tags = get_learning_tags(&conn, learning_id).unwrap();
    assert!(tags.contains(&"new-tag".to_string()));
    assert!(!tags.contains(&"original-tag".to_string()));
}

#[test]
fn test_edit_learning_add_files() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        add_files: Some(vec!["src/new.rs".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_files".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let files = learning.applies_to_files.unwrap();
    assert!(files.contains(&"src/old.rs".to_string()));
    assert!(files.contains(&"src/new.rs".to_string()));
}

#[test]
fn test_edit_learning_remove_files() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        remove_files: Some(vec!["src/old.rs".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_files".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    // Files should be empty or None now
    assert!(learning.applies_to_files.is_none() || learning.applies_to_files.unwrap().is_empty());
}

#[test]
fn test_edit_learning_no_updates() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams::default();

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(result.updated_fields.is_empty());
    assert_eq!(result.tags_added, 0);
    assert_eq!(result.tags_removed, 0);
}

#[test]
fn test_edit_learning_not_found() {
    let (_temp_dir, conn) = setup_db();

    let params = EditLearningParams {
        title: Some("New Title".to_string()),
        ..Default::default()
    };

    let result = edit_learning(&conn, 999, params);
    assert!(result.is_err());
}

#[test]
fn test_edit_learning_all_fields() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        title: Some("All New Title".to_string()),
        content: Some("All New Content".to_string()),
        solution: Some("All New Solution".to_string()),
        root_cause: Some("All New Root Cause".to_string()),
        confidence: Some(Confidence::Low),
        add_tags: Some(vec!["new-tag".to_string()]),
        remove_tags: Some(vec!["original-tag".to_string()]),
        add_files: Some(vec!["src/new.rs".to_string()]),
        remove_files: Some(vec!["src/old.rs".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();

    assert_eq!(result.title, "All New Title");
    assert!(result.updated_fields.contains(&"title".to_string()));
    assert!(result.updated_fields.contains(&"content".to_string()));
    assert!(result.updated_fields.contains(&"solution".to_string()));
    assert!(result.updated_fields.contains(&"root_cause".to_string()));
    assert!(result.updated_fields.contains(&"confidence".to_string()));
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_files".to_string())
    );
    assert!(result.updated_fields.contains(&"tags".to_string()));
    assert_eq!(result.tags_added, 1);
    assert_eq!(result.tags_removed, 1);

    // Verify database state
    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert_eq!(learning.title, "All New Title");
    assert_eq!(learning.content, "All New Content");
    assert_eq!(learning.solution, Some("All New Solution".to_string()));
    assert_eq!(learning.root_cause, Some("All New Root Cause".to_string()));
    assert_eq!(learning.confidence, Confidence::Low);

    let files = learning.applies_to_files.unwrap();
    assert!(files.contains(&"src/new.rs".to_string()));
    assert!(!files.contains(&"src/old.rs".to_string()));

    let tags = get_learning_tags(&conn, learning_id).unwrap();
    assert!(tags.contains(&"new-tag".to_string()));
    assert!(!tags.contains(&"original-tag".to_string()));
}

#[test]
fn test_edit_learning_params_has_updates() {
    let empty = EditLearningParams::default();
    assert!(!empty.has_updates());

    let with_title = EditLearningParams {
        title: Some("New".to_string()),
        ..Default::default()
    };
    assert!(with_title.has_updates());

    let with_tags = EditLearningParams {
        add_tags: Some(vec!["tag".to_string()]),
        ..Default::default()
    };
    assert!(with_tags.has_updates());
}

#[test]
fn test_format_edit_text() {
    let result = EditLearningResult {
        learning_id: 42,
        title: "Test Learning".to_string(),
        updated_fields: vec!["title".to_string(), "content".to_string()],
        tags_added: 2,
        tags_removed: 1,
    };

    let text = format_edit_text(&result);
    assert!(text.contains("Updated learning #42"));
    assert!(text.contains("Test Learning"));
    assert!(text.contains("Updated fields: title, content"));
    assert!(text.contains("Tags added: 2"));
    assert!(text.contains("Tags removed: 1"));
}

#[test]
fn test_format_edit_text_no_updates() {
    let result = EditLearningResult {
        learning_id: 42,
        title: "Test Learning".to_string(),
        updated_fields: vec![],
        tags_added: 0,
        tags_removed: 0,
    };

    let text = format_edit_text(&result);
    assert!(text.contains("No fields were updated"));
}

// ============ EditLearningParams: task_types and errors (FR-006) ============
// These tests are #[ignore] until FEAT-001 implements the update.rs handling.

#[test]
fn test_edit_learning_add_task_types() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        add_task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_task_types".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let task_types = learning.applies_to_task_types.unwrap();
    assert!(task_types.contains(&"US-".to_string()));
    assert!(task_types.contains(&"FIX-".to_string()));
}

#[test]
fn test_edit_learning_remove_task_types() {
    let (_temp_dir, conn) = setup_db();

    // Create a learning with pre-existing task types
    let create_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Task types learning".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    let learning_id = record_learning(&conn, create_params).unwrap().learning_id;

    let params = EditLearningParams {
        remove_task_types: Some(vec!["US-".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_task_types".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let task_types = learning.applies_to_task_types.unwrap();
    assert!(!task_types.contains(&"US-".to_string()));
    assert!(task_types.contains(&"FIX-".to_string()));
}

#[test]
fn test_edit_learning_add_errors() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        add_errors: Some(vec!["E0001".to_string(), "timeout".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_errors".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let errors = learning.applies_to_errors.unwrap();
    assert!(errors.contains(&"E0001".to_string()));
    assert!(errors.contains(&"timeout".to_string()));
}

#[test]
fn test_edit_learning_remove_errors() {
    let (_temp_dir, conn) = setup_db();

    // Create a learning with pre-existing errors
    let create_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Errors learning".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: Some(vec!["E0001".to_string(), "timeout".to_string()]),
        tags: None,
        confidence: Confidence::Medium,
    };
    let learning_id = record_learning(&conn, create_params).unwrap().learning_id;

    let params = EditLearningParams {
        remove_errors: Some(vec!["E0001".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_errors".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let errors = learning.applies_to_errors.unwrap();
    assert!(!errors.contains(&"E0001".to_string()));
    assert!(errors.contains(&"timeout".to_string()));
}

#[test]
fn test_edit_learning_task_types_null_field_creates_array() {
    // Learning starts with NULL applies_to_task_types; adding should create the array
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn); // created with applies_to_task_types: None

    let params = EditLearningParams {
        add_task_types: Some(vec!["US-".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_task_types".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert_eq!(
        learning.applies_to_task_types,
        Some(vec!["US-".to_string()])
    );
}

#[test]
fn test_edit_learning_errors_null_field_remove_is_noop() {
    // Learning starts with NULL applies_to_errors; removing should not crash
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn); // created with applies_to_errors: None

    let params = EditLearningParams {
        remove_errors: Some(vec!["E0001".to_string()]),
        ..Default::default()
    };

    // Should not error
    let result = edit_learning(&conn, learning_id, params).unwrap();
    // Field still updated (even though nothing changed)
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_errors".to_string())
    );

    // applies_to_errors should remain None/empty after removing from NULL
    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert!(
        learning.applies_to_errors.is_none()
            || learning
                .applies_to_errors
                .as_ref()
                .is_none_or(|v| v.is_empty())
    );
}

#[test]
fn test_edit_learning_task_types_duplicate_add_is_idempotent() {
    let (_temp_dir, conn) = setup_db();

    let create_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Idempotent test".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: Some(vec!["US-".to_string()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    let learning_id = record_learning(&conn, create_params).unwrap().learning_id;

    let params = EditLearningParams {
        add_task_types: Some(vec!["US-".to_string()]), // already exists
        ..Default::default()
    };

    edit_learning(&conn, learning_id, params).unwrap();

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let task_types = learning.applies_to_task_types.unwrap();
    // Should still be exactly one "US-", not two
    assert_eq!(task_types.iter().filter(|t| t.as_str() == "US-").count(), 1);
}

#[test]
fn test_edit_learning_task_types_does_not_overwrite_files() {
    // Known-bad discriminator: adding task_types must not affect applies_to_files
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn); // has applies_to_files: ["src/old.rs"]

    let params = EditLearningParams {
        add_task_types: Some(vec!["US-".to_string()]),
        ..Default::default()
    };

    edit_learning(&conn, learning_id, params).unwrap();

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    // applies_to_files must be unchanged
    let files = learning.applies_to_files.unwrap();
    assert_eq!(files, vec!["src/old.rs".to_string()]);
    // applies_to_task_types must be set
    assert_eq!(
        learning.applies_to_task_types,
        Some(vec!["US-".to_string()])
    );
}

#[test]
fn test_edit_learning_has_updates_with_task_types_and_errors() {
    // These fields exist on the struct (stubs), so has_updates() should reflect them
    let with_add_task_types = EditLearningParams {
        add_task_types: Some(vec!["US-".to_string()]),
        ..Default::default()
    };
    assert!(with_add_task_types.has_updates());

    let with_remove_task_types = EditLearningParams {
        remove_task_types: Some(vec!["US-".to_string()]),
        ..Default::default()
    };
    assert!(with_remove_task_types.has_updates());

    let with_add_errors = EditLearningParams {
        add_errors: Some(vec!["E0001".to_string()]),
        ..Default::default()
    };
    assert!(with_add_errors.has_updates());

    let with_remove_errors = EditLearningParams {
        remove_errors: Some(vec!["E0001".to_string()]),
        ..Default::default()
    };
    assert!(with_remove_errors.has_updates());
}

#[test]
fn test_edit_learning_duplicate_tag_ignored() {
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    // Try to add a tag that already exists
    let params = EditLearningParams {
        add_tags: Some(vec!["original-tag".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    // Should not count as added since it was a duplicate
    assert_eq!(result.tags_added, 0);
}

// ============ Comprehensive FR-006 tests (TEST-001 acceptance criteria) ============

#[test]
fn test_edit_learning_all_four_new_fields_simultaneously() {
    // Acceptance criterion: edit_learning with all 4 new fields set simultaneously
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        title: Some("Updated title".to_string()),
        add_task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
        remove_task_types: Some(vec![]), // no-op remove
        add_errors: Some(vec!["E0001".to_string(), "timeout".to_string()]),
        remove_errors: Some(vec![]), // no-op remove
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(result.updated_fields.contains(&"title".to_string()));
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_task_types".to_string())
    );
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_errors".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert_eq!(learning.title, "Updated title");
    let task_types = learning.applies_to_task_types.unwrap();
    assert!(task_types.contains(&"US-".to_string()));
    assert!(task_types.contains(&"FIX-".to_string()));
    let errors = learning.applies_to_errors.unwrap();
    assert!(errors.contains(&"E0001".to_string()));
    assert!(errors.contains(&"timeout".to_string()));
}

#[test]
fn test_edit_learning_add_and_remove_task_types_in_same_call() {
    // Acceptance criterion: add_task_types + remove_task_types in same call (remove first, then add)
    let (_temp_dir, conn) = setup_db();

    let create_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Combined edit".to_string(),
        content: "Content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: Some(vec!["US-".to_string(), "FEAT-".to_string()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    let learning_id = record_learning(&conn, create_params).unwrap().learning_id;

    // Remove FEAT-, add FIX- in same call
    let params = EditLearningParams {
        remove_task_types: Some(vec!["FEAT-".to_string()]),
        add_task_types: Some(vec!["FIX-".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_task_types".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let task_types = learning.applies_to_task_types.unwrap();
    assert!(task_types.contains(&"US-".to_string()), "US- should remain");
    assert!(
        task_types.contains(&"FIX-".to_string()),
        "FIX- should be added"
    );
    assert!(
        !task_types.contains(&"FEAT-".to_string()),
        "FEAT- should be removed"
    );
}

#[test]
fn test_edit_learning_add_errors_long_patterns() {
    // Acceptance criterion: add_errors with very long error patterns (stress test)
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    // Generate 50 error patterns of varying lengths
    let long_errors: Vec<String> = (0..50)
        .map(|i| format!("error_pattern_{:0>100}", i)) // 113-char patterns
        .collect();

    let params = EditLearningParams {
        add_errors: Some(long_errors.clone()),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_errors".to_string())
    );

    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let errors = learning.applies_to_errors.unwrap();
    assert_eq!(errors.len(), 50);
    // Spot check first and last
    assert_eq!(errors[0], long_errors[0]);
    assert_eq!(errors[49], long_errors[49]);
}

#[test]
fn test_edit_learning_task_types_round_trip_add_read_remove_null() {
    // Acceptance criterion: round-trip: add → read → verify populated → remove → verify NULL
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn); // starts with no task_types

    // Step 1: add task types
    let add_params = EditLearningParams {
        add_task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
        ..Default::default()
    };
    edit_learning(&conn, learning_id, add_params).unwrap();

    // Step 2: read and verify populated
    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    let task_types = learning.applies_to_task_types.unwrap();
    assert_eq!(task_types.len(), 2);
    assert!(task_types.contains(&"US-".to_string()));
    assert!(task_types.contains(&"FIX-".to_string()));

    // Step 3: remove all → verify NULL (stored as None when empty)
    let remove_params = EditLearningParams {
        remove_task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
        ..Default::default()
    };
    edit_learning(&conn, learning_id, remove_params).unwrap();

    // Step 4: verify NULL
    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert!(
        learning.applies_to_task_types.is_none()
            || learning
                .applies_to_task_types
                .as_ref()
                .is_none_or(|v| v.is_empty()),
        "applies_to_task_types should be NULL/empty after removing all items"
    );
}

#[test]
fn test_edit_learning_updated_fields_includes_task_types_and_errors() {
    // Acceptance criterion: JSON output of EditLearningResult includes task_types and errors in updated_fields
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    let params = EditLearningParams {
        add_task_types: Some(vec!["US-".to_string()]),
        add_errors: Some(vec!["E0001".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();

    // Serialize to JSON and verify both fields appear
    let json = serde_json::to_string(&result).unwrap();
    assert!(
        json.contains("applies_to_task_types"),
        "JSON should contain applies_to_task_types in updated_fields"
    );
    assert!(
        json.contains("applies_to_errors"),
        "JSON should contain applies_to_errors in updated_fields"
    );
}

#[test]
fn test_edit_learning_only_new_fields_no_existing_fields() {
    // Acceptance criterion: edit_learning with only new fields set (no existing fields) works correctly
    let (_temp_dir, conn) = setup_db();
    let learning_id = create_test_learning(&conn);

    // Only set new fields, nothing else
    let params = EditLearningParams {
        add_task_types: Some(vec!["TEST-".to_string()]),
        add_errors: Some(vec!["compile error".to_string()]),
        ..Default::default()
    };

    let result = edit_learning(&conn, learning_id, params).unwrap();

    // Only task_types and errors should be in updated_fields
    assert_eq!(result.updated_fields.len(), 2);
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_task_types".to_string())
    );
    assert!(
        result
            .updated_fields
            .contains(&"applies_to_errors".to_string())
    );

    // Verify original fields remain unchanged
    let learning = get_learning(&conn, learning_id).unwrap().unwrap();
    assert_eq!(learning.title, "Original Title");
    assert_eq!(learning.content, "Original Content");
    // New fields set correctly
    assert_eq!(
        learning.applies_to_task_types,
        Some(vec!["TEST-".to_string()])
    );
    assert_eq!(
        learning.applies_to_errors,
        Some(vec!["compile error".to_string()])
    );
}
