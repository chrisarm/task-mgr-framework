//! Skip command — defer tasks without marking them failed.

use rusqlite::Connection;
use serde::Serialize;

use crate::lifecycle::{TaskLifecycle, TransitionChange, TransitionIntent, TransitionSource};
use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

/// Result of skipping a single task.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSkipResult {
    pub task_id: String,
    pub previous_status: TaskStatus,
    pub reason: String,
    pub was_already_skipped: bool,
}

/// Result of skipping multiple tasks.
#[derive(Debug, Clone, Serialize)]
pub struct SkipResult {
    pub tasks: Vec<TaskSkipResult>,
    pub skipped_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// Skip one or more tasks with a reason.
///
/// Pre-validates all tasks before any writes to preserve all-or-nothing semantics.
pub fn skip(
    conn: &mut Connection,
    task_ids: &[String],
    reason: &str,
    run_id: Option<&str>,
) -> TaskMgrResult<SkipResult> {
    for id in task_ids {
        ensure_skippable(conn, id)?;
    }

    let intents: Vec<TransitionIntent> = task_ids
        .iter()
        .map(|id| TransitionIntent {
            task_id: id.clone(),
            change: TransitionChange::Skipped,
            source: TransitionSource::Operator,
            reason: Some(reason.to_string()),
            fail_status: None,
            audit_note: None,
        })
        .collect();

    let mut lc = match run_id {
        Some(rid) => TaskLifecycle::with_run(conn, rid),
        None => TaskLifecycle::new(conn),
    };
    let outcomes = lc.apply(&intents);

    let tasks: Vec<TaskSkipResult> = outcomes
        .into_iter()
        .zip(task_ids.iter())
        .map(|(o, id)| TaskSkipResult {
            task_id: id.clone(),
            previous_status: o.previous.unwrap_or(TaskStatus::InProgress),
            reason: reason.to_string(),
            was_already_skipped: o.previous == Some(TaskStatus::Skipped),
        })
        .collect();

    Ok(SkipResult {
        skipped_count: tasks.len(),
        tasks,
        run_id: run_id.map(String::from),
    })
}

/// Validate that a task exists and is not Done before writing.
fn ensure_skippable(conn: &Connection, id: &str) -> TaskMgrResult<()> {
    let s: String = conn
        .query_row("SELECT status FROM tasks WHERE id = ?", [id], |r| r.get(0))
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(id),
            _ => e.into(),
        })?;
    if s.parse::<TaskStatus>()? == TaskStatus::Done {
        return Err(TaskMgrError::invalid_state(
            "Task",
            id,
            "todo or in_progress",
            "done",
        ));
    }
    Ok(())
}

/// Format skip result as human-readable text.
#[must_use]
pub fn format_text(result: &SkipResult) -> String {
    let mut output = String::new();

    if result.tasks.len() == 1 {
        let task = &result.tasks[0];
        if task.was_already_skipped {
            output.push_str(&format!(
                "Task {} was already skipped.\nUpdated reason: {}\n",
                task.task_id, task.reason
            ));
        } else {
            output.push_str(&format!(
                "Skipped task {} (was {}).\nReason: {}\n",
                task.task_id, task.previous_status, task.reason
            ));
        }
    } else {
        output.push_str(&format!("Skipped {} task(s).\n", result.skipped_count));
        for task in &result.tasks {
            output.push_str(&format!(
                "  - {} (was {})",
                task.task_id, task.previous_status
            ));
            if task.was_already_skipped {
                output.push_str(" [already skipped]");
            }
            output.push('\n');
        }
        if !result.tasks.is_empty() {
            output.push_str(&format!("Reason: {}\n", result.tasks[0].reason));
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
    fn test_skip_todo_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "todo");

        let result = skip(
            &mut conn,
            &["US-001".to_string()],
            "Deferring to next sprint",
            None,
        )
        .unwrap();

        assert_eq!(result.tasks.len(), 1);
        let task = &result.tasks[0];
        assert_eq!(task.task_id, "US-001");
        assert_eq!(task.previous_status, TaskStatus::Todo);
        assert_eq!(task.reason, "Deferring to next sprint");
        assert!(!task.was_already_skipped);

        // Verify status was updated
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "skipped");
    }

    #[test]
    fn test_skip_in_progress_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-002", "in_progress");

        let result = skip(&mut conn, &["US-002".to_string()], "Need more info", None).unwrap();

        let task = &result.tasks[0];
        assert_eq!(task.previous_status, TaskStatus::InProgress);
        assert!(!task.was_already_skipped);
    }

    #[test]
    fn test_skip_already_skipped_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-003", "skipped");

        let result = skip(&mut conn, &["US-003".to_string()], "New reason", None).unwrap();

        let task = &result.tasks[0];
        assert_eq!(task.previous_status, TaskStatus::Skipped);
        assert!(task.was_already_skipped);
    }

    #[test]
    fn test_skip_done_task_fails() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-004", "done");

        let result = skip(&mut conn, &["US-004".to_string()], "Should fail", None);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidState { .. }) => {}
            _ => panic!("Expected InvalidState error"),
        }
    }

    #[test]
    fn test_skip_nonexistent_task() {
        let (_dir, mut conn) = setup_test_db();

        let result = skip(&mut conn, &["NONEXISTENT".to_string()], "Should fail", None);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_skip_preserves_existing_notes() {
        let (_dir, mut conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, notes) VALUES ('US-005', 'Test', 'todo', 10, 'Existing notes')",
            [],
        )
        .unwrap();

        skip(&mut conn, &["US-005".to_string()], "New skip reason", None).unwrap();

        let notes: String = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'US-005'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(notes.contains("Existing notes"));
        assert!(notes.contains("[SKIPPED] New skip reason"));
    }

    #[test]
    fn test_skip_does_not_increment_error_count() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-006", "todo");

        skip(&mut conn, &["US-006".to_string()], "Skip reason", None).unwrap();

        let error_count: i32 = conn
            .query_row(
                "SELECT error_count FROM tasks WHERE id = 'US-006'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(error_count, 0);
    }

    #[test]
    fn test_skip_with_run_id() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-007", "in_progress");

        // Create a run and run_task entry
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-123', 'active')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-123', 'US-007', 'started', 1)",
            [],
        )
        .unwrap();

        skip(
            &mut conn,
            &["US-007".to_string()],
            "Skip with run",
            Some("run-123"),
        )
        .unwrap();

        // Verify run_tasks was updated
        let run_task_status: String = conn
            .query_row(
                "SELECT status FROM run_tasks WHERE run_id = 'run-123' AND task_id = 'US-007'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_task_status, "skipped");
    }

    #[test]
    fn test_skip_multiple_tasks() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-008", "todo");
        insert_test_task(&conn, "US-009", "in_progress");
        insert_test_task(&conn, "US-010", "todo");

        let result = skip(
            &mut conn,
            &[
                "US-008".to_string(),
                "US-009".to_string(),
                "US-010".to_string(),
            ],
            "Batch skip",
            None,
        )
        .unwrap();

        assert_eq!(result.skipped_count, 3);
        assert_eq!(result.tasks.len(), 3);

        // Verify all tasks are skipped
        for task_id in ["US-008", "US-009", "US-010"] {
            let status: String = conn
                .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(status, "skipped");
        }
    }

    #[test]
    fn test_skip_multiple_rolls_back_on_error() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-011", "todo");
        // US-012 doesn't exist

        let result = skip(
            &mut conn,
            &["US-011".to_string(), "US-012".to_string()],
            "Should fail",
            None,
        );

        // Should fail because US-012 doesn't exist
        assert!(result.is_err());

        // US-011 remains todo — pre-validation fails on US-012 before any writes
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-011'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "todo");
    }

    #[test]
    fn test_format_text_single_task() {
        let result = SkipResult {
            tasks: vec![TaskSkipResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::Todo,
                reason: "Deferring".to_string(),
                was_already_skipped: false,
            }],
            skipped_count: 1,
            run_id: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Skipped task US-001"));
        assert!(text.contains("was todo"));
        assert!(text.contains("Reason: Deferring"));
    }

    #[test]
    fn test_format_text_multiple_tasks() {
        let result = SkipResult {
            tasks: vec![
                TaskSkipResult {
                    task_id: "US-001".to_string(),
                    previous_status: TaskStatus::Todo,
                    reason: "Batch reason".to_string(),
                    was_already_skipped: false,
                },
                TaskSkipResult {
                    task_id: "US-002".to_string(),
                    previous_status: TaskStatus::InProgress,
                    reason: "Batch reason".to_string(),
                    was_already_skipped: false,
                },
            ],
            skipped_count: 2,
            run_id: Some("run-123".to_string()),
        };

        let text = format_text(&result);
        assert!(text.contains("Skipped 2 task(s)"));
        assert!(text.contains("US-001"));
        assert!(text.contains("US-002"));
        assert!(text.contains("Run: run-123"));
    }

    #[test]
    fn test_format_text_already_skipped() {
        let result = SkipResult {
            tasks: vec![TaskSkipResult {
                task_id: "US-002".to_string(),
                previous_status: TaskStatus::Skipped,
                reason: "Updated reason".to_string(),
                was_already_skipped: true,
            }],
            skipped_count: 1,
            run_id: None,
        };

        let text = format_text(&result);
        assert!(text.contains("was already skipped"));
        assert!(text.contains("Updated reason"));
    }
}
