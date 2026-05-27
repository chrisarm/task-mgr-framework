//! Pre-spawn per-task recovery resolution (CONTRACT-001 scaffold; contract
//! expanded by TEST-INIT-002, body filled by FEAT-002).
//!
//! Folds the pre-dispatch per-task recovery reactions that must run at the top
//! of every iteration, BEFORE the spawn, into a single [`TaskExecutionPlan`]:
//!
//! 1. `recovery::check_override_invalidation` — the operator escape valve
//!    (clears stale auto-recovery overrides when `tasks.model` was edited
//!    out-of-band). Load-bearing order: it MUST run FIRST so the subsequent
//!    crash-escalation + effort/runner reads see cleared channels.
//! 2. `recovery::check_crash_escalation` — the escalated model when the
//!    previous iteration on this task crashed (→ [`TaskExecutionPlan::model`]).
//! 3. The prior-overflow effort override carried on
//!    `ctx.effort_overrides[task_id]` (→ [`TaskExecutionPlan::effort`]) — this
//!    is the audit-#6-effort channel the plan must surface, NOT drop.
//! 4. `engine::resolve_effective_runner` over the post-escalation effective
//!    model (→ [`TaskExecutionPlan::runner`]).
//!
//! Both execution paths route through this single coordinator: the sequential
//! path folds its 1 call; the wave path folds one call per slot. Identical
//! `(ctx, task, resolved_model, conn)` inputs MUST produce an identical
//! `TaskExecutionPlan` regardless of path shape — the parity contract pinned by
//! `tests/reaction_parity.rs`.
//!
//! Wired into both paths by FEAT-002/FEAT-008 (sequential: `iteration.rs`
//! ~L367/L419; wave: per-slot, folded in `wave_scheduler.rs` ~L983).

use rusqlite::Connection;

use crate::loop_engine::engine::IterationContext;
use crate::loop_engine::runner::RunnerKind;

/// The resolved per-task execution decision produced by
/// [`resolve_task_execution`]. The single home for the three pre-spawn
/// recovery channels both paths must agree on.
///
/// Equality is structural so the parity tests can assert that the sequential
/// (1 call) and wave (per-slot) shapes compute the SAME plan for identical
/// inputs.
#[derive(Debug, PartialEq, Eq)]
pub struct TaskExecutionPlan {
    /// Crash-escalated model for this iteration, or `None` when no escalation
    /// applies (the caller keeps its already-resolved model). Sourced from
    /// `recovery::check_crash_escalation`.
    pub model: Option<String>,
    /// Prior-overflow effort override in effect for this task, or `None`.
    /// Sourced from `ctx.effort_overrides[task_id]` AFTER override
    /// invalidation has had a chance to clear it. A plan that drops this
    /// channel is a parity regression (the audit-#6-effort bug).
    pub effort: Option<&'static str>,
    /// Dispatch target after runner-override resolution over the
    /// post-escalation effective model. Sourced from
    /// `engine::resolve_effective_runner`.
    pub runner: RunnerKind,
}

/// Inputs to [`resolve_task_execution`]. Destructured exhaustively (no `..`) by
/// the FEAT-002 body so a new field forces a deliberate update here and in
/// every caller — the compile-time parity lock.
///
/// `crashed_last_iteration` is intentionally NOT a separate field: the
/// canonical per-task crash flag lives on `ctx.crashed_last_iteration`, so the
/// coordinator reads it from `ctx` to keep a single source of truth.
pub struct ResolveTaskExecutionParams<'a> {
    pub ctx: &'a mut IterationContext,
    pub conn: &'a Connection,
    pub task_id: &'a str,
    /// The model resolved for this task BEFORE crash escalation (the baseline
    /// passed to `check_crash_escalation` and used as the runner-resolution
    /// fallback when no escalation applies).
    pub resolved_model: Option<&'a str>,
}

/// Pre-spawn recovery coordinator: invalidate stale overrides, then fold crash
/// escalation, the effort override, and runner resolution into one
/// [`TaskExecutionPlan`].
///
/// **Scaffold under TEST-INIT-002** — body implemented by FEAT-002, which
/// removes the `#[ignore]` attributes on the behavioral cases in
/// `tests/reaction_parity.rs`. The exhaustive destructure of
/// [`ResolveTaskExecutionParams`] (no `..`) is part of the FEAT-002 body.
#[allow(dead_code)] // wired into both paths by FEAT-002/FEAT-008
pub fn resolve_task_execution(params: ResolveTaskExecutionParams<'_>) -> TaskExecutionPlan {
    let _ = params;
    unimplemented!(
        "FEAT-002: destructure ResolveTaskExecutionParams exhaustively; call \
         check_override_invalidation(ctx, conn, task_id) FIRST; then build the \
         plan from check_crash_escalation(&ctx.crashed_last_iteration, task_id, \
         resolved_model) (model), ctx.effort_overrides.get(task_id).copied() \
         (effort), and resolve_effective_runner over the post-escalation \
         effective model (runner)"
    )
}
