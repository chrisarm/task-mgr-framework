//! Transition matrix validator — (from, to, source) triples per PRD §6.
//!
//! The Operator/LoopStatusTag baseline matches `TaskStatus::can_transition_to`
//! bit-identically. Recovery, ReconcilePrd, and DoctorRepair each layer
//! source-specific allowances on top of the baseline.
//!
//! ## Matrix consultation policy
//!
//! This module is the authoritative `(from, to, source)` allowance table.
//! Not every code path consults it on every transition — the design is
//! intentionally asymmetric for two reasons:
//!
//! **apply_one (Category A, user-intent):** `apply_one` in `apply.rs` consults
//! [`validate`] **only for the `Done` variant** (the `TransitionChange::Done`
//! arm). The other five per-variant helpers — `skip_one`, `fail_one`,
//! `irrelevant_one`, `unblock_one`, `unskip_one`, and `reset_one` — each apply
//! narrower inline pre-checks instead. This asymmetry is deliberate and cannot
//! be mechanically unified:
//!
//! - `fail_one` accepts any current status; the matrix rejects `Done → Failed`.
//! - `unblock_one` and `unskip_one` have an `audit_note` override path (used by
//!   `review.rs`'s `[AUTO-UNBLOCKED]` / `[RESOLVED]` flows) that bypasses the
//!   strict-status guard — the matrix has no model for this override.
//! - `complete_one` is idempotent on `Done → Done`; the matrix short-circuits
//!   same-status via `from == to` at the top of [`validate`], so the asymmetry
//!   there is harmless, but the matrix is not called at all for that path.
//!
//! **Plan-driven paths (Categories C & D):** `reconcile_from_prd`, `repair_stale`,
//! and `decay_reset` call [`validate`] unconditionally via
//! `apply_plan_with_source` in `plan_apply.rs`. Every task in the plan is
//! checked against the matrix before any SQL write.
//!
//! **Unification is PRD 2's responsibility.** Routing all `apply_one` variants
//! behind [`validate`] would change observable behavior (break `fail_one`'s
//! any-status acceptance, break the `audit_note` override paths). That work
//! requires modeling `audit_note` overrides and per-variant idempotency in the
//! matrix schema, and is explicitly deferred to the Engine Orchestration
//! Boundaries PRD.

use crate::models::TaskStatus;

use super::apply::TransitionRejectReason;

/// Source of a status transition. Different sources may permit different
/// transitions for the same `(from, to)` pair — e.g. `ReconcilePrd` is the
/// only source allowed to flip `Done -> Irrelevant`, and `Recovery` /
/// `DoctorRepair` are the only sources allowed to reset `InProgress -> Todo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionSource {
    /// CLI direct user-intent (complete / fail / skip / unblock / unskip).
    Operator,
    /// Loop engine consuming `<task-status>...</task-status>` side-band tags.
    LoopStatusTag,
    /// Category C bulk recovery (recover_in_progress / auto_block / resurrect).
    Recovery,
    /// Category D PRD-driven reconciliation (allows certain terminal flips).
    ReconcilePrd,
    /// Category D doctor heuristic repair (stale in_progress -> todo/done).
    DoctorRepair,
    /// Category C bulk decay reset (blocked/skipped -> todo after age threshold).
    DecayReset,
}

/// Validate a `(from, to, source)` transition triple.
///
/// Returns `Ok(())` when the transition is permitted under the source's
/// matrix, `Err(TransitionRejectReason::InvalidTransition { .. })` otherwise.
/// Same-status (`from == to`) is always permitted as a no-op.
pub fn validate(
    from: TaskStatus,
    to: TaskStatus,
    source: TransitionSource,
) -> Result<(), TransitionRejectReason> {
    if from == to {
        return Ok(());
    }

    // Operator baseline — matches TaskStatus::can_transition_to bit-identically.
    let baseline = matches!(
        (from, to),
        (TaskStatus::Todo, TaskStatus::InProgress)
            | (TaskStatus::InProgress, TaskStatus::Done)
            | (TaskStatus::InProgress, TaskStatus::Blocked)
            | (TaskStatus::InProgress, TaskStatus::Skipped)
            | (TaskStatus::InProgress, TaskStatus::Irrelevant)
            | (TaskStatus::Blocked, TaskStatus::Todo)
            | (TaskStatus::Skipped, TaskStatus::Todo)
    );

    let allowed = match source {
        // Both CLI and loop side-band share the same user-intent semantics.
        TransitionSource::Operator | TransitionSource::LoopStatusTag => baseline,

        // Category C: reclaim stuck in_progress back to todo (recover_in_progress).
        TransitionSource::Recovery => {
            baseline || matches!((from, to), (TaskStatus::InProgress, TaskStatus::Todo))
        }

        // Category D PRD reconciliation: may flip done→irrelevant when a story is
        // removed from the PRD, and todo→done / in_progress→done when passes:true
        // (prd_reconcile.rs:305 uses WHERE status IN ('todo', 'in_progress')).
        // Also allows todo→irrelevant (prd_reconcile.rs:550 irrelevant mutation,
        // which used WHERE status IN ('todo', 'in_progress')).
        TransitionSource::ReconcilePrd => {
            baseline
                || matches!(
                    (from, to),
                    (TaskStatus::Done, TaskStatus::Irrelevant)
                        | (TaskStatus::Todo, TaskStatus::Done)
                        | (TaskStatus::Todo, TaskStatus::Irrelevant)
                )
        }

        // Category D doctor heuristic: stale in_progress reset to todo
        // (doctor/fixes.rs:30) and git-derived done-mark from any non-terminal
        // status (doctor/fixes.rs:93's legacy SQL had no status WHERE clause,
        // so a `todo` task with a matching git commit also flipped to `done`).
        TransitionSource::DoctorRepair => {
            baseline
                || matches!(
                    (from, to),
                    (TaskStatus::InProgress, TaskStatus::Todo)
                        | (TaskStatus::Todo, TaskStatus::Done)
                )
        }

        // Category C decay reset: blocked/skipped → todo after age threshold.
        // The baseline already covers both transitions; no extra allowances
        // are needed. Explicitly enumerating them anyway keeps the matrix
        // self-documenting — a future change to the baseline must not
        // silently drop decay's two legitimate transitions.
        TransitionSource::DecayReset => matches!(
            (from, to),
            (TaskStatus::Blocked, TaskStatus::Todo) | (TaskStatus::Skipped, TaskStatus::Todo)
        ),
    };

    if allowed {
        Ok(())
    } else {
        Err(TransitionRejectReason::InvalidTransition { from, to, source })
    }
}

/// Return the full set of `from` statuses that may legally transition INTO
/// `target` under `source` (covering both the Operator baseline and the
/// source-specific extensions / narrowings). Used by plan-driven verbs
/// (decay / reconcile / repair) that want to surface the matrix-permitted
/// starting states without iterating every status manually.
///
/// Same-status (`target` itself) is NOT included — callers that want to count
/// a reflexive transition as a no-op handle that branch separately (the
/// matrix's [`validate`] is the canonical place where `from == to` is
/// short-circuited as `Ok(())`).
///
/// Only the three plan-driven sources ([`TransitionSource::ReconcilePrd`],
/// [`TransitionSource::DoctorRepair`], [`TransitionSource::DecayReset`]) have
/// populated entries — the function returns an empty slice for any other
/// `source`, including `Operator` / `LoopStatusTag` / `Recovery`, because
/// those verbs do not produce flat target-keyed plans.
///
/// **Recovery policy**: The three Recovery verbs intentionally bypass this
/// matrix for the reasons documented in `src/lifecycle/CLAUDE.md` §"Recovery
/// verb families". They use their own status predicates (or none, in the
/// case of `resurrect_for_iteration`).
#[must_use]
pub(crate) fn allowed_from_for_plan(
    target: TaskStatus,
    source: TransitionSource,
) -> &'static [TaskStatus] {
    use TaskStatus::*;
    use TransitionSource::*;

    match (source, target) {
        // ReconcilePrd: baseline + done→irrelevant + todo→done + todo→irrelevant.
        (ReconcilePrd, Todo) => &[Blocked, Skipped],
        (ReconcilePrd, InProgress) => &[Todo],
        (ReconcilePrd, Done) => &[Todo, InProgress],
        (ReconcilePrd, Blocked) => &[InProgress],
        (ReconcilePrd, Skipped) => &[InProgress],
        (ReconcilePrd, Irrelevant) => &[Todo, InProgress, Done],

        // DoctorRepair: baseline + in_progress→todo + todo→done.
        (DoctorRepair, Todo) => &[InProgress, Blocked, Skipped],
        (DoctorRepair, InProgress) => &[Todo],
        (DoctorRepair, Done) => &[Todo, InProgress],
        (DoctorRepair, Blocked) => &[InProgress],
        (DoctorRepair, Skipped) => &[InProgress],
        (DoctorRepair, Irrelevant) => &[InProgress],

        // DecayReset NARROWS the baseline — only Blocked/Skipped → Todo are
        // legitimate entry points. Every other `(target, DecayReset)` is empty.
        (DecayReset, Todo) => &[Blocked, Skipped],
        (DecayReset, _) => &[],

        // Operator / LoopStatusTag / Recovery aren't plan-driven verbs; no
        // entries by design.
        _ => &[],
    }
}
