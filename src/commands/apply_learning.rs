//! Apply-learning command implementation.
//!
//! Records when a learning was applied (marked as useful by the agent).
//! This provides feedback for the UCB bandit ranking system.

use rusqlite::Connection;
use serde::Serialize;

use crate::learnings::bandit::record_learning_applied;
use crate::TaskMgrResult;

/// Result of applying a learning.
#[derive(Debug, Clone, Serialize)]
pub struct ApplyLearningResult {
    /// ID of the learning that was applied
    pub learning_id: i64,
    /// Title of the learning (for confirmation message)
    pub title: String,
    /// Updated times_applied count
    pub times_applied: i32,
    /// Updated window_applied count
    pub window_applied: i32,
}

/// Records that a learning was applied (confirmed useful).
///
/// This updates both global (times_applied, last_applied_at) and
/// window-specific (window_applied) statistics used by the UCB
/// bandit ranking algorithm.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning that was applied
///
/// # Returns
///
/// `ApplyLearningResult` with updated statistics.
///
/// # Errors
///
/// Returns `TaskMgrError::NotFound` if the learning doesn't exist.
pub fn apply_learning(conn: &Connection, learning_id: i64) -> TaskMgrResult<ApplyLearningResult> {
    // Verify learning exists and get its title
    let (title,): (String,) = conn
        .query_row(
            "SELECT title FROM learnings WHERE id = ?1",
            [learning_id],
            |row| Ok((row.get(0)?,)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                crate::TaskMgrError::learning_not_found(learning_id.to_string())
            }
            _ => e.into(),
        })?;

    // Record the application
    record_learning_applied(conn, learning_id)?;

    // Get updated stats
    let (times_applied, window_applied): (i32, i32) = conn.query_row(
        "SELECT times_applied, COALESCE(window_applied, 0) FROM learnings WHERE id = ?1",
        [learning_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    Ok(ApplyLearningResult {
        learning_id,
        title,
        times_applied,
        window_applied,
    })
}

/// Format apply-learning result as text.
pub fn format_text(result: &ApplyLearningResult) -> String {
    format!(
        "Applied: {} (ID: {}) - total applications: {}, window: {}",
        result.title, result.learning_id, result.times_applied, result.window_applied
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, migrations::run_migrations, open_connection};
    use crate::learnings::crud::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};
    use tempfile::TempDir;

    fn setup_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    #[test]
    fn test_apply_learning_success() {
        let (_temp_dir, conn) = setup_db();

        // Create a learning
        let params = RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: "Test learning".to_string(),
            content: "Content".to_string(),
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
        let learn_result = record_learning(&conn, params).unwrap();

        // Apply it
        let result = apply_learning(&conn, learn_result.learning_id).unwrap();

        assert_eq!(result.learning_id, learn_result.learning_id);
        assert_eq!(result.title, "Test learning");
        assert_eq!(result.times_applied, 1);
        assert_eq!(result.window_applied, 1);

        // Apply again
        let result2 = apply_learning(&conn, learn_result.learning_id).unwrap();
        assert_eq!(result2.times_applied, 2);
        assert_eq!(result2.window_applied, 2);
    }

    #[test]
    fn test_apply_learning_not_found() {
        let (_temp_dir, conn) = setup_db();

        let result = apply_learning(&conn, 9999);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_format_text() {
        let result = ApplyLearningResult {
            learning_id: 42,
            title: "Useful pattern".to_string(),
            times_applied: 5,
            window_applied: 2,
        };

        let text = format_text(&result);
        assert!(text.contains("Useful pattern"));
        assert!(text.contains("42"));
        assert!(text.contains("5"));
        assert!(text.contains("2"));
    }
}
