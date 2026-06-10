//! Category C recovery primitives — three bulk verbs.
//!
//! Today's call sites:
//! - `recover_in_progress_for_prefix` — `engine.rs:2407` (mid-run sweep),
//!   `engine.rs:3258` (Step 6.6 startup).
//! - `auto_block_after_failures` — `engine.rs:5145` (`auto_block_task`,
//!   inside `handle_task_failure` tx).
//! - `resurrect_for_iteration` — per-id reset (cf. `reset_task_to_todo`
//!   at `engine.rs:1642` + overflow rungs 1-3 at `overflow.rs:473`).
//!
//! All three issue a single UPDATE statement — SQLite autocommit makes each
//! call atomic (the "single transaction" contract from FEAT-005 AC).
//!
//! Source variant: every transition emitted by this module is
//! [`crate::lifecycle::matrix::TransitionSource::Recovery`].

use rusqlite::params;

use crate::TaskMgrError;
use crate::db::prefix::prefix_and;

use super::TaskLifecycle;

impl<'a> TaskLifecycle<'a> {
    /// Bulk reset every `in_progress` row (optionally scoped to `prefix`)
    /// back to `todo`. Idempotent — running twice is a no-op.
    ///
    /// `prefix` follows the [`prefix_and`] convention: the bare prefix (no
    /// trailing dash) is passed in; the helper appends `-%` to produce the
    /// LIKE pattern. Concurrent loops on different PRDs MUST NOT reset each
    /// other's rows — that's the whole point of the scope guard.
    ///
    /// Returns the number of rows updated.
    pub fn recover_in_progress_for_prefix(
        &self,
        prefix: Option<&str>,
    ) -> Result<usize, TaskMgrError> {
        let (clause, like_param) = prefix_and(prefix);
        let sql = format!(
            "UPDATE tasks SET status = 'todo', started_at = NULL, \
             updated_at = datetime('now') \
             WHERE status = 'in_progress' {clause}"
        );
        let rows = match like_param {
            Some(p) => self.conn.execute(&sql, [p])?,
            None => self.conn.execute(&sql, [])?,
        };
        Ok(rows)
    }

    /// Set `task_id` to `blocked` with `last_error = err` and
    /// `blocked_at_iteration = iteration`. Gated on `status = 'in_progress'`
    /// via conditional WHERE — terminal rows (done / irrelevant / blocked /
    /// skipped) are a clean `Ok(false)` no-op with NO stderr emission and
    /// NO `last_error` mutation, matching the legacy 0-rows-affected behavior
    /// at `engine.rs:5151`.
    ///
    /// Returns `Ok(true)` when one row was updated.
    pub fn auto_block_after_failures(
        &self,
        task_id: &str,
        err: &str,
        iteration: i64,
    ) -> Result<bool, TaskMgrError> {
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'blocked', last_error = ?, \
             blocked_at_iteration = ?, updated_at = datetime('now') \
             WHERE id = ? AND status = 'in_progress'",
            params![err, iteration, task_id],
        )?;
        Ok(rows > 0)
    }

    /// Reset a specific task to `todo` AND set `tasks.model = model` in a
    /// single atomic UPDATE. Gated on `status = 'in_progress'` via conditional
    /// WHERE so terminal rows are a clean `Ok(false)` no-op.
    ///
    /// Used by the rung-4 `FallbackToProvider` overflow recovery arm to
    /// atomically persist the Grok model before clearing `started_at`, so
    /// model resolution picks it up on the next iteration without an
    /// intermediate state window. Source: [`TransitionSource::Recovery`].
    ///
    /// Returns `Ok(true)` when one row was updated.
    pub fn resurrect_with_model_override(
        &self,
        task_id: &str,
        model: &str,
    ) -> Result<bool, crate::TaskMgrError> {
        let rows = self.conn.execute(
            "UPDATE tasks SET model = ?, status = 'todo', started_at = NULL, \
             updated_at = datetime('now') \
             WHERE id = ? AND status = 'in_progress'",
            params![model, task_id],
        )?;
        Ok(rows > 0)
    }

    /// Reset a specific set of task IDs back to `todo`. `prefix`, when
    /// `Some`, scopes the UPDATE via `id LIKE ? || '%'` so cross-PRD IDs in
    /// the slice are filtered at the DB boundary (no row touched).
    ///
    /// Unlike [`recover_in_progress_for_prefix`], the `prefix` argument here
    /// is appended raw (`prefix || '%'`) — callers pass `"FEAT-"` if they
    /// want the trailing-dash semantic. This matches the call shape at
    /// `engine.rs:1642` / `overflow.rs:473`.
    ///
    /// **Contract note**: This verb deliberately does *not* guard on
    /// `status = 'in_progress'` (unlike the bulk prefix recovery verb).
    /// Callers (wave FEAT-002 reset, overflow rungs) may list any task ID
    /// they want forced back to `todo` for the next iteration.
    ///
    /// An empty `ids` slice short-circuits to `Ok(0)` with no DB round-trip
    /// (the "no transaction commit" AC).
    pub fn resurrect_for_iteration(
        &self,
        prefix: Option<&str>,
        ids: &[&str],
    ) -> Result<usize, TaskMgrError> {
        if ids.is_empty() {
            return Ok(0);
        }

        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let like_pattern = prefix.map(|p| format!("{p}%"));
        let like_clause = if like_pattern.is_some() {
            " AND id LIKE ?"
        } else {
            ""
        };
        let sql = format!(
            "UPDATE tasks SET status = 'todo', started_at = NULL, \
             updated_at = datetime('now') \
             WHERE id IN ({placeholders}){like_clause}"
        );

        let mut bound: Vec<&str> = ids.to_vec();
        if let Some(p) = like_pattern.as_deref() {
            bound.push(p);
        }

        let rows = self.conn.execute(&sql, rusqlite::params_from_iter(bound))?;
        Ok(rows)
    }
}
