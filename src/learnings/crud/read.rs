//! Read operations for learnings.
//!
//! This module provides functions for retrieving learnings from the database.

use rusqlite::Connection;

use crate::models::Learning;
use crate::TaskMgrResult;

/// Gets a learning by ID.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning to retrieve
///
/// # Returns
///
/// The learning if found, or None if not found.
pub fn get_learning(conn: &Connection, learning_id: i64) -> TaskMgrResult<Option<Learning>> {
    let result = conn.query_row(
        r#"
        SELECT
            id, created_at, task_id, run_id, outcome, title, content,
            root_cause, solution,
            applies_to_files, applies_to_task_types, applies_to_errors,
            confidence, times_shown, times_applied, last_shown_at, last_applied_at
        FROM learnings
        WHERE id = ?1
        "#,
        [learning_id],
        |row| {
            Learning::try_from(row)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        },
    );

    match result {
        Ok(learning) => Ok(Some(learning)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Gets tags for a learning.
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `learning_id` - ID of the learning
///
/// # Returns
///
/// Vector of tag strings.
pub fn get_learning_tags(conn: &Connection, learning_id: i64) -> TaskMgrResult<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT tag FROM learning_tags WHERE learning_id = ?1 ORDER BY tag")?;
    let tags: Vec<String> = stmt
        .query_map([learning_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(tags)
}
