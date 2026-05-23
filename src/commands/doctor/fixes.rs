//! Fix functions for the doctor command.
//!
//! These functions repair database inconsistencies detected by the checks:
//! - Reset stale in_progress tasks to todo
//! - Mark abandoned active runs as aborted
//! - Delete orphaned relationships

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::lifecycle::{RepairItem, RepairPlan, TaskLifecycle};
use crate::models::TaskStatus;

/// Fix a stale in_progress task by resetting to todo with audit note.
pub fn fix_stale_task(conn: &mut Connection, task_id: &str) -> TaskMgrResult<()> {
    let audit_note =
        "[DOCTOR] Reset from 'in_progress' to 'todo' - no active run tracking this task";
    let plan = RepairPlan {
        items: vec![RepairItem {
            task_id: task_id.to_string(),
            target: TaskStatus::Todo,
            audit_label: Some(audit_note.to_string()),
        }],
    };
    TaskLifecycle::new(conn).repair_stale(plan)?;
    Ok(())
}

/// Fix an active run by marking it as aborted.
pub fn fix_active_run(conn: &Connection, run_id: &str) -> TaskMgrResult<()> {
    // Add audit note to run
    let audit_note = "[DOCTOR] Marked as aborted - run was active without proper end";

    conn.execute(
        "UPDATE runs SET status = 'aborted', ended_at = datetime('now'), notes = ? WHERE run_id = ?",
        rusqlite::params![audit_note, run_id],
    )?;

    // Also update any 'started' run_tasks to 'failed'
    conn.execute(
        r#"
        UPDATE run_tasks
        SET status = 'failed',
            ended_at = datetime('now'),
            notes = '[DOCTOR] Marked as failed - parent run was aborted'
        WHERE run_id = ? AND status = 'started'
        "#,
        [run_id],
    )?;

    Ok(())
}

/// Fix a git reconciliation task by marking it as done.
///
/// Sets the task status to 'done' with an audit note referencing the git commit.
pub fn fix_git_reconciliation(
    conn: &mut Connection,
    task_id: &str,
    commit_msg: &str,
) -> TaskMgrResult<()> {
    let audit_note = if commit_msg.is_empty() {
        "[DOCTOR] Reconciled from git history - task found in commit log".to_string()
    } else {
        format!(
            "[DOCTOR] Reconciled from git history - commit: {}",
            commit_msg
        )
    };
    let plan = RepairPlan {
        items: vec![RepairItem {
            task_id: task_id.to_string(),
            target: TaskStatus::Done,
            audit_label: Some(audit_note),
        }],
    };
    TaskLifecycle::new(conn).repair_stale(plan)?;
    Ok(())
}

/// Fix an orphaned relationship by deleting it.
pub fn fix_orphaned_relationship(
    conn: &Connection,
    task_id: &str,
    related_id: &str,
    rel_type: &str,
) -> TaskMgrResult<()> {
    conn.execute(
        "DELETE FROM task_relationships WHERE task_id = ? AND related_id = ? AND rel_type = ?",
        rusqlite::params![task_id, related_id, rel_type],
    )?;

    Ok(())
}
