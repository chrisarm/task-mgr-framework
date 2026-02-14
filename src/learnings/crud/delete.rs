//! Delete operations for learnings.
//!
//! This module provides the `delete_learning` function for removing learnings.

use rusqlite::Connection;

use super::read::{get_learning, get_learning_tags};
use super::types::DeleteLearningResult;
use crate::TaskMgrResult;

/// Deletes a learning from the database.
///
/// This also deletes any associated tags via the ON DELETE CASCADE constraint.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning to delete
///
/// # Returns
///
/// Result containing the deleted learning's metadata.
///
/// # Errors
///
/// Returns an error if:
/// - Learning doesn't exist
/// - Database delete fails
pub fn delete_learning(conn: &Connection, learning_id: i64) -> TaskMgrResult<DeleteLearningResult> {
    // First get the learning to verify it exists and get its title
    let learning = get_learning(conn, learning_id)?
        .ok_or_else(|| crate::TaskMgrError::learning_not_found(learning_id.to_string()))?;

    // Count tags before deletion (they will be cascade deleted)
    let tags_count = get_learning_tags(conn, learning_id)?.len();

    // Delete the learning (tags are cascade deleted)
    let rows_deleted = conn.execute("DELETE FROM learnings WHERE id = ?1", [learning_id])?;

    if rows_deleted == 0 {
        return Err(crate::TaskMgrError::learning_not_found(
            learning_id.to_string(),
        ));
    }

    Ok(DeleteLearningResult {
        learning_id,
        title: learning.title,
        tags_deleted: tags_count,
    })
}
