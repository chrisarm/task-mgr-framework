//! `complete_one` — `* → Done` transition.
//!
//! Pre-checks: none (matrix gate in `apply_one`; idempotent on `Done → Done`).
//! Side effects: sets `completed_at`, resets `consecutive_failures`,
//! conditionally updates `run_tasks` when a run context is present.

use rusqlite::params;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::TaskLifecycle;
use super::TransitionIntent;

impl<'a> TaskLifecycle<'a> {
    /// Inline complete implementation — owns the SQL previously in
    /// `commands/complete.rs::complete_single_task`. The CLI complete()
    /// retains its own dependency / required-tests / force gating BEFORE
    /// calling `apply()`; this helper assumes those checks already passed.
    /// Idempotent on already-Done tasks (no SQL write).
    ///
    /// `previous` is threaded from `apply_one`. When present we use it for the
    /// audit label and skip the status SELECT. When absent we fall back to a
    /// real read (no behavior change).
    pub(crate) fn complete_one(
        &mut self,
        intent: &TransitionIntent,
        previous: Option<TaskStatus>,
    ) -> TaskMgrResult<()> {
        let id = intent.task_id.as_str();
        let previous = match previous {
            Some(p) => p,
            None => {
                let previous_status_str: String = self
                    .conn
                    .query_row("SELECT status FROM tasks WHERE id = ?", [id], |row| {
                        row.get(0)
                    })
                    .map_err(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(id),
                        _ => TaskMgrError::from(e),
                    })?;
                previous_status_str.parse()?
            }
        };

        if previous != TaskStatus::Done {
            self.conn.execute(
                "UPDATE tasks SET status = 'done', completed_at = datetime('now'), \
                 updated_at = datetime('now') WHERE id = ?",
                [id],
            )?;
            // Reset consecutive_failures on success (column added by v13).
            if let Err(e) = self.conn.execute(
                "UPDATE tasks SET consecutive_failures = 0 WHERE id = ?",
                [id],
            ) {
                eprintln!("Warning: failed to reset consecutive_failures for {id}: {e}");
            }
        }

        if let Some(rid) = self.run_id {
            let run_task_exists: bool = self
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM run_tasks WHERE run_id = ? AND task_id = ?)",
                    params![rid, id],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if run_task_exists {
                self.conn.execute(
                    "UPDATE run_tasks SET status = 'completed', \
                     ended_at = datetime('now'), \
                     duration_seconds = CAST((julianday('now') - julianday(started_at)) * 86400 AS INTEGER) \
                     WHERE run_id = ? AND task_id = ? AND status = 'started'",
                    params![rid, id],
                )?;
            }
        }
        Ok(())
    }
}
