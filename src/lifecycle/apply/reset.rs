//! `reset_one` — `* → Todo` transition via the reset command.
//!
//! Pre-checks: rejects `Todo → Todo` same-state reset.
//! Side effects: clears `started_at`, `completed_at`, `last_error`;
//! increments `error_count` (per legacy reset semantics); appends
//! `[RESET] Reset to todo from <prev> status` (or `audit_note` override).

use rusqlite::params;

use crate::models::TaskStatus;
use crate::{TaskMgrError, TaskMgrResult};

use super::TaskLifecycle;
use super::TransitionIntent;

impl<'a> TaskLifecycle<'a> {
    /// Inline reset implementation — owns the SQL previously in
    /// `commands/reset.rs::reset_single_task`. Rejects same-status (Todo)
    /// resets, clears `started_at` / `completed_at` / `last_error`, and
    /// increments `error_count` (per legacy semantics).
    pub(crate) fn reset_one(
        &mut self,
        intent: &TransitionIntent,
        previous: Option<TaskStatus>,
    ) -> TaskMgrResult<()> {
        let id = intent.task_id.as_str();

        // Determine previous status, preserving fail-fast on corrupt data.
        // If apply_one already gave us a good value, use it (saves a SELECT).
        // Otherwise fall back to a real SELECT + parse? (restores legacy behavior
        // for bad DB rows). This is the approved remediation design for the
        // H2 threading.
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

        if previous == TaskStatus::Todo {
            return Err(TaskMgrError::invalid_state(
                "Task",
                id,
                "non-todo status",
                "todo",
            ));
        }

        let (current_notes, error_count): (Option<String>, i64) = self
            .conn
            .query_row(
                "SELECT notes, error_count FROM tasks WHERE id = ?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => TaskMgrError::task_not_found(id),
                _ => TaskMgrError::from(e),
            })?;

        let audit = intent
            .audit_note
            .clone()
            .unwrap_or_else(|| {
                format!("[RESET] Reset to todo from {previous} status")
            });
        let new_notes = match &current_notes {
            Some(existing) if !existing.is_empty() => format!("{existing}\n\n{audit}"),
            _ => audit,
        };
        self.conn.execute(
            "UPDATE tasks SET status = ?, started_at = NULL, completed_at = NULL, \
             last_error = NULL, error_count = ?, notes = ?, updated_at = datetime('now') \
             WHERE id = ?",
            params![TaskStatus::Todo.as_db_str(), error_count + 1, new_notes, id],
        )?;
        Ok(())
    }
}
