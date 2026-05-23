//! Status transition logic for the fail command. Pre-validates the matrix
//! transition (when `!force`), then routes the status mutation through
//! `TaskLifecycle::apply` (TransitionChange::Failed). The lifecycle service
//! owns the SQL, error_count increment, decay-iteration tracking, and
//! per-status notes prefix; this module owns CLI-side validation hints.

use rusqlite::Connection;

use crate::cli::FailStatus;
use crate::lifecycle::matrix;
use crate::lifecycle::{TaskLifecycle, TransitionChange, TransitionIntent, TransitionSource};
use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::output::TaskFailResult;

/// Fail a single task, updating its status and tracking information.
pub fn fail_single_task(
    conn: &mut Connection,
    task_id: &str,
    error: Option<&str>,
    status: FailStatus,
    run_id: Option<&str>,
    force: bool,
) -> TaskMgrResult<TaskFailResult> {
    let (previous_status_str, current_error_count): (String, i32) = conn
        .query_row(
            "SELECT status, error_count FROM tasks WHERE id = ?",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(task_id),
            _ => TaskMgrError::from(e),
        })?;
    let previous_status: TaskStatus = previous_status_str.parse()?;
    let new_status = match status {
        FailStatus::Blocked => TaskStatus::Blocked,
        FailStatus::Skipped => TaskStatus::Skipped,
        FailStatus::Irrelevant => TaskStatus::Irrelevant,
    };
    validate_transition(task_id, previous_status, new_status, force)?;

    let intent = TransitionIntent {
        task_id: task_id.to_string(),
        change: TransitionChange::Failed,
        source: TransitionSource::Operator,
        reason: error.map(String::from),
        fail_status: Some(status),
        audit_note: None,
    };
    let outcomes = {
        let mut lc = match run_id {
            Some(rid) => TaskLifecycle::with_run(conn, rid),
            None => TaskLifecycle::new(conn),
        };
        lc.apply(&[intent])
    };
    let outcome = &outcomes[0];
    if !outcome.applied
        && let Some(crate::lifecycle::TransitionRejectReason::DispatchFailed(msg)) = &outcome.reason
    {
        return Err(TaskMgrError::lock_error_with_hint(
            format!("fail dispatch failed for {task_id}: {msg}"),
            "internal lifecycle dispatch error; check earlier stderr for details",
        ));
    }

    Ok(TaskFailResult {
        task_id: task_id.to_string(),
        previous_status,
        new_status,
        error: error.map(String::from),
        error_count: current_error_count + 1,
        next_steps: generate_next_steps(status),
    })
}

/// Validate that the matrix transition is allowed. Lives at the CLI layer
/// (rather than inside `TaskLifecycle::apply`) because the hint text
/// references CLI affordances (`task-mgr next --claim`, `--force`).
fn validate_transition(
    task_id: &str,
    previous_status: TaskStatus,
    new_status: TaskStatus,
    force: bool,
) -> TaskMgrResult<()> {
    if force
        || matrix::validate(
            previous_status,
            new_status,
            matrix::TransitionSource::Operator,
        )
        .is_ok()
    {
        return Ok(());
    }
    let valid_transitions = previous_status.valid_transitions();
    let status_name = new_status.as_db_str();
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
    Err(TaskMgrError::invalid_transition(
        task_id,
        previous_status.to_string(),
        status_name,
        hint,
    ))
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
