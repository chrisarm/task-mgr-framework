//! `TaskLifecycle::reconcile_from_prd` — Category D PRD-driven reconciliation.
//!
//! Today's sites:
//! - `loop_engine/prd_reconcile.rs:305` — PRD `passes: true` flip
//!   (`todo|in_progress -> done`).
//! - `loop_engine/prd_reconcile.rs:550` — PRD modification 'irrelevant'
//!   (`todo|in_progress -> irrelevant`).
//!
//! `ReconcilePlan` carries the items discovered by `prd_reconcile.rs`;
//! the verb here EXECUTES the plan. Plan-building stays in
//! `prd_reconcile.rs` per the §6 doctor sub-decision and PRD §FR-007
//! ("explicitly NOT consolidated") — do NOT migrate it into
//! `src/lifecycle/`.

use crate::TaskMgrError;
use crate::models::TaskStatus;

use super::TaskLifecycle;
use super::matrix::TransitionSource;

/// One reconciliation item carried by a `ReconcilePlan`.
///
/// The `target` is the desired status (typically `Done` or `Irrelevant`
/// for `ReconcilePrd`). `audit_label`, when `Some`, is appended to the
/// task's `notes` column on success — `prd_reconcile.rs` does not write
/// notes today, so this is usually `None` for the reconcile verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileItem {
    pub task_id: String,
    pub target: TaskStatus,
    pub audit_label: Option<String>,
}

/// Bag of reconciliation items produced by `prd_reconcile::*`.
///
/// Construction lives in `loop_engine/prd_reconcile.rs`; this module only
/// consumes plans.
#[derive(Debug, Clone, Default)]
pub struct ReconcilePlan {
    pub items: Vec<ReconcileItem>,
}

/// Summary of what `reconcile_from_prd` actually did.
#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    /// Number of items whose DB write touched a row.
    pub applied: usize,
    /// Number of items whose conditional-WHERE matched no rows (already
    /// terminal — a clean no-op, not a failure).
    pub skipped: usize,
    /// Task IDs whose transition was rejected — missing row, parse failure,
    /// matrix rejection, or DB error.
    pub rejected: Vec<String>,
}

impl<'a> TaskLifecycle<'a> {
    /// Execute a `ReconcilePlan` produced by `prd_reconcile::*`.
    ///
    /// Per-item partial-failure tolerance: a failure on one item does
    /// NOT abort the batch — the failing task id is appended to
    /// `ReconcileReport::rejected` and execution continues. Matches the
    /// per-task tolerance contract of [`Self::apply`].
    ///
    /// Source: [`TransitionSource::ReconcilePrd`] — permits `done ->
    /// irrelevant` and `todo|in_progress -> done` (matches
    /// `prd_reconcile.rs:305` and `:550`).
    pub fn reconcile_from_prd(
        &mut self,
        plan: ReconcilePlan,
    ) -> Result<ReconcileReport, TaskMgrError> {
        let report = self.apply_plan_with_source(&plan.items, TransitionSource::ReconcilePrd)?;
        Ok(ReconcileReport {
            applied: report.applied,
            skipped: report.skipped,
            rejected: report.rejected,
        })
    }
}
