//! Post-output overflow recovery (CONTRACT-001 / FEAT-005).
//!
//! - [`handle_overflow`] — the "Prompt is too long" five-rung recovery ladder.
//!   FEAT-005 physically relocated the body here from
//!   `overflow::handle_prompt_too_long` (which is now a `#[deprecated]` shim
//!   delegating to this coordinator). Both `iteration.rs` and `slot.rs` (via
//!   `process_slot_result`) route through it, and the three engine files carry
//!   `#![deny(deprecated)]` so a direct call to the old leaf is a compile error.
//!
//! The diagnostics primitives the ladder writes (`dump_prompt`,
//! `append_event_log`, `rotate_dumps_keep_n`, `sanitize_id_for_filename`) and
//! the wire types (`RecoveryAction`, `OverflowEvent`, `DumpHeader`) stay in
//! [`crate::loop_engine::overflow`] — they are the path-traversal/serialization
//! primitives, exercised by that module's own unit tests as the equivalence
//! oracle for this relocation.
//!
//! **Ordering relative to the shared pipeline**: `handle_overflow` fires on the
//! `PromptTooLong` crash outcome BEFORE
//! [`crate::loop_engine::iteration_pipeline::process_iteration_output`] runs for
//! that iteration/slot — in both paths. The recovery state (the `todo`/`blocked`
//! DB reset and the ctx overrides) must be durable before the pipeline's
//! crash-tracking write observes the outcome. See
//! `src/loop_engine/CLAUDE.md` → "Overflow recovery and diagnostics".
//!
//! The post-output **rate-limit** reaction (`react_to_outputs`) was relocated
//! to [`super::account`] by FEAT-006 — it is account-global (it reflects the
//! shared API account state, not per-task state), so it lives alongside
//! `account_usage_gate` rather than here.

use std::path::Path;

use rusqlite::Connection;

use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::engine::IterationContext;
use crate::loop_engine::model;
use crate::loop_engine::overflow::{
    DumpHeader, OverflowEvent, RecoveryAction, append_event_log, dump_prompt, rotate_dumps_keep_n,
    sanitize_id_for_filename,
};
use crate::loop_engine::project_config::ProjectConfig;
use crate::loop_engine::prompt::PromptResult;
use crate::loop_engine::runner::RunnerKind;

/// Select the rung-4 fallback target for the given effective runner, or
/// `None` when no cross-provider promotion is configured for that direction.
///
/// Two mirror directions (PRD §2.5, FEAT-006 + FEAT-PRIMARY-004):
///   - **Claude → Grok**: fires when `fallback_runner` is `Some(cfg)` with
///     `cfg.enabled = true`. Target = `cfg.model`, provider = `cfg.provider`.
///   - **Grok → Claude**: fires when `primary_runner` is `Some(pr)` with
///     `pr.claude_fallback_model = Some(model)`. Target = that model,
///     provider = `"claude"`.
///
/// Returns `(provider, model, target_runner)` so the caller writes a single
/// `RecoveryAction::FallbackToProvider` plus matching `runner_overrides` /
/// `model_overrides` entries without branching on direction itself. The
/// idempotency guard (a task already carrying a promotion override skips
/// rung 4 entirely) is enforced by the caller, not here.
fn select_fallback_target(
    effective_runner: RunnerKind,
    project_config: &ProjectConfig,
) -> Option<(String, String, RunnerKind)> {
    match effective_runner {
        RunnerKind::Claude => {
            let cfg = project_config.fallback_runner.as_ref()?;
            if !cfg.enabled {
                return None;
            }
            Some((cfg.provider.clone(), cfg.model.clone(), RunnerKind::Grok))
        }
        RunnerKind::Grok => {
            let primary = project_config.primary_runner.as_ref()?;
            let claude_model = primary.claude_fallback_model.as_ref()?;
            Some((
                "claude".to_string(),
                claude_model.clone(),
                RunnerKind::Claude,
            ))
        }
    }
}

/// Read the current `tasks.model` column for a task, returning `Ok(None)` when
/// the column is NULL and `Err` only on a connectivity / schema failure.
/// Used by [`handle_overflow`] to capture the pre-fallback model into
/// `ctx.overflow_original_task_model` before the rung-4 UPDATE mutates the
/// column.
fn read_task_model_from_db(conn: &Connection, task_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT model FROM tasks WHERE id = ?1", [task_id], |r| {
        r.get::<_, Option<String>>(0)
    })
}

/// Inputs to [`handle_overflow`]. Destructured exhaustively (no `..`). Mirrors
/// the twelve arguments of the (now deprecated) `overflow::handle_prompt_too_long`
/// shim; `slot_index` is `Some(n)` for a wave slot and `None` for the
/// sequential path.
pub struct HandleOverflowParams<'a> {
    pub ctx: &'a mut IterationContext,
    pub conn: &'a mut Connection,
    pub task_id: &'a str,
    pub effort: Option<&'a str>,
    pub effective_model: Option<&'a str>,
    pub prompt_result: &'a PromptResult,
    pub iteration: u32,
    pub run_id: Option<&'a str>,
    pub base_dir: &'a Path,
    pub slot_index: Option<usize>,
    pub effective_runner: RunnerKind,
    pub project_config: &'a ProjectConfig,
}

/// Overflow recovery coordinator: the single home both execution paths call
/// when a task hits "Prompt is too long". Sequential passes `slot_index: None`
/// and folds the one result; wave passes `slot_index: Some(n)` per slot.
///
/// Handles a `PromptTooLong` outcome end-to-end: pick a recovery rung, mutate
/// `IterationContext`, update the task row, emit the stderr message, and write
/// the diagnostics bundle (dump + JSONL + rotation).
///
/// Returns the chosen [`RecoveryAction`] so callers can keep flowing the
/// classification (e.g. for outcome telemetry); the side effects are the
/// primary contract.
///
/// **Order of operations** (must not be reordered — the recovery state
/// must be durable before any best-effort observability runs):
/// 1. Pick recovery rung (1-effort downgrade → 2-model escalate → 3-1M model
///    → 4-fallback-to-provider → 5-blocked).
/// 2. Update `ctx.overflow_recovered`, `ctx.overflow_original_model`
///    (first-overflow only), and `ctx.overflow_original_task_model`
///    (first-fallback snapshot of the `tasks.model` DB column).
/// 3. UPDATE the task row (status='todo' on rungs 1-3, 'todo' AND set
///    `tasks.model = cfg.model` on rung 4, 'blocked' on rung 5).
/// 4. Emit the rung-specific stderr message.
/// 5. Best-effort: write prompt dump.
/// 6. Best-effort: append JSONL event line.
/// 7. Best-effort: rotate dumps (keep newest 3 per task).
///
/// Filesystem failures in steps 5-7 are logged via `eprintln!` and never
/// propagate — observability is best-effort, recovery is not.
///
/// `effective_runner` is the single computed value from
/// [`crate::loop_engine::engine::resolve_effective_runner`] at the spawn
/// site — the rung-4 idempotency guard pins on this value (PRD §2.5
/// "single-predicate guard" — never re-derive via
/// `runner_overrides.get(task)` OR `provider_for_model(model)`).
pub fn handle_overflow(params: HandleOverflowParams<'_>) -> RecoveryAction {
    let HandleOverflowParams {
        ctx,
        conn,
        task_id,
        effort,
        effective_model,
        prompt_result,
        iteration,
        run_id,
        base_dir,
        slot_index,
        effective_runner,
        project_config,
    } = params;

    // M2: snapshot whether this task already has a Grok promotion recorded
    // BEFORE the rung-4 arm can insert one. Used below to suppress a duplicate
    // banner when the RuntimeError hook fires in the same wave for the same task.
    let was_already_promoted = ctx.runner_overrides.contains_key(task_id);

    // Step 1: pick recovery rung. Rung 4 (FallbackToProvider) sits between
    // rung 3 (to_1m_model) and rung 5 (Blocked); its precondition is a
    // SINGLE-predicate guard (PRD §2.5): the computed `effective_runner`
    // value MUST be `RunnerKind::Claude` AND the fallback config MUST be
    // enabled. Re-deriving the guard via
    // `runner_overrides.get(...).is_none() || provider_for_model(...) == Claude`
    // is explicitly prohibited because it can silently drift between the
    // spawn-site value and the rung-4 check.
    let action = if let Some(next_effort) = model::downgrade_effort(effort) {
        ctx.effort_overrides
            .insert(task_id.to_string(), next_effort);
        RecoveryAction::DowngradeEffort {
            new_effort: next_effort.to_string(),
        }
    } else if let Some(next_model) = model::escalate_below_opus(effective_model) {
        ctx.model_overrides
            .insert(task_id.to_string(), next_model.to_string());
        RecoveryAction::EscalateModel {
            new_model: next_model.to_string(),
        }
    } else if let Some(m1m) = model::to_1m_model(effective_model) {
        ctx.model_overrides
            .insert(task_id.to_string(), m1m.to_string());
        RecoveryAction::To1mModel {
            new_model: m1m.to_string(),
        }
    // kind-correct: rung 4 gate — a task NOT yet promoted is eligible for a
    // cross-provider pivot whose direction follows `effective_runner`
    // (Claude→Grok via fallback_runner, Grok→Claude via primary_runner). The
    // single `!was_already_promoted` predicate is the idempotency guard: a
    // task already carrying a promotion override (in EITHER direction) skips
    // rung 4 and falls through to rung 5 (Blocked), so it never bounces back
    // to the runner it came from.
    } else if !was_already_promoted
        && let Some((provider, model, target_runner)) =
            select_fallback_target(effective_runner, project_config)
    {
        // kind-correct: writes the promoted provider identity into the override map — the VALUE is the provider, not a capability flag
        ctx.runner_overrides
            .insert(task_id.to_string(), target_runner);
        ctx.model_overrides
            .insert(task_id.to_string(), model.clone());
        RecoveryAction::FallbackToProvider { provider, model }
    } else {
        RecoveryAction::Blocked
    };

    // Step 2: capture overflow markers — first-overflow capture for
    // `overflow_original_model` (entry().or_insert_with), unconditional
    // insert for the recovered set. Also capture the pre-fallback
    // `tasks.model` DB column into `overflow_original_task_model` BEFORE the
    // rung-4 DB UPDATE mutates it; the snapshot is used by FEAT-008's
    // `check_override_invalidation` to detect operator edits and drop stale
    // overrides. Captured for every rung (entry().or_insert_with is
    // idempotent) so the snapshot remains stable across repeated overflows
    // on the same task.
    ctx.overflow_recovered.insert(task_id.to_string());
    ctx.overflow_original_model
        .entry(task_id.to_string())
        .or_insert_with(|| effective_model.unwrap_or("(default)").to_string());
    ctx.overflow_original_task_model
        .entry(task_id.to_string())
        .or_insert_with(|| {
            match read_task_model_from_db(conn, task_id) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "Warning: read_task_model_from_db({task_id}) for overflow snapshot: DB read failed: {e}"
                    );
                    None
                }
            }
        });

    // Step 3: update DB.
    //   - Blocked            → status='blocked' (started_at preserved for audit)
    //   - FallbackToProvider → status='todo' + clear started_at + set tasks.model
    //                          to the cross-provider target (Grok model for
    //                          Claude→Grok, Claude model for Grok→Claude) so
    //                          `resolve_task_model` picks it up next iteration.
    //                          The DB UPDATE and the rung-1 ctx override inserts
    //                          (above) run together — never split across a
    //                          deferred-commit boundary — so the in-memory
    //                          overrides and the persisted `tasks.model` can
    //                          never disagree on the next iteration.
    //   - Rungs 1-3          → status='todo' + clear started_at (model unchanged)
    match action {
        RecoveryAction::Blocked => {
            let _ = TaskLifecycle::new(conn).auto_block_after_failures(
                task_id,
                "prompt too long",
                i64::from(iteration),
            );
        }
        RecoveryAction::FallbackToProvider { ref model, .. } => {
            let _ = TaskLifecycle::new(conn).resurrect_with_model_override(task_id, model);
        }
        _ => {
            let _ = TaskLifecycle::new(conn).resurrect_for_iteration(None, &[task_id]);
        }
    }

    // Step 4: rung-specific stderr message. For FallbackToProvider, suppress
    // the banner when this task was already promoted (was_already_promoted ==
    // true) so a wave-mode task that triggers BOTH the overflow rung-4 path
    // and the RuntimeError hook in the same wave emits exactly one banner.
    if !matches!(action, RecoveryAction::FallbackToProvider { .. }) || !was_already_promoted {
        eprintln!("{}", action.user_message(task_id, effort, effective_model));
    }

    // Step 5: best-effort prompt dump.
    let dumps_dir = base_dir.join("overflow-dumps");
    let ts_iso8601 = chrono::Utc::now().to_rfc3339();
    let header = DumpHeader {
        iteration,
        model: effective_model.map(String::from),
        effort: effort.map(String::from),
        ts_iso8601: ts_iso8601.clone(),
        total_bytes: prompt_result.prompt.len(),
        sections: prompt_result.section_sizes.as_slice(),
        dropped_sections: prompt_result.dropped_sections.as_slice(),
    };
    let dump_path = match dump_prompt(&dumps_dir, task_id, &header, &prompt_result.prompt) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("warning: overflow dump write failed: {}", e);
            // Synthetic placeholder path so JSONL still records *something*.
            dumps_dir.join(format!(
                "{}-iter{}-FAILED.txt",
                sanitize_id_for_filename(task_id),
                iteration,
            ))
        }
    };

    // Step 6: best-effort JSONL append.
    let event = OverflowEvent {
        ts: ts_iso8601,
        task_id: task_id.to_string(),
        run_id: run_id.map(String::from),
        iteration,
        slot_index,
        model: effective_model.map(String::from),
        effort: effort.map(String::from),
        task_difficulty: prompt_result.task_difficulty.clone(),
        prompt_bytes: prompt_result.prompt.len(),
        sections: prompt_result
            .section_sizes
            .iter()
            .map(|(n, s)| ((*n).to_string(), *s))
            .collect(),
        dropped_sections: prompt_result.dropped_sections.clone(),
        recovery: action.clone(),
        dump_path: dump_path.to_string_lossy().into_owned(),
        // kind-correct: stringifies provider identity for JSONL diagnostic output — pure serialization
        runner: Some(
            match effective_runner {
                RunnerKind::Claude => "claude",
                RunnerKind::Grok => "grok",
            }
            .to_string(),
        ),
    };
    if let Err(e) = append_event_log(base_dir, &event) {
        eprintln!("warning: overflow event log append failed: {}", e);
    }

    // Step 7: best-effort dump rotation (keep newest 3 per task).
    let sanitized = sanitize_id_for_filename(task_id);
    if let Err(e) = rotate_dumps_keep_n(&dumps_dir, &sanitized, 3) {
        eprintln!("warning: overflow dump rotation failed: {}", e);
    }

    action
}
