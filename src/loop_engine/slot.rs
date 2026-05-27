//! Per-slot lifecycle and result processing for parallel-wave execution.
//!
//! Carved out of `engine.rs` (PRD 02, FEAT-001). This module owns the leaf
//! concerns of a single parallel slot: claiming its task on the main thread
//! (`claim_slot_task`), running its iteration on a worker thread
//! (`run_slot_iteration`), the early-exit / failure `SlotResult` constructors
//! (`slot_early_exit`, `slot_failure_result`), and the main-thread per-slot
//! post-processing (`process_slot_result`).
//!
//! The data types these functions operate on (`SlotContext`, `SlotResult`,
//! `SlotEarlyExit`, `SlotIterationParams`, `WaveAggregator`, …) remain in
//! `engine.rs` and are imported here — they are consumed by `IterationContext`
//! and by tests outside the `loop_engine` module, so moving them would widen
//! the carve's blast radius.
//!
//! **Post-spawn processing**: `process_slot_result` delegates to
//! `iteration_pipeline::process_iteration_output` (the shared post-Claude
//! pipeline) so wave-mode picks up learning extraction, bandit feedback,
//! `<task-status>` dispatch, and the completion fallback identically to the
//! sequential path.
//!
//! **Per-task recovery is NOT owned here**: auto-block, crash escalation, and
//! model/provider promotion live in `recovery.rs` and are invoked by
//! `wave_scheduler.rs` after it collects the `SlotResult` from this module.
//!
//! **Reaction single-home lock (CONTRACT-001)**: `#![deny(deprecated)]` makes a
//! direct call to any relocated reaction leaf (marked `#[deprecated]`) a compile
//! error here. The per-slot overflow reaction routes through
//! `crate::loop_engine::reactions::post_output::handle_overflow`. Pre-existing
//! `#[allow(deprecated)]` shims (e.g. the `claim_slot_task` lifecycle shim) keep
//! working — an inner `#[allow]` is more specific than this module-level deny.
#![deny(deprecated)]

use std::sync::Arc;

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::claude;
use crate::loop_engine::config::{self, IterationOutcome, TASKS_JSON_DISALLOWED_TOOLS};
use crate::loop_engine::detection;
use crate::loop_engine::display;
use crate::loop_engine::engine::{
    IterationContext, IterationResult, SlotContext, SlotEarlyExit, SlotIterationParams, SlotResult,
    WaveAggregator, WaveIterationParams, resolve_effective_runner,
};
use crate::loop_engine::iteration_pipeline;
use crate::loop_engine::model;
use crate::loop_engine::monitor;
use crate::loop_engine::reactions;
use crate::loop_engine::runner::{self, RunnerKind};
use crate::loop_engine::watchdog;
use crate::models::TaskStatus;

/// Build a slot-scoped early-exit `SlotResult`.
///
/// Centralizes the `IterationResult` construction for the handful of early
/// returns in `run_slot_iteration` so the 10-field struct literal isn't
/// duplicated. Pulls task identity AND `shown_learning_ids` from the slot's
/// bundle so the orphan-reset accounting AND bandit feedback stay correct
/// even on early-exit paths (signal received, timeout, etc.).
fn slot_early_exit(slot: &SlotContext, exit: SlotEarlyExit) -> SlotResult {
    SlotResult {
        slot_index: slot.slot_index,
        iteration_result: IterationResult {
            outcome: exit.outcome,
            task_id: Some(slot.prompt_bundle.task_id.clone()),
            files_modified: exit.files_modified,
            should_stop: exit.should_stop,
            output: exit.output,
            effective_model: exit.effective_model,
            effective_effort: exit.effective_effort,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        },
        // Early exit always runs after a successful claim (the slot thread
        // started); the orphan reset must consider this task pending until
        // process_slot_result clears it.
        claim_succeeded: true,
        shown_learning_ids: slot.prompt_bundle.shown_learning_ids.clone(),
        prompt_for_overflow: None,
        section_sizes: slot.prompt_bundle.section_sizes.clone(),
        dropped_sections: slot.prompt_bundle.dropped_sections.clone(),
        task_difficulty: slot.prompt_bundle.difficulty.clone(),
        effective_runner: slot.effective_runner,
    }
}

/// Run one slot's iteration: spawn Claude in the slot worktree using the
/// pre-assembled prompt bundle, then analyze output. Opens its own DB
/// connection and creates its own watchdog activity epoch so nothing is
/// shared across slot threads.
///
/// This is a deliberately simpler counterpart to `run_iteration`:
/// - No `&mut IterationContext` — slot threads must not touch shared state.
/// - No crash-escalation, reorder, stale, or rate-limit-wait logic — those
///   are orchestrated by the outer loop after the wave returns.
/// - No prompt-overflow recovery path — wave-mode overflow handling is a
///   per-slot follow-up tracked elsewhere in the PRD.
///
/// The prompt bundle MUST have been built on the main thread before the
/// worker was spawned (FEAT-002 contract). The worker only reads from the
/// bundle — it never reaches back into a `&Connection` to re-derive task
/// state, because rusqlite `Connection` is not `Send` (learnings #1893 /
/// #1852 / #1871).
pub fn run_slot_iteration(
    slot: &SlotContext,
    params: &SlotIterationParams,
) -> TaskMgrResult<SlotResult> {
    let bundle = &slot.prompt_bundle;
    let task_id = bundle.task_id.as_str();
    let task_files = bundle.task_files.clone();

    // Early exit on signal — no work should start if the loop is stopping.
    if params.signal_flag.is_signaled() {
        return Ok(slot_early_exit(
            slot,
            SlotEarlyExit {
                outcome: IterationOutcome::Empty,
                files_modified: vec![],
                should_stop: true,
                output: String::new(),
                effective_model: None,
                effective_effort: None,
            },
        ));
    }

    // Effective model: bundle-resolved (per-task) > params.default_model.
    // Cluster-wide escalation (sequential path) is intentionally not applied
    // in parallel; the wave engine targets tasks already scored as disjoint,
    // not clusters.
    let effective_model: Option<String> = bundle
        .resolved_model
        .clone()
        .or_else(|| params.default_model.clone());

    let effort = model::effort_for_difficulty(bundle.difficulty.as_deref());

    if params.verbose {
        eprintln!(
            "[slot {}] task={} model={} effort={} cwd={}",
            slot.slot_index,
            task_id,
            effective_model.as_deref().unwrap_or("(default)"),
            effort.unwrap_or("(default)"),
            slot.working_root.display(),
        );
    }

    // Prefix every line of this slot's tee output with its slot index so
    // concurrent slots stay attributable on a shared stderr.
    let slot_label_buf = format!("[slot {}]", slot.slot_index);

    // Iteration banner — same shape as the sequential `print_iteration_header`
    // but routed through `emit_prefixed_lines` so each line carries the slot
    // label. `format_iteration_header` emits a leading newline for visual
    // separation; we strip it here because a blank line would land as
    // `[slot N] ` in the output, which adds noise without adding signal.
    let banner = display::format_iteration_header(
        params.iteration,
        params.max_iterations,
        task_id,
        params.elapsed_secs,
        effective_model.as_deref(),
        effort,
    );
    claude::emit_prefixed_lines(Some(&slot_label_buf), banner.trim_start_matches('\n'));

    // Per-slot activity monitor + timeout. Each slot gets its own monitor
    // polling its own working_root so heartbeats/change-tracking lines and
    // activity-driven deadline extensions both function in wave mode the
    // same way they do for the sequential path. `prefix` attributes monitor
    // output to the originating slot when slots interleave on stderr.
    let monitor_handle = monitor::start_monitor(&slot.working_root, Some(&slot_label_buf));
    let timeout_config = watchdog::TimeoutConfig::from_difficulty(
        bundle.difficulty.as_deref(),
        Arc::clone(&monitor_handle.last_activity_epoch),
    );

    // FEAT-005: dispatch via the precomputed effective_runner (resolved on
    // the main thread in `run_wave_iteration` before the slot was spawned).
    // The slot body MUST NOT read the IterationContext override maps
    // directly (Learning #1810; enforced by the source-sniff test in
    // tests/runtime_error_fallback.rs).
    let claude_result = runner::dispatch(
        slot.effective_runner,
        &bundle.prompt,
        &params.permission_mode,
        claude::SpawnOpts {
            signal_flag: Some(&params.signal_flag),
            working_dir: Some(&slot.working_root),
            model: effective_model.as_deref(),
            timeout: Some(timeout_config),
            stream_json: true,
            effort,
            disallowed_tools: Some(TASKS_JSON_DISALLOWED_TOOLS),
            db_dir: Some(&params.db_dir),
            use_pty: false,
            target_task_id: Some(task_id),
            slot_label: Some(&slot_label_buf),
            active_prefix: params.task_prefix.as_deref(),
            // Each iteration's ai-title metadata stub otherwise clutters the
            // worktree's interactive resume picker. See claude.rs:119. Only
            // request it from a runner that emits the artifact — dispatch
            // fail-closes on runners (e.g. Grok) that lack the capability.
            // REGRESSION: do NOT hardcode `true` — Grok dispatch is rejected
            // with UnsupportedRunnerCapability. Gate on the selected runner.
            cleanup_title_artifact: slot
                .effective_runner
                .supports(runner::RunnerCapability::TitleArtifactCleanup),
            ..Default::default()
        },
    );
    monitor::stop_monitor(monitor_handle);
    // FEAT-007: route TaskMgrError::GrokAuthFailure into a Crash(GrokAuthFailure)
    // outcome instead of propagating out of the slot. The post-wave aggregator
    // detects this variant and skips both the failure-counter increment AND
    // the Grok promotion hook (cascade prevention).
    let claude_result = match claude_result {
        Ok(r) => r,
        Err(crate::error::TaskMgrError::GrokAuthFailure { hint }) => {
            eprintln!(
                "[slot {}] Grok auth failure for task {}: {}",
                slot.slot_index, task_id, hint
            );
            return Ok(slot_early_exit(
                slot,
                SlotEarlyExit {
                    outcome: IterationOutcome::Crash(config::CrashType::GrokAuthFailure),
                    files_modified: task_files,
                    should_stop: false,
                    output: hint,
                    effective_model,
                    effective_effort: effort,
                },
            ));
        }
        Err(e) => return Err(e),
    };

    if claude_result.timed_out {
        eprintln!(
            "[slot {}] iteration timed out for task {}",
            slot.slot_index, task_id,
        );
        return Ok(slot_early_exit(
            slot,
            SlotEarlyExit {
                outcome: IterationOutcome::Crash(config::CrashType::RuntimeError),
                files_modified: task_files,
                should_stop: false,
                output: claude_result.output,
                effective_model,
                effective_effort: effort,
            },
        ));
    }

    // If Claude was killed by SIGINT/SIGTERM (exit 130/143), propagate to
    // our shared signal flag so peer slots and the outer loop observe the stop.
    //
    // Exception: if the watchdog fired the post-completion grace kill, the
    // SIGTERM (143) was issued internally as a successful-completion finalizer
    // — not an external Ctrl+C. Propagating it would end the whole loop (and
    // any chained PRDs) despite the task completing normally.
    if matches!(claude_result.exit_code, 130 | 143) && !claude_result.completion_killed {
        params.signal_flag.set();
    }

    if params.signal_flag.is_signaled() {
        return Ok(slot_early_exit(
            slot,
            SlotEarlyExit {
                outcome: IterationOutcome::Empty,
                files_modified: task_files,
                should_stop: true,
                output: claude_result.output,
                effective_model: None,
                effective_effort: None,
            },
        ));
    }

    let outcome = detection::analyze_output(
        &claude_result.output,
        claude_result.exit_code,
        &slot.working_root,
    );

    // Thread the structured stream-json transcript through to the pipeline so
    // wave-mode learning extraction reads the same conversation source the
    // sequential path uses (engine.rs:2109). Dropping this here is the
    // pre-FEAT-004 bug that makes the wave path silently fall back to
    // `claude_result.output` (just the final result string).
    let conversation = claude_result.conversation;

    let is_prompt_too_long = matches!(
        outcome,
        config::IterationOutcome::Crash(config::CrashType::PromptTooLong)
    );
    Ok(SlotResult {
        slot_index: slot.slot_index,
        iteration_result: IterationResult {
            outcome,
            task_id: Some(bundle.task_id.clone()),
            files_modified: task_files,
            should_stop: false,
            output: claude_result.output,
            effective_model,
            effective_effort: effort,
            key_decisions_count: 0,
            conversation,
            shown_learning_ids: bundle.shown_learning_ids.clone(),
        },
        claim_succeeded: true,
        shown_learning_ids: bundle.shown_learning_ids.clone(),
        prompt_for_overflow: is_prompt_too_long.then(|| bundle.prompt.clone()),
        section_sizes: bundle.section_sizes.clone(),
        dropped_sections: bundle.dropped_sections.clone(),
        task_difficulty: bundle.difficulty.clone(),
        effective_runner: slot.effective_runner,
    })
}

/// Claim a slot's task on the main thread by updating status to `in_progress`.
///
/// Returns `true` when the claim succeeded (the row was in `todo` or already
/// `in_progress` for this task), `false` when the task was in an unexpected
/// state (e.g. `done`, `blocked`) — the caller should skip spawning that slot.
///
/// Done as a single UPDATE with an optimistic-locking WHERE clause so a
/// concurrent external writer cannot clobber a `done` row back to `in_progress`.
///
/// `'in_progress'` is intentionally included in the WHERE clause: re-claiming
/// an already-claimed task is idempotent (covered by the test
/// `test_claim_slot_task_idempotent_on_already_in_progress`) and supports
/// retry-after-recovery scenarios where step 6.6 left a row in `in_progress`
/// that the loop wants to take over. The selection layer is responsible for
/// not surfacing in-flight rows to other slots in the same wave; this guard
/// only protects against `done`/`blocked` transitions, not duplicate claims.
#[deprecated(
    note = "use TaskLifecycle::try_claim — this shim will be removed in PRD 2 (engine carve)"
)]
pub(super) fn claim_slot_task(conn: &mut Connection, task_id: &str) -> bool {
    match TaskLifecycle::new(conn).try_claim(task_id, &[TaskStatus::Todo, TaskStatus::InProgress]) {
        Ok(claimed) => claimed,
        Err(e) => {
            eprintln!(
                "Warning: failed to claim slot task {}: {} — skipping slot",
                task_id, e,
            );
            false
        }
    }
}

/// Build a SlotResult representing a slot thread that panicked or errored.
///
/// `claim_succeeded` distinguishes two cases:
///   - `true`: claim went through, but the slot thread later panicked / crashed
///     while its task was already `in_progress` — the orphan reset must run.
///   - `false`: claim itself failed (task already `done`/`blocked`) and no row
///     was ever moved to `in_progress` — orphan reset must skip this entry.
///
/// `shown_learning_ids` is left empty on this path: the bundle was either
/// never built (claim failed) or has been moved into the panicked worker
/// thread and is no longer recoverable from the main thread. Bandit feedback
/// for failed runs is intentionally suppressed — crediting/discrediting
/// learnings against a worker that crashed before any agent reasoning would
/// produce noise, not signal.
pub(super) fn slot_failure_result(
    slot_index: usize,
    task_id: Option<String>,
    reason: String,
    claim_succeeded: bool,
) -> SlotResult {
    SlotResult {
        slot_index,
        iteration_result: IterationResult {
            outcome: IterationOutcome::Crash(config::CrashType::RuntimeError),
            task_id,
            files_modified: vec![],
            should_stop: false,
            output: reason,
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        },
        claim_succeeded,
        shown_learning_ids: Vec::new(),
        prompt_for_overflow: None,
        section_sizes: Vec::new(),
        dropped_sections: Vec::new(),
        task_difficulty: None,
        effective_runner: RunnerKind::Claude, // kind-correct: sentinel default; main-thread enrichment overwrites with resolved provider before slot spawn
    }
}

/// Per-slot post-processing on the main thread.
///
/// The post-Claude work shared with the sequential path — progress logging,
/// `<key-decision>` extraction, `<task-status>` dispatch, the full completion
/// ladder (status-tag → completed-tag → output-scan → already-complete
/// fallback), learning extraction, and bandit feedback — runs inside
/// `iteration_pipeline::process_iteration_output`. `skip_git_completion_detection`
/// is `true` because slot commits live on an unmerged ephemeral branch; the
/// post-merge reconcile at the `run_wave_iteration` boundary
/// (`reconcile_merged_slot_completions` over the `{pre_merge_head}..HEAD`
/// range on slot 0) catches `<TASK-ID>-completed` markers from agents whose
/// subprocess exited before flushing the `<completed>` tag.
///
/// Slot-specific bookkeeping stays here: pending-slot-task accounting, the
/// `agg.all_crashed` invariant, reorder hint queueing (with `[slot N]` log
/// prefix), file aggregation, and the wave-level stop flag.
///
/// Updates `agg.all_crashed` only when the slot crashed AND its claimed
/// task did not finish — any non-crash slot (or a crashed slot whose task
/// was nonetheless marked done) breaks the all-crashed invariant.
pub(super) fn process_slot_result(
    slot_result: &mut SlotResult,
    params: &mut WaveIterationParams<'_>,
    ctx: &mut IterationContext,
    agg: &mut WaveAggregator,
) {
    let slot_idx = slot_result.slot_index;
    let task_id = slot_result.iteration_result.task_id.clone();

    // Track every claimed slot task as pending until we observe a "done"
    // signal for it. The post-loop cleanup uses this to reset rows still in
    // `in_progress` when the loop exits via deadline / max-iterations rather
    // than waiting for the next process's step 6.6 recovery.
    if slot_result.claim_succeeded
        && let Some(ref tid) = task_id
        && !ctx.pending_slot_tasks.contains(tid)
    {
        ctx.pending_slot_tasks.push(tid.clone());
    }

    // Synthetic slot_failure_result entries with claim_succeeded=false represent
    // tasks that were never moved to in_progress. Running the overflow handler or
    // the pipeline for them would pollute ctx.crashed_last_iteration past its
    // 'bounded by active task count' invariant (engine.rs:218-221) and emit
    // spurious JSONL overflow events for tasks that never executed.
    if !slot_result.claim_succeeded {
        return;
    }

    // Per-slot PromptTooLong recovery — mirrors the sequential Step 8.5 in
    // `run_iteration`. Must run BEFORE `process_iteration_output` so the
    // task row is reset to `todo` (rungs 1-3) or `blocked` (rung 4) before
    // the pipeline's crash-tracking write. Order of operations is
    // contractual: ctx update → DB UPDATE → stderr → dump → JSONL → rotate.
    if matches!(
        slot_result.iteration_result.outcome,
        config::IterationOutcome::Crash(config::CrashType::PromptTooLong)
    ) && let Some(ref tid) = task_id
    {
        debug_assert!(
            slot_result.prompt_for_overflow.is_some(),
            "PromptTooLong without prompt_for_overflow for task {tid}"
        );
        let synthetic_prompt = crate::loop_engine::prompt::PromptResult {
            prompt: slot_result.prompt_for_overflow.take().unwrap_or_default(),
            task_id: tid.clone(),
            task_files: slot_result.iteration_result.files_modified.clone(),
            shown_learning_ids: Vec::new(),
            resolved_model: slot_result.iteration_result.effective_model.clone(),
            // Wave-mode prompts now apply the same TOTAL_PROMPT_BUDGET cap as
            // the sequential builder; surface any dropped sections threaded
            // through from `SlotPromptBundle` so overflow dumps and JSONL
            // events match what the agent actually saw.
            dropped_sections: slot_result.dropped_sections.clone(),
            task_difficulty: slot_result.task_difficulty.clone(),
            cluster_effort: slot_result.iteration_result.effective_effort,
            section_sizes: slot_result.section_sizes.clone(),
        };
        // FEAT-005/H3: use the pre-dispatch runner threaded from SlotContext
        // so the rung-4 idempotency guard pins on the same value the slot used.
        // The assertion cross-checks that re-derivation from the result's
        // effective_model agrees (catches drift if runner_overrides logic
        // changes). W4: kept as a real `assert_eq!` (not `debug_assert_eq!`)
        // so drift is caught in release builds too — the cost is one HashMap
        // lookup vs. a silent dispatch mismatch that would route the wrong
        // model id to the wrong runner binary.
        let effective_runner = slot_result.effective_runner;
        assert_eq!(
            resolve_effective_runner(
                ctx,
                tid,
                slot_result.iteration_result.effective_model.as_deref()
            ),
            effective_runner,
            "effective_runner drift: process_slot_result re-derivation diverged from pre-dispatch value"
        );
        // CONTRACT-001: route the per-slot overflow reaction through the shared
        // coordinator (`slot_index: Some(slot_idx)`); the direct leaf call is
        // denied here by `#![deny(deprecated)]`.
        let _ =
            reactions::post_output::handle_overflow(reactions::post_output::HandleOverflowParams {
                ctx,
                conn: params.conn,
                task_id: tid,
                effort: slot_result.iteration_result.effective_effort,
                effective_model: slot_result.iteration_result.effective_model.as_deref(),
                prompt_result: &synthetic_prompt,
                iteration: params.iteration,
                run_id: Some(params.run_id),
                base_dir: params.db_dir,
                slot_index: Some(slot_idx),
                effective_runner,
                project_config: params.project_config,
            });
    }

    // Pipeline contract requires a `working_root` even when skip_git is on
    // (the field is unused in that mode but still part of the struct). Use
    // the slot's pre-allocated worktree path so a future change that begins
    // honoring git history wouldn't silently fall back to the source tree.
    let working_root = params
        .slot_worktree_paths
        .get(slot_idx)
        .cloned()
        .unwrap_or_else(|| params.source_root.to_path_buf());

    let processing_outcome =
        iteration_pipeline::process_iteration_output(iteration_pipeline::ProcessingParams {
            conn: params.conn,
            run_id: params.run_id,
            iteration: params.iteration,
            task_id: task_id.as_deref(),
            output: &slot_result.iteration_result.output,
            conversation: slot_result.iteration_result.conversation.as_deref(),
            shown_learning_ids: &slot_result.shown_learning_ids,
            outcome: &mut slot_result.iteration_result.outcome,
            working_root: &working_root,
            git_scan_depth: 0,
            skip_git_completion_detection: true,
            prd_path: params.prd_path,
            task_prefix: params.task_prefix,
            progress_path: params.progress_path,
            db_dir: params.db_dir,
            signal_flag: params.signal_flag,
            ctx,
            files_modified: &slot_result.iteration_result.files_modified,
            effective_model: slot_result.iteration_result.effective_model.as_deref(),
            effective_effort: slot_result.iteration_result.effective_effort,
            slot_index: Some(slot_idx),
        });

    slot_result.iteration_result.key_decisions_count = processing_outcome.key_decisions_count;

    // The claimed task was completed in this pass iff its id appears in the
    // pipeline's deduped completion list. Cross-task `<completed>Y</completed>`
    // entries land in the list too but never satisfy this predicate — Y stays
    // out of `pending_slot_tasks` (it was a peer slot's task or was already
    // terminal), so the orphan-reset semantics are preserved.
    let slot_marked_done = task_id
        .as_ref()
        .map(|tid| {
            processing_outcome
                .completed_task_ids
                .iter()
                .any(|c| c == tid)
        })
        .unwrap_or(false);

    agg.tasks_completed += processing_outcome.tasks_completed;
    if processing_outcome.tasks_completed > 0 {
        agg.any_completed = true;
    }

    // Pipeline may have flipped the outcome from a non-Completed value to
    // `Completed` via the completion ladder; checking `outcome` post-pipeline
    // collapses the legacy "crash but task done" branch into the same arm as
    // a clean success.
    if !matches!(
        slot_result.iteration_result.outcome,
        IterationOutcome::Crash(_)
    ) || slot_marked_done
    {
        agg.all_crashed = false;
    }

    if slot_marked_done && let Some(ref tid) = task_id {
        ctx.pending_slot_tasks.retain(|t| t != tid);
    }

    // FEAT-010 AC: queue reorder hints for the next wave. `select_parallel_group`
    // does not yet honor hints (it ranks by score), so this acts as a
    // preservation queue — operators see them in logs and a future selection
    // pass can drain them.
    if let IterationOutcome::Reorder(ref rid) = slot_result.iteration_result.outcome {
        ctx.pending_reorder_hints.push(rid.clone());
        eprintln!("[slot {}] Queued reorder hint: {}", slot_idx, rid);
    }

    for f in &slot_result.iteration_result.files_modified {
        if !agg.aggregated_files.contains(f) {
            agg.aggregated_files.push(f.clone());
        }
    }

    if slot_result.iteration_result.should_stop {
        agg.wave_should_stop = true;
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use crate::loop_engine::config::PermissionMode;
    use crate::loop_engine::engine::{SlotContext, SlotIterationParams};
    use crate::loop_engine::model::OPUS_MODEL;
    use crate::loop_engine::prompt::slot::{
        SlotPromptBundle, SlotPromptParams, build_prompt as build_slot_prompt_bundle,
    };
    use crate::loop_engine::runner::RunnerKind;
    use crate::loop_engine::signals::SignalFlag;
    use crate::loop_engine::test_utils::{insert_task, setup_test_db};
    use crate::models::Task;
    use std::path::Path;

    /// Build a minimal SlotIterationParams wired to a test DB.
    /// `signal_flag` is shared so tests can observe/trip it across slots.
    fn make_slot_params(db_dir: &Path, signal_flag: SignalFlag) -> SlotIterationParams {
        SlotIterationParams {
            db_dir: db_dir.to_path_buf(),
            permission_mode: PermissionMode::Dangerous,
            signal_flag,
            default_model: None,
            verbose: false,
            iteration: 1,
            max_iterations: 1,
            elapsed_secs: 0,
            task_prefix: None,
        }
    }

    /// Build a SlotPromptParams pointing at a temp project root + base prompt.
    fn make_prompt_params(
        project_root: &Path,
        base_prompt_path: std::path::PathBuf,
    ) -> SlotPromptParams<'static> {
        SlotPromptParams {
            project_root: project_root.to_path_buf(),
            base_prompt_path,
            permission_mode: PermissionMode::Dangerous,
            steering_path: None,
            session_guidance: "",
        }
    }

    /// Synthesize a `SlotPromptBundle` directly without invoking
    /// `build_prompt`. Useful for tests that don't need the full
    /// learnings/source-context pipeline.
    fn dummy_bundle(task_id: &str) -> SlotPromptBundle {
        SlotPromptBundle {
            prompt: format!("# slot prompt for {task_id}\n"),
            task_id: task_id.to_string(),
            task_files: Vec::new(),
            shown_learning_ids: Vec::new(),
            resolved_model: None,
            difficulty: None,
            section_sizes: Vec::new(),
            dropped_sections: Vec::new(),
        }
    }

    fn make_slot(
        slot_index: usize,
        working_root: std::path::PathBuf,
        prompt_bundle: SlotPromptBundle,
    ) -> SlotContext {
        SlotContext {
            slot_index,
            working_root,
            prompt_bundle,
            effective_runner: RunnerKind::Claude,
        }
    }

    // --- Struct field contracts (AC 1-3) ---

    #[test]
    fn test_slot_context_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bundle = dummy_bundle("FEAT-1");
        let ctx = make_slot(2, tmp.path().to_path_buf(), bundle);
        assert_eq!(ctx.slot_index, 2);
        assert_eq!(ctx.working_root, tmp.path());
        assert_eq!(ctx.prompt_bundle.task_id, "FEAT-1");
    }

    #[test]
    fn test_slot_result_fields() {
        use crate::loop_engine::config::IterationOutcome;
        use crate::loop_engine::engine::{IterationResult, SlotResult};
        let sr = SlotResult {
            slot_index: 1,
            iteration_result: IterationResult {
                outcome: IterationOutcome::Completed,
                task_id: Some("FEAT-1".to_string()),
                files_modified: vec!["a.rs".to_string()],
                should_stop: false,
                output: String::new(),
                effective_model: None,
                effective_effort: None,
                key_decisions_count: 0,
                conversation: None,
                shown_learning_ids: Vec::new(),
            },
            claim_succeeded: true,
            shown_learning_ids: vec![42, 77],
            prompt_for_overflow: None,
            section_sizes: Vec::new(),
            dropped_sections: Vec::new(),
            task_difficulty: None,
            effective_runner: RunnerKind::Claude,
        };
        assert_eq!(sr.slot_index, 1);
        assert!(matches!(
            sr.iteration_result.outcome,
            IterationOutcome::Completed
        ));
        // FEAT-002 AC: SlotResult exposes shown_learning_ids at the top
        // level so the main thread can record bandit feedback without
        // re-reading the bundle (which has been moved into the worker).
        assert_eq!(sr.shown_learning_ids, vec![42, 77]);
    }

    // --- run_slot_iteration: early exit on pre-signaled flag (AC 8) ---

    #[test]
    fn test_run_slot_iteration_honors_pre_set_signal_flag() {
        let (temp, _conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();

        let signal = SignalFlag::new();
        signal.set(); // pre-signal — slot must bail before spawning Claude
        let params = make_slot_params(temp.path(), signal);

        let slot = make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-1"));
        let result = run_slot_iteration(&slot, &params).expect("run_slot_iteration");
        assert_eq!(result.slot_index, 0);
        assert!(matches!(
            result.iteration_result.outcome,
            IterationOutcome::Empty
        ));
        assert!(result.iteration_result.should_stop);
        assert_eq!(result.iteration_result.task_id.as_deref(), Some("FEAT-1"),);
    }

    // --- prompt::slot::build_prompt: includes task JSON + completion ---

    #[test]
    fn test_slot_bundle_contains_task_and_completion_sections() {
        let (_temp, conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().join("base.md");
        std::fs::write(&base, "BASE_PROMPT_CONTENT").unwrap();

        let mut task = Task::new("FEAT-42", "Do the thing");
        task.description = Some("Detailed desc".to_string());
        task.difficulty = Some("high".to_string());

        let prompt_params = make_prompt_params(tmp.path(), base);
        let bundle = build_slot_prompt_bundle(&conn, &task, &prompt_params);
        let prompt = &bundle.prompt;
        assert!(prompt.contains("FEAT-42"), "missing task id");
        assert!(prompt.contains("Do the thing"), "missing title");
        assert!(prompt.contains("Detailed desc"), "missing description");
        assert!(prompt.contains("\"difficulty\""), "missing difficulty");
        assert!(
            prompt.contains("<completed>FEAT-42</completed>"),
            "missing completion tag instruction",
        );
        assert!(
            prompt.contains("BASE_PROMPT_CONTENT"),
            "missing base prompt content",
        );
        assert_eq!(bundle.difficulty.as_deref(), Some("high"));
    }

    #[test]
    fn test_slot_bundle_tolerates_missing_base_prompt() {
        let (_temp, conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let task = Task::new("FEAT-1", "t");
        // base_prompt_path does not exist — must not panic
        let prompt_params = make_prompt_params(tmp.path(), tmp.path().join("does-not-exist.md"));
        let bundle = build_slot_prompt_bundle(&conn, &task, &prompt_params);
        assert!(bundle.prompt.contains("FEAT-1"));
    }

    // --- claim_slot_task ---

    #[test]
    fn test_claim_slot_task_updates_todo_to_in_progress() {
        let (_tmp, mut conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "todo", 10);
        assert!(claim_slot_task(&mut conn, "FEAT-1"));
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn test_claim_slot_task_idempotent_on_already_in_progress() {
        let (_tmp, mut conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "in_progress", 10);
        // UPDATE matches because WHERE clause accepts in_progress too
        assert!(claim_slot_task(&mut conn, "FEAT-1"));
    }

    #[test]
    fn test_claim_slot_task_rejects_done_task() {
        let (_tmp, mut conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "done", 10);
        assert!(!claim_slot_task(&mut conn, "FEAT-1"));
    }

    // --- SlotIterationParams cloneability (Arc + clone into threads) ---

    #[test]
    fn test_slot_iteration_params_is_clone() {
        let tmp = tempfile::TempDir::new().unwrap();
        let params = SlotIterationParams {
            db_dir: tmp.path().to_path_buf(),
            permission_mode: PermissionMode::Dangerous,
            signal_flag: SignalFlag::new(),
            default_model: Some(OPUS_MODEL.to_string()),
            verbose: true,
            iteration: 7,
            max_iterations: 100,
            elapsed_secs: 42,
            task_prefix: None,
        };
        let cloned = params.clone();
        assert_eq!(cloned.db_dir, params.db_dir);
        assert_eq!(cloned.verbose, params.verbose);
        assert_eq!(cloned.default_model.as_deref(), Some(OPUS_MODEL));
    }

    // --- AC 7: claim_slot_task / try_claim predicate semantics
    //     (moved from recovery_primitives in orchestrator.rs) ---

    #[test]
    fn try_claim_succeeds_on_todo() {
        let (_tmp, mut conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "todo", 10);
        assert!(claim_slot_task(&mut conn, "FEAT-1"));

        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "in_progress");
    }

    #[test]
    fn try_claim_idempotent_on_in_progress() {
        let (_tmp, mut conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "in_progress", 10);
        assert!(
            claim_slot_task(&mut conn, "FEAT-1"),
            "in_progress is in the WHERE set — re-claim is idempotent",
        );
    }

    #[test]
    fn try_claim_rejects_blocked() {
        let (_tmp, mut conn) = setup_test_db();
        insert_task(&conn, "FEAT-1", "t", "blocked", 10);
        assert!(
            !claim_slot_task(&mut conn, "FEAT-1"),
            "blocked is outside the WHERE set — slot must skip",
        );

        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "blocked", "row must not change on failed claim");
    }
}
