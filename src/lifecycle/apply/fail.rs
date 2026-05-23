//! `fail_one` — `* → Blocked/Skipped/Irrelevant` transition via the fail verb.
//!
//! Pre-checks: none (matrix gate is the CLI fail path's responsibility).
//! Side effects: increments `error_count`, records `blocked_at_iteration` /
//! `skipped_at_iteration` for decay tracking, appends a `[BLOCKED/SKIPPED/
//! IRRELEVANT]` prefix to notes, conditionally updates `run_tasks`.
//!
//! Distinct from `irrelevant_one`: this path increments `error_count`;
//! `irrelevant_one` (direct `irrelevant` command) does not.

use rusqlite::params;

use crate::cli::FailStatus;
use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::TaskLifecycle;
use super::TransitionIntent;

impl<'a> TaskLifecycle<'a> {
    /// Inline fail implementation — owns the SQL previously in
    /// `commands/fail/transition.rs::fail_single_task`. Handles all three
    /// `FailStatus` variants (Blocked / Skipped / Irrelevant) with shared
    /// error_count increment + decay-iteration tracking + per-status notes
    /// prefix. The CLI fail() retains its !force matrix-validation gate.
    pub(crate) fn fail_one(
        &mut self,
        intent: &TransitionIntent,
        fail_status: FailStatus,
    ) -> TaskMgrResult<()> {
        let id = intent.task_id.as_str();
        let error = intent.reason.as_deref();

        let (current_error_count, current_notes): (i32, Option<String>) = self
            .conn
            .query_row(
                "SELECT error_count, notes FROM tasks WHERE id = ?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(id),
                _ => TaskMgrError::from(e),
            })?;

        let new_status = match fail_status {
            FailStatus::Blocked => TaskStatus::Blocked,
            FailStatus::Skipped => TaskStatus::Skipped,
            FailStatus::Irrelevant => TaskStatus::Irrelevant,
        };
        let new_error_count = current_error_count + 1;
        let status_prefix = match fail_status {
            FailStatus::Blocked => "[BLOCKED]",
            FailStatus::Skipped => "[SKIPPED]",
            FailStatus::Irrelevant => "[IRRELEVANT]",
        };
        let new_notes = match (&current_notes, error) {
            (Some(existing), Some(err)) if !existing.is_empty() => {
                format!("{existing}\n\n{status_prefix} {err}")
            }
            (Some(existing), None) if !existing.is_empty() => {
                format!("{existing}\n\n{status_prefix}")
            }
            (_, Some(err)) => format!("{status_prefix} {err}"),
            (_, None) => status_prefix.to_string(),
        };

        let current_iteration: i64 = self
            .conn
            .query_row(
                "SELECT iteration_counter FROM global_state WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let (blocked_at, skipped_at) = match fail_status {
            FailStatus::Blocked => (Some(current_iteration), None::<i64>),
            FailStatus::Skipped => (None::<i64>, Some(current_iteration)),
            FailStatus::Irrelevant => (None::<i64>, None::<i64>),
        };

        self.conn.execute(
            "UPDATE tasks SET status = ?, error_count = ?, last_error = ?, notes = ?, \
             blocked_at_iteration = COALESCE(?, blocked_at_iteration), \
             skipped_at_iteration = COALESCE(?, skipped_at_iteration), \
             updated_at = datetime('now') WHERE id = ?",
            params![
                new_status.to_string(),
                new_error_count,
                error,
                new_notes,
                blocked_at,
                skipped_at,
                id
            ],
        )?;

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
                let run_task_status = match fail_status {
                    FailStatus::Blocked => "failed",
                    FailStatus::Skipped | FailStatus::Irrelevant => "skipped",
                };
                self.conn.execute(
                    "UPDATE run_tasks SET status = ?, notes = ?, ended_at = datetime('now') \
                     WHERE run_id = ? AND task_id = ?",
                    params![run_task_status, error.unwrap_or(""), rid, id],
                )?;
            }
        }
        Ok(())
    }
}
