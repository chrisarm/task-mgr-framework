//! Tests for retrieval backends.

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, open_connection};
use crate::learnings::crud::{record_learning, RecordLearningParams};
use crate::models::{Confidence, LearningOutcome};

use super::fts5::{escape_fts5_query, is_fts5_available};
use super::patterns::{extract_task_prefix, file_matches_pattern};
use super::{
    CompositeBackend, Fts5Backend, PatternsBackend, RetrievalBackend, RetrievalQuery,
};

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

// ========== Fts5Backend Tests ==========

#[test]
fn test_fts5_backend_no_text_returns_recent() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    create_test_learning(&conn, "Test", "Content", LearningOutcome::Pattern);

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();
    // With no text and no task context, returns recent learnings
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].learning.title, "Test");
}

#[test]
fn test_fts5_backend_defers_to_patterns_when_task_context() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    create_test_learning(&conn, "Test", "Content", LearningOutcome::Pattern);

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        task_files: vec!["src/main.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();
    // With task context but no text, FTS5 defers to PatternsBackend
    assert!(results.is_empty());
}

#[test]
fn test_fts5_backend_text_search() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    create_test_learning(
        &conn,
        "Database error",
        "SQLite crashed",
        LearningOutcome::Failure,
    );
    create_test_learning(
        &conn,
        "API success",
        "REST worked",
        LearningOutcome::Success,
    );

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("database".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].learning.title, "Database error");
    assert!(results[0].relevance_score > 0.0);
}

#[test]
fn test_fts5_backend_with_outcome_filter() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    create_test_learning(
        &conn,
        "Database failure",
        "The database crashed",
        LearningOutcome::Failure,
    );
    create_test_learning(
        &conn,
        "Database success",
        "Database worked great",
        LearningOutcome::Success,
    );

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("database".to_string()),
        outcome: Some(LearningOutcome::Failure),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].learning.title, "Database failure");
}

#[test]
fn test_fts5_backend_like_fallback() {
    let (_temp_dir, conn) = setup_db(); // No migrations = no FTS5

    create_test_learning(
        &conn,
        "Database error",
        "SQLite crashed",
        LearningOutcome::Failure,
    );

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("database".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].learning.title, "Database error");
}

// ========== PatternsBackend Tests ==========

#[test]
fn test_patterns_backend_no_context_returns_empty() {
    let (_temp_dir, conn) = setup_db();

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

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_patterns_backend_file_matching() {
    let (_temp_dir, conn) = setup_db();

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

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["src/db/schema.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].learning.title, "DB pattern");
    assert_eq!(results[0].relevance_score, 10.0);
}

#[test]
fn test_patterns_backend_type_matching() {
    let (_temp_dir, conn) = setup_db();

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

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_prefix: Some("US-001".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].learning.title, "US pattern");
    assert_eq!(results[0].relevance_score, 5.0);
}

// ========== CompositeBackend Tests ==========

#[test]
fn test_composite_backend_merges_results() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    // Create a learning with both text content AND file applicability
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Database pattern".to_string(),
        content: "Use transactions in database code".to_string(),
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

    let backend = CompositeBackend::default_backends();
    let query = RetrievalQuery {
        text: Some("database".to_string()),
        task_files: vec!["src/db/schema.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    // Should find 1 result (deduplicated), matched by both backends
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].learning.title, "Database pattern");
    // Should have combined match reasons
    let reason = results[0].match_reason.as_ref().unwrap();
    assert!(
        reason.contains("FTS5") || reason.contains("file pattern"),
        "Expected combined reason, got: {}",
        reason
    );
}

#[test]
fn test_composite_backend_respects_limit() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    for i in 1..=5 {
        create_test_learning(
            &conn,
            &format!("Learning {}", i),
            "Content",
            LearningOutcome::Pattern,
        );
    }

    let backend = CompositeBackend::default_backends();
    let query = RetrievalQuery {
        text: Some("Content".to_string()),
        limit: 2,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();
    assert!(results.len() <= 2);
}

// ========== LIKE Escape Tests ==========

#[test]
fn test_like_escape_metacharacters() {
    let (_temp_dir, conn) = setup_db(); // No FTS5 → uses LIKE fallback

    // Insert learnings — one with a literal % in the title
    create_test_learning(
        &conn,
        "50% discount applied",
        "Price was reduced",
        LearningOutcome::Success,
    );
    create_test_learning(
        &conn,
        "Normal learning",
        "Nothing special here",
        LearningOutcome::Pattern,
    );

    let backend = Fts5Backend;

    // Searching for literal "%" should match only the first row, not everything
    let query = RetrievalQuery {
        text: Some("%".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();
    assert_eq!(results.len(), 1, "% should match only the row containing literal %");
    assert_eq!(results[0].learning.title, "50% discount applied");

    // Searching for literal "_" should match nothing (no titles contain _)
    let query_underscore = RetrievalQuery {
        text: Some("_".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results_underscore = backend.retrieve(&conn, &query_underscore).unwrap();
    assert!(
        results_underscore.is_empty(),
        "_ should not act as wildcard, got {} results",
        results_underscore.len()
    );
}

// ========== Composite Ordering Tests ==========

#[test]
fn test_composite_preserves_insertion_order() {
    let (_temp_dir, conn) = setup_db(); // No FTS5 → unfiltered path

    // Create 3 learnings with different last_applied_at values
    let id1 = create_test_learning(&conn, "Old", "Content", LearningOutcome::Pattern);
    let id2 = create_test_learning(&conn, "Middle", "Content", LearningOutcome::Pattern);
    let id3 = create_test_learning(&conn, "Recent", "Content", LearningOutcome::Pattern);

    // Set last_applied_at: id3 most recent, id2 middle, id1 oldest
    conn.execute(
        "UPDATE learnings SET last_applied_at = datetime('now', '-3 hours') WHERE id = ?1",
        [id1],
    )
    .unwrap();
    conn.execute(
        "UPDATE learnings SET last_applied_at = datetime('now', '-1 hour') WHERE id = ?1",
        [id2],
    )
    .unwrap();
    conn.execute(
        "UPDATE learnings SET last_applied_at = datetime('now') WHERE id = ?1",
        [id3],
    )
    .unwrap();

    // No text query → all get 0.5 score from unfiltered path
    let backend = CompositeBackend::default_backends();
    let query = RetrievalQuery {
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 3);
    // SQL orders by last_applied_at DESC: Recent, Middle, Old
    assert_eq!(results[0].learning.title, "Recent");
    assert_eq!(results[1].learning.title, "Middle");
    assert_eq!(results[2].learning.title, "Old");
}

// ========== Utility Tests (moved from recall/tests.rs) ==========

#[test]
fn test_extract_task_prefix() {
    assert_eq!(extract_task_prefix("US-001"), "US-001");
    assert_eq!(extract_task_prefix("FIX-123"), "FIX-123");
    assert_eq!(extract_task_prefix("SEC-42"), "SEC-42");
    assert_eq!(extract_task_prefix("TECH-999"), "TECH-999");
    assert_eq!(extract_task_prefix("nodash"), "nodash");
}

#[test]
fn test_extract_task_prefix_uuid_prefixed() {
    assert_eq!(
        extract_task_prefix("f424ade5-PA-FEAT-003"),
        "PA-FEAT-003"
    );
    assert_eq!(extract_task_prefix("abcdef01-US-001"), "US-001");
    assert_eq!(extract_task_prefix("00000000-FIX-42"), "FIX-42");
}

#[test]
fn test_extract_task_prefix_edge_cases() {
    // 7 hex chars — too short for UUID prefix, no strip
    assert_eq!(extract_task_prefix("abcdef0-X"), "abcdef0-X");
    // No dash after hex — not a UUID prefix
    assert_eq!(extract_task_prefix("abcdef01X"), "abcdef01X");
    // Empty string
    assert_eq!(extract_task_prefix(""), "");
    // Exactly 9 chars (8 hex + dash) with nothing after — returns empty
    assert_eq!(extract_task_prefix("abcdef01-"), "");
}

#[test]
fn test_file_matches_pattern_exact() {
    assert!(file_matches_pattern("src/main.rs", "src/main.rs"));
    assert!(!file_matches_pattern("src/main.rs", "src/lib.rs"));
}

#[test]
fn test_file_matches_pattern_wildcard() {
    assert!(file_matches_pattern("src/main.rs", "*.rs"));
    assert!(file_matches_pattern("src/main.rs", "src/*.rs"));
    assert!(file_matches_pattern("src/db/connection.rs", "src/db/*.rs"));
    assert!(file_matches_pattern("src/db/connection.rs", "*/db/*"));
    assert!(!file_matches_pattern("src/main.rs", "*.py"));
}

#[test]
fn test_file_matches_pattern_case_insensitive() {
    assert!(file_matches_pattern("src/Main.rs", "src/main.rs"));
    assert!(file_matches_pattern("SRC/main.RS", "src/main.rs"));
}

#[test]
fn test_escape_fts5_query_simple() {
    assert_eq!(escape_fts5_query("hello"), "\"hello\"");
}

#[test]
fn test_escape_fts5_query_with_quotes() {
    assert_eq!(
        escape_fts5_query("hello \"world\""),
        "\"hello \"\"world\"\"\""
    );
}

#[test]
fn test_is_fts5_available_without_migration() {
    let (_temp_dir, conn) = setup_db();
    assert!(!is_fts5_available(&conn));
}

#[test]
fn test_is_fts5_available_after_migration() {
    let (_temp_dir, conn) = setup_db_with_fts5();
    assert!(is_fts5_available(&conn));
}
