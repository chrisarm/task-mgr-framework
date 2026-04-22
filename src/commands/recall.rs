//! Recall command implementation.
//!
//! Provides CLI entry point for querying learnings from the institutional memory system.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::TaskMgrResult;
use crate::cli::LearningOutcome as CliOutcome;
use crate::learnings::embeddings::{DEFAULT_EMBEDDING_MODEL, DEFAULT_OLLAMA_URL};
use crate::learnings::{
    CompositeBackend, RecallParams as LibRecallParams, ScoredRecallResult, recall_learnings_scored,
};
use crate::models::LearningOutcome;

/// Parameters for the recall command from CLI.
#[derive(Debug, Clone, Default)]
pub struct RecallCmdParams {
    /// Free-text search query (LIKE matching on title and content)
    pub query: Option<String>,
    /// Task ID to find learnings matching the task's files and type
    pub for_task: Option<String>,
    /// Filter by tags (learning must have at least one of these tags)
    pub tags: Option<Vec<String>>,
    /// Filter by outcome type (CLI enum)
    pub outcome: Option<CliOutcome>,
    /// Maximum number of results to return
    pub limit: usize,
    /// Ollama server URL from config.json (None = default)
    pub ollama_url: Option<String>,
    /// Embedding model from config.json (None = default)
    pub embedding_model: Option<String>,
    /// When `true`, include superseded learnings in results (default: exclude them).
    pub include_superseded: bool,
}

/// Result of the recall command (wrapper for serialization).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallCmdResult {
    /// Number of learnings returned
    pub count: usize,
    /// The matching learnings
    pub learnings: Vec<LearningSummary>,
    /// The query parameters used
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub for_task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags_filter: Option<Vec<String>>,
}

/// Summary of a learning for recall output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LearningSummary {
    /// Learning ID
    pub id: Option<i64>,
    /// Title of the learning
    pub title: String,
    /// Outcome type
    pub outcome: String,
    /// Confidence level
    pub confidence: String,
    /// Content (may be truncated in text format)
    pub content: String,
    /// File patterns this learning applies to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applies_to_files: Option<Vec<String>>,
    /// Task type prefixes this learning applies to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applies_to_task_types: Option<Vec<String>>,
    /// Times this learning has been shown
    pub times_shown: i32,
    /// Times this learning has been applied
    pub times_applied: i32,
    /// Backend relevance score (FTS5 BM25, pattern points, or vector cosine)
    #[serde(default)]
    pub relevance_score: f64,
    /// UCB bandit score — Some only for `--for-task` recall
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ucb_score: Option<f64>,
    /// Final ranking score: `relevance_score * 100.0 + ucb_score` for task recall;
    /// equal to `relevance_score` when no UCB applies.
    #[serde(default)]
    pub combined_score: f64,
    /// Human-readable explanation of why this learning matched
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_reason: Option<String>,
}

/// Converts CLI outcome enum to model outcome enum.
fn cli_outcome_to_model(outcome: CliOutcome) -> LearningOutcome {
    match outcome {
        CliOutcome::Failure => LearningOutcome::Failure,
        CliOutcome::Success => LearningOutcome::Success,
        CliOutcome::Workaround => LearningOutcome::Workaround,
        CliOutcome::Pattern => LearningOutcome::Pattern,
    }
}

/// Executes the recall command from CLI parameters.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `params` - Recall command parameters from CLI
///
/// # Returns
///
/// Result containing the matching learnings.
pub fn recall(conn: &Connection, params: RecallCmdParams) -> TaskMgrResult<RecallCmdResult> {
    // Convert CLI params to library params
    let lib_params = LibRecallParams {
        query: params.query.clone(),
        for_task: params.for_task.clone(),
        tags: params.tags.clone(),
        outcome: params.outcome.map(cli_outcome_to_model),
        limit: params.limit,
        include_superseded: params.include_superseded,
    };

    // Build composite backend with config-aware VectorBackend
    let ollama_url = params.ollama_url.as_deref().unwrap_or(DEFAULT_OLLAMA_URL);
    let model = params
        .embedding_model
        .as_deref()
        .unwrap_or(DEFAULT_EMBEDDING_MODEL);

    let backend = CompositeBackend::with_ollama_config(ollama_url, model);

    let result = recall_learnings_scored(conn, lib_params, &backend)?;

    // Convert to command result
    Ok(RecallCmdResult::from_scored_result(result, &params))
}

impl RecallCmdResult {
    fn from_scored_result(result: ScoredRecallResult, params: &RecallCmdParams) -> Self {
        let learnings = result
            .scored_learnings
            .into_iter()
            .map(|s| LearningSummary {
                id: s.learning.id,
                title: s.learning.title,
                outcome: s.learning.outcome.to_string(),
                confidence: s.learning.confidence.to_string(),
                content: s.learning.content,
                applies_to_files: s.learning.applies_to_files,
                applies_to_task_types: s.learning.applies_to_task_types,
                times_shown: s.learning.times_shown,
                times_applied: s.learning.times_applied,
                relevance_score: s.relevance_score,
                ucb_score: s.ucb_score,
                combined_score: s.combined_score,
                match_reason: s.match_reason,
            })
            .collect();

        RecallCmdResult {
            count: result.count,
            learnings,
            query: params.query.clone(),
            for_task: params.for_task.clone(),
            outcome_filter: params.outcome.map(|o| format!("{:?}", o).to_lowercase()),
            tags_filter: params.tags.clone(),
        }
    }
}

/// Formats the recall result for text output.
#[must_use]
pub fn format_text(result: &RecallCmdResult) -> String {
    let mut output = String::new();

    if result.learnings.is_empty() {
        output.push_str("No matching learnings found.\n");
        return output;
    }

    output.push_str(&format!("Found {} learning(s):\n\n", result.count));

    for (i, learning) in result.learnings.iter().enumerate() {
        output.push_str(&format!(
            "{}. [{}] {} ({})\n",
            i + 1,
            learning.id.map(|id| id.to_string()).unwrap_or_default(),
            learning.title,
            learning.outcome
        ));

        // Show confidence and (when non-zero) scores. A pure recency recall
        // with no query / no for_task produces zero scores across the board;
        // printing "Score: 0.00 | 0.00" there is noise, so we skip it.
        output.push_str(&format!("   Confidence: {}\n", learning.confidence));
        let has_score = learning.relevance_score != 0.0
            || learning.combined_score != 0.0
            || learning.ucb_score.is_some();
        if has_score {
            output.push_str(&format!(
                "   Score: {:.2} (relevance) | {:.2} (combined)\n",
                learning.relevance_score, learning.combined_score
            ));
        }

        // Show content (truncated)
        let content_preview = super::truncate_str(&learning.content, 100);
        output.push_str(&format!("   {}\n", content_preview));

        // Show applicability
        if let Some(ref files) = learning.applies_to_files {
            output.push_str(&format!("   Files: {}\n", files.join(", ")));
        }
        if let Some(ref types) = learning.applies_to_task_types {
            output.push_str(&format!("   Task types: {}\n", types.join(", ")));
        }

        // Show stats
        output.push_str(&format!(
            "   Stats: {} shown, {} applied\n",
            learning.times_shown, learning.times_applied
        ));

        output.push('\n');
    }

    output
}

/// Formats verbose output for the recall command (to stderr).
///
/// Returns a string that should be written to stderr when --verbose is enabled.
#[must_use]
pub fn format_verbose(result: &RecallCmdResult) -> String {
    let mut output = String::new();

    output.push_str("[verbose] Recall Query Parameters:\n");
    output.push_str(&format!("{}\n", "-".repeat(50)));

    if let Some(ref query) = result.query {
        output.push_str(&format!("  Text query: \"{}\"\n", query));
    }
    if let Some(ref task) = result.for_task {
        output.push_str(&format!("  For task: {}\n", task));
    }
    if let Some(ref outcome) = result.outcome_filter {
        output.push_str(&format!("  Outcome filter: {}\n", outcome));
    }
    if let Some(ref tags) = result.tags_filter {
        output.push_str(&format!("  Tags filter: {}\n", tags.join(", ")));
    }
    if result.query.is_none()
        && result.for_task.is_none()
        && result.outcome_filter.is_none()
        && result.tags_filter.is_none()
    {
        output.push_str("  (no filters, returning recent learnings)\n");
    }

    output.push_str(&format!("{}\n", "-".repeat(50)));

    // Show match details for each learning
    if !result.learnings.is_empty() {
        output.push_str("\n[verbose] Match Details:\n");
        for learning in &result.learnings {
            output.push_str(&format!(
                "  [{}] {} - matched by:\n",
                learning.id.unwrap_or(0),
                learning.title
            ));

            if let Some(ref reason) = learning.match_reason {
                output.push_str(&format!("    - {}\n", reason));
            } else {
                output.push_str("    - (no specific match reason)\n");
            }
        }
    }

    output.push_str(&format!(
        "\n[verbose] {} learning(s) returned\n",
        result.count
    ));

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, migrations::run_migrations, open_connection};
    use crate::learnings::{RecordLearningParams, record_learning};
    use crate::models::Confidence;
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    fn setup_db_with_migrations() -> (TempDir, Connection) {
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

        let params = RecallCmdParams::default();
        let result = recall(&conn, params).unwrap();

        assert_eq!(result.count, 0);
        assert!(result.learnings.is_empty());
    }

    #[test]
    fn test_recall_all_learnings() {
        let (_temp_dir, conn) = setup_db();

        create_test_learning(&conn, "Learning 1", "Content 1", LearningOutcome::Failure);
        create_test_learning(&conn, "Learning 2", "Content 2", LearningOutcome::Success);

        let params = RecallCmdParams {
            limit: 10,
            ..Default::default()
        };
        let result = recall(&conn, params).unwrap();

        assert_eq!(result.count, 2);
        assert_eq!(result.learnings.len(), 2);
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

        let params = RecallCmdParams {
            query: Some("database".to_string()),
            limit: 10,
            ..Default::default()
        };
        let result = recall(&conn, params).unwrap();

        assert_eq!(result.count, 1);
        assert_eq!(result.learnings[0].title, "Database error");
    }

    #[test]
    fn test_recall_with_outcome_filter() {
        let (_temp_dir, conn) = setup_db();

        create_test_learning(&conn, "Failure 1", "Content", LearningOutcome::Failure);
        create_test_learning(&conn, "Success 1", "Content", LearningOutcome::Success);
        create_test_learning(&conn, "Failure 2", "Content", LearningOutcome::Failure);

        let params = RecallCmdParams {
            outcome: Some(CliOutcome::Failure),
            limit: 10,
            ..Default::default()
        };
        let result = recall(&conn, params).unwrap();

        assert_eq!(result.count, 2);
        assert!(result.learnings.iter().all(|l| l.outcome == "failure"));
    }

    #[test]
    fn test_recall_with_tags_filter() {
        let (_temp_dir, conn) = setup_db();

        // Create learning with tags
        let params1 = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Rust pattern".to_string(),
            content: "Use Result".to_string(),
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

        // Filter by rust tag
        let params = RecallCmdParams {
            tags: Some(vec!["rust".to_string()]),
            limit: 10,
            ..Default::default()
        };
        let result = recall(&conn, params).unwrap();

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
                "Content",
                LearningOutcome::Pattern,
            );
        }

        let params = RecallCmdParams {
            limit: 3,
            ..Default::default()
        };
        let result = recall(&conn, params).unwrap();

        assert_eq!(result.count, 3);
        assert_eq!(result.learnings.len(), 3);
    }

    #[test]
    fn test_recall_with_for_task() {
        let (_temp_dir, conn) = setup_db_with_migrations();

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
        let recall_params = RecallCmdParams {
            for_task: Some("US-001".to_string()),
            limit: 10,
            ..Default::default()
        };
        let result = recall(&conn, recall_params).unwrap();

        // DB pattern matches via file, CLI pattern comes via UCB fallback
        assert_eq!(result.count, 2);
        // File-matched learning should be first (higher relevance tier)
        assert_eq!(result.learnings[0].title, "DB pattern");
    }

    #[test]
    fn test_format_text_empty() {
        let result = RecallCmdResult {
            count: 0,
            learnings: vec![],
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
        let result = RecallCmdResult {
            count: 1,
            learnings: vec![LearningSummary {
                id: Some(1),
                title: "Test failure".to_string(),
                outcome: "failure".to_string(),
                confidence: "medium".to_string(),
                content: "Detailed content here".to_string(),
                applies_to_files: None,
                applies_to_task_types: None,
                times_shown: 5,
                times_applied: 2,
                ..Default::default()
            }],
            query: None,
            for_task: None,
            outcome_filter: None,
            tags_filter: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Found 1 learning"));
        assert!(text.contains("Test failure"));
        assert!(text.contains("failure"));
        assert!(text.contains("Confidence: medium"));
        assert!(text.contains("Stats: 5 shown, 2 applied"));
    }

    #[test]
    fn test_format_text_truncates_long_content() {
        let long_content = "a".repeat(200);
        let result = RecallCmdResult {
            count: 1,
            learnings: vec![LearningSummary {
                id: Some(1),
                title: "Long content test".to_string(),
                outcome: "pattern".to_string(),
                confidence: "high".to_string(),
                content: long_content,
                applies_to_files: None,
                applies_to_task_types: None,
                times_shown: 0,
                times_applied: 0,
                ..Default::default()
            }],
            query: None,
            for_task: None,
            outcome_filter: None,
            tags_filter: None,
        };

        let text = format_text(&result);
        // Should have truncation indicator
        assert!(text.contains("..."));
        // Should not contain full 200 characters
        assert!(text.len() < 300);
    }

    #[test]
    fn test_format_text_with_applicability() {
        let result = RecallCmdResult {
            count: 1,
            learnings: vec![LearningSummary {
                id: Some(1),
                title: "Pattern with applicability".to_string(),
                outcome: "pattern".to_string(),
                confidence: "high".to_string(),
                content: "Content".to_string(),
                applies_to_files: Some(vec!["src/*.rs".to_string(), "tests/*.rs".to_string()]),
                applies_to_task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
                times_shown: 0,
                times_applied: 0,
                ..Default::default()
            }],
            query: None,
            for_task: None,
            outcome_filter: None,
            tags_filter: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Files: src/*.rs, tests/*.rs"));
        assert!(text.contains("Task types: US-, FIX-"));
    }

    #[test]
    fn test_cli_outcome_to_model() {
        assert_eq!(
            cli_outcome_to_model(CliOutcome::Failure),
            LearningOutcome::Failure
        );
        assert_eq!(
            cli_outcome_to_model(CliOutcome::Success),
            LearningOutcome::Success
        );
        assert_eq!(
            cli_outcome_to_model(CliOutcome::Workaround),
            LearningOutcome::Workaround
        );
        assert_eq!(
            cli_outcome_to_model(CliOutcome::Pattern),
            LearningOutcome::Pattern
        );
    }

    #[test]
    fn test_result_serialization() {
        let result = RecallCmdResult {
            count: 1,
            learnings: vec![LearningSummary {
                id: Some(42),
                title: "Test".to_string(),
                outcome: "failure".to_string(),
                confidence: "high".to_string(),
                content: "Content".to_string(),
                applies_to_files: None,
                applies_to_task_types: None,
                times_shown: 1,
                times_applied: 0,
                ..Default::default()
            }],
            query: Some("test".to_string()),
            for_task: None,
            outcome_filter: None,
            tags_filter: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"count\":1"));
        assert!(json.contains("\"title\":\"Test\""));
        assert!(json.contains("\"query\":\"test\""));
        // Optional None fields should not appear
        assert!(!json.contains("for_task"));
        assert!(!json.contains("outcome_filter"));
    }

    // ========== TEST-INIT-001: LearningSummary score fields ==========

    #[test]
    fn test_learning_summary_serializes_new_score_fields() {
        // AC5 (CLI layer): JSON output includes relevance_score + combined_score,
        // omits ucb_score/match_reason when None (skip_serializing_if).
        let summary = LearningSummary {
            id: Some(1),
            title: "scored".into(),
            outcome: "pattern".into(),
            confidence: "high".into(),
            content: "body".into(),
            times_shown: 0,
            times_applied: 0,
            relevance_score: 12.5,
            ucb_score: None,
            combined_score: 12.5,
            match_reason: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(
            json.contains("\"relevance_score\":12.5"),
            "expected relevance_score in JSON: {json}"
        );
        assert!(
            json.contains("\"combined_score\":12.5"),
            "expected combined_score in JSON: {json}"
        );
        assert!(
            !json.contains("ucb_score"),
            "None ucb_score must be skipped: {json}"
        );
        assert!(
            !json.contains("match_reason"),
            "None match_reason must be skipped: {json}"
        );
    }

    #[test]
    fn test_learning_summary_round_trip_with_ucb_and_reason() {
        // AC5: JSON round-trip preserves score fields when UCB + reason are present.
        let summary = LearningSummary {
            id: Some(7),
            title: "task-scored".into(),
            outcome: "pattern".into(),
            confidence: "medium".into(),
            content: "body".into(),
            times_shown: 2,
            times_applied: 1,
            relevance_score: 10.0,
            ucb_score: Some(0.35),
            combined_score: 1000.35,
            match_reason: Some("file match: src/db/*.rs".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&summary).unwrap();
        let parsed: LearningSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.relevance_score, 10.0);
        assert_eq!(parsed.ucb_score, Some(0.35));
        assert_eq!(parsed.combined_score, 1000.35);
        assert_eq!(
            parsed.match_reason.as_deref(),
            Some("file match: src/db/*.rs")
        );
    }

    #[test]
    fn test_learning_summary_deserializes_legacy_without_score_fields() {
        // Backward compatibility: a LearningSummary JSON emitted before the score fields
        // existed must still deserialize (serde(default) on new fields).
        let legacy = serde_json::json!({
            "id": 1,
            "title": "legacy",
            "outcome": "pattern",
            "confidence": "high",
            "content": "body",
            "times_shown": 0,
            "times_applied": 0
        });
        let parsed: LearningSummary = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.relevance_score, 0.0);
        assert_eq!(parsed.ucb_score, None);
        assert_eq!(parsed.combined_score, 0.0);
        assert_eq!(parsed.match_reason, None);
    }

    // ========== FEAT-002: score line in format_text ==========

    #[test]
    fn test_format_text_shows_score_line() {
        let result = RecallCmdResult {
            count: 1,
            learnings: vec![LearningSummary {
                id: Some(1),
                title: "Scored learning".to_string(),
                outcome: "pattern".to_string(),
                confidence: "high".to_string(),
                content: "Content".to_string(),
                times_shown: 0,
                times_applied: 0,
                relevance_score: 8.75,
                combined_score: 875.42,
                ucb_score: Some(0.42),
                match_reason: Some("file match: src/*.rs".into()),
                ..Default::default()
            }],
            query: None,
            for_task: None,
            outcome_filter: None,
            tags_filter: None,
        };

        let text = format_text(&result);
        assert!(
            text.contains("Score: 8.75 (relevance) | 875.42 (combined)"),
            "expected score line in format_text output: {text}"
        );
        // Score line comes after confidence line
        let conf_pos = text.find("Confidence:").unwrap();
        let score_pos = text.find("Score:").unwrap();
        assert!(
            score_pos > conf_pos,
            "Score line should appear after Confidence line"
        );
    }

    #[test]
    fn test_format_text_score_line_omitted_when_all_zero() {
        // A recall with no query and no for_task (pure recency) yields zero
        // relevance/combined scores and no ucb_score. Suppressing the Score:
        // line in that case keeps the output focused on the learning content.
        let result = RecallCmdResult {
            count: 1,
            learnings: vec![LearningSummary {
                id: Some(2),
                title: "Zero score".to_string(),
                outcome: "failure".to_string(),
                confidence: "low".to_string(),
                content: "Content".to_string(),
                times_shown: 0,
                times_applied: 0,
                ..Default::default()
            }],
            query: None,
            for_task: None,
            outcome_filter: None,
            tags_filter: None,
        };

        let text = format_text(&result);
        assert!(
            !text.contains("Score:"),
            "all-zero scores should be omitted from text output: {text}"
        );
    }

    // ========== FEAT-002: match_reason in format_verbose ==========

    #[test]
    fn test_format_verbose_uses_match_reason_from_backend() {
        let result = RecallCmdResult {
            count: 1,
            learnings: vec![LearningSummary {
                id: Some(10),
                title: "Pattern learning".to_string(),
                outcome: "pattern".to_string(),
                confidence: "high".to_string(),
                content: "Content".to_string(),
                times_shown: 0,
                times_applied: 0,
                match_reason: Some("file match: src/db/*.rs, task type: FEAT-".into()),
                ..Default::default()
            }],
            query: Some("database".to_string()),
            for_task: None,
            outcome_filter: None,
            tags_filter: None,
        };

        let verbose = format_verbose(&result);
        assert!(
            verbose.contains("file match: src/db/*.rs, task type: FEAT-"),
            "format_verbose should use match_reason from backend: {verbose}"
        );
        // Should NOT contain the old hand-reconstructed reason
        assert!(
            !verbose.contains("title contains query"),
            "format_verbose must not reconstruct reasons from title: {verbose}"
        );
    }

    #[test]
    fn test_format_verbose_no_match_reason_shows_fallback() {
        let result = RecallCmdResult {
            count: 1,
            learnings: vec![LearningSummary {
                id: Some(11),
                title: "No reason".to_string(),
                outcome: "pattern".to_string(),
                confidence: "medium".to_string(),
                content: "Content".to_string(),
                times_shown: 0,
                times_applied: 0,
                match_reason: None,
                ..Default::default()
            }],
            query: None,
            for_task: None,
            outcome_filter: None,
            tags_filter: None,
        };

        let verbose = format_verbose(&result);
        assert!(
            verbose.contains("(no specific match reason)"),
            "format_verbose should show fallback text when match_reason is None: {verbose}"
        );
    }

    // ========== Integration tests: recall() end-to-end with real DB ==========

    fn insert_supersession(conn: &Connection, old_id: i64, new_id: i64) {
        conn.execute(
            "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (?1, ?2)",
            rusqlite::params![old_id, new_id],
        )
        .unwrap();
    }

    fn insert_task_with_file(conn: &Connection, task_id: &str, file_path: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES (?1, 'Test Task')",
            [task_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_files (task_id, file_path) VALUES (?1, ?2)",
            [task_id, file_path],
        )
        .unwrap();
    }

    fn create_file_matched_learning(conn: &Connection, title: &str, file_glob: &str) -> i64 {
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: title.to_string(),
            content: format!("Content for {title}"),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: Some(vec![file_glob.to_string()]),
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        };
        record_learning(conn, params).unwrap().learning_id
    }

    #[test]
    fn test_recall_for_task_ucb_score_populated() {
        // AC: recall() with for_task returns LearningSummary with ucb_score: Some(...)
        let (_temp_dir, conn) = setup_db_with_migrations();
        insert_task_with_file(&conn, "US-001", "src/db/schema.rs");
        create_file_matched_learning(&conn, "DB pattern", "src/db/*.rs");

        let result = recall(
            &conn,
            RecallCmdParams {
                for_task: Some("US-001".to_string()),
                limit: 5,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            !result.learnings.is_empty(),
            "expected results from for_task recall"
        );
        assert!(
            result.learnings.iter().any(|l| l.ucb_score.is_some()),
            "for_task recall must carry ucb_score: Some(...) on at least one learning"
        );
    }

    #[test]
    fn test_recall_free_text_ucb_score_none() {
        // AC: recall() with text query returns LearningSummary with ucb_score: None
        let (_temp_dir, conn) = setup_db_with_migrations();
        create_test_learning(
            &conn,
            "Database error handling",
            "SQLite crashed during migration",
            LearningOutcome::Failure,
        );

        let result = recall(
            &conn,
            RecallCmdParams {
                query: Some("database".to_string()),
                limit: 5,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            !result.learnings.is_empty(),
            "text query must return results"
        );
        assert!(
            result.learnings.iter().all(|l| l.ucb_score.is_none()),
            "free-text recall must return ucb_score: None on every learning; got {:?}",
            result
                .learnings
                .iter()
                .map(|l| l.ucb_score)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn test_recall_real_scores_nonzero_for_file_match() {
        // AC: recall() with real data returns relevance_score > 1.0 for a file-pattern match.
        let (_temp_dir, conn) = setup_db_with_migrations();
        insert_task_with_file(&conn, "US-001", "src/db/schema.rs");
        create_file_matched_learning(&conn, "DB transactions", "src/db/*.rs");

        let result = recall(
            &conn,
            RecallCmdParams {
                for_task: Some("US-001".to_string()),
                limit: 5,
                ..Default::default()
            },
        )
        .unwrap();

        let matched = result
            .learnings
            .iter()
            .find(|l| l.title == "DB transactions")
            .expect("file-matched learning must appear");

        assert!(
            matched.relevance_score > 1.0,
            "file-match must yield relevance_score > 1.0 (stub would give 0.0), got {}",
            matched.relevance_score
        );
        assert!(
            matched.combined_score > 1.0,
            "file-match must yield combined_score > 1.0, got {}",
            matched.combined_score
        );
    }

    #[test]
    fn test_recall_excludes_superseded_by_default() {
        // AC: recall() default (include_superseded: false) hides the superseded learning.
        let (_temp_dir, conn) = setup_db_with_migrations();
        let old_id = create_test_learning(
            &conn,
            "Old superseded pattern",
            "unique-supersede-marker body",
            LearningOutcome::Pattern,
        );
        let new_id = create_test_learning(
            &conn,
            "New superseding pattern",
            "unique-supersede-marker body",
            LearningOutcome::Pattern,
        );
        insert_supersession(&conn, old_id, new_id);

        let result = recall(
            &conn,
            RecallCmdParams {
                query: Some("unique-supersede-marker".to_string()),
                limit: 10,
                include_superseded: false,
                ..Default::default()
            },
        )
        .unwrap();

        let ids: Vec<Option<i64>> = result.learnings.iter().map(|l| l.id).collect();
        assert!(
            !ids.contains(&Some(old_id)),
            "superseded learning (id={old_id}) must be excluded by default; got {ids:?}"
        );
        assert!(
            ids.contains(&Some(new_id)),
            "superseding learning (id={new_id}) must appear in results; got {ids:?}"
        );
    }

    #[test]
    fn test_recall_include_superseded_flag_returns_both() {
        // AC: recall() with include_superseded: true includes the superseded learning.
        let (_temp_dir, conn) = setup_db_with_migrations();
        let old_id = create_test_learning(
            &conn,
            "Old pattern",
            "include-flag-marker body",
            LearningOutcome::Pattern,
        );
        let new_id = create_test_learning(
            &conn,
            "New pattern",
            "include-flag-marker body",
            LearningOutcome::Pattern,
        );
        insert_supersession(&conn, old_id, new_id);

        let result = recall(
            &conn,
            RecallCmdParams {
                query: Some("include-flag-marker".to_string()),
                limit: 10,
                include_superseded: true,
                ..Default::default()
            },
        )
        .unwrap();

        let ids: Vec<Option<i64>> = result.learnings.iter().map(|l| l.id).collect();
        assert!(
            ids.contains(&Some(old_id)),
            "with include_superseded=true, superseded learning (id={old_id}) must appear; got {ids:?}"
        );
        assert!(
            ids.contains(&Some(new_id)),
            "with include_superseded=true, superseding learning (id={new_id}) must appear; got {ids:?}"
        );
    }

    #[test]
    fn test_recall_json_score_fields_present_and_nonzero() {
        // AC: recall() JSON output contains relevance_score and combined_score with real values.
        let (_temp_dir, conn) = setup_db_with_migrations();
        insert_task_with_file(&conn, "FEAT-001", "src/commands/learn.rs");
        create_file_matched_learning(&conn, "Command pattern", "src/commands/*.rs");

        let result = recall(
            &conn,
            RecallCmdParams {
                for_task: Some("FEAT-001".to_string()),
                limit: 5,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(!result.learnings.is_empty());
        let json = serde_json::to_string(&result).unwrap();
        assert!(
            json.contains("relevance_score"),
            "JSON must include relevance_score: {json}"
        );
        assert!(
            json.contains("combined_score"),
            "JSON must include combined_score: {json}"
        );

        let matched = result
            .learnings
            .iter()
            .find(|l| l.title == "Command pattern")
            .expect("file-matched learning must be in results");
        assert!(
            matched.relevance_score > 0.0,
            "relevance_score must be non-zero for file-matched learning, got {}",
            matched.relevance_score
        );
        assert!(
            matched.combined_score > 0.0,
            "combined_score must be non-zero for file-matched learning, got {}",
            matched.combined_score
        );
    }

    #[test]
    fn test_recall_score_monotonicity_through_command() {
        // AC: scores returned by recall() are in non-increasing combined_score order.
        let (_temp_dir, conn) = setup_db_with_migrations();
        insert_task_with_file(&conn, "US-001", "src/db/schema.rs");

        // High-relevance: file match (relevance ~10)
        create_file_matched_learning(&conn, "File matched", "src/db/*.rs");
        // Low-relevance: no pattern match — arrives via UCB fallback (relevance ~0.1)
        create_test_learning(
            &conn,
            "Fallback one",
            "unrelated content A",
            LearningOutcome::Pattern,
        );
        create_test_learning(
            &conn,
            "Fallback two",
            "unrelated content B",
            LearningOutcome::Pattern,
        );

        let result = recall(
            &conn,
            RecallCmdParams {
                for_task: Some("US-001".to_string()),
                limit: 10,
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            result.learnings.len() >= 2,
            "expected multiple results for ordering check"
        );
        for pair in result.learnings.windows(2) {
            assert!(
                pair[0].combined_score >= pair[1].combined_score,
                "combined_score must be non-increasing: {} -> {}",
                pair[0].combined_score,
                pair[1].combined_score
            );
        }
        // File-matched learning must rank first
        assert_eq!(
            result.learnings[0].title, "File matched",
            "file-matched learning must rank first by combined_score"
        );
    }
}
