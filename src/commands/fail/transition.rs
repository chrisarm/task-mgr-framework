//! Status transition logic for the fail command.
//!
//! This module handles the core logic for transitioning tasks to failure states
//! (blocked, skipped, irrelevant) including validation and database updates.

use rusqlite::Connection;

use crate::cli::FailStatus;
use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::output::TaskFailResult;

/// Fail a single task, updating its status and tracking information.
///
/// # Arguments
/// * `conn` - Database connection
/// * `task_id` - ID of the task to fail
/// * `error` - Optional error message
/// * `status` - Target failure status
/// * `run_id` - Optional run ID for tracking
/// * `force` - If true, skip transition validation
///
/// # Returns
/// Result with task failure information or error.
pub fn fail_single_task(
    conn: &Connection,
    task_id: &str,
    error: Option<&str>,
    status: FailStatus,
    run_id: Option<&str>,
    force: bool,
) -> TaskMgrResult<TaskFailResult> {
    // Query current task status and error count
    let (previous_status_str, current_error_count, current_notes): (String, i32, Option<String>) =
        conn.query_row(
            "SELECT status, error_count, notes FROM tasks WHERE id = ?",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            _ => TaskMgrError::from(e),
        })?;

    let previous_status: TaskStatus = previous_status_str.parse()?;

    // Map FailStatus to TaskStatus
    let new_status = match status {
        FailStatus::Blocked => TaskStatus::Blocked,
        FailStatus::Skipped => TaskStatus::Skipped,
        FailStatus::Irrelevant => TaskStatus::Irrelevant,
    };

    // Validate status transition
    validate_transition(task_id, previous_status, new_status, force)?;

    // Increment error count
    let new_error_count = current_error_count + 1;

    // Build notes update with error prefix
    let new_notes = build_notes(&current_notes, error, status);

    // Get the current global iteration for decay tracking
    let current_iteration: i64 = conn
        .query_row(
            "SELECT iteration_counter FROM global_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // Determine which iteration column to set based on target status
    // Note: Irrelevant tasks don't decay, so we don't track their iteration
    let (blocked_at, skipped_at) = match status {
        FailStatus::Blocked => (Some(current_iteration), None::<i64>),
        FailStatus::Skipped => (None::<i64>, Some(current_iteration)),
        FailStatus::Irrelevant => (None::<i64>, None::<i64>), // Irrelevant tasks don't decay
    };

    // Update task status, error count, last_error, and decay tracking columns
    conn.execute(
        "UPDATE tasks SET status = ?, error_count = ?, last_error = ?, notes = ?, \
         blocked_at_iteration = COALESCE(?, blocked_at_iteration), \
         skipped_at_iteration = COALESCE(?, skipped_at_iteration), \
         updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![
            new_status.to_string(),
            new_error_count,
            error,
            new_notes,
            blocked_at,
            skipped_at,
            task_id
        ],
    )?;

    // If run_id provided, update run_tasks if exists
    if let Some(rid) = run_id {
        update_run_task(conn, rid, task_id, status, error)?;
    }

    // Generate next steps hint based on status
    let next_steps = generate_next_steps(status);

    Ok(TaskFailResult {
        task_id: task_id.to_string(),
        previous_status,
        new_status,
        error: error.map(String::from),
        error_count: new_error_count,
        next_steps,
    })
}

/// Validate that the status transition is allowed.
fn validate_transition(
    task_id: &str,
    previous_status: TaskStatus,
    new_status: TaskStatus,
    force: bool,
) -> TaskMgrResult<()> {
    let can_transition = previous_status.can_transition_to(new_status);

    // If invalid transition and not forcing, return error
    if !can_transition && !force {
        let valid_transitions = previous_status.valid_transitions();
        let status_name = new_status.as_db_str();
        let hint = if valid_transitions.is_empty() {
            format!(
                "Task '{}' is in '{}' status which is a terminal state. No transitions allowed.",
                task_id, previous_status
            )
        } else if previous_status == TaskStatus::Todo {
            format!(
                "Task '{}' is in 'todo' status. Use 'task-mgr next --claim {}' to claim it first, then mark as {}. Or use --force to override.",
                task_id, task_id, status_name
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
            status_name,
            hint,
        ));
    }

    Ok(())
}

/// Build the updated notes field with status prefix.
fn build_notes(current_notes: &Option<String>, error: Option<&str>, status: FailStatus) -> String {
    let status_prefix = match status {
        FailStatus::Blocked => "[BLOCKED]",
        FailStatus::Skipped => "[SKIPPED]",
        FailStatus::Irrelevant => "[IRRELEVANT]",
    };

    match (current_notes, error) {
        (Some(existing), Some(err)) if !existing.is_empty() => {
            format!("{}\n\n{} {}", existing, status_prefix, err)
        }
        (Some(existing), None) if !existing.is_empty() => {
            format!("{}\n\n{}", existing, status_prefix)
        }
        (_, Some(err)) => format!("{} {}", status_prefix, err),
        (_, None) => status_prefix.to_string(),
    }
}

/// Update run_task entry if it exists.
fn update_run_task(
    conn: &Connection,
    run_id: &str,
    task_id: &str,
    status: FailStatus,
    error: Option<&str>,
) -> TaskMgrResult<()> {
    let run_task_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM run_tasks WHERE run_id = ? AND task_id = ?)",
            rusqlite::params![run_id, task_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if run_task_exists {
        // Map to run_tasks status - use 'failed' for blocked, 'skipped' for skipped/irrelevant
        let run_task_status = match status {
            FailStatus::Blocked => "failed",
            FailStatus::Skipped | FailStatus::Irrelevant => "skipped",
        };

        conn.execute(
            "UPDATE run_tasks SET status = ?, notes = ?, ended_at = datetime('now') \
             WHERE run_id = ? AND task_id = ?",
            rusqlite::params![run_task_status, error.unwrap_or(""), run_id, task_id],
        )?;
    }

    Ok(())
}

/// Generate next steps hint based on failure status.
fn generate_next_steps(status: FailStatus) -> Option<String> {
    match status {
        FailStatus::Blocked => Some(
            "Use `task-mgr doctor` to check for stale blocked tasks, or fix the blocker and retry."
                .to_string(),
        ),
        FailStatus::Skipped => {
            Some("Skipped tasks can be picked up later with `task-mgr next`.".to_string())
        }
        FailStatus::Irrelevant => {
            Some("Irrelevant tasks are permanently excluded from selection.".to_string())
        }
    }
}
