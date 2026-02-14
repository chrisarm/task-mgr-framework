//! Learnings list command implementation.
//!
//! Provides CLI entry point for listing all learnings from the institutional memory system.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::models::Learning;
use crate::TaskMgrResult;

/// Parameters for the learnings list command.
#[derive(Debug, Clone, Default)]
pub struct LearningsListParams {
    /// Show only the N most recent learnings
    pub recent: Option<usize>,
}

/// Summary of a learning for list output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningSummary {
    /// Learning ID
    pub id: i64,
    /// Title of the learning
    pub title: String,
    /// Outcome type
    pub outcome: String,
    /// Confidence level
    pub confidence: String,
    /// When the learning was created (ISO 8601)
    pub created_at: String,
    /// Times shown to agent
    pub times_shown: i32,
    /// Times marked as applied
    pub times_applied: i32,
}

/// Result of the learnings list command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningsListResult {
    /// Number of learnings returned
    pub count: usize,
    /// Total number of learnings in the database
    pub total: usize,
    /// The learnings (summaries)
    pub learnings: Vec<LearningSummary>,
    /// Whether the result was limited by --recent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limited_to: Option<usize>,
}

/// Lists learnings from the database.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `params` - List parameters (optional recent limit)
///
/// # Returns
///
/// Result containing the list of learnings.
pub fn list_learnings(
    conn: &Connection,
    params: LearningsListParams,
) -> TaskMgrResult<LearningsListResult> {
    // Get total count first
    let total: usize = conn.query_row("SELECT COUNT(*) FROM learnings", [], |row| row.get(0))?;

    // Build query with optional LIMIT
    // Use id DESC as secondary sort to ensure deterministic ordering when timestamps are equal
    let query = if let Some(limit) = params.recent {
        format!(
            r#"
            SELECT
                id, created_at, task_id, run_id, outcome, title, content,
                root_cause, solution,
                applies_to_files, applies_to_task_types, applies_to_errors,
                confidence, times_shown, times_applied, last_shown_at, last_applied_at
            FROM learnings
            ORDER BY created_at DESC, id DESC
            LIMIT {}
            "#,
            limit
        )
    } else {
        r#"
            SELECT
                id, created_at, task_id, run_id, outcome, title, content,
                root_cause, solution,
                applies_to_files, applies_to_task_types, applies_to_errors,
                confidence, times_shown, times_applied, last_shown_at, last_applied_at
            FROM learnings
            ORDER BY created_at DESC, id DESC
            "#
        .to_string()
    };

    let mut stmt = conn.prepare(&query)?;
    let learnings: Vec<Learning> = stmt
        .query_map([], |row| {
            Learning::try_from(row)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Convert to summaries
    let summaries: Vec<LearningSummary> = learnings
        .into_iter()
        .filter_map(|l| {
            l.id.map(|id| LearningSummary {
                id,
                title: l.title,
                outcome: l.outcome.to_string(),
                confidence: l.confidence.to_string(),
                created_at: l.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                times_shown: l.times_shown,
                times_applied: l.times_applied,
            })
        })
        .collect();

    let count = summaries.len();

    Ok(LearningsListResult {
        count,
        total,
        learnings: summaries,
        limited_to: params.recent,
    })
}

/// Formats the learnings list result for text output.
#[must_use]
pub fn format_text(result: &LearningsListResult) -> String {
    let mut output = String::new();

    if result.learnings.is_empty() {
        output.push_str("No learnings found.\n");
        return output;
    }

    // Header
    if let Some(limit) = result.limited_to {
        output.push_str(&format!(
            "Showing {} of {} learnings (limited to {} most recent):\n\n",
            result.count, result.total, limit
        ));
    } else {
        output.push_str(&format!("Showing {} learnings:\n\n", result.count));
    }

    // Table header
    output.push_str(&format!(
        "{:>4}  {:<40}  {:<10}  {:<8}  {:>5}  {:>7}\n",
        "ID", "TITLE", "OUTCOME", "CONF", "SHOWN", "APPLIED"
    ));
    output.push_str(&format!("{:-<80}\n", ""));

    // Table rows
    for learning in &result.learnings {
        // Truncate title if too long
        let title = if learning.title.len() > 40 {
            format!("{}...", &learning.title[..37])
        } else {
            learning.title.clone()
        };

        output.push_str(&format!(
            "{:>4}  {:<40}  {:<10}  {:<8}  {:>5}  {:>7}\n",
            learning.id,
            title,
            learning.outcome,
            learning.confidence,
            learning.times_shown,
            learning.times_applied
        ));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, open_connection};
    use crate::learnings::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    fn create_test_learning(conn: &Connection, title: &str, outcome: LearningOutcome) -> i64 {
        let params = RecordLearningParams {
            outcome,
            title: title.to_string(),
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
        };
        record_learning(conn, params).unwrap().learning_id
    }

    #[test]
    fn test_list_learnings_empty_database() {
        let (_temp_dir, conn) = setup_db();

        let params = LearningsListParams::default();
        let result = list_learnings(&conn, params).unwrap();

        assert_eq!(result.count, 0);
        assert_eq!(result.total, 0);
        assert!(result.learnings.is_empty());
        assert!(result.limited_to.is_none());
    }

    #[test]
    fn test_list_learnings_all() {
        let (_temp_dir, conn) = setup_db();

        create_test_learning(&conn, "Learning 1", LearningOutcome::Failure);
        create_test_learning(&conn, "Learning 2", LearningOutcome::Success);
        create_test_learning(&conn, "Learning 3", LearningOutcome::Pattern);

        let params = LearningsListParams::default();
        let result = list_learnings(&conn, params).unwrap();

        assert_eq!(result.count, 3);
        assert_eq!(result.total, 3);
        assert_eq!(result.learnings.len(), 3);
        assert!(result.limited_to.is_none());
    }

    #[test]
    fn test_list_learnings_with_recent_limit() {
        let (_temp_dir, conn) = setup_db();

        for i in 1..=10 {
            create_test_learning(&conn, &format!("Learning {}", i), LearningOutcome::Pattern);
        }

        let params = LearningsListParams { recent: Some(5) };
        let result = list_learnings(&conn, params).unwrap();

        assert_eq!(result.count, 5);
        assert_eq!(result.total, 10);
        assert_eq!(result.learnings.len(), 5);
        assert_eq!(result.limited_to, Some(5));
    }

    #[test]
    fn test_list_learnings_ordered_by_created_at_desc() {
        let (_temp_dir, conn) = setup_db();

        // Create learnings in order
        let id1 = create_test_learning(&conn, "First", LearningOutcome::Failure);
        let id2 = create_test_learning(&conn, "Second", LearningOutcome::Success);
        let id3 = create_test_learning(&conn, "Third", LearningOutcome::Pattern);

        let params = LearningsListParams::default();
        let result = list_learnings(&conn, params).unwrap();

        // Most recent first (Third, Second, First)
        assert_eq!(result.learnings[0].id, id3);
        assert_eq!(result.learnings[1].id, id2);
        assert_eq!(result.learnings[2].id, id1);
    }

    #[test]
    fn test_list_learnings_includes_outcome() {
        let (_temp_dir, conn) = setup_db();

        create_test_learning(&conn, "Failure learning", LearningOutcome::Failure);
        create_test_learning(&conn, "Success learning", LearningOutcome::Success);
        create_test_learning(&conn, "Workaround learning", LearningOutcome::Workaround);
        create_test_learning(&conn, "Pattern learning", LearningOutcome::Pattern);

        let params = LearningsListParams::default();
        let result = list_learnings(&conn, params).unwrap();

        // Check outcomes are correct (order is DESC by created_at)
        assert_eq!(result.learnings[0].outcome, "pattern");
        assert_eq!(result.learnings[1].outcome, "workaround");
        assert_eq!(result.learnings[2].outcome, "success");
        assert_eq!(result.learnings[3].outcome, "failure");
    }

    #[test]
    fn test_list_learnings_includes_confidence() {
        let (_temp_dir, conn) = setup_db();

        // Create learnings with different confidences
        let params_high = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "High confidence".to_string(),
            content: "Content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        };
        record_learning(&conn, params_high).unwrap();

        let params_low = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Low confidence".to_string(),
            content: "Content".to_string(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::Low,
        };
        record_learning(&conn, params_low).unwrap();

        let params = LearningsListParams::default();
        let result = list_learnings(&conn, params).unwrap();

        // Most recent first (Low, High)
        assert_eq!(result.learnings[0].confidence, "low");
        assert_eq!(result.learnings[1].confidence, "high");
    }

    #[test]
    fn test_list_learnings_recent_larger_than_total() {
        let (_temp_dir, conn) = setup_db();

        create_test_learning(&conn, "Learning 1", LearningOutcome::Pattern);
        create_test_learning(&conn, "Learning 2", LearningOutcome::Pattern);

        // Request more than exist
        let params = LearningsListParams { recent: Some(100) };
        let result = list_learnings(&conn, params).unwrap();

        assert_eq!(result.count, 2);
        assert_eq!(result.total, 2);
        assert_eq!(result.learnings.len(), 2);
        assert_eq!(result.limited_to, Some(100));
    }

    #[test]
    fn test_format_text_empty() {
        let result = LearningsListResult {
            count: 0,
            total: 0,
            learnings: vec![],
            limited_to: None,
        };

        let text = format_text(&result);
        assert!(text.contains("No learnings found"));
    }

    #[test]
    fn test_format_text_with_learnings() {
        let result = LearningsListResult {
            count: 2,
            total: 2,
            learnings: vec![
                LearningSummary {
                    id: 1,
                    title: "Test failure".to_string(),
                    outcome: "failure".to_string(),
                    confidence: "medium".to_string(),
                    created_at: "2026-01-18 12:00:00".to_string(),
                    times_shown: 5,
                    times_applied: 2,
                },
                LearningSummary {
                    id: 2,
                    title: "Test success".to_string(),
                    outcome: "success".to_string(),
                    confidence: "high".to_string(),
                    created_at: "2026-01-18 13:00:00".to_string(),
                    times_shown: 3,
                    times_applied: 1,
                },
            ],
            limited_to: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Showing 2 learnings"));
        assert!(text.contains("Test failure"));
        assert!(text.contains("failure"));
        assert!(text.contains("Test success"));
        assert!(text.contains("success"));
    }

    #[test]
    fn test_format_text_with_limit() {
        let result = LearningsListResult {
            count: 5,
            total: 20,
            learnings: vec![LearningSummary {
                id: 1,
                title: "Test".to_string(),
                outcome: "pattern".to_string(),
                confidence: "medium".to_string(),
                created_at: "2026-01-18 12:00:00".to_string(),
                times_shown: 0,
                times_applied: 0,
            }],
            limited_to: Some(5),
        };

        let text = format_text(&result);
        assert!(text.contains("Showing 5 of 20 learnings"));
        assert!(text.contains("limited to 5 most recent"));
    }

    #[test]
    fn test_format_text_truncates_long_title() {
        let result = LearningsListResult {
            count: 1,
            total: 1,
            learnings: vec![LearningSummary {
                id: 1,
                title: "This is a very long title that should definitely be truncated to fit in the table".to_string(),
                outcome: "pattern".to_string(),
                confidence: "medium".to_string(),
                created_at: "2026-01-18 12:00:00".to_string(),
                times_shown: 0,
                times_applied: 0,
            }],
            limited_to: None,
        };

        let text = format_text(&result);
        // Should contain truncated title with "..."
        assert!(text.contains("..."));
        // Should not contain full title
        assert!(!text.contains("should definitely be truncated to fit in the table"));
    }

    #[test]
    fn test_learning_summary_serialization() {
        let summary = LearningSummary {
            id: 42,
            title: "Test learning".to_string(),
            outcome: "failure".to_string(),
            confidence: "high".to_string(),
            created_at: "2026-01-18 12:00:00".to_string(),
            times_shown: 10,
            times_applied: 5,
        };

        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"id\":42"));
        assert!(json.contains("\"title\":\"Test learning\""));
        assert!(json.contains("\"outcome\":\"failure\""));
        assert!(json.contains("\"confidence\":\"high\""));
        assert!(json.contains("\"times_shown\":10"));
        assert!(json.contains("\"times_applied\":5"));
    }

    #[test]
    fn test_list_result_serialization() {
        let result = LearningsListResult {
            count: 1,
            total: 5,
            learnings: vec![LearningSummary {
                id: 1,
                title: "Test".to_string(),
                outcome: "pattern".to_string(),
                confidence: "medium".to_string(),
                created_at: "2026-01-18 12:00:00".to_string(),
                times_shown: 0,
                times_applied: 0,
            }],
            limited_to: Some(1),
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"count\":1"));
        assert!(json.contains("\"total\":5"));
        assert!(json.contains("\"limited_to\":1"));
    }

    #[test]
    fn test_list_result_serialization_skips_none_limited_to() {
        let result = LearningsListResult {
            count: 0,
            total: 0,
            learnings: vec![],
            limited_to: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("limited_to"));
    }
}
