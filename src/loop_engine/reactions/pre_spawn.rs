//! Pre-spawn per-task recovery resolution (converged by FEAT-002; contract
//! expanded by TEST-INIT-002).
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

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::loop_engine::engine::{
    EffectiveRunnerInput, IterationContext, resolve_effective_runner,
};
use crate::loop_engine::model::{self, Provider, ResolvedModelsConfig};
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
/// `ctx.model_overrides` (prior-overflow model) — stays at the call sites: it
/// rewrites the `--model` string AFTER this plan is
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
    //    crash — the caller keeps its already-resolved model). Resolves the
    //    Claude ladder from the OPERATOR's config carried on the context, so a
    //    remapped Claude ladder escalates onto models that config defines, not
    //    the builtin defaults (REFACTOR-007).
    let model = crash_escalated_model_with_config(
        &ctx.crashed_last_iteration,
        task_id,
        resolved_model,
        &ctx.resolved_models,
    );

    // 3. Prior-overflow effort override, read AFTER invalidation.
    let effort = ctx.effort_overrides.get(task_id).copied();

    // 4. Runner over the post-escalation effective model. `provider_hint`
    //    is intentionally `None` here — primaryRunner provider intent is
    //    threaded through to the dispatcher's re-resolution at the final
    //    spawn site (iteration.rs / wave_scheduler.rs), and this pre-spawn
    //    plan only reflects the pre-rewrite baseline runner. Construct the
    //    input explicitly so a missed-thread bug is a compile error rather
    //    than a silent Codex→Claude misroute (the `From<Option<&str>>` impl
    //    is `#[cfg(test)]`-gated).
    let effective_model = model.as_deref().or(resolved_model);
    let runner = resolve_effective_runner(
        ctx,
        task_id,
        EffectiveRunnerInput {
            model: effective_model,
            provider_hint: None,
        },
    );

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
///
/// This is the **builtin-ladder convenience variant** — it resolves the Claude
/// tier ladder from [`model::builtin_resolved_models`]. The production
/// coordinator [`resolve_task_execution`] calls
/// [`crash_escalated_model_with_config`] with the operator-resolved config from
/// the context instead (REFACTOR-007); this 3-arg form is retained for the
/// equivalence tests (and the `engine::check_crash_escalation` re-export) that
/// exercise the default ladder.
pub fn crash_escalated_model(
    crashed_last_iteration: &HashMap<String, bool>,
    current_task_id: &str,
    resolved_model: Option<&str>,
) -> Option<String> {
    crash_escalated_model_with_config(
        crashed_last_iteration,
        current_task_id,
        resolved_model,
        model::builtin_resolved_models(),
    )
}

/// Operator-config-aware crash-recovery model escalation: identical to
/// [`crash_escalated_model`] but resolves the Claude tier ladder from the
/// supplied `models` config rather than the builtin defaults.
///
/// This is the production path (REFACTOR-007): [`resolve_task_execution`] passes
/// `&ctx.resolved_models` so an operator who remapped Claude tiers (custom
/// ladder, null rungs) gets a crash escalation onto a model THEIR config defines
/// — closing the FIX-001 config-input divergence where the recovery paths walked
/// the builtin ladder regardless of operator config.
pub fn crash_escalated_model_with_config(
    crashed_last_iteration: &HashMap<String, bool>,
    current_task_id: &str,
    resolved_model: Option<&str>,
    models: &ResolvedModelsConfig,
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
        Some(m) => model::escalate_tier(models, model::Provider::Claude, Some(m)),
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
pub fn invalidate_stale_overrides(ctx: &mut IterationContext, conn: &Connection, task_id: &str) {
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

    // The snapshot's inner value: `None` = anchor-resolved (NULL `tasks.model`
    // at first overflow); `Some(m)` = the task carried an explicit model.
    let snapshot_inner: Option<String> = match ctx.overflow_original_task_model.get(task_id) {
        Some(inner) => inner.clone(),
        None => return,
    };

    // No divergence from the snapshot → no-op (the dominant steady state).
    if snapshot_inner.as_deref() == current_model.as_deref() {
        return;
    }

    // NULL-original semantics (FEAT-004): an anchor-resolved task snapshotted a
    // NULL `tasks.model`, then the escalation ladder wrote `tasks.model` itself
    // (to the auto-recovery model it also recorded in `model_overrides`). That
    // write is the LADDER's, NOT an operator edit — absorb it into the snapshot
    // so the next pass compares against the escalated model. Only a SUBSEQUENT
    // edit to a DIFFERENT model fires the six-channel clear. Without this, the
    // ladder's own first write (`Some(None) != Some(Some(opus))`) would
    // self-trip the escape valve and wipe the recovery it just set up.
    if snapshot_inner.is_none() {
        let ladder_model = ctx.model_overrides.get(task_id).map(String::as_str);
        if current_model.is_some() && current_model.as_deref() == ladder_model {
            ctx.overflow_original_task_model
                .insert(task_id.to_string(), current_model);
            return;
        }
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

/// FEAT-008: the set of todo task ids that are QUOTA-DEFERRED under the active
/// provider blackouts — their effective provider is still blacked out and they
/// cannot reroute off it. This is the `excluded_ids` set passed to
/// `select_next_task_excluding` / `select_parallel_group_excluding`; its
/// complement (todo tasks NOT returned) are the spillover-eligible tasks
/// selection may run on an alternate provider.
///
/// Per-task effective provider — the spillover-eligibility SSoT:
/// - A task carrying a `ctx.runner_overrides` entry is PINNED to that runner
///   (the permanent cross-provider promotion owned by `promote_once`). It is
///   never spillover-eligible: if its pinned provider is blacked out it defers,
///   otherwise it runs. This is the "no runner override" half of the FR-008
///   eligibility rule — a pinned task is read here but `runner_overrides` is
///   NEVER written.
/// - Otherwise the spawn-side resolver `model::resolve_execution_plan` (WITH the
///   active blackouts) decides: a spillover-eligible implementation task at or
///   below `spillover.maxDifficulty` reroutes to a non-blacked provider (→ NOT
///   deferred); a frontier/review task, an explicit-model task, or one with no
///   enabled alternative stays on the blacked-out provider (→ deferred).
///
/// Returns an empty set when no provider is blacked out (the dominant case), so
/// the DB scan only runs while a blackout is live. Read-only — DB errors are
/// logged and treated as "nothing excluded" so a transient failure degrades to
/// the pre-FEAT-008 selection rather than stranding the wave.
pub fn compute_quota_excluded_ids(
    ctx: &IterationContext,
    conn: &Connection,
    task_prefix: Option<&str>,
    models: &ResolvedModelsConfig,
    active_blackouts: &HashSet<Provider>,
) -> HashSet<String> {
    if active_blackouts.is_empty() {
        return HashSet::new();
    }

    // `id LIKE '' || '%'` collapses to `id LIKE '%'` (every non-null id) when no
    // prefix is given, so one parameterized query covers both cases.
    let like_prefix = task_prefix.unwrap_or("");
    let mut stmt = match conn.prepare(
        "SELECT id, model, difficulty FROM tasks \
         WHERE status = 'todo' AND id LIKE ?1 || '%' AND archived_at IS NULL",
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: compute_quota_excluded_ids: prepare failed: {e}");
            return HashSet::new();
        }
    };
    let rows = stmt.query_map(rusqlite::params![like_prefix], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: compute_quota_excluded_ids: query failed: {e}");
            return HashSet::new();
        }
    };

    let mut excluded = HashSet::new();
    for (id, model_col, difficulty) in rows.flatten() {
        let effective_provider = match ctx.runner_overrides.get(&id) {
            Some(kind) => provider_of_runner(*kind),
            None => {
                model::resolve_execution_plan(&model::PlanContext {
                    task_id: &id,
                    task_model: model_col.as_deref(),
                    difficulty: difficulty.as_deref(),
                    models,
                    provider_blackouts: active_blackouts,
                })
                .provider
            }
        };
        if active_blackouts.contains(&effective_provider) {
            excluded.insert(id);
        }
    }
    excluded
}

/// `RunnerKind → Provider` identity translation (the inverse of the match in
/// `resolve_effective_runner`). Local to the blackout-exclusion computation so a
/// pinned runner override can be compared against the active blackout set.
fn provider_of_runner(kind: RunnerKind) -> Provider {
    match kind {
        RunnerKind::Claude => Provider::Claude,
        RunnerKind::Grok => Provider::Grok,
        RunnerKind::Codex => Provider::Codex,
    }
}
