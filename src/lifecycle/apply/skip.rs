//! `skip_one` — `InProgress/Todo → Skipped` transition.
//!
//! Pre-checks: rejects `Done` origin (matches legacy `skip_single_task`).
//! Side effects: appends `[SKIPPED] <reason>` to task notes; conditionally
//! updates `run_tasks` when a run context is present.

use rusqlite::params;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::TaskLifecycle;
use super::TransitionIntent;

impl<'a> TaskLifecycle<'a> {
    /// Inline skip implementation — owns the same side effects as the legacy
    /// `commands::skip::skip_single_task` without opening its own transaction.
    /// Callers that need multi-task atomicity must wrap `apply()` in an outer
    /// transaction (e.g. via pre-validation in `commands/skip.rs`).
    pub(super) fn skip_one(
        &mut self,
        intent: &TransitionIntent,
        previous: Option<TaskStatus>,
    ) -> TaskMgrResult<()> {
        let reason = intent.reason.as_deref().unwrap_or("<task-status> tag");

        // Done tasks cannot be skipped (matches legacy skip_single_task check).
        if previous == Some(TaskStatus::Done) {
            return Err(TaskMgrError::invalid_state(
                "Task",
                &intent.task_id,
                "todo or in_progress",
                "done",
            ));
        }

        // Read current notes — also validates the row exists.
        let current_notes: Option<String> = self
            .conn
            .query_row(
                "SELECT notes FROM tasks WHERE id = ?",
                [intent.task_id.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    TaskMgrError::task_not_found(&intent.task_id)
                }
                _ => TaskMgrError::from(e),
            })?;

        let new_notes = match current_notes {
            Some(existing) if !existing.is_empty() => {
                format!("{existing}\n\n[SKIPPED] {reason}")
            }
            _ => format!("[SKIPPED] {reason}"),
        };

        self.conn.execute(
            "UPDATE tasks SET status = 'skipped', notes = ?, updated_at = datetime('now') \
             WHERE id = ?",
            params![new_notes, intent.task_id.as_str()],
        )?;

        // Conditionally update run_tasks when run_id is present.
        if let Some(run_id) = self.run_id {
            let run_task_exists: bool = self
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM run_tasks WHERE run_id = ? AND task_id = ?)",
                    params![run_id, intent.task_id.as_str()],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if run_task_exists {
                self.conn.execute(
                    "UPDATE run_tasks SET status = 'skipped', notes = ?, \
                     ended_at = datetime('now') WHERE run_id = ? AND task_id = ?",
                    params![reason, run_id, intent.task_id.as_str()],
                )?;
            }
        }

        Ok(())
    }
}
