//! Tests for retrieval backends.

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, open_connection};
use crate::learnings::crud::{RecordLearningParams, record_learning};
use crate::models::{Confidence, LearningOutcome};

use super::fts5::{escape_fts5_query, is_fts5_available};
use super::patterns::{extract_task_prefix, file_matches_pattern};
use super::{CompositeBackend, Fts5Backend, PatternsBackend, RetrievalBackend, RetrievalQuery};

fn setup_db() -> (TempDir, Connection) {
    use crate::db::migrations::run_migrations;
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
    // Run all migrations (for retired_at column), then remove FTS5 to force LIKE path.
    // Must drop triggers before the table to avoid insert failures.
    let (_temp_dir, conn) = setup_db();
    conn.execute("DROP TRIGGER IF EXISTS learnings_ai", [])
        .unwrap();
    conn.execute("DROP TRIGGER IF EXISTS learnings_ad", [])
        .unwrap();
    conn.execute("DROP TRIGGER IF EXISTS learnings_au", [])
        .unwrap();
    conn.execute("DROP TABLE IF EXISTS learnings_fts", [])
        .unwrap();

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
    assert_eq!(
        results.len(),
        1,
        "% should match only the row containing literal %"
    );
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

// ========== PatternsBackend Tag-Aware Retrieval Tests (B3/FR-005) ==========
// Tests are #[ignore] — they define expected behavior before implementation.
// TAG_CONTEXT_MATCH_SCORE = 3 (additive, less than TYPE_MATCH_SCORE = 5).
//
// Tag-to-path semantic mapping (implementation will define full table):
//   "workflow-detour*"  → matches paths containing "workflow/"
//   "ses" / "email"     → matches paths containing "ses/"
//   "consumer"          → matches paths containing "consumer/"
//
// Excluded (never trigger tag-path scoring):
//   Source tags: "long-term", "raw"
//   Category tags: "rust-patterns", "python-patterns", "architecture-patterns",
//                  "database-sql", "testing-patterns", "general"

/// Creates a learning with tags but no applies_to_* metadata.
/// Used to test tag-only learnings (no file/type/error applicability).
fn create_tagged_learning(conn: &Connection, title: &str, tags: Vec<&str>) -> i64 {
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: title.to_string(),
        content: "Tag-aware retrieval test content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(tags.into_iter().map(str::to_string).collect()),
        confidence: Confidence::High,
    };
    record_learning(conn, params).unwrap().learning_id
}

#[test]
fn test_tag_context_workflow_detour_phase3_matches_workflow_path() {
    // Happy path: semantic tag maps to workflow path → TAG_CONTEXT_MATCH_SCORE = 3
    let (_dir, conn) = setup_db();
    create_tagged_learning(
        &conn,
        "Workflow detour pattern",
        vec!["workflow-detour-phase3"],
    );

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(
        results.len(),
        1,
        "tag-only learning must be found via semantic tag"
    );
    assert_eq!(
        results[0].relevance_score, 3.0,
        "TAG_CONTEXT_MATCH_SCORE must be 3"
    );
}

#[test]
fn test_excluded_source_tag_long_term_scores_zero() {
    // Edge case: 'long-term' is a source/meta tag — must never trigger tag scoring.
    // Even with task files present, this learning should not appear in results.
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Long-term insight", vec!["long-term"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "excluded source tag 'long-term' must not contribute tag score"
    );
}

#[test]
fn test_excluded_source_tag_raw_scores_zero() {
    // Edge case: 'raw' is a source tag — must never trigger tag scoring.
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Raw observation", vec!["raw"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/any/path.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "excluded source tag 'raw' must not contribute tag score"
    );
}

#[test]
fn test_file_match_and_tag_match_scores_stack() {
    // Edge case: FILE_MATCH_SCORE (10) + TAG_CONTEXT_MATCH_SCORE (3) = 13.
    // Learning has both a matching applies_to_files glob AND a semantic tag.
    let (_dir, conn) = setup_db();
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Stacked scoring learning".to_string(),
        content: "File pattern + semantic tag both match".to_string(),
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
        "FILE_MATCH_SCORE (10) + TAG_CONTEXT_MATCH_SCORE (3) must stack to 13"
    );
}

#[test]
fn test_excluded_category_tag_rust_patterns_scores_zero() {
    // Edge case: 'rust-patterns' is a category tag — must never trigger tag scoring.
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Rust error handling pattern", vec!["rust-patterns"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "excluded category tag 'rust-patterns' must not contribute tag score"
    );
}

#[test]
fn test_tag_workflow_detour_does_not_match_tools_path() {
    // Known-bad discriminator: 'workflow-detour' maps to workflow/ paths, not tools/.
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Workflow detour note", vec!["workflow-detour"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        // tools/ path — NOT under workflow/, so tag should not match
        task_files: vec!["service/src/tools/some_tool.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "tag 'workflow-detour' must not match service/src/tools/ (wrong path)"
    );
}

#[test]
fn test_no_tag_context_match_without_task_files() {
    // Invariant: PatternsBackend returns empty when task context is absent.
    // Tag scoring must not fire without task_files — matches existing early-exit.
    let (_dir, conn) = setup_db();
    create_tagged_learning(
        &conn,
        "Tagged no-context learning",
        vec!["workflow-detour-phase3"],
    );

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        // No task_files, no task_prefix, no task_error
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "PatternsBackend must return empty when no task context is present"
    );
}

// ========== TEST-002: Comprehensive tag-aware retrieval tests ==========

#[test]
fn test_tag_ses_matches_ses_path() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "SES email insight", vec!["ses-email-limits"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/ses/message_sender.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1, "ses tag should match ses/ path");
    assert_eq!(
        results[0].relevance_score, 3.0,
        "TAG_CONTEXT_MATCH_SCORE = 3"
    );
}

#[test]
fn test_tag_email_matches_ses_path() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Email delivery note", vec!["email-delivery"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/ses/delivery.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1, "email tag should match ses/ path");
    assert_eq!(
        results[0].relevance_score, 3.0,
        "TAG_CONTEXT_MATCH_SCORE = 3"
    );
}

#[test]
fn test_tag_pto_matches_date_path() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "PTO balance fix", vec!["pto-accrual"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/date/pto_calculator.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1, "pto tag should match date/ path");
    assert_eq!(
        results[0].relevance_score, 3.0,
        "TAG_CONTEXT_MATCH_SCORE = 3"
    );
}

#[test]
fn test_tag_embedding_matches_kb_path() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Embedding indexing note", vec!["embedding-routing"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/kb/embedding_store.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1, "embedding tag should match kb/ path");
    assert_eq!(
        results[0].relevance_score, 3.0,
        "TAG_CONTEXT_MATCH_SCORE = 3"
    );
}

#[test]
fn test_tag_consumer_matches_consumer_path() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Consumer handler note", vec!["consumer-routing"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/consumer/handler.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1, "consumer tag should match consumer/ path");
    assert_eq!(
        results[0].relevance_score, 3.0,
        "TAG_CONTEXT_MATCH_SCORE = 3"
    );
}

#[test]
fn test_excluded_tag_python_patterns_scores_zero() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Python pattern note", vec!["python-patterns"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "excluded tag 'python-patterns' must not trigger scoring"
    );
}

#[test]
fn test_excluded_tag_architecture_patterns_scores_zero() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Architecture insight", vec!["architecture-patterns"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "excluded tag 'architecture-patterns' must not trigger scoring"
    );
}

#[test]
fn test_excluded_tag_database_sql_scores_zero() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "SQL pattern", vec!["database-sql"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "excluded tag 'database-sql' must not trigger scoring"
    );
}

#[test]
fn test_excluded_tag_testing_patterns_scores_zero() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "Testing insight", vec!["testing-patterns"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "excluded tag 'testing-patterns' must not trigger scoring"
    );
}

#[test]
fn test_excluded_tag_general_scores_zero() {
    let (_dir, conn) = setup_db();
    create_tagged_learning(&conn, "General note", vec!["general"]);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "excluded tag 'general' must not trigger scoring"
    );
}

#[test]
fn test_multiple_matching_tags_counted_once_not_double_scored() {
    // A learning with two tags that both map to workflow/ should score 3 once, not 6.
    let (_dir, conn) = setup_db();
    // Both "workflow-detour-phase3" and "workflow-engine" have "workflow" token → "workflow/"
    create_tagged_learning(
        &conn,
        "Double workflow learning",
        vec!["workflow-detour-phase3", "workflow-engine-fix"],
    );

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1, "learning should be found exactly once");
    assert_eq!(
        results[0].relevance_score, 3.0,
        "Multiple matching tags must not double-score: expected 3 (once), got {}",
        results[0].relevance_score
    );
}

#[test]
fn test_combined_scoring_file_type_tag_error_sums_to_20() {
    // Combined: FILE_MATCH_SCORE(10) + TYPE_MATCH_SCORE(5) + TAG_CONTEXT_MATCH_SCORE(3) + ERROR_MATCH_SCORE(2) = 20
    let (_dir, conn) = setup_db();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "Full-score learning".to_string(),
        content: "serde_json parse error at root".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["**/workflow/**".to_string()]),
        applies_to_task_types: Some(vec!["FEAT-".to_string()]),
        applies_to_errors: Some(vec!["serde_json".to_string()]),
        tags: Some(vec!["workflow-detour-phase3".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        task_prefix: Some("FEAT-003".to_string()),
        task_error: Some("serde_json error".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].relevance_score, 20.0,
        "file(10) + type(5) + tag(3) + error(2) = 20, got {}",
        results[0].relevance_score
    );
}

#[test]
fn test_tag_match_visible_through_composite_backend() {
    // Tag-aware retrieval through CompositeBackend: tag match produces a result
    // that survives UCB reranking and deduplication.
    let (_dir, conn) = setup_db_with_fts5();
    // Tagged learning with no applies_to_* metadata — only visible via tag scoring
    create_tagged_learning(&conn, "Tag-only composite test", vec!["workflow-detour"]);

    let backend = CompositeBackend::default_backends();
    let query = RetrievalQuery {
        task_files: vec!["service/src/agent/workflow/engine.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(
        results.len(),
        1,
        "tag-scored learning must survive composite backend"
    );
    assert_eq!(
        results[0].learning.title, "Tag-only composite test",
        "should be the tag-scored learning"
    );
    assert!(
        results[0].relevance_score >= 3.0,
        "tag score should be at least 3.0 (TAG_CONTEXT_MATCH_SCORE)"
    );
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
    assert_eq!(extract_task_prefix("f424ade5-PA-FEAT-003"), "PA-FEAT-003");
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
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    // Intentionally no migrations — verifying FTS5 is absent before migration
    assert!(!is_fts5_available(&conn));
}

#[test]
fn test_is_fts5_available_after_migration() {
    let (_temp_dir, conn) = setup_db_with_fts5();
    assert!(is_fts5_available(&conn));
}

// ========== FTS5 Tag Search Tests (v8/B4/FR-007) ==========
//
// These tests are #[ignore] — they define the contract for the FTS5 tag indexing
// implementation in migration v8. They verify that the Fts5Backend can find
// learnings by tag content after the v8 migration.
//
// FTS5 tokenization: the ascii tokenizer splits on '-', so tag
// `chrono-date-handling` yields tokens `chrono`, `date`, `handling`.
// Searching for `chrono` or `workflow` matches via tags_text column.

#[test]
fn test_fts5_backend_searches_by_tag() {
    // Happy path: FTS5 search for 'chrono' finds learning tagged 'chrono-date-handling'
    // even when title and content don't contain 'chrono'
    let (_dir, conn) = setup_db_with_fts5();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Temporal handling note".to_string(),
        content: "Time zone offsets behave unexpectedly".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["chrono-date-handling".to_string()]),
        confidence: crate::models::Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("chrono".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(
        results.len(),
        1,
        "Fts5Backend must find learning tagged 'chrono-date-handling' when searching 'chrono'"
    );
    assert_eq!(results[0].learning.title, "Temporal handling note");
}

#[test]
fn test_fts5_backend_tag_search_hyphenated_token_workflow() {
    // Happy path: FTS5 tokenizes 'pto-workflow-ux-fixes-v2' → tokens include 'workflow'.
    // Searching 'workflow' returns this learning even though title/content lack the word.
    let (_dir, conn) = setup_db_with_fts5();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Sprint deviation note".to_string(),
        content: "Detour taken during planning session".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["pto-workflow-ux-fixes-v2".to_string()]),
        confidence: crate::models::Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // Control: unrelated learning with no 'workflow' anywhere
    create_test_learning(
        &conn,
        "Unrelated observation",
        "Something completely different",
        LearningOutcome::Pattern,
    );

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("workflow".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    // Known-bad discriminator: exactly 1 result (the tagged one), not the control
    assert_eq!(
        results.len(),
        1,
        "FTS5 must return exactly the learning tagged 'pto-workflow-ux-fixes-v2', not the control"
    );
    assert_eq!(results[0].learning.title, "Sprint deviation note");
}

#[test]
fn test_fts5_backend_pto_token_finds_hyphenated_tag() {
    // AC6: FTS5 search for 'pto' finds learning tagged 'pto-workflow-ux-fixes-v2'.
    // Ascii tokenizer splits hyphens: 'pto-workflow-ux-fixes-v2' → 'pto', 'workflow', ...
    let (_dir, conn) = setup_db_with_fts5();

    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Leave balance adjustment".to_string(),
        content: "Accrual calculation was off by one day".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["pto-workflow-ux-fixes-v2".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // Control: unrelated learning (must not contain 'pto' anywhere)
    create_test_learning(
        &conn,
        "Unrelated note",
        "Nothing relevant in this one",
        LearningOutcome::Pattern,
    );

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("pto".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert_eq!(
        results.len(),
        1,
        "FTS5 search for 'pto' must find learning tagged 'pto-workflow-ux-fixes-v2'"
    );
    assert_eq!(results[0].learning.title, "Leave balance adjustment");
}

// ========== TEST-INIT-001: retired_at Filtering Tests ==========
//
// These tests verify that retired learnings (retired_at IS NOT NULL) are excluded
// from all retrieval paths. All tests are #[ignore] until:
//   FEAT-001: adds `retired_at` column via migration
//   FEAT-002: adds `retired_at IS NULL` filter to all 14 query locations
//
// Query locations covered in this file:
//   1. FTS5 text search (Fts5Backend, FTS5 index path)
//   2. LIKE fallback search (Fts5Backend, no-FTS5 path)
//   3. Unfiltered recency query (Fts5Backend, no-text + no-task-context path)
//   4. Pattern-matching retrieval (PatternsBackend)
//   5. UCB candidate fallback — CompositeBackend (both recency and with-task variants)
//   Discriminator: confirms naive query without filter WOULD include retired learning.

use crate::learnings::test_helpers::retire_learning;

#[test]
fn test_retired_excluded_from_fts5_search() {
    // AC: retired learning excluded from FTS5 search
    let (_dir, conn) = setup_db_with_fts5();
    let id = create_test_learning(
        &conn,
        "FTS5 retired target",
        "unique searchable content xyz",
        LearningOutcome::Success,
    );
    retire_learning(&conn, id);
    // Active learning to confirm backend still returns results
    create_test_learning(
        &conn,
        "Active learning",
        "active content",
        LearningOutcome::Pattern,
    );

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("searchable".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.iter().all(|r| r.learning.id != Some(id)),
        "retired learning must be excluded from FTS5 text search results"
    );
}

#[test]
fn test_retired_excluded_from_like_fallback() {
    // AC: retired learning excluded from LIKE fallback search (no FTS5 table)
    let (_dir, conn) = setup_db(); // No migrations → no FTS5 → uses LIKE fallback
    let id = create_test_learning(
        &conn,
        "LIKE retired target",
        "unique fallback content xyz",
        LearningOutcome::Success,
    );
    retire_learning(&conn, id);

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("fallback".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.iter().all(|r| r.learning.id != Some(id)),
        "retired learning must be excluded from LIKE fallback search results"
    );
}

#[test]
fn test_retired_excluded_from_recency_query() {
    // AC: retired learning excluded from unfiltered recency query (no text, no task context)
    let (_dir, conn) = setup_db_with_fts5();
    let id = create_test_learning(
        &conn,
        "Retired recent learning",
        "content",
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);
    // Active learning to confirm query still runs
    create_test_learning(&conn, "Active recency", "content", LearningOutcome::Pattern);

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.iter().all(|r| r.learning.id != Some(id)),
        "retired learning must be excluded from unfiltered recency query"
    );
    assert_eq!(results.len(), 1, "only the active learning should appear");
}

#[test]
fn test_retired_excluded_from_pattern_matching() {
    // AC: retired learning excluded from PatternsBackend pattern-matching retrieval
    let (_dir, conn) = setup_db();
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Retired file-matched pattern".to_string(),
        content: "Should not appear in pattern results".to_string(),
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
    let result = record_learning(&conn, params).unwrap();
    retire_learning(&conn, result.learning_id);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["src/db/schema.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "retired learning must be excluded from pattern-matching retrieval"
    );
}

#[test]
fn test_retired_excluded_from_ucb_fallback_no_task_context() {
    // AC: retired learning excluded from UCB fallback (no-task-context recency variant)
    let (_dir, conn) = setup_db_with_fts5();
    let id = create_test_learning(
        &conn,
        "Retired UCB no-task candidate",
        "content",
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);
    create_test_learning(
        &conn,
        "Active UCB candidate",
        "content",
        LearningOutcome::Pattern,
    );

    let backend = CompositeBackend::default_backends();
    let query = RetrievalQuery {
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.iter().all(|r| r.learning.id != Some(id)),
        "retired learning must not appear via UCB fallback (no-task-context variant)"
    );
    assert_eq!(results.len(), 1, "only the active learning should appear");
}

#[test]
fn test_retired_excluded_from_ucb_fallback_with_task_context() {
    // AC: retired learning excluded from UCB fallback (with-task-context exploration variant)
    // Even when task context is provided, retired learnings must not be UCB candidates.
    let (_dir, conn) = setup_db_with_fts5();
    let id = create_test_learning(
        &conn,
        "Retired UCB task candidate",
        "content",
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);

    let backend = CompositeBackend::default_backends();
    // Provide task_files so CompositeBackend uses the with-task UCB path
    let query = RetrievalQuery {
        task_files: vec!["src/main.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.iter().all(|r| r.learning.id != Some(id)),
        "retired learning must not appear via UCB fallback (with-task-context variant)"
    );
}

/// Known-bad discriminator: confirms that WITHOUT `retired_at IS NULL` filtering,
/// a retired learning IS present in the database and WOULD be returned.
/// This test verifies the pre-FEAT-002 baseline — it passes before implementation
/// and should be removed/updated after FEAT-002 adds the filters.
#[test]
fn test_discriminator_naive_query_includes_retired() {
    let (_dir, conn) = setup_db_with_fts5();
    let id = create_test_learning(
        &conn,
        "Discriminator retired target",
        "content",
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);

    // Direct SQL without `retired_at IS NULL` — naive implementation, no filter
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "without filtering, retired learning IS still in the learnings table"
    );
}

/// AC: FTS5 index still contains retired learning content after retirement
/// (retire does NOT delete from learnings_fts), but retrieval query excludes it.
/// This verifies that the `retired_at` column approach (vs. deleting from FTS index)
/// correctly filters at query time without corrupting the FTS index.
#[test]
fn test_fts5_index_retains_content_after_retire_but_query_excludes_it() {
    let (_dir, conn) = setup_db_with_fts5();
    let id = create_test_learning(
        &conn,
        "FTS5 index retention test",
        "unique retire indexing content qzxw",
        LearningOutcome::Pattern,
    );
    retire_learning(&conn, id);

    // FTS5 index still contains the content (no trigger removes it on retire)
    let fts_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings_fts WHERE learnings_fts MATCH '\"qzxw\"'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    assert_eq!(
        fts_count, 1,
        "FTS5 index must still contain retired learning content (retire does not delete from index)"
    );

    // But retrieval query excludes the retired learning
    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("qzxw".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();
    assert!(
        results.iter().all(|r| r.learning.id != Some(id)),
        "retired learning must be excluded from FTS5 retrieval even though it is still in the index"
    );
}

#[test]
fn test_fts5_backend_tag_search_no_false_positives() {
    // Discriminator: searching a rare token that only appears in tags must not
    // return learnings whose tags don't contain it.
    let (_dir, conn) = setup_db_with_fts5();

    // Learning tagged with something unrelated to 'chrono'
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Database note".to_string(),
        content: "SQLite connection pooling".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: Some(vec!["database-sql".to_string()]),
        confidence: crate::models::Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("chrono".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    assert!(
        results.is_empty(),
        "FTS5 must not return a learning tagged 'database-sql' when searching 'chrono'"
    );
}

// ========== TEST-INIT-002: Supersession filter tests ==========
//
// Tests the contract that recall (via backends) must exclude superseded learnings
// by default, and include them only when `include_superseded: true`.
//
// Structural tests (supersession row insertable, table accessible) run now against
// the v17 schema. Behavioral tests are `#[ignore]`d until FEAT-005 adds the
// `include_superseded: bool` field to `RetrievalQuery` and wires the NOT IN filter
// into FTS5 / Patterns / Vector backends.
//
// The behavioral tests deliberately use only currently-available API surfaces so
// the file compiles today; when FEAT-005 lands, each test's body will need the
// one-line change documented in its comment (set `include_superseded: true` where
// noted) and the `#[ignore]` removed.

/// Inserts a supersession row linking `old_id` → `new_id` via direct SQL. Used by
/// the supersession filter tests; FEAT-004 will add a higher-level CRUD helper.
fn insert_supersession(conn: &Connection, old_id: i64, new_id: i64) {
    conn.execute(
        "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (?1, ?2)",
        [old_id, new_id],
    )
    .unwrap();
}

/// Structural test that the v17 table is reachable from retrieval tests.
/// Always-active guard — fails loudly if the migration is rolled back or renamed.
#[test]
fn test_supersession_row_is_insertable_after_migrations() {
    let (_temp_dir, conn) = setup_db_with_fts5();
    let old_id = create_test_learning(&conn, "Old", "content", LearningOutcome::Pattern);
    let new_id = create_test_learning(&conn, "New", "content", LearningOutcome::Pattern);

    insert_supersession(&conn, old_id, new_id);

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learning_supersessions WHERE old_learning_id = ?1 AND new_learning_id = ?2",
            [old_id, new_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

/// AC: superseded learning excluded from recall results (default behavior).
///
/// Uses FTS5 backend with a text query that matches the old title. With the
/// default `include_superseded: false`, `execute_fts5_query` filters out the
/// superseded learning via the `NOT IN (SELECT old_learning_id ...)` clause.
#[test]
fn test_supersession_excluded_from_fts5_recall_by_default() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    let old_id = create_test_learning(
        &conn,
        "Old pattern",
        "unique-supersede-marker content",
        LearningOutcome::Pattern,
    );
    let new_id = create_test_learning(
        &conn,
        "New pattern",
        "unique-supersede-marker content",
        LearningOutcome::Pattern,
    );
    insert_supersession(&conn, old_id, new_id);

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("unique-supersede-marker".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    let ids: Vec<Option<i64>> = results.iter().map(|r| r.learning.id).collect();
    assert!(
        !ids.contains(&Some(old_id)),
        "superseded learning (id={old_id}) must NOT appear in default recall results; got {ids:?}"
    );
    assert!(
        ids.contains(&Some(new_id)),
        "superseding learning (id={new_id}) MUST appear in recall results; got {ids:?}"
    );
}

/// AC: superseded learning excluded from patterns backend recall (file match path).
#[test]
fn test_supersession_excluded_from_patterns_recall_by_default() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    let old_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Old DB pattern".to_string(),
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
    let old_id = record_learning(&conn, old_params).unwrap().learning_id;

    let new_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "New DB pattern".to_string(),
        content: "Use savepoints".to_string(),
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
    let new_id = record_learning(&conn, new_params).unwrap().learning_id;
    insert_supersession(&conn, old_id, new_id);

    let backend = PatternsBackend;
    let query = RetrievalQuery {
        task_files: vec!["src/db/schema.rs".to_string()],
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    let ids: Vec<Option<i64>> = results.iter().map(|r| r.learning.id).collect();
    assert!(
        !ids.contains(&Some(old_id)),
        "superseded learning (id={old_id}) must NOT appear in patterns recall; got {ids:?}"
    );
    assert!(
        ids.contains(&Some(new_id)),
        "superseding learning (id={new_id}) MUST appear in patterns recall; got {ids:?}"
    );
}

/// AC: `--include-superseded` flag includes superseded learning in results.
///
/// With `include_superseded: true`, backends skip the NOT IN (supersessions)
/// filter and return all learnings regardless of supersession status.
#[test]
fn test_include_superseded_flag_returns_superseded_learning() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    let old_id = create_test_learning(
        &conn,
        "Old pattern",
        "include-flag-marker content",
        LearningOutcome::Pattern,
    );
    let new_id = create_test_learning(
        &conn,
        "New pattern",
        "include-flag-marker content",
        LearningOutcome::Pattern,
    );
    insert_supersession(&conn, old_id, new_id);

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("include-flag-marker".to_string()),
        limit: 10,
        include_superseded: true,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    let ids: Vec<Option<i64>> = results.iter().map(|r| r.learning.id).collect();
    assert!(
        ids.contains(&Some(old_id)),
        "with include_superseded=true, superseded learning (id={old_id}) MUST appear; got {ids:?}"
    );
    assert!(
        ids.contains(&Some(new_id)),
        "with include_superseded=true, superseding learning (id={new_id}) MUST appear; got {ids:?}"
    );
}

/// AC: transitive supersession (A->B->C) correctly filters both A and B.
///
/// A chain A→B→C means B supersedes A, and C supersedes B. Default recall
/// must exclude BOTH A and B, returning only C. This guards against a naive
/// single-hop filter that only hides the most-recently-superseded learning.
#[test]
fn test_supersession_filter_is_transitive_over_chain() {
    let (_temp_dir, conn) = setup_db_with_fts5();

    let a_id = create_test_learning(
        &conn,
        "A",
        "transitive-chain body",
        LearningOutcome::Pattern,
    );
    let b_id = create_test_learning(
        &conn,
        "B",
        "transitive-chain body",
        LearningOutcome::Pattern,
    );
    let c_id = create_test_learning(
        &conn,
        "C",
        "transitive-chain body",
        LearningOutcome::Pattern,
    );
    insert_supersession(&conn, a_id, b_id); // B supersedes A
    insert_supersession(&conn, b_id, c_id); // C supersedes B

    let backend = Fts5Backend;
    let query = RetrievalQuery {
        text: Some("transitive-chain".to_string()),
        limit: 10,
        ..Default::default()
    };
    let results = backend.retrieve(&conn, &query).unwrap();

    let ids: Vec<Option<i64>> = results.iter().map(|r| r.learning.id).collect();
    assert!(
        !ids.contains(&Some(a_id)),
        "A (id={a_id}) is superseded by B — must be excluded; got {ids:?}"
    );
    assert!(
        !ids.contains(&Some(b_id)),
        "B (id={b_id}) is superseded by C — must be excluded; got {ids:?}"
    );
    assert!(
        ids.contains(&Some(c_id)),
        "C (id={c_id}) is the terminal supersession — must appear; got {ids:?}"
    );
}
