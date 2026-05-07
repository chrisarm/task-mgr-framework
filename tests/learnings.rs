//! Integration tests for the learnings system.
//!
//! These tests verify learning creation, storage, and recall functionality
//! as an end-to-end workflow.

use tempfile::TempDir;

use task_mgr::commands::init;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::learnings::crud::{
    RecordLearningParams, get_learning, get_learning_tags, record_learning,
};
use task_mgr::learnings::recall::{RecallParams, recall_learnings};
use task_mgr::models::{Confidence, LearningOutcome};

/// Get the path to the sample PRD fixture file.
fn sample_prd_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_prd.json")
}

/// Set up a fresh database with schema and all migrations.
fn setup_db() -> (TempDir, rusqlite::Connection) {
    use task_mgr::db::migrations::run_migrations;
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (temp_dir, conn)
}

/// Set up a fresh database with schema and all migrations (needed for UCB columns).
fn setup_db_with_migrations() -> (TempDir, rusqlite::Connection) {
    use task_mgr::db::migrations::run_migrations;
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (temp_dir, conn)
}

// =============================================================================
// Test: create learning -> verify stored correctly
// =============================================================================

#[test]
fn test_create_learning_stores_all_fields() {
    let (_temp_dir, conn) = setup_db();

    // Create a learning with all fields populated
    let params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "Database connection timeout".to_string(),
        content: "The SQLite database connection timed out after 5 seconds".to_string(),
        task_id: None,
        run_id: None,
        root_cause: Some("Database lock was held by another process".to_string()),
        solution: Some("Increased busy_timeout pragma to 10 seconds".to_string()),
        applies_to_files: Some(vec![
            "src/db/*.rs".to_string(),
            "src/commands/*.rs".to_string(),
        ]),
        applies_to_task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
        applies_to_errors: Some(vec!["timeout".to_string(), "SQLITE_BUSY".to_string()]),
        tags: Some(vec![
            "database".to_string(),
            "sqlite".to_string(),
            "timeout".to_string(),
        ]),
        confidence: Confidence::High,
    };

    let result = record_learning(&conn, params).unwrap();
    assert!(result.learning_id > 0);
    assert_eq!(result.title, "Database connection timeout");
    assert_eq!(result.outcome, LearningOutcome::Failure);
    assert_eq!(result.tags_added, 3);

    // Retrieve and verify the learning
    let learning = get_learning(&conn, result.learning_id)
        .unwrap()
        .expect("Learning should exist");

    assert_eq!(learning.title, "Database connection timeout");
    assert_eq!(
        learning.content,
        "The SQLite database connection timed out after 5 seconds"
    );
    assert_eq!(learning.outcome, LearningOutcome::Failure);
    assert_eq!(
        learning.root_cause,
        Some("Database lock was held by another process".to_string())
    );
    assert_eq!(
        learning.solution,
        Some("Increased busy_timeout pragma to 10 seconds".to_string())
    );
    assert_eq!(learning.confidence, Confidence::High);

    // Verify JSON array fields
    let applies_files = learning.applies_to_files.unwrap();
    assert_eq!(applies_files.len(), 2);
    assert!(applies_files.contains(&"src/db/*.rs".to_string()));
    assert!(applies_files.contains(&"src/commands/*.rs".to_string()));

    let applies_types = learning.applies_to_task_types.unwrap();
    assert_eq!(applies_types.len(), 2);
    assert!(applies_types.contains(&"US-".to_string()));

    let applies_errors = learning.applies_to_errors.unwrap();
    assert_eq!(applies_errors.len(), 2);
    assert!(applies_errors.contains(&"timeout".to_string()));

    // Verify tags are stored
    let tags = get_learning_tags(&conn, result.learning_id).unwrap();
    assert_eq!(tags.len(), 3);
    // Tags are returned sorted alphabetically
    assert_eq!(tags, vec!["database", "sqlite", "timeout"]);

    // Verify initial stats
    assert_eq!(learning.times_shown, 0);
    assert_eq!(learning.times_applied, 0);
    assert!(learning.last_shown_at.is_none());
    assert!(learning.last_applied_at.is_none());
}

#[test]
fn test_create_learning_with_minimal_fields() {
    let (_temp_dir, conn) = setup_db();

    // Create a learning with only required fields
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Simple pattern".to_string(),
        content: "A minimal learning".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium, // Default
    };

    let result = record_learning(&conn, params).unwrap();
    assert!(result.learning_id > 0);
    assert_eq!(result.tags_added, 0);

    // Retrieve and verify
    let learning = get_learning(&conn, result.learning_id)
        .unwrap()
        .expect("Learning should exist");
    assert_eq!(learning.title, "Simple pattern");
    assert_eq!(learning.confidence, Confidence::Medium);
    assert!(learning.root_cause.is_none());
    assert!(learning.solution.is_none());
    assert!(learning.applies_to_files.is_none());
}

// =============================================================================
// Test: recall by file pattern matches
// =============================================================================

#[test]
fn test_recall_by_file_pattern_matches() {
    let (temp_dir, conn) = setup_db_with_migrations();

    // Import sample PRD to get tasks with files
    init::init(
        temp_dir.path(),
        &[&sample_prd_path()],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Create learnings with different file patterns
    let db_learning = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Database pattern".to_string(),
        content: "Use transactions for batch operations".to_string(),
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
    record_learning(&conn, db_learning).unwrap();

    let cli_learning = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "CLI pattern".to_string(),
        content: "Use clap derive macros".to_string(),
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
    record_learning(&conn, cli_learning).unwrap();

    // Add a task with db files for testing
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('TEST-DB', 'DB Task', 'todo')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('TEST-DB', 'src/db/connection.rs')",
        [],
    )
    .unwrap();

    // Recall for the DB task - should match the db pattern
    let params = RecallParams {
        for_task: Some("TEST-DB".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    // DB pattern matches via file, CLI pattern comes via UCB fallback
    assert_eq!(result.count, 2);
    // File-matched learning should be first (higher relevance tier)
    assert_eq!(result.learnings[0].title, "Database pattern");
}

#[test]
fn test_recall_file_pattern_with_wildcard() {
    let (_temp_dir, conn) = setup_db_with_migrations();

    // Create learning with glob pattern
    let params = RecordLearningParams {
        outcome: LearningOutcome::Success,
        title: "Rust file pattern".to_string(),
        content: "Works for all Rust files".to_string(),
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

    // Create a task with a Rust file
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('TEST-RS', 'Rust Task', 'todo')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('TEST-RS', 'src/main.rs')",
        [],
    )
    .unwrap();

    // Recall for task
    let recall_params = RecallParams {
        for_task: Some("TEST-RS".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Rust file pattern");
}

// =============================================================================
// Test: recall by task type prefix matches
// =============================================================================

#[test]
fn test_recall_by_task_type_prefix_matches() {
    let (_temp_dir, conn) = setup_db_with_migrations();

    // Create learnings for different task types
    let us_learning = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "User story pattern".to_string(),
        content: "Break down user stories into small tasks".to_string(),
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
    record_learning(&conn, us_learning).unwrap();

    let fix_learning = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Bug fix pattern".to_string(),
        content: "Always add regression tests for fixes".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: Some(vec!["FIX-".to_string()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    record_learning(&conn, fix_learning).unwrap();

    let sec_learning = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Security pattern".to_string(),
        content: "Validate all inputs".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: Some(vec!["SEC-".to_string()]),
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    record_learning(&conn, sec_learning).unwrap();

    // Create tasks of different types
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'User Story Task', 'todo')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('FIX-001', 'Bug Fix Task', 'todo')",
        [],
    )
    .unwrap();

    // Recall for US task - US pattern should be first, others via UCB fallback
    let us_params = RecallParams {
        for_task: Some("US-001".to_string()),
        limit: 10,
        ..Default::default()
    };
    let us_result = recall_learnings(&conn, us_params).unwrap();
    assert_eq!(us_result.count, 3); // 1 type match + 2 UCB fallback
    assert_eq!(us_result.learnings[0].title, "User story pattern");

    // Recall for FIX task - FIX pattern should be first, others via UCB fallback
    let fix_params = RecallParams {
        for_task: Some("FIX-001".to_string()),
        limit: 10,
        ..Default::default()
    };
    let fix_result = recall_learnings(&conn, fix_params).unwrap();
    assert_eq!(fix_result.count, 3); // 1 type match + 2 UCB fallback
    assert_eq!(fix_result.learnings[0].title, "Bug fix pattern");
}

#[test]
fn test_recall_task_type_multiple_prefixes() {
    let (_temp_dir, conn) = setup_db_with_migrations();

    // Create a learning that applies to multiple task types
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Multi-type pattern".to_string(),
        content: "Applies to both US and FIX tasks".to_string(),
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
    record_learning(&conn, params).unwrap();

    // Create tasks
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('US-002', 'User Story 2', 'todo')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('FIX-002', 'Bug Fix 2', 'todo')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('SEC-001', 'Security Task', 'todo')",
        [],
    )
    .unwrap();

    // Both US and FIX should match via type prefix (learning is first)
    let us_params = RecallParams {
        for_task: Some("US-002".to_string()),
        limit: 10,
        ..Default::default()
    };
    let us_result = recall_learnings(&conn, us_params).unwrap();
    assert_eq!(us_result.count, 1);
    assert_eq!(us_result.learnings[0].title, "Multi-type pattern");

    let fix_params = RecallParams {
        for_task: Some("FIX-002".to_string()),
        limit: 10,
        ..Default::default()
    };
    let fix_result = recall_learnings(&conn, fix_params).unwrap();
    assert_eq!(fix_result.count, 1);
    assert_eq!(fix_result.learnings[0].title, "Multi-type pattern");

    // SEC doesn't type-match, but UCB fallback returns it as exploration candidate
    let sec_params = RecallParams {
        for_task: Some("SEC-001".to_string()),
        limit: 10,
        ..Default::default()
    };
    let sec_result = recall_learnings(&conn, sec_params).unwrap();
    assert_eq!(sec_result.count, 1);
    assert_eq!(sec_result.learnings[0].title, "Multi-type pattern");
}

// =============================================================================
// Test: recall by text query
// =============================================================================

#[test]
fn test_recall_by_text_query_title() {
    let (_temp_dir, conn) = setup_db();

    // Create several learnings
    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Failure,
            title: "Database connection error".to_string(),
            content: "The connection failed".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Success,
            title: "API integration success".to_string(),
            content: "The API worked correctly".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        },
    )
    .unwrap();

    // Search by title keyword
    let params = RecallParams {
        query: Some("database".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Database connection error");
}

#[test]
fn test_recall_by_text_query_content() {
    let (_temp_dir, conn) = setup_db();

    // Create learnings
    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Workaround,
            title: "Issue workaround".to_string(),
            content: "Added a retry mechanism with exponential backoff".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Code pattern".to_string(),
            content: "Use dependency injection for testability".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        },
    )
    .unwrap();

    // Search by content keyword
    let params = RecallParams {
        query: Some("exponential".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Issue workaround");
}

#[test]
fn test_recall_text_query_case_insensitive() {
    let (_temp_dir, conn) = setup_db();

    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "SQLite Pattern".to_string(),
            content: "Use WAL mode for better concurrency".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        },
    )
    .unwrap();

    // Search with different case
    let params = RecallParams {
        query: Some("sqlite".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "SQLite Pattern");
}

// =============================================================================
// Test: recall orders by most recently applied
// =============================================================================

#[test]
fn test_recall_orders_by_most_recently_applied() {
    let (_temp_dir, conn) = setup_db();

    // Create three learnings
    let learning1 = record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Pattern A".to_string(),
            content: "Content A".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    let learning2 = record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Pattern B".to_string(),
            content: "Content B".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    let _learning3 = record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Pattern C".to_string(),
            content: "Content C".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    // Set last_applied_at for learning2 (make it most recently applied)
    conn.execute(
        "UPDATE learnings SET last_applied_at = datetime('now', '-1 day') WHERE id = ?1",
        [learning1.learning_id],
    )
    .unwrap();
    conn.execute(
        "UPDATE learnings SET last_applied_at = datetime('now') WHERE id = ?1",
        [learning2.learning_id],
    )
    .unwrap();
    // learning3 has no last_applied_at (NULL)

    // Recall all
    let params = RecallParams {
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 3);
    // Order should be: learning2 (most recent), learning1 (older), learning3 (never applied)
    assert_eq!(result.learnings[0].title, "Pattern B");
    assert_eq!(result.learnings[1].title, "Pattern A");
    assert_eq!(result.learnings[2].title, "Pattern C");
}

#[test]
fn test_recall_increments_times_shown() {
    let (_temp_dir, conn) = setup_db();

    let learning = record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Show count test".to_string(),
            content: "Test content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    // Verify initial state
    let initial = get_learning(&conn, learning.learning_id).unwrap().unwrap();
    assert_eq!(initial.times_shown, 0);

    // Recall once
    let make_params = || RecallParams {
        limit: 10,
        ..Default::default()
    };
    recall_learnings(&conn, make_params()).unwrap();

    // Recall no longer increments times_shown (bandit::record_learning_shown does)
    let after_one = get_learning(&conn, learning.learning_id).unwrap().unwrap();
    assert_eq!(after_one.times_shown, 0);

    // Recall again — still 0
    recall_learnings(&conn, make_params()).unwrap();

    let after_two = get_learning(&conn, learning.learning_id).unwrap().unwrap();
    assert_eq!(after_two.times_shown, 0);
}

// =============================================================================
// Additional tests for edge cases
// =============================================================================

#[test]
fn test_recall_with_outcome_filter() {
    let (_temp_dir, conn) = setup_db();

    // Create learnings with different outcomes
    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Failure,
            title: "Failure learning".to_string(),
            content: "Something failed".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Success,
            title: "Success learning".to_string(),
            content: "Something succeeded".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        },
    )
    .unwrap();

    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Workaround,
            title: "Workaround learning".to_string(),
            content: "A workaround".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    // Filter by failure outcome
    let params = RecallParams {
        outcome: Some(LearningOutcome::Failure),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].outcome, LearningOutcome::Failure);
}

#[test]
fn test_recall_with_tags_filter() {
    let (_temp_dir, conn) = setup_db();

    // Create learnings with tags
    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Rust pattern".to_string(),
            content: "Use Result for error handling".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: Some(vec!["rust".to_string(), "error-handling".to_string()]),
            confidence: Confidence::High,
        },
    )
    .unwrap();

    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Python pattern".to_string(),
            content: "Use try/except".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: Some(vec!["python".to_string(), "error-handling".to_string()]),
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    // Filter by rust tag
    let params = RecallParams {
        tags: Some(vec!["rust".to_string()]),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Rust pattern");

    // Filter by error-handling tag (should match both)
    let params2 = RecallParams {
        tags: Some(vec!["error-handling".to_string()]),
        limit: 10,
        ..Default::default()
    };
    let result2 = recall_learnings(&conn, params2).unwrap();

    assert_eq!(result2.count, 2);
}

#[test]
fn test_recall_with_limit() {
    let (_temp_dir, conn) = setup_db();

    // Create many learnings
    for i in 1..=10 {
        record_learning(
            &conn,
            RecordLearningParams {
                outcome: LearningOutcome::Pattern,
                title: format!("Learning {}", i),
                content: format!("Content {}", i),
                task_id: None,
                run_id: None,
                root_cause: None,
                solution: None,
                applies_to_files: None,
                applies_to_task_types: None,
                applies_to_errors: None,
                tags: None,
                confidence: Confidence::Medium,
            },
        )
        .unwrap();
    }

    // Recall with limit
    let params = RecallParams {
        limit: 3,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 3);
}

#[test]
fn test_recall_combined_filters() {
    let (_temp_dir, conn) = setup_db();

    // Create learnings with various attributes
    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Failure,
            title: "Database failure".to_string(),
            content: "SQLite error".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: Some(vec!["database".to_string()]),
            confidence: Confidence::High,
        },
    )
    .unwrap();

    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Success,
            title: "Database success".to_string(),
            content: "SQLite worked".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: Some(vec!["database".to_string()]),
            confidence: Confidence::High,
        },
    )
    .unwrap();

    record_learning(
        &conn,
        RecordLearningParams {
            outcome: LearningOutcome::Failure,
            title: "API failure".to_string(),
            content: "REST error".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: Some(vec!["api".to_string()]),
            confidence: Confidence::Medium,
        },
    )
    .unwrap();

    // Combine text query + outcome filter
    let params = RecallParams {
        query: Some("database".to_string()),
        outcome: Some(LearningOutcome::Failure),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 1);
    assert_eq!(result.learnings[0].title, "Database failure");
}

#[test]
fn test_recall_empty_database() {
    let (_temp_dir, conn) = setup_db();

    let params = RecallParams {
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert_eq!(result.count, 0);
    assert!(result.learnings.is_empty());
}

// =============================================================================
// INT-001: Integration tests for enriched learning recall (FEAT-002/003/005)
// =============================================================================

/// E2E: create task → learn with task_id (auto-populate) → PatternsBackend recall
/// with for-task context → verify auto-populated fields contributed to scoring.
#[test]
fn test_e2e_learn_auto_populate_then_recall_for_task() {
    use task_mgr::cli::enums::{Confidence as CliConfidence, LearningOutcome as CliOutcome};
    use task_mgr::commands::learn::{LearnParams, learn};
    use task_mgr::learnings::{PatternsBackend, RetrievalBackend, RetrievalQuery};

    let (_temp_dir, conn) = setup_db();

    // Create task with associated files in the DB
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('FEAT-003', 'Integration Test Task')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('FEAT-003', 'src/integration.rs')",
        [],
    )
    .unwrap();

    // learn() with task_id — auto-populate kicks in
    let learn_result = learn(
        &conn,
        None,
        LearnParams {
            outcome: CliOutcome::Pattern,
            title: "Auto-populated integration learning".to_string(),
            content: "End-to-end pipeline test".to_string(),
            task_id: Some("FEAT-003".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,      // auto-populate from task context
            task_types: None, // auto-populate from task ID prefix
            errors: None,
            tags: None,
            confidence: CliConfidence::High,
            supersedes: None,
        },
    )
    .unwrap();
    assert!(learn_result.learning_id > 0);

    // Recall with PatternsBackend using the same task context
    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["src/integration.rs".to_string()],
        task_prefix: Some("FEAT-003".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();
    assert_eq!(
        results.len(),
        1,
        "auto-populated learning must be found by task context"
    );
    // Should score at least FILE_MATCH(10) + TYPE_MATCH(5) = 15
    assert!(
        results[0].relevance_score >= 15.0,
        "auto-populate must contribute both file and type match scores, got: {}",
        results[0].relevance_score
    );
}

/// E2E: create learning with tags → FTS5 search by tag keyword → verify learning found.
#[test]
fn test_e2e_learning_with_tag_found_by_fts5_tag_keyword_search() {
    use task_mgr::learnings::{Fts5Backend, RetrievalBackend, RetrievalQuery};

    let (_temp_dir, conn) = setup_db_with_migrations();

    // Create learning tagged with 'workflow-engine-fix'
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "FTS5 tag integration test".to_string(),
        content: "This learning has no workflow keyword in title or content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["workflow-engine-fix".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // FTS5 search for 'workflow' — FTS5 tokenizes 'workflow-engine-fix' to include 'workflow' token
    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("workflow".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        !results.is_empty(),
        "FTS5 must find learning by tag token 'workflow'"
    );
    assert_eq!(
        results[0].learning.title, "FTS5 tag integration test",
        "correct learning returned via tag token search"
    );
}

/// E2E: PatternsBackend scoring — file match (10) + tag match (3) = 13.
#[test]
fn test_e2e_patterns_backend_file_and_tag_score_stack() {
    use task_mgr::learnings::{PatternsBackend, RetrievalBackend, RetrievalQuery};

    let (_temp_dir, conn) = setup_db();

    // Create learning with file pattern AND a workflow tag
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Stacked score integration test".to_string(),
        content: "Workflow engine pattern".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["**/workflow/**".to_string()]),
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["workflow-detour-phase3".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].relevance_score, 13.0,
        "file_match(10) + tag_match(3) = 13, got {}",
        results[0].relevance_score
    );
}

/// E2E: CompositeBackend merges FTS5 and PatternsBackend results with tag scoring.
#[test]
fn test_e2e_composite_backend_merges_fts5_and_patterns_with_tags() {
    use task_mgr::learnings::{CompositeBackend, RetrievalBackend, RetrievalQuery};

    let (_temp_dir, conn) = setup_db_with_migrations();

    // Tag-only learning (visible to PatternsBackend via tag scoring, FTS5 via token search)
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Composite integration tag test".to_string(),
        content: "This has no workflow keyword in title or content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["workflow-detour".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    let backend = CompositeBackend::default_backends();
    let query = RetrievalQuery {
        // Both FTS5 text search and PatternsBackend tag scoring should find this
        text: Some("workflow".to_string()),
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        !results.is_empty(),
        "CompositeBackend must find tag-only learning via merged results"
    );
    assert_eq!(
        results[0].learning.title, "Composite integration tag test",
        "correct learning returned by composite backend"
    );
}
