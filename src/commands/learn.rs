//! Learn command implementation.
//!
//! Records learnings from task outcomes into the institutional memory system.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::cli::{Confidence as CliConfidence, LearningOutcome as CliOutcome};
use crate::learnings::{record_learning, RecordLearningParams, RecordLearningResult};
use crate::models::{Confidence, LearningOutcome};
use crate::TaskMgrResult;

/// Parameters for the learn command.
#[derive(Debug, Clone)]
pub struct LearnParams {
    /// Type of learning outcome
    pub outcome: CliOutcome,
    /// Short title for the learning
    pub title: String,
    /// Detailed content of the learning
    pub content: String,
    /// Task ID this learning is associated with
    pub task_id: Option<String>,
    /// Run ID this learning is associated with
    pub run_id: Option<String>,
    /// Root cause of the issue
    pub root_cause: Option<String>,
    /// Solution that was applied
    pub solution: Option<String>,
    /// File patterns this learning applies to
    pub files: Option<Vec<String>>,
    /// Task type prefixes this learning applies to
    pub task_types: Option<Vec<String>>,
    /// Error patterns this learning applies to
    pub errors: Option<Vec<String>>,
    /// Tags for categorization
    pub tags: Option<Vec<String>>,
    /// Confidence level for this learning
    pub confidence: CliConfidence,
}

/// Result of the learn command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnResult {
    /// Database ID of the created learning
    pub learning_id: i64,
    /// Title of the learning
    pub title: String,
    /// Outcome type
    pub outcome: String,
    /// Number of tags added
    pub tags_added: usize,
}

impl From<RecordLearningResult> for LearnResult {
    fn from(result: RecordLearningResult) -> Self {
        LearnResult {
            learning_id: result.learning_id,
            title: result.title,
            outcome: result.outcome.to_string(),
            tags_added: result.tags_added,
        }
    }
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

/// Converts CLI confidence enum to model confidence enum.
fn cli_confidence_to_model(confidence: CliConfidence) -> Confidence {
    match confidence {
        CliConfidence::High => Confidence::High,
        CliConfidence::Medium => Confidence::Medium,
        CliConfidence::Low => Confidence::Low,
    }
}

/// Records a learning from CLI parameters.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `params` - Learn command parameters from CLI
///
/// # Returns
///
/// Result containing the learning ID and metadata.
///
/// # Errors
///
/// Returns an error if:
/// - Database insert fails
/// - Task ID doesn't exist (foreign key violation)
/// - Run ID doesn't exist (foreign key violation)
pub fn learn(conn: &Connection, params: LearnParams) -> TaskMgrResult<LearnResult> {
    let record_params = RecordLearningParams {
        outcome: cli_outcome_to_model(params.outcome),
        title: params.title,
        content: params.content,
        task_id: params.task_id,
        run_id: params.run_id,
        root_cause: params.root_cause,
        solution: params.solution,
        applies_to_files: params.files,
        applies_to_task_types: params.task_types,
        applies_to_errors: params.errors,
        tags: params.tags,
        confidence: cli_confidence_to_model(params.confidence),
    };

    let result = record_learning(conn, record_params)?;
    Ok(LearnResult::from(result))
}

/// Formats the learn result for text output.
pub fn format_text(result: &LearnResult) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Learning recorded: {}", result.title));
    lines.push(format!("  ID: {}", result.learning_id));
    lines.push(format!("  Outcome: {}", result.outcome));
    if result.tags_added > 0 {
        lines.push(format!("  Tags added: {}", result.tags_added));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    #[test]
    fn test_learn_minimal() {
        let (_temp_dir, conn) = setup_db();

        let params = LearnParams {
            outcome: CliOutcome::Failure,
            title: "Test failure".to_string(),
            content: "Something went wrong".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, params).unwrap();

        assert!(result.learning_id > 0);
        assert_eq!(result.title, "Test failure");
        assert_eq!(result.outcome, "failure");
        assert_eq!(result.tags_added, 0);
    }

    #[test]
    fn test_learn_with_all_options() {
        let (_temp_dir, conn) = setup_db();

        // Create task and run for foreign key references
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
            .unwrap();

        let params = LearnParams {
            outcome: CliOutcome::Success,
            title: "Successful pattern".to_string(),
            content: "This worked well".to_string(),
            task_id: Some("US-001".to_string()),
            run_id: Some("run-001".to_string()),
            root_cause: Some("Root cause".to_string()),
            solution: Some("Applied fix".to_string()),
            files: Some(vec!["src/*.rs".to_string()]),
            task_types: Some(vec!["US-".to_string(), "FIX-".to_string()]),
            errors: Some(vec!["E0001".to_string()]),
            tags: Some(vec!["rust".to_string(), "database".to_string()]),
            confidence: CliConfidence::High,
        };

        let result = learn(&conn, params).unwrap();

        assert!(result.learning_id > 0);
        assert_eq!(result.title, "Successful pattern");
        assert_eq!(result.outcome, "success");
        assert_eq!(result.tags_added, 2);
    }

    #[test]
    fn test_learn_all_outcomes() {
        let (_temp_dir, conn) = setup_db();

        let outcomes = [
            (CliOutcome::Failure, "failure"),
            (CliOutcome::Success, "success"),
            (CliOutcome::Workaround, "workaround"),
            (CliOutcome::Pattern, "pattern"),
        ];

        for (cli_outcome, expected_str) in outcomes {
            let params = LearnParams {
                outcome: cli_outcome,
                title: format!("{} test", expected_str),
                content: "Content".to_string(),
                task_id: None,
                run_id: None,
                root_cause: None,
                solution: None,
                files: None,
                task_types: None,
                errors: None,
                tags: None,
                confidence: CliConfidence::Medium,
            };

            let result = learn(&conn, params).unwrap();
            assert_eq!(result.outcome, expected_str);
        }
    }

    #[test]
    fn test_learn_invalid_task_id() {
        let (_temp_dir, conn) = setup_db();

        let params = LearnParams {
            outcome: CliOutcome::Failure,
            title: "Test".to_string(),
            content: "Content".to_string(),
            task_id: Some("NONEXISTENT".to_string()),
            run_id: None,
            root_cause: None,
            solution: None,
            files: None,
            task_types: None,
            errors: None,
            tags: None,
            confidence: CliConfidence::Medium,
        };

        let result = learn(&conn, params);
        assert!(result.is_err(), "Should fail with invalid task_id");
    }

    #[test]
    fn test_format_text_basic() {
        let result = LearnResult {
            learning_id: 42,
            title: "Test learning".to_string(),
            outcome: "failure".to_string(),
            tags_added: 0,
        };

        let text = format_text(&result);
        assert!(text.contains("Learning recorded: Test learning"));
        assert!(text.contains("ID: 42"));
        assert!(text.contains("Outcome: failure"));
        assert!(!text.contains("Tags added"));
    }

    #[test]
    fn test_format_text_with_tags() {
        let result = LearnResult {
            learning_id: 1,
            title: "Tagged learning".to_string(),
            outcome: "pattern".to_string(),
            tags_added: 3,
        };

        let text = format_text(&result);
        assert!(text.contains("Tags added: 3"));
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
    fn test_cli_confidence_to_model() {
        assert_eq!(
            cli_confidence_to_model(CliConfidence::High),
            Confidence::High
        );
        assert_eq!(
            cli_confidence_to_model(CliConfidence::Medium),
            Confidence::Medium
        );
        assert_eq!(cli_confidence_to_model(CliConfidence::Low), Confidence::Low);
    }
}
