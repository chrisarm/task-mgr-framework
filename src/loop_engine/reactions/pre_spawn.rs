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

use std::collections::HashMap;

use rusqlite::Connection;

use crate::loop_engine::engine::{IterationContext, resolve_effective_runner};
use crate::loop_engine::model;
use crate::loop_engine::recovery::normalize_baseline;
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
/// The four steps run in a load-bearing order:
/// 1. [`invalidate_stale_overrides`] — operator escape valve, FIRST so the
///    subsequent effort/runner reads see cleared channels (an out-of-band
///    `tasks.model` edit must not let a stale override resurface this
///    iteration).
/// 2. [`crash_escalated_model`] — `Some(model)` when the previous iteration on
///    this task crashed, else `None` (the caller keeps its resolved baseline).
/// 3. `ctx.effort_overrides[task_id]` — read AFTER invalidation so a cleared
///    override is gone.
/// 4. [`resolve_effective_runner`] over the post-escalation effective model
///    (the crash-escalated model, or the resolved baseline when no escalation).
///
/// Both execution paths route through this single coordinator (the sequential
/// path folds its 1 call; the wave path folds one call per slot), so identical
/// `(ctx, task, resolved_model, conn)` inputs MUST produce an identical plan.
///
/// Model-string layers that are NOT account-global recovery channels —
/// `ctx.model_overrides` (prior-overflow model) and `apply_review_model_override`
/// (review-class routing) — stay at the call sites: they rewrite the `--model`
/// string and, for review routing, re-resolve the runner, AFTER this plan is
/// produced. `model_overrides` is always paired with a `runner_overrides`
/// entry, so [`TaskExecutionPlan::runner`] already reflects it.
pub fn resolve_task_execution(params: ResolveTaskExecutionParams<'_>) -> TaskExecutionPlan {
    let ResolveTaskExecutionParams {
        ctx,
        conn,
        task_id,
        resolved_model,
    } = params;

    // 1. Operator escape valve FIRST — clears stale auto-recovery channels when
    //    an operator edited `tasks.model` out-of-band.
    invalidate_stale_overrides(ctx, conn, task_id);

    // 2. Crash escalation (None when the last iteration on this task did not
    //    crash — the caller keeps its already-resolved model).
    let model = crash_escalated_model(&ctx.crashed_last_iteration, task_id, resolved_model);

    // 3. Prior-overflow effort override, read AFTER invalidation.
    let effort = ctx.effort_overrides.get(task_id).copied();

    // 4. Runner over the post-escalation effective model.
    let effective_model = model.as_deref().or(resolved_model);
    let runner = resolve_effective_runner(ctx, task_id, effective_model);

    TaskExecutionPlan {
        model,
        effort,
        runner,
    }
}

/// Crash-recovery model escalation (relocated from `recovery::check_crash_escalation`
/// — the home now carries a `#[deprecated]` shim that delegates here).
///
/// Returns `Some(escalated_model)` when the previous iteration on
/// `current_task_id` crashed (`crashed_last_iteration[current_task_id] == true`).
/// Returns `None` when the task is absent from the map or its last outcome was
/// not a crash. A `None`/empty/whitespace `resolved_model` is treated as the
/// sonnet baseline and escalates to opus. Escalation is independent of
/// `CrashTracker` backoff.
pub(crate) fn crash_escalated_model(
    crashed_last_iteration: &HashMap<String, bool>,
    current_task_id: &str,
    resolved_model: Option<&str>,
) -> Option<String> {
    if !crashed_last_iteration
        .get(current_task_id)
        .copied()
        .unwrap_or(false)
    {
        return None;
    }
    match normalize_baseline(resolved_model) {
        None => Some(model::OPUS_MODEL.to_string()),
        Some(m) => model::escalate_model(Some(m)),
    }
}

/// Operator escape valve (relocated from `recovery::check_override_invalidation`
/// — the home now carries a `#[deprecated]` shim that delegates here).
///
/// Detects an out-of-band `tasks.model` edit and clears all six per-task
/// auto-recovery channels for that task. Short-circuits when `task_id` has no
/// `overflow_original_task_model` snapshot (the dominant case — most tasks never
/// trigger the overflow ladder — pays no DB round-trip). DB read errors are
/// logged and treated as no-op so a transient failure never blocks the
/// iteration. A single stderr line announces the clear.
pub(crate) fn invalidate_stale_overrides(
    ctx: &mut IterationContext,
    conn: &Connection,
    task_id: &str,
) {
    if !ctx.overflow_original_task_model.contains_key(task_id) {
        return;
    }

    let current_model: Option<String> = match conn.query_row(
        "SELECT model FROM tasks WHERE id = ?1",
        rusqlite::params![task_id],
        |row| row.get(0),
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Warning: invalidate_stale_overrides({task_id}): DB read failed: {e}");
            return;
        }
    };

    let snapshotted = ctx.overflow_original_task_model.get(task_id);
    if snapshotted.map(Option::as_deref) == Some(current_model.as_deref()) {
        return;
    }

    ctx.runner_overrides.remove(task_id);
    ctx.model_overrides.remove(task_id);
    ctx.effort_overrides.remove(task_id);
    ctx.overflow_recovered.remove(task_id);
    ctx.overflow_original_model.remove(task_id);
    ctx.overflow_original_task_model.remove(task_id);

    eprintln!(
        "Operator changed task model for {task_id} — clearing auto-recovery overrides; resolving fresh."
    );
}
