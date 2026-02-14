//! Update operations for learnings.
//!
//! This module provides the `edit_learning` function for modifying existing learnings.

use rusqlite::Connection;

use super::read::get_learning;
use super::types::{EditLearningParams, EditLearningResult};
use crate::TaskMgrResult;

/// Edits an existing learning in the database.
///
/// Only fields that are `Some` in the params will be updated.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning to edit
/// * `params` - Edit parameters
///
/// # Returns
///
/// Result containing the edit result with updated fields list.
///
/// # Errors
///
/// Returns an error if:
/// - Learning doesn't exist
/// - Database update fails
pub fn edit_learning(
    conn: &Connection,
    learning_id: i64,
    params: EditLearningParams,
) -> TaskMgrResult<EditLearningResult> {
    // First get the learning to verify it exists and get current values
    let learning = get_learning(conn, learning_id)?
        .ok_or_else(|| crate::TaskMgrError::learning_not_found(learning_id.to_string()))?;

    let mut updated_fields: Vec<String> = Vec::new();

    // Update title if provided
    if let Some(ref new_title) = params.title {
        conn.execute(
            "UPDATE learnings SET title = ?1 WHERE id = ?2",
            rusqlite::params![new_title, learning_id],
        )?;
        updated_fields.push("title".to_string());
    }

    // Update content if provided
    if let Some(ref new_content) = params.content {
        conn.execute(
            "UPDATE learnings SET content = ?1 WHERE id = ?2",
            rusqlite::params![new_content, learning_id],
        )?;
        updated_fields.push("content".to_string());
    }

    // Update solution if provided
    if let Some(ref new_solution) = params.solution {
        conn.execute(
            "UPDATE learnings SET solution = ?1 WHERE id = ?2",
            rusqlite::params![new_solution, learning_id],
        )?;
        updated_fields.push("solution".to_string());
    }

    // Update root_cause if provided
    if let Some(ref new_root_cause) = params.root_cause {
        conn.execute(
            "UPDATE learnings SET root_cause = ?1 WHERE id = ?2",
            rusqlite::params![new_root_cause, learning_id],
        )?;
        updated_fields.push("root_cause".to_string());
    }

    // Update confidence if provided
    if let Some(new_confidence) = params.confidence {
        conn.execute(
            "UPDATE learnings SET confidence = ?1 WHERE id = ?2",
            rusqlite::params![new_confidence.as_db_str(), learning_id],
        )?;
        updated_fields.push("confidence".to_string());
    }

    // Handle file pattern modifications
    let files_modified = params.add_files.is_some() || params.remove_files.is_some();
    if files_modified {
        // Get current files
        let mut current_files: Vec<String> = learning.applies_to_files.unwrap_or_default();

        // Remove files first
        if let Some(ref remove) = params.remove_files {
            current_files.retain(|f| !remove.contains(f));
        }

        // Add new files
        if let Some(ref add) = params.add_files {
            for file in add {
                if !current_files.contains(file) {
                    current_files.push(file.clone());
                }
            }
        }

        // Store as JSON
        let files_json = if current_files.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&current_files).unwrap_or_default())
        };

        conn.execute(
            "UPDATE learnings SET applies_to_files = ?1 WHERE id = ?2",
            rusqlite::params![files_json, learning_id],
        )?;
        updated_fields.push("applies_to_files".to_string());
    }

    // Handle tag modifications
    let mut tags_added = 0;
    let mut tags_removed = 0;

    // Remove tags first
    if let Some(ref remove_tags) = params.remove_tags {
        for tag in remove_tags {
            let rows = conn.execute(
                "DELETE FROM learning_tags WHERE learning_id = ?1 AND tag = ?2",
                rusqlite::params![learning_id, tag],
            )?;
            tags_removed += rows;
        }
    }

    // Add new tags
    if let Some(ref add_tags) = params.add_tags {
        for tag in add_tags {
            // Use INSERT OR IGNORE to handle duplicates gracefully
            let rows = conn.execute(
                "INSERT OR IGNORE INTO learning_tags (learning_id, tag) VALUES (?1, ?2)",
                rusqlite::params![learning_id, tag],
            )?;
            if rows > 0 {
                tags_added += 1;
            }
        }
    }

    if tags_added > 0 || tags_removed > 0 {
        updated_fields.push("tags".to_string());
    }

    // Get final title (may have been updated)
    let final_title = params.title.unwrap_or(learning.title);

    Ok(EditLearningResult {
        learning_id,
        title: final_title,
        updated_fields,
        tags_added,
        tags_removed,
    })
}
