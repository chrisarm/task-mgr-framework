//! Category C recovery primitives — bulk + per-id Recovery verbs.
//!
//! Today's call sites:
//! - `recover_in_progress_for_prefix` — `startup.rs`, `iteration.rs`,
//!   `reactions/account.rs` (bulk stale / rate-limit sweeps).
//! - `recover_in_progress` — per-id orphan reclaim (17.5/17.6 via
//!   `reset_orphan_claim_to_todo`) and overflow rungs 1–3 (`post_output`).
//! - `reopen_after_merge_fail` — FEAT-002 merge-fail reopen of
//!   `in_progress|done` (via `wave_scheduler::reset_task_to_todo`).
//! - `auto_block_after_failures` — overflow rung 5 (`post_output`) and the
//!   `auto_block_task` shim inside `handle_task_failure`.
//! - `resurrect_for_iteration` — unguarded per-id force-to-todo escape hatch
//!   (tests + intentional force-any-status only — not orphan reclaim or merge-fail).
//!
//! Every verb issues a single UPDATE — SQLite autocommit makes each call
//! atomic (the "single transaction" contract from FEAT-005 AC). Gated verbs
//! use conditional WHERE on `status`; they do **not** SELECT-then-write
//! (learning #4810). `resurrect_for_iteration` deliberately omits a status
//! guard (learning #4358) — do not "fix" it.
//!
//! Source variant: every transition emitted by this module is
//! [`crate::lifecycle::matrix::TransitionSource::Recovery`].

use rusqlite::params;

use crate::TaskMgrError;
use crate::db::prefix::prefix_and;

use super::TaskLifecycle;

impl<'a> TaskLifecycle<'a> {
    /// Per-id orphan reclaim: `in_progress → todo` with `started_at` cleared.
    ///
    /// Atomic conditional UPDATE — no SELECT-then-write (learning #4810):
    /// ```sql
    /// UPDATE tasks SET status='todo', started_at=NULL, updated_at=datetime('now')
    /// WHERE id = ? AND status = 'in_progress'
    /// ```
    ///
    /// Returns `Ok(true)` iff exactly one row was updated. Missing rows,
    /// terminal statuses (`done`/`blocked`/`skipped`/`irrelevant`), and
    /// already-`todo` rows are a clean `Ok(false)` no-op (no log obligation
    /// here — wrappers log only on `true`).
    ///
    /// **Callers (post CONTRACT-001):** loop-exit 17.5/17.6 (FIX-001) and
    /// overflow rungs 1–3 (FIX-003). Not for merge-fail — that needs
    /// [`Self::reopen_after_merge_fail`] so premature `:done` on the
    /// ephemeral can reopen. Not a substitute for unguarded
    /// [`Self::resurrect_for_iteration`].
    pub fn recover_in_progress(&self, task_id: &str) -> Result<bool, TaskMgrError> {
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'todo', started_at = NULL, \
             updated_at = datetime('now') \
             WHERE id = ? AND status = 'in_progress'",
            params![task_id],
        )?;
        Ok(rows > 0)
    }

    /// Per-id merge-fail reopen: `in_progress|done → todo` with `started_at`
    /// cleared.
    ///
    /// Atomic conditional UPDATE — no SELECT-then-write (learning #4810):
    /// ```sql
    /// UPDATE tasks SET status='todo', started_at=NULL, updated_at=datetime('now')
    /// WHERE id = ? AND status IN ('in_progress', 'done')
    /// ```
    ///
    /// Returns `Ok(true)` iff exactly one row was updated. Terminal honest
    /// closes (`blocked`/`skipped`/`irrelevant`), already-`todo`, and missing
    /// rows are `Ok(false)`. The `done` arm is intentional: merge-fail after
    /// premature `:done` on a slot-local ephemeral must reopen so work is not
    /// stranded on the feature branch (a shared `in_progress`-only helper
    /// fails this contract).
    ///
    /// **Callers (post CONTRACT-001):** FEAT-002 merge-fail path only
    /// (FIX-002). Do not reuse for 17.5/17.6 orphan reclaim.
    pub fn reopen_after_merge_fail(&self, task_id: &str) -> Result<bool, TaskMgrError> {
        let rows = self.conn.execute(
            "UPDATE tasks SET status = 'todo', started_at = NULL, \
             updated_at = datetime('now') \
             WHERE id = ? AND status IN ('in_progress', 'done')",
            params![task_id],
        )?;
        Ok(rows > 0)
    }

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
    /// want the trailing-dash semantic. Production orphan / overflow /
    /// merge-fail callers have moved off this verb; it is the unguarded
    /// escape hatch (tests + intentional force-any-status only).
    ///
    /// **Contract note**: This verb deliberately does *not* guard on
    /// `status = 'in_progress'` (unlike the bulk prefix recovery verb).
    /// Callers may force any listed ID back to `todo`. Do **not** use for
    /// orphan reclaim (`recover_in_progress`) or merge-fail
    /// (`reopen_after_merge_fail`) — learning #4358.
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
