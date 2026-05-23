//! Review command — interactive / batch cycle through blocked and skipped
//! tasks. The three status-mutation sites (auto-unblock, auto-unskip,
//! resolve-with-notes) route through `TaskLifecycle::apply` with
//! `audit_note` overrides for the custom `[AUTO-UNBLOCKED]` /
//! `[AUTO-UNSKIPPED]` / `[RESOLVED] ...` audit prefixes.

use rusqlite::Connection;
use serde::Serialize;

use crate::lifecycle::{TaskLifecycle, TransitionChange, TransitionIntent, TransitionSource};
use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

#[derive(Debug, Clone, Serialize)]
pub struct ReviewTask {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewResult {
    pub blocked_tasks: Vec<ReviewTask>,
    pub skipped_tasks: Vec<ReviewTask>,
    pub total_count: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions_taken: Vec<ReviewAction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewAction {
    pub task_id: String,
    pub action: ReviewActionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewActionType {
    Unblocked,
    Unskipped,
    Resolved,
    Skipped,
    Kept,
}

#[derive(Debug, Clone, Default)]
pub struct ReviewOptions {
    pub blocked_only: bool,
    pub skipped_only: bool,
    pub auto_unblock: bool,
}

/// List blocked / skipped tasks for review.
pub fn get_reviewable_tasks(
    conn: &Connection,
    options: &ReviewOptions,
) -> TaskMgrResult<ReviewResult> {
    let status_filter = if options.blocked_only {
        "status = 'blocked'"
    } else if options.skipped_only {
        "status = 'skipped'"
    } else {
        "status IN ('blocked', 'skipped')"
    };
    let query = format!(
        "SELECT id, title, description, status, last_error, notes, priority \
         FROM tasks WHERE {status_filter} AND archived_at IS NULL \
         ORDER BY priority ASC, id ASC"
    );
    let mut stmt = conn.prepare(&query)?;
    let rows = stmt
        .query_map([], |row| {
            let status_str: String = row.get(3)?;
            Ok(ReviewTask {
                id: row.get(0)?,
                title: row.get(1)?,
                description: row.get(2)?,
                status: status_str.parse().unwrap_or(TaskStatus::Blocked),
                last_error: row.get(4)?,
                notes: row.get(5)?,
                priority: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let (blocked_tasks, skipped_tasks): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|t| t.status == TaskStatus::Blocked);
    let total_count = blocked_tasks.len() + skipped_tasks.len();
    Ok(ReviewResult {
        blocked_tasks,
        skipped_tasks,
        total_count,
        actions_taken: Vec::new(),
    })
}

/// Auto-unblock all blocked / skipped tasks. The `audit_note` override
/// sidesteps the lifecycle service's expected-state checks so any blocked
/// or skipped row cycles back to Todo with the AUTO prefix.
pub fn auto_unblock_all(
    conn: &mut Connection,
    options: &ReviewOptions,
) -> TaskMgrResult<ReviewResult> {
    let tasks = get_reviewable_tasks(conn, options)?;
    let mut actions = Vec::with_capacity(tasks.total_count);
    for task in &tasks.blocked_tasks {
        apply_review_unblock(
            conn,
            &task.id,
            TransitionChange::Unblock,
            "[AUTO-UNBLOCKED] Returned to todo via review --auto",
        )?;
        actions.push(ReviewAction {
            task_id: task.id.clone(),
            action: ReviewActionType::Unblocked,
            notes: None,
        });
    }
    for task in &tasks.skipped_tasks {
        apply_review_unblock(
            conn,
            &task.id,
            TransitionChange::Unskip,
            "[AUTO-UNSKIPPED] Returned to todo via review --auto",
        )?;
        actions.push(ReviewAction {
            task_id: task.id.clone(),
            action: ReviewActionType::Unskipped,
            notes: None,
        });
    }
    Ok(ReviewResult {
        blocked_tasks: Vec::new(),
        skipped_tasks: Vec::new(),
        total_count: tasks.total_count,
        actions_taken: actions,
    })
}

/// Resolve a task with custom resolution notes (returns to Todo, clears
/// `last_error`).
pub fn resolve_task(
    conn: &mut Connection,
    task_id: &str,
    resolution_notes: &str,
) -> TaskMgrResult<ReviewAction> {
    let audit = format!("[RESOLVED] {resolution_notes}");
    apply_review_unblock(conn, task_id, TransitionChange::Unblock, &audit)?;
    Ok(ReviewAction {
        task_id: task_id.to_string(),
        action: ReviewActionType::Resolved,
        notes: Some(resolution_notes.to_string()),
    })
}

/// Shared apply() call for review's three Todo-bound paths. The
/// caller-supplied `audit_note` overrides the lifecycle service's default
/// prefix and bypasses the expected-state validation.
fn apply_review_unblock(
    conn: &mut Connection,
    task_id: &str,
    change: TransitionChange,
    audit: &str,
) -> TaskMgrResult<()> {
    let intent = TransitionIntent {
        task_id: task_id.to_string(),
        change,
        source: TransitionSource::Operator,
        reason: None,
        fail_status: None,
        audit_note: Some(audit.to_string()),
    };
    let outcomes = {
        let mut lc = TaskLifecycle::new(conn);
        lc.apply(&[intent])
    };
    let outcome = &outcomes[0];
    if !outcome.applied {
        return Err(match outcome.previous {
            None => TaskMgrError::task_not_found(task_id),
            _ => {
                let msg = match &outcome.reason {
                    Some(crate::lifecycle::TransitionRejectReason::DispatchFailed(m)) => m.clone(),
                    _ => "unknown lifecycle dispatch failure".to_string(),
                };
                TaskMgrError::lock_error_with_hint(
                    format!("review dispatch failed for {task_id}: {msg}"),
                    "internal lifecycle dispatch error; check earlier stderr for details",
                )
            }
        });
    }
    Ok(())
}

/// Format review result as human-readable text.
#[must_use]
pub fn format_text(result: &ReviewResult) -> String {
    let mut output = String::new();

    if result.total_count == 0 {
        output.push_str("No blocked or skipped tasks to review.\n");
        return output;
    }

    // Show actions taken if any
    if !result.actions_taken.is_empty() {
        output.push_str("Actions taken:\n");
        for action in &result.actions_taken {
            let action_str = match action.action {
                ReviewActionType::Unblocked => "unblocked",
                ReviewActionType::Unskipped => "unskipped",
                ReviewActionType::Resolved => "resolved",
                ReviewActionType::Skipped => "kept skipped",
                ReviewActionType::Kept => "kept",
            };
            if let Some(ref notes) = action.notes {
                output.push_str(&format!(
                    "  {} - {} ({})\n",
                    action.task_id, action_str, notes
                ));
            } else {
                output.push_str(&format!("  {} - {}\n", action.task_id, action_str));
            }
        }
        output.push('\n');
    }

    // Show remaining blocked tasks
    if !result.blocked_tasks.is_empty() {
        output.push_str(&format!(
            "Blocked tasks ({}):\n",
            result.blocked_tasks.len()
        ));
        for task in &result.blocked_tasks {
            output.push_str(&format!(
                "  [{}] {} (priority {})\n",
                task.id, task.title, task.priority
            ));
            if let Some(ref error) = task.last_error {
                output.push_str(&format!("    Error: {}\n", error));
            }
        }
        output.push('\n');
    }

    // Show remaining skipped tasks
    if !result.skipped_tasks.is_empty() {
        output.push_str(&format!(
            "Skipped tasks ({}):\n",
            result.skipped_tasks.len()
        ));
        for task in &result.skipped_tasks {
            output.push_str(&format!(
                "  [{}] {} (priority {})\n",
                task.id, task.title, task.priority
            ));
        }
        output.push('\n');
    }

    // Summary
    let remaining = result.blocked_tasks.len() + result.skipped_tasks.len();
    if remaining > 0 {
        output.push_str(&format!(
            "Total: {} task(s) to review ({} blocked, {} skipped)\n",
            remaining,
            result.blocked_tasks.len(),
            result.skipped_tasks.len()
        ));
        output.push_str(
            "Use 'task-mgr unblock <id>' or 'task-mgr unskip <id>' to return tasks to todo.\n",
        );
    } else if !result.actions_taken.is_empty() {
        output.push_str(&format!(
            "Processed {} task(s). All tasks have been addressed.\n",
            result.actions_taken.len()
        ));
    }

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

    fn insert_test_task(conn: &Connection, id: &str, status: &str, priority: i32) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, error_count) VALUES (?, 'Test Task', ?, ?, 0)",
            rusqlite::params![id, status, priority],
        )
        .unwrap();
    }

    fn insert_task_with_details(
        conn: &Connection,
        id: &str,
        title: &str,
        status: &str,
        priority: i32,
        last_error: Option<&str>,
        notes: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, error_count, last_error, notes) VALUES (?, ?, ?, ?, 0, ?, ?)",
            rusqlite::params![id, title, status, priority, last_error, notes],
        )
        .unwrap();
    }

    #[test]
    fn test_get_reviewable_tasks_empty() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "todo", 10);
        insert_test_task(&conn, "US-002", "done", 20);

        let options = ReviewOptions::default();
        let result = get_reviewable_tasks(&conn, &options).unwrap();

        assert_eq!(result.total_count, 0);
        assert!(result.blocked_tasks.is_empty());
        assert!(result.skipped_tasks.is_empty());
    }

    #[test]
    fn test_get_reviewable_tasks_mixed() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "blocked", 10);
        insert_test_task(&conn, "US-002", "skipped", 20);
        insert_test_task(&conn, "US-003", "blocked", 5);
        insert_test_task(&conn, "US-004", "todo", 15);

        let options = ReviewOptions::default();
        let result = get_reviewable_tasks(&conn, &options).unwrap();

        assert_eq!(result.total_count, 3);
        assert_eq!(result.blocked_tasks.len(), 2);
        assert_eq!(result.skipped_tasks.len(), 1);

        // Verify ordering by priority
        assert_eq!(result.blocked_tasks[0].id, "US-003"); // priority 5
        assert_eq!(result.blocked_tasks[1].id, "US-001"); // priority 10
    }

    #[test]
    fn test_get_reviewable_tasks_blocked_only() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "blocked", 10);
        insert_test_task(&conn, "US-002", "skipped", 20);

        let options = ReviewOptions {
            blocked_only: true,
            ..Default::default()
        };
        let result = get_reviewable_tasks(&conn, &options).unwrap();

        assert_eq!(result.total_count, 1);
        assert_eq!(result.blocked_tasks.len(), 1);
        assert!(result.skipped_tasks.is_empty());
    }

    #[test]
    fn test_get_reviewable_tasks_skipped_only() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "blocked", 10);
        insert_test_task(&conn, "US-002", "skipped", 20);

        let options = ReviewOptions {
            skipped_only: true,
            ..Default::default()
        };
        let result = get_reviewable_tasks(&conn, &options).unwrap();

        assert_eq!(result.total_count, 1);
        assert!(result.blocked_tasks.is_empty());
        assert_eq!(result.skipped_tasks.len(), 1);
    }

    #[test]
    fn test_get_reviewable_tasks_with_details() {
        let (_dir, conn) = setup_test_db();
        insert_task_with_details(
            &conn,
            "US-001",
            "Fix the bug",
            "blocked",
            10,
            Some("Missing dependency"),
            Some("Blocked on US-000"),
        );

        let options = ReviewOptions::default();
        let result = get_reviewable_tasks(&conn, &options).unwrap();

        assert_eq!(result.blocked_tasks.len(), 1);
        let task = &result.blocked_tasks[0];
        assert_eq!(task.id, "US-001");
        assert_eq!(task.title, "Fix the bug");
        assert_eq!(task.last_error, Some("Missing dependency".to_string()));
        assert_eq!(task.notes, Some("Blocked on US-000".to_string()));
    }

    #[test]
    fn test_auto_unblock_all() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "blocked", 10);
        insert_test_task(&conn, "US-002", "skipped", 20);
        insert_test_task(&conn, "US-003", "blocked", 5);

        let options = ReviewOptions {
            auto_unblock: true,
            ..Default::default()
        };
        let result = auto_unblock_all(&mut conn, &options).unwrap();

        assert_eq!(result.actions_taken.len(), 3);
        assert!(result.blocked_tasks.is_empty());
        assert!(result.skipped_tasks.is_empty());

        // Verify all tasks are now todo
        let statuses: Vec<String> = conn
            .prepare("SELECT status FROM tasks ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(statuses.iter().all(|s| s == "todo"));
    }

    #[test]
    fn test_auto_unblock_blocked_only() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "blocked", 10);
        insert_test_task(&conn, "US-002", "skipped", 20);

        let options = ReviewOptions {
            auto_unblock: true,
            blocked_only: true,
            ..Default::default()
        };
        let result = auto_unblock_all(&mut conn, &options).unwrap();

        assert_eq!(result.actions_taken.len(), 1);
        assert_eq!(result.actions_taken[0].action, ReviewActionType::Unblocked);

        // Verify blocked task is now todo, skipped is unchanged
        let (status1, status2): (String, String) = (
            conn.query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap(),
            conn.query_row("SELECT status FROM tasks WHERE id = 'US-002'", [], |row| {
                row.get(0)
            })
            .unwrap(),
        );
        assert_eq!(status1, "todo");
        assert_eq!(status2, "skipped");
    }

    #[test]
    fn test_resolve_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_task_with_details(
            &conn,
            "US-001",
            "Fix issue",
            "blocked",
            10,
            Some("Error"),
            Some("Initial notes"),
        );

        let action = resolve_task(&mut conn, "US-001", "Fixed by upgrading dependency").unwrap();

        assert_eq!(action.task_id, "US-001");
        assert_eq!(action.action, ReviewActionType::Resolved);
        assert_eq!(
            action.notes,
            Some("Fixed by upgrading dependency".to_string())
        );

        // Verify task is now todo
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "todo");

        // Verify notes were updated
        let notes: String = conn
            .query_row("SELECT notes FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(notes.contains("Initial notes"));
        assert!(notes.contains("[RESOLVED]"));
        assert!(notes.contains("Fixed by upgrading dependency"));

        // Verify last_error was cleared
        let last_error: Option<String> = conn
            .query_row(
                "SELECT last_error FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(last_error.is_none());
    }

    #[test]
    fn test_resolve_nonexistent_task() {
        let (_dir, mut conn) = setup_test_db();

        let result = resolve_task(&mut conn, "NONEXISTENT", "Notes");

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_format_text_empty() {
        let result = ReviewResult {
            blocked_tasks: Vec::new(),
            skipped_tasks: Vec::new(),
            total_count: 0,
            actions_taken: Vec::new(),
        };

        let text = format_text(&result);
        assert!(text.contains("No blocked or skipped tasks"));
    }

    #[test]
    fn test_format_text_with_tasks() {
        let result = ReviewResult {
            blocked_tasks: vec![ReviewTask {
                id: "US-001".to_string(),
                title: "Fix bug".to_string(),
                description: None,
                status: TaskStatus::Blocked,
                last_error: Some("Missing dep".to_string()),
                notes: None,
                priority: 10,
            }],
            skipped_tasks: vec![ReviewTask {
                id: "US-002".to_string(),
                title: "Add feature".to_string(),
                description: None,
                status: TaskStatus::Skipped,
                last_error: None,
                notes: None,
                priority: 20,
            }],
            total_count: 2,
            actions_taken: Vec::new(),
        };

        let text = format_text(&result);
        assert!(text.contains("Blocked tasks (1)"));
        assert!(text.contains("US-001"));
        assert!(text.contains("Fix bug"));
        assert!(text.contains("Error: Missing dep"));
        assert!(text.contains("Skipped tasks (1)"));
        assert!(text.contains("US-002"));
        assert!(text.contains("2 task(s) to review"));
    }

    #[test]
    fn test_format_text_with_actions() {
        let result = ReviewResult {
            blocked_tasks: Vec::new(),
            skipped_tasks: Vec::new(),
            total_count: 2,
            actions_taken: vec![
                ReviewAction {
                    task_id: "US-001".to_string(),
                    action: ReviewActionType::Unblocked,
                    notes: None,
                },
                ReviewAction {
                    task_id: "US-002".to_string(),
                    action: ReviewActionType::Resolved,
                    notes: Some("Fixed it".to_string()),
                },
            ],
        };

        let text = format_text(&result);
        assert!(text.contains("Actions taken"));
        assert!(text.contains("US-001 - unblocked"));
        assert!(text.contains("US-002 - resolved (Fixed it)"));
        assert!(text.contains("Processed 2 task(s)"));
    }
}
