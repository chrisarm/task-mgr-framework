//! `TaskLifecycle` — single source of truth for `tasks.status` writes.
//!
//! This module owns every mutation of the `tasks.status` column plus its
//! side effects (`run_tasks` bookkeeping, PRD JSON sync, decay columns,
//! notes formatting, exact stderr warning shape). Six public verbs cover
//! all five audit categories from PRD §6:
//!
//! - **Category A** (user-intent + LoopStatusTag) — [`TaskLifecycle::apply`]
//! - **Category B** (race-safe pre-claim) — [`TaskLifecycle::try_claim`]
//! - **Category C** (bulk recovery) —
//!   [`TaskLifecycle::recover_in_progress_for_prefix`],
//!   [`TaskLifecycle::auto_block_after_failures`],
//!   [`TaskLifecycle::resurrect_for_iteration`],
//!   [`TaskLifecycle::decay_reset`]
//! - **Category D** (PRD-driven) — [`TaskLifecycle::reconcile_from_prd`]
//! - **Category D** (doctor heuristic) — [`TaskLifecycle::repair_stale`]
//!
//! `reconcile_from_prd` and `repair_stale` are kept distinct per the §6
//! doctor sub-decision: doctor never consults the PRD JSON and its
//! source-allowance set ([`TransitionSource::DoctorRepair`]) is narrower
//! than `ReconcilePrd`. Folding them is explicitly prohibited.
//!
//! # Hard contracts preserved bit-identically
//!
//! 1. Auto-claim on `<task-status>:done` for `Todo` rows (today at
//!    `engine.rs:4724`).
//! 2. Per-task partial-failure tolerance in [`TaskLifecycle::apply`]
//!    (learning #2284 / #2238 — NEVER convert to batch-level
//!    `Result<(), Err>`).
//! 3. DB-authoritative-PRD-best-effort — PRD JSON sync failures never
//!    block the DB write.
//! 4. Exact stderr warning shape (locked by `tests/lifecycle_stderr_contract.rs`):
//!    `Warning: <task-status> dispatched {id} to done in DB but PRD JSON sync failed ({path}): {err}`
//! 5. Conditional-WHERE in [`TaskLifecycle::try_claim`] — the expected-status
//!    set MUST stay explicit (PRD FR-005).

use std::path::Path;

use rusqlite::Connection;

use crate::models::TaskStatus;

pub mod apply;
pub mod claim;
pub mod decay;
pub mod matrix;
pub(crate) mod plan_apply;
pub mod reconcile;
pub mod recovery;
pub mod repair;

mod tests;

pub use apply::{TransitionChange, TransitionIntent, TransitionOutcome, TransitionRejectReason};
pub use decay::{DecayItem, DecayPlan, DecayReport};
pub use matrix::TransitionSource;
pub use reconcile::{ReconcileItem, ReconcilePlan, ReconcileReport};
pub use repair::{RepairItem, RepairPlan, RepairReport};

/// The lifecycle service. Holds borrows to the DB connection and (when
/// invoked from the loop engine) the run context required for `run_tasks`
/// bookkeeping and PRD JSON sync.
///
/// Construction patterns:
///
/// - `TaskLifecycle::new(conn)` — CLI direct paths (no run, no PRD sync).
/// - `TaskLifecycle::with_run(conn, run_id)` — loop iterations.
/// - `…with_prd_sync(path, prefix)` — chained onto either constructor when
///   the caller wants DB writes to flip the PRD JSON `passes` field too
///   (loop engine + CLI `complete`).
///
/// The struct stores `&'a mut Connection`: [`TaskLifecycle::apply`] dispatches
/// through `complete_cmd::complete` / `fail` / `skip` / `irrelevant` whose
/// signatures all require `&mut Connection` (they open their own transactions
/// internally). Read-only verbs like [`TaskLifecycle::try_claim`] still take
/// `&self` — Rust auto-reborrows `&mut Connection` as `&Connection` when the
/// method only needs `&Connection::execute`.
pub struct TaskLifecycle<'a> {
    pub(crate) conn: &'a mut Connection,
    pub(crate) run_id: Option<&'a str>,
    pub(crate) prd_json_path: Option<&'a Path>,
    pub(crate) task_prefix: Option<&'a str>,
}

impl<'a> TaskLifecycle<'a> {
    /// Construct a CLI-direct lifecycle (no run context, no PRD JSON sync).
    /// Matches the call shape of today's `complete::complete(conn, ..)`
    /// invocations made from the CLI.
    #[must_use]
    pub fn new(conn: &'a mut Connection) -> Self {
        Self {
            conn,
            run_id: None,
            prd_json_path: None,
            task_prefix: None,
        }
    }

    /// Construct a loop-iteration lifecycle. `run_id` threads into `run_tasks`
    /// bookkeeping the same way today's `apply_status_updates(.., Some(run_id), ..)` path does.
    #[must_use]
    pub fn with_run(conn: &'a mut Connection, run_id: &'a str) -> Self {
        Self {
            conn,
            run_id: Some(run_id),
            prd_json_path: None,
            task_prefix: None,
        }
    }

    /// Enable PRD JSON sync. Subsequent `Done` transitions flip the
    /// `passes` field of the named story in `prd_json_path` (matched by
    /// `task_prefix`). Failures print the exact stderr line
    /// `"PRD JSON sync failed for {task}: {err}\n"` and do NOT abort
    /// the DB write — that's the DB-authoritative-PRD-best-effort
    /// invariant.
    #[must_use]
    pub fn with_prd_sync(mut self, path: &'a Path, prefix: &'a str) -> Self {
        self.prd_json_path = Some(path);
        self.task_prefix = Some(prefix);
        self
    }
}

/// Read `tasks.status` for `id`, returning `None` when the row is missing or
/// the stored string fails to parse. Internal SSoT shared by apply/decay/
/// plan_apply — keeping a single helper avoids three drifting copies of a
/// trivial-but-load-bearing SELECT.
pub(crate) fn read_status(conn: &Connection, id: &str) -> Option<TaskStatus> {
    conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |row| {
        row.get::<_, String>(0)
    })
    .ok()
    .and_then(|s| s.parse().ok())
}
