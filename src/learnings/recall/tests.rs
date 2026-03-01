//! Tests for the recall module.
//!
//! These tests verify the backward-compatible `recall_learnings()` function
//! which now delegates to the composite retrieval backend.

use rusqlite::Connection;
use tempfile::TempDir;

use super::{format_text, recall_learnings, RecallParams, RecallResult};
use crate::db::{create_schema, migrations::run_migrations, open_connection};
use crate::learnings::crud::{record_learning, RecordLearningParams};
use crate::models::{Confidence, Learning, LearningOutcome};

fn setup_db() -> (TempDir, Connection) {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
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

    create_test_learning(
        &conn,
        "Failure 1",
        "Content about failure",
        LearningOutcome::Failure,
    );
    create_test_learning(
        &conn,
        "Success 1",
        "Content about success",
        LearningOutcome::Success,
    );
    create_test_learning(
        &conn,
        "Failure 2",
        "Another failure story",
        LearningOutcome::Failure,
    );

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

    let learning_id = create_test_learning(
        &conn,
        "Test search target",
        "Searchable content",
        LearningOutcome::Pattern,
    );

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

    // Recall no longer increments times_shown (bandit::record_learning_shown does)
    let after: i32 = conn
        .query_row(
            "SELECT times_shown FROM learnings WHERE id = ?1",
            [learning_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, 0);
}

#[test]
fn test_recall_for_task_file_matching() {
    let (_temp_dir, conn) = setup_db_with_fts5();

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

    // DB pattern matches via file, CLI pattern comes via UCB fallback
    assert_eq!(result.count, 2);
    // File-matched learning should be first (higher relevance tier)
    assert_eq!(result.learnings[0].title, "DB pattern");
}

#[test]
fn test_recall_for_task_type_matching() {
    let (_temp_dir, conn) = setup_db_with_fts5();

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

    // US pattern matches via type prefix, FIX pattern comes via UCB fallback
    assert_eq!(result.count, 2);
    // Type-matched learning should be first (higher relevance tier)
    assert_eq!(result.learnings[0].title, "US pattern");
}

#[test]
fn test_recall_for_nonexistent_task() {
    let (_temp_dir, conn) = setup_db_with_fts5();

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

    // Recall for nonexistent task — UCB fallback fills empty slots
    let recall_params = RecallParams {
        for_task: Some("NONEXISTENT".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    // UCB fallback returns the learning as an exploration candidate
    assert_eq!(result.count, 1);
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

// ========== UCB Fallback Tests ==========

#[test]
fn test_ucb_fallback_fills_empty_slots() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    // Create a task with a file
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

    // Create 1 learning that will pattern-match (file match)
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

    // Create 4 learnings with no applicability (won't pattern-match)
    for i in 1..=4 {
        create_test_learning(
            &conn,
            &format!("Unmatched {}", i),
            &format!("Content {}", i),
            LearningOutcome::Pattern,
        );
    }

    let recall_params = RecallParams {
        for_task: Some("US-001".to_string()),
        limit: 5,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    // 1 pattern match + 4 UCB fallback = 5 total
    assert_eq!(result.count, 5);
    // Pattern-matched learning should be first (highest relevance tier)
    assert_eq!(result.learnings[0].title, "DB pattern");
}

#[test]
fn test_ucb_fallback_excludes_already_matched() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    // Create a task
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

    // Create 1 learning that will pattern-match
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

    let recall_params = RecallParams {
        for_task: Some("US-001".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    // Only 1 learning exists, so no duplicates possible — just 1 result
    assert_eq!(result.count, 1);
    // Verify no duplicate IDs
    let ids: Vec<Option<i64>> = result.learnings.iter().map(|l| l.id).collect();
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), unique.len(), "No duplicate learning IDs");
}

#[test]
fn test_ucb_fallback_only_activates_for_task() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    // Create learnings that won't text-match
    create_test_learning(&conn, "Alpha", "Unrelated", LearningOutcome::Pattern);
    create_test_learning(&conn, "Beta", "Unrelated", LearningOutcome::Pattern);

    // CLI recall with query but no --for-task: no UCB fallback
    let recall_params = RecallParams {
        query: Some("nonexistent-query-xyz".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    // No text match, no for_task → no fallback → 0 results
    assert_eq!(result.count, 0);
}

#[test]
fn test_rerank_preserves_relevance_tiers() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    // Create a task with a file
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

    // Create a file-matched learning (relevance = 10.0)
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "File matched".to_string(),
        content: "High relevance".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/db/*.rs".to_string()]),
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Low, // Low confidence, but high relevance tier
    };
    record_learning(&conn, params).unwrap();

    // Create a fallback learning (will get relevance = 0.1)
    create_test_learning(&conn, "Fallback", "Low relevance", LearningOutcome::Pattern);

    let recall_params = RecallParams {
        for_task: Some("US-001".to_string()),
        limit: 5,
        ..Default::default()
    };
    let result = recall_learnings(&conn, recall_params).unwrap();

    assert_eq!(result.count, 2);
    // File-matched (10.0 * 100 + ucb) always beats fallback (0.1 * 100 + ucb)
    assert_eq!(result.learnings[0].title, "File matched");
    assert_eq!(result.learnings[1].title, "Fallback");
}

// ========== TEST-INIT-001: retired_at Filtering Tests ==========
//
// Tests for retired learning exclusion in the recall and bandit paths.
// All tests are #[ignore] until FEAT-001 and FEAT-002 are implemented.
//
// Query locations covered:
//   6. Bandit total_window_shows aggregate (get_total_window_shows)
//   7. Recall list (recall_learnings text/recency path)
//   Exempt: get_learning() by ID still returns retired
//   Exempt: apply_learning() still works for retired

use crate::learnings::test_helpers::retire_learning;

#[test]
fn test_retired_excluded_from_bandit_total_window_shows() {
    // AC: retired learning excluded from bandit total_window_shows aggregate
    use crate::learnings::bandit::get_total_window_shows;

    let (_dir, conn) = setup_db_with_fts5();

    // Retired learning with window_shown = 7
    let retired_id =
        create_test_learning(&conn, "Retired bandit", "content", LearningOutcome::Pattern);
    conn.execute(
        "UPDATE learnings SET window_shown = 7 WHERE id = ?1",
        [retired_id],
    )
    .unwrap();
    retire_learning(&conn, retired_id);

    // Active learning with window_shown = 3
    let active_id =
        create_test_learning(&conn, "Active bandit", "content", LearningOutcome::Pattern);
    conn.execute(
        "UPDATE learnings SET window_shown = 3 WHERE id = ?1",
        [active_id],
    )
    .unwrap();

    let total = get_total_window_shows(&conn).unwrap();
    assert_eq!(
        total, 3,
        "retired learning's window_shown (7) must be excluded from total_window_shows aggregate; \
         got {total} (expected 3)"
    );
}

#[test]
fn test_retired_excluded_from_recall_text_search() {
    // AC: retired learning excluded from recall text search (LIKE or FTS5 path)
    let (_dir, conn) = setup_db_with_fts5();

    let retired_id = create_test_learning(
        &conn,
        "Retired recall target",
        "unique searchable xyz",
        LearningOutcome::Success,
    );
    retire_learning(&conn, retired_id);

    create_test_learning(
        &conn,
        "Active learning",
        "other content",
        LearningOutcome::Pattern,
    );

    let params = RecallParams {
        query: Some("searchable".to_string()),
        limit: 10,
        ..Default::default()
    };
    let result = recall_learnings(&conn, params).unwrap();

    assert!(
        result.learnings.iter().all(|l| l.id != Some(retired_id)),
        "retired learning must not appear in recall text search results"
    );
}

#[test]
fn test_get_learning_by_id_still_returns_retired() {
    // AC (exempt): get_learning() by ID is NOT subject to retired_at filter
    use crate::learnings::crud::get_learning;

    let (_dir, conn) = setup_db_with_fts5();

    let id = create_test_learning(
        &conn,
        "Retired learning",
        "content",
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);

    let result = get_learning(&conn, id).unwrap();
    assert!(
        result.is_some(),
        "get_learning() must still return retired learning by ID (single-record lookup is exempt)"
    );
    assert_eq!(result.unwrap().title, "Retired learning");
}

#[test]
fn test_apply_learning_works_for_retired() {
    // AC (exempt): apply_learning() by ID is NOT subject to retired_at filter
    use crate::commands::apply_learning::apply_learning;

    let (_dir, conn) = setup_db_with_fts5();

    let id = create_test_learning(
        &conn,
        "Retired apply target",
        "content",
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);

    let result = apply_learning(&conn, id);
    assert!(
        result.is_ok(),
        "apply_learning() must succeed for retired learning by ID (single-record lookup is exempt)"
    );
}
