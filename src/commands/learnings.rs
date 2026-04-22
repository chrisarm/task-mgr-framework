//! Learnings list command implementation.
//!
//! Provides CLI entry point for listing all learnings from the institutional memory system.

use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::TaskMgrResult;
use crate::models::Learning;

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
    /// ID of the learning that supersedes this one, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<i64>,
    /// IDs of learnings this one supersedes, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Vec<i64>>,
}

/// Result of the learnings list command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningsListResult {
    /// Number of learnings returned
    pub count: usize,
    /// Total number of active (non-retired) learnings in the database
    pub total: usize,
    /// Total number of learnings including retired ones
    pub total_including_retired: usize,
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
    // Get active count (non-retired)
    let total: usize = conn.query_row(
        "SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL",
        [],
        |row| row.get(0),
    )?;

    // Get total count including retired
    let total_including_retired: usize =
        conn.query_row("SELECT COUNT(*) FROM learnings", [], |row| row.get(0))?;

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
            WHERE retired_at IS NULL
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
            WHERE retired_at IS NULL
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

    // Convert to summaries (supersession fields filled below)
    let mut summaries: Vec<LearningSummary> = learnings
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
                superseded_by: None,
                supersedes: None,
            })
        })
        .collect();

    // Batch-query supersessions for all displayed IDs (single query, not N+1).
    if !summaries.is_empty() {
        let id_list: Vec<i64> = summaries.iter().map(|s| s.id).collect();
        // Parameterised IN clauses for both columns (chained in params below).
        // Numbered placeholders let the first IN list reuse bindings 1..=N and
        // the second use N+1..=2N without re-binding the same values.
        let first_placeholders: String = (1..=id_list.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let second_placeholders: String = (id_list.len() + 1..=id_list.len() * 2)
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sup_sql = format!(
            "SELECT old_learning_id, new_learning_id FROM learning_supersessions \
             WHERE old_learning_id IN ({first_placeholders}) \
             OR new_learning_id IN ({second_placeholders})"
        );
        let mut sup_stmt = conn.prepare(&sup_sql)?;
        let id_set: std::collections::HashSet<i64> = id_list.iter().copied().collect();
        let mut superseded_by_map: HashMap<i64, i64> = HashMap::new();
        let mut supersedes_map: HashMap<i64, Vec<i64>> = HashMap::new();
        let chained_params: Vec<i64> = id_list.iter().chain(id_list.iter()).copied().collect();
        let rows = sup_stmt
            .query_map(rusqlite::params_from_iter(chained_params.iter()), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
        for row in rows {
            let (old_id, new_id) = row?;
            if id_set.contains(&old_id) {
                superseded_by_map.insert(old_id, new_id);
            }
            if id_set.contains(&new_id) {
                supersedes_map.entry(new_id).or_default().push(old_id);
            }
        }
        for summary in &mut summaries {
            summary.superseded_by = superseded_by_map.get(&summary.id).copied();
            let sups = supersedes_map.remove(&summary.id);
            if sups.is_some() {
                summary.supersedes = sups;
            }
        }
    }

    let count = summaries.len();

    Ok(LearningsListResult {
        count,
        total,
        total_including_retired,
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
    let retired_count = result.total_including_retired.saturating_sub(result.total);
    if let Some(limit) = result.limited_to {
        if retired_count > 0 {
            output.push_str(&format!(
                "Showing {} of {} active learnings ({} retired) (limited to {} most recent):\n\n",
                result.count, result.total, retired_count, limit
            ));
        } else {
            output.push_str(&format!(
                "Showing {} of {} learnings (limited to {} most recent):\n\n",
                result.count, result.total, limit
            ));
        }
    } else if retired_count > 0 {
        output.push_str(&format!(
            "Showing {} of {} active learnings ({} retired):\n\n",
            result.count, result.total, retired_count
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
        let annotation = if let Some(sup_by) = learning.superseded_by {
            format!(" (superseded by #{})", sup_by)
        } else if let Some(sups) = &learning.supersedes {
            if !sups.is_empty() {
                let ids: Vec<String> = sups.iter().map(|id| format!("#{}", id)).collect();
                format!(" (supersedes {})", ids.join(", "))
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // truncate_str(s, N) produces at most N+3 chars when truncated ("..." appended).
        // We need title + annotation ≤ 40 chars, so max_trunc = 40 - annotation_len - 3.
        let max_trunc = if annotation.is_empty() {
            37
        } else {
            (40usize.saturating_sub(annotation.len()))
                .saturating_sub(3)
                .max(5)
        };
        let title = super::truncate_str(&learning.title, max_trunc);
        let title_col = format!("{}{}", title, annotation);

        output.push_str(&format!(
            "{:>4}  {:<40}  {:<10}  {:<8}  {:>5}  {:>7}\n",
            learning.id,
            title_col,
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
    use crate::db::{create_schema, migrations::run_migrations, open_connection};
    use crate::learnings::{RecordLearningParams, record_learning};
    use crate::models::{Confidence, LearningOutcome};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
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

    fn make_summary(id: i64, title: &str, outcome: &str) -> LearningSummary {
        LearningSummary {
            id,
            title: title.to_string(),
            outcome: outcome.to_string(),
            confidence: "medium".to_string(),
            created_at: "2026-01-18 12:00:00".to_string(),
            times_shown: 0,
            times_applied: 0,
            superseded_by: None,
            supersedes: None,
        }
    }

    #[test]
    fn test_format_text_empty() {
        let result = LearningsListResult {
            count: 0,
            total: 0,
            total_including_retired: 0,
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
            total_including_retired: 2,
            learnings: vec![
                LearningSummary {
                    times_shown: 5,
                    times_applied: 2,
                    confidence: "medium".to_string(),
                    ..make_summary(1, "Test failure", "failure")
                },
                LearningSummary {
                    times_shown: 3,
                    times_applied: 1,
                    confidence: "high".to_string(),
                    created_at: "2026-01-18 13:00:00".to_string(),
                    ..make_summary(2, "Test success", "success")
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
            total_including_retired: 20,
            learnings: vec![make_summary(1, "Test", "pattern")],
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
            total_including_retired: 1,
            learnings: vec![make_summary(
                1,
                "This is a very long title that should definitely be truncated to fit in the table",
                "pattern",
            )],
            limited_to: None,
        };

        let text = format_text(&result);
        assert!(text.contains("..."));
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
            superseded_by: None,
            supersedes: None,
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
            total_including_retired: 5,
            learnings: vec![make_summary(1, "Test", "pattern")],
            limited_to: Some(1),
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"count\":1"));
        assert!(json.contains("\"total\":5"));
        assert!(json.contains("\"limited_to\":1"));
    }

    // ========== TEST-INIT-001: retired_at Filtering Tests ==========
    //
    // Tests verify retired learnings are excluded from list results and counts.
    // All tests are #[ignore] until FEAT-001 and FEAT-002 are implemented.
    //
    // Query locations covered:
    //   8. Learnings list (list_learnings — SELECT rows)
    //   9. Learnings count (list_learnings — SELECT COUNT(*) total)

    use crate::learnings::test_helpers::retire_learning;

    #[test]
    fn test_retired_excluded_from_list_results() {
        // AC: retired learning excluded from learnings list rows
        let (_temp_dir, conn) = setup_db();

        let active_id = create_test_learning(&conn, "Active learning", LearningOutcome::Pattern);
        let retired_id = create_test_learning(&conn, "Retired learning", LearningOutcome::Success);
        retire_learning(&conn, retired_id);

        let result = list_learnings(&conn, LearningsListParams::default()).unwrap();

        assert_eq!(result.count, 1, "count must exclude retired learning");
        assert!(
            result.learnings.iter().all(|s| s.id != retired_id),
            "retired learning must not appear in list results"
        );
        assert_eq!(
            result.learnings[0].id, active_id,
            "only the active learning must be listed"
        );
    }

    #[test]
    fn test_retired_excluded_from_list_total_count() {
        // AC: retired learning excluded from learnings list `total` (SELECT COUNT(*))
        let (_temp_dir, conn) = setup_db();

        create_test_learning(&conn, "Active", LearningOutcome::Pattern);
        let retired_id = create_test_learning(&conn, "Retired", LearningOutcome::Success);
        retire_learning(&conn, retired_id);

        let result = list_learnings(&conn, LearningsListParams::default()).unwrap();

        assert_eq!(
            result.total, 1,
            "total must exclude retired learning (expected 1, got {})",
            result.total
        );
        assert_eq!(
            result.count, 1,
            "count must also exclude retired learning (expected 1, got {})",
            result.count
        );
        assert_eq!(
            result.total_including_retired, 2,
            "total_including_retired must include retired learning (expected 2, got {})",
            result.total_including_retired
        );
    }

    #[test]
    fn test_recent_limit_with_mix_of_active_and_retired() {
        // AC: learnings list with --recent limit works correctly with mix of active/retired.
        // The limit applies only to active learnings; retired learnings are excluded entirely.
        let (_temp_dir, conn) = setup_db();

        // Insert 6 active learnings
        for i in 1..=6 {
            create_test_learning(&conn, &format!("Active {}", i), LearningOutcome::Pattern);
        }
        // Insert 3 retired learnings
        for i in 1..=3 {
            let id =
                create_test_learning(&conn, &format!("Retired {}", i), LearningOutcome::Success);
            conn.execute(
                "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
                [id],
            )
            .unwrap();
        }

        // --recent 4 should return the 4 most recent ACTIVE learnings, ignoring retired
        let params = LearningsListParams { recent: Some(4) };
        let result = list_learnings(&conn, params).unwrap();

        assert_eq!(result.count, 4, "should return 4 active learnings");
        assert_eq!(result.total, 6, "total active learnings is 6");
        assert_eq!(
            result.total_including_retired, 9,
            "total including retired is 9"
        );
        assert_eq!(result.limited_to, Some(4));
        assert!(
            result
                .learnings
                .iter()
                .all(|s| !s.title.starts_with("Retired")),
            "no retired learnings should appear in results"
        );
    }

    #[test]
    fn test_format_text_shows_retired_count_when_nonzero() {
        let result = LearningsListResult {
            count: 1,
            total: 1,
            total_including_retired: 3,
            learnings: vec![LearningSummary {
                confidence: "high".to_string(),
                ..make_summary(1, "Active", "pattern")
            }],
            limited_to: None,
        };

        let text = format_text(&result);
        assert!(
            text.contains("1 of 1 active learnings"),
            "should show active count"
        );
        assert!(text.contains("2 retired"), "should show retired count");
    }

    #[test]
    fn test_format_text_no_retired_count_when_zero() {
        let result = LearningsListResult {
            count: 2,
            total: 2,
            total_including_retired: 2,
            learnings: vec![
                LearningSummary {
                    confidence: "high".to_string(),
                    ..make_summary(1, "Test", "pattern")
                },
                make_summary(2, "Test 2", "failure"),
            ],
            limited_to: None,
        };

        let text = format_text(&result);
        assert!(
            text.contains("Showing 2 learnings"),
            "no retired info when all active"
        );
        assert!(
            !text.contains("retired"),
            "should not mention retired when count is 0"
        );
    }

    #[test]
    fn test_list_result_serialization_includes_total_including_retired() {
        let result = LearningsListResult {
            count: 1,
            total: 1,
            total_including_retired: 3,
            learnings: vec![make_summary(1, "Test", "pattern")],
            limited_to: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"total_including_retired\":3"));
    }

    #[test]
    fn test_list_result_serialization_skips_none_limited_to() {
        let result = LearningsListResult {
            count: 0,
            total: 0,
            total_including_retired: 0,
            learnings: vec![],
            limited_to: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("limited_to"));
    }

    // ========== FEAT-006: Supersession annotation tests ==========

    fn insert_supersession(conn: &Connection, old_id: i64, new_id: i64) {
        conn.execute(
            "INSERT INTO learning_supersessions (old_learning_id, new_learning_id) VALUES (?1, ?2)",
            rusqlite::params![old_id, new_id],
        )
        .unwrap();
    }

    #[test]
    fn test_list_superseded_learning_shows_superseded_by_field() {
        // AC: learnings list JSON output includes superseded_by when a learning is superseded.
        let (_temp_dir, conn) = setup_db();
        let old_id = create_test_learning(&conn, "Old learning", LearningOutcome::Pattern);
        let new_id = create_test_learning(&conn, "New learning", LearningOutcome::Pattern);
        insert_supersession(&conn, old_id, new_id);

        let result = list_learnings(&conn, LearningsListParams::default()).unwrap();

        let old_summary = result.learnings.iter().find(|s| s.id == old_id).unwrap();
        assert_eq!(
            old_summary.superseded_by,
            Some(new_id),
            "superseded learning must report superseded_by = new_id"
        );
        assert!(
            old_summary.supersedes.is_none(),
            "superseded learning must not claim to supersede anything"
        );
    }

    #[test]
    fn test_list_superseding_learning_shows_supersedes_field() {
        // AC: learnings list JSON output includes supersedes when a learning supersedes another.
        let (_temp_dir, conn) = setup_db();
        let old_id = create_test_learning(&conn, "Old learning", LearningOutcome::Pattern);
        let new_id = create_test_learning(&conn, "New learning", LearningOutcome::Pattern);
        insert_supersession(&conn, old_id, new_id);

        let result = list_learnings(&conn, LearningsListParams::default()).unwrap();

        let new_summary = result.learnings.iter().find(|s| s.id == new_id).unwrap();
        assert_eq!(
            new_summary.supersedes,
            Some(vec![old_id]),
            "superseding learning must list the old learning ID in supersedes"
        );
        assert!(
            new_summary.superseded_by.is_none(),
            "superseding learning must not itself be marked as superseded"
        );
    }

    #[test]
    fn test_list_no_annotation_for_unrelated_learning() {
        // AC: No annotation when learning has no supersession relationship.
        let (_temp_dir, conn) = setup_db();
        let id_a = create_test_learning(&conn, "Unrelated A", LearningOutcome::Pattern);
        let id_b = create_test_learning(&conn, "Linked old", LearningOutcome::Pattern);
        let id_c = create_test_learning(&conn, "Linked new", LearningOutcome::Pattern);
        insert_supersession(&conn, id_b, id_c);

        let result = list_learnings(&conn, LearningsListParams::default()).unwrap();

        let unrelated = result.learnings.iter().find(|s| s.id == id_a).unwrap();
        assert!(unrelated.superseded_by.is_none());
        assert!(unrelated.supersedes.is_none());
    }

    #[test]
    fn test_format_text_shows_superseded_by_annotation() {
        // AC: text output shows '(superseded by #N)' after title when superseded.
        let result = LearningsListResult {
            count: 1,
            total: 1,
            total_including_retired: 1,
            learnings: vec![LearningSummary {
                superseded_by: Some(42),
                ..make_summary(1, "Old learning", "pattern")
            }],
            limited_to: None,
        };

        let text = format_text(&result);
        assert!(
            text.contains("(superseded by #42)"),
            "text output must contain '(superseded by #42)'"
        );
    }

    #[test]
    fn test_format_text_shows_supersedes_annotation() {
        // AC: text output shows '(supersedes #N)' after title when superseding another.
        let result = LearningsListResult {
            count: 1,
            total: 1,
            total_including_retired: 1,
            learnings: vec![LearningSummary {
                supersedes: Some(vec![7]),
                ..make_summary(2, "New learning", "pattern")
            }],
            limited_to: None,
        };

        let text = format_text(&result);
        assert!(
            text.contains("(supersedes #7)"),
            "text output must contain '(supersedes #7)'"
        );
    }

    #[test]
    fn test_format_text_no_annotation_for_plain_learning() {
        // AC: no annotation when learning has no supersession relationship.
        let result = LearningsListResult {
            count: 1,
            total: 1,
            total_including_retired: 1,
            learnings: vec![make_summary(1, "Plain learning", "pattern")],
            limited_to: None,
        };

        let text = format_text(&result);
        assert!(
            !text.contains("superseded"),
            "no supersession annotation expected"
        );
        assert!(
            !text.contains("supersedes"),
            "no supersession annotation expected"
        );
    }

    #[test]
    fn test_json_superseded_by_omitted_when_none() {
        // AC: JSON output omits superseded_by when not present.
        let summary = make_summary(1, "Plain", "pattern");
        let json = serde_json::to_string(&summary).unwrap();
        assert!(
            !json.contains("superseded_by"),
            "superseded_by must be absent from JSON when None"
        );
        assert!(
            !json.contains("supersedes"),
            "supersedes must be absent from JSON when None"
        );
    }

    #[test]
    fn test_json_superseded_by_present_when_set() {
        // AC: JSON output includes superseded_by and supersedes when applicable.
        let summary = LearningSummary {
            superseded_by: Some(99),
            supersedes: Some(vec![5, 6]),
            ..make_summary(1, "Linked", "pattern")
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"superseded_by\":99"));
        assert!(json.contains("\"supersedes\":[5,6]"));
    }

    #[test]
    fn test_list_learnings_supersession_batch_query() {
        // AC: single batch query for all displayed learning IDs (structural — confirmed by
        // the implementation using a single SQL statement, but we verify correct results
        // across multiple learnings with mixed supersession states in one call).
        let (_temp_dir, conn) = setup_db();
        let id_a = create_test_learning(&conn, "A: superseded", LearningOutcome::Pattern);
        let id_b = create_test_learning(&conn, "B: superseding", LearningOutcome::Pattern);
        let id_c = create_test_learning(&conn, "C: plain", LearningOutcome::Success);
        insert_supersession(&conn, id_a, id_b);

        let result = list_learnings(&conn, LearningsListParams::default()).unwrap();
        assert_eq!(result.count, 3);

        let a = result.learnings.iter().find(|s| s.id == id_a).unwrap();
        assert_eq!(a.superseded_by, Some(id_b));
        assert!(a.supersedes.is_none());

        let b = result.learnings.iter().find(|s| s.id == id_b).unwrap();
        assert_eq!(b.supersedes, Some(vec![id_a]));
        assert!(b.superseded_by.is_none());

        let c = result.learnings.iter().find(|s| s.id == id_c).unwrap();
        assert!(c.superseded_by.is_none());
        assert!(c.supersedes.is_none());
    }
}
