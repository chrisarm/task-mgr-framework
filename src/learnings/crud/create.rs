//! Create operations for learnings.
//!
//! This module provides the `record_learning` function for creating new learnings.

use rusqlite::Connection;

use super::types::{RecordLearningParams, RecordLearningResult};
use crate::TaskMgrResult;

/// Records a new learning in the database.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `params` - Learning parameters
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
pub fn record_learning(
    conn: &Connection,
    params: RecordLearningParams,
) -> TaskMgrResult<RecordLearningResult> {
    // Convert optional Vec<String> to JSON strings for storage
    let applies_to_files_json = params
        .applies_to_files
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default());
    let applies_to_task_types_json = params
        .applies_to_task_types
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default());
    let applies_to_errors_json = params
        .applies_to_errors
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default());

    // Insert the learning
    conn.execute(
        r#"
        INSERT INTO learnings (
            task_id, run_id, outcome, title, content,
            root_cause, solution,
            applies_to_files, applies_to_task_types, applies_to_errors,
            confidence
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5,
            ?6, ?7,
            ?8, ?9, ?10,
            ?11
        )
        "#,
        rusqlite::params![
            params.task_id,
            params.run_id,
            params.outcome.as_db_str(),
            params.title,
            params.content,
            params.root_cause,
            params.solution,
            applies_to_files_json,
            applies_to_task_types_json,
            applies_to_errors_json,
            params.confidence.as_db_str(),
        ],
    )?;

    // Get the learning ID
    let learning_id = conn.last_insert_rowid();

    // Insert tags if provided
    let tags_added = if let Some(ref tags) = params.tags {
        for tag in tags {
            conn.execute(
                "INSERT INTO learning_tags (learning_id, tag) VALUES (?1, ?2)",
                rusqlite::params![learning_id, tag],
            )?;
        }
        tags.len()
    } else {
        0
    };

    Ok(RecordLearningResult {
        learning_id,
        title: params.title,
        outcome: params.outcome,
        tags_added,
    })
}
