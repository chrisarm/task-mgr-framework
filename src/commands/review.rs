//! Review command implementation.
//!
//! The review command provides an interactive or batch way to cycle through
//! blocked and skipped tasks, allowing users to resolve, unblock, skip,
//! or move to the next task.

use rusqlite::Connection;
use serde::Serialize;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

/// Task details for review.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewTask {
    /// Task ID
    pub id: String,
    /// Task title
    pub title: String,
    /// Task description
    pub description: Option<String>,
    /// Current status (blocked or skipped)
    pub status: TaskStatus,
    /// Last error message (for blocked tasks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Notes (may contain skip/block reason)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Priority
    pub priority: i32,
}

/// Result of reviewing tasks.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewResult {
    /// Tasks that were reviewed and are still blocked/skipped
    pub blocked_tasks: Vec<ReviewTask>,
    /// Tasks that were reviewed and are still skipped
    pub skipped_tasks: Vec<ReviewTask>,
    /// Total count of tasks available for review
    pub total_count: usize,
    /// Actions taken during review (for text output)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions_taken: Vec<ReviewAction>,
}

/// An action taken during review.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewAction {
    /// Task ID that was acted on
    pub task_id: String,
    /// Action type
    pub action: ReviewActionType,
    /// Optional notes (for resolve action)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Type of action taken on a task during review.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewActionType {
    /// Task was unblocked (returned to todo)
    Unblocked,
    /// Task was unskipped (returned to todo)
    Unskipped,
    /// Task was resolved with notes (returned to todo)
    Resolved,
    /// Task was skipped (remains skipped or updated reason)
    Skipped,
    /// Task was kept as-is
    Kept,
}

/// Options for the review command.
#[derive(Debug, Clone, Default)]
pub struct ReviewOptions {
    /// Only review blocked tasks
    pub blocked_only: bool,
    /// Only review skipped tasks
    pub skipped_only: bool,
    /// Auto-unblock all tasks without prompts
    pub auto_unblock: bool,
}

/// Get all blocked and/or skipped tasks for review.
///
/// # Arguments
/// * `conn` - Database connection
/// * `options` - Review options controlling which tasks to fetch
///
/// # Returns
/// * `Ok(ReviewResult)` - List of tasks to review
/// * `Err(TaskMgrError)` - On database error
pub fn get_reviewable_tasks(
    conn: &Connection,
    options: &ReviewOptions,
) -> TaskMgrResult<ReviewResult> {
    let mut blocked_tasks = Vec::new();
    let mut skipped_tasks = Vec::new();

    // Build query based on options
    let status_filter = if options.blocked_only {
        "status = 'blocked'"
    } else if options.skipped_only {
        "status = 'skipped'"
    } else {
        "status IN ('blocked', 'skipped')"
    };

    let query = format!(
        "SELECT id, title, description, status, last_error, notes, priority \
         FROM tasks \
         WHERE {} \
         ORDER BY priority ASC, id ASC",
        status_filter
    );

    let mut stmt = conn.prepare(&query)?;
    let rows = stmt.query_map([], |row| {
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
    })?;

    for row in rows {
        let task = row?;
        match task.status {
            TaskStatus::Blocked => blocked_tasks.push(task),
            TaskStatus::Skipped => skipped_tasks.push(task),
            _ => {} // Shouldn't happen with our query
        }
    }

    let total_count = blocked_tasks.len() + skipped_tasks.len();

    Ok(ReviewResult {
        blocked_tasks,
        skipped_tasks,
        total_count,
        actions_taken: Vec::new(),
    })
}

/// Auto-unblock all blocked tasks.
///
/// # Arguments
/// * `conn` - Database connection
/// * `options` - Review options
///
/// # Returns
/// * `Ok(ReviewResult)` - Result with actions taken
pub fn auto_unblock_all(conn: &Connection, options: &ReviewOptions) -> TaskMgrResult<ReviewResult> {
    let mut actions = Vec::new();

    // Get all reviewable tasks first
    let tasks = get_reviewable_tasks(conn, options)?;

    // Unblock all blocked tasks
    for task in &tasks.blocked_tasks {
        unblock_task(conn, &task.id)?;
        actions.push(ReviewAction {
            task_id: task.id.clone(),
            action: ReviewActionType::Unblocked,
            notes: None,
        });
    }

    // Unskip all skipped tasks
    for task in &tasks.skipped_tasks {
        unskip_task(conn, &task.id)?;
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

/// Unblock a single task (internal helper).
fn unblock_task(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    let audit_note = "[AUTO-UNBLOCKED] Returned to todo via review --auto".to_string();

    // Get current notes
    let current_notes: Option<String> = conn
        .query_row("SELECT notes FROM tasks WHERE id = ?", [task_id], |row| {
            row.get(0)
        })
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            _ => TaskMgrError::from(e),
        })?;

    let new_notes = match &current_notes {
        Some(existing) if !existing.is_empty() => format!("{}\n\n{}", existing, audit_note),
        _ => audit_note,
    };

    conn.execute(
        "UPDATE tasks SET status = ?, last_error = NULL, notes = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![TaskStatus::Todo.as_db_str(), new_notes, task_id],
    )?;

    Ok(())
}

/// Unskip a single task (internal helper).
fn unskip_task(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    let audit_note = "[AUTO-UNSKIPPED] Returned to todo via review --auto".to_string();

    // Get current notes
    let current_notes: Option<String> = conn
        .query_row("SELECT notes FROM tasks WHERE id = ?", [task_id], |row| {
            row.get(0)
        })
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            _ => TaskMgrError::from(e),
        })?;

    let new_notes = match &current_notes {
        Some(existing) if !existing.is_empty() => format!("{}\n\n{}", existing, audit_note),
        _ => audit_note,
    };

    conn.execute(
        "UPDATE tasks SET status = ?, notes = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![TaskStatus::Todo.as_db_str(), new_notes, task_id],
    )?;

    Ok(())
}

/// Resolve a task with custom notes (returns to todo).
///
/// # Arguments
/// * `conn` - Database connection
/// * `task_id` - Task to resolve
/// * `resolution_notes` - Notes explaining the resolution
pub fn resolve_task(
    conn: &Connection,
    task_id: &str,
    resolution_notes: &str,
) -> TaskMgrResult<ReviewAction> {
    let audit_note = format!("[RESOLVED] {}", resolution_notes);

    // Get current status and notes
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

    let _status: TaskStatus = status_str.parse()?;

    let new_notes = match &current_notes {
        Some(existing) if !existing.is_empty() => format!("{}\n\n{}", existing, audit_note),
        _ => audit_note.clone(),
    };

    conn.execute(
        "UPDATE tasks SET status = ?, last_error = NULL, notes = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![TaskStatus::Todo.as_db_str(), new_notes, task_id],
    )?;

    Ok(ReviewAction {
        task_id: task_id.to_string(),
        action: ReviewActionType::Resolved,
        notes: Some(resolution_notes.to_string()),
    })
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
    use crate::db::{create_schema, open_connection};
    use tempfile::TempDir;

    fn setup_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let conn = open_connection(temp_dir.path()).unwrap();
        create_schema(&conn).unwrap();
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
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "blocked", 10);
        insert_test_task(&conn, "US-002", "skipped", 20);
        insert_test_task(&conn, "US-003", "blocked", 5);

        let options = ReviewOptions {
            auto_unblock: true,
            ..Default::default()
        };
        let result = auto_unblock_all(&conn, &options).unwrap();

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
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "blocked", 10);
        insert_test_task(&conn, "US-002", "skipped", 20);

        let options = ReviewOptions {
            auto_unblock: true,
            blocked_only: true,
            ..Default::default()
        };
        let result = auto_unblock_all(&conn, &options).unwrap();

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
        let (_dir, conn) = setup_test_db();
        insert_task_with_details(
            &conn,
            "US-001",
            "Fix issue",
            "blocked",
            10,
            Some("Error"),
            Some("Initial notes"),
        );

        let action = resolve_task(&conn, "US-001", "Fixed by upgrading dependency").unwrap();

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
        let (_dir, conn) = setup_test_db();

        let result = resolve_task(&conn, "NONEXISTENT", "Notes");

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
