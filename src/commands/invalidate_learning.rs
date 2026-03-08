//! Invalidate-learning command implementation.
//!
//! Implements two-step degradation for learnings:
//! - First call: downgrades confidence to Low (action='downgraded')
//! - Second call (already Low): soft-archives via retired_at (action='retired')

use rusqlite::Connection;
use serde::Serialize;

use crate::learnings::crud::get_learning;
use crate::learnings::crud::{edit_learning, EditLearningParams};
use crate::models::Confidence;
use crate::TaskMgrResult;

/// Result of invalidating a learning.
#[derive(Debug, Clone, Serialize)]
pub struct InvalidateLearningResult {
    /// ID of the invalidated learning
    pub learning_id: i64,
    /// Title of the learning (for confirmation message)
    pub title: String,
    /// Confidence level before this operation (e.g. "high", "medium", "low")
    pub previous_confidence: String,
    /// Action taken: "downgraded" or "retired"
    pub action: String,
    /// New confidence level: Some("low") after downgrade, None after retirement
    pub new_confidence: Option<String>,
}

/// Invalidates a learning via two-step degradation.
///
/// - If confidence is not Low: sets confidence to Low, returns action='downgraded'.
/// - If confidence is already Low: sets retired_at, returns action='retired'.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning to invalidate
///
/// # Returns
///
/// `InvalidateLearningResult` describing the action taken.
///
/// # Errors
///
/// Returns `TaskMgrError::NotFound` if the learning doesn't exist.
/// Returns `TaskMgrError::InvalidState` if the learning is already retired.
pub fn invalidate_learning(
    conn: &Connection,
    learning_id: i64,
) -> TaskMgrResult<InvalidateLearningResult> {
    // Fetch the learning; return NotFound if absent
    let learning = get_learning(conn, learning_id)?
        .ok_or_else(|| crate::TaskMgrError::learning_not_found(learning_id.to_string()))?;

    // Check retirement status via direct SQL (Learning struct lacks retired_at)
    let retired_at: Option<String> = conn.query_row(
        "SELECT retired_at FROM learnings WHERE id = ?1",
        [learning_id],
        |row| row.get(0),
    )?;

    if retired_at.is_some() {
        return Err(crate::TaskMgrError::invalid_state(
            "learning",
            learning_id.to_string(),
            "not retired",
            "already retired",
        ));
    }

    let previous_confidence = learning.confidence.as_db_str().to_string();

    if learning.confidence != Confidence::Low {
        // Downgrade: set confidence to Low
        edit_learning(
            conn,
            learning_id,
            EditLearningParams {
                confidence: Some(Confidence::Low),
                ..Default::default()
            },
        )?;

        Ok(InvalidateLearningResult {
            learning_id,
            title: learning.title,
            previous_confidence,
            action: "downgraded".to_string(),
            new_confidence: Some("low".to_string()),
        })
    } else {
        // Already Low: retire via soft-delete
        conn.execute(
            "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
            [learning_id],
        )?;

        Ok(InvalidateLearningResult {
            learning_id,
            title: learning.title,
            previous_confidence,
            action: "retired".to_string(),
            new_confidence: None,
        })
    }
}

/// Format invalidate-learning result as human-readable text.
///
/// - Downgrade: `Invalidated learning #ID: "Title" (confidence: prev -> low)`
/// - Retire:    `Retired learning #ID: "Title" (was already low confidence)`
pub fn format_text(result: &InvalidateLearningResult) -> String {
    if result.action == "retired" {
        format!(
            "Retired learning #{}: \"{}\" (was already low confidence)",
            result.learning_id, result.title
        )
    } else {
        format!(
            "Invalidated learning #{}: \"{}\" (confidence: {} -> low)",
            result.learning_id, result.title, result.previous_confidence
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::learnings::test_helpers::{retire_learning, setup_db};
    use crate::models::{Confidence, LearningOutcome};

    fn make_params(title: &str, confidence: Confidence) -> RecordLearningParams {
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
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
            confidence,
        }
    }

    fn get_retired_at(conn: &Connection, learning_id: i64) -> Option<String> {
        conn.query_row(
            "SELECT retired_at FROM learnings WHERE id = ?1",
            [learning_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn get_confidence_from_db(conn: &Connection, learning_id: i64) -> String {
        conn.query_row(
            "SELECT confidence FROM learnings WHERE id = ?1",
            [learning_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    // --- Downgrade tests ---

    #[test]
    fn test_high_confidence_downgraded() {
        let (_tmp, conn) = setup_db();
        let rec = record_learning(&conn, make_params("High learning", Confidence::High)).unwrap();

        let result = invalidate_learning(&conn, rec.learning_id).unwrap();

        assert_eq!(result.action, "downgraded");
        assert_eq!(result.previous_confidence, "high");
        assert_eq!(result.new_confidence, Some("low".to_string()));
        assert_eq!(result.learning_id, rec.learning_id);
        assert_eq!(result.title, "High learning");
        // Must not retire on first call
        assert!(
            get_retired_at(&conn, rec.learning_id).is_none(),
            "retired_at must be NULL after downgrade"
        );
    }

    #[test]
    fn test_medium_confidence_downgraded() {
        let (_tmp, conn) = setup_db();
        let rec =
            record_learning(&conn, make_params("Medium learning", Confidence::Medium)).unwrap();

        let result = invalidate_learning(&conn, rec.learning_id).unwrap();

        assert_eq!(result.action, "downgraded");
        assert_eq!(result.previous_confidence, "medium");
        assert_eq!(result.new_confidence, Some("low".to_string()));
        assert!(
            get_retired_at(&conn, rec.learning_id).is_none(),
            "retired_at must be NULL after downgrade"
        );
    }

    // --- Retire test ---

    #[test]
    fn test_low_confidence_retired() {
        let (_tmp, conn) = setup_db();
        let rec = record_learning(&conn, make_params("Low learning", Confidence::Low)).unwrap();

        let result = invalidate_learning(&conn, rec.learning_id).unwrap();

        assert_eq!(result.action, "retired");
        assert_eq!(result.previous_confidence, "low");
        assert_eq!(result.new_confidence, None);
        assert!(
            get_retired_at(&conn, rec.learning_id).is_some(),
            "retired_at must be set after retirement"
        );
    }

    #[test]
    fn test_db_confidence_reads_low_after_downgrade() {
        let (_tmp, conn) = setup_db();
        let rec =
            record_learning(&conn, make_params("High to downgrade", Confidence::High)).unwrap();

        invalidate_learning(&conn, rec.learning_id).unwrap();

        let db_confidence = get_confidence_from_db(&conn, rec.learning_id);
        assert_eq!(
            db_confidence, "low",
            "confidence column in DB must be 'low' after downgrade, got: {db_confidence}"
        );
    }

    // --- Two-step sequence test ---

    #[test]
    fn test_two_step_sequence_high_to_low_to_retired() {
        let (_tmp, conn) = setup_db();
        let rec =
            record_learning(&conn, make_params("Sequence learning", Confidence::High)).unwrap();

        // First call: downgrade to low
        let first = invalidate_learning(&conn, rec.learning_id).unwrap();
        assert_eq!(first.action, "downgraded");
        assert_eq!(first.new_confidence, Some("low".to_string()));
        assert!(
            get_retired_at(&conn, rec.learning_id).is_none(),
            "retired_at must be NULL after first call (downgrade only)"
        );

        // Second call: retire
        let second = invalidate_learning(&conn, rec.learning_id).unwrap();
        assert_eq!(second.action, "retired");
        assert_eq!(second.new_confidence, None);
        assert!(
            get_retired_at(&conn, rec.learning_id).is_some(),
            "retired_at must be set after second call (retirement)"
        );
    }

    // --- Error tests ---

    #[test]
    fn test_already_retired_returns_invalid_state() {
        let (_tmp, conn) = setup_db();
        let rec =
            record_learning(&conn, make_params("Retired learning", Confidence::High)).unwrap();
        retire_learning(&conn, rec.learning_id);

        let result = invalidate_learning(&conn, rec.learning_id);

        assert!(
            result.is_err(),
            "must return Err for already-retired learning"
        );
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("Invalid state") || err_str.contains("retired"),
            "error must mention invalid state or retirement, got: {err_str}"
        );
    }

    #[test]
    fn test_nonexistent_id_returns_not_found() {
        let (_tmp, conn) = setup_db();

        let result = invalidate_learning(&conn, 99999);

        assert!(result.is_err(), "must return Err for non-existent learning");
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("not found") || err_str.contains("99999"),
            "error must mention 'not found' or the ID, got: {err_str}"
        );
    }

    // --- format_text tests ---

    #[test]
    fn test_format_text_downgrade() {
        let result = InvalidateLearningResult {
            learning_id: 42,
            title: "Useful pattern".to_string(),
            previous_confidence: "high".to_string(),
            action: "downgraded".to_string(),
            new_confidence: Some("low".to_string()),
        };

        let text = format_text(&result);

        assert!(text.contains("42"), "output must contain learning ID");
        assert!(text.contains("Useful pattern"), "output must contain title");
        assert!(
            text.contains("->") && text.contains("low"),
            "output must show confidence transition (-> low), got: {text}"
        );
    }

    #[test]
    fn test_format_text_retire() {
        let result = InvalidateLearningResult {
            learning_id: 7,
            title: "Old pattern".to_string(),
            previous_confidence: "low".to_string(),
            action: "retired".to_string(),
            new_confidence: None,
        };

        let text = format_text(&result);

        assert!(text.contains("7"), "output must contain learning ID");
        assert!(text.contains("Old pattern"), "output must contain title");
        assert!(
            text.contains("Retired") && text.contains("low confidence"),
            "output must contain 'Retired' and 'low confidence', got: {text}"
        );
    }

    // --- Known-bad discriminator ---

    /// Rejects a naive stub that retires on any call regardless of confidence.
    ///
    /// After the first invalidation of a High-confidence learning, confidence is
    /// now Low but action must be 'downgraded' — NOT 'retired'. A naive "always
    /// retire" stub would fail this test by returning action='retired'.
    #[test]
    fn test_known_bad_discriminator_first_call_is_downgraded_not_retired() {
        let (_tmp, conn) = setup_db();
        let rec = record_learning(&conn, make_params("High confidence", Confidence::High)).unwrap();

        let result = invalidate_learning(&conn, rec.learning_id).unwrap();

        assert_eq!(
            result.action, "downgraded",
            "First call must return 'downgraded', not 'retired' — rejects naive retire-all stub"
        );
        assert_eq!(result.new_confidence, Some("low".to_string()));
        assert!(
            get_retired_at(&conn, rec.learning_id).is_none(),
            "retired_at must be NULL after first call — learning is not yet retired"
        );
    }
}
