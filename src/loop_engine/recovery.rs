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
use crate::loop_engine::project_config;
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
/// This is the THIRD cross-provider promotion site the symmetric-fallback
/// contract anticipated — see `src/loop_engine/CLAUDE.md` → "Symmetric
/// Claude↔Grok fallback contract": *"When you add a THIRD cross-provider
/// promotion site, replicate the `contains_key` guard there too."* The three
/// existing branches (Claude→Grok, Grok→Claude, Codex→Claude) each inline the
/// same check today; REFACTOR-001 routes them through this single guard.
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
//
// `#[allow(dead_code)]`: REFACTOR-001 adopts this at the three promotion call
// sites and removes the attribute. Until then the only callers are the unit
// tests below, so the non-test lib build sees it as unused.
#[allow(dead_code)]
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

/// Inner helper: performs the DB writes for escalation/promotion but does
/// **not** mutate `ctx`. Returns the escalated model AND an optional
/// `PendingPromotion` the caller must apply via `apply_pending_promotion`
/// after any enclosing transaction commits. Transactional callers
/// (`handle_task_failure`) MUST use this variant + apply post-commit to
/// avoid dirty-ctx on rollback.
///
/// Reads `ctx` immutably to resolve the effective runner (for the
/// idempotency guard). Does not capture override-snapshot state — that is
/// part of the deferred apply.
///
/// `project_default` / `user_default` are the engine-cached config defaults
/// (`engine.rs` run-config fields), threaded straight through to
/// `maybe_codex_fallback_to_claude` so a recovering Codex task derives its
/// baseline tier from the SAME four inputs the primary spawn-site uses (FIX-001).
#[allow(clippy::too_many_arguments)]
pub(crate) fn escalate_task_model_if_needed_inner(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    ctx: &IterationContext,
    executed_runner: RunnerKind,
    cfg: Option<&project_config::FallbackRunnerConfig>,
    primary_cfg: Option<&project_config::PrimaryRunnerConfig>,
    project_default: Option<&str>,
    user_default: Option<&str>,
) -> TaskMgrResult<(Option<String>, Option<PendingPromotion>)> {
    if !should_escalate_for_consecutive_failures(new_count) {
        return Ok((None, None));
    }
    let current_model: Option<String> =
        conn.query_row("SELECT model FROM tasks WHERE id = ?", [task_id], |r| {
            r.get::<_, Option<String>>(0)
        })?;
    let effective_runner = executed_runner;
    if effective_runner == RunnerKind::Codex {
        // FEAT-005: opt-in Codex→Claude fallback on RUNTIME failure. Auth
        // failures (CodexAuthFailure) never reach `handle_task_failure` (the
        // sequential + wave callers short-circuit on the auth crash variant),
        // so a Codex outcome here is by construction a runtime fault. Promote
        // to Claude only when:
        //   1. No prior promotion exists for this task (idempotency — mirrors
        //      the symmetric Claude↔Grok ping-pong guard below).
        //   2. The matching `primaryRunner` Codex route has
        //      `runtimeErrorFallback: true`.
        // Otherwise return `(None, None)` so the legacy auto-block ladder runs
        // (existing Codex projects without the opt-in see unchanged behavior).
        return maybe_codex_fallback_to_claude(
            conn,
            task_id,
            new_count,
            ctx,
            primary_cfg,
            project_default,
            user_default,
        );
    }
    // None / empty / whitespace model: assume sonnet baseline → escalate to opus.
    let escalated = match normalize_baseline(current_model.as_deref()) {
        None => Some(model::OPUS_MODEL.to_string()),
        Some(m) => model::escalate_model(Some(m)),
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

    // Provider promotion hooks. The effective runner is computed once against
    // the PRE-escalation model and dispatches one of two mirror branches:
    //   - Claude→Grok (FEAT-007) when the task is still on Claude.
    //   - Grok→Claude (FEAT-PRIMARY-003) when the task is on Grok.
    //
    // FEAT-PRIMARY-004 idempotency: a task that has ALREADY been promoted in
    // either direction carries a `runner_overrides` entry. Promoting it again
    // would flip it into the OPPOSITE branch (Grok→Claude then Claude→Grok …),
    // producing an infinite Claude↔Grok ping-pong bounded only by max_retries.
    // Mirror the `was_already_promoted` guard in `handle_prompt_too_long`
    // (overflow.rs): once a task has pivoted providers once, no further
    // cross-provider promotion fires — it falls through to normal failure
    // accounting → `auto_block_task` per `max_retries`.
    if ctx.runner_overrides.contains_key(task_id) {
        return Ok((escalated, None));
    }
    match effective_runner {
        // kind-correct: Claude is the source side; Grok is the fallback target — provider identity, not capability.
        RunnerKind::Claude => {
            // FEAT-007: Grok fallback promotion. Fires only when the task was
            // ALREADY at Opus before this call (i.e. the Claude escalation step
            // was a no-op self-loop or no escalation was needed) — Sonnet-tier
            // escalations get a fresh chance at Opus before any Grok pivot is
            // considered.
            let fallback = match cfg {
                Some(c) if c.enabled => c,
                _ => return Ok((escalated, None)),
            };
            // H2: use ModelTier-based inclusive check so both OPUS_MODEL and
            // OPUS_MODEL_1M qualify as "at Opus" — string-eq on OPUS_MODEL
            // excluded the 1M variant.
            let was_at_opus = matches!(
                model::model_tier(current_model.as_deref()),
                model::ModelTier::Opus
            );
            // M1: compare in u32 space; new_count is a DB counter (always >= 0 in
            // practice) but guard the negative case to keep the cast sound.
            if !was_at_opus
                || new_count < 0
                || (new_count as u32) < fallback.runtime_error_threshold
            {
                return Ok((escalated, None));
            }

            // All gates passed — promote to Grok. DB write happens here; ctx
            // mutations are bundled into a `PendingPromotion` for the caller to
            // apply after commit. The pre-promotion snapshot of `tasks.model` is
            // captured BEFORE the UPDATE rewrites it so the FEAT-008 override-
            // invalidation detector sees the original value on next iteration.
            conn.execute(
                "UPDATE tasks SET model = ? WHERE id = ?",
                rusqlite::params![fallback.model, task_id],
            )?;
            let promotion = PendingPromotion {
                task_id: task_id.to_string(),
                pre_promotion_model: current_model,
                source_runner: RunnerKind::Claude,
                target_runner: RunnerKind::Grok,
                target_model: fallback.model.clone(),
                new_count,
            };
            Ok((Some(fallback.model.clone()), Some(promotion)))
        }
        // FEAT-PRIMARY-003: inverse Grok→Claude fallback. A task routed to the
        // Grok primary runner that keeps hitting RuntimeErrors is promoted onto
        // a Claude model after `primary.runtime_error_threshold` consecutive
        // failures — the mirror of FEAT-007, opposite direction.
        RunnerKind::Grok => {
            let primary = match primary_cfg {
                Some(p) => p,
                None => return Ok((escalated, None)),
            };
            // `claudeFallbackModel` absent → no inverse promotion target; the
            // task dead-ends just like a Claude task with no Grok fallback.
            let Some(claude_model) = primary.claude_fallback_model.as_deref() else {
                return Ok((escalated, None));
            };
            // Guard the gate per AC: the DB model must actually be a Grok model
            // (provider identity), not merely a stale runner override.
            if model::provider_for_model(current_model.as_deref()) != model::Provider::Grok {
                return Ok((escalated, None));
            }
            // M1 parity: compare in u32 space and guard the negative case.
            if new_count < 0 || (new_count as u32) < primary.runtime_error_threshold {
                return Ok((escalated, None));
            }

            // All gates passed — promote to Claude. Mirror of the FEAT-007 step:
            // capture the pre-promotion (Grok) model BEFORE the UPDATE, then
            // bundle the ctx mutations into a `PendingPromotion`.
            conn.execute(
                "UPDATE tasks SET model = ? WHERE id = ?",
                rusqlite::params![claude_model, task_id],
            )?;
            let promotion = PendingPromotion {
                task_id: task_id.to_string(),
                pre_promotion_model: current_model,
                source_runner: RunnerKind::Grok,
                target_runner: RunnerKind::Claude,
                target_model: claude_model.to_string(),
                new_count,
            };
            Ok((Some(claude_model.to_string()), Some(promotion)))
        }
        RunnerKind::Codex => Ok((None, None)),
    }
}

/// FEAT-005: Codex→Claude opt-in promotion helper. Returns `(None, None)`
/// (auto-block) unless the matching `primaryRunner` Codex route opted in via
/// `runtimeErrorFallback: true`, in which case the task is promoted onto a Claude
/// model. Bound to one promotion per task — re-entry while
/// `ctx.runner_overrides` already holds the task short-circuits.
///
/// Auth failures must never reach this function — the sequential and wave
/// callers exclude `Crash(CodexAuthFailure)` before invoking
/// `handle_task_failure`. Treat any reached call as a runtime fault.
///
/// FIX-001: `project_default` / `user_default` are the engine-cached config
/// defaults threaded from the failure-handler chain. The baseline tier (used to
/// match a `baselineTierRoutes` route) is derived via
/// [`model::compute_baseline_model`] from the SAME four inputs the primary
/// spawn-site (`resolve_task_execution_target`) uses — `difficulty`,
/// `prd_default`, `project_default`, `user_default` — so a recovering Codex task
/// matches the route it was originally routed by. Earlier this site substituted
/// `primary.claude_fallback_model` for `project_default` and omitted
/// `user_default`, causing a confirmed recovery↔primary divergence.
fn maybe_codex_fallback_to_claude(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    ctx: &IterationContext,
    primary_cfg: Option<&project_config::PrimaryRunnerConfig>,
    project_default: Option<&str>,
    user_default: Option<&str>,
) -> TaskMgrResult<(Option<String>, Option<PendingPromotion>)> {
    use crate::loop_engine::model::{
        Provider, parse_config_provider, primary_runner_baseline_tier_match, primary_runner_match,
    };

    if ctx.runner_overrides.contains_key(task_id) {
        return Ok((None, None));
    }
    let Some(primary) = primary_cfg else {
        return Ok((None, None));
    };
    let difficulty: Option<String> = conn
        .query_row(
            "SELECT difficulty FROM tasks WHERE id = ?",
            [task_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let prd_default: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let baseline_model = model::compute_baseline_model(
        difficulty.as_deref(),
        prd_default.as_deref(),
        project_default,
        user_default,
    );
    // `task_type` is not threaded into recovery; production Codex routing is
    // dominated by `byIdPrefix` and `baselineTierRoutes`.
    let Some(spec) = primary_runner_match(primary, Some(task_id), None).or_else(|| {
        primary_runner_baseline_tier_match(primary, Some(task_id), baseline_model.as_deref())
    }) else {
        return Ok((None, None));
    };
    if !spec.runtime_error_fallback {
        return Ok((None, None));
    }
    // Guard against misconfiguration: the field is only meaningful on Codex
    // routes. Treating a Grok/Claude route with `runtimeErrorFallback:true` as a
    // promotion trigger would silently override the existing fallback paths.
    if parse_config_provider(&spec.provider).ok() != Some(Provider::Codex) {
        return Ok((None, None));
    }

    // Resolve the target Claude model. The AC says "OPUS for difficulty high,
    // else default" — we read the task's difficulty from the DB and pick:
    //   - high → OPUS_MODEL (matches `resolve_task_model` rung 3 semantics)
    //   - else → `primary.claude_fallback_model` if set (mirrors the
    //     symmetric Grok→Claude branch's source of truth), else OPUS_MODEL
    //     as the safe baseline. Opting in via `runtimeErrorFallback:true` makes
    //     the operator's intent explicit, so we never bail on a missing
    //     `claudeFallbackModel` (unlike the Grok→Claude branch which is
    //     project-level rather than per-route).
    let target_model = if difficulty
        .as_deref()
        .is_some_and(|d| d.eq_ignore_ascii_case("high"))
    {
        model::OPUS_MODEL.to_string()
    } else {
        primary
            .claude_fallback_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(model::OPUS_MODEL)
            .to_string()
    };

    // Capture the pre-promotion `tasks.model` BEFORE the UPDATE so the
    // FEAT-008 override-invalidation detector sees the original value on next
    // iteration. Same pattern as the Claude→Grok and Grok→Claude branches.
    let pre_promotion_model: Option<String> =
        conn.query_row("SELECT model FROM tasks WHERE id = ?", [task_id], |r| {
            r.get::<_, Option<String>>(0)
        })?;
    conn.execute(
        "UPDATE tasks SET model = ? WHERE id = ?",
        rusqlite::params![target_model, task_id],
    )?;
    let promotion = PendingPromotion {
        task_id: task_id.to_string(),
        pre_promotion_model,
        source_runner: RunnerKind::Codex,
        target_runner: RunnerKind::Claude,
        target_model: target_model.clone(),
        new_count,
    };
    Ok((Some(target_model), Some(promotion)))
}

/// Escalate the model for a task in the DB when consecutive failures reach the threshold.
///
/// Follows the same sonnet-baseline pattern as `check_crash_escalation`:
/// - `None` or empty model assumes sonnet baseline → escalates to opus.
/// - Sonnet → opus, Haiku → sonnet, Opus → opus (no-op at ceiling).
///
/// **FEAT-007 Grok promotion**: after the Claude-tier escalation runs, when
/// `cfg.enabled` AND the post-escalation model is Opus AND `effective_runner`
/// resolves to Claude AND `new_count >= cfg.runtime_error_threshold`, the
/// task is promoted to the Grok runner. The promotion writes BOTH the
/// `tasks.model` column AND the in-memory override maps on `ctx`
/// (`runner_overrides`, `model_overrides`) so the next iteration's
/// `resolve_task_model` + `resolve_effective_runner` agree. The pre-promotion
/// `tasks.model` value is captured into `ctx.overflow_original_task_model`
/// via `entry().or_insert_with(...)` so the FEAT-008 override-invalidation
/// detector can spot operator edits later.
///
/// **FEAT-PRIMARY-003 inverse Grok→Claude promotion**: the mirror branch.
/// When the task's effective runner is Grok AND `tasks.model` is a Grok model
/// AND `primary_cfg.claude_fallback_model.is_some()` AND
/// `new_count >= primary_cfg.runtime_error_threshold`, the task is promoted
/// onto the configured Claude model (same DB-column-plus-override-maps write
/// shape as the Claude→Grok hook). When `primary_cfg` is `None` or
/// `claude_fallback_model` is absent, no inverse promotion fires.
///
/// When `cfg` is `None` or `!enabled`, behavior is byte-identical to the
/// pre-FEAT-007 ladder (Sonnet → Opus → terminal) for Claude tasks. The `ctx`
/// argument is otherwise unused in that path.
///
/// Returns `Some(new_model)` if escalation OR promotion fired, `None` if
/// below threshold, the model tier is unknown (e.g. already at Grok), or
/// the Opus self-loop produced no change AND promotion conditions weren't met.
/// The DB is updated in-place when `Some` is returned.
///
/// This is the convenience variant — DB and ctx writes happen back-to-back.
/// Transactional callers should prefer `escalate_task_model_if_needed_inner`
/// + `apply_pending_promotion` (see W5).
#[allow(clippy::too_many_arguments)]
pub fn escalate_task_model_if_needed(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    ctx: &mut IterationContext,
    cfg: Option<&project_config::FallbackRunnerConfig>,
    primary_cfg: Option<&project_config::PrimaryRunnerConfig>,
    project_default: Option<&str>,
    user_default: Option<&str>,
) -> TaskMgrResult<Option<String>> {
    // Derive the runner from the DB model before entering the inner helper.
    // The inner function requires an explicit runner; callers that DO know the
    // executed runner (production paths) should use
    // `escalate_task_model_if_needed_for_runner` to avoid this pre-read.
    // For Codex tasks this derivation produces Claude (gpt-* has no provider
    // hint here), which is safe: the non-Codex escalation path is benign for
    // Claude and Grok tasks; Codex callers must use the explicit-runner variant.
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
        ctx,
        runner,
        cfg,
        primary_cfg,
        project_default,
        user_default,
    )?;
    if let Some(p) = pending {
        apply_pending_promotion(ctx, &p);
    }
    Ok(model)
}

/// Variant of [`escalate_task_model_if_needed`] for callers that know the
/// executed runner at the call site.
///
/// The explicit `executed_runner` bypasses the model-string re-derivation
/// that the plain `escalate_task_model_if_needed` uses when the runner is not
/// known. This is critical for Codex tasks: their `gpt-*` model would
/// otherwise re-classify as Claude and miss the Codex-specific early-return
/// path. Used in integration tests and by `handle_task_failure_with_runner`.
#[allow(clippy::too_many_arguments)]
pub fn escalate_task_model_if_needed_for_runner(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    executed_runner: RunnerKind,
    ctx: &mut IterationContext,
    cfg: Option<&project_config::FallbackRunnerConfig>,
    primary_cfg: Option<&project_config::PrimaryRunnerConfig>,
    project_default: Option<&str>,
    user_default: Option<&str>,
) -> TaskMgrResult<Option<String>> {
    let (model, pending) = escalate_task_model_if_needed_inner(
        conn,
        task_id,
        new_count,
        ctx,
        executed_runner,
        cfg,
        primary_cfg,
        project_default,
        user_default,
    )?;
    if let Some(p) = pending {
        apply_pending_promotion(ctx, &p);
    }
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
/// **FEAT-007**: `ctx` threads `IterationContext` through so the embedded
/// `escalate_task_model_if_needed` call can write Grok promotion overrides
/// (paired with the DB UPDATE). `cfg` carries the optional fallback-runner
/// configuration; pass `None` to suppress the Grok branch entirely (preserves
/// pre-FEAT-007 behavior byte-for-byte). Callers MUST short-circuit BEFORE
/// invoking this when the iteration outcome is `Crash(GrokAuthFailure)` so
/// auth lapses do not push healthy tasks toward `auto_block_task`.
///
/// **FEAT-PRIMARY-003**: `primary_cfg` carries the optional primary-runner
/// configuration so the embedded escalation call can fire the inverse
/// Grok→Claude promotion. Pass `None` to suppress the inverse branch.
///
/// **FIX-001**: `project_default` / `user_default` are the engine-cached config
/// defaults (`engine.rs` run-config fields), threaded through to the embedded
/// `escalate_task_model_if_needed_inner` → `maybe_codex_fallback_to_claude`
/// baseline derivation so a recovering Codex task matches the same
/// `baselineTierRoutes` route the primary spawn-site routed it by.
#[allow(clippy::too_many_arguments)]
pub fn handle_task_failure(
    conn: &mut Connection,
    task_id: &str,
    current_iteration: i64,
    ctx: &mut IterationContext,
    cfg: Option<&project_config::FallbackRunnerConfig>,
    primary_cfg: Option<&project_config::PrimaryRunnerConfig>,
    project_default: Option<&str>,
    user_default: Option<&str>,
) -> TaskMgrResult<()> {
    handle_task_failure_with_runner(
        conn,
        task_id,
        current_iteration,
        ctx,
        None,
        cfg,
        primary_cfg,
        project_default,
        user_default,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn handle_task_failure_with_runner(
    conn: &mut Connection,
    task_id: &str,
    current_iteration: i64,
    ctx: &mut IterationContext,
    executed_runner: Option<RunnerKind>,
    cfg: Option<&project_config::FallbackRunnerConfig>,
    primary_cfg: Option<&project_config::PrimaryRunnerConfig>,
    project_default: Option<&str>,
    user_default: Option<&str>,
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
    let (new_count, max_retries, pending_promotion) = {
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

        // W5: stage the Grok promotion's ctx mutations as a `PendingPromotion`
        // and defer applying them until `tx.commit()?` returns Ok below. If
        // commit fails, the in-memory ctx stays consistent with the rolled-back
        // DB (no dirty `runner_overrides` / `model_overrides` entries pointing
        // to a Grok model the DB still records as Opus).
        //
        // Only escalate if auto-block won't immediately follow — the escalated
        // model would never be used.
        let mut pending_promotion: Option<PendingPromotion> = None;
        if !should_auto_block(new_count, max_retries) {
            match escalate_task_model_if_needed_inner(
                &tx,
                task_id,
                new_count,
                ctx,
                runner,
                cfg,
                primary_cfg,
                project_default,
                user_default,
            ) {
                Ok((_model, promotion)) => {
                    pending_promotion = promotion;
                }
                Err(e) => {
                    tracing::warn!(task_id = %task_id, error = %e, "failed to escalate model");
                }
            }
        }

        tx.commit()?;
        (new_count, max_retries, pending_promotion)
    };

    // Commit succeeded — safe to mutate ctx.
    if let Some(p) = pending_promotion {
        apply_pending_promotion(ctx, &p);
    }

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
    use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
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

    /// AC: same task + crash + already opus → stays opus (ceiling, no panic).
    #[test]

    fn test_crash_escalation_opus_ceiling() {
        let result = crash_escalated_model(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(OPUS_MODEL),
        );
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "opus crash on same task must stay at opus ceiling"
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
    /// haiku → sonnet → opus → opus (ceiling).
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

        // Third crash: opus → opus (ceiling)
        let third = crash_escalated_model(&crashed, "FEAT-001", second.as_deref());
        assert_eq!(
            third,
            Some(OPUS_MODEL.to_string()),
            "third crash: opus stays at ceiling"
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
        let result =
            escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx, None, None, None, None)
                .unwrap();
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

    /// Opus task at 2 consecutive failures → model stays at opus (ceiling, no-op).
    #[test]
    fn test_model_escalation_opus_stays_at_ceiling() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            &format!("INSERT INTO tasks (id, title, status, model, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', '{OPUS_MODEL}', 0)"),
            [],
        )
        .unwrap();

        let mut ctx = IterationContext::new(8);
        let result =
            escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx, None, None, None, None)
                .unwrap();
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "opus at ceiling must return opus (no-op value)"
        );
        let model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'T-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            model,
            Some(OPUS_MODEL.to_string()),
            "opus model in DB must remain opus"
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
        let result =
            escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx, None, None, None, None)
                .unwrap();
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
        let result =
            escalate_task_model_if_needed(&conn, "T-001", 1, &mut ctx, None, None, None, None)
                .unwrap();
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

    // --- FEAT-005: opt-in Codex→Claude fallback tests ---

    use crate::loop_engine::project_config::{PrimaryRunnerConfig, RunnerSpec};

    fn primary_with_codex_route(prefix: &str, runtime_error_fallback: bool) -> PrimaryRunnerConfig {
        let mut by_id_prefix = std::collections::HashMap::new();
        by_id_prefix.insert(
            prefix.to_string(),
            RunnerSpec {
                provider: "codex".to_string(),
                model: String::new(),
                runtime_error_fallback,
            },
        );
        PrimaryRunnerConfig {
            claude_fallback_model: None,
            runtime_error_threshold: 2,
            by_task_type: std::collections::HashMap::new(),
            by_id_prefix,
            ..Default::default()
        }
    }

    fn insert_codex_task_at_threshold(conn: &Connection, id: &str, difficulty: Option<&str>) {
        // Codex routes carry `model=""` in the spec, so tasks.model is NULL.
        // The `difficulty` column drives the OPUS_MODEL escalation when
        // promoting Codex→Claude.
        let diff_col = if difficulty.is_some() {
            ", difficulty"
        } else {
            ""
        };
        let diff_val = difficulty.map(|d| format!(", '{d}'")).unwrap_or_default();
        conn.execute(
            &format!(
                "INSERT INTO tasks (id, title, status, consecutive_failures{diff_col}) \
                 VALUES ('{id}', 'Test', 'in_progress', 0{diff_val})"
            ),
            [],
        )
        .unwrap();
    }

    /// Thin wrapper: runs the inner with `executed_runner=Some(Codex)` (so
    /// the Codex branch is exercised regardless of the task's resolved
    /// model), and applies any PendingPromotion to ctx — matching the
    /// production sequence inside `handle_task_failure_with_runner`.
    fn run_codex_escalation(
        conn: &Connection,
        task_id: &str,
        new_count: i32,
        ctx: &mut IterationContext,
        primary_cfg: Option<&PrimaryRunnerConfig>,
    ) -> Option<String> {
        let (model, pending) = escalate_task_model_if_needed_inner(
            conn,
            task_id,
            new_count,
            ctx,
            RunnerKind::Codex,
            None,
            primary_cfg,
            None,
            None,
        )
        .unwrap();
        if let Some(p) = pending {
            apply_pending_promotion(ctx, &p);
        }
        model
    }

    /// Positive AC: runtimeErrorFallback:true + Codex RUNTIME failure →
    /// runner_overrides[id] == RunnerKind::Claude with a Claude model, exactly once.
    #[test]
    fn test_codex_fallback_to_claude_true_promotes_to_claude_once() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-001", Some("medium"));
        let cfg = primary_with_codex_route("SPIKE-", true);
        let mut ctx = IterationContext::new(8);

        let result = run_codex_escalation(&conn, "SPIKE-001", 2, &mut ctx, Some(&cfg));
        assert!(result.is_some(), "promotion must return Some(target_model)");
        let target = result.unwrap();

        // runner_overrides flipped to Claude
        assert_eq!(
            ctx.runner_overrides.get("SPIKE-001"),
            Some(&RunnerKind::Claude),
            "Codex→Claude promotion must insert Claude into runner_overrides",
        );
        // model_overrides carries the Claude model
        assert_eq!(
            ctx.model_overrides.get("SPIKE-001").map(String::as_str),
            Some(target.as_str()),
            "model_overrides must carry the promoted Claude model",
        );
        // DB row reflects the same model
        let db_model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'SPIKE-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            db_model.as_deref(),
            Some(target.as_str()),
            "tasks.model must reflect the promoted Claude model",
        );

        // Re-entry with executed_runner=Codex must NOT re-promote
        // (idempotency — the runner_overrides entry blocks the branch).
        let again = run_codex_escalation(&conn, "SPIKE-001", 3, &mut ctx, Some(&cfg));
        assert_eq!(
            again, None,
            "already-promoted task must NOT re-promote on the next Codex failure",
        );
        assert_eq!(
            ctx.runner_overrides.get("SPIKE-001"),
            Some(&RunnerKind::Claude),
            "runner_overrides must remain Claude — never flipped back to Codex",
        );
    }

    /// Difficulty=high → target Claude model is OPUS_MODEL.
    #[test]
    fn test_codex_fallback_to_claude_high_difficulty_uses_opus() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-002", Some("high"));
        let cfg = primary_with_codex_route("SPIKE-", true);
        let mut ctx = IterationContext::new(8);

        let result = run_codex_escalation(&conn, "SPIKE-002", 2, &mut ctx, Some(&cfg));
        assert_eq!(
            result.as_deref(),
            Some(OPUS_MODEL),
            "high difficulty must promote to OPUS_MODEL",
        );
        assert_eq!(
            ctx.model_overrides.get("SPIKE-002").map(String::as_str),
            Some(OPUS_MODEL),
        );
    }

    /// Non-high difficulty + claude_fallback_model set → uses configured Claude model.
    #[test]
    fn test_codex_fallback_to_claude_non_high_uses_configured_claude() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-003", Some("medium"));
        let mut cfg = primary_with_codex_route("SPIKE-", true);
        cfg.claude_fallback_model = Some(SONNET_MODEL.to_string());
        let mut ctx = IterationContext::new(8);

        let result = run_codex_escalation(&conn, "SPIKE-003", 2, &mut ctx, Some(&cfg));
        assert_eq!(
            result.as_deref(),
            Some(SONNET_MODEL),
            "non-high difficulty must use claude_fallback_model when set",
        );
    }

    /// Negative AC: runtimeErrorFallback:false → no promotion, auto-block (legacy).
    #[test]
    fn test_codex_fallback_to_claude_false_yields_no_promotion() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-004", Some("medium"));
        let cfg = primary_with_codex_route("SPIKE-", false);
        let mut ctx = IterationContext::new(8);

        let result = run_codex_escalation(&conn, "SPIKE-004", 2, &mut ctx, Some(&cfg));
        assert_eq!(
            result, None,
            "runtimeErrorFallback:false must return None (auto-block path)",
        );
        assert!(
            !ctx.runner_overrides.contains_key("SPIKE-004"),
            "no runner_overrides entry must be inserted",
        );
        let db_model: Option<String> = conn
            .query_row("SELECT model FROM tasks WHERE id = 'SPIKE-004'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            db_model.is_none(),
            "tasks.model must be unchanged (still NULL)",
        );
    }

    /// Negative AC: runtimeErrorFallback absent → defaults to false (serde).
    /// Tests deserialization path end-to-end.
    #[test]
    fn test_codex_fallback_to_claude_absent_defaults_to_false() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-005", Some("medium"));
        // Build the spec via JSON to exercise the serde default.
        let cfg: PrimaryRunnerConfig = serde_json::from_str(
            r#"{
                "byIdPrefix": {
                    "SPIKE-": { "provider": "codex" }
                }
            }"#,
        )
        .unwrap();
        let mut ctx = IterationContext::new(8);

        let result = run_codex_escalation(&conn, "SPIKE-005", 2, &mut ctx, Some(&cfg));
        assert_eq!(result, None, "absent runtimeErrorFallback must auto-block");
        assert!(!ctx.runner_overrides.contains_key("SPIKE-005"));
    }

    /// Negative: no primaryRunner config → no promotion.
    #[test]
    fn test_codex_no_primary_runner_config_no_promotion() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-006", Some("medium"));
        let mut ctx = IterationContext::new(8);
        // Simulate Codex executing the task even without primary_runner config
        // (e.g. an orphan ctx.runner_overrides entry from a prior session).
        ctx.runner_overrides
            .insert("SPIKE-006".to_string(), RunnerKind::Codex);

        let result = escalate_task_model_if_needed_inner(
            &conn,
            "SPIKE-006",
            2,
            &ctx,
            RunnerKind::Codex,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            result.0, None,
            "Codex without primaryRunner config must auto-block",
        );
        assert!(result.1.is_none(), "no PendingPromotion must be returned",);
    }

    /// Negative: non-Codex route with runtimeErrorFallback:true must be ignored.
    /// The field is only meaningful on Codex routes; a Grok route with the
    /// flag set must not accidentally trigger a Codex→Claude promotion path.
    #[test]
    fn test_codex_fallback_on_non_codex_route_is_ignored() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-007", Some("medium"));
        let mut by_id_prefix = std::collections::HashMap::new();
        by_id_prefix.insert(
            "SPIKE-".to_string(),
            RunnerSpec {
                provider: "grok".to_string(),
                model: "grok-build".to_string(),
                runtime_error_fallback: true, // set on a non-Codex route
            },
        );
        let cfg = PrimaryRunnerConfig {
            claude_fallback_model: None,
            runtime_error_threshold: 2,
            by_task_type: std::collections::HashMap::new(),
            by_id_prefix,
            ..Default::default()
        };
        let mut ctx = IterationContext::new(8);
        // Force-mark this task as Codex-executing so we exercise the new branch.
        ctx.runner_overrides
            .insert("SPIKE-007".to_string(), RunnerKind::Codex);
        // The branch should NOT promote: the matching route is Grok, not Codex.
        // (The Codex branch short-circuits on the provider guard.)

        // But wait — the idempotency guard short-circuits on the existing entry.
        // Strip it so we exercise the provider check, not idempotency.
        ctx.runner_overrides.remove("SPIKE-007");
        let result = escalate_task_model_if_needed_inner(
            &conn,
            "SPIKE-007",
            2,
            &ctx,
            RunnerKind::Codex,
            None,
            Some(&cfg),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            result.0, None,
            "non-Codex route with runtimeErrorFallback:true must NOT promote",
        );
    }

    /// Idempotency: a task already promoted (runner_overrides entry exists)
    /// must NOT promote again — protects against the Codex→Claude→Codex ping-pong
    /// the symmetric Claude↔Grok guard prevents on the other branches.
    #[test]
    fn test_codex_fallback_idempotent_when_already_promoted() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-008", Some("medium"));
        let cfg = primary_with_codex_route("SPIKE-", true);
        let mut ctx = IterationContext::new(8);
        // Simulate a prior promotion: task is now on Claude with an existing override.
        ctx.runner_overrides
            .insert("SPIKE-008".to_string(), RunnerKind::Claude);

        // Pass executed_runner=Codex to force-enter the Codex branch even
        // though runner_overrides says Claude (simulates a stale dispatch path).
        let result = escalate_task_model_if_needed_inner(
            &conn,
            "SPIKE-008",
            3,
            &ctx,
            RunnerKind::Codex,
            None,
            Some(&cfg),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            result.0, None,
            "already-promoted task must NOT re-promote (idempotency guard)",
        );
        assert!(
            result.1.is_none(),
            "no PendingPromotion must be returned on re-entry",
        );
    }

    /// Invariant: the Codex→Claude promotion never inserts RunnerKind::Codex
    /// into runner_overrides (cf. codex_runner_overrides_invariant.rs). Reads
    /// every inserted runner_overrides entry and verifies none are Codex.
    #[test]
    fn test_codex_fallback_promotion_never_inserts_codex_override() {
        let (_dir, conn) = setup_test_db();
        insert_codex_task_at_threshold(&conn, "SPIKE-009", Some("high"));
        let cfg = primary_with_codex_route("SPIKE-", true);
        let mut ctx = IterationContext::new(8);

        let _ = run_codex_escalation(&conn, "SPIKE-009", 2, &mut ctx, Some(&cfg));
        for (id, kind) in &ctx.runner_overrides {
            assert_ne!(
                *kind,
                RunnerKind::Codex,
                "runner_overrides[{id}] must never be Codex \
                 (the promotion inserts Claude, never Codex)",
            );
        }
    }

    /// Known-bad regression: confirms `handle_task_failure_with_runner` is
    /// NEVER called for `Crash(CodexAuthFailure)` at either caller, so the
    /// promotion path is unreachable for auth failures even when
    /// runtimeErrorFallback:true. Both sequential (orchestrator.rs) and wave
    /// (wave_scheduler.rs) gates must list `CodexAuthFailure` in the
    /// exclusion match — if a future refactor drops it, this test fails.
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
