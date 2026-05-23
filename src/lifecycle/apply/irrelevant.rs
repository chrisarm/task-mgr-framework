//! `irrelevant_one` — `* → Irrelevant` transition via the direct irrelevant command.
//!
//! Pre-checks: rejects `Done` origin.
//! Side effects: appends `[IRRELEVANT] <reason>` (or `audit_note` override)
//! to notes; conditionally updates `run_tasks`. Does NOT increment
//! `error_count` — distinct from `fail_one` with `FailStatus::Irrelevant`.

use rusqlite::params;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::TaskLifecycle;
use super::TransitionIntent;

impl<'a> TaskLifecycle<'a> {
    /// Inline irrelevant implementation — owns the SQL previously in
    /// `commands/irrelevant.rs::irrelevant_single_task`. Does NOT increment
    /// error_count (distinct from fail_one with FailStatus::Irrelevant).
    /// Validates that the task is not already Done.
    pub(crate) fn irrelevant_one(
        &mut self,
        intent: &TransitionIntent,
        previous: Option<TaskStatus>,
    ) -> TaskMgrResult<()> {
        let id = intent.task_id.as_str();
        if previous == Some(TaskStatus::Done) {
            return Err(TaskMgrError::invalid_state("Task", id, "not done", "done"));
        }

        let current_notes: Option<String> = self
            .conn
            .query_row("SELECT notes FROM tasks WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(id),
                _ => TaskMgrError::from(e),
            })?;

        let reason = intent.reason.as_deref().unwrap_or("<task-status> tag");
        let audit = intent
            .audit_note
            .clone()
            .unwrap_or_else(|| format!("[IRRELEVANT] {reason}"));
        let new_notes = match current_notes {
            Some(existing) if !existing.is_empty() => format!("{existing}\n\n{audit}"),
            _ => audit,
        };

        self.conn.execute(
            "UPDATE tasks SET status = 'irrelevant', notes = ?, updated_at = datetime('now') \
             WHERE id = ?",
            params![new_notes, id],
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
                self.conn.execute(
                    "UPDATE run_tasks SET status = 'skipped', notes = ?, \
                     ended_at = datetime('now') WHERE run_id = ? AND task_id = ?",
                    params![reason, rid, id],
                )?;
            }
        }
        Ok(())
    }
}
