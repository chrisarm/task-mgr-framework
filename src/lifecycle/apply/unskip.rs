//! `unskip_one` — `Skipped → Todo` transition (with audit-note override path).
//!
//! Pre-checks: requires `Skipped` origin when no `audit_note` override is
//! supplied. When `audit_note` is `Some`, any origin is accepted — used by
//! `review.rs` auto path.
//! Side effects: appends audit note to task notes. Does NOT clear `last_error`
//! (distinct from `unblock_one`).

use rusqlite::params;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::TaskLifecycle;
use super::TransitionIntent;

impl<'a> TaskLifecycle<'a> {
    /// Inline unskip implementation — owns the SQL previously in
    /// `commands/unblock.rs::unskip`. Validates current status == Skipped
    /// (when no `audit_note` override; review.rs's auto path supplies one
    /// and skips the validation). Does NOT clear `last_error` (distinct
    /// from unblock_one).
    pub(crate) fn unskip_one(
        &mut self,
        intent: &TransitionIntent,
    ) -> TaskMgrResult<()> {
        let id = intent.task_id.as_str();

        let (status_str, current_notes): (String, Option<String>) = self
            .conn
            .query_row(
                "SELECT status, notes FROM tasks WHERE id = ?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(id),
                _ => TaskMgrError::from(e),
            })?;
        let previous: TaskStatus = status_str.parse()?;

        if intent.audit_note.is_none() && previous != TaskStatus::Skipped {
            return Err(TaskMgrError::invalid_state(
                "Task",
                id,
                "skipped",
                previous.to_string(),
            ));
        }

        let audit = intent
            .audit_note
            .clone()
            .unwrap_or_else(|| "[UNSKIPPED] Returned to todo from skipped status".to_string());
        let new_notes = match &current_notes {
            Some(existing) if !existing.is_empty() => format!("{existing}\n\n{audit}"),
            _ => audit,
        };
        self.conn.execute(
            "UPDATE tasks SET status = ?, notes = ?, updated_at = datetime('now') WHERE id = ?",
            params![TaskStatus::Todo.as_db_str(), new_notes, id],
        )?;
        Ok(())
    }
}
