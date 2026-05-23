//! `TaskLifecycle::repair_stale` — Category D doctor heuristic repair.
//!
//! Today's sites:
//! - `commands/doctor/fixes.rs:30` (`fix_stale_task`) — stale
//!   `in_progress -> todo` with `[DOCTOR] Reset…` notes.
//! - `commands/doctor/fixes.rs:93` (`fix_git_reconciliation`) — git-derived
//!   completion: `in_progress -> done` with `[DOCTOR] Reconciled from git
//!   history…` notes.
//!
//! Kept distinct from `reconcile_from_prd` per the §6 doctor sub-decision
//! and PRD §FR-007 ("explicitly NOT consolidated"): the doctor verb does
//! not consult the PRD JSON, runs as a one-shot human command, and its
//! source-allowance set ([`TransitionSource::DoctorRepair`]) is narrower
//! than `ReconcilePrd`. Plan-building stays in `doctor/fixes.rs`.

use crate::TaskMgrError;
use crate::models::TaskStatus;

use super::TaskLifecycle;
use super::matrix::TransitionSource;

/// One repair item carried by a `RepairPlan`.
///
/// `audit_label`, when `Some`, is appended to the task's `notes` column on
/// success. Doctor plan-builders construct the full `[DOCTOR] …` text in
/// `doctor/fixes.rs` and pass it via this field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairItem {
    pub task_id: String,
    pub target: TaskStatus,
    pub audit_label: Option<String>,
}

/// Bag of repair items produced by `doctor/fixes.rs`.
#[derive(Debug, Clone, Default)]
pub struct RepairPlan {
    pub items: Vec<RepairItem>,
}

/// Summary of what `repair_stale` actually did.
#[derive(Debug, Clone, Default)]
pub struct RepairReport {
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
    /// Execute a `RepairPlan` produced by `doctor/fixes.rs`.
    ///
    /// Per-item partial-failure tolerance: same shape as
    /// [`Self::reconcile_from_prd`] — one failure does NOT abort the batch.
    ///
    /// Source: [`TransitionSource::DoctorRepair`] — permits `in_progress
    /// -> todo` (stale reset) and `in_progress -> done` (git
    /// reconciliation).
    pub fn repair_stale(&mut self, plan: RepairPlan) -> Result<RepairReport, TaskMgrError> {
        let report = self.apply_plan_with_source(&plan.items, TransitionSource::DoctorRepair)?;
        Ok(RepairReport {
            applied: report.applied,
            skipped: report.skipped,
            rejected: report.rejected,
        })
    }
}
