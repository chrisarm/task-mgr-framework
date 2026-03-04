//! Learnings section builder for the agent loop prompt.
//!
//! Provides `build_learnings_section` (formats UCB-ranked learnings as JSON)
//! and `record_shown_learnings` (records which learnings were displayed so the
//! bandit feedback loop can update their scores).

use rusqlite::Connection;

use crate::commands::next::output::LearningSummaryOutput;
use crate::learnings::bandit;

use super::truncate_to_budget;

/// Byte budget for the serialized learnings JSON block.
const LEARNINGS_BUDGET: usize = 4_000;

/// Build a learnings section string.
pub(crate) fn build_learnings_section(learnings: &[LearningSummaryOutput]) -> String {
    if learnings.is_empty() {
        return String::new();
    }

    let learnings_json =
        serde_json::to_string_pretty(learnings).unwrap_or_else(|_| "[]".to_string());
    let learnings_json = truncate_to_budget(&learnings_json, LEARNINGS_BUDGET);
    format!(
        "## Relevant Learnings\n\n```json\n{}\n```\n\n",
        learnings_json
    )
}

/// Record which learnings were shown to Claude for this iteration.
///
/// Returns the list of shown learning IDs (used to track feedback loop).
pub(crate) fn record_shown_learnings(
    conn: &Connection,
    learnings: &[LearningSummaryOutput],
    iteration: i64,
) -> Vec<i64> {
    let mut shown_ids = Vec::with_capacity(learnings.len());
    for learning in learnings {
        shown_ids.push(learning.id);
        if let Err(e) = bandit::record_learning_shown(conn, learning.id, iteration) {
            eprintln!(
                "Warning: failed to record learning {} as shown: {}",
                learning.id, e
            );
        }
    }
    shown_ids
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::commands::next::output::LearningSummaryOutput;
    use crate::loop_engine::test_utils::{insert_test_learning, setup_test_db};

    #[test]
    fn test_append_learnings_empty() {
        let result = build_learnings_section(&[]);
        assert!(result.is_empty(), "No learnings should produce no section");
    }

    #[test]
    fn test_append_learnings_with_content() {
        let learnings = vec![LearningSummaryOutput {
            id: 1,
            title: "Test Learning".to_string(),
            outcome: "pattern".to_string(),
            confidence: "high".to_string(),
            content: Some("Use X instead of Y".to_string()),
            applies_to_files: None,
            applies_to_task_types: None,
        }];

        let result = build_learnings_section(&learnings);
        assert!(result.contains("## Relevant Learnings"));
        assert!(result.contains("Test Learning"));
    }

    #[test]
    fn test_append_learnings_multiple() {
        let learnings = vec![
            LearningSummaryOutput {
                id: 1,
                title: "First Learning".to_string(),
                outcome: "pattern".to_string(),
                confidence: "high".to_string(),
                content: Some("Content 1".to_string()),
                applies_to_files: Some(vec!["src/*.rs".to_string()]),
                applies_to_task_types: None,
            },
            LearningSummaryOutput {
                id: 2,
                title: "Second Learning".to_string(),
                outcome: "failure".to_string(),
                confidence: "medium".to_string(),
                content: None,
                applies_to_files: None,
                applies_to_task_types: Some(vec!["FEAT-".to_string()]),
            },
        ];

        let result = build_learnings_section(&learnings);

        assert!(result.contains("## Relevant Learnings"));
        assert!(result.contains("```json"));
        assert!(result.contains("First Learning"));
        assert!(result.contains("Second Learning"));
        assert!(result.contains("Content 1"));
    }

    #[test]
    fn test_record_shown_learnings_empty() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let ids = record_shown_learnings(&conn, &[], 1);
        assert!(ids.is_empty());
    }

    #[test]
    fn test_record_shown_learnings_tracks_ids() {
        let (_temp_dir, conn) = setup_test_db();

        let id1 = insert_test_learning(&conn, "Learning A");
        let id2 = insert_test_learning(&conn, "Learning B");

        let learnings = vec![
            LearningSummaryOutput {
                id: id1,
                title: "Learning A".to_string(),
                outcome: "pattern".to_string(),
                confidence: "medium".to_string(),
                content: Some("Content A".to_string()),
                applies_to_files: None,
                applies_to_task_types: None,
            },
            LearningSummaryOutput {
                id: id2,
                title: "Learning B".to_string(),
                outcome: "success".to_string(),
                confidence: "high".to_string(),
                content: Some("Content B".to_string()),
                applies_to_files: None,
                applies_to_task_types: None,
            },
        ];

        let ids = record_shown_learnings(&conn, &learnings, 5);

        assert_eq!(ids.len(), 2, "Should return 2 shown IDs");
        assert_eq!(ids[0], id1, "First ID should match");
        assert_eq!(ids[1], id2, "Second ID should match");
    }

    #[test]
    fn test_record_shown_learnings_graceful_on_invalid_id() {
        let (_temp_dir, conn) = setup_test_db();

        // Learning ID 99999 doesn't exist, but record_learning_shown should
        // either succeed (no-op) or log warning and continue
        let learnings = vec![LearningSummaryOutput {
            id: 99999,
            title: "Ghost learning".to_string(),
            outcome: "pattern".to_string(),
            confidence: "low".to_string(),
            content: None,
            applies_to_files: None,
            applies_to_task_types: None,
        }];

        // Should not panic
        let ids = record_shown_learnings(&conn, &learnings, 1);
        assert_eq!(
            ids.len(),
            1,
            "Should still return the ID even if DB op fails"
        );
        assert_eq!(ids[0], 99999);
    }

    #[test]
    fn test_append_learnings_truncation_over_budget() {
        // 5 learnings with ~2KB content each → ~10KB total, exceeds LEARNINGS_BUDGET (4K)
        let learnings: Vec<LearningSummaryOutput> = (0..5)
            .map(|i| LearningSummaryOutput {
                id: i,
                title: format!("Learning {}", i),
                outcome: "pattern".to_string(),
                confidence: "high".to_string(),
                content: Some("x".repeat(2000)),
                applies_to_files: None,
                applies_to_task_types: None,
            })
            .collect();

        let result = build_learnings_section(&learnings);

        assert!(
            result.contains("## Relevant Learnings"),
            "Should have learnings section header"
        );
        assert!(
            result.contains("[truncated to"),
            "Learnings JSON should be truncated when exceeding budget"
        );
        let json_start = result.find("```json\n").unwrap() + 8;
        let json_end = result.rfind("\n```").unwrap();
        let json_section = &result[json_start..json_end];
        assert!(
            json_section.len() <= LEARNINGS_BUDGET + 100,
            "Learnings JSON section ({} bytes) should be within budget ({} + overhead)",
            json_section.len(),
            LEARNINGS_BUDGET
        );
    }

    #[test]
    fn test_append_learnings_under_budget_not_truncated() {
        let learnings = vec![LearningSummaryOutput {
            id: 1,
            title: "Small learning".to_string(),
            outcome: "pattern".to_string(),
            confidence: "high".to_string(),
            content: Some("Use X".to_string()),
            applies_to_files: None,
            applies_to_task_types: None,
        }];

        let result = build_learnings_section(&learnings);

        assert!(
            !result.contains("[truncated to"),
            "Small learnings should not be truncated"
        );
        assert!(
            result.contains("Small learning"),
            "Content should be preserved"
        );
    }
}
