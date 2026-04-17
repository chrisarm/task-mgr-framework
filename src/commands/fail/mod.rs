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
    // Wrap all operations in a transaction for atomicity when failing multiple tasks
    let tx = conn.transaction()?;

    let mut results = Vec::with_capacity(task_ids.len());

    for task_id in task_ids {
        let result = transition::fail_single_task(&tx, task_id, error, status, run_id, force)?;
        results.push(result);
    }

    // Commit the transaction - all changes are atomic
    tx.commit()?;

    let failed_count = results.len();

    Ok(FailResult {
        tasks: results,
        failed_count,
        run_id: run_id.map(String::from),
    })
}
