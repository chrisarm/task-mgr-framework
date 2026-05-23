//! `unblock_one` — `Blocked → Todo` transition (with audit-note override path).
//!
//! Pre-checks: requires `Blocked` origin when no `audit_note` override is
//! supplied. When `audit_note` is `Some`, any origin is accepted — used by
//! `review.rs` `[AUTO-UNBLOCKED]` and `[RESOLVED]` paths.
//! Side effects: clears `last_error`, appends audit note to task notes.

use rusqlite::params;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::TaskLifecycle;
use super::TransitionIntent;

impl<'a> TaskLifecycle<'a> {
    /// Inline unblock implementation — owns the SQL previously in
    /// `commands/unblock.rs::unblock`. Validates current status == Blocked
    /// (unless the caller passed `audit_note` override, in which case the
    /// validation is the caller's responsibility — used by review.rs's
    /// `[AUTO-UNBLOCKED]` and `[RESOLVED]` paths which cycle ANY status to
    /// todo). Clears `last_error`.
    pub(crate) fn unblock_one(
        &mut self,
        intent: &TransitionIntent,
        previous: Option<TaskStatus>,
    ) -> TaskMgrResult<()> {
        let id = intent.task_id.as_str();

        // Use threaded previous when available to avoid re-reading status.
        // Falls back to real SELECT + parse when None (preserves fail-fast).
        // We still need notes for the audit append.
        let previous = match previous {
            Some(p) => p,
            None => {
                let status_str: String = self
                    .conn
                    .query_row("SELECT status FROM tasks WHERE id = ?", [id], |row| {
                        row.get(0)
                    })
                    .map_err(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => {
                            TaskMgrError::task_not_found(id)
                        }
                        _ => TaskMgrError::from(e),
                    })?;
                status_str.parse()?
            }
        };

        let current_notes: Option<String> = self
            .conn
            .query_row("SELECT notes FROM tasks WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(id),
                _ => TaskMgrError::from(e),
            })?;

        // When the caller supplied an audit_note override (review.rs auto and
        // resolve paths) we skip the Blocked-only check and let any state
        // transition to Todo with last_error cleared. The CLI unblock path
        // leaves audit_note = None and gets the legacy strict validation.
        if intent.audit_note.is_none() && previous != TaskStatus::Blocked {
            return Err(TaskMgrError::invalid_state(
                "Task",
                id,
                "blocked",
                previous.to_string(),
            ));
        }

        let audit = intent
            .audit_note
            .clone()
            .unwrap_or_else(|| "[UNBLOCKED] Returned to todo from blocked status".to_string());
        let new_notes = match &current_notes {
            Some(existing) if !existing.is_empty() => format!("{existing}\n\n{audit}"),
            _ => audit,
        };
        self.conn.execute(
            "UPDATE tasks SET status = ?, last_error = NULL, notes = ?, \
             updated_at = datetime('now') WHERE id = ?",
            params![TaskStatus::Todo.as_db_str(), new_notes, id],
        )?;
        Ok(())
    }
}
