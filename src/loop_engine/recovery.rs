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

/// Check whether crash recovery should escalate the model for this iteration.
///
/// Returns `Some(escalated_model)` when the previous iteration on
/// `current_task_id` crashed (i.e. `crashed_last_iteration[current_task_id]
/// == true`). Returns `None` when the task is absent from the map or its
/// last outcome was not a crash.
///
/// When `resolved_model` is `None`, assumes `SONNET_MODEL` baseline
/// and escalates to `OPUS_MODEL` (architect decision: None crash → opus).
///
/// Escalation is independent of `CrashTracker` backoff logic.
pub fn check_crash_escalation(
    crashed_last_iteration: &std::collections::HashMap<String, bool>,
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
    // None / empty / whitespace model: assume sonnet baseline, escalate to opus
    match normalize_baseline(resolved_model) {
        None => Some(model::OPUS_MODEL.to_string()),
        Some(m) => model::escalate_model(Some(m)),
    }
}

/// Operator escape valve: detect when an operator edited `tasks.model` in the
/// DB out-of-band and clear any stale auto-recovery overrides for that task.
///
/// Called at the top of every iteration (both wave and sequential) BEFORE
/// `resolve_effective_runner`. Short-circuits immediately when `task_id` has no
/// entry in `ctx.overflow_original_task_model` — the dominant case (most tasks
/// never trigger the overflow ladder) is free.
///
/// When the current DB value differs from the snapshot, all six per-task
/// override entries are cleared (in the same order the code removes them):
/// 1. `runner_overrides`
/// 2. `model_overrides`
/// 3. `effort_overrides`
/// 4. `overflow_recovered`
/// 5. `overflow_original_model`
/// 6. `overflow_original_task_model`
///
/// A single stderr line is emitted so operators can see the escape valve fired.
/// DB read errors are logged and treated as no-op so a transient failure never
/// blocks the iteration.
pub fn check_override_invalidation(ctx: &mut IterationContext, conn: &Connection, task_id: &str) {
    // No snapshot → no override was ever set for this task; skip DB round-trip.
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
            eprintln!("Warning: check_override_invalidation({task_id}): DB read failed: {e}");
            return;
        }
    };

    let snapshotted = ctx.overflow_original_task_model.get(task_id);
    if snapshotted.map(Option::as_deref) == Some(current_model.as_deref()) {
        return;
    }

    // Operator changed tasks.model — clear all six per-task override channels.
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

/// Treat `Some("")` and `Some("   ")` as "no model known" so both escalation
/// paths (`check_crash_escalation` and `escalate_task_model_if_needed`) share
/// the same baseline-fallback semantics.
fn normalize_baseline(model: Option<&str>) -> Option<&str> {
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

/// W5: deferred Grok promotion bundle. Carries everything needed to mutate
/// `IterationContext` after a DB write commits. Used by
/// `escalate_task_model_if_needed_inner` to decouple the DB step from the
/// ctx step so transactional callers (`handle_task_failure`) can hold the
/// ctx mutations until `tx.commit()` returns Ok — preventing a one-iteration
/// dirty-ctx-vs-rolled-back-DB window when commit fails.
pub(crate) struct PendingPromotion {
    task_id: String,
    pre_promotion_model: Option<String>,
    grok_model: String,
    new_count: i32,
}

/// Apply a deferred promotion to the `IterationContext`. Idempotent w.r.t.
/// `overflow_original_task_model` (`or_insert_with` preserves the first
/// snapshot). Emits the one-line stderr banner exactly once per promotion
/// (gated on whether `runner_overrides` already held an entry — see M2 in
/// the FEAT-007 commit).
pub(crate) fn apply_pending_promotion(ctx: &mut IterationContext, p: &PendingPromotion) {
    ctx.overflow_original_task_model
        .entry(p.task_id.clone())
        .or_insert_with(|| p.pre_promotion_model.clone());
    let already_promoted = ctx.runner_overrides.contains_key(&p.task_id);
    // kind-correct: writes the promoted provider identity into the override map — the VALUE is the provider, not a capability flag
    ctx.runner_overrides
        .insert(p.task_id.clone(), RunnerKind::Grok);
    ctx.model_overrides
        .insert(p.task_id.clone(), p.grok_model.clone());
    if !already_promoted {
        eprintln!(
            "Promoted task {} to Grok runner (model={}) after {} consecutive failures at Opus",
            p.task_id, p.grok_model, p.new_count
        );
    }
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
pub(crate) fn escalate_task_model_if_needed_inner(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    ctx: &IterationContext,
    cfg: Option<&project_config::FallbackRunnerConfig>,
) -> TaskMgrResult<(Option<String>, Option<PendingPromotion>)> {
    if !should_escalate_for_consecutive_failures(new_count) {
        return Ok((None, None));
    }
    let current_model: Option<String> =
        conn.query_row("SELECT model FROM tasks WHERE id = ?", [task_id], |r| {
            r.get::<_, Option<String>>(0)
        })?;
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
        eprintln!(
            "Escalated task {} to model {} after {} consecutive failures",
            task_id, new_model, new_count
        );
    }

    // FEAT-007: Grok fallback promotion. Fires only when the task was
    // ALREADY at Opus before this call (i.e. the Claude escalation step was
    // a no-op self-loop or no escalation was needed) — Sonnet-tier
    // escalations get a fresh chance at Opus before any Grok pivot is
    // considered. The effective runner is computed against the
    // pre-escalation model so a task already on Grok skips this branch via
    // Provider::Grok (idempotency).
    let fallback = match cfg {
        Some(c) if c.enabled => c,
        _ => return Ok((escalated, None)),
    };
    // H2: use ModelTier-based inclusive check so both OPUS_MODEL and OPUS_MODEL_1M
    // qualify as "at Opus" — string-eq on OPUS_MODEL excluded the 1M variant.
    let was_at_opus = matches!(
        model::model_tier(current_model.as_deref()),
        model::ModelTier::Opus
    );
    // M1: compare in u32 space; new_count is a DB counter (always >= 0 in practice)
    // but guard the negative case to keep the cast sound for all inputs.
    if !was_at_opus || new_count < 0 || (new_count as u32) < fallback.runtime_error_threshold {
        return Ok((escalated, None));
    }
    let effective_runner = resolve_effective_runner(ctx, task_id, current_model.as_deref());
    // kind-correct: fires from Claude only because Grok is the fallback target — provider identity, not capability
    if effective_runner != RunnerKind::Claude {
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
        grok_model: fallback.model.clone(),
        new_count,
    };
    Ok((Some(fallback.model.clone()), Some(promotion)))
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
/// When `cfg` is `None` or `!enabled`, behavior is byte-identical to the
/// pre-FEAT-007 ladder (Sonnet → Opus → terminal). The `ctx` argument is
/// otherwise unused in that path.
///
/// Returns `Some(new_model)` if escalation OR promotion fired, `None` if
/// below threshold, the model tier is unknown (e.g. already at Grok), or
/// the Opus self-loop produced no change AND promotion conditions weren't met.
/// The DB is updated in-place when `Some` is returned.
///
/// This is the convenience variant — DB and ctx writes happen back-to-back.
/// Transactional callers should prefer `escalate_task_model_if_needed_inner`
/// + `apply_pending_promotion` (see W5).
pub fn escalate_task_model_if_needed(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
    ctx: &mut IterationContext,
    cfg: Option<&project_config::FallbackRunnerConfig>,
) -> TaskMgrResult<Option<String>> {
    let (model, pending) = escalate_task_model_if_needed_inner(conn, task_id, new_count, ctx, cfg)?;
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
#[deprecated(note = "use TaskLifecycle::auto_block_after_failures")]
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
pub fn handle_task_failure(
    conn: &mut Connection,
    task_id: &str,
    current_iteration: i64,
    ctx: &mut IterationContext,
    cfg: Option<&project_config::FallbackRunnerConfig>,
) -> TaskMgrResult<()> {
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
            eprintln!(
                "Warning: failed to increment consecutive_failures for {}: {}",
                task_id, e
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
            match escalate_task_model_if_needed_inner(&tx, task_id, new_count, ctx, cfg) {
                Ok((_model, promotion)) => {
                    pending_promotion = promotion;
                }
                Err(e) => {
                    eprintln!("Warning: failed to escalate model for {}: {}", task_id, e);
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
    // lifecycle service via the deprecated shim).
    if should_auto_block(new_count, max_retries) {
        #[allow(deprecated)]
        let res = auto_block_task(conn, task_id, new_count, current_iteration);
        if let Err(e) = res {
            eprintln!("Warning: failed to auto-block task {}: {}", task_id, e);
        } else {
            eprintln!(
                "Auto-blocked task {} after {} consecutive failures",
                task_id, new_count
            );
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
    eprintln!(
        "FATAL: Prompt critical sections ({} bytes) exceed budget ({} bytes) for task {}. \
         Reduce base prompt.md size or split the task.",
        critical_size, budget, task_id,
    );
    IterationResult {
        outcome: IterationOutcome::PromptOverflow,
        task_id: Some(task_id),
        files_modified: vec![],
        should_stop: true,
        output: String::new(),
        effective_model: None,
        effective_effort: None,
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
            eprintln!("  Probe failed to spawn: {}", e);
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
    use super::*;
    use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
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

    // --- check_crash_escalation tests ---

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
        let result = check_crash_escalation(&crash_map(&[]), "FEAT-001", Some(SONNET_MODEL));
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
        let result = check_crash_escalation(&crash_map(&[]), "FEAT-001", Some(SONNET_MODEL));
        assert_eq!(
            result, None,
            "first iteration crash has no previous task context, cannot escalate"
        );
    }

    /// Same task but no crash — no escalation.
    #[test]
    fn test_crash_escalation_same_task_no_crash() {
        let result = check_crash_escalation(
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
        let result = check_crash_escalation(
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
        let result = check_crash_escalation(
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
        let result = check_crash_escalation(
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
        let result = check_crash_escalation(
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
        let result = check_crash_escalation(&crash_map(&[("FEAT-001", true)]), "FEAT-001", None);
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
                check_crash_escalation(&crash_map(&[("FEAT-001", true)]), "FEAT-001", Some(bad));
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
        let no_crash = check_crash_escalation(
            &crash_map(&[("FEAT-001", false)]),
            "FEAT-001",
            Some(SONNET_MODEL),
        );
        assert_eq!(no_crash, None, "same task without crash must NOT escalate");

        // Only crash (different task) — must NOT escalate
        let diff_task = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-002",
            Some(SONNET_MODEL),
        );
        assert_eq!(diff_task, None, "crash on different task must NOT escalate");

        // BOTH conditions — MUST escalate
        let both = check_crash_escalation(
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
        let first = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        assert_eq!(first, Some(SONNET_MODEL.to_string()));

        // After success the pipeline writes false into the map.
        let after_success = check_crash_escalation(
            &crash_map(&[("FEAT-001", false)]),
            "FEAT-001",
            first.as_deref(),
        );
        assert_eq!(
            after_success, None,
            "After success, no crash escalation should occur"
        );

        // Crash again on same task with original base model.
        let second_crash = check_crash_escalation(
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
            check_crash_escalation(&crash_map(&[("TASK-A", true)]), "TASK-A", Some(HAIKU_MODEL));
        assert_eq!(crash_a, Some(SONNET_MODEL.to_string()));

        // Task B is selected next. TASK-A crashed but TASK-B is absent from map.
        let crash_b =
            check_crash_escalation(&crash_map(&[("TASK-A", true)]), "TASK-B", Some(HAIKU_MODEL));
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
        let result1 = check_crash_escalation(
            &crash_map(&[("FEAT-001", true)]),
            "FEAT-001",
            Some(HAIKU_MODEL),
        );
        let result2 = check_crash_escalation(
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
        let first = check_crash_escalation(&crashed, "FEAT-001", Some(HAIKU_MODEL));
        assert_eq!(
            first,
            Some(SONNET_MODEL.to_string()),
            "first crash: haiku → sonnet"
        );

        // Second crash: feed escalated model back in (sonnet → opus)
        let second = check_crash_escalation(&crashed, "FEAT-001", first.as_deref());
        assert_eq!(
            second,
            Some(OPUS_MODEL.to_string()),
            "second crash: sonnet → opus"
        );

        // Third crash: opus → opus (ceiling)
        let third = check_crash_escalation(&crashed, "FEAT-001", second.as_deref());
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

        #[allow(deprecated)]
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
        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx, None).unwrap();
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
        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx, None).unwrap();
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
        let result = escalate_task_model_if_needed(&conn, "T-001", 2, &mut ctx, None).unwrap();
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
        let result = escalate_task_model_if_needed(&conn, "T-001", 1, &mut ctx, None).unwrap();
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
}
