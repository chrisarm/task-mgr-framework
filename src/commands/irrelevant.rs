//! Irrelevant command implementation.
//!
//! The irrelevant command marks one or more tasks as no longer needed due to
//! changed requirements or learnings. This is a deliberate decision that a task
//! is no longer needed - distinct from failure or skipping.
//!
//! Irrelevant tasks are permanently excluded from task selection.

use rusqlite::Connection;
use serde::Serialize;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

/// Result of marking a single task as irrelevant.
#[derive(Debug, Clone, Serialize)]
pub struct TaskIrrelevantResult {
    /// The task that was marked irrelevant
    pub task_id: String,
    /// Previous status before marking irrelevant
    pub previous_status: TaskStatus,
    /// Reason for marking irrelevant
    pub reason: String,
    /// Learning ID that made this task irrelevant (if provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learning_id: Option<i64>,
    /// Whether the task was already irrelevant
    pub was_already_irrelevant: bool,
}

/// Result of marking multiple tasks as irrelevant.
#[derive(Debug, Clone, Serialize)]
pub struct IrrelevantResult {
    /// Results for each task
    pub tasks: Vec<TaskIrrelevantResult>,
    /// Number of tasks marked as irrelevant
    pub irrelevant_count: usize,
    /// Run ID if tracking was enabled
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// Mark one or more tasks as irrelevant with a reason.
///
/// # Arguments
/// * `conn` - Database connection (mutable for transaction support)
/// * `task_ids` - IDs of tasks to mark as irrelevant
/// * `reason` - Reason why tasks are no longer relevant
/// * `run_id` - Optional run ID for tracking
/// * `learning_id` - Optional learning ID that made these tasks irrelevant
///
/// # Returns
/// * `Ok(IrrelevantResult)` - Information about the tasks
/// * `Err(TaskMgrError)` - If any task not found or already done
///
/// # Atomicity
/// When multiple task IDs are provided, all operations are wrapped in a
/// transaction. Either all tasks are marked as irrelevant, or none are (on error).
pub fn irrelevant(
    conn: &mut Connection,
    task_ids: &[String],
    reason: &str,
    run_id: Option<&str>,
    learning_id: Option<i64>,
) -> TaskMgrResult<IrrelevantResult> {
    // Wrap all operations in a transaction for atomicity
    let tx = conn.transaction()?;

    let mut results = Vec::with_capacity(task_ids.len());

    for task_id in task_ids {
        let result = irrelevant_single_task(&tx, task_id, reason, run_id, learning_id)?;
        results.push(result);
    }

    // Commit the transaction - all changes are atomic
    tx.commit()?;

    let irrelevant_count = results.len();

    Ok(IrrelevantResult {
        tasks: results,
        irrelevant_count,
        run_id: run_id.map(String::from),
    })
}

/// Mark a single task as irrelevant.
fn irrelevant_single_task(
    conn: &Connection,
    task_id: &str,
    reason: &str,
    run_id: Option<&str>,
    learning_id: Option<i64>,
) -> TaskMgrResult<TaskIrrelevantResult> {
    // Query current task status
    let (previous_status, current_notes): (String, Option<String>) = conn
        .query_row(
            "SELECT status, notes FROM tasks WHERE id = ?",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            _ => TaskMgrError::from(e),
        })?;

    let previous_status: TaskStatus = previous_status.parse()?;

    // Validate that task is not already done (completed successfully)
    if previous_status == TaskStatus::Done {
        return Err(TaskMgrError::invalid_state(
            "Task", task_id, "not done", "done",
        ));
    }

    // Check if already irrelevant
    let was_already_irrelevant = previous_status == TaskStatus::Irrelevant;

    // Build notes with irrelevant reason and optional learning reference
    let irrelevant_note = if let Some(lid) = learning_id {
        format!("[IRRELEVANT (learning #{})] {}", lid, reason)
    } else {
        format!("[IRRELEVANT] {}", reason)
    };

    let new_notes = match current_notes {
        Some(existing) if !existing.is_empty() => {
            format!("{}\n\n{}", existing, irrelevant_note)
        }
        _ => irrelevant_note,
    };

    // Update task status to irrelevant (do NOT increment error_count)
    conn.execute(
        "UPDATE tasks SET status = 'irrelevant', notes = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![new_notes, task_id],
    )?;

    // If run_id provided, update run_tasks if exists
    if let Some(run_id) = run_id {
        let run_task_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM run_tasks WHERE run_id = ? AND task_id = ?)",
                rusqlite::params![run_id, task_id],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if run_task_exists {
            let notes = if let Some(lid) = learning_id {
                format!("{} (learning #{})", reason, lid)
            } else {
                reason.to_string()
            };
            conn.execute(
                "UPDATE run_tasks SET status = 'skipped', notes = ?, ended_at = datetime('now') \
                 WHERE run_id = ? AND task_id = ?",
                rusqlite::params![notes, run_id, task_id],
            )?;
        }
    }

    Ok(TaskIrrelevantResult {
        task_id: task_id.to_string(),
        previous_status,
        reason: reason.to_string(),
        learning_id,
        was_already_irrelevant,
    })
}

/// Format irrelevant result as human-readable text.
#[must_use]
pub fn format_text(result: &IrrelevantResult) -> String {
    let mut output = String::new();

    if result.tasks.len() == 1 {
        // Single task output
        let task = &result.tasks[0];
        let learning_suffix = match task.learning_id {
            Some(lid) => format!(" (due to learning #{})", lid),
            None => String::new(),
        };

        if task.was_already_irrelevant {
            output.push_str(&format!(
                "Task {} was already marked as irrelevant.\nUpdated reason: {}{}\n",
                task.task_id, task.reason, learning_suffix
            ));
        } else {
            output.push_str(&format!(
                "Marked task {} as irrelevant (was {}).\nReason: {}{}\n",
                task.task_id, task.previous_status, task.reason, learning_suffix
            ));
        }
    } else {
        // Multiple tasks output
        output.push_str(&format!(
            "Marked {} task(s) as irrelevant.\n",
            result.irrelevant_count
        ));
        for task in &result.tasks {
            output.push_str(&format!(
                "  - {} (was {})",
                task.task_id, task.previous_status
            ));
            if task.was_already_irrelevant {
                output.push_str(" [already irrelevant]");
            }
            output.push('\n');
        }
        if !result.tasks.is_empty() {
            output.push_str(&format!("Reason: {}\n", result.tasks[0].reason));
            if let Some(lid) = result.tasks[0].learning_id {
                output.push_str(&format!("Due to learning: #{}\n", lid));
            }
        }
    }

    if let Some(ref rid) = result.run_id {
        output.push_str(&format!("Run: {}\n", rid));
    }

    output
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
            "INSERT INTO tasks (id, title, status, priority) VALUES (?, 'Test Task', ?, 10)",
            rusqlite::params![id, status],
        )
        .unwrap();
    }

    #[test]
    fn test_irrelevant_todo_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "todo");

        let result = irrelevant(
            &mut conn,
            &["US-001".to_string()],
            "Requirements changed",
            None,
            None,
        )
        .unwrap();

        assert_eq!(result.tasks.len(), 1);
        let task = &result.tasks[0];
        assert_eq!(task.task_id, "US-001");
        assert_eq!(task.previous_status, TaskStatus::Todo);
        assert_eq!(task.reason, "Requirements changed");
        assert!(!task.was_already_irrelevant);
        assert!(task.learning_id.is_none());

        // Verify status was updated
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "irrelevant");
    }

    #[test]
    fn test_irrelevant_in_progress_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-002", "in_progress");

        let result = irrelevant(
            &mut conn,
            &["US-002".to_string()],
            "Feature dropped",
            None,
            None,
        )
        .unwrap();

        let task = &result.tasks[0];
        assert_eq!(task.previous_status, TaskStatus::InProgress);
        assert!(!task.was_already_irrelevant);
    }

    #[test]
    fn test_irrelevant_blocked_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-003", "blocked");

        let result = irrelevant(
            &mut conn,
            &["US-003".to_string()],
            "No longer needed",
            None,
            None,
        )
        .unwrap();

        let task = &result.tasks[0];
        assert_eq!(task.previous_status, TaskStatus::Blocked);
    }

    #[test]
    fn test_irrelevant_already_irrelevant() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-004", "irrelevant");

        let result = irrelevant(
            &mut conn,
            &["US-004".to_string()],
            "Updated reason",
            None,
            None,
        )
        .unwrap();

        let task = &result.tasks[0];
        assert_eq!(task.previous_status, TaskStatus::Irrelevant);
        assert!(task.was_already_irrelevant);
    }

    #[test]
    fn test_irrelevant_done_task_fails() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-005", "done");

        let result = irrelevant(
            &mut conn,
            &["US-005".to_string()],
            "Should fail",
            None,
            None,
        );

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState { .. }) => {}
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_irrelevant_nonexistent_task() {
        let (_dir, mut conn) = setup_test_db();

        let result = irrelevant(
            &mut conn,
            &["NONEXISTENT".to_string()],
            "Should fail",
            None,
            None,
        );

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_irrelevant_with_learning_id() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-006", "todo");

        let result = irrelevant(
            &mut conn,
            &["US-006".to_string()],
            "Covered by learning",
            None,
            Some(42),
        )
        .unwrap();

        assert_eq!(result.tasks[0].learning_id, Some(42));

        // Verify notes contain learning reference
        let notes: String = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'US-006'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(notes.contains("learning #42"));
    }

    #[test]
    fn test_irrelevant_preserves_existing_notes() {
        let (_dir, mut conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, notes) VALUES ('US-007', 'Test', 'todo', 10, 'Existing notes')",
            [],
        )
        .unwrap();

        irrelevant(&mut conn, &["US-007".to_string()], "New reason", None, None).unwrap();

        let notes: String = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'US-007'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(notes.contains("Existing notes"));
        assert!(notes.contains("[IRRELEVANT] New reason"));
    }

    #[test]
    fn test_irrelevant_does_not_increment_error_count() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-008", "todo");

        irrelevant(&mut conn, &["US-008".to_string()], "Reason", None, None).unwrap();

        let error_count: i32 = conn
            .query_row(
                "SELECT error_count FROM tasks WHERE id = 'US-008'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(error_count, 0);
    }

    #[test]
    fn test_irrelevant_with_run_id() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-009", "in_progress");

        // Create a run and run_task entry
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-456', 'active')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-456', 'US-009', 'started', 1)",
            [],
        )
        .unwrap();

        irrelevant(
            &mut conn,
            &["US-009".to_string()],
            "No longer needed",
            Some("run-456"),
            None,
        )
        .unwrap();

        // Verify run_tasks was updated
        let run_task_status: String = conn
            .query_row(
                "SELECT status FROM run_tasks WHERE run_id = 'run-456' AND task_id = 'US-009'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_task_status, "skipped");
    }

    #[test]
    fn test_irrelevant_excluded_from_next_selection() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-010", "todo");
        insert_test_task(&conn, "US-011", "todo");

        // Mark US-010 as irrelevant
        irrelevant(&mut conn, &["US-010".to_string()], "Not needed", None, None).unwrap();

        // Query todo tasks (simulating what next command does)
        let todo_count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE status = 'todo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(todo_count, 1); // Only US-011 should be todo
    }

    #[test]
    fn test_irrelevant_multiple_tasks() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-012", "todo");
        insert_test_task(&conn, "US-013", "in_progress");
        insert_test_task(&conn, "US-014", "blocked");

        let result = irrelevant(
            &mut conn,
            &[
                "US-012".to_string(),
                "US-013".to_string(),
                "US-014".to_string(),
            ],
            "Batch irrelevant",
            None,
            None,
        )
        .unwrap();

        assert_eq!(result.irrelevant_count, 3);
        assert_eq!(result.tasks.len(), 3);

        // Verify all tasks are irrelevant
        for task_id in ["US-012", "US-013", "US-014"] {
            let status: String = conn
                .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(status, "irrelevant");
        }
    }

    #[test]
    fn test_irrelevant_multiple_rolls_back_on_error() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-015", "todo");
        // US-016 doesn't exist

        let result = irrelevant(
            &mut conn,
            &["US-015".to_string(), "US-016".to_string()],
            "Should fail",
            None,
            None,
        );

        // Should fail because US-016 doesn't exist
        assert!(result.is_err());

        // US-015 should be rolled back to todo (transaction failed)
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-015'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "todo");
    }

    #[test]
    fn test_format_text_single_task() {
        let result = IrrelevantResult {
            tasks: vec![TaskIrrelevantResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::Todo,
                reason: "Requirements changed".to_string(),
                learning_id: None,
                was_already_irrelevant: false,
            }],
            irrelevant_count: 1,
            run_id: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Marked task US-001 as irrelevant"));
        assert!(text.contains("was todo"));
        assert!(text.contains("Reason: Requirements changed"));
    }

    #[test]
    fn test_format_text_multiple_tasks() {
        let result = IrrelevantResult {
            tasks: vec![
                TaskIrrelevantResult {
                    task_id: "US-001".to_string(),
                    previous_status: TaskStatus::Todo,
                    reason: "Batch reason".to_string(),
                    learning_id: None,
                    was_already_irrelevant: false,
                },
                TaskIrrelevantResult {
                    task_id: "US-002".to_string(),
                    previous_status: TaskStatus::InProgress,
                    reason: "Batch reason".to_string(),
                    learning_id: None,
                    was_already_irrelevant: false,
                },
            ],
            irrelevant_count: 2,
            run_id: Some("run-123".to_string()),
        };

        let text = format_text(&result);
        assert!(text.contains("Marked 2 task(s) as irrelevant"));
        assert!(text.contains("US-001"));
        assert!(text.contains("US-002"));
        assert!(text.contains("Run: run-123"));
    }

    #[test]
    fn test_format_text_with_learning() {
        let result = IrrelevantResult {
            tasks: vec![TaskIrrelevantResult {
                task_id: "US-002".to_string(),
                previous_status: TaskStatus::Todo,
                reason: "Covered by learning".to_string(),
                learning_id: Some(42),
                was_already_irrelevant: false,
            }],
            irrelevant_count: 1,
            run_id: None,
        };

        let text = format_text(&result);
        assert!(text.contains("due to learning #42"));
    }

    #[test]
    fn test_format_text_already_irrelevant() {
        let result = IrrelevantResult {
            tasks: vec![TaskIrrelevantResult {
                task_id: "US-003".to_string(),
                previous_status: TaskStatus::Irrelevant,
                reason: "Updated reason".to_string(),
                learning_id: None,
                was_already_irrelevant: true,
            }],
            irrelevant_count: 1,
            run_id: None,
        };

        let text = format_text(&result);
        assert!(text.contains("was already marked as irrelevant"));
        assert!(text.contains("Updated reason"));
    }
}
