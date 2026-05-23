//! Reset — return task(s) to todo for re-running. Routes through
//! `TaskLifecycle::apply` (PRD §6 Category A).

use rusqlite::Connection;
use serde::Serialize;

use crate::lifecycle::{TaskLifecycle, TransitionChange, TransitionIntent, TransitionSource};
use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

#[derive(Debug, Clone, Serialize)]
pub struct TaskResetResult {
    pub task_id: String,
    pub previous_status: TaskStatus,
    pub new_status: TaskStatus,
    pub audit_note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResetResult {
    pub tasks_reset: usize,
    pub tasks: Vec<TaskResetResult>,
    pub was_reset_all: bool,
}

/// Reset one task via the lifecycle service. The helper rejects same-status
/// (Todo) resets and increments `error_count`; failures are re-typed back
/// to legacy `NotFound` / `InvalidState` errors.
fn reset_via_lifecycle(conn: &mut Connection, task_id: &str) -> TaskMgrResult<TaskResetResult> {
    let intent = TransitionIntent {
        task_id: task_id.to_string(),
        change: TransitionChange::Reset,
        source: TransitionSource::Operator,
        reason: None,
        fail_status: None,
        audit_note: None,
    };
    let outcome = {
        let mut lc = TaskLifecycle::new(conn);
        lc.apply(&[intent]).remove(0)
    };
    if !outcome.applied {
        return Err(match outcome.previous {
            None => TaskMgrError::task_not_found(task_id),
            Some(TaskStatus::Todo) => {
                TaskMgrError::invalid_state("Task", task_id, "non-todo status", "todo")
            }
            _ => {
                let msg = match &outcome.reason {
                    Some(crate::lifecycle::TransitionRejectReason::DispatchFailed(m)) => m.clone(),
                    _ => "unknown lifecycle dispatch failure".to_string(),
                };
                TaskMgrError::lock_error_with_hint(
                    format!("reset dispatch failed for {task_id}: {msg}"),
                    "internal lifecycle dispatch error; check earlier stderr for details",
                )
            }
        });
    }
    let previous_status = outcome.previous.unwrap_or(TaskStatus::Done);
    Ok(TaskResetResult {
        task_id: task_id.to_string(),
        previous_status,
        new_status: TaskStatus::Todo,
        audit_note: format!("[RESET] Reset to todo from {previous_status} status"),
    })
}

/// Reset multiple tasks by their IDs. The first error short-circuits.
pub fn reset_tasks(conn: &mut Connection, task_ids: &[String]) -> TaskMgrResult<ResetResult> {
    let tasks = task_ids
        .iter()
        .map(|id| reset_via_lifecycle(conn, id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResetResult {
        tasks_reset: tasks.len(),
        tasks,
        was_reset_all: false,
    })
}

/// Reset all non-todo tasks. Reads the candidate ID set first, then
/// delegates each row through the lifecycle service.
pub fn reset_all_tasks(conn: &mut Connection) -> TaskMgrResult<ResetResult> {
    let ids: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM tasks WHERE status != 'todo' AND archived_at IS NULL ORDER BY priority ASC",
        )?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let tasks = ids
        .iter()
        .map(|id| reset_via_lifecycle(conn, id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResetResult {
        tasks_reset: tasks.len(),
        tasks,
        was_reset_all: true,
    })
}

pub fn count_resettable_tasks(conn: &Connection) -> TaskMgrResult<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE status != 'todo' AND archived_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

#[must_use]
pub fn format_text(result: &ResetResult) -> String {
    if result.tasks_reset == 0 {
        return if result.was_reset_all {
            "No tasks to reset (all tasks are already in todo status).\n".to_string()
        } else {
            "No tasks were reset.\n".to_string()
        };
    }
    let mut output = if result.was_reset_all {
        format!("Reset {} task(s) to todo status:\n", result.tasks_reset)
    } else {
        format!("Reset {} task(s):\n", result.tasks_reset)
    };
    for task in &result.tasks {
        output.push_str(&format!(
            "  {} (was {}) → todo\n",
            task.task_id, task.previous_status
        ));
    }
    output.push_str("\nAll reset tasks are now available for selection.\n");
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_schema, migrations::run_migrations, open_connection};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
        run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    fn insert_test_task(conn: &Connection, id: &str, status: &str, error_count: i64) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, error_count) VALUES (?, 'Test Task', ?, 10, ?)",
            rusqlite::params![id, status, error_count],
        )
        .unwrap();
    }

    // ============ reset_single_task tests ============

    #[test]
    fn test_reset_done_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "done", 0);

        let result = reset_via_lifecycle(&mut conn, "US-001").unwrap();

        assert_eq!(result.task_id, "US-001");
        assert_eq!(result.previous_status, TaskStatus::Done);
        assert_eq!(result.new_status, TaskStatus::Todo);
        assert!(result.audit_note.contains("RESET"));
        assert!(result.audit_note.contains("done"));

        // Verify database state
        let (status, error_count): (String, i64) = conn
            .query_row(
                "SELECT status, error_count FROM tasks WHERE id = 'US-001'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "todo");
        assert_eq!(error_count, 1); // Incremented
    }

    #[test]
    fn test_reset_blocked_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-002", "blocked", 2);

        // Set some values that should be cleared
        conn.execute(
            "UPDATE tasks SET started_at = datetime('now'), last_error = 'Some error' WHERE id = 'US-002'",
            [],
        )
        .unwrap();

        let result = reset_via_lifecycle(&mut conn, "US-002").unwrap();

        assert_eq!(result.previous_status, TaskStatus::Blocked);
        assert_eq!(result.new_status, TaskStatus::Todo);

        // Verify cleared fields
        let (status, started_at, last_error, error_count): (
            String,
            Option<String>,
            Option<String>,
            i64,
        ) = conn
            .query_row(
                "SELECT status, started_at, last_error, error_count FROM tasks WHERE id = 'US-002'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(status, "todo");
        assert!(started_at.is_none());
        assert!(last_error.is_none());
        assert_eq!(error_count, 3); // Was 2, incremented to 3
    }

    #[test]
    fn test_reset_in_progress_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-003", "in_progress", 0);

        let result = reset_via_lifecycle(&mut conn, "US-003").unwrap();

        assert_eq!(result.previous_status, TaskStatus::InProgress);
        assert_eq!(result.new_status, TaskStatus::Todo);
    }

    #[test]
    fn test_reset_skipped_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-004", "skipped", 1);

        let result = reset_via_lifecycle(&mut conn, "US-004").unwrap();

        assert_eq!(result.previous_status, TaskStatus::Skipped);
        assert_eq!(result.new_status, TaskStatus::Todo);
    }

    #[test]
    fn test_reset_irrelevant_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-005", "irrelevant", 0);

        let result = reset_via_lifecycle(&mut conn, "US-005").unwrap();

        assert_eq!(result.previous_status, TaskStatus::Irrelevant);
        assert_eq!(result.new_status, TaskStatus::Todo);
    }

    #[test]
    fn test_reset_todo_task_fails() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-006", "todo", 0);

        let result = reset_via_lifecycle(&mut conn, "US-006");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState {
                expected, actual, ..
            }) => {
                assert_eq!(expected, "non-todo status");
                assert_eq!(actual, "todo");
            }
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_reset_nonexistent_task_fails() {
        let (_dir, mut conn) = setup_test_db();

        let result = reset_via_lifecycle(&mut conn, "NONEXISTENT");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_reset_preserves_existing_notes() {
        let (_dir, mut conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, notes, error_count) VALUES ('US-007', 'Test', 'done', 10, 'Existing notes', 0)",
            [],
        )
        .unwrap();

        reset_via_lifecycle(&mut conn, "US-007").unwrap();

        let notes: String = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'US-007'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(notes.contains("Existing notes"));
        assert!(notes.contains("[RESET]"));
    }

    // ============ reset_tasks tests ============

    #[test]
    fn test_reset_multiple_tasks() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-010", "done", 0);
        insert_test_task(&conn, "US-011", "blocked", 1);
        insert_test_task(&conn, "US-012", "skipped", 0);

        let task_ids = vec![
            "US-010".to_string(),
            "US-011".to_string(),
            "US-012".to_string(),
        ];
        let result = reset_tasks(&mut conn, &task_ids).unwrap();

        assert_eq!(result.tasks_reset, 3);
        assert_eq!(result.tasks.len(), 3);
        assert!(!result.was_reset_all);

        // Verify all are now todo
        for task_id in &task_ids {
            let status: String = conn
                .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(status, "todo");
        }
    }

    #[test]
    fn test_reset_tasks_stops_on_error() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-020", "done", 0);
        // US-021 doesn't exist, should fail

        let task_ids = vec!["US-020".to_string(), "US-021".to_string()];
        let result = reset_tasks(&mut conn, &task_ids);

        // First task should succeed, but overall should fail
        assert!(result.is_err());

        // First task should have been reset before the error
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-020'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "todo");
    }

    // ============ reset_all_tasks tests ============

    #[test]
    fn test_reset_all_tasks() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-030", "done", 0);
        insert_test_task(&conn, "US-031", "blocked", 0);
        insert_test_task(&conn, "US-032", "todo", 0); // Should not be reset

        let result = reset_all_tasks(&mut conn).unwrap();

        assert_eq!(result.tasks_reset, 2);
        assert!(result.was_reset_all);

        // Verify all non-todo tasks are now todo
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE status = 'todo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_reset_all_when_all_todo() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-040", "todo", 0);
        insert_test_task(&conn, "US-041", "todo", 0);

        let result = reset_all_tasks(&mut conn).unwrap();

        assert_eq!(result.tasks_reset, 0);
        assert!(result.was_reset_all);
        assert!(result.tasks.is_empty());
    }

    // ============ count_resettable_tasks tests ============

    #[test]
    fn test_count_resettable_tasks() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-050", "done", 0);
        insert_test_task(&conn, "US-051", "blocked", 0);
        insert_test_task(&conn, "US-052", "todo", 0);
        insert_test_task(&conn, "US-053", "skipped", 0);

        let count = count_resettable_tasks(&conn).unwrap();

        assert_eq!(count, 3); // done, blocked, skipped (not todo)
    }

    // ============ format_text tests ============

    #[test]
    fn test_format_text_multiple_tasks() {
        let result = ResetResult {
            tasks_reset: 2,
            tasks: vec![
                TaskResetResult {
                    task_id: "US-001".to_string(),
                    previous_status: TaskStatus::Done,
                    new_status: TaskStatus::Todo,
                    audit_note: "[RESET] Reset to todo from done status".to_string(),
                },
                TaskResetResult {
                    task_id: "US-002".to_string(),
                    previous_status: TaskStatus::Blocked,
                    new_status: TaskStatus::Todo,
                    audit_note: "[RESET] Reset to todo from blocked status".to_string(),
                },
            ],
            was_reset_all: false,
        };

        let text = format_text(&result);
        assert!(text.contains("Reset 2 task(s)"));
        assert!(text.contains("US-001 (was done)"));
        assert!(text.contains("US-002 (was blocked)"));
        assert!(text.contains("available for selection"));
    }

    #[test]
    fn test_format_text_reset_all() {
        let result = ResetResult {
            tasks_reset: 1,
            tasks: vec![TaskResetResult {
                task_id: "US-003".to_string(),
                previous_status: TaskStatus::Done,
                new_status: TaskStatus::Todo,
                audit_note: "[RESET] Reset to todo from done status".to_string(),
            }],
            was_reset_all: true,
        };

        let text = format_text(&result);
        assert!(text.contains("Reset 1 task(s) to todo status"));
    }

    #[test]
    fn test_format_text_no_tasks_reset() {
        let result = ResetResult {
            tasks_reset: 0,
            tasks: Vec::new(),
            was_reset_all: true,
        };

        let text = format_text(&result);
        assert!(text.contains("No tasks to reset"));
        assert!(text.contains("all tasks are already in todo status"));
    }

    #[test]
    fn test_format_text_no_tasks_specific() {
        let result = ResetResult {
            tasks_reset: 0,
            tasks: Vec::new(),
            was_reset_all: false,
        };

        let text = format_text(&result);
        assert!(text.contains("No tasks were reset"));
    }
}
