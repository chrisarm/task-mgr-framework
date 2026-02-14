//! Unblock and Unskip command implementations.
//!
//! The unblock command returns a blocked task to todo status for retry.
//! The unskip command returns a skipped task to todo status for retry.

use rusqlite::Connection;
use serde::Serialize;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

/// Result of unblocking a task.
#[derive(Debug, Clone, Serialize)]
pub struct UnblockResult {
    /// The task that was unblocked
    pub task_id: String,
    /// Previous status (should be 'blocked')
    pub previous_status: TaskStatus,
    /// New status (always 'todo')
    pub new_status: TaskStatus,
    /// The error that was cleared
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleared_error: Option<String>,
    /// Note added for audit trail
    pub audit_note: String,
}

/// Result of unskipping a task.
#[derive(Debug, Clone, Serialize)]
pub struct UnskipResult {
    /// The task that was unskipped
    pub task_id: String,
    /// Previous status (should be 'skipped')
    pub previous_status: TaskStatus,
    /// New status (always 'todo')
    pub new_status: TaskStatus,
    /// Note added for audit trail
    pub audit_note: String,
}

/// Return a blocked task to todo status for retry.
///
/// # Arguments
/// * `conn` - Database connection
/// * `task_id` - ID of the task to unblock
///
/// # Returns
/// * `Ok(UnblockResult)` - Information about the unblocked task
/// * `Err(TaskMgrError)` - If task not found or not in blocked status
///
/// # Status Transition
/// Changes status from `blocked` to `todo` and clears `last_error`.
pub fn unblock(conn: &Connection, task_id: &str) -> TaskMgrResult<UnblockResult> {
    // Query current task status, notes, and last_error
    let (status_str, current_notes, last_error): (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT status, notes, last_error FROM tasks WHERE id = ?",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            _ => TaskMgrError::from(e),
        })?;

    let previous_status: TaskStatus = status_str.parse()?;

    // Validate that task is in blocked status
    if previous_status != TaskStatus::Blocked {
        return Err(TaskMgrError::invalid_state(
            "Task",
            task_id,
            "blocked",
            previous_status.to_string(),
        ));
    }

    // Build audit note
    let audit_note = "[UNBLOCKED] Returned to todo from blocked status".to_string();
    let new_notes = match &current_notes {
        Some(existing) if !existing.is_empty() => format!("{}\n\n{}", existing, audit_note),
        _ => audit_note.clone(),
    };

    // Update task: status to todo, clear last_error, add audit note
    conn.execute(
        "UPDATE tasks SET status = ?, last_error = NULL, notes = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![TaskStatus::Todo.as_db_str(), new_notes, task_id],
    )?;

    Ok(UnblockResult {
        task_id: task_id.to_string(),
        previous_status,
        new_status: TaskStatus::Todo,
        cleared_error: last_error,
        audit_note,
    })
}

/// Return a skipped task to todo status for retry.
///
/// # Arguments
/// * `conn` - Database connection
/// * `task_id` - ID of the task to unskip
///
/// # Returns
/// * `Ok(UnskipResult)` - Information about the unskipped task
/// * `Err(TaskMgrError)` - If task not found or not in skipped status
///
/// # Status Transition
/// Changes status from `skipped` to `todo`.
pub fn unskip(conn: &Connection, task_id: &str) -> TaskMgrResult<UnskipResult> {
    // Query current task status and notes
    let (status_str, current_notes): (String, Option<String>) = conn
        .query_row(
            "SELECT status, notes FROM tasks WHERE id = ?",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            _ => TaskMgrError::from(e),
        })?;

    let previous_status: TaskStatus = status_str.parse()?;

    // Validate that task is in skipped status
    if previous_status != TaskStatus::Skipped {
        return Err(TaskMgrError::invalid_state(
            "Task",
            task_id,
            "skipped",
            previous_status.to_string(),
        ));
    }

    // Build audit note
    let audit_note = "[UNSKIPPED] Returned to todo from skipped status".to_string();
    let new_notes = match &current_notes {
        Some(existing) if !existing.is_empty() => format!("{}\n\n{}", existing, audit_note),
        _ => audit_note.clone(),
    };

    // Update task: status to todo, add audit note
    conn.execute(
        "UPDATE tasks SET status = ?, notes = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![TaskStatus::Todo.as_db_str(), new_notes, task_id],
    )?;

    Ok(UnskipResult {
        task_id: task_id.to_string(),
        previous_status,
        new_status: TaskStatus::Todo,
        audit_note,
    })
}

/// Format unblock result as human-readable text.
#[must_use]
pub fn format_unblock_text(result: &UnblockResult) -> String {
    let mut output = format!(
        "Unblocked task {} (was {}, now {}).\n",
        result.task_id, result.previous_status, result.new_status
    );

    if let Some(ref error) = result.cleared_error {
        output.push_str(&format!("Cleared error: {}\n", error));
    }

    output.push_str("Task is now available for selection.\n");

    output
}

/// Format unskip result as human-readable text.
#[must_use]
pub fn format_unskip_text(result: &UnskipResult) -> String {
    format!(
        "Unskipped task {} (was {}, now {}).\nTask is now available for selection.\n",
        result.task_id, result.previous_status, result.new_status
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_test_task(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, error_count) VALUES (?, 'Test Task', ?, 10, 0)",
            rusqlite::params![id, status],
        )
        .unwrap();
    }

    // ============ Unblock tests ============

    #[test]
    fn test_unblock_blocked_task() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "blocked");

        // Set last_error
        conn.execute(
            "UPDATE tasks SET last_error = 'Missing dependency' WHERE id = 'US-001'",
            [],
        )
        .unwrap();

        let result = unblock(&conn, "US-001").unwrap();

        assert_eq!(result.task_id, "US-001");
        assert_eq!(result.previous_status, TaskStatus::Blocked);
        assert_eq!(result.new_status, TaskStatus::Todo);
        assert_eq!(result.cleared_error, Some("Missing dependency".to_string()));
        assert!(result.audit_note.contains("UNBLOCKED"));

        // Verify status was updated in DB
        let (status, last_error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, last_error FROM tasks WHERE id = 'US-001'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "todo");
        assert!(last_error.is_none());
    }

    #[test]
    fn test_unblock_preserves_existing_notes() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, notes, error_count) VALUES ('US-002', 'Test', 'blocked', 10, 'Existing notes', 0)",
            [],
        )
        .unwrap();

        unblock(&conn, "US-002").unwrap();

        let notes: String = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'US-002'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(notes.contains("Existing notes"));
        assert!(notes.contains("[UNBLOCKED]"));
    }

    #[test]
    fn test_unblock_todo_task_fails() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-003", "todo");

        let result = unblock(&conn, "US-003");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "blocked");
                assert_eq!(actual, "todo");
            }
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_unblock_done_task_fails() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-004", "done");

        let result = unblock(&conn, "US-004");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "blocked");
                assert_eq!(actual, "done");
            }
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_unblock_skipped_task_fails() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-005", "skipped");

        let result = unblock(&conn, "US-005");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "blocked");
                assert_eq!(actual, "skipped");
            }
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_unblock_nonexistent_task_fails() {
        let (_dir, conn) = setup_test_db();

        let result = unblock(&conn, "NONEXISTENT");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    // ============ Unskip tests ============

    #[test]
    fn test_unskip_skipped_task() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-010", "skipped");

        let result = unskip(&conn, "US-010").unwrap();

        assert_eq!(result.task_id, "US-010");
        assert_eq!(result.previous_status, TaskStatus::Skipped);
        assert_eq!(result.new_status, TaskStatus::Todo);
        assert!(result.audit_note.contains("UNSKIPPED"));

        // Verify status was updated in DB
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-010'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "todo");
    }

    #[test]
    fn test_unskip_preserves_existing_notes() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, notes, error_count) VALUES ('US-011', 'Test', 'skipped', 10, 'Previous notes', 0)",
            [],
        )
        .unwrap();

        unskip(&conn, "US-011").unwrap();

        let notes: String = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'US-011'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(notes.contains("Previous notes"));
        assert!(notes.contains("[UNSKIPPED]"));
    }

    #[test]
    fn test_unskip_todo_task_fails() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-012", "todo");

        let result = unskip(&conn, "US-012");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "skipped");
                assert_eq!(actual, "todo");
            }
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_unskip_blocked_task_fails() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-013", "blocked");

        let result = unskip(&conn, "US-013");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "skipped");
                assert_eq!(actual, "blocked");
            }
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_unskip_done_task_fails() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-014", "done");

        let result = unskip(&conn, "US-014");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "skipped");
                assert_eq!(actual, "done");
            }
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_unskip_nonexistent_task_fails() {
        let (_dir, conn) = setup_test_db();

        let result = unskip(&conn, "NONEXISTENT");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    // ============ Format tests ============

    #[test]
    fn test_format_unblock_text() {
        let result = UnblockResult {
            task_id: "US-001".to_string(),
            previous_status: TaskStatus::Blocked,
            new_status: TaskStatus::Todo,
            cleared_error: Some("Missing dependency".to_string()),
            audit_note: "[UNBLOCKED] Returned to todo from blocked status".to_string(),
        };

        let text = format_unblock_text(&result);
        assert!(text.contains("Unblocked task US-001"));
        assert!(text.contains("was blocked"));
        assert!(text.contains("now todo"));
        assert!(text.contains("Cleared error: Missing dependency"));
        assert!(text.contains("available for selection"));
    }

    #[test]
    fn test_format_unblock_text_no_error() {
        let result = UnblockResult {
            task_id: "US-002".to_string(),
            previous_status: TaskStatus::Blocked,
            new_status: TaskStatus::Todo,
            cleared_error: None,
            audit_note: "[UNBLOCKED] Returned to todo from blocked status".to_string(),
        };

        let text = format_unblock_text(&result);
        assert!(text.contains("Unblocked task US-002"));
        assert!(!text.contains("Cleared error:"));
    }

    #[test]
    fn test_format_unskip_text() {
        let result = UnskipResult {
            task_id: "US-003".to_string(),
            previous_status: TaskStatus::Skipped,
            new_status: TaskStatus::Todo,
            audit_note: "[UNSKIPPED] Returned to todo from skipped status".to_string(),
        };

        let text = format_unskip_text(&result);
        assert!(text.contains("Unskipped task US-003"));
        assert!(text.contains("was skipped"));
        assert!(text.contains("now todo"));
        assert!(text.contains("available for selection"));
    }
}
