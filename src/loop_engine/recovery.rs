//! Per-task recovery cluster: crash escalation, operator override invalidation,
//! consecutive-failure tracking, model escalation / Grok promotion, auto-block,
//! and the per-iteration crash/stale tracker update.
//!
//! Carved out of `engine.rs` (PRD 02, FEAT-002). These are the leaf primitives
//! the sequential (`run_iteration`) and wave (`run_wave_iteration`) orchestrators
//! call after an iteration resolves. The orchestration types they operate on
//! (`IterationContext`, `IterationResult`) and the spawn-discriminant resolver
//! (`resolve_effective_runner`) remain in `engine.rs` and are imported here;
//! `engine.rs` re-exports the public functions so external import paths
//! (`task_mgr::loop_engine::engine::handle_task_failure`, …) stay valid (FR-008).
//!
//! The transactional-promotion contract is load-bearing: `handle_task_failure`
//! performs its DB writes inside a transaction and applies the in-memory ctx
//! mutations (`apply_pending_promotion`) ONLY after `tx.commit()` succeeds, via
//! the inner/apply split (`escalate_task_model_if_needed_inner` returning a
//! `PendingPromotion`). See `src/loop_engine/CLAUDE.md` →
//! "Transactional promotion ctx writes are deferred".

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::config::{IterationOutcome, PermissionMode};
use crate::loop_engine::detection;
use crate::loop_engine::engine::{IterationContext, IterationResult, resolve_effective_runner};
use crate::loop_engine::model;
use crate::loop_engine::runner::RunnerKind;
use crate::output::ui;

/// Treat `Some("")` and `Some("   ")` as "no model known" so both escalation
/// paths share the same baseline-fallback semantics:
/// `reactions::pre_spawn::crash_escalated_model` (crash recovery) and
/// `escalate_task_model_if_needed` (consecutive-failure recovery). `pub(crate)`
/// so the relocated pre-spawn coordinator can reuse it as the single
/// normalize-then-escalate primitive.
pub(crate) fn normalize_baseline(model: Option<&str>) -> Option<&str> {
    model.filter(|s| !s.trim().is_empty())
}

/// Returns true if a task should be auto-blocked due to consecutive failures.
///
/// Auto-block fires when `consecutive_failures >= max_retries` AND `max_retries > 0`.
/// `max_retries=0` disables auto-blocking entirely (task retries indefinitely).
pub fn should_auto_block(consecutive_failures: i32, max_retries: i32) -> bool {
    max_retries > 0 && consecutive_failures >= max_retries
}

/// Returns true if the model should be escalated due to consecutive failures.
///
/// Fires at `consecutive_failures >= 2`, before the auto-block threshold.
/// Gives the task one more attempt at a higher-tier model before blocking.
pub fn should_escalate_for_consecutive_failures(consecutive_failures: i32) -> bool {
    consecutive_failures >= 2
}

/// W5: deferred promotion bundle. Carries everything needed to mutate
/// `IterationContext` after a DB write commits. Used by
/// `escalate_task_model_if_needed_inner` to decouple the DB step from the
/// ctx step so transactional callers (`handle_task_failure`) can hold the
/// ctx mutations until `tx.commit()` returns Ok — preventing a one-iteration
/// dirty-ctx-vs-rolled-back-DB window when commit fails.
pub(crate) struct PendingPromotion {
    task_id: String,
    pre_promotion_model: Option<String>,
    /// Runner the task is leaving — drives the banner's "from <X>" label so
    /// Codex→Claude (FEAT-005) and Grok→Claude (FEAT-PRIMARY-003) are
    /// distinguishable even though both target `RunnerKind::Claude`.
    source_runner: RunnerKind,
    /// Runner the task is being promoted TO: `Grok` for the FEAT-007
    /// Claude→Grok hook, `Claude` for the FEAT-PRIMARY-003 inverse Grok→Claude
    /// hook AND the FEAT-005 Codex→Claude hook. Written verbatim into
    /// `runner_overrides`.
    target_runner: RunnerKind,
    /// Model id written to BOTH `tasks.model` (in the inner DB step) and
    /// `ctx.model_overrides` (here). For Claude→Grok this is the fallback
    /// runner's Grok model; for Grok→Claude it is `claude_fallback_model`;
    /// for Codex→Claude it is the resolved Claude target (high difficulty →
    /// OPUS_MODEL, else project default or OPUS_MODEL baseline).
    target_model: String,
    new_count: i32,
}

/// Apply a deferred promotion to the `IterationContext`. Idempotent w.r.t.
/// `overflow_original_task_model` (`or_insert_with` preserves the first
/// snapshot). Emits the one-line stderr banner exactly once per promotion
/// (gated on whether `runner_overrides` already held an entry — see M2 in
/// the FEAT-007 commit). Direction-neutral: the banner text adapts to
/// `target_runner` so both the Claude→Grok and Grok→Claude hooks share this
/// apply step.
pub(crate) fn apply_pending_promotion(ctx: &mut IterationContext, p: &PendingPromotion) {
    ctx.overflow_original_task_model
        .entry(p.task_id.clone())
        .or_insert_with(|| p.pre_promotion_model.clone());
    let already_promoted = ctx.runner_overrides.contains_key(&p.task_id);
    // kind-correct: writes the promoted provider identity into the override map — the VALUE is the provider, not a capability flag
    ctx.runner_overrides
        .insert(p.task_id.clone(), p.target_runner);
    ctx.model_overrides
        .insert(p.task_id.clone(), p.target_model.clone());
    if !already_promoted {
        // The "from" tier names the runner the task is leaving, so the banner
        // reads naturally in all directions. Grok→Claude and Codex→Claude
        // both target Claude, so we disambiguate on `source_runner`.
        let runner_label = match p.target_runner {
            RunnerKind::Grok => "Grok",
            RunnerKind::Claude => "Claude",
            RunnerKind::Codex => "Codex",
        };
        let from_label = match p.source_runner {
            RunnerKind::Claude => "Opus",
            RunnerKind::Grok => "Grok",
            RunnerKind::Codex => "Codex",
        };
        ui::emit(&format!(
            "Promoted task {} to {} runner (model={}) after {} consecutive failures at {}",
            p.task_id, runner_label, p.target_model, p.new_count, from_label
        ));
    }
}

/// CONTRACT-PROMO-001: the cross-provider promotion idempotency primitive.
///
/// Owns the single `ctx.runner_overrides.contains_key(task_id)` snapshot that
/// bounds every cross-provider promotion to ONCE per loop run, and constructs
/// the [`PendingPromotion`] the caller applies post-commit via
/// [`apply_pending_promotion`]. Returns `None` when the task already carries a
/// promotion override (in EITHER direction) so the caller falls through to
/// normal failure accounting (→ `auto_block_task`) instead of pivoting a
/// second time; otherwise `Some(PendingPromotion)` built from the args
/// verbatim.
///
/// Historically this was the shared idempotency guard for four cross-provider
/// promotion sites (Claude→Grok, Grok→Claude, Codex→Claude RuntimeError
/// escalation, and the overflow rung-4 pivot). REFACTOR-006 deleted the three
/// RuntimeError promotion arms — unreachable once preflight hard-rejected
/// `primaryRunner` / `fallbackRunner` — so the **overflow rung-4 pivot**
/// (`reactions::post_output`) is now the sole caller. The primitive stays
/// direction-generic (the CONTRACT-PROMO-001 tests still exercise the
/// Claude→Grok / Grok→Claude / Codex→Claude shapes), so a future cross-provider
/// escape valve can reuse it — and MUST route its `contains_key` guard through
/// here rather than inlining its own snapshot.
///
/// Contract (compiler- and test-enforced):
/// - Reads `ctx` IMMUTABLY (`&IterationContext`) → performs NO ctx mutation.
///   The `runner_overrides` / `model_overrides` inserts stay in
///   `apply_pending_promotion`, preserving the deferred-apply split that keeps
///   ctx consistent with a rolled-back DB on commit failure.
/// - Takes no `Connection` → performs NO DB write. The caller's inner helper
///   still owns the `UPDATE tasks SET model` step and the apply *timing*
///   (deferred post-commit vs. immediate); this primitive does NOT collapse
///   that split.
/// - `source` / `target` are written verbatim into the `PendingPromotion`.
///   For the Codex→Claude path the caller MUST pass `target =
///   RunnerKind::Claude` (never Codex) so the eventual `runner_overrides`
///   insert is insert-safe (learning [4553]); `source` disambiguates
///   Grok→Claude vs. Codex→Claude for the direction-neutral banner
///   (learning [4532]).
pub(crate) fn promote_once(
    ctx: &IterationContext,
    task_id: &str,
    source: RunnerKind,
    target: RunnerKind,
    target_model: String,
    pre_promotion_model: Option<String>,
    new_count: i32,
) -> Option<PendingPromotion> {
    if ctx.runner_overrides.contains_key(task_id) {
        return None;
    }
    Some(PendingPromotion {
        task_id: task_id.to_string(),
        pre_promotion_model,
        source_runner: source,
        target_runner: target,
        target_model,
        new_count,
    })
}

/// Inner helper: performs the DB writes for same-provider Claude tier
/// escalation but does **not** mutate `ctx`. Returns the escalated model AND an
/// `Option<PendingPromotion>` the transactional caller (`handle_task_failure`)
/// applies via `apply_pending_promotion` after `tx.commit()` succeeds.
///
/// Post-hard-break this path performs NO cross-provider promotion. The legacy
/// `primaryRunner` / `fallbackRunner` RuntimeError-fallback surfaces are
/// rejected at preflight (`LEGACY_MODEL_KEYS`), so the Claude→Grok /
/// Grok→Claude / Codex→Claude promotion arms were unreachable and have been
/// deleted (REFACTOR-006). The `Option<PendingPromotion>` slot is therefore
/// always `None` here; it is retained because the deferred-apply skeleton
/// (`apply_pending_promotion`) is the sole non-test reader of
/// [`PendingPromotion`]'s fields, and [`PendingPromotion`] is still constructed
/// by [`promote_once`] for the LIVE overflow rung-4 pivot
/// (`reactions::post_output`). Only the same-provider Claude tier escalation
/// remains live here.
///
/// `executed_runner` gates the Codex short-circuit: a Codex task is off the
/// Claude tier ladder and (with the legacy fallback removed) has no
/// cross-provider escape, so it must NOT climb the ladder — escalating a
/// NULL-model Codex task would normalize it to the Sonnet baseline and write
/// Opus into `tasks.model`, silently pivoting the task to Claude next iteration.
pub(crate) fn escalate_task_model_if_needed_inner(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    executed_runner: RunnerKind,
    models: &model::ResolvedModelsConfig,
) -> TaskMgrResult<(Option<String>, Option<PendingPromotion>)> {
    if !should_escalate_for_consecutive_failures(new_count) {
        return Ok((None, None));
    }
    // Codex tasks never climb the Claude ladder (see fn rustdoc).
    if executed_runner == RunnerKind::Codex {
        return Ok((None, None));
    }
    let current_model: Option<String> =
        conn.query_row("SELECT model FROM tasks WHERE id = ?", [task_id], |r| {
            r.get::<_, Option<String>>(0)
        })?;
    // None / empty / whitespace model: assume sonnet baseline → escalate to opus.
    // Otherwise step up one DEFINED tier on the Claude ladder (config exact-match
    // via `escalate_tier`, no substring matching). The Claude ladder is resolved
    // from the OPERATOR's config (`models`, threaded down from the orchestrator's
    // `IterationContext::resolved_models`) so an operator who remapped Claude
    // tiers (custom ladder, null rungs) gets escalations onto models THEIR config
    // defines — not the builtin defaults (REFACTOR-007, the FIX-001 divergence).
    // A non-Claude (Grok) `current_model` is off the Claude ladder, so
    // `escalate_tier` returns `None` and no escalation fires; the task then
    // proceeds to normal failure accounting (→ `auto_block_task`).
    let escalated = match normalize_baseline(current_model.as_deref()) {
        None => Some(model::OPUS_MODEL.to_string()),
        Some(m) => model::escalate_tier(models, model::Provider::Claude, Some(m)),
    };
    if let Some(ref new_model) = escalated {
        conn.execute(
            "UPDATE tasks SET model = ? WHERE id = ?",
            rusqlite::params![new_model, task_id],
        )?;
        ui::emit(&format!(
            "Escalated task {} to model {} after {} consecutive failures",
            task_id, new_model, new_count
        ));
    }

    Ok((escalated, None))
}

/// Absorb a consecutive-failure escalation's `tasks.model` write into the
/// overflow snapshot so the escape valve (`pre_spawn::invalidate_stale_overrides`)
/// does not mistake the ladder's own write for an out-of-band operator edit.
///
/// The escape valve fires its six-channel clear when `tasks.model` diverges from
/// the snapshot in `ctx.overflow_original_task_model`. Crash escalation records
/// its new model in `ctx.model_overrides`, so the valve's NULL-original absorb
/// branch recognizes it; consecutive-failure escalation, however, writes
/// `tasks.model` WITHOUT touching `model_overrides`, so without this refresh the
/// valve would treat the ladder's own write as an operator edit and wipe the
/// recovery (including the effort downgrade) it just set up.
///
/// `and_modify` (never `insert`/`or_insert_with`): only an EXISTING snapshot is
/// refreshed. A task that never overflowed has no snapshot and must NOT gain one
/// here — otherwise the valve would start tracking a task it should ignore.
/// No-op when `escalated` is `None` (escalation did not fire).
fn absorb_escalation_into_overflow_snapshot(
    ctx: &mut IterationContext,
    task_id: &str,
    escalated: Option<&str>,
) {
    if let Some(model) = escalated {
        ctx.overflow_original_task_model
            .entry(task_id.to_string())
            .and_modify(|snapshot| *snapshot = Some(model.to_string()));
    }
}

/// Escalate the model for a task in the DB when consecutive failures reach the threshold.
///
/// Follows the same sonnet-baseline pattern as `check_crash_escalation`:
/// - `None` or empty model assumes sonnet baseline → escalates to opus.
/// - Sonnet → opus, Opus → fable, fable → fable (no-op at ceiling).
///
/// Same-provider Claude tier escalation only. Post-hard-break there is no
/// cross-provider promotion here (REFACTOR-006). A Grok task's model is off the
/// Claude ladder, so it never escalates; a Codex task short-circuits in the
/// inner helper.
///
/// Returns `Some(new_model)` if escalation fired, `None` if below threshold or
/// the model tier is unknown (e.g. already at Grok) or the fable self-loop
/// produced no change. The DB is updated in-place when `Some` is returned.
///
/// This is the convenience variant — DB and (deferred) ctx writes happen
/// back-to-back. Transactional callers should prefer
/// `escalate_task_model_if_needed_inner` + `apply_pending_promotion` (see W5).
pub fn escalate_task_model_if_needed(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    ctx: &mut IterationContext,
) -> TaskMgrResult<Option<String>> {
    // Derive the runner from the DB model before entering the inner helper.
    // The inner function requires an explicit runner; callers that DO know the
    // executed runner (production paths) should use
    // `escalate_task_model_if_needed_for_runner` to avoid this pre-read.
    // For Codex tasks this derivation produces Claude (gpt-* has no provider
    // hint here); Codex callers must use the explicit-runner variant so the
    // inner helper's Codex short-circuit fires.
    let current_model: Option<String> = conn
        .query_row("SELECT model FROM tasks WHERE id = ?", [task_id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten();
    let runner = resolve_effective_runner(
        ctx,
        task_id,
        crate::loop_engine::engine::EffectiveRunnerInput {
            model: current_model.as_deref(),
            provider_hint: None,
        },
    );
    let (model, pending) = escalate_task_model_if_needed_inner(
        conn,
        task_id,
        new_count,
        runner,
        &ctx.resolved_models,
    )?;
    if let Some(p) = pending {
        apply_pending_promotion(ctx, &p);
    }
    // Refresh any existing overflow snapshot so the escape valve does not
    // misread this consecutive-failure escalation as an operator edit. Keyed on
    // the returned model (post-REFACTOR-006 `pending` is always `None`, so this
    // must sit OUTSIDE the promotion branch).
    absorb_escalation_into_overflow_snapshot(ctx, task_id, model.as_deref());
    Ok(model)
}

/// Variant of [`escalate_task_model_if_needed`] for callers that know the
/// executed runner at the call site.
///
/// The explicit `executed_runner` bypasses the model-string re-derivation
/// that the plain `escalate_task_model_if_needed` uses when the runner is not
/// known. This is critical for Codex tasks: their `gpt-*` model would
/// otherwise re-classify as Claude and miss the Codex short-circuit in the
/// inner helper. Used in integration tests and by `handle_task_failure_with_runner`.
pub fn escalate_task_model_if_needed_for_runner(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    executed_runner: RunnerKind,
    ctx: &mut IterationContext,
) -> TaskMgrResult<Option<String>> {
    let (model, pending) = escalate_task_model_if_needed_inner(
        conn,
        task_id,
        new_count,
        executed_runner,
        &ctx.resolved_models,
    )?;
    if let Some(p) = pending {
        apply_pending_promotion(ctx, &p);
    }
    // Refresh any existing overflow snapshot (see the convenience variant above
    // and `absorb_escalation_into_overflow_snapshot`'s rustdoc for why).
    absorb_escalation_into_overflow_snapshot(ctx, task_id, model.as_deref());
    Ok(model)
}

/// Increment `consecutive_failures` for a task in the DB.
///
/// Returns the new `consecutive_failures` count after incrementing.
pub fn increment_consecutive_failures(conn: &Connection, task_id: &str) -> TaskMgrResult<i32> {
    conn.execute(
        "UPDATE tasks SET consecutive_failures = consecutive_failures + 1 WHERE id = ?",
        [task_id],
    )?;
    let count: i32 = conn.query_row(
        "SELECT consecutive_failures FROM tasks WHERE id = ?",
        [task_id],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// Reset `consecutive_failures` for a task in the DB to 0.
///
/// Called after a Completed outcome to clear the failure streak.
pub fn reset_consecutive_failures(conn: &Connection, task_id: &str) -> TaskMgrResult<()> {
    conn.execute(
        "UPDATE tasks SET consecutive_failures = 0 WHERE id = ?",
        [task_id],
    )?;
    Ok(())
}

/// Auto-block a task by setting status to 'blocked' and recording a descriptive last_error.
///
/// Called when `should_auto_block()` returns true after an iteration.
/// Sets `blocked_at_iteration` for decay tracking (consistent with `fail/transition.rs`).
///
/// Now a thin shim over [`TaskLifecycle::auto_block_after_failures`]. The
/// lifecycle verb gates on `status = 'in_progress'` (conditional WHERE);
/// terminal rows are a clean `Ok(_)` no-op which tightens the legacy behavior
/// without losing observability (callers ignore the row-count and rely on the
/// stderr emission elsewhere).
pub fn auto_block_task(
    conn: &mut Connection,
    task_id: &str,
    consecutive_failures: i32,
    current_iteration: i64,
) -> TaskMgrResult<()> {
    let msg = format!(
        "Auto-blocked after {} consecutive failures (task: {})",
        consecutive_failures, task_id
    );
    TaskLifecycle::new(conn).auto_block_after_failures(task_id, &msg, current_iteration)?;
    Ok(())
}

/// Increment consecutive failure count, escalate model tier if needed, and auto-block if the
/// task has exhausted its retry budget. All DB writes are wrapped in a single transaction.
///
/// `current_iteration` is used to set `blocked_at_iteration` on auto-blocked tasks for
/// decay tracking. Escalation is skipped when auto-block fires on the same iteration
/// (the escalated model would never be used).
///
/// `ctx` threads `IterationContext` through so the embedded escalation can apply
/// any deferred `PendingPromotion` after `tx.commit()`. Post-hard-break the
/// escalation never produces a cross-provider promotion (REFACTOR-006), so that
/// apply step is a no-op in practice; the skeleton is retained because
/// `apply_pending_promotion` is shared with the live overflow rung-4 pivot
/// (`promote_once` / [`PendingPromotion`]). Callers MUST short-circuit BEFORE
/// invoking this when the iteration outcome is `Crash(GrokAuthFailure)` /
/// `Crash(CodexAuthFailure)` so auth lapses do not push healthy tasks toward
/// `auto_block_task`.
pub fn handle_task_failure(
    conn: &mut Connection,
    task_id: &str,
    current_iteration: i64,
    ctx: &mut IterationContext,
) -> TaskMgrResult<()> {
    handle_task_failure_with_runner(conn, task_id, current_iteration, ctx, None)
}

pub fn handle_task_failure_with_runner(
    conn: &mut Connection,
    task_id: &str,
    current_iteration: i64,
    ctx: &mut IterationContext,
    executed_runner: Option<RunnerKind>,
) -> TaskMgrResult<()> {
    // Resolve the effective runner before entering the transaction.
    // `escalate_task_model_if_needed_inner` requires an explicit RunnerKind;
    // callers that know the executed runner (production paths) thread it via
    // `executed_runner`. When None (legacy/non-Codex callers), derive from the
    // current DB model snapshot. For Codex tasks this derivation produces Claude
    // (gpt-* has no provider hint), but Codex tasks always arrive with
    // `executed_runner = Some(RunnerKind::Codex)` from the production engine.
    let runner = match executed_runner {
        Some(r) => r,
        None => {
            let current_model: Option<String> = conn
                .query_row("SELECT model FROM tasks WHERE id = ?", [task_id], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .ok()
                .flatten();
            resolve_effective_runner(
                ctx,
                task_id,
                crate::loop_engine::engine::EffectiveRunnerInput {
                    model: current_model.as_deref(),
                    provider_hint: None,
                },
            )
        }
    };
    // Phase 1: increment consecutive_failures + (conditional) model escalation
    // inside a single transaction so a mid-flight failure rolls both back.
    //
    // Phase 2 (auto-block) is intentionally OUTSIDE the transaction: the
    // lifecycle service requires `&mut Connection`, and `rusqlite::Transaction`
    // does not implement `DerefMut`. Pulling auto-block out of the tx
    // is acceptable degradation — a crash between commit and auto-block
    // simply means the bumped `consecutive_failures` re-triggers auto-block
    // on the next iteration via the same `should_auto_block` check.
    let (new_count, max_retries, pending_promotion, escalated_model) = {
        let tx = conn.transaction()?;

        let new_count = increment_consecutive_failures(&tx, task_id).map_err(|e| {
            tracing::warn!(
                task_id = %task_id,
                error = %e,
                "failed to increment consecutive_failures",
            );
            e
        })?;

        let max_retries: i32 = tx
            .query_row(
                "SELECT max_retries FROM tasks WHERE id = ?",
                [task_id],
                |r| r.get(0),
            )
            .unwrap_or(3);

        // W5: stage any deferred `PendingPromotion` and apply it only after
        // `tx.commit()?` returns Ok below, so a rolled-back DB never leaves a
        // dirty ctx. Post-hard-break the escalation never returns a promotion
        // (REFACTOR-006), so `pending_promotion` stays `None`; the skeleton is
        // retained for the shared deferred-apply contract (see fn rustdoc).
        //
        // Only escalate if auto-block won't immediately follow — the escalated
        // model would never be used.
        let mut pending_promotion: Option<PendingPromotion> = None;
        // Carry the escalated model out of the tx scope so the post-commit ctx
        // mutation can absorb it into the overflow snapshot (escape-valve
        // misfire fix). `None` when escalation did not fire or auto-block won.
        let mut escalated_model: Option<String> = None;
        if !should_auto_block(new_count, max_retries) {
            match escalate_task_model_if_needed_inner(
                &tx,
                task_id,
                new_count,
                runner,
                &ctx.resolved_models,
            ) {
                Ok((model, promotion)) => {
                    escalated_model = model;
                    pending_promotion = promotion;
                }
                Err(e) => {
                    tracing::warn!(task_id = %task_id, error = %e, "failed to escalate model");
                }
            }
        }

        tx.commit()?;
        (new_count, max_retries, pending_promotion, escalated_model)
    };

    // Commit succeeded — safe to mutate ctx.
    if let Some(p) = pending_promotion {
        apply_pending_promotion(ctx, &p);
    }
    // Refresh any existing overflow snapshot so the escape valve does not misread
    // this consecutive-failure escalation as an operator edit (see
    // `absorb_escalation_into_overflow_snapshot`). Post-commit, mirroring the
    // deferred-promotion apply: a rolled-back DB never leaves a dirty ctx.
    absorb_escalation_into_overflow_snapshot(ctx, task_id, escalated_model.as_deref());

    // Phase 2: auto-block (outside the transaction; routed through the
    // lifecycle service via auto_block_task).
    if should_auto_block(new_count, max_retries) {
        let res = auto_block_task(conn, task_id, new_count, current_iteration);
        if let Err(e) = res {
            tracing::warn!(task_id = %task_id, error = %e, "failed to auto-block task");
        } else {
            ui::emit(&format!(
                "Auto-blocked task {} after {} consecutive failures",
                task_id, new_count
            ));
        }
    }

    Ok(())
}

/// Build an `IterationResult` for a prompt overflow, logging the error to stderr.
pub(super) fn prompt_overflow_result(
    critical_size: usize,
    budget: usize,
    task_id: String,
) -> IterationResult {
    ui::emit_err(&format!(
        "FATAL: Prompt critical sections ({} bytes) exceed budget ({} bytes) for task {}. \
         Reduce base prompt.md size or split the task.",
        critical_size, budget, task_id,
    ));
    IterationResult {
        outcome: IterationOutcome::PromptOverflow,
        task_id: Some(task_id),
        files_modified: vec![],
        should_stop: true,
        output: String::new(),
        effective_model: None,
        effective_effort: None,
        effective_runner: None,
        key_decisions_count: 0,
        conversation: None,
        shown_learning_ids: Vec::new(),
    }
}

/// Probe whether the CLI rate limit has been lifted by spawning a minimal Claude call.
///
/// Sends `claude -p "." --print --max-turns 1 --no-session-persistence` and checks
/// whether the output still contains rate-limit patterns. Returns `true` if the
/// limit appears to be lifted (Claude responds without a rate-limit error).
pub(super) fn probe_rate_limit_lifted(permission_mode: &PermissionMode) -> bool {
    let binary = std::env::var("CLAUDE_BINARY").unwrap_or_else(|_| "claude".to_string());

    let mut args = vec!["--print", "--no-session-persistence", "--max-turns", "1"];

    // Use the same permission mode as the main loop so the probe doesn't hang
    // on a permission prompt.
    let allowed_tools_str;
    match permission_mode {
        PermissionMode::Dangerous => {
            args.push("--dangerously-skip-permissions");
        }
        PermissionMode::Scoped { allowed_tools } => {
            args.push("--permission-mode");
            args.push("dontAsk");
            if let Some(tools) = allowed_tools {
                allowed_tools_str = tools.clone();
                args.push("--allowedTools");
                args.push(&allowed_tools_str);
            }
        }
        PermissionMode::Auto { allowed_tools } => {
            args.push("--permission-mode");
            args.push("auto");
            if let Some(tools) = allowed_tools {
                allowed_tools_str = tools.clone();
                args.push("--allowedTools");
                args.push(&allowed_tools_str);
            }
        }
    }

    args.push("-p");
    args.push(".");

    let output = match std::process::Command::new(&binary)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "probe failed to spawn");
            return false;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}\n{}", stdout, stderr);

    !detection::is_rate_limited(&combined)
}

/// Update crash and stale trackers based on iteration outcome.
/// Returns true if the loop should stop.
pub(super) fn update_trackers(ctx: &mut IterationContext, outcome: &IterationOutcome) -> bool {
    match outcome {
        IterationOutcome::Completed => {
            ctx.crash_tracker.record_success();
            // Stale tracker: "different hash" means progress was made
            // We use a simple proxy: completed = progress
            false
        }
        IterationOutcome::Crash(_) => {
            ctx.crash_tracker.record_crash();
            ctx.crash_tracker.should_abort()
        }
        IterationOutcome::Blocked => {
            // Blocked is not a crash — don't increment crash counter
            ctx.crash_tracker.record_success();
            false
        }
        IterationOutcome::RateLimit => {
            // Rate limit — don't count as crash but don't reset either
            false
        }
        IterationOutcome::TransientBackend { .. } => {
            // FEAT-014: transient backend error — like RateLimit, don't count
            // as a crash and don't reset. The converged
            // `reactions::account::react_to_transient` owns the bounded
            // backoff-retry; an escalation rewrites the outcome to Crash
            // upstream (so this arm never sees the escalation case).
            false
        }
        IterationOutcome::Reorder(_) => {
            // Reorder — skip, not a real iteration result
            false
        }
        IterationOutcome::NoEligibleTasks => {
            // Stale detection handled by the outer loop via stale_tracker.check()
            false
        }
        IterationOutcome::Empty => {
            ctx.crash_tracker.record_crash();
            ctx.crash_tracker.should_abort()
        }
        IterationOutcome::PromptOverflow => {
            // Fatal — loop will stop via should_stop
            false
        }
    }
}

#[cfg(test)]
mod tests {
    // CLEANUP-001: shims removed; tests now call the relocated functions directly.

    use super::*;
    use crate::loop_engine::model::{FABLE_MODEL, HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
    use crate::loop_engine::project_config::ProjectConfig;
    use crate::loop_engine::reactions::pre_spawn::crash_escalated_model;
    use crate::loop_engine::test_utils::setup_test_db;

    // --- update_trackers tests ---

    #[test]
    fn test_update_trackers_completed_resets_crash() {
        let mut ctx = IterationContext::new(5);
        ctx.crash_tracker.record_crash();
        ctx.crash_tracker.record_crash();
        assert_eq!(ctx.crash_tracker.count(), 2);

        let should_stop = update_trackers(&mut ctx, &IterationOutcome::Completed);
        assert!(!should_stop);
        assert_eq!(ctx.crash_tracker.count(), 0);
    }

    #[test]
    fn test_update_trackers_crash_increments() {
        let mut ctx = IterationContext::new(3);
        let crash = IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError);

        update_trackers(&mut ctx, &crash);
        assert_eq!(ctx.crash_tracker.count(), 1);

        update_trackers(&mut ctx, &crash);
        assert_eq!(ctx.crash_tracker.count(), 2);
    }

    #[test]
    fn test_update_trackers_crash_signals_abort() {
        let mut ctx = IterationContext::new(2);
        let crash = IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError);

        update_trackers(&mut ctx, &crash);
        let should_stop = update_trackers(&mut ctx, &crash);
        assert!(should_stop, "Should abort after max crashes");
    }

    #[test]
    fn test_update_trackers_blocked_does_not_increment_crash() {
        let mut ctx = IterationContext::new(5);
        update_trackers(&mut ctx, &IterationOutcome::Blocked);
        assert_eq!(ctx.crash_tracker.count(), 0);
    }

    #[test]
    fn test_update_trackers_rate_limit_no_crash() {
        let mut ctx = IterationContext::new(5);
        ctx.crash_tracker.record_crash(); // pre-existing crash
        update_trackers(&mut ctx, &IterationOutcome::RateLimit);
        // Should not reset or increment
        assert_eq!(ctx.crash_tracker.count(), 1);
    }

    #[test]
    fn test_update_trackers_reorder_no_crash() {
        let mut ctx = IterationContext::new(5);
        let reorder = IterationOutcome::Reorder("FEAT-001".to_string());
        let should_stop = update_trackers(&mut ctx, &reorder);
        assert!(!should_stop);
        assert_eq!(ctx.crash_tracker.count(), 0);
    }

    #[test]
    fn test_update_trackers_empty_increments_crash() {
        let mut ctx = IterationContext::new(5);
        update_trackers(&mut ctx, &IterationOutcome::Empty);
        assert_eq!(ctx.crash_tracker.count(), 1);
    }

    // --- crash_escalated_model tests (relocated leaf from check_crash_escalation) ---

    /// Build a `crashed_last_iteration` map from a slice of `(task_id, is_crash)` pairs.
    fn crash_map(entries: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect()
    }

    /// First iteration: empty map — no crash recorded yet.
    #[test]
    fn test_crash_escalation_first_iteration_no_crash() {
        let result = crash_escalated_model(&crash_map(&[]), "FEAT-001", Some(SONNET_MODEL));
        assert_eq!(
            result, None,
            "first iteration without crash must not escalate"
        );
    }

    /// First iteration with crash: task absent from map — no escalation yet
    /// (the pipeline writes to the map AFTER the iteration, so the first pick
    /// of a new task always finds it absent).
    #[test]
    fn test_crash_escalation_first_iteration_with_crash() {
        let result = crash_escalated_model(&crash_map(&[]), "FEAT-001", Some(SONNET_MODEL));
        assert_eq!(
            result, None,
            "first iteration crash has no previous task context, cannot escalate"
        );
    }

    /// Same task but no crash — no escalation.
    #[test]
    fn test_crash_escalation_same_task_no_crash() {
        let result = crash_escalated_model(
            &crash_map(&[("FEAT-001", false)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(result, None, "same task without crash must not escalate");
    }

    /// Different task with crash — no escalation (crash on a different task
    /// does not carry forward).
    #[test]
    fn test_crash_escalation_different_task_with_crash() {
        let result = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-002",
            Some(SONNET_MODEL),
        );
        assert_eq!(
            result, None,
            "crash on different task must not escalate for new task"
        );
    }

    /// AC: same task + crash + haiku model → escalate to sonnet.
    #[test]

    fn test_crash_escalation_haiku_to_sonnet() {
        let result = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(
            result,
            Some(SONNET_MODEL.to_string()),
            "haiku crash on same task must escalate to sonnet"
        );
    }

    /// AC: same task + crash + sonnet model → escalate to opus.
    #[test]

    fn test_crash_escalation_sonnet_to_opus() {
        let result = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "sonnet crash on same task must escalate to opus"
        );
    }

    /// AC: same task + crash + opus → escalate one DEFINED tier up the Claude
    /// ladder to the Frontier rung (fable). opus is the Standard rung now; the
    /// ceiling self-loop happens at fable, not opus.
    #[test]
    fn test_crash_escalation_opus_to_fable() {
        let result = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(OPUS_MODEL),
        );
        assert_eq!(
            result,
            Some(FABLE_MODEL.to_string()),
            "opus crash must escalate up one tier to the frontier (fable)"
        );
    }

    /// AC: same task + crash + already at the frontier (fable) → stays fable
    /// (ceiling self-loop, no panic, no wrap).
    #[test]
    fn test_crash_escalation_fable_ceiling() {
        let result = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(FABLE_MODEL),
        );
        assert_eq!(
            result,
            Some(FABLE_MODEL.to_string()),
            "fable crash on same task must stay at the frontier ceiling"
        );
    }

    /// AC: resolved_model=None + crash → treated as SONNET_MODEL baseline,
    /// escalated to OPUS_MODEL. Architect decision: None crash assumes sonnet
    /// baseline and escalates to opus (not a no-op).
    #[test]

    fn test_crash_escalation_none_model_to_opus() {
        let result = crash_escalated_model(&crash_map(&[("FEAT-001", true)]), "FEAT-001", None);
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "None model crash must assume sonnet baseline and escalate to opus"
        );
    }

    /// Empty / whitespace-only models must be normalized to baseline so they
    /// escalate to opus rather than silently dropping the model on the floor.
    /// Keeps `check_crash_escalation` and `escalate_task_model_if_needed` in sync.
    #[test]
    fn test_crash_escalation_empty_and_whitespace_normalize_to_opus() {
        for bad in ["", "   ", "\t", " \n "] {
            let result =
                crash_escalated_model(&crash_map(&[("FEAT-001", true)]), "FEAT-001", Some(bad));
            assert_eq!(
                result,
                Some(OPUS_MODEL.to_string()),
                "bogus model {bad:?} must normalize to sonnet baseline and escalate"
            );
        }
    }

    /// Known-bad discriminator: escalation requires BOTH same task AND crash.
    /// An implementation that checks only one condition would pass one assertion
    /// but fail the other.
    #[test]

    fn test_crash_escalation_requires_both_conditions() {
        // Only same task (no crash) — must NOT escalate
        let no_crash = crash_escalated_model(
            &crash_map(&[("FEAT-001", false)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(no_crash, None, "same task without crash must NOT escalate");

        // Only crash (different task) — must NOT escalate
        let diff_task = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-002",
            Some(SONNET_MODEL),
        );
        assert_eq!(diff_task, None, "crash on different task must NOT escalate");

        // BOTH conditions — MUST escalate
        let both = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(
            both,
            Some(OPUS_MODEL.to_string()),
            "same task + crash MUST escalate"
        );
    }

    // ===== TEST-004: Comprehensive crash recovery escalation tests =====

    /// AC: Crash on task A, success on task A, crash on task A again.
    /// After success the map entry flips to false, so the next crash escalates
    /// from the base model (not the previously escalated model).
    #[test]
    fn test_crash_escalation_success_resets_escalation() {
        // First crash: haiku → sonnet
        let first = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(first, Some(SONNET_MODEL.to_string()));

        // After success the pipeline writes false into the map.
        let after_success = crash_escalated_model(
            &crash_map(&[("FEAT-001", false)]),
            "FEAT-001",
            first.as_deref(),
        );
        assert_eq!(
            after_success, None,
            "After success, no crash escalation should occur"
        );

        // Crash again on same task with original base model.
        let second_crash = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(
            second_crash,
            Some(SONNET_MODEL.to_string()),
            "After success reset, crash escalates from base model again"
        );
    }

    /// AC: Crash on task A, then task B is picked → no escalation for task B.
    /// The crash flag is keyed by task_id; TASK-B is absent from the map.
    #[test]
    fn test_crash_escalation_task_boundary_isolation() {
        // Crash on task A: haiku → sonnet
        let crash_a =
            crash_escalated_model(&crash_map(&[("TASK-A", true)]), "TASK-A", Some(HAIKU_MODEL));
        assert_eq!(crash_a, Some(SONNET_MODEL.to_string()));

        // Task B is selected next. TASK-A crashed but TASK-B is absent from map.
        let crash_b =
            crash_escalated_model(&crash_map(&[("TASK-A", true)]), "TASK-B", Some(HAIKU_MODEL));
        assert_eq!(
            crash_b, None,
            "Crash escalation must not carry across task boundaries"
        );
    }

    /// AC: Crash escalation is independent of CrashTracker backoff count.
    /// check_crash_escalation only consults the map and resolved_model.
    #[test]
    fn test_crash_escalation_independent_of_crash_tracker() {
        // Same map + same task + same model → same result every time.
        let result1 = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        let result2 = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(
            result1, result2,
            "Same inputs must produce same outputs — no hidden state"
        );
        assert_eq!(
            result1,
            Some(SONNET_MODEL.to_string()),
            "Escalation result is deterministic"
        );
    }

    /// Edge case: multiple consecutive crashes on same task follow the ladder:
    /// haiku → sonnet → opus → fable → fable (ceiling self-loop).
    #[test]
    fn test_crash_escalation_consecutive_ladder() {
        let crashed = crash_map(&[("FEAT-001", true)]);
        // First crash: haiku → sonnet
        let first = crash_escalated_model(&crashed, "FEAT-001", Some(HAIKU_MODEL));
        assert_eq!(
            first,
            Some(SONNET_MODEL.to_string()),
            "first crash: haiku → sonnet"
        );

        // Second crash: feed escalated model back in (sonnet → opus)
        let second = crash_escalated_model(&crashed, "FEAT-001", first.as_deref());
        assert_eq!(
            second,
            Some(OPUS_MODEL.to_string()),
            "second crash: sonnet → opus"
        );

        // Third crash: opus → fable (Standard → Frontier)
        let third = crash_escalated_model(&crashed, "FEAT-001", second.as_deref());
        assert_eq!(
            third,
            Some(FABLE_MODEL.to_string()),
            "third crash: opus → fable (one tier up)"
        );

        // Fourth crash: fable → fable (Frontier ceiling, self-loop)
        let fourth = crash_escalated_model(&crashed, "FEAT-001", third.as_deref());
        assert_eq!(
            fourth,
            Some(FABLE_MODEL.to_string()),
            "fourth crash: fable stays at the frontier ceiling"
        );
    }

    // --- retry tracking and auto-block tests ---
    //
    // Active tests verify the "should NOT block/escalate" cases — these pass
    // against the current stub (returns false).
    // Ignored tests define the expected behavior contract for FEAT-003/FEAT-004.

    /// Active: auto-block must NOT trigger on first attempt (consecutive_failures=0).
    #[test]
    fn test_auto_block_not_triggered_on_first_attempt() {
        assert!(
            !should_auto_block(0, 3),
            "auto-block must not fire on first attempt (consecutive_failures=0, max_retries=3)"
        );
    }

    /// Active: max_retries=0 disables auto-block entirely (never fires regardless of failures).
    #[test]
    fn test_auto_block_disabled_when_max_retries_zero() {
        assert!(
            !should_auto_block(0, 0),
            "max_retries=0 must disable auto-block at 0 failures"
        );
        assert!(
            !should_auto_block(5, 0),
            "max_retries=0 must disable auto-block at 5 failures"
        );
        assert!(
            !should_auto_block(100, 0),
            "max_retries=0 must disable auto-block regardless of failure count"
        );
    }

    /// Active: auto-block does NOT fire one below the threshold (2 < 3).
    #[test]
    fn test_auto_block_not_triggered_below_threshold() {
        assert!(
            !should_auto_block(2, 3),
            "auto-block must not fire at consecutive_failures=2, max_retries=3 (threshold not reached)"
        );
    }

    /// Active: negative consecutive_failures never triggers auto-block (safety invariant).
    #[test]
    fn test_auto_block_negative_failures_safe() {
        assert!(
            !should_auto_block(-1, 3),
            "negative consecutive_failures must never trigger auto-block"
        );
    }

    /// Active: model escalation NOT triggered at zero failures.
    #[test]
    fn test_failure_escalation_not_triggered_on_zero_failures() {
        assert!(
            !should_escalate_for_consecutive_failures(0),
            "model escalation must not fire at consecutive_failures=0"
        );
    }

    /// Active: model escalation NOT triggered at consecutive_failures=1.
    #[test]
    fn test_failure_escalation_not_triggered_on_single_failure() {
        assert!(
            !should_escalate_for_consecutive_failures(1),
            "model escalation must not fire at consecutive_failures=1"
        );
    }

    /// Auto-block triggers at exactly the max_retries threshold.
    #[test]
    fn test_auto_block_triggers_at_max_retries_threshold() {
        assert!(
            should_auto_block(3, 3),
            "auto-block must fire when consecutive_failures == max_retries (3 >= 3)"
        );
    }

    /// Auto-block triggers above the threshold.
    #[test]
    fn test_auto_block_triggers_above_threshold() {
        assert!(
            should_auto_block(4, 3),
            "auto-block must fire when consecutive_failures > max_retries (4 >= 3)"
        );
    }

    /// Auto-block triggers with max_retries=1 after one failure.
    #[test]
    fn test_auto_block_triggers_with_max_retries_one() {
        assert!(
            should_auto_block(1, 1),
            "auto-block must fire when consecutive_failures=1 and max_retries=1"
        );
    }

    /// Model escalation fires at consecutive_failures >= 2 (before auto-block at 3).
    #[test]
    fn test_failure_escalation_fires_at_consecutive_failures_two() {
        assert!(
            should_escalate_for_consecutive_failures(2),
            "model escalation must fire at consecutive_failures=2 (before auto-block threshold of 3)"
        );
    }

    /// Model escalation also fires at consecutive_failures=3.
    #[test]
    fn test_failure_escalation_fires_at_three() {
        assert!(
            should_escalate_for_consecutive_failures(3),
            "model escalation must fire at consecutive_failures=3"
        );
    }

    /// consecutive_failures increments by 1 in the DB after a non-Completed outcome.
    #[test]
    fn test_consecutive_failures_increments_in_db() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let new_count = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert_eq!(
            new_count, 1,
            "consecutive_failures must increment from 0 to 1"
        );

        let new_count2 = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert_eq!(
            new_count2, 2,
            "consecutive_failures must increment from 1 to 2"
        );
    }

    /// consecutive_failures resets to 0 in the DB after a Completed outcome.
    #[test]
    fn test_consecutive_failures_resets_to_zero_in_db() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 3)",
            [],
        )
        .unwrap();

        reset_consecutive_failures(&conn, "T-001").unwrap();

        let count: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "consecutive_failures must reset to 0 after success"
        );
    }

    /// Auto-block sets last_error with a descriptive message.
    #[test]
    fn test_auto_block_sets_last_error_with_descriptive_message() {
        let (_dir, mut conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures, max_retries) VALUES ('T-001', 'Test', 'in_progress', 3, 3)",
            [],
        )
        .unwrap();

        auto_block_task(&mut conn, "T-001", 3, 1).unwrap();

        let (status, last_error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, last_error FROM tasks WHERE id = 'T-001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            status, "blocked",
            "auto-blocked task must have status='blocked'"
        );
        assert!(last_error.is_some(), "auto-block must set last_error");
        let err = last_error.unwrap();
        // Message must reference failures — exact wording up to implementer
        assert!(
            err.contains('3')
                || err.to_lowercase().contains("consecutive")
                || err.to_lowercase().contains("fail"),
            "last_error must describe the failure count, got: '{}'",
            err
        );
    }

    /// Task succeeds on 3rd attempt → counter resets to 0, auto-block NOT triggered.
    #[test]
    fn test_task_succeeds_on_third_attempt_counter_resets() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures, max_retries) VALUES ('T-001', 'Test', 'in_progress', 0, 3)",
            [],
        )
        .unwrap();

        // Two failures (counter: 0 → 2, below max_retries=3 → no auto-block)
        let c1 = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert_eq!(c1, 1);
        assert!(!should_auto_block(c1, 3), "no auto-block at count=1");

        let c2 = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert_eq!(c2, 2);
        assert!(
            !should_auto_block(c2, 3),
            "no auto-block at count=2 (below max_retries=3)"
        );

        // Success on 3rd attempt → counter resets to 0
        reset_consecutive_failures(&conn, "T-001").unwrap();
        let final_count: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            final_count, 0,
            "counter must reset to 0 after success on attempt 3"
        );
        assert!(
            !should_auto_block(final_count, 3),
            "reset counter must not trigger auto-block"
        );
    }

    /// Rapid alternating success/failure on same task → counter tracks correctly.
    #[test]
    fn test_rapid_alternating_success_failure_tracks_correctly() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        // Pattern: fail → reset → fail → fail → reset
        increment_consecutive_failures(&conn, "T-001").unwrap(); // 1
        reset_consecutive_failures(&conn, "T-001").unwrap(); // 0
        increment_consecutive_failures(&conn, "T-001").unwrap(); // 1
        increment_consecutive_failures(&conn, "T-001").unwrap(); // 2
        reset_consecutive_failures(&conn, "T-001").unwrap(); // 0

        let count: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "reset must zero counter regardless of prior alternation pattern"
        );

        // Verify next failure increments from 0
        increment_consecutive_failures(&conn, "T-001").unwrap();
        let count2: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count2, 1,
            "failure after reset must start from 0, not carry over prior streak"
        );
    }

    /// Resetting one task's failures does not affect a different task's counter.
    #[test]
    fn test_reset_scoped_to_task_not_cross_task() {
        let (_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES
             ('T-001', 'Task A', 'in_progress', 2),
             ('T-002', 'Task B', 'in_progress', 0);",
        )
        .unwrap();

        // Succeeding T-002 must NOT reset T-001's counter
        reset_consecutive_failures(&conn, "T-002").unwrap();
        let count_a: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count_a, 2,
            "resetting T-002 must not affect T-001's consecutive_failures"
        );
    }

    /// Increment always produces a non-negative result (invariant).
    #[test]
    fn test_consecutive_failures_never_goes_negative() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let count = increment_consecutive_failures(&conn, "T-001").unwrap();
        assert!(
            count >= 0,
            "consecutive_failures must never be negative, got {}",
            count
        );

        reset_consecutive_failures(&conn, "T-001").unwrap();
        let after_reset: i32 = conn
            .query_row(
                "SELECT consecutive_failures FROM tasks WHERE id = 'T-001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            after_reset >= 0,
            "consecutive_failures must never be negative after reset, got {}",
            after_reset
        );
    }

    // --- escalate_task_model_if_needed tests (FEAT-004) ---

    /// Sonnet task at 2 consecutive failures → model escalated to opus in DB.
    #[test]
    fn test_model_escalation_sonnet_to_opus_at_two_failures() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{SONNET_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx).unwrap();
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "sonnet at 2 failures must escalate to opus"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(OPUS_MODEL.to_string()),
            "model column in DB must be updated to opus"
        );
    }

    /// Opus task at 2 consecutive failures → escalate one tier to the frontier
    /// (fable). opus is the Standard rung; Frontier sits above it.
    #[test]
    fn test_model_escalation_opus_to_fable() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{OPUS_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx).unwrap();
        assert_eq!(
            result,
            Some(FABLE_MODEL.to_string()),
            "opus at 2 failures must escalate up one tier to fable"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(FABLE_MODEL.to_string()),
            "model column in DB must be updated to fable"
        );
    }

    /// Fable task at 2 consecutive failures → stays at fable (Frontier ceiling).
    #[test]
    fn test_model_escalation_fable_stays_at_ceiling() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{FABLE_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx).unwrap();
        assert_eq!(
            result,
            Some(FABLE_MODEL.to_string()),
            "fable at the frontier ceiling must return fable (self-loop value)"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(FABLE_MODEL.to_string()),
            "fable model in DB must remain fable"
        );
    }

    // --- escape-valve interaction: consecutive-failure escalation must not
    //     self-trip `invalidate_stale_overrides` (Medium-1 /review-loop fix) ---

    /// Minimal `PromptResult` for driving `handle_overflow` in these tests.
    fn overflow_prompt_result(task_id: &str) -> crate::loop_engine::prompt::PromptResult {
        crate::loop_engine::prompt::PromptResult {
            prompt: "TASK\n\nBASE\n".to_string(),
            task_id: task_id.to_string(),
            task_files: Vec::new(),
            shown_learning_ids: Vec::new(),
            resolved_model: None,
            provider_hint: None,
            dropped_sections: Vec::new(),
            task_difficulty: Some("high".to_string()),
            cluster_effort: None,
            section_sizes: vec![("task", 5), ("base_prompt", 5)],
        }
    }

    /// Drive a RUNG-1 (effort downgrade) overflow on a NULL-model task. Rung 1
    /// sets `effort_overrides` and snapshots the NULL `tasks.model` into
    /// `overflow_original_task_model` (= `Some(None)`) but does NOT write
    /// `model_overrides` or `tasks.model` — the exact precondition for the
    /// escape-valve misfire. Then a consecutive-failure escalation writes
    /// `tasks.model` (sonnet-baseline → opus) WITHOUT touching `model_overrides`.
    /// The fix refreshes the snapshot so a follow-up `invalidate_stale_overrides`
    /// recognizes the ladder's own write and leaves the recovery intact.
    #[test]
    fn consecutive_failure_escalation_after_rung1_overflow_does_not_clear_channels() {
        use crate::loop_engine::reactions::post_output::{HandleOverflowParams, handle_overflow};
        use crate::loop_engine::reactions::pre_spawn::invalidate_stale_overrides;
        use tempfile::TempDir;

        let task_id = "T-001";
        let (_dir, mut conn) = setup_test_db();
        // NULL model (anchor-resolved): model column defaults to NULL.
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) \
             VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let pr = overflow_prompt_result(task_id);
        let project_cfg = ProjectConfig::default();
        let tmp = TempDir::new().unwrap();

        // Rung 1: `xhigh` is downgradeable, so the ladder takes the effort branch.
        let action = handle_overflow(HandleOverflowParams {
            ctx: &mut ctx,
            conn: &mut conn,
            task_id,
            effort: Some("xhigh"),
            effective_model: None,
            prompt_result: &pr,
            iteration: 1,
            run_id: Some("run-rung1"),
            base_dir: tmp.path(),
            slot_index: None,
            effective_runner: RunnerKind::Claude,
            project_config: &project_cfg,
        });
        assert!(
            matches!(
                action,
                crate::loop_engine::overflow::RecoveryAction::DowngradeEffort { .. }
            ),
            "xhigh effort must take the rung-1 downgrade branch, got {action:?}",
        );
        // Precondition state: effort override set, snapshot is Some(None), and
        // crucially model_overrides is EMPTY (the misfire trigger).
        assert!(
            ctx.effort_overrides.contains_key(task_id),
            "rung 1 must record the effort downgrade",
        );
        assert_eq!(
            ctx.overflow_original_task_model.get(task_id),
            Some(&None),
            "rung 1 snapshots the NULL tasks.model as Some(None)",
        );
        assert!(
            !ctx.model_overrides.contains_key(task_id),
            "rung 1 does NOT write model_overrides — this is what makes the \
             escape valve's NULL-original absorb branch fail without the fix",
        );

        // Consecutive-failure escalation: NULL baseline → opus, writes tasks.model.
        let escalated = escalate_task_model_if_needed(&conn, task_id, 2, &mut ctx).unwrap();
        assert_eq!(
            escalated,
            Some(OPUS_MODEL.to_string()),
            "NULL-baseline task at 2 failures escalates to opus",
        );
        // The fix refreshed the snapshot to the escalated model.
        assert_eq!(
            ctx.overflow_original_task_model.get(task_id),
            Some(&Some(OPUS_MODEL.to_string())),
            "the escalation's tasks.model write must be absorbed into the snapshot",
        );

        // The escape valve must now treat the escalation as the ladder's own
        // write (snapshot == current) and NOT clear the recovery channels.
        invalidate_stale_overrides(&mut ctx, &conn, task_id);
        assert!(
            ctx.effort_overrides.contains_key(task_id),
            "the effort downgrade must SURVIVE — the escape valve must not \
             self-trip on the consecutive-failure escalation's own write",
        );
        assert!(
            ctx.overflow_recovered.contains(task_id),
            "overflow_recovered must survive the no-op invalidate pass",
        );
        assert!(
            ctx.overflow_original_task_model.contains_key(task_id),
            "the snapshot must survive (no six-channel clear fired)",
        );
    }

    /// `and_modify` discipline: a task that never overflowed has no snapshot, and
    /// a consecutive-failure escalation must NOT create one — otherwise the
    /// escape valve would start tracking a task it should ignore.
    #[test]
    fn escalation_without_prior_overflow_creates_no_snapshot() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!(
                "INSERT INTO tasks (id, title, status, model, consecutive_failures) \
                 VALUES ('T-001', 'Test', 'in_progress', '{SONNET_MODEL}', 0)"
            ),
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let escalated = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx).unwrap();
        assert_eq!(escalated, Some(OPUS_MODEL.to_string()));
        assert!(
            !ctx.overflow_original_task_model.contains_key("T-001"),
            "a never-overflowed task must not gain an overflow snapshot from escalation",
        );
    }

    /// A genuine out-of-band operator edit AFTER a consecutive-failure escalation
    /// must still fire the six-channel clear — the fix narrows the absorb to the
    /// ladder's own write, it does not disable the escape valve.
    #[test]
    fn operator_edit_after_consecutive_failure_escalation_still_clears() {
        use crate::loop_engine::reactions::post_output::{HandleOverflowParams, handle_overflow};
        use crate::loop_engine::reactions::pre_spawn::invalidate_stale_overrides;
        use tempfile::TempDir;

        let task_id = "T-001";
        let (_dir, mut conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) \
             VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let pr = overflow_prompt_result(task_id);
        let project_cfg = ProjectConfig::default();
        let tmp = TempDir::new().unwrap();
        handle_overflow(HandleOverflowParams {
            ctx: &mut ctx,
            conn: &mut conn,
            task_id,
            effort: Some("xhigh"),
            effective_model: None,
            prompt_result: &pr,
            iteration: 1,
            run_id: Some("run-rung1"),
            base_dir: tmp.path(),
            slot_index: None,
            effective_runner: RunnerKind::Claude,
            project_config: &project_cfg,
        });
        escalate_task_model_if_needed(&conn, task_id, 2, &mut ctx).unwrap();

        // Operator edits tasks.model to a DIFFERENT model out-of-band.
        conn.execute(
            &format!("UPDATE tasks SET model = '{HAIKU_MODEL}' WHERE id = 'T-001'"),
            [],
        )
        .unwrap();
        invalidate_stale_overrides(&mut ctx, &conn, task_id);
        assert!(
            !ctx.effort_overrides.contains_key(task_id),
            "a genuine operator edit after escalation must still fire the clear",
        );
        assert!(
            !ctx.overflow_original_task_model.contains_key(task_id),
            "the six-channel clear must remove the snapshot on a real operator edit",
        );
    }

    /// Task with None model at 2 consecutive failures → model set to opus (sonnet baseline).
    #[test]
    fn test_model_escalation_none_model_to_opus() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx).unwrap();
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "None model assumes sonnet baseline and must escalate to opus"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(OPUS_MODEL.to_string()),
            "model in DB must be set to opus when previously unset"
        );
    }

    /// Escalation not triggered at 1 consecutive failure (threshold is 2).
    #[test]
    fn test_model_escalation_not_triggered_at_one_failure() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{SONNET_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed(&conn, "T-001", 1, &mut ctx).unwrap();
        assert_eq!(result, None, "no escalation at 1 failure (threshold is 2)");
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(SONNET_MODEL.to_string()),
            "model in DB must be unchanged at 1 failure"
        );
    }

    // --- REFACTOR-007: operator-resolved config threading ---
    //
    // Both escalation paths must resolve the Claude tier ladder from the
    // OPERATOR's config (carried on `IterationContext::resolved_models`), not the
    // builtin defaults. These use a production-shaped FR-001 JSON config with a
    // custom Claude ladder that omits the `standard` rung — so a `sonnet`
    // (cost-efficient) baseline escalates PAST the (undefined) standard rung
    // straight to the frontier (`fable`). The builtin ladder would give `opus`,
    // so the two outputs are unambiguously distinguishable.

    /// Build a production-shaped (FR-001 JSON, real serde field names) Claude
    /// ladder with NO `standard` rung. Model ids come from the exported
    /// constants — never literals — so `no_hardcoded_models` stays green.
    fn operator_models_no_standard_rung() -> model::ResolvedModelsConfig {
        let json = serde_json::json!({
            "primaryProvider": "claude",
            "anchor": "standard",
            "providers": {
                "claude": {
                    "enabled": true,
                    "tiers": {
                        "cheapest": HAIKU_MODEL,
                        "cost-efficient": SONNET_MODEL,
                        "frontier": FABLE_MODEL
                    }
                }
            }
        });
        let models: crate::loop_engine::project_config::ModelsConfig =
            serde_json::from_value(json).expect("production-shaped models JSON deserializes");
        model::resolve_models_config(
            &models,
            &crate::loop_engine::project_config::RoutingConfig::default(),
        )
    }

    /// Consecutive-failure escalation writes a model from the OPERATOR ladder.
    /// With the standard rung dropped, sonnet escalates to fable — the builtin
    /// ladder would write opus, so this falsifies a `builtin_resolved_models()`
    /// regression at `escalate_task_model_if_needed_inner`.
    #[test]
    fn test_consecutive_escalation_uses_operator_ladder_not_builtin() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{SONNET_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        ctx.resolved_models = operator_models_no_standard_rung();

        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx).unwrap();
        assert_eq!(
            result,
            Some(FABLE_MODEL.to_string()),
            "operator ladder (no standard rung): sonnet must escalate past the \
             undefined standard rung to the frontier (fable)"
        );
        assert_ne!(
            result,
            Some(OPUS_MODEL.to_string()),
            "must NOT fall back to the builtin ladder (which would write opus)"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(FABLE_MODEL.to_string()),
            "the DB model column must be written from the operator ladder"
        );
    }

    /// Crash escalation resolves the same operator ladder via
    /// `crash_escalated_model_with_config` (the production path
    /// `resolve_task_execution` uses): sonnet → fable, not opus.
    #[test]
    fn test_crash_escalation_uses_operator_ladder_not_builtin() {
        use crate::loop_engine::reactions::pre_spawn::crash_escalated_model_with_config;

        let operator = operator_models_no_standard_rung();
        let result = crash_escalated_model_with_config(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(SONNET_MODEL),
            &operator,
        );
        assert_eq!(
            result,
            Some(FABLE_MODEL.to_string()),
            "operator ladder: sonnet crash must escalate to the frontier (fable)"
        );
        assert_ne!(
            result,
            Some(OPUS_MODEL.to_string()),
            "must NOT use the builtin ladder (which would give opus)"
        );

        // Same inputs against the builtin ladder still give opus — proving the
        // divergence is the config input, not the model/task arguments.
        let builtin = crash_escalated_model_with_config(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(SONNET_MODEL),
            model::builtin_resolved_models(),
        );
        assert_eq!(builtin, Some(OPUS_MODEL.to_string()));
    }

    /// Default-config behavior is byte-identical: with the builtin ladder on
    /// the context (the `IterationContext::new` default), consecutive-failure
    /// escalation still walks sonnet → opus exactly as before the threading.
    #[test]
    fn test_consecutive_escalation_default_config_byte_identical() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{SONNET_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx).unwrap();
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "default (builtin) ladder must still escalate sonnet → opus"
        );
    }

    /// Known-bad regression: confirms `handle_task_failure_with_runner` is
    /// NEVER called for `Crash(CodexAuthFailure)` at either caller, so an auth
    /// lapse never pushes a healthy task toward `auto_block_task`. Both
    /// sequential (orchestrator.rs) and wave (wave_scheduler.rs) gates must
    /// list `CodexAuthFailure` in the exclusion match — if a future refactor
    /// drops it, this test fails.
    #[test]
    fn test_codex_auth_failure_excluded_at_callers() {
        let orch = std::fs::read_to_string("src/loop_engine/orchestrator.rs")
            .expect("orchestrator.rs readable");
        let wave = std::fs::read_to_string("src/loop_engine/wave_scheduler.rs")
            .expect("wave_scheduler.rs readable");
        // Both files must list CodexAuthFailure in the exclusion pattern next
        // to handle_task_failure_with_runner.
        assert!(
            orch.contains("CrashType::CodexAuthFailure"),
            "orchestrator.rs MUST exclude CodexAuthFailure from handle_task_failure",
        );
        assert!(
            wave.contains("CrashType::CodexAuthFailure"),
            "wave_scheduler.rs MUST exclude CodexAuthFailure from handle_task_failure",
        );
    }

    // --- Category C recovery primitive unit tests (moved from orchestrator.rs) ---
    //
    // Shadow tests for the future `TaskLifecycle` service surface. Each
    // future verb is mirrored by a thin in-module wrapper whose SQL matches
    // today's legacy site byte-for-byte (the inline bulk-recovery UPDATE at
    // `engine.rs:2407` / `engine.rs:3258`, `auto_block_task` at
    // `engine.rs:5145`, and `reset_task_to_todo` at `engine.rs:1642`). The
    // FEAT-006 migration replaces the wrappers with `TaskLifecycle::xxx`
    // calls; the tests themselves stay identical and become the safety
    // harness for that swap.

    use crate::db::prefix::prefix_and;
    use crate::loop_engine::test_utils::insert_task;
    use rusqlite::{Connection, params};

    /// Future `TaskLifecycle::recover_in_progress_for_prefix`.
    ///
    /// Today: inline SQL at engine.rs:2407 (mid-loop sweep) and
    /// engine.rs:3258 (startup Step 6.6). Both share this exact shape.
    fn recover_in_progress_for_prefix(
        conn: &Connection,
        prefix: Option<&str>,
    ) -> rusqlite::Result<usize> {
        let (clause, param) = prefix_and(prefix);
        let sql = format!(
            "UPDATE tasks SET status = 'todo', started_at = NULL \
             WHERE status = 'in_progress' {clause}"
        );
        let ps: Vec<&dyn rusqlite::types::ToSql> = match &param {
            Some(p) => vec![p as &dyn rusqlite::types::ToSql],
            None => vec![],
        };
        conn.execute(&sql, ps.as_slice())
    }

    /// Future `TaskLifecycle::auto_block_after_failures(id, err, iter)`.
    ///
    /// Today: `auto_block_task` writes unconditionally; the future verb
    /// gates on `status='in_progress'` and returns `applied: bool` so
    /// terminal rows are a clean no-op. The wrapper pre-checks status
    /// to encode that contract today; post-migration the gate moves
    /// into the service body.
    fn auto_block_after_failures(
        conn: &Connection,
        task_id: &str,
        err: &str,
        iteration: i64,
    ) -> rusqlite::Result<bool> {
        let status: String =
            conn.query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |r| {
                r.get(0)
            })?;
        if status != "in_progress" {
            return Ok(false);
        }
        let rows = conn.execute(
            "UPDATE tasks SET status = 'blocked', last_error = ?, \
             blocked_at_iteration = ?, updated_at = datetime('now') \
             WHERE id = ?",
            params![err, iteration, task_id],
        )?;
        Ok(rows > 0)
    }

    /// Future `TaskLifecycle::resurrect_for_iteration(prefix, ids)`.
    ///
    /// Today: per-id reset (cf. `reset_task_to_todo` at engine.rs:1642).
    /// The future verb takes an explicit id slice and an optional prefix
    /// scope guard so cross-PRD ids are rejected at the boundary.
    fn resurrect_for_iteration(
        conn: &Connection,
        prefix: Option<&str>,
        ids: &[&str],
    ) -> rusqlite::Result<usize> {
        let mut count = 0;
        for id in ids {
            if let Some(pfx) = prefix
                && !id.starts_with(pfx)
            {
                continue;
            }
            count += conn.execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL, \
                 updated_at = datetime('now') WHERE id = ?",
                [id],
            )?;
        }
        Ok(count)
    }

    // --- AC 1, 2, 3: recover_in_progress_for_prefix ---

    #[test]
    fn recover_in_progress_unscoped_reverts_all_in_progress_to_todo() {
        let (_tmp, conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "in_progress", 10);
        insert_task(&conn, "FIX-2", "t", "in_progress", 10);
        insert_task(&conn, "FEAT-3", "t", "done", 10);
        conn.execute(
            "UPDATE tasks SET started_at = datetime('now') WHERE status = 'in_progress'",
            [],
        )
        .unwrap();

        let count = recover_in_progress_for_prefix(&conn, None).unwrap();
        assert_eq!(count, 2, "both in_progress rows must be reset");

        for id in ["FEAT-1", "FIX-2"] {
            let (status, started): (String, Option<String>) = conn
                .query_row(
                    "SELECT status, started_at FROM tasks WHERE id = ?",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, "todo", "{id} must be reset to todo");
            assert!(started.is_none(), "{id} started_at must be cleared");
        }
        // Terminal row untouched.
        let done: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-3'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(done, "done", "terminal row must not be touched");
    }

    #[test]
    fn recover_in_progress_prefix_scoped_only_touches_matching_rows() {
        let (_tmp, conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "in_progress", 10);
        insert_task(&conn, "FEAT-2", "t", "in_progress", 10);
        insert_task(&conn, "FIX-1", "t", "in_progress", 10);

        // `prefix_and` convention: bare prefix without trailing dash;
        // the helper appends `-%` to produce the LIKE pattern. Concurrent
        // loops on different PRDs MUST NOT reset each other's rows.
        let count = recover_in_progress_for_prefix(&conn, Some("FEAT")).unwrap();
        assert_eq!(count, 2, "only FEAT- rows in scope");

        let fix_status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FIX-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            fix_status, "in_progress",
            "prefix scope MUST NOT leak across PRD boundaries",
        );
    }

    #[test]
    fn recover_in_progress_empty_result_returns_zero() {
        let (_tmp, conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "todo", 10);
        insert_task(&conn, "FEAT-2", "t", "done", 10);

        let count = recover_in_progress_for_prefix(&conn, None).unwrap();
        assert_eq!(
            count, 0,
            "no in_progress rows — no-op (autocommit; no transaction overhead)",
        );

        // No row should have changed.
        let mut stmt = conn
            .prepare("SELECT id, status FROM tasks ORDER BY id")
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            rows,
            vec![
                ("FEAT-1".to_string(), "todo".to_string()),
                ("FEAT-2".to_string(), "done".to_string()),
            ],
        );
    }

    // --- AC 4, 5: auto_block_after_failures ---

    #[test]
    fn auto_block_after_failures_sets_blocked_when_in_progress() {
        let (_tmp, conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "in_progress", 10);

        let applied =
            auto_block_after_failures(&conn, "FEAT-1", "max retries exceeded", 42).unwrap();
        assert!(applied, "in_progress→blocked transition must apply");

        let (status, last_err, blocked_iter): (String, String, i64) = conn
            .query_row(
                "SELECT status, last_error, blocked_at_iteration \
                 FROM tasks WHERE id = 'FEAT-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "blocked");
        assert_eq!(
            last_err, "max retries exceeded",
            "free-form err must be stored verbatim",
        );
        assert_eq!(blocked_iter, 42, "iteration recorded for decay-tracking",);
    }

    #[test]
    fn auto_block_after_failures_is_noop_on_done_task() {
        let (_tmp, conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "done", 10);

        let applied = auto_block_after_failures(&conn, "FEAT-1", "err", 7).unwrap();
        assert!(!applied, "terminal Done must NOT be re-blocked");

        let (status, last_err): (String, Option<String>) = conn
            .query_row(
                "SELECT status, last_error FROM tasks WHERE id = 'FEAT-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "done", "row untouched");
        assert!(
            last_err.is_none(),
            "no stderr emission AND no last_error mutation on no-op path",
        );
    }

    // --- AC 6: resurrect_for_iteration ---

    #[test]
    fn resurrect_for_iteration_flips_listed_ids_to_todo() {
        let (_tmp, conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "in_progress", 10);
        insert_task(&conn, "FEAT-2", "t", "blocked", 10);
        insert_task(&conn, "FEAT-3", "t", "done", 10);
        conn.execute(
            "UPDATE tasks SET started_at = datetime('now') WHERE id IN ('FEAT-1','FEAT-2')",
            [],
        )
        .unwrap();

        let count = resurrect_for_iteration(&conn, Some("FEAT-"), &["FEAT-1", "FEAT-2"]).unwrap();
        assert_eq!(count, 2);

        for id in ["FEAT-1", "FEAT-2"] {
            let (status, started): (String, Option<String>) = conn
                .query_row(
                    "SELECT status, started_at FROM tasks WHERE id = ?",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(status, "todo", "{id}");
            assert!(started.is_none(), "{id} started_at must be cleared");
        }

        // Out-of-list row untouched.
        let unchanged: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-3'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(unchanged, "done");
    }

    #[test]
    fn resurrect_for_iteration_prefix_filters_out_cross_prd_ids() {
        let (_tmp, conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "in_progress", 10);
        insert_task(&conn, "FIX-1", "t", "in_progress", 10);

        // FIX-1 is in the list but the FEAT- prefix guard must skip it.
        let count = resurrect_for_iteration(&conn, Some("FEAT-"), &["FEAT-1", "FIX-1"]).unwrap();
        assert_eq!(count, 1, "only FEAT-1 reset");

        let fix_status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FIX-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            fix_status, "in_progress",
            "cross-PRD id must be skipped at the boundary",
        );
    }

    // --- CONTRACT-PROMO-001: promote_once cross-provider idempotency primitive ---

    /// AC: already-promoted task → None. A `runner_overrides` entry (in EITHER
    /// direction) is the single snapshot that bounds a task to one cross-provider
    /// pivot per run; promote_once must bail on it.
    #[test]
    fn promote_once_already_promoted_returns_none() {
        let mut ctx = IterationContext::new(8);
        ctx.runner_overrides
            .insert("FEAT-1".to_string(), RunnerKind::Claude);

        let result = promote_once(
            &ctx,
            "FEAT-1",
            RunnerKind::Grok,
            RunnerKind::Claude,
            SONNET_MODEL.to_string(),
            Some(SONNET_MODEL.to_string()),
            2,
        );
        assert!(
            result.is_none(),
            "a task already carrying a promotion override must not promote again",
        );
    }

    /// AC: already-promoted task → None for the Claude→Grok direction. When the
    /// existing override is `Grok` (meaning Claude already promoted to Grok), a
    /// subsequent Claude→Grok call must return None — the guard is
    /// `runner_overrides.contains_key`, independent of which direction the
    /// NEW attempt targets.
    #[test]
    fn promote_once_already_promoted_claude_to_grok_returns_none() {
        let mut ctx = IterationContext::new(8);
        ctx.runner_overrides
            .insert("FEAT-5".to_string(), RunnerKind::Grok);

        let result = promote_once(
            &ctx,
            "FEAT-5",
            RunnerKind::Claude,
            RunnerKind::Grok,
            "grok-build".to_string(),
            Some(SONNET_MODEL.to_string()),
            2,
        );
        assert!(
            result.is_none(),
            "Claude→Grok direction: a Grok override already present must block re-promotion",
        );
    }

    /// AC: fresh task → Some(PendingPromotion) carrying every arg verbatim. The
    /// source/target fields are what keep the `apply_pending_promotion` banner
    /// direction-correct ([4532]); model/pre/count flow straight through.
    #[test]
    fn promote_once_fresh_returns_some_with_verbatim_fields() {
        let ctx = IterationContext::new(8);

        let p = promote_once(
            &ctx,
            "FEAT-2",
            RunnerKind::Grok,
            RunnerKind::Claude,
            OPUS_MODEL.to_string(),
            Some(SONNET_MODEL.to_string()),
            3,
        )
        .expect("fresh task must promote");

        assert_eq!(p.task_id, "FEAT-2");
        assert_eq!(
            p.source_runner,
            RunnerKind::Grok,
            "source drives the banner 'from' label"
        );
        assert_eq!(
            p.target_runner,
            RunnerKind::Claude,
            "target is written into runner_overrides"
        );
        assert_eq!(p.target_model, OPUS_MODEL);
        assert_eq!(p.pre_promotion_model.as_deref(), Some(SONNET_MODEL));
        assert_eq!(p.new_count, 3);
    }

    /// AC: a Some return performs NO ctx mutation. The `&IterationContext`
    /// signature makes mutation a compile error; this pins the behavioral
    /// contract so a future refactor to `&mut` can't silently start writing the
    /// override maps here (that insert belongs to `apply_pending_promotion`).
    #[test]
    fn promote_once_does_not_mutate_ctx_on_some() {
        let ctx = IterationContext::new(8);
        let before_runner = ctx.runner_overrides.clone();
        let before_model = ctx.model_overrides.clone();
        let before_orig = ctx.overflow_original_task_model.clone();

        let p = promote_once(
            &ctx,
            "FEAT-3",
            RunnerKind::Codex,
            RunnerKind::Claude,
            OPUS_MODEL.to_string(),
            None,
            2,
        );
        assert!(p.is_some(), "fresh task promotes (Some path under test)");

        assert_eq!(
            ctx.runner_overrides, before_runner,
            "promote_once must NOT touch runner_overrides — the apply step owns the insert",
        );
        assert_eq!(
            ctx.model_overrides, before_model,
            "promote_once must NOT touch model_overrides",
        );
        assert_eq!(
            ctx.overflow_original_task_model, before_orig,
            "promote_once must NOT touch overflow_original_task_model",
        );
    }

    /// Known-bad discriminator: all three cross-provider directions construct a
    /// PendingPromotion whose source/target match the caller's intent, and none
    /// ever target Codex. An implementation that hard-coded a single direction
    /// (or swapped source/target) would fail one of these rows.
    #[test]
    fn promote_once_preserves_all_cross_provider_directions() {
        let ctx = IterationContext::new(8);
        let cases = [
            ("CLAUDE-GROK", RunnerKind::Claude, RunnerKind::Grok),
            ("GROK-CLAUDE", RunnerKind::Grok, RunnerKind::Claude),
            ("CODEX-CLAUDE", RunnerKind::Codex, RunnerKind::Claude),
        ];
        for (id, source, target) in cases {
            let p = promote_once(&ctx, id, source, target, OPUS_MODEL.to_string(), None, 2)
                .expect("fresh task must promote");
            assert_eq!(p.source_runner, source, "{id}: source preserved");
            assert_eq!(p.target_runner, target, "{id}: target preserved");
            assert_ne!(
                p.target_runner,
                RunnerKind::Codex,
                "{id}: a promotion never targets Codex within a run",
            );
        }
    }

    /// Composition check across the construct+apply boundary: promote_once for
    /// Codex→Claude (caller passes target=Claude) then apply inserts Claude —
    /// NEVER Codex ([4553]) — and the now-present `runner_overrides` entry makes
    /// the next promote_once a no-op (the full idempotency loop).
    #[test]
    fn promote_once_then_apply_codex_to_claude_inserts_claude_and_then_blocks() {
        let mut ctx = IterationContext::new(8);

        let p = promote_once(
            &ctx,
            "SPIKE-1",
            RunnerKind::Codex,
            RunnerKind::Claude,
            OPUS_MODEL.to_string(),
            None,
            2,
        )
        .expect("fresh Codex task promotes");
        apply_pending_promotion(&mut ctx, &p);

        assert_eq!(
            ctx.runner_overrides.get("SPIKE-1"),
            Some(&RunnerKind::Claude),
            "Codex→Claude must insert Claude into runner_overrides, never Codex",
        );

        let again = promote_once(
            &ctx,
            "SPIKE-1",
            RunnerKind::Codex,
            RunnerKind::Claude,
            OPUS_MODEL.to_string(),
            None,
            3,
        );
        assert!(
            again.is_none(),
            "post-apply, the contains_key guard blocks re-promotion",
        );
    }
}
