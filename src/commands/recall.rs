//! Recall command implementation.
//!
//! Provides CLI entry point for querying learnings from the institutional memory system.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::cli::LearningOutcome as CliOutcome;
use crate::learnings::embeddings::{DEFAULT_EMBEDDING_MODEL, DEFAULT_OLLAMA_URL};
use crate::learnings::{
    recall_learnings_with_backend, CompositeBackend, RecallParams as LibRecallParams, RecallResult,
};
use crate::models::LearningOutcome;
use crate::TaskMgrResult;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    };

    // Build composite backend with config-aware VectorBackend
    let ollama_url = params.ollama_url.as_deref().unwrap_or(DEFAULT_OLLAMA_URL);
    let model = params
        .embedding_model
        .as_deref()
        .unwrap_or(DEFAULT_EMBEDDING_MODEL);

    let backend = CompositeBackend::with_ollama_config(ollama_url, model);

    let result = recall_learnings_with_backend(conn, lib_params, &backend)?;

    // Convert to command result
    Ok(RecallCmdResult::from_recall_result(result, &params))
}

impl RecallCmdResult {
    fn from_recall_result(result: RecallResult, params: &RecallCmdParams) -> Self {
        let learnings = result
            .learnings
            .into_iter()
            .map(|l| LearningSummary {
                id: l.id,
                title: l.title,
                outcome: l.outcome.to_string(),
                confidence: l.confidence.to_string(),
                content: l.content,
                applies_to_files: l.applies_to_files,
                applies_to_task_types: l.applies_to_task_types,
                times_shown: l.times_shown,
                times_applied: l.times_applied,
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

        // Show confidence
        output.push_str(&format!("   Confidence: {}\n", learning.confidence));

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

            let mut match_reasons = Vec::new();

            // Check what criteria matched
            if let Some(ref query) = result.query {
                if learning
                    .title
                    .to_lowercase()
                    .contains(&query.to_lowercase())
                {
                    match_reasons.push("title contains query".to_string());
                } else if learning
                    .content
                    .to_lowercase()
                    .contains(&query.to_lowercase())
                {
                    match_reasons.push("content contains query".to_string());
                }
            }

            if let Some(ref outcome) = result.outcome_filter
                && learning.outcome == *outcome {
                    match_reasons.push(format!("outcome is {}", outcome));
                }

            if result.for_task.is_some() {
                if let Some(ref files) = learning.applies_to_files {
                    match_reasons.push(format!("file patterns: {}", files.join(", ")));
                }
                if let Some(ref types) = learning.applies_to_task_types {
                    match_reasons.push(format!("task types: {}", types.join(", ")));
                }
            }

            if match_reasons.is_empty() {
                match_reasons.push("recency ordering".to_string());
            }

            for reason in match_reasons {
                output.push_str(&format!("    - {}\n", reason));
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
    use crate::learnings::{record_learning, RecordLearningParams};
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
}
