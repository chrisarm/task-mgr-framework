//! Fail command implementation.
//!
//! The fail command marks one or more tasks as blocked, skipped, or irrelevant
//! with error tracking. This is used when tasks cannot be completed due to issues
//! or blockers.

mod output;
mod transition;

#[cfg(test)]
mod tests;

pub use output::{FailResult, TaskFailResult, format_text};

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::cli::FailStatus;

/// Mark one or more tasks as failed (blocked, skipped, or irrelevant).
///
/// # Arguments
/// * `conn` - Database connection (mutable for transaction support)
/// * `task_ids` - IDs of tasks to fail
/// * `error` - Optional error message describing the failure
/// * `status` - Failure status (blocked, skipped, irrelevant)
/// * `run_id` - Optional run ID for tracking
/// * `force` - If true, skip status transition validation
///
/// # Returns
/// * `Ok(FailResult)` - Information about failed tasks
/// * `Err(TaskMgrError)` - If any task not found, invalid transition, or database error
///
/// # Atomicity
/// When multiple task IDs are provided, all operations are wrapped in a
/// transaction. Either all tasks are failed, or none are (on error).
///
/// # Status Transition Validation
/// By default, only tasks in `in_progress` status can be marked as blocked/skipped/irrelevant.
/// Use `force=true` to override transition validation.
pub fn fail(
    conn: &mut Connection,
    task_ids: &[String],
    error: Option<&str>,
    status: FailStatus,
    run_id: Option<&str>,
    force: bool,
) -> TaskMgrResult<FailResult> {
    // Pre-validate every task before any writes — preserves the legacy
    // all-or-nothing semantics without an outer transaction (the lifecycle
    // service manages per-task atomicity internally). The matrix gate is
    // consulted directly (rather than `TaskStatus::can_transition_to`) so
    // there is exactly one source of truth for Operator transitions.
    {
        use crate::lifecycle::matrix;
        use crate::models::TaskStatus;
        let new_status = match status {
            FailStatus::Blocked => TaskStatus::Blocked,
            FailStatus::Skipped => TaskStatus::Skipped,
            FailStatus::Irrelevant => TaskStatus::Irrelevant,
        };
        for task_id in task_ids {
            let prev_str: String = conn
                .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |r| {
                    r.get(0)
                })
                .map_err(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => {
                        crate::TaskMgrError::task_not_found(task_id)
                    }
                    _ => crate::TaskMgrError::from(e),
                })?;
            let prev: TaskStatus = prev_str.parse()?;
            if !force
                && matrix::validate(prev, new_status, matrix::TransitionSource::Operator).is_err()
            {
                return Err(legacy_transition_error(task_id, prev, new_status));
            }
        }
    }

    let mut results = Vec::with_capacity(task_ids.len());
    for task_id in task_ids {
        // `force=true` skips re-validation inside fail_single_task; the
        // pre-validate pass above already gated invalid transitions.
        let result = transition::fail_single_task(conn, task_id, error, status, run_id, true)?;
        results.push(result);
    }
    let failed_count = results.len();
    Ok(FailResult {
        tasks: results,
        failed_count,
        run_id: run_id.map(String::from),
    })
}

/// Reproduce the legacy `validate_transition` error shape. Kept here (not
/// re-exported from transition.rs) so the pre-validate pass can build the
/// same hint without a second matrix lookup.
fn legacy_transition_error(
    task_id: &str,
    previous_status: crate::models::TaskStatus,
    new_status: crate::models::TaskStatus,
) -> crate::TaskMgrError {
    use crate::models::TaskStatus;
    let status_name = new_status.as_db_str();
    let valid_transitions = previous_status.valid_transitions();
    let hint = if valid_transitions.is_empty() {
        format!(
            "Task '{task_id}' is in '{previous_status}' status which is a terminal state. No transitions allowed."
        )
    } else if previous_status == TaskStatus::Todo {
        format!(
            "Task '{task_id}' is in 'todo' status. Use 'task-mgr next --claim {task_id}' to claim it first, then mark as {status_name}. Or use --force to override."
        )
    } else {
        format!(
            "Task '{task_id}' is in '{previous_status}' status. Valid transitions: {}. Use --force to override.",
            valid_transitions.join(", ")
        )
    };
    crate::TaskMgrError::invalid_transition(task_id, previous_status.to_string(), status_name, hint)
}
