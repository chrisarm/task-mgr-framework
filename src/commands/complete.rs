//! Complete command implementation.
//!
//! The complete command marks one or more tasks as done, updating timestamps
//! and run tracking information.

use rusqlite::Connection;
use serde::Serialize;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

/// Result of completing a single task.
#[derive(Debug, Clone, Serialize)]
pub struct TaskCompletionResult {
    /// The task that was completed
    pub task_id: String,
    /// Previous status before completion
    pub previous_status: TaskStatus,
    /// Whether the task was already done
    pub was_already_done: bool,
    /// Warning message if task was not in_progress
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Result of completing multiple tasks.
#[derive(Debug, Clone, Serialize)]
pub struct CompleteResult {
    /// Results for each task
    pub tasks: Vec<TaskCompletionResult>,
    /// Number of tasks successfully completed
    pub completed_count: usize,
    /// Number of tasks that were already done
    pub already_done_count: usize,
    /// Run ID if tracking was enabled
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Commit hash if provided
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Complete one or more tasks.
///
/// # Arguments
/// * `conn` - Database connection (mutable for transaction support)
/// * `task_ids` - IDs of tasks to complete
/// * `run_id` - Optional run ID for tracking
/// * `commit` - Optional commit hash to record
/// * `force` - If true, skip status transition validation
///
/// # Returns
/// * `Ok(CompleteResult)` - Information about completed tasks
/// * `Err(TaskMgrError)` - If any task not found, invalid transition, or database error
///
/// # Atomicity
/// When multiple task IDs are provided, all operations are wrapped in a
/// transaction. Either all tasks are completed, or none are (on error).
///
/// # Status Transition Validation
/// By default, only tasks in `in_progress` status can be completed.
/// Tasks in `todo` must be claimed first. Use `force=true` to override.
pub fn complete(
    conn: &mut Connection,
    task_ids: &[String],
    run_id: Option<&str>,
    commit: Option<&str>,
    force: bool,
) -> TaskMgrResult<CompleteResult> {
    // Wrap all operations in a transaction for atomicity when completing multiple tasks
    let tx = conn.transaction()?;

    let mut results = Vec::with_capacity(task_ids.len());
    let mut completed_count = 0;
    let mut already_done_count = 0;

    for task_id in task_ids {
        let result = complete_single_task(&tx, task_id, run_id, force)?;
        if result.was_already_done {
            already_done_count += 1;
        } else {
            completed_count += 1;
        }
        results.push(result);
    }

    // If commit provided and run_id exists, update run's last_commit
    if let (Some(rid), Some(commit_hash)) = (run_id, commit) {
        tx.execute(
            "UPDATE runs SET last_commit = ? WHERE run_id = ?",
            rusqlite::params![commit_hash, rid],
        )?;
    }

    // Increment run's iteration_count if run_id provided
    if let Some(rid) = run_id {
        tx.execute(
            "UPDATE runs SET iteration_count = iteration_count + 1 WHERE run_id = ?",
            [rid],
        )?;
    }

    // Commit the transaction - all changes are atomic
    tx.commit()?;

    Ok(CompleteResult {
        tasks: results,
        completed_count,
        already_done_count,
        run_id: run_id.map(String::from),
        commit: commit.map(String::from),
    })
}

/// Returns unsatisfied dependency IDs for a task.
///
/// Queries `task_relationships` for `dependsOn` entries, then checks if each
/// dependency is `done` or `irrelevant`. Returns only the IDs that are NOT
/// in a terminal state.
fn get_unsatisfied_deps(conn: &Connection, task_id: &str) -> TaskMgrResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT related_id FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'",
    )?;
    let dep_ids: Vec<String> = stmt
        .query_map([task_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let mut unsatisfied = Vec::new();
    for dep_id in &dep_ids {
        let status: Option<String> = conn
            .query_row("SELECT status FROM tasks WHERE id = ?", [dep_id], |row| {
                row.get(0)
            })
            .ok();
        match status.as_deref() {
            Some("done") | Some("irrelevant") => {} // satisfied
            _ => unsatisfied.push(dep_id.clone()),  // not done, missing, or other status
        }
    }

    Ok(unsatisfied)
}

/// Check whether all `dependsOn` dependencies for a task are satisfied.
///
/// Returns `true` if the task has no dependencies, or all dependencies are
/// `done` or `irrelevant`. **Fail-closed**: returns `false` on query errors.
pub fn are_dependencies_satisfied(conn: &Connection, task_id: &str) -> bool {
    match get_unsatisfied_deps(conn, task_id) {
        Ok(unsatisfied) => unsatisfied.is_empty(),
        Err(e) => {
            eprintln!(
                "Warning: dependency check failed for task {}, assuming unsatisfied: {}",
                task_id, e
            );
            false
        }
    }
}

/// Gate task completion on dependency satisfaction.
///
/// Returns `Ok(())` if all dependencies are met, or `Err(DependencyNotSatisfied)`
/// with the list of unsatisfied dependency IDs.
fn check_dependencies_satisfied(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    let unsatisfied = get_unsatisfied_deps(conn, task_id)?;
    if unsatisfied.is_empty() {
        Ok(())
    } else {
        Err(TaskMgrError::dependency_not_satisfied(task_id, unsatisfied))
    }
}

/// Complete a single task.
fn complete_single_task(
    conn: &Connection,
    task_id: &str,
    run_id: Option<&str>,
    force: bool,
) -> TaskMgrResult<TaskCompletionResult> {
    // Query current task status
    let previous_status_str: String = conn
        .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |row| {
            row.get(0)
        })
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            _ => TaskMgrError::from(e),
        })?;

    let previous_status: TaskStatus = previous_status_str.parse()?;

    // Check if already done
    let was_already_done = previous_status == TaskStatus::Done;

    // Gate on dependency satisfaction (skip if already done or forcing)
    if !was_already_done && !force {
        check_dependencies_satisfied(conn, task_id)?;
    }

    // Validate status transition
    let can_transition = previous_status.can_transition_to(TaskStatus::Done);

    // If invalid transition and not forcing, return error
    if !was_already_done && !can_transition && !force {
        let valid_transitions = previous_status.valid_transitions();
        let hint = if valid_transitions.is_empty() {
            format!(
                "Task '{}' is in '{}' status which is a terminal state. No transitions allowed.",
                task_id, previous_status
            )
        } else if previous_status == TaskStatus::Todo {
            format!(
                "Task '{}' is in 'todo' status. Use 'task-mgr next --claim {}' to claim it first, then complete. Or use --force to override.",
                task_id, task_id
            )
        } else {
            format!(
                "Task '{}' is in '{}' status. Valid transitions: {}. Use --force to override.",
                task_id,
                previous_status,
                valid_transitions.join(", ")
            )
        };
        return Err(TaskMgrError::invalid_transition(
            task_id,
            previous_status.to_string(),
            "done",
            hint,
        ));
    }

    // Generate warning if task was not in_progress (but force was used)
    let warning = if !was_already_done && !can_transition && force {
        Some(format!(
            "Forced completion: task was in '{}' status (invalid transition, overridden with --force).",
            previous_status
        ))
    } else if !was_already_done && previous_status != TaskStatus::InProgress {
        Some(format!(
            "Task was in '{}' status, not 'in_progress'. Completing anyway.",
            previous_status
        ))
    } else {
        None
    };

    if !was_already_done {
        // Update task status to done
        conn.execute(
            "UPDATE tasks SET status = 'done', completed_at = datetime('now'), updated_at = datetime('now') WHERE id = ?",
            [task_id],
        )?;
    }

    // If run_id provided, update run_tasks entry
    if let Some(rid) = run_id {
        // Check if there's an existing run_tasks entry
        let run_task_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM run_tasks WHERE run_id = ? AND task_id = ?)",
                rusqlite::params![rid, task_id],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if run_task_exists {
            // Update existing entry with completion info
            // Calculate duration using julianday
            conn.execute(
                r#"
                UPDATE run_tasks
                SET status = 'completed',
                    ended_at = datetime('now'),
                    duration_seconds = CAST(
                        (julianday('now') - julianday(started_at)) * 86400 AS INTEGER
                    )
                WHERE run_id = ? AND task_id = ? AND status = 'started'
                "#,
                rusqlite::params![rid, task_id],
            )?;
        }
    }

    Ok(TaskCompletionResult {
        task_id: task_id.to_string(),
        previous_status,
        was_already_done,
        warning,
    })
}

/// Format complete result as human-readable text.
#[must_use]
pub fn format_text(result: &CompleteResult) -> String {
    let mut output = String::new();

    if result.tasks.len() == 1 {
        // Single task output
        let task = &result.tasks[0];
        if task.was_already_done {
            output.push_str(&format!("Task {} was already done.\n", task.task_id));
        } else {
            output.push_str(&format!(
                "Completed task {} (was {}).\n",
                task.task_id, task.previous_status
            ));
        }
        if let Some(ref warning) = task.warning {
            output.push_str(&format!("Warning: {}\n", warning));
        }
    } else {
        // Multiple tasks output
        output.push_str(&format!(
            "Completed {} task(s), {} already done.\n",
            result.completed_count, result.already_done_count
        ));
        for task in &result.tasks {
            if task.was_already_done {
                output.push_str(&format!("  - {} (already done)\n", task.task_id));
            } else {
                output.push_str(&format!(
                    "  - {} (was {})\n",
                    task.task_id, task.previous_status
                ));
            }
            if let Some(ref warning) = task.warning {
                output.push_str(&format!("    Warning: {}\n", warning));
            }
        }
    }

    if let Some(ref commit) = result.commit {
        output.push_str(&format!("Recorded commit: {}\n", commit));
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
    fn test_complete_single_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        let result = complete(&mut conn, &["US-001".to_string()], None, None, false).unwrap();

        assert_eq!(result.completed_count, 1);
        assert_eq!(result.already_done_count, 0);
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].task_id, "US-001");
        assert_eq!(result.tasks[0].previous_status, TaskStatus::InProgress);
        assert!(!result.tasks[0].was_already_done);
        assert!(result.tasks[0].warning.is_none());

        // Verify status in database
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");
    }

    #[test]
    fn test_complete_multiple_tasks() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");
        insert_test_task(&conn, "US-002", "in_progress");
        insert_test_task(&conn, "US-003", "in_progress");

        let result = complete(
            &mut conn,
            &[
                "US-001".to_string(),
                "US-002".to_string(),
                "US-003".to_string(),
            ],
            None,
            None,
            false,
        )
        .unwrap();

        assert_eq!(result.completed_count, 3);
        assert_eq!(result.already_done_count, 0);
        assert_eq!(result.tasks.len(), 3);

        // Verify all tasks are done
        for task_id in ["US-001", "US-002", "US-003"] {
            let status: String = conn
                .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(status, "done");
        }
    }

    #[test]
    fn test_complete_already_done_task() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "done");

        let result = complete(&mut conn, &["US-001".to_string()], None, None, false).unwrap();

        assert_eq!(result.completed_count, 0);
        assert_eq!(result.already_done_count, 1);
        assert!(result.tasks[0].was_already_done);
        assert_eq!(result.tasks[0].previous_status, TaskStatus::Done);
    }

    #[test]
    fn test_complete_todo_task_requires_force() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "todo");

        // Without force, should return InvalidTransition error
        let result = complete(&mut conn, &["US-001".to_string()], None, None, false);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::InvalidTransition {
                task_id, from, to, ..
            }) => {
                assert_eq!(task_id, "US-001");
                assert_eq!(from, "todo");
                assert_eq!(to, "done");
            }
            _ => panic!("Expected InvalidTransition error"),
        }
    }

    #[test]
    fn test_complete_todo_task_with_force() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "todo");

        // With force, should complete but with warning
        let result = complete(&mut conn, &["US-001".to_string()], None, None, true).unwrap();

        assert_eq!(result.completed_count, 1);
        assert!(!result.tasks[0].was_already_done);
        assert!(result.tasks[0].warning.is_some());
        assert!(result.tasks[0]
            .warning
            .as_ref()
            .unwrap()
            .contains("--force"));
    }

    #[test]
    fn test_complete_nonexistent_task() {
        let (_dir, mut conn) = setup_test_db();

        let result = complete(&mut conn, &["NONEXISTENT".to_string()], None, None, false);

        assert!(result.is_err());
        match result {
            Err(TaskMgrError::NotFound { .. }) => {}
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_complete_with_run_id() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        // Create a run and run_task entry
        conn.execute(
            "INSERT INTO runs (run_id, status, iteration_count) VALUES ('run-123', 'active', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration, started_at) VALUES ('run-123', 'US-001', 'started', 1, datetime('now'))",
            [],
        )
        .unwrap();

        let result = complete(
            &mut conn,
            &["US-001".to_string()],
            Some("run-123"),
            None,
            false,
        )
        .unwrap();

        assert_eq!(result.run_id, Some("run-123".to_string()));

        // Verify run_tasks was updated
        let run_task_status: String = conn
            .query_row(
                "SELECT status FROM run_tasks WHERE run_id = 'run-123' AND task_id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_task_status, "completed");

        // Verify iteration_count was incremented
        let iteration_count: i32 = conn
            .query_row(
                "SELECT iteration_count FROM runs WHERE run_id = 'run-123'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(iteration_count, 1);
    }

    #[test]
    fn test_complete_with_commit() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        // Create a run
        conn.execute(
            "INSERT INTO runs (run_id, status, iteration_count) VALUES ('run-456', 'active', 0)",
            [],
        )
        .unwrap();

        let result = complete(
            &mut conn,
            &["US-001".to_string()],
            Some("run-456"),
            Some("abc123def"),
            false,
        )
        .unwrap();

        assert_eq!(result.commit, Some("abc123def".to_string()));

        // Verify last_commit was updated
        let last_commit: Option<String> = conn
            .query_row(
                "SELECT last_commit FROM runs WHERE run_id = 'run-456'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(last_commit, Some("abc123def".to_string()));
    }

    #[test]
    fn test_complete_sets_completed_at() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        complete(&mut conn, &["US-001".to_string()], None, None, false).unwrap();

        // Verify completed_at was set
        let completed_at: Option<String> = conn
            .query_row(
                "SELECT completed_at FROM tasks WHERE id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(completed_at.is_some());
    }

    #[test]
    fn test_complete_mixed_statuses_with_force() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");
        insert_test_task(&conn, "US-002", "done");
        insert_test_task(&conn, "US-003", "todo");

        // Use force=true since US-003 is in todo status
        let result = complete(
            &mut conn,
            &[
                "US-001".to_string(),
                "US-002".to_string(),
                "US-003".to_string(),
            ],
            None,
            None,
            true,
        )
        .unwrap();

        assert_eq!(result.completed_count, 2); // US-001 and US-003
        assert_eq!(result.already_done_count, 1); // US-002
        assert!(result.tasks[2].warning.is_some()); // US-003 had warning about force
    }

    #[test]
    fn test_format_text_single_task() {
        let result = CompleteResult {
            tasks: vec![TaskCompletionResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::InProgress,
                was_already_done: false,
                warning: None,
            }],
            completed_count: 1,
            already_done_count: 0,
            run_id: None,
            commit: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Completed task US-001"));
        assert!(text.contains("was in_progress"));
    }

    #[test]
    fn test_format_text_already_done() {
        let result = CompleteResult {
            tasks: vec![TaskCompletionResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::Done,
                was_already_done: true,
                warning: None,
            }],
            completed_count: 0,
            already_done_count: 1,
            run_id: None,
            commit: None,
        };

        let text = format_text(&result);
        assert!(text.contains("already done"));
    }

    #[test]
    fn test_format_text_with_warning() {
        let result = CompleteResult {
            tasks: vec![TaskCompletionResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::Todo,
                was_already_done: false,
                warning: Some("Task was in 'todo' status".to_string()),
            }],
            completed_count: 1,
            already_done_count: 0,
            run_id: None,
            commit: None,
        };

        let text = format_text(&result);
        assert!(text.contains("Warning:"));
    }

    #[test]
    fn test_format_text_multiple_tasks() {
        let result = CompleteResult {
            tasks: vec![
                TaskCompletionResult {
                    task_id: "US-001".to_string(),
                    previous_status: TaskStatus::InProgress,
                    was_already_done: false,
                    warning: None,
                },
                TaskCompletionResult {
                    task_id: "US-002".to_string(),
                    previous_status: TaskStatus::InProgress,
                    was_already_done: false,
                    warning: None,
                },
            ],
            completed_count: 2,
            already_done_count: 0,
            run_id: Some("run-123".to_string()),
            commit: Some("abc123".to_string()),
        };

        let text = format_text(&result);
        assert!(text.contains("Completed 2 task(s)"));
        assert!(text.contains("US-001"));
        assert!(text.contains("US-002"));
        assert!(text.contains("Recorded commit: abc123"));
        assert!(text.contains("Run: run-123"));
    }

    #[test]
    fn test_complete_duration_calculation() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "US-001", "in_progress");

        // Create a run and run_task entry with a known started_at
        conn.execute(
            "INSERT INTO runs (run_id, status, iteration_count) VALUES ('run-789', 'active', 0)",
            [],
        )
        .unwrap();
        // Set started_at to 60 seconds ago
        conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, status, iteration, started_at) VALUES ('run-789', 'US-001', 'started', 1, datetime('now', '-60 seconds'))",
            [],
        )
        .unwrap();

        complete(
            &mut conn,
            &["US-001".to_string()],
            Some("run-789"),
            None,
            false,
        )
        .unwrap();

        // Verify duration_seconds is approximately 60
        let duration: Option<i64> = conn
            .query_row(
                "SELECT duration_seconds FROM run_tasks WHERE run_id = 'run-789' AND task_id = 'US-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Allow some tolerance for test execution time
        assert!(duration.is_some());
        let d = duration.unwrap();
        assert!((59..=65).contains(&d), "Duration was {} seconds", d);
    }

    // --- Dependency gating tests ---

    fn insert_relationship(conn: &Connection, task_id: &str, related_id: &str, rel_type: &str) {
        conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?, ?, ?)",
            rusqlite::params![task_id, related_id, rel_type],
        )
        .unwrap();
    }

    #[test]
    fn test_complete_blocked_by_unsatisfied_dependency() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "todo");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, false);
        assert!(result.is_err());
        match result {
            Err(TaskMgrError::DependencyNotSatisfied {
                task_id,
                unsatisfied,
                ..
            }) => {
                assert_eq!(task_id, "TASK-001");
                assert!(unsatisfied.contains("DEP-001"));
            }
            other => panic!("Expected DependencyNotSatisfied, got {:?}", other),
        }
    }

    #[test]
    fn test_complete_succeeds_when_dependencies_satisfied() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "done");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().completed_count, 1);
    }

    #[test]
    fn test_complete_force_bypasses_dependency_check() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "todo");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, true);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().completed_count, 1);
    }

    #[test]
    fn test_complete_no_dependencies_succeeds() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "TASK-001", "in_progress");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().completed_count, 1);
    }

    #[test]
    fn test_complete_dependency_on_irrelevant_task_succeeds() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "irrelevant");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");

        let result = complete(&mut conn, &["TASK-001".to_string()], None, None, false);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().completed_count, 1);
    }

    #[test]
    fn test_are_dependencies_satisfied_returns_correct_bool() {
        let (_dir, conn) = setup_test_db();
        insert_test_task(&conn, "DEP-001", "todo");
        insert_test_task(&conn, "DEP-002", "done");
        insert_test_task(&conn, "TASK-001", "in_progress");
        insert_relationship(&conn, "TASK-001", "DEP-001", "dependsOn");
        insert_relationship(&conn, "TASK-001", "DEP-002", "dependsOn");

        // DEP-001 is still todo, so not satisfied
        assert!(!are_dependencies_satisfied(&conn, "TASK-001"));

        // No deps → satisfied
        assert!(are_dependencies_satisfied(&conn, "DEP-001"));
    }

    #[test]
    fn test_complete_circular_dependency_with_force_bypasses() {
        let (_dir, mut conn) = setup_test_db();
        insert_test_task(&conn, "A", "in_progress");
        insert_test_task(&conn, "B", "in_progress");
        insert_relationship(&conn, "A", "B", "dependsOn");
        insert_relationship(&conn, "B", "A", "dependsOn");

        // Without force, both should fail
        let result_a = complete(&mut conn, &["A".to_string()], None, None, false);
        assert!(result_a.is_err());

        // With force, both complete
        let result_a = complete(&mut conn, &["A".to_string()], None, None, true);
        assert!(result_a.is_ok());
        let result_b = complete(&mut conn, &["B".to_string()], None, None, true);
        assert!(result_b.is_ok());
    }
}
