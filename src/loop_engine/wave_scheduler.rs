//! Parallel-wave scheduling and merge-back orchestration.
//!
//! Carved out of `engine.rs` (PRD 02, FEAT-003). This module owns the
//! parallel-slot wave pipeline: the per-wave preflight (`wave_preflight_check`),
//! eligible-group selection plumbing (`build_slot_contexts`,
//! `build_shared_slot_params`, `build_slot_prompt_params`), the threaded
//! claim-and-spawn fan-out (`run_parallel_wave`), the no-eligible / deadlock
//! handlers (`handle_no_eligible_tasks`, `handle_ephemeral_deadlock`), the
//! FEAT-002 reset/halt contract (`apply_merge_fail_reset_and_halt_check`), the
//! post-merge reconcile (`apply_post_merge_reconcile`), and the wave entry
//! point (`run_wave_iteration`).
//!
//! The data types these functions operate on (`WaveIterationParams`,
//! `WaveOutcome`, `WaveTerminal`, `WaveResult`, `WaveAggregator`, `FailedMerge`,
//! `MergeFailHaltDecision`, `IterationContext`, the `Slot*` family, …) remain in
//! `engine.rs` and are imported here — they are consumed by `run_loop` and the
//! inline test modules that stay in `engine.rs`, so moving them would widen the
//! carve's blast radius. The leaf concerns this module depends on come from
//! `slot.rs` (FEAT-001: `run_slot_iteration`, `claim_slot_task`,
//! `process_slot_result`, `slot_failure_result`) and `recovery.rs` (FEAT-002:
//! `handle_task_failure`, `check_override_invalidation`).
//!
//! `engine.rs` re-exports `run_wave_iteration` / `run_parallel_wave` `pub` so
//! the external import paths integration tests rely on stay valid (FR-008); the
//! re-exported helpers are `pub(super)`.
//!
//! Defense layer #1 (slot-path threading) is load-bearing here: the
//! `&[PathBuf]` returned by `ensure_slot_worktrees` (carried by
//! `WaveIterationParams::slot_worktree_paths`) is threaded straight into
//! `merge_slot_branches_with_resolver`; slot 0's path is NEVER recomputed via
//! `compute_slot_worktree_path(_, branch, 0)`. See `src/loop_engine/CLAUDE.md`
//! → "Parallel-slot scheduling".

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::commands::next::selection::select_parallel_group;
use crate::commands::run as run_cmd;
use crate::db::prefix::prefix_and;
use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::branch;
use crate::loop_engine::config::{self, IterationOutcome};
use crate::loop_engine::display;
use crate::loop_engine::engine::{
    FailedMerge, IterationContext, MergeFailHaltDecision, SlotContext, SlotIterationParams,
    SlotResult, WaveAggregator, WaveIterationParams, WaveOutcome, WaveResult, WaveTerminal,
    apply_review_model_override, resolve_effective_runner,
};
use crate::loop_engine::git_reconcile::{
    reconcile_external_git_completions, reconcile_merged_slot_completions,
};
use crate::loop_engine::merge_resolver;
use crate::loop_engine::model;
use crate::loop_engine::progress;
use crate::loop_engine::prompt;
use crate::loop_engine::recovery::{check_override_invalidation, handle_task_failure};
use crate::loop_engine::runner::RunnerKind;
use crate::loop_engine::signals::{self, SignalFlag};
// `claim_slot_task` is a deprecated shim over `TaskLifecycle::try_claim`; the
// wave fan-out still calls it verbatim (this carve does not migrate it).
#[allow(deprecated)]
use crate::loop_engine::slot::{
    claim_slot_task, process_slot_result, run_slot_iteration, slot_failure_result,
};
use crate::loop_engine::worktree;
use crate::models::RunStatus;

/// Run one parallel wave: claim all tasks sequentially (main thread), then
/// spawn one OS thread per slot and wait for every thread to join.
///
/// **FEAT-002 contract**: every `SlotContext` passed in MUST already carry
/// a fully-built `prompt_bundle` (constructed by `prompt::slot::build_prompt`
/// on the main thread before this function is called — see
/// `build_slot_contexts`). This function never opens a `&Connection` from
/// inside a worker thread; the claim is the only DB write here, and it
/// stays on the main thread.
///
/// The claim loop is intentionally serial on the main thread:
/// - rusqlite `Connection` is not `Send`, so there's one DB writer.
/// - Serial claims prevent two slots from racing to claim the same row
///   (the UPDATE WHERE `status IN ('todo', 'in_progress')` guard would also
///   catch this, but we prefer the stronger invariant that every spawned
///   thread has an already-claimed task).
///
/// Slots whose claim fails (task already `done`/`blocked`) are reported as
/// a `Crash(RuntimeError)` entry so the outer loop's tracking logic sees
/// them; they are not silently dropped.
///
/// Thread panics are captured from `JoinHandle::join`'s `Err` branch and
/// converted into `Crash(RuntimeError)` entries — we never unwrap on join.
pub fn run_parallel_wave(
    conn: &mut Connection,
    slots: Vec<SlotContext>,
    params: Arc<SlotIterationParams>,
) -> WaveResult {
    let start_time = Instant::now();

    let mut claimed: Vec<(SlotContext, bool)> = Vec::with_capacity(slots.len());
    for slot in slots {
        #[allow(deprecated)]
        let ok = claim_slot_task(conn, &slot.prompt_bundle.task_id);
        claimed.push((slot, ok));
    }

    type SlotHandle = Option<thread::JoinHandle<TaskMgrResult<SlotResult>>>;
    let mut handles: Vec<(usize, String, SlotHandle)> = Vec::with_capacity(claimed.len());
    let mut failures: Vec<SlotResult> = Vec::new();

    for (slot, ok) in claimed {
        let slot_index = slot.slot_index;
        let task_id = slot.prompt_bundle.task_id.clone();
        if !ok {
            failures.push(slot_failure_result(
                slot_index,
                Some(task_id.clone()),
                format!("claim failed for task {}", task_id),
                false,
            ));
            handles.push((slot_index, task_id, None));
            continue;
        }

        let params_for_thread = Arc::clone(&params);
        let handle = thread::spawn(move || run_slot_iteration(&slot, &params_for_thread));
        handles.push((slot_index, task_id, Some(handle)));
    }

    let mut outcomes: Vec<SlotResult> = Vec::with_capacity(handles.len());
    for (slot_index, task_id, maybe_handle) in handles {
        let Some(handle) = maybe_handle else {
            // Already recorded in `failures` above — skip re-emitting.
            continue;
        };
        match handle.join() {
            Ok(Ok(result)) => outcomes.push(result),
            Ok(Err(e)) => {
                eprintln!(
                    "[slot {}] iteration error for {}: {}",
                    slot_index, task_id, e,
                );
                outcomes.push(slot_failure_result(
                    slot_index,
                    Some(task_id),
                    format!("iteration error: {}", e),
                    true,
                ));
            }
            Err(panic_payload) => {
                let msg = panic_payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| {
                        panic_payload
                            .downcast_ref::<&'static str>()
                            .map(|s| (*s).to_string())
                    })
                    .unwrap_or_else(|| "unknown panic".to_string());
                eprintln!("[slot {}] thread panicked: {}", slot_index, msg);
                outcomes.push(slot_failure_result(
                    slot_index,
                    Some(task_id),
                    format!("thread panic: {}", msg),
                    true,
                ));
            }
        }
    }

    // Merge claim failures into outcomes so callers see every slot represented.
    outcomes.extend(failures);
    outcomes.sort_by_key(|r| r.slot_index);

    WaveResult {
        outcomes,
        wave_duration: start_time.elapsed(),
    }
}

/// Pre-wave preflight: signal/stop-file checks and crash backoff/abort.
/// Returns `Some(WaveOutcome)` when the wave should bail out before doing
/// any work, `None` when execution should proceed.
fn wave_preflight_check(
    params: &WaveIterationParams<'_>,
    ctx: &mut IterationContext,
) -> Option<WaveOutcome> {
    // Match sequential semantics so Ctrl+C and `.stop` files exit the loop
    // the same way in both paths.
    if params.signal_flag.is_signaled() {
        return Some(WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: false,
            terminal: Some(WaveTerminal {
                exit_code: 130,
                reason: "signal received".to_string(),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges: Vec::new(),
        });
    }
    if signals::check_stop_signal(params.tasks_dir, params.task_prefix) {
        eprintln!("Stop signal detected (.stop file found)");
        return Some(WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: false,
            terminal: Some(WaveTerminal {
                exit_code: 0,
                reason: "stop signal".to_string(),
                run_status: None,
            }),
            was_stopped: true,
            failed_merges: Vec::new(),
        });
    }

    // Crash backoff + abort. Identical contract to the sequential path so
    // learning [1005] (don't burn iterations on a wedged task) holds even
    // when every slot of the previous wave crashed.
    let backoff = ctx.crash_tracker.backoff_duration();
    if !backoff.is_zero() {
        eprintln!(
            "Crash backoff: waiting {} before retry...",
            display::format_duration(backoff.as_secs())
        );
        thread::sleep(backoff);
    }
    if ctx.crash_tracker.should_abort() {
        eprintln!("Too many consecutive crashes, aborting loop");
        return Some(WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 1,
                reason: "too many crashes".to_string(),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges: Vec::new(),
        });
    }
    None
}

/// No eligible tasks were selected for this wave. Drives the stale tracker
/// exactly like sequential does on `IterationOutcome::NoEligibleTasks`,
/// returning a terminal outcome when the tracker tripped its abort threshold.
fn handle_no_eligible_tasks(
    params: &WaveIterationParams<'_>,
    ctx: &mut IterationContext,
) -> WaveOutcome {
    ctx.stale_tracker.check("stale", "stale");
    progress::log_iteration(
        params.progress_path,
        params.iteration,
        None,
        &IterationOutcome::NoEligibleTasks,
        &[],
        None,
        None,
        None,
    );
    if ctx.stale_tracker.should_abort() {
        eprintln!(
            "Aborting: no eligible tasks after {} consecutive stale iterations",
            ctx.stale_tracker.count()
        );
        return WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 1,
                reason: format!(
                    "no eligible tasks after {} consecutive stale iterations",
                    ctx.stale_tracker.count()
                ),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges: Vec::new(),
        };
    }
    WaveOutcome {
        tasks_completed: 0,
        iteration_consumed: true,
        terminal: None,
        was_stopped: false,
        failed_merges: Vec::new(),
    }
}

/// FEAT-004 deadlock guard: every eligible candidate was blocked exclusively
/// by un-merged work on `{branch}-slot-N` ephemeral branches. Emit per-
/// candidate diagnostics, then synthesize a merge-fail wave by deriving slot
/// indices from the blocking branch names so `run_loop`'s wave-loop boundary
/// runs the FEAT-002 reset/halt-check contract.
///
/// **No tasks to reset**: the deferred candidates are still `todo` — they
/// were never claimed. The synthesized `FailedMerge` entries therefore carry
/// `task_id: None`, and `apply_merge_fail_reset_and_halt_check`'s reset
/// pass becomes a no-op (`None` skipped via `if let Some`).
///
/// **Slot index derivation**: branches sourced from
/// `worktree::list_ephemeral_slot_branches` are guaranteed to match the
/// `{branch}-slot-N` shape (parsed and re-emitted by that helper). We strip
/// `{branch}-slot-` and parse the suffix; on the off chance a branch ever
/// slips through with a non-numeric suffix, that branch is logged and skipped
/// — slot index 0 is reserved for slot 0 (the loop's own branch) and must
/// never be synthesized here.
fn handle_ephemeral_deadlock(
    params: &WaveIterationParams<'_>,
    ctx: &mut IterationContext,
    diagnostics: Vec<(String, Vec<String>)>,
) -> WaveOutcome {
    ctx.stale_tracker.check("stale", "stale");
    progress::log_iteration(
        params.progress_path,
        params.iteration,
        None,
        &IterationOutcome::NoEligibleTasks,
        &[],
        None,
        None,
        None,
    );

    eprintln!(
        "Cross-wave deadlock: every eligible candidate is blocked by un-merged ephemeral branch(es). \
         Treating as merge-fail wave so the halt threshold can fire."
    );
    for (cand_id, branches) in &diagnostics {
        eprintln!("  {} blocked by: {}", cand_id, branches.join(", "));
    }

    // Collect distinct slot indices from the union of blocking branches across
    // all diagnostics. Order is stable (sorted ascending) so the eventual
    // halt-diagnostic from `apply_merge_fail_reset_and_halt_check` lists slots
    // in a predictable order.
    let prefix = format!("{}-slot-", params.branch);
    let mut synth_slots: Vec<usize> = Vec::new();
    for (_, branches) in &diagnostics {
        for branch in branches {
            let Some(suffix) = branch.strip_prefix(&prefix) else {
                continue;
            };
            match suffix.parse::<usize>() {
                Ok(slot) if slot > 0 => {
                    if !synth_slots.contains(&slot) {
                        synth_slots.push(slot);
                    }
                }
                _ => eprintln!(
                    "Warning: skipping ephemeral branch with non-numeric / zero slot suffix: {}",
                    branch
                ),
            }
        }
    }
    synth_slots.sort();
    // If every blocking branch had an unparseable slot suffix, `synth_slots`
    // is empty and `WaveOutcome.failed_merges` would be empty too — at which
    // point `apply_merge_fail_reset_and_halt_check` would reset the
    // consecutive counter to 0 and the deadlock would silently spin forever
    // until the stale-iteration tracker aborts. Insert a single
    // `SYNTHETIC_DEADLOCK_SLOT` entry so the threshold counter still
    // increments. The diagnostic step in
    // `apply_merge_fail_reset_and_halt_check` special-cases this slot index
    // to print a `<malformed deadlock blocker>` placeholder instead of
    // synthesizing a `{branch}-slot-18446744073709551615` name.
    if synth_slots.is_empty() && !diagnostics.is_empty() {
        eprintln!(
            "warning: deadlock guard fired with no parseable ephemeral slot indices \
             — inserting synthetic halt slot so the threshold counter still increments"
        );
        synth_slots.push(SYNTHETIC_DEADLOCK_SLOT);
    }
    let failed_merges: Vec<FailedMerge> = synth_slots
        .into_iter()
        .map(|slot| FailedMerge {
            slot,
            task_id: None,
        })
        .collect();

    WaveOutcome {
        tasks_completed: 0,
        iteration_consumed: true,
        terminal: None,
        was_stopped: false,
        failed_merges,
    }
}

/// Build per-slot `SlotContext` entries, pairing each scored task with its
/// pre-allocated worktree path AND assembling the full `SlotPromptBundle`
/// on the main thread.
///
/// **FEAT-002 contract**: bundle assembly happens here, on the main thread,
/// BEFORE `run_parallel_wave` spawns any worker. The bundle is the only
/// task-derived state a worker reads; `SlotContext` no longer carries a
/// `Task` reference, so all DB-backed prompt assembly (learnings,
/// task_files, source context) MUST run from this single connection.
///
/// Each slot gets its own activity epoch — sharing one would let activity
/// in one slot silently extend another slot's watchdog deadline.
pub(super) fn build_slot_contexts(
    conn: &Connection,
    group: Vec<crate::commands::next::selection::ScoredTask>,
    slot_paths: &[PathBuf],
    slot_prompt_params: &prompt::slot::SlotPromptParams<'_>,
) -> Vec<SlotContext> {
    group
        .into_iter()
        .zip(slot_paths.iter())
        .enumerate()
        .map(|(idx, (scored, path))| {
            // The DB row will flip to `in_progress` in run_parallel_wave's
            // claim step; reflect that in the bundle's task JSON so the
            // agent sees the post-claim state. The acceptance criteria,
            // description, model, and difficulty fields don't change with
            // claim, so this is the only field that needs a pre-mutation.
            let mut task = scored.task;
            task.status = crate::models::TaskStatus::InProgress;
            let prompt_bundle = prompt::slot::build_prompt(conn, &task, slot_prompt_params);
            SlotContext {
                slot_index: idx,
                working_root: path.clone(),
                prompt_bundle,
                // Default to Claude. Main-thread enrichment in
                // `run_wave_iteration` overwrites with the resolved value
                // from `resolve_effective_runner(ctx, ...)` before the slot
                // thread spawns. Test fixtures that call `build_slot_contexts`
                // directly (without the enrichment step) inherit Claude —
                // the default-empty regression behavior.
                effective_runner: RunnerKind::Claude, // kind-correct: sentinel default; same pattern as SlotPromptBundle — provider identity, not capability
            }
        })
        .collect()
}

/// Build the shared per-slot `SlotIterationParams`. `SignalFlag` clones the
/// inner `Arc` so all threads observe the same SIGINT/SIGTERM signal.
///
/// `base_prompt_path` is intentionally NOT carried on this struct anymore —
/// the base prompt content is baked into `SlotPromptBundle.prompt` at
/// assembly time, so workers never need to read the file (and thus don't
/// need a path that might race with a concurrent edit).
fn build_shared_slot_params(params: &WaveIterationParams<'_>) -> Arc<SlotIterationParams> {
    Arc::new(SlotIterationParams {
        db_dir: params.db_dir.to_path_buf(),
        permission_mode: params.permission_mode.clone(),
        signal_flag: params.signal_flag.clone(),
        default_model: params.default_model.map(|s| s.to_string()),
        verbose: params.verbose,
        iteration: params.iteration,
        max_iterations: params.max_iterations,
        elapsed_secs: params.elapsed_secs,
        task_prefix: params.task_prefix.map(|s| s.to_string()),
    })
}

/// Build the `SlotPromptParams` shared across all slots in this wave.
///
/// Pulled out of `build_slot_contexts` so the call site at
/// `run_wave_iteration` can construct it once and pass by reference, which
/// avoids cloning the `PathBuf` per slot.
fn build_slot_prompt_params<'a>(
    params: &'a WaveIterationParams<'a>,
) -> prompt::slot::SlotPromptParams<'a> {
    prompt::slot::SlotPromptParams {
        project_root: params.source_root.to_path_buf(),
        base_prompt_path: params.base_prompt_path.to_path_buf(),
        permission_mode: params.permission_mode.clone(),
        steering_path: params.steering_path,
        session_guidance: params.session_guidance,
    }
}

/// Count tasks still in flight for the current PRD prefix. "Done",
/// "skipped", "irrelevant" and "blocked" are terminal — anything else is
/// still work to do.
fn count_remaining_active_tasks(conn: &Connection, task_prefix: Option<&str>) -> i64 {
    let (clause, param) = prefix_and(task_prefix);
    let sql = format!(
        "SELECT COUNT(*) FROM tasks WHERE status NOT IN \
         ('done','irrelevant','skipped','blocked') {clause}"
    );
    let p_vec: Vec<&dyn rusqlite::types::ToSql> = match &param {
        Some(p) => vec![p],
        None => vec![],
    };
    conn.query_row(&sql, p_vec.as_slice(), |r| r.get(0))
        .unwrap_or(0)
}

/// Sleep between waves so the loop respects `--iteration-delay` the same
/// way it does between sequential iterations. Polls the signal flag every
/// 200ms so SIGINT/SIGTERM short-circuits the delay; returns true if the
/// signal fired during the wait so the caller can treat it as a stop.
fn wait_inter_wave_delay(delay: Duration, signal_flag: &SignalFlag) -> bool {
    if delay.is_zero() {
        return false;
    }
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline {
        if signal_flag.is_signaled() {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Reset a listed task back to `todo` (used by wave FEAT-002 and overflow
/// recovery rungs). The per-ID `resurrect_for_iteration` verb intentionally
/// accepts any status for the IDs in the list — callers are responsible for
/// only passing tasks whose state warrants a reset to the next iteration.
/// Contrast with `recover_in_progress_for_prefix`, which keeps the
/// `status = 'in_progress'` guard.
///
/// Used by:
/// - The wave-loop FEAT-002 reset/halt-check contract — a slot's task whose
///   merge-back failed must not stay pinned in `in_progress`.
/// - Post-loop cleanup (Step 17.5 / 17.6) when the loop exits via deadline /
///   max-iterations rather than a per-task done signal.
///
/// Logs success / no-op / failure to stderr; failures never propagate
/// (matches the FEAT-002 failure-mode AC: "if reset itself fails for a task,
/// log the failure but continue with remaining failed slots").
pub(super) fn reset_task_to_todo(conn: &mut Connection, task_id: &str, kind_label: &str) {
    match TaskLifecycle::new(conn).resurrect_for_iteration(None, &[task_id]) {
        Ok(1) => eprintln!("Reset {} {} to todo", kind_label, task_id),
        Ok(_) => {} // row missing, or status changed by reconciliation
        Err(e) => eprintln!("Warning: failed to reset task {}: {}", task_id, e),
    }
}

/// Sentinel slot index for deadlock-guard waves where every blocking
/// `{branch}-slot-N` ephemeral had a non-numeric / zero suffix and the
/// guard had nothing parseable to enumerate. `handle_ephemeral_deadlock`
/// inserts one `FailedMerge { slot: SYNTHETIC_DEADLOCK_SLOT, .. }` so the
/// FEAT-002 threshold counter still increments. `apply_merge_fail_reset_and_halt_check`
/// special-cases this slot index in its diagnostic step to avoid
/// synthesizing the meaningless `{branch}-slot-18446744073709551615` name.
pub(crate) const SYNTHETIC_DEADLOCK_SLOT: usize = usize::MAX;

/// Apply the FEAT-002 reset/halt contract to one wave's outcome.
///
/// Step ordering (contractual — reset MUST come before increment so a halted
/// run never leaves a failed-slot task pinned in `in_progress`):
///
/// 1. For each failed slot's claimed task: reset to `todo`; drain it from
///    `ctx.pending_slot_tasks` (mirror of engine.rs:1269). Reset failures are
///    logged but never fatal — remaining slots still process.
/// 2. Increment `ctx.consecutive_merge_fail_waves`.
/// 3. Compare to `threshold`. `0` disables the halt entirely (legacy "log
///    and continue" behavior preserved bit-for-bit).
/// 4. On halt: emit a per-slot ephemeral branch name diagnostic to stderr.
/// 5. Return `Halt { ... }` to break the loop, or `Continue` otherwise.
///
/// When `failed_merges.is_empty()` the counter is reset to `0` and `Continue`
/// is returned without doing any reset / diagnostic work.
pub(super) fn apply_merge_fail_reset_and_halt_check(
    conn: &mut Connection,
    ctx: &mut IterationContext,
    branch: &str,
    failed_merges: &[FailedMerge],
    threshold: u32,
) -> MergeFailHaltDecision {
    if failed_merges.is_empty() {
        ctx.consecutive_merge_fail_waves = 0;
        return MergeFailHaltDecision::Continue;
    }

    // (1) Reset every failed slot's claimed task. The single-slice shape
    //     (a `Vec<FailedMerge>`) makes the slot/task pairing a type-level
    //     guarantee — no zip-truncation footgun.
    for fm in failed_merges {
        if let Some(tid) = &fm.task_id {
            reset_task_to_todo(conn, tid, "failed-slot task");
            ctx.pending_slot_tasks.retain(|t| t != tid);
        }
    }

    // (2) Increment AFTER reset so a halted run still leaves the DB
    //     re-runnable (the known-bad implementation does this in the wrong
    //     order — see the regression test).
    ctx.consecutive_merge_fail_waves += 1;

    // (3) Threshold check. `0` is a sentinel for "never halt".
    if threshold == 0 || ctx.consecutive_merge_fail_waves < threshold {
        return MergeFailHaltDecision::Continue;
    }

    // (4) Per-slot diagnostic. `ephemeral_slot_branch` is the canonical name
    //     source — never construct `{branch}-slot-{N}` inline (learning [1870]).
    //     The `SYNTHETIC_DEADLOCK_SLOT` sentinel renders as a placeholder
    //     instead of `{branch}-slot-18446744073709551615`.
    let names: Vec<String> = failed_merges
        .iter()
        .map(|fm| {
            if fm.slot == SYNTHETIC_DEADLOCK_SLOT {
                "<malformed deadlock blocker>".to_string()
            } else {
                worktree::ephemeral_slot_branch(branch, fm.slot)
            }
        })
        .collect();
    eprintln!(
        "Aborting: {} consecutive merge-back failure wave(s) (threshold={}). \
         Failed slot branches: {}",
        ctx.consecutive_merge_fail_waves,
        threshold,
        names.join(", ")
    );

    // (5) Halt.
    MergeFailHaltDecision::Halt {
        exit_code: 1,
        exit_reason: format!(
            "{} consecutive merge-back failure wave(s) (threshold={})",
            ctx.consecutive_merge_fail_waves, threshold
        ),
    }
}

/// Read the PRD JSON at `prd_path` and return its `implicitOverlapFiles`
/// list (FEAT-003). Returns an empty Vec on any error (file missing,
/// invalid JSON, field absent) — implicit-overlap detection still works
/// from the baseline list and ProjectConfig extension.
///
/// Best-effort I/O on the wave-loop hot path: a parse failure should NOT
/// abort the wave, so all error paths swallow with a Vec::new() default.
pub(super) fn read_prd_implicit_overlap_files(prd_path: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(prd_path) else {
        return Vec::new();
    };
    let Ok(prd): Result<crate::commands::init::parse::PrdFile, _> = serde_json::from_str(&contents)
    else {
        return Vec::new();
    };
    prd.implicit_overlap_files.unwrap_or_default()
}

/// FEAT-003 post-merge reconcile step. Scans slot 0's `{pre_merge_head}..HEAD`
/// for `<TASK-ID>-completed` markers and, on a non-empty result, mirrors the
/// agg/ctx mutations the sequential completion ladder performs:
///   - bump `agg.tasks_completed` by the reconciled count,
///   - flip `agg.any_completed`,
///   - record success on the crash tracker (FEAT-010 parity with the
///     `any_completed` branch in `run_wave_iteration`),
///   - drain reconciled IDs from `ctx.pending_slot_tasks` so step 17.6's
///     loop-exit reset doesn't flip the row back to `todo`,
///   - emit a one-line stderr summary for the operator.
///
/// Called from `run_wave_iteration` BEFORE `run_cmd::update` and BEFORE the
/// external-git `let mut tasks_completed = agg.tasks_completed;` shadow so
/// the terminal returns observe the bumped counter. The reconcile function
/// itself never errors — failures (git, DB, PRD I/O) are absorbed there and
/// surface as an empty Vec, so this caller has no failure path.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_post_merge_reconcile(
    slot0_path: &Path,
    pre_merge_head: &str,
    conn: &mut Connection,
    run_id: &str,
    prd_path: &Path,
    task_prefix: Option<&str>,
    ctx: &mut IterationContext,
    agg: &mut WaveAggregator,
) {
    let reconciled = reconcile_merged_slot_completions(
        slot0_path,
        pre_merge_head,
        conn,
        run_id,
        prd_path,
        task_prefix,
    );
    if reconciled.is_empty() {
        return;
    }
    agg.tasks_completed += reconciled.len() as u32;
    agg.any_completed = true;
    ctx.crash_tracker.record_success();
    ctx.pending_slot_tasks.retain(|t| !reconciled.contains(t));
    eprintln!(
        "Post-merge reconcile: marked {} task(s) done from merged commits ({})",
        reconciled.len(),
        reconciled.join(", ")
    );
}

/// Wave-mode equivalent of `run_iteration` for the parallel execution path.
///
/// Pre-wave: signal/stop checks, crash backoff, parallel group selection,
/// stale-tracker bookkeeping. Wave: spawn slots via `run_parallel_wave`,
/// merge slot branches back to the loop branch. Post-wave: per-slot progress
/// logging, status update dispatch, completion detection, crash policy
/// (all-crashed → record_crash; any-completed → record_success), reorder
/// hint queueing, external-git reconciliation, terminal-condition checks.
///
/// The function only mutates `IterationContext` from the main thread — slot
/// threads inside `run_parallel_wave` never touch shared state. All file
/// aggregation and PRD updates happen here so the per-thread invariants
/// established by FEAT-009 stay intact.
pub fn run_wave_iteration(
    mut params: WaveIterationParams<'_>,
    ctx: &mut IterationContext,
) -> WaveOutcome {
    if let Some(outcome) = wave_preflight_check(&params, ctx) {
        return outcome;
    }

    // Parallel group selection. Reuses the same scoring + greedy
    // file-overlap walk as `task-mgr next`, capped at `parallel_slots`.
    //
    // FEAT-003: merge project-config and PRD-level implicit-overlap extensions
    // into the slate so `select_parallel_group` can detect shared-infra files
    // beyond the baseline `IMPLICIT_OVERLAP_FILES` list. Both lists were
    // loaded once at run-loop startup and threaded through `WaveParams`
    // (Fix 2 from /review-loop) so the wave hot path no longer re-parses
    // disk every iteration.
    let extra_implicit: Vec<String> = params
        .project_config
        .implicit_overlap_files
        .iter()
        .cloned()
        .chain(params.prd_implicit_overlap_files.iter().cloned())
        .collect();

    // FEAT-004: cross-wave file affinity. Enumerate `{branch}-slot-N` ephemeral
    // branches once per wave on the main thread (slot worker threads from the
    // prior wave have already joined per the engine's join discipline) and
    // collect their un-merged files. The (branch, files) pairs become synthetic
    // claims for `select_parallel_group`, which then defers any candidate
    // whose `touchesFiles` would conflict with un-merged work.
    //
    // `git diff` is called once per ephemeral branch (NOT per candidate) so
    // the overhead is bounded by `parallel_slots`. Each per-branch failure
    // logs and degrades to "no claim" — selection continues without the
    // overlay rather than crashing the loop (per FEAT-004 failure-mode AC).
    let ephemeral_branches =
        worktree::list_ephemeral_slot_branches(params.source_root, params.branch);
    let ephemeral_overlay: Vec<(String, Vec<String>)> = ephemeral_branches
        .iter()
        .map(|eph| {
            let files = match worktree::list_unmerged_branch_files(
                params.source_root,
                params.branch,
                eph,
            ) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "Warning: list_unmerged_branch_files({}) failed: {} (treating as no claim)",
                        eph, e
                    );
                    Vec::new()
                }
            };
            (eph.clone(), files)
        })
        .collect();

    let result = match select_parallel_group(
        params.conn,
        &ctx.last_files,
        params.task_prefix,
        params.parallel_slots,
        &extra_implicit,
        &ephemeral_overlay,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "Warning: select_parallel_group failed: {} (treating wave as stale)",
                e
            );
            crate::commands::next::selection::ParallelGroupResult::default()
        }
    };

    let group = result.group;
    if group.is_empty() {
        // FEAT-004 deadlock guard: a non-empty diagnostic means every eligible
        // candidate was blocked exclusively by un-merged ephemeral work. Surface
        // the per-candidate breakdown to stderr and synthesize a merge-fail
        // wave so the FEAT-002 reset/halt-check contract fires — without this,
        // the loop would spin until stale-iteration abort with no signal to
        // the operator about what's blocking forward progress.
        if !result.ephemeral_block_diagnostics.is_empty() {
            return handle_ephemeral_deadlock(&params, ctx, result.ephemeral_block_diagnostics);
        }
        return handle_no_eligible_tasks(&params, ctx);
    }

    // Selected at least one eligible task → reset stale tracker (mirrors
    // the sequential `else` branch that calls `check("a", "b")`).
    ctx.stale_tracker.check("a", "b");

    let n_slots = group.len();
    let slot_paths: &[PathBuf] = &params.slot_worktree_paths[..n_slots];
    let slot_prompt_params = build_slot_prompt_params(&params);
    let mut slot_contexts =
        build_slot_contexts(params.conn, group, slot_paths, &slot_prompt_params);
    // FEAT-005: resolve effective runner per slot on the main thread before
    // spawning the worker. Single source of truth for the formula — slots
    // never read `ctx.runner_overrides` directly (Learning #1810). The
    // effective_model mirrors what `run_slot_iteration` recomputes internally
    // (bundle.resolved_model → params.default_model fallback) so the
    // runner resolution sees the same model string the slot will use.
    for slot in slot_contexts.iter_mut() {
        // FEAT-008: operator escape valve — clear stale overrides if tasks.model changed.
        check_override_invalidation(ctx, params.conn, &slot.prompt_bundle.task_id);
        // FEAT-002: route review-class slots to `reviewModel` BEFORE recomputing
        // `effective_model`. Mutating the bundle's `resolved_model` (not just a
        // local) keeps runner selection, the `--model` flag in
        // `run_slot_iteration`, and the prompt-baked model consistent — a
        // drift-`assert!` cross-check on `slot_result.effective_runner` would
        // panic if these disagreed.
        if let Some(review_model_override) = apply_review_model_override(
            params.project_config.review_model.as_deref(),
            &slot.prompt_bundle.task_id,
        ) {
            let old = slot
                .prompt_bundle
                .resolved_model
                .as_deref()
                .unwrap_or("(default)");
            eprintln!(
                "Review-class routing [slot {}]: {} → {} (reviewModel)",
                slot.slot_index, old, review_model_override,
            );
            slot.prompt_bundle.resolved_model = Some(review_model_override);
        }
        let effective_model = slot
            .prompt_bundle
            .resolved_model
            .as_deref()
            .or(params.default_model);
        slot.effective_runner =
            resolve_effective_runner(ctx, &slot.prompt_bundle.task_id, effective_model);
    }
    let slot_params = build_shared_slot_params(&params);

    // Run wave (blocks until every spawned slot thread joins).
    let mut wave_result = run_parallel_wave(params.conn, slot_contexts, slot_params);

    // Per-slot post-processing on the main thread. The pipeline mutates each
    // slot's `IterationOutcome` in place when retroactive completion is
    // detected, so the iteration borrows mutably.
    let mut agg = WaveAggregator::new(wave_result.outcomes.len());
    for slot_result in &mut wave_result.outcomes {
        process_slot_result(slot_result, &mut params, ctx, &mut agg);
    }

    // FEAT-007: post-wave retry tracking. Mirrors the sequential call site in
    // `run_loop` (engine.rs:~3992) so wave-mode RuntimeError slots feed the
    // `consecutive_failures` counter, the Claude-tier escalation ladder, AND
    // the new Grok promotion hook on the main thread (Learning #1810:
    // IterationContext is not thread-safe; this MUST NOT run inside a slot
    // worker). Skips claim-fail entries (`claim_succeeded == false`) so a
    // synthetic crash record never increments the counter on a task that
    // never actually executed. Crash(GrokAuthFailure) short-circuits — auth
    // lapses are operator problems, not task failures.
    for slot_result in &wave_result.outcomes {
        if !slot_result.claim_succeeded {
            continue;
        }
        let Some(ref task_id) = slot_result.iteration_result.task_id else {
            continue;
        };
        if matches!(
            slot_result.iteration_result.outcome,
            IterationOutcome::Completed
                | IterationOutcome::Empty
                | IterationOutcome::Reorder(_)
                | IterationOutcome::RateLimit
                | IterationOutcome::Crash(config::CrashType::GrokAuthFailure)
        ) {
            continue;
        }
        if let Err(e) = handle_task_failure(
            params.conn,
            task_id,
            params.iteration as i64,
            ctx,
            params.project_config.fallback_runner.as_ref(),
        ) {
            eprintln!(
                "Warning: failed to start retry tracking transaction for slot {} task {}: {}",
                slot_result.slot_index, task_id, e
            );
        }
    }

    // CrashTracker policy per FEAT-010 AC.
    if agg.any_completed {
        ctx.crash_tracker.record_success();
    } else if agg.all_crashed {
        ctx.crash_tracker.record_crash();
    }

    // Feed the next wave's locality scoring.
    ctx.last_files = agg.aggregated_files.clone();

    // Merge ephemeral slot branches back into the main loop branch so the
    // next wave starts from a unified base. No-op when parallel_slots <= 1.
    // Per-slot failures (merge conflicts, spawn errors, ff failures) are
    // logged but never fatal — slot 0 is restored to its captured pre-merge
    // HEAD via `git reset --hard`, and the next wave still benefits from any
    // slots that did merge. The wave-loop boundary in `run_loop` consumes
    // `failed_merges` to drive the FEAT-002 reset/halt-check contract.
    let mut failed_merges: Vec<FailedMerge> = Vec::new();
    if params.parallel_slots > 1 {
        let resolved_model = params
            .default_model
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(model::SONNET_MODEL)
            .to_string();
        // Per-project overrides for the merge-conflict resolver. Both fall
        // back to safe defaults so projects without a config.json keep the
        // pre-existing behavior (600s / "medium"). The shared `project_config`
        // was loaded once at run-loop startup and threaded through
        // `WaveParams` (Fix 2 from /review-loop).
        let claude_timeout = Duration::from_secs(
            params
                .project_config
                .merge_resolver_timeout_secs
                .unwrap_or(600),
        );
        let effort = params
            .project_config
            .merge_resolver_effort
            .clone()
            .unwrap_or_else(|| "medium".to_string());
        let resolver = merge_resolver::ClaudeMergeResolver {
            model: resolved_model,
            db_dir: Some(params.db_dir),
            signal_flag: Some(params.signal_flag),
            claude_timeout,
            effort,
        };
        let outcomes = worktree::merge_slot_branches_with_resolver(
            params.source_root,
            params.branch,
            params.parallel_slots,
            &resolver,
            params.slot_worktree_paths,
            params.run_id,
            params.project_config.slot_stash_limit,
        );
        for (slot, detail, kind) in &outcomes.failed_slots {
            if *kind == worktree::SlotFailureKind::ResolverAttempted {
                eprintln!(
                    "Warning: slot {} merge-back failed after Claude resolution attempt: {} (slot's commits remain on its ephemeral branch)",
                    slot, detail
                );
            } else {
                eprintln!(
                    "Warning: slot {} merge-back failed: {} (slot's commits remain on its ephemeral branch)",
                    slot, detail
                );
            }
            // Capture failed slot index alongside the task_id its slot had
            // claimed. The `FailedMerge` shape keeps the pairing as one
            // value so downstream consumers can't accidentally zip with a
            // mismatched length.
            let task_id = wave_result
                .outcomes
                .iter()
                .find(|o| o.slot_index == *slot)
                .and_then(|o| o.iteration_result.task_id.clone());
            failed_merges.push(FailedMerge {
                slot: *slot,
                task_id,
            });
        }

        // FEAT-006: fold each slot's untracked progress file into slot 0's so
        // the operator sees one unified, wave-separated view by wave end.
        // Runs after merge-back, before cleanup_slot_worktrees, so slot 1+
        // paths still exist. A union failure never blocks the wave.
        if let Err(e) = worktree::union_slot_progress_files(
            &params.slot_worktree_paths[..n_slots],
            &branch::progress_file_name(params.task_prefix),
        ) {
            eprintln!("Warning: failed to union slot progress files: {}", e);
        }

        // FEAT-003: post-merge reconcile. Scan slot 0's `{pre..HEAD}` for
        // `<TASK-ID>-completed` markers from slot agents whose subprocess
        // exited before flushing `<completed>` (output drop, watchdog kill,
        // deadline). Must run BEFORE `run_cmd::update` AND BEFORE the
        // external-git `tasks_completed` shadow below — otherwise the shadow
        // captures a stale agg and the terminal returns report the wrong
        // counter. Single-threaded within the loop, so the `retain` on
        // `pending_slot_tasks` cannot race another wave.
        if let (Some(pre), Some(slot0)) = (
            outcomes.pre_merge_head.as_deref(),
            params.slot_worktree_paths.first(),
        ) {
            apply_post_merge_reconcile(
                slot0,
                pre,
                params.conn,
                params.run_id,
                params.prd_path,
                params.task_prefix,
                ctx,
                &mut agg,
            );
        }
    }

    if let Err(e) = run_cmd::update(
        params.conn,
        params.run_id,
        ctx.last_commit.as_deref(),
        Some(&agg.aggregated_files),
    ) {
        eprintln!("Warning: failed to update run: {}", e);
    }

    // Per-wave external-git reconciliation. Mirrors the sequential
    // "Post-iteration: reconcile external git completions" step.
    let mut tasks_completed = agg.tasks_completed;
    if let Some(ext_repo) = params.external_repo_path {
        let count = reconcile_external_git_completions(
            ext_repo,
            params.conn,
            params.run_id,
            params.prd_path,
            params.task_prefix,
            params.external_git_scan_depth,
        );
        if count > 0 {
            tasks_completed += count as u32;
            agg.any_completed = true;
            ctx.crash_tracker.record_success();
            eprintln!("Post-wave reconciliation: marked {} task(s) done", count);
        }
    }

    // Terminal conditions: signal precedes everything; crash abort wins
    // over completion-of-all-tasks because the abort is a hard stop.
    if params.signal_flag.is_signaled() {
        return WaveOutcome {
            tasks_completed,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 130,
                reason: "signal received".to_string(),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges,
        };
    }
    if ctx.crash_tracker.should_abort() {
        return WaveOutcome {
            tasks_completed,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 1,
                reason: "too many crashes".to_string(),
                run_status: None,
            }),
            was_stopped: false,
            failed_merges,
        };
    }

    if agg.any_completed && count_remaining_active_tasks(params.conn, params.task_prefix) == 0 {
        return WaveOutcome {
            tasks_completed,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: 0,
                reason: "all tasks complete".to_string(),
                run_status: Some(RunStatus::Completed),
            }),
            was_stopped: false,
            failed_merges,
        };
    }

    let mut wave_should_stop = agg.wave_should_stop;
    if !wave_should_stop && wait_inter_wave_delay(params.inter_iteration_delay, params.signal_flag)
    {
        wave_should_stop = true;
    }

    WaveOutcome {
        tasks_completed,
        iteration_consumed: true,
        terminal: if wave_should_stop {
            if params.signal_flag.is_signaled() {
                Some(WaveTerminal {
                    exit_code: 130,
                    reason: "signal received".to_string(),
                    run_status: None,
                })
            } else {
                Some(WaveTerminal {
                    exit_code: 0,
                    reason: "stop signal".to_string(),
                    run_status: None,
                })
            }
        } else {
            None
        },
        was_stopped: false,
        failed_merges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Defense layer #1 (slot-path threading) regression guard — NEW in the
    /// FEAT-003 carve (the pre-carve code lacked it).
    ///
    /// The `&[PathBuf]` slice that `ensure_slot_worktrees` returns (carried by
    /// `WaveIterationParams::slot_worktree_paths`) MUST be threaded straight
    /// into `merge_slot_branches_with_resolver`; slot 0's path must NEVER be
    /// recomputed via `compute_slot_worktree_path(_, branch, 0)`. The
    /// recomputation diverges when the loop runs from inside the matching
    /// worktree (slot 0 IS the project root), which is exactly the
    /// recomputed-slot-path ENOENT that produced the mw-datalake cascade.
    ///
    /// The nested `_thread` fn is the type-level half: it only compiles if
    /// `WaveIterationParams::slot_worktree_paths` is `&[PathBuf]` — the same
    /// type `merge_slot_branches_with_resolver` consumes as `slot_paths`. The
    /// source-grep half asserts `run_wave_iteration` actually threads the field
    /// and never recomputes a slot path.
    #[test]
    fn defense_layer_1_slot_paths_threaded_not_recomputed() {
        // Type-level: drift on the field type breaks compilation here.
        #[allow(dead_code)]
        fn _thread<'a>(p: &'a WaveIterationParams<'a>) -> &'a [PathBuf] {
            p.slot_worktree_paths
        }

        let src = std::fs::read_to_string("src/loop_engine/wave_scheduler.rs")
            .expect("could not read src/loop_engine/wave_scheduler.rs from package root");
        let start = src
            .find("pub fn run_wave_iteration(")
            .expect("expected `pub fn run_wave_iteration(` in wave_scheduler.rs");
        // Bound the slice to the function body — `run_wave_iteration` is the
        // last item before the test module, whose doc comment intentionally
        // names `compute_slot_worktree_path` and would otherwise poison the
        // negative assertion below.
        let after = &src[start..];
        let end = after.find("\n#[cfg(test)]").unwrap_or(after.len());
        let body = &after[..end];

        assert!(
            body.contains("params.slot_worktree_paths"),
            "run_wave_iteration MUST thread WaveIterationParams::slot_worktree_paths \
             into merge_slot_branches_with_resolver (defense layer #1)",
        );
        assert!(
            !body.contains("compute_slot_worktree_path("),
            "run_wave_iteration MUST NOT recompute a slot path \
             (defense layer #1 cause-fix — slot 0's path is threaded, not recomputed)",
        );
    }
}
