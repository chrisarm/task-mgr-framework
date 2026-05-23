//! `TaskLifecycle::try_claim` — Category B race-safe pre-claim.
//!
//! Today's implementations:
//! - `commands/next/mod.rs:244` (CLI `next --claim`): `WHERE id=? AND status='todo'`
//! - `loop_engine/engine.rs:786` (`claim_slot_task`): `WHERE id=? AND status IN ('todo','in_progress')`
//!
//! The expected-status predicate MUST stay explicit (PRD FR-005) — hiding it
//! behind an unconditional `try_claim` method would change observable
//! optimistic-locking semantics.

use crate::TaskMgrError;
use crate::models::TaskStatus;

use super::TaskLifecycle;

impl<'a> TaskLifecycle<'a> {
    /// Race-safe pre-claim: transition `task_id` to `InProgress` only when
    /// its current status is one of `expected`.
    ///
    /// Returns `Ok(true)` when one row was updated, `Ok(false)` when the row
    /// existed but did not match `expected` (claim lost), and `Err(_)` for
    /// any DB failure. Callers MUST treat `Ok(false)` as "skip this task"
    /// rather than retry — the row may have advanced to a terminal state
    /// between selection and claim.
    ///
    /// `expected` is `&[TaskStatus]` to preserve the conditional-WHERE
    /// shape both CLI and engine paths use today. `try_claim(.., &[Todo])`
    /// reproduces the CLI behavior; `try_claim(.., &[Todo, InProgress])`
    /// reproduces the slot-claim idempotent retry-after-recovery shape
    /// (second call refreshes `started_at`).
    pub fn try_claim(&self, task_id: &str, expected: &[TaskStatus]) -> Result<bool, TaskMgrError> {
        if expected.is_empty() {
            return Ok(false);
        }

        let placeholders = expected.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "UPDATE tasks \
             SET status = 'in_progress', started_at = datetime('now'), updated_at = datetime('now') \
             WHERE id = ? AND status IN ({placeholders})"
        );

        let mut params: Vec<&str> = Vec::with_capacity(1 + expected.len());
        params.push(task_id);
        for s in expected {
            params.push(s.as_db_str());
        }

        let rows_affected = self
            .conn
            .execute(&sql, rusqlite::params_from_iter(params))?;
        Ok(rows_affected == 1)
    }
}
