//! Tests for retrieval backends.

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, open_connection};
use crate::learnings::crud::{record_learning, RecordLearningParams};
use crate::models::{Confidence, LearningOutcome};

use super::fts5::{escape_fts5_query, is_fts5_available};
use super::patterns::{extract_task_prefix, file_matches_pattern};
use super::{CompositeBackend, Fts5Backend, PatternsBackend, RetrievalBackend, RetrievalQuery};

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
    let (_temp_dir, conn) = setup_db();
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
#[ignore = "pending v8 FTS5 tag indexing implementation (B4/FR-007)"]
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
#[ignore = "pending v8 FTS5 tag indexing implementation (B4/FR-007)"]
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
#[ignore = "pending v8 FTS5 tag indexing implementation (B4/FR-007)"]
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
