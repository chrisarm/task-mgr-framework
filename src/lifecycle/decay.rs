//! `TaskLifecycle::decay_reset` — Category C bulk decay reset.
//!
//! Today's site:
//! - `commands/next/decay.rs:127` (`apply_decay`) — auto-resets stale
//!   blocked/skipped tasks back to `todo` with the `[DECAY]` audit note.
//!
//! Distinct from [`super::recovery`] verbs because decay legitimately starts
//! from `Blocked` or `Skipped` (the caller has already pre-filtered by
//! age-vs-threshold) — `recover_in_progress_for_prefix` would reject those.
//! The audit-note append uses SQL `CASE WHEN` so the read-and-write happens
//! in a single statement (no SELECT-then-UPDATE round-trip on `notes`, which
//! the legacy site had).
//!
//! Matrix source: [`TransitionSource::DecayReset`] — permits
//! `Blocked → Todo` and `Skipped → Todo`. `Todo → Todo` falls through the
//! reflexive same-status branch as a no-op (`skipped` in the report).

use rusqlite::params;

use crate::TaskMgrError;
use crate::models::TaskStatus;

use super::TaskLifecycle;
use super::matrix::{self, TransitionSource};

/// One decay item carried by a [`DecayPlan`].
///
/// `audit_label` is the full `[DECAY] …` string the caller pre-formatted. It
/// is appended to the task's `notes` column in the same UPDATE statement via
/// `CASE WHEN`, so the legacy SELECT-then-UPDATE round-trip is eliminated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecayItem {
    pub task_id: String,
    pub audit_label: String,
}

/// Bag of decay items produced by `commands/next/decay.rs::apply_decay`.
#[derive(Debug, Clone, Default)]
pub struct DecayPlan {
    pub items: Vec<DecayItem>,
}

/// Summary of what [`TaskLifecycle::decay_reset`] actually did.
#[derive(Debug, Clone, Default)]
pub struct DecayReport {
    /// Number of items whose UPDATE wrote a row.
    pub applied: usize,
    /// Number of items already at `Todo` — a clean no-op, not a failure.
    pub skipped: usize,
    /// Task IDs whose transition was rejected — missing row, parse failure,
    /// matrix rejection (e.g. status was `Done` or `Irrelevant`), or DB error.
    pub rejected: Vec<String>,
}

impl<'a> TaskLifecycle<'a> {
    /// Execute a [`DecayPlan`] under [`TransitionSource::DecayReset`].
    ///
    /// Per-item flow:
    /// 1. Read the current `tasks.status`. Missing row → `rejected`.
    /// 2. Validate `(from, Todo, DecayReset)` through [`matrix::validate`].
    ///    Rejection → `rejected`.
    /// 3. If `from == Todo`, increment `skipped` (idempotent no-op).
    /// 4. Otherwise execute a single atomic UPDATE: status flips to `todo`,
    ///    decay-iteration columns clear, and the `audit_label` is appended
    ///    to `notes` via `CASE WHEN notes IS NULL OR notes = '' THEN ?
    ///    ELSE notes || char(10) || char(10) || ? END`.
    ///
    /// Per-item partial-failure tolerance: a single item's failure NEVER
    /// aborts the batch — failures land in `rejected` and iteration continues.
    ///
    /// Empty `plan.items` returns `DecayReport::default()` with no DB writes.
    pub fn decay_reset(&mut self, plan: DecayPlan) -> Result<DecayReport, TaskMgrError> {
        let mut report = DecayReport::default();
        if plan.items.is_empty() {
            return Ok(report);
        }

        for item in &plan.items {
            let from = match super::read_status(self.conn, &item.task_id) {
                Some(s) => s,
                None => {
                    report.rejected.push(item.task_id.clone());
                    continue;
                }
            };

            if matrix::validate(from, TaskStatus::Todo, TransitionSource::DecayReset).is_err() {
                report.rejected.push(item.task_id.clone());
                continue;
            }

            if from == TaskStatus::Todo {
                report.skipped += 1;
                continue;
            }

            // Atomic single-statement UPDATE: notes append via CASE WHEN
            // eliminates the legacy SELECT notes + UPDATE round-trip. The
            // matrix gate above has already rejected non-decay-eligible
            // statuses, so a conditional WHERE on status is unnecessary
            // and would diverge from the legacy decay site's behavior.
            let rows = self.conn.execute(
                "UPDATE tasks SET status = 'todo', \
                 blocked_at_iteration = NULL, \
                 skipped_at_iteration = NULL, \
                 notes = CASE WHEN notes IS NULL OR notes = '' \
                         THEN ?1 \
                         ELSE notes || char(10) || char(10) || ?1 END, \
                 updated_at = datetime('now') \
                 WHERE id = ?2",
                params![item.audit_label, item.task_id],
            );

            match rows {
                Ok(n) if n > 0 => report.applied += 1,
                Ok(_) => report.rejected.push(item.task_id.clone()),
                Err(_) => report.rejected.push(item.task_id.clone()),
            }
        }

        Ok(report)
    }
}
