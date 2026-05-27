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
//! `process_slot_result`, `slot_failure_result`), `recovery.rs` (FEAT-002:
//! `handle_task_failure`, `check_override_invalidation`),
//! `iteration_pipeline.rs` (the shared post-Claude pipeline invoked inside
//! `slot.rs::process_slot_result` — not called directly here, but every wave
//! iteration result flows through it), and `worktree.rs` (merge-back via
//! `merge_slot_branches_with_resolver`, ephemeral branch hygiene, stash
//! preflight, and `union_slot_progress_files`).
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
use crate::loop_engine::prd_reconcile::reconcile_passes_with_db;
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

/// No eligible tasks were selected for this wave.
///
/// Before counting an empty selection as a stale iteration, this mirrors the
/// two checks the sequential path runs when `build_prompt` returns `Ok(None)`
/// (`run_iteration`, the `remaining == 0` exit and the auto-recovery sweep) —
/// behaviors the wave path previously lacked, which let a fully-completed PRD
/// abort with the misleading "no eligible tasks after N consecutive stale
/// iterations" (exit 1) instead of exiting cleanly:
///
/// 1. **Queue drained → terminal exit.** If no schedulable tasks remain, return
///    a terminal outcome via `classify_drained_queue` (the shared
///    sequential/wave predicate): exit 0 when only done/irrelevant remain,
///    exit 1 when `blocked`/`skipped` work was left stuck. Same classifier used
///    by the all-complete exit at the bottom of `run_wave_iteration`.
/// 2. **Stranded `in_progress` recovery.** Reconcile any PRD `passes: true`
///    rows the DB never marked done, then reset tasks a prior wave left in
///    `in_progress` (merge-back failure / completion-detection gap). Safe here:
///    selection runs on the main thread before any slot spawns, and the prior
///    wave's slot threads have all joined, so nothing is legitimately
///    mid-flight. If recovery frees anything, retry next wave WITHOUT touching
///    the stale tracker — progress is now possible. A *static* stranding
///    self-corrects: the next empty-group pass recovers nothing and falls
///    through to the stale path. A task that is repeatedly claimed then
///    re-stranded would not advance the stale counter, but it is bounded by
///    `max_iterations` (and, for the merge-back-failure variant, by the FEAT-002
///    consecutive-merge-fail halt threshold).
///
/// Only a genuinely stuck queue (tasks remain, none eligible, nothing
/// recoverable) drives the stale tracker, returning a terminal outcome when the
/// tracker tripped its abort threshold.
fn handle_no_eligible_tasks(
    params: &mut WaveIterationParams<'_>,
    ctx: &mut IterationContext,
) -> WaveOutcome {
    let task_prefix = params.task_prefix;
    let prd_path = params.prd_path;

    // (1) Queue genuinely drained → terminal completion exit, NOT a stale
    // failure. The classifier separates a fully-successful drain (exit 0) from
    // one where tasks were left blocked/skipped (non-zero exit, named reason)
    // so the loop-end banner is honest.
    if let Some(drained) = classify_drained_queue(params.conn, task_prefix) {
        progress::log_iteration(
            params.progress_path,
            params.iteration,
            None,
            &IterationOutcome::Completed,
            &[],
            None,
            None,
            None,
        );
        return WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: drained.exit_code,
                reason: drained.reason,
                run_status: Some(drained.run_status),
            }),
            was_stopped: false,
            failed_merges: Vec::new(),
        };
    }

    // (2) Tasks remain but none are eligible → attempt auto-recovery before
    // counting this as stale.
    reconcile_passes_with_db(params.conn, prd_path, task_prefix);
    let recovered = TaskLifecycle::new(params.conn)
        .recover_in_progress_for_prefix(task_prefix)
        .unwrap_or(0);
    if recovered > 0 {
        eprintln!(
            "Auto-recovered {} stale in_progress task(s), retrying task selection next wave...",
            recovered
        );
        return WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: None,
            was_stopped: false,
            failed_merges: Vec::new(),
        };
    }

    // (3) Genuinely stuck: nothing eligible, nothing recoverable. Count toward
    // the stale-abort threshold exactly as before.
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
pub(crate) fn count_remaining_active_tasks(conn: &Connection, task_prefix: Option<&str>) -> i64 {
    let (clause, param) = prefix_and(task_prefix);
    // `archived_at IS NULL` is mandatory: archiving stamps `archived_at` on all
    // prefix-matched rows regardless of status (archive.rs), so an archived
    // todo/in_progress row would otherwise count as remaining work and the wave
    // would never recognize completion. Matches the sequential predicate and is
    // locked by archive.rs::test_archived_tasks_invisible_to_status_count_query.
    let sql = format!(
        "SELECT COUNT(*) FROM tasks WHERE status NOT IN \
         ('done','irrelevant','skipped','blocked') AND archived_at IS NULL {clause}"
    );
    let p_vec: Vec<&dyn rusqlite::types::ToSql> = match &param {
        Some(p) => vec![p],
        None => vec![],
    };
    conn.query_row(&sql, p_vec.as_slice(), |r| r.get(0))
        .unwrap_or(0)
}

/// Count tasks in a single terminal-but-unfinished `status` for the current PRD
/// prefix (`blocked` or `skipped`). Both are excluded from
/// `count_remaining_active_tasks` (not schedulable) but, unlike
/// `done`/`irrelevant`, represent work that did not finish — the drain
/// classifier uses them to avoid reporting a clean "all tasks complete".
/// `archived_at IS NULL` — see `count_remaining_active_tasks`.
fn count_tasks_in_status(conn: &Connection, task_prefix: Option<&str>, status: &str) -> i64 {
    let (clause, param) = prefix_and(task_prefix);
    let sql =
        format!("SELECT COUNT(*) FROM tasks WHERE status = ?1 AND archived_at IS NULL {clause}");
    let mut p_vec: Vec<&dyn rusqlite::types::ToSql> = vec![&status];
    if let Some(p) = &param {
        p_vec.push(p);
    }
    conn.query_row(&sql, p_vec.as_slice(), |r| r.get(0))
        .unwrap_or(0)
}

/// Terminal outcome for a loop that finds no schedulable tasks. Shared by the
/// sequential and wave paths so the "what counts as clean completion vs stuck"
/// predicate lives in exactly one place (see `src/loop_engine/CLAUDE.md`).
pub(crate) struct DrainedOutcome {
    pub(crate) exit_code: i32,
    pub(crate) reason: String,
    pub(crate) run_status: RunStatus,
}

/// Classify the loop-end state when no *schedulable* tasks remain (no `todo` /
/// `in_progress` rows — `count_remaining_active_tasks == 0`).
///
/// Returns `None` while schedulable work is still in flight (the caller should
/// keep looping / recovering). When the queue is drained, distinguishes:
///
/// - **Fully successful** — every remaining row is `done`/`irrelevant`: exit 0,
///   `RunStatus::Completed`, reason "all tasks complete".
/// - **Stuck** — at least one `blocked` and/or `skipped` row remains: a
///   non-zero exit, `RunStatus::Aborted`, and a reason naming the counts plus a
///   `task-mgr review` hint. `skipped` is treated as unfinished work (not a
///   clean success) — a deliberate product decision so neither execution path
///   claims completion while deferred work is outstanding.
pub(crate) fn classify_drained_queue(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> Option<DrainedOutcome> {
    if count_remaining_active_tasks(conn, task_prefix) != 0 {
        return None;
    }
    let blocked = count_tasks_in_status(conn, task_prefix, "blocked");
    let skipped = count_tasks_in_status(conn, task_prefix, "skipped");
    if blocked == 0 && skipped == 0 {
        return Some(DrainedOutcome {
            exit_code: 0,
            reason: "all tasks complete".to_string(),
            run_status: RunStatus::Completed,
        });
    }
    let mut parts = Vec::new();
    if blocked > 0 {
        parts.push(format!("{blocked} blocked"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    Some(DrainedOutcome {
        exit_code: 1,
        reason: format!(
            "no schedulable tasks remain ({}) — run `task-mgr review` to resolve",
            parts.join(", ")
        ),
        run_status: RunStatus::Aborted,
    })
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
        return handle_no_eligible_tasks(&mut params, ctx);
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
            params.project_config.primary_runner.as_ref(),
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

    if agg.any_completed
        && let Some(drained) = classify_drained_queue(params.conn, params.task_prefix)
    {
        return WaveOutcome {
            tasks_completed,
            iteration_consumed: true,
            terminal: Some(WaveTerminal {
                exit_code: drained.exit_code,
                reason: drained.reason,
                run_status: Some(drained.run_status),
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

    // ---------------------------------------------------------------
    // Wave + merge-back + post-merge tests (moved from orchestrator.rs)
    // ---------------------------------------------------------------

    use crate::loop_engine::config::PermissionMode;
    use crate::loop_engine::engine::{
        IterationContext, SlotContext, SlotIterationParams, WaveAggregator, WaveOutcome,
    };
    use crate::loop_engine::prompt::slot::{SlotPromptBundle, SlotPromptParams};
    use crate::loop_engine::runner::RunnerKind;
    use crate::loop_engine::signals::SignalFlag;
    use crate::loop_engine::test_utils::{
        get_task_status, insert_relationship, insert_run, insert_task, insert_task_file,
        setup_git_repo, setup_test_db,
    };
    use crate::models::Task;
    use rusqlite::Connection;
    use std::path::Path;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// Opt every task out of the FEAT-003 buildy shared-infra heuristic.
    fn opt_out_buildy(conn: &Connection) {
        conn.execute("UPDATE tasks SET claims_shared_infra = 0", [])
            .unwrap();
    }

    /// Build a minimal SlotIterationParams wired to a test DB.
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

    /// Synthesize a `SlotPromptBundle` directly without invoking `build_prompt`.
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

    #[allow(clippy::too_many_arguments)]
    fn make_wave_params<'a>(
        conn: &'a mut Connection,
        db_dir: &'a Path,
        source_root: &'a Path,
        branch: &'a str,
        slot_paths: &'a [std::path::PathBuf],
        base_prompt: &'a Path,
        permission_mode: &'a PermissionMode,
        signal_flag: &'a SignalFlag,
        tasks_dir: &'a Path,
        prd_path: &'a Path,
        progress_path: &'a Path,
        parallel_slots: usize,
        project_config: &'a crate::loop_engine::project_config::ProjectConfig,
        prd_implicit_overlap_files: &'a [String],
    ) -> WaveIterationParams<'a> {
        WaveIterationParams {
            conn,
            db_dir,
            source_root,
            branch,
            parallel_slots,
            slot_worktree_paths: slot_paths,
            iteration: 1,
            max_iterations: 1,
            elapsed_secs: 0,
            run_id: "test-run",
            base_prompt_path: base_prompt,
            permission_mode,
            signal_flag,
            default_model: None,
            verbose: false,
            task_prefix: None,
            prd_path,
            progress_path,
            tasks_dir,
            external_repo_path: None,
            external_git_scan_depth: 50,
            inter_iteration_delay: Duration::ZERO,
            steering_path: None,
            session_guidance: "",
            prd_implicit_overlap_files,
            project_config,
        }
    }

    // --- WaveResult fields ---

    #[test]
    fn test_wave_result_fields() {
        let wr = WaveResult {
            outcomes: vec![],
            wave_duration: Duration::from_millis(10),
        };
        assert!(wr.outcomes.is_empty());
        assert!(wr.wave_duration >= Duration::from_millis(10));
    }

    // --- run_parallel_wave: orchestration + panic handling (AC 9, 10, 11, 12) ---

    #[test]
    fn test_run_parallel_wave_empty_slots_returns_empty_outcomes() {
        let (temp, mut conn) = setup_test_db();

        let params = Arc::new(make_slot_params(temp.path(), SignalFlag::new()));
        let wave = run_parallel_wave(&mut conn, vec![], params);
        assert!(wave.outcomes.is_empty());
    }

    #[test]
    fn test_run_parallel_wave_reports_claim_failure_for_done_task() {
        let (temp, mut conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();

        // Task is already done — claim_slot_task returns false; wave must
        // emit a Crash(RuntimeError) entry rather than silently drop the slot.
        insert_task(&conn, "FEAT-DONE", "t", "done", 10);

        let signal = SignalFlag::new();
        // Pre-signal so if the claim logic regresses and still spawns Claude,
        // the slot's early-signal check bails before touching the network.
        signal.set();
        let params = Arc::new(make_slot_params(temp.path(), signal));

        let slot = make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-DONE"));
        let wave = run_parallel_wave(&mut conn, vec![slot], params);
        assert_eq!(wave.outcomes.len(), 1);
        assert_eq!(wave.outcomes[0].slot_index, 0);
        assert!(matches!(
            wave.outcomes[0].iteration_result.outcome,
            IterationOutcome::Crash(_)
        ));
        assert_eq!(
            wave.outcomes[0].iteration_result.task_id.as_deref(),
            Some("FEAT-DONE"),
        );
    }

    #[test]
    fn test_run_parallel_wave_claims_all_tasks_before_spawning() {
        // Main-thread claim must flip every task to in_progress before any
        // slot thread runs. We verify by pre-signaling so slot threads bail
        // immediately; the DB must still show in_progress for both tasks.
        let (temp, mut conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();

        insert_task(&conn, "FEAT-A", "a", "todo", 10);
        insert_task(&conn, "FEAT-B", "b", "todo", 10);

        let signal = SignalFlag::new();
        signal.set();
        let params = Arc::new(make_slot_params(temp.path(), signal));

        let slot_a = make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-A"));
        let slot_b = make_slot(1, tmp.path().to_path_buf(), dummy_bundle("FEAT-B"));

        let wave = run_parallel_wave(&mut conn, vec![slot_a, slot_b], params);
        assert_eq!(wave.outcomes.len(), 2);

        let status_a: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-A'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let status_b: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-B'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status_a, "in_progress");
        assert_eq!(status_b, "in_progress");
    }

    #[test]
    fn test_run_parallel_wave_outcomes_sorted_by_slot_index() {
        // Mix claim-failure and successful pre-signal early-exits to force
        // the reorder path. Outcomes must always emerge slot-ordered.
        let (temp, mut conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();

        insert_task(&conn, "FEAT-A", "a", "todo", 10);
        insert_task(&conn, "FEAT-B", "b", "done", 10); // claim fails
        insert_task(&conn, "FEAT-C", "c", "todo", 10);

        let signal = SignalFlag::new();
        signal.set();
        let params = Arc::new(make_slot_params(temp.path(), signal));

        let slots = vec![
            make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-A")),
            make_slot(1, tmp.path().to_path_buf(), dummy_bundle("FEAT-B")),
            make_slot(2, tmp.path().to_path_buf(), dummy_bundle("FEAT-C")),
        ];

        let wave = run_parallel_wave(&mut conn, slots, params);
        assert_eq!(wave.outcomes.len(), 3);
        assert_eq!(wave.outcomes[0].slot_index, 0);
        assert_eq!(wave.outcomes[1].slot_index, 1);
        assert_eq!(wave.outcomes[2].slot_index, 2);
    }

    #[test]
    fn test_run_parallel_wave_measures_wall_clock_duration() {
        let (temp, mut conn) = setup_test_db();

        let params = Arc::new(make_slot_params(temp.path(), SignalFlag::new()));
        let wave = run_parallel_wave(&mut conn, vec![], params);
        // Empty wave still records a non-negative duration; ensures the
        // Instant::now() → elapsed() contract holds.
        assert!(wave.wave_duration < Duration::from_secs(5));
    }

    // --- run_wave_iteration: dispatch & policy ---

    #[test]
    fn test_run_wave_iteration_pre_set_signal_returns_terminal_signal() {
        let (temp, mut conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "base").unwrap();
        let prd = tmp.path().join("prd.json");
        let progress = tmp.path().join("progress.txt");
        let mode = PermissionMode::Dangerous;
        let signal = SignalFlag::new();
        signal.set();
        let mut ctx = IterationContext::new(5);
        let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
        let prd_implicit: Vec<String> = Vec::new();
        let outcome = run_wave_iteration(
            make_wave_params(
                &mut conn,
                temp.path(),
                tmp.path(),
                "main",
                &[],
                &base_prompt,
                &mode,
                &signal,
                tmp.path(),
                &prd,
                &progress,
                2,
                &project_cfg,
                &prd_implicit,
            ),
            &mut ctx,
        );
        assert!(matches!(
            outcome.terminal,
            Some(WaveTerminal { exit_code: 130, .. })
        ));
        assert!(!outcome.iteration_consumed);
        assert_eq!(outcome.tasks_completed, 0);
    }

    #[test]
    fn test_run_wave_iteration_no_eligible_tasks_increments_stale_tracker() {
        let (temp, mut conn) = setup_test_db();
        // Genuinely-stuck queue: FEAT-B (todo) dependsOn FEAT-A (blocked).
        // FEAT-A is terminal so it isn't a candidate; FEAT-B's dependency is
        // unsatisfied so it isn't eligible → empty group, but FEAT-B keeps
        // `count_remaining_active_tasks` non-zero so the all-complete and
        // recovery short-circuits don't fire and we exercise the stale path.
        insert_task(&conn, "FEAT-A", "Blocked dep", "blocked", 10);
        insert_task(&conn, "FEAT-B", "Dependent", "todo", 20);
        insert_relationship(&conn, "FEAT-B", "FEAT-A", "dependsOn");
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "base").unwrap();
        let prd = tmp.path().join("prd.json");
        let progress = tmp.path().join("progress.txt");
        let mode = PermissionMode::Dangerous;
        let signal = SignalFlag::new();
        let mut ctx = IterationContext::new(5);
        let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
        let prd_implicit: Vec<String> = Vec::new();
        let outcome = run_wave_iteration(
            make_wave_params(
                &mut conn,
                temp.path(),
                tmp.path(),
                "main",
                &[],
                &base_prompt,
                &mode,
                &signal,
                tmp.path(),
                &prd,
                &progress,
                2,
                &project_cfg,
                &prd_implicit,
            ),
            &mut ctx,
        );
        // Empty DB → empty group → wave consumes the iteration but does
        // not flag terminal; stale_tracker bumps so 3 such waves abort.
        assert!(outcome.terminal.is_none());
        assert!(outcome.iteration_consumed);
        assert_eq!(ctx.stale_tracker.count(), 1);
        // log_iteration must have written a NoEligibleTasks entry.
        let log = std::fs::read_to_string(&progress).unwrap();
        assert!(log.contains("NoEligibleTasks"), "got: {log}");
    }

    #[test]
    fn test_run_wave_iteration_third_no_eligible_wave_aborts_via_stale() {
        let (temp, mut conn) = setup_test_db();
        // Genuinely-stuck queue (see sibling test): a todo task whose only
        // dependency is blocked. Keeps the queue non-empty so the stale
        // path — not the all-complete short-circuit — is exercised.
        insert_task(&conn, "FEAT-A", "Blocked dep", "blocked", 10);
        insert_task(&conn, "FEAT-B", "Dependent", "todo", 20);
        insert_relationship(&conn, "FEAT-B", "FEAT-A", "dependsOn");
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "base").unwrap();
        let prd = tmp.path().join("prd.json");
        let progress = tmp.path().join("progress.txt");
        let mode = PermissionMode::Dangerous;
        let signal = SignalFlag::new();
        let mut ctx = IterationContext::new(5);
        // Pre-stale twice so the next NoEligibleTasks wave hits threshold=3.
        ctx.stale_tracker.check("x", "x");
        ctx.stale_tracker.check("x", "x");
        let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
        let prd_implicit: Vec<String> = Vec::new();
        let outcome = run_wave_iteration(
            make_wave_params(
                &mut conn,
                temp.path(),
                tmp.path(),
                "main",
                &[],
                &base_prompt,
                &mode,
                &signal,
                tmp.path(),
                &prd,
                &progress,
                2,
                &project_cfg,
                &prd_implicit,
            ),
            &mut ctx,
        );
        let t = outcome.terminal.expect("terminal expected");
        assert_eq!(t.exit_code, 1);
        assert!(t.reason.contains("no eligible tasks"), "got: {}", t.reason);
        assert!(t.run_status.is_none());
    }

    /// Regression: a wave that selects no eligible tasks because the queue
    /// is fully and successfully drained (only done/irrelevant) must exit
    /// cleanly as "all tasks complete" (exit 0, RunStatus::Completed) — NOT
    /// spin into the stale tracker and abort with exit 1. Mirrors the
    /// sequential `remaining == 0 → "All tasks complete!"` exit that the
    /// wave path previously lacked.
    #[test]
    fn test_run_wave_iteration_drained_queue_exits_complete_not_stale() {
        let (temp, mut conn) = setup_test_db();
        // Every row terminal AND successful: done + irrelevant only.
        insert_task(&conn, "FEAT-A", "Done", "done", 10);
        insert_task(&conn, "FEAT-D", "Irrelevant", "irrelevant", 40);
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "base").unwrap();
        let prd = tmp.path().join("prd.json");
        let progress = tmp.path().join("progress.txt");
        let mode = PermissionMode::Dangerous;
        let signal = SignalFlag::new();
        let mut ctx = IterationContext::new(5);
        let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
        let prd_implicit: Vec<String> = Vec::new();
        let outcome = run_wave_iteration(
            make_wave_params(
                &mut conn,
                temp.path(),
                tmp.path(),
                "main",
                &[],
                &base_prompt,
                &mode,
                &signal,
                tmp.path(),
                &prd,
                &progress,
                2,
                &project_cfg,
                &prd_implicit,
            ),
            &mut ctx,
        );
        let t = outcome.terminal.expect("terminal expected");
        assert_eq!(t.exit_code, 0, "drained queue must exit 0, got {t:?}");
        assert_eq!(t.reason, "all tasks complete");
        assert!(matches!(t.run_status, Some(RunStatus::Completed)));
        // Crucially, the stale tracker was NOT touched.
        assert_eq!(ctx.stale_tracker.count(), 0);
    }

    /// A drained queue that still has `blocked` and/or `skipped` tasks must
    /// NOT report a clean "all tasks complete". The classifier downgrades it
    /// to a non-zero exit with `RunStatus::Aborted` and a reason naming the
    /// counts, so the loop-end banner is honest about stuck/deferred work.
    /// `skipped` downgrades just like `blocked` (product decision).
    #[test]
    fn test_run_wave_iteration_drained_with_blocked_downgrades() {
        let (temp, mut conn) = setup_test_db();
        insert_task(&conn, "FEAT-A", "Done", "done", 10);
        insert_task(&conn, "FEAT-C", "Blocked", "blocked", 30);
        insert_task(&conn, "FEAT-E", "Skipped", "skipped", 50);
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "base").unwrap();
        let prd = tmp.path().join("prd.json");
        let progress = tmp.path().join("progress.txt");
        let mode = PermissionMode::Dangerous;
        let signal = SignalFlag::new();
        let mut ctx = IterationContext::new(5);
        let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
        let prd_implicit: Vec<String> = Vec::new();
        let outcome = run_wave_iteration(
            make_wave_params(
                &mut conn,
                temp.path(),
                tmp.path(),
                "main",
                &[],
                &base_prompt,
                &mode,
                &signal,
                tmp.path(),
                &prd,
                &progress,
                2,
                &project_cfg,
                &prd_implicit,
            ),
            &mut ctx,
        );
        let t = outcome.terminal.expect("terminal expected");
        assert_eq!(t.exit_code, 1, "blocked/skipped must downgrade exit, got {t:?}");
        assert!(t.reason.contains("blocked"), "got: {}", t.reason);
        assert!(t.reason.contains("skipped"), "got: {}", t.reason);
        assert!(matches!(t.run_status, Some(RunStatus::Aborted)));
        assert_eq!(ctx.stale_tracker.count(), 0);
    }

    /// Regression: a wave that selects nothing because a prior wave left a
    /// task stranded in `in_progress` (merge-back / completion-detection
    /// gap) must auto-recover it to `todo` and retry next wave WITHOUT
    /// counting toward the stale-abort threshold. Mirrors the sequential
    /// auto-recovery sweep the wave path previously lacked.
    #[test]
    fn test_run_wave_iteration_recovers_stranded_in_progress_not_stale() {
        let (temp, mut conn) = setup_test_db();
        // No eligible todo candidate; the only active row is stranded
        // in_progress (not a selection candidate, so the group is empty).
        insert_task(&conn, "FEAT-A", "Stranded", "in_progress", 10);
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "base").unwrap();
        let prd = tmp.path().join("prd.json");
        let progress = tmp.path().join("progress.txt");
        let mode = PermissionMode::Dangerous;
        let signal = SignalFlag::new();
        let mut ctx = IterationContext::new(5);
        let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
        let prd_implicit: Vec<String> = Vec::new();
        let outcome = run_wave_iteration(
            make_wave_params(
                &mut conn,
                temp.path(),
                tmp.path(),
                "main",
                &[],
                &base_prompt,
                &mode,
                &signal,
                tmp.path(),
                &prd,
                &progress,
                2,
                &project_cfg,
                &prd_implicit,
            ),
            &mut ctx,
        );
        // Recovery path: iteration consumed, no terminal, stale untouched.
        assert!(
            outcome.terminal.is_none(),
            "recovery wave must not be terminal, got {:?}",
            outcome.terminal
        );
        assert!(outcome.iteration_consumed);
        assert_eq!(ctx.stale_tracker.count(), 0, "recovery must not bump stale");
        // The stranded task was reset to todo so the next wave can claim it.
        assert_eq!(get_task_status(&conn, "FEAT-A"), "todo");
    }

    #[test]
    fn test_run_wave_iteration_crash_should_abort_returns_terminal() {
        let (temp, mut conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "base").unwrap();
        let prd = tmp.path().join("prd.json");
        let progress = tmp.path().join("progress.txt");
        let mode = PermissionMode::Dangerous;
        let signal = SignalFlag::new();
        let mut ctx = IterationContext::new(1); // first crash aborts
        ctx.crash_tracker.record_crash();
        assert!(ctx.crash_tracker.should_abort());
        let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
        let prd_implicit: Vec<String> = Vec::new();
        let outcome = run_wave_iteration(
            make_wave_params(
                &mut conn,
                temp.path(),
                tmp.path(),
                "main",
                &[],
                &base_prompt,
                &mode,
                &signal,
                tmp.path(),
                &prd,
                &progress,
                2,
                &project_cfg,
                &prd_implicit,
            ),
            &mut ctx,
        );
        let t = outcome.terminal.expect("terminal expected");
        assert_eq!(t.exit_code, 1);
        assert!(t.reason.contains("too many crashes"), "got: {}", t.reason);
    }

    #[test]
    fn test_run_wave_iteration_signal_during_inter_wave_delay_returns_130() {
        // Regression: signal fired during inter-wave delay must return exit
        // code 130 ("signal received"), not 0 ("stop signal"), so operators
        // can distinguish SIGINT/SIGTERM from a clean .stop-file termination.
        //
        // Setup: point CLAUDE_BINARY at a nonexistent path so each slot
        // thread fails instantly (no real Claude spawn), letting run_parallel_wave
        // complete in microseconds.  Then the 500 ms delay starts, and the
        // background thread fires the signal at 100 ms — well inside the
        // delay window and well after steps 0-13 have already passed without
        // seeing the signal.
        let _env_lock = crate::loop_engine::test_utils::CLAUDE_BINARY_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env_guard = crate::loop_engine::test_utils::EnvGuard::set(
            "CLAUDE_BINARY",
            "/nonexistent_binary_for_test",
        );

        let (temp, mut conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "base").unwrap();
        let prd = tmp.path().join("prd.json");
        let progress = tmp.path().join("progress.txt");
        let mode = PermissionMode::Dangerous;

        // Insert an eligible task so select_parallel_group returns it and
        // run_parallel_wave actually spawns a slot thread (which fails fast
        // because CLAUDE_BINARY is invalid, so should_stop stays false and
        // step 13 sees no signal yet).
        insert_task(&conn, "FEAT-DELAY-SIGNAL", "delay signal test", "todo", 1);

        let signal = SignalFlag::new();
        let signal_clone = signal.clone();
        // Fire at 100 ms: steps 0-13 complete in < 10 ms (slot fails at process
        // spawn with ENOENT), so the signal always lands inside the 500 ms delay.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            signal_clone.set();
        });

        let mut ctx = IterationContext::new(5);
        let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
        let prd_implicit: Vec<String> = Vec::new();
        let outcome = run_wave_iteration(
            WaveIterationParams {
                conn: &mut conn,
                db_dir: temp.path(),
                source_root: tmp.path(),
                branch: "main",
                parallel_slots: 1,
                slot_worktree_paths: &[tmp.path().to_path_buf()],
                iteration: 1,
                max_iterations: 1,
                elapsed_secs: 0,
                run_id: "test-run",
                base_prompt_path: &base_prompt,
                permission_mode: &mode,
                signal_flag: &signal,
                default_model: None,
                verbose: false,
                task_prefix: None,
                prd_path: &prd,
                progress_path: &progress,
                tasks_dir: tmp.path(),
                external_repo_path: None,
                external_git_scan_depth: 50,
                inter_iteration_delay: Duration::from_millis(500),
                steering_path: None,
                session_guidance: "",
                prd_implicit_overlap_files: &prd_implicit,
                project_config: &project_cfg,
            },
            &mut ctx,
        );
        let t = outcome.terminal.expect("terminal expected");
        assert_eq!(
            t.exit_code, 130,
            "expected 130 for SIGINT during delay, got {}",
            t.exit_code
        );
        assert_eq!(t.reason, "signal received", "got: {}", t.reason);
    }

    #[test]
    fn test_iteration_context_initializes_pending_reorder_hints_empty() {
        let ctx = IterationContext::new(5);
        assert!(ctx.pending_reorder_hints.is_empty());
    }

    // --- FEAT-002 CONTRACT: build_bundle (main) → spawn(worker) ---
    //
    // The slot worker must NEVER touch a `&Connection` or read task data
    // from anything other than its prompt bundle. The compile-time
    // `assert_impl_all!(SlotPromptBundle: Send)` in `tests/prompt_slot.rs`
    // guards the type-level invariant; this test guards the wiring:
    // `build_slot_contexts` populates the bundle (with its DB-derived
    // `task_files`) on the main thread, BEFORE any worker spawn, and the
    // bundle survives the trip through `run_parallel_wave` unmodified.

    #[test]
    fn test_build_slot_contexts_populates_bundle_on_main_thread() {
        use crate::commands::next::selection::ScoredTask;
        let (temp, conn) = setup_test_db();
        let tmp = tempfile::TempDir::new().unwrap();
        let base_prompt = tmp.path().join("base.md");
        std::fs::write(&base_prompt, "BASE\n").unwrap();

        insert_task(&conn, "FEAT-CONTRACT", "contract task", "todo", 10);
        insert_task_file(&conn, "FEAT-CONTRACT", "src/contract.rs");

        let mut task = Task::new("FEAT-CONTRACT", "contract task");
        task.difficulty = Some("low".to_string());
        let scored = ScoredTask {
            task,
            files: vec!["src/contract.rs".to_string()],
            total_score: 0,
            score_breakdown: crate::commands::next::selection::ScoreBreakdown {
                priority_score: 0,
                file_score: 0,
                file_overlap_count: 0,
            },
        };

        let prompt_params = SlotPromptParams {
            project_root: tmp.path().to_path_buf(),
            base_prompt_path: base_prompt,
            permission_mode: PermissionMode::Dangerous,
            steering_path: None,
            session_guidance: "",
        };
        let slot_paths = vec![tmp.path().to_path_buf()];
        let slots = build_slot_contexts(&conn, vec![scored], &slot_paths, &prompt_params);

        assert_eq!(slots.len(), 1);
        let bundle = &slots[0].prompt_bundle;
        // task_id, task_files, and difficulty came from the DB / Task on
        // the main thread. The worker thread will read from this bundle
        // and never reopen `conn`.
        assert_eq!(bundle.task_id, "FEAT-CONTRACT");
        assert_eq!(bundle.task_files, vec!["src/contract.rs"]);
        assert_eq!(bundle.difficulty.as_deref(), Some("low"));
        // The task JSON inside the bundle reflects the post-claim state
        // even though the row is still 'todo' (claim happens later in
        // run_parallel_wave). This keeps the agent prompt honest about
        // the state the row WILL be in by the time the worker runs.
        assert!(
            bundle.prompt.contains("\"status\": \"in_progress\""),
            "bundle prompt must reflect post-claim status; got:\n{}",
            bundle.prompt
        );
        // Drop temp last so the connection (held in scope) outlives it.
        drop(temp);
    }

    // --- TEST-001: Comprehensive parallel execution tests -------------
    //
    // End-to-end behavior of `run_parallel_wave` and `run_wave_iteration`
    // using a mock Claude binary. Every test here mutates the process-wide
    // `CLAUDE_BINARY` env var, so each one takes the shared mutex to
    // serialize with other tests that touch the same variable.
    mod comprehensive {
        use super::*;
        use crate::loop_engine::test_utils::{CLAUDE_BINARY_MUTEX, EnvGuard};
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        /// Create a mock `claude` script for wave tests.
        ///
        /// Behavior:
        /// - Reads prompt from stdin (how `spawn_claude` delivers it).
        /// - Extracts `TASK_ID` from the task JSON `"id": "TASK-ID"` line.
        /// - Emits one stream-json `result` line so the claude wrapper's
        ///   stream-json parser yields `<completed>TASK-ID</completed>`
        ///   as the slot's output text.
        /// - When the `MOCK_CRASH_TASKS` env var lists the extracted id
        ///   (comma-delimited), exit 1 with no output so the slot outcome
        ///   becomes `Crash(RuntimeError)`.
        ///
        /// The caller removes the script with `std::fs::remove_file` after
        /// the wave completes.
        fn make_mock_script(name: &str) -> std::path::PathBuf {
            let path = std::env::temp_dir().join(format!("task_mgr_test_wave_{name}.sh"));
            {
                let mut f = std::fs::File::create(&path).unwrap();
                writeln!(f, "#!/bin/sh").unwrap();
                writeln!(f, r#"PROMPT=$(cat)"#).unwrap();
                writeln!(
                    f,
                    r#"TASK_ID=$(printf '%s' "$PROMPT" | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p' | head -n 1)"#
                )
                .unwrap();
                writeln!(
                    f,
                    r#"case ",${{MOCK_CRASH_TASKS:-}}," in *",${{TASK_ID}},"*) exit 1 ;; esac"#
                )
                .unwrap();
                writeln!(
                    f,
                    r#"printf '{{"type":"result","result":"<completed>%s</completed>"}}\n' "$TASK_ID""#
                )
                .unwrap();
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        /// Fetch a task's status, panicking if the row is missing.
        fn task_status(conn: &Connection, id: &str) -> String {
            conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |r| r.get(0))
                .unwrap()
        }

        /// Minimal PRD with the given ids so `update_prd_task_passes`
        /// finds matching `userStories` entries to flip `passes=true` on.
        fn write_prd(dir: &Path, ids: &[&str]) -> std::path::PathBuf {
            use serde_json::json;
            let stories: Vec<_> = ids
                .iter()
                .map(|id| json!({"id": id, "title": "t", "priority": 10, "passes": false}))
                .collect();
            let path = dir.join("prd.json");
            std::fs::write(
                &path,
                serde_json::to_string(&json!({"userStories": stories})).unwrap(),
            )
            .unwrap();
            path
        }

        /// Assemble a WaveIterationParams for the common test wiring.
        #[allow(clippy::too_many_arguments)]
        fn build_wave_params<'a>(
            conn: &'a mut Connection,
            db_dir: &'a Path,
            source_root: &'a Path,
            slot_paths: &'a [std::path::PathBuf],
            base_prompt: &'a Path,
            permission_mode: &'a PermissionMode,
            signal_flag: &'a SignalFlag,
            prd_path: &'a Path,
            progress_path: &'a Path,
            parallel_slots: usize,
            run_id: &'a str,
            project_config: &'a crate::loop_engine::project_config::ProjectConfig,
            prd_implicit_overlap_files: &'a [String],
        ) -> WaveIterationParams<'a> {
            WaveIterationParams {
                conn,
                db_dir,
                source_root,
                branch: "main",
                parallel_slots,
                slot_worktree_paths: slot_paths,
                iteration: 1,
                max_iterations: 1,
                elapsed_secs: 0,
                run_id,
                base_prompt_path: base_prompt,
                permission_mode,
                signal_flag,
                default_model: None,
                verbose: false,
                task_prefix: None,
                prd_path,
                progress_path,
                tasks_dir: source_root,
                external_repo_path: None,
                external_git_scan_depth: 50,
                inter_iteration_delay: Duration::ZERO,
                steering_path: None,
                session_guidance: "",
                prd_implicit_overlap_files,
                project_config,
            }
        }

        /// AC1: two non-conflicting tasks complete in one wave (--parallel 2).
        ///
        /// Two eligible tasks with disjoint `touchesFiles` fill both slots;
        /// the mock emits `<completed>` for each, so both rows flip to
        /// `done` and `tasks_completed == 2` after the wave.
        #[test]
        fn test_wave_two_disjoint_tasks_both_complete() {
            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = make_mock_script("two_complete");
            let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
            let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

            let (temp, mut conn) = setup_test_db();
            let run_id = "run-wave-complete";
            insert_run(&conn, run_id);
            insert_task(&conn, "FEAT-A", "Task A", "todo", 10);
            insert_task(&conn, "FEAT-B", "Task B", "todo", 20);
            insert_task_file(&conn, "FEAT-A", "src/a.rs");
            insert_task_file(&conn, "FEAT-B", "src/b.rs");
            // FEAT-003: opt out of the buildy shared-infra heuristic so this
            // wave-infrastructure test isolates merge-back semantics from
            // implicit-overlap detection (the test's intent predates FEAT-003).
            opt_out_buildy(&conn);

            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = write_prd(tmp.path(), &["FEAT-A", "FEAT-B"]);
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

            let mut ctx = IterationContext::new(5);
            let outcome = run_wave_iteration(
                build_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    &slot_paths,
                    &base_prompt,
                    &mode,
                    &signal,
                    &prd,
                    &progress,
                    2,
                    run_id,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );

            let _ = std::fs::remove_file(&script);

            assert_eq!(
                outcome.tasks_completed, 2,
                "both slots should complete their tasks"
            );
            assert!(outcome.iteration_consumed);
            assert_eq!(task_status(&conn, "FEAT-A"), "done");
            assert_eq!(task_status(&conn, "FEAT-B"), "done");
        }

        /// Regression: each running slot in a wave MUST start its own
        /// activity monitor. The original bug let slot-mode iterations
        /// silently skip `monitor::start_monitor`, so the watchdog's
        /// `last_activity_epoch` never advanced (no activity extensions)
        /// and there were no heartbeat / change-tracking logs.
        ///
        /// `MONITOR_START_COUNT` is a `#[cfg(test)]`-only call counter in
        /// `monitor.rs`; observing it bump by `parallel_slots` proves both
        /// slots called `start_monitor`. Uses the same fast mock-claude
        /// scaffold as the disjoint-tasks test, so the assertion is the
        /// only meaningful difference.
        #[test]
        fn test_wave_each_slot_starts_its_own_monitor() {
            use crate::loop_engine::monitor::MONITOR_START_COUNT;
            use std::sync::atomic::Ordering;

            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = make_mock_script("monitor_per_slot");
            let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
            let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

            let (temp, mut conn) = setup_test_db();
            let run_id = "run-wave-monitor-per-slot";
            insert_run(&conn, run_id);
            insert_task(&conn, "FEAT-MA", "Task MA", "todo", 10);
            insert_task(&conn, "FEAT-MB", "Task MB", "todo", 20);
            insert_task_file(&conn, "FEAT-MA", "src/ma.rs");
            insert_task_file(&conn, "FEAT-MB", "src/mb.rs");
            opt_out_buildy(&conn);

            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = write_prd(tmp.path(), &["FEAT-MA", "FEAT-MB"]);
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

            let before = MONITOR_START_COUNT.load(Ordering::Relaxed);
            let mut ctx = IterationContext::new(5);
            let outcome = run_wave_iteration(
                build_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    &slot_paths,
                    &base_prompt,
                    &mode,
                    &signal,
                    &prd,
                    &progress,
                    2,
                    run_id,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );
            let after = MONITOR_START_COUNT.load(Ordering::Relaxed);

            let _ = std::fs::remove_file(&script);

            // Sanity: both slots actually ran (so the assertion below isn't
            // satisfied by a path that short-circuited before the monitor).
            assert_eq!(outcome.tasks_completed, 2, "both slots should complete");
            assert!(
                after.saturating_sub(before) >= 2,
                "expected ≥2 monitor starts (one per running slot); before={before}, after={after}",
            );
        }

        /// AC2: signal during wave terminates all slots.
        ///
        /// Pre-set the shared signal before the wave starts. Steps 0/13 of
        /// `run_wave_iteration` short-circuit on signal, so the direct
        /// wave-iteration path is covered by
        /// `test_run_wave_iteration_pre_set_signal_returns_terminal_signal`.
        /// This test exercises `run_parallel_wave` itself: every spawned
        /// slot thread must observe the signal and bail out of its iteration
        /// without ever reaching the mock Claude process.
        #[test]
        fn test_wave_pre_signal_terminates_every_slot() {
            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Point the binary at a path that would crash if spawned, so a
            // regression that skips the signal check would surface as a
            // non-Empty outcome instead of a silent pass.
            let _guard = EnvGuard::set("CLAUDE_BINARY", "/nonexistent_binary_for_signal_test");

            let (temp, mut conn) = setup_test_db();
            insert_task(&conn, "FEAT-A", "a", "todo", 10);
            insert_task(&conn, "FEAT-B", "b", "todo", 20);

            let tmp = tempfile::TempDir::new().unwrap();

            let signal = SignalFlag::new();
            signal.set();
            let params = Arc::new(make_slot_params(temp.path(), signal.clone()));

            let slots = vec![
                make_slot(0, tmp.path().to_path_buf(), dummy_bundle("FEAT-A")),
                make_slot(1, tmp.path().to_path_buf(), dummy_bundle("FEAT-B")),
            ];
            let wave = run_parallel_wave(&mut conn, slots, params);

            assert_eq!(wave.outcomes.len(), 2, "every slot must report an outcome");
            for outcome in &wave.outcomes {
                assert!(
                    outcome.iteration_result.should_stop,
                    "slot {} must stop on signal",
                    outcome.slot_index
                );
                assert!(
                    matches!(outcome.iteration_result.outcome, IterationOutcome::Empty),
                    "slot {} outcome must be Empty on pre-set signal, got {:?}",
                    outcome.slot_index,
                    outcome.iteration_result.outcome
                );
            }
        }

        /// AC3: crash in one slot doesn't affect other slots.
        ///
        /// Mock crashes slot 0 (`FEAT-CRASH`) by setting `MOCK_CRASH_TASKS`
        /// to that id; slot 1 (`FEAT-OK`) completes normally. We expect
        /// one `Crash(RuntimeError)` outcome plus one completion mark; the
        /// completion must not be lost because its peer crashed.
        #[test]
        fn test_wave_crash_in_one_slot_does_not_affect_others() {
            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = make_mock_script("mixed_crash");
            let _bin_guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
            let _crash_guard = EnvGuard::set("MOCK_CRASH_TASKS", "FEAT-CRASH");

            let (temp, mut conn) = setup_test_db();
            let run_id = "run-wave-mixed";
            insert_run(&conn, run_id);
            insert_task(&conn, "FEAT-CRASH", "crash slot", "todo", 10);
            insert_task(&conn, "FEAT-OK", "passing slot", "todo", 20);
            insert_task_file(&conn, "FEAT-CRASH", "src/crash.rs");
            insert_task_file(&conn, "FEAT-OK", "src/ok.rs");
            opt_out_buildy(&conn);

            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = write_prd(tmp.path(), &["FEAT-CRASH", "FEAT-OK"]);
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

            let mut ctx = IterationContext::new(5);
            let outcome = run_wave_iteration(
                build_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    &slot_paths,
                    &base_prompt,
                    &mode,
                    &signal,
                    &prd,
                    &progress,
                    2,
                    run_id,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );

            let _ = std::fs::remove_file(&script);

            assert_eq!(
                outcome.tasks_completed, 1,
                "the non-crashing slot must still mark its task done"
            );
            assert_eq!(task_status(&conn, "FEAT-OK"), "done");
            assert_ne!(
                task_status(&conn, "FEAT-CRASH"),
                "done",
                "the crashed slot must not mark its task done"
            );
        }

        /// AC4: `--parallel 1` produces identical behavior to sequential.
        ///
        /// With `parallel_slots=1` and three eligible disjoint-file tasks,
        /// `select_parallel_group` caps at one task — the same pick
        /// sequential `select_next_task` would make. After the wave, the
        /// winning task is `done` and the other two are still `todo`.
        #[test]
        fn test_wave_parallel_slots_one_runs_a_single_task() {
            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = make_mock_script("parallel_one");
            let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
            let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

            let (temp, mut conn) = setup_test_db();
            let run_id = "run-wave-parallel-one";
            insert_run(&conn, run_id);
            // Priorities: 10 wins, 20 and 30 must not be touched.
            insert_task(&conn, "FEAT-WIN", "winner", "todo", 10);
            insert_task(&conn, "FEAT-SKIP1", "skip 1", "todo", 20);
            insert_task(&conn, "FEAT-SKIP2", "skip 2", "todo", 30);
            insert_task_file(&conn, "FEAT-WIN", "src/win.rs");
            insert_task_file(&conn, "FEAT-SKIP1", "src/skip1.rs");
            insert_task_file(&conn, "FEAT-SKIP2", "src/skip2.rs");

            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = write_prd(tmp.path(), &["FEAT-WIN", "FEAT-SKIP1", "FEAT-SKIP2"]);
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let slot_paths = vec![tmp.path().to_path_buf()];

            let mut ctx = IterationContext::new(5);
            let outcome = run_wave_iteration(
                build_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    &slot_paths,
                    &base_prompt,
                    &mode,
                    &signal,
                    &prd,
                    &progress,
                    1,
                    run_id,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );

            let _ = std::fs::remove_file(&script);

            assert_eq!(outcome.tasks_completed, 1, "exactly one slot runs");
            assert_eq!(task_status(&conn, "FEAT-WIN"), "done");
            assert_eq!(
                task_status(&conn, "FEAT-SKIP1"),
                "todo",
                "lower-priority task must be untouched by --parallel 1"
            );
            assert_eq!(task_status(&conn, "FEAT-SKIP2"), "todo");

            // Only one progress entry was emitted — matches sequential cadence.
            let log = std::fs::read_to_string(&progress).unwrap();
            assert_eq!(
                log.matches("- Task: FEAT-").count(),
                1,
                "exactly one task entry in progress, got: {log}"
            );
        }

        /// AC5: parallel group with all-overlapping tasks degenerates to
        /// sequential.
        ///
        /// Three tasks all touch `src/shared.rs`. Even with
        /// `parallel_slots=3`, `select_parallel_group` returns a group of
        /// one; the wave runs a single slot and only the highest-priority
        /// task advances.
        #[test]
        fn test_wave_all_overlapping_tasks_run_sequentially() {
            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = make_mock_script("all_overlap");
            let _guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
            let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

            let (temp, mut conn) = setup_test_db();
            let run_id = "run-wave-overlap";
            insert_run(&conn, run_id);
            insert_task(&conn, "FEAT-HOT1", "hot 1", "todo", 10);
            insert_task(&conn, "FEAT-HOT2", "hot 2", "todo", 20);
            insert_task(&conn, "FEAT-HOT3", "hot 3", "todo", 30);
            for id in ["FEAT-HOT1", "FEAT-HOT2", "FEAT-HOT3"] {
                insert_task_file(&conn, id, "src/shared.rs");
            }

            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = write_prd(tmp.path(), &["FEAT-HOT1", "FEAT-HOT2", "FEAT-HOT3"]);
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let slot_paths = vec![
                tmp.path().to_path_buf(),
                tmp.path().to_path_buf(),
                tmp.path().to_path_buf(),
            ];

            let mut ctx = IterationContext::new(5);
            let outcome = run_wave_iteration(
                build_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    &slot_paths,
                    &base_prompt,
                    &mode,
                    &signal,
                    &prd,
                    &progress,
                    3,
                    run_id,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );

            let _ = std::fs::remove_file(&script);

            assert_eq!(
                outcome.tasks_completed, 1,
                "file-conflict collapse must leave only one slot running"
            );
            assert_eq!(task_status(&conn, "FEAT-HOT1"), "done");
            assert_eq!(task_status(&conn, "FEAT-HOT2"), "todo");
            assert_eq!(task_status(&conn, "FEAT-HOT3"), "todo");
        }

        /// AC7 — CrashTracker wave policy: all-slot crash increments the
        /// tracker, any-slot success resets it.
        ///
        /// Two tasks → both crash → `record_crash()` called so
        /// `ctx.crash_tracker.count() == 1` after the wave.
        #[test]
        fn test_wave_crash_tracker_all_crashed_increments() {
            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = make_mock_script("all_crash");
            let _bin_guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
            // Both tasks crash.
            let _crash_guard = EnvGuard::set("MOCK_CRASH_TASKS", "FEAT-X,FEAT-Y");

            let (temp, mut conn) = setup_test_db();
            let run_id = "run-wave-all-crash";
            insert_run(&conn, run_id);
            insert_task(&conn, "FEAT-X", "x", "todo", 10);
            insert_task(&conn, "FEAT-Y", "y", "todo", 20);
            insert_task_file(&conn, "FEAT-X", "src/x.rs");
            insert_task_file(&conn, "FEAT-Y", "src/y.rs");

            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = write_prd(tmp.path(), &["FEAT-X", "FEAT-Y"]);
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

            let mut ctx = IterationContext::new(10);
            assert_eq!(ctx.crash_tracker.count(), 0);

            let outcome = run_wave_iteration(
                build_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    &slot_paths,
                    &base_prompt,
                    &mode,
                    &signal,
                    &prd,
                    &progress,
                    2,
                    run_id,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );

            let _ = std::fs::remove_file(&script);

            assert_eq!(outcome.tasks_completed, 0, "no slot should complete");
            assert_eq!(
                ctx.crash_tracker.count(),
                1,
                "all-slots-crashed must bump the crash tracker exactly once per wave"
            );
        }

        /// AC7 — mirror: at least one slot completes, so the crash tracker
        /// resets even if a sibling crashed. Seeds `count = 2` first so the
        /// reset-to-zero assertion is meaningful.
        #[test]
        fn test_wave_crash_tracker_any_completed_resets() {
            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = make_mock_script("mixed_reset");
            let _bin_guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
            let _crash_guard = EnvGuard::set("MOCK_CRASH_TASKS", "FEAT-CRASH");

            let (temp, mut conn) = setup_test_db();
            let run_id = "run-wave-mixed-reset";
            insert_run(&conn, run_id);
            insert_task(&conn, "FEAT-CRASH", "c", "todo", 10);
            insert_task(&conn, "FEAT-OK2", "ok", "todo", 20);
            insert_task_file(&conn, "FEAT-CRASH", "src/crash2.rs");
            insert_task_file(&conn, "FEAT-OK2", "src/ok2.rs");
            opt_out_buildy(&conn);

            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = write_prd(tmp.path(), &["FEAT-CRASH", "FEAT-OK2"]);
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

            let mut ctx = IterationContext::new(10);
            ctx.crash_tracker.record_crash();
            ctx.crash_tracker.record_crash();
            assert_eq!(ctx.crash_tracker.count(), 2);

            run_wave_iteration(
                build_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    &slot_paths,
                    &base_prompt,
                    &mode,
                    &signal,
                    &prd,
                    &progress,
                    2,
                    run_id,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );

            let _ = std::fs::remove_file(&script);

            assert_eq!(
                ctx.crash_tracker.count(),
                0,
                "any-slot success must reset the crash tracker to zero"
            );
        }

        /// AC8: progress file entries include slot numbers.
        ///
        /// After a 2-slot wave, the progress log must carry per-slot
        /// headers (`Iteration N Slot M`) and body lines (`- Slot: M`)
        /// so operators can correlate entries with wave slots.
        #[test]
        fn test_wave_progress_entries_include_slot_numbers() {
            let _env_lock = CLAUDE_BINARY_MUTEX
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let script = make_mock_script("progress_slots");
            let _bin_guard = EnvGuard::set("CLAUDE_BINARY", script.to_str().unwrap());
            let _crash_guard = EnvGuard::remove("MOCK_CRASH_TASKS");

            let (temp, mut conn) = setup_test_db();
            let run_id = "run-wave-progress";
            insert_run(&conn, run_id);
            insert_task(&conn, "FEAT-P1", "p1", "todo", 10);
            insert_task(&conn, "FEAT-P2", "p2", "todo", 20);
            insert_task_file(&conn, "FEAT-P1", "src/p1.rs");
            insert_task_file(&conn, "FEAT-P2", "src/p2.rs");
            opt_out_buildy(&conn);

            let tmp = tempfile::TempDir::new().unwrap();
            let base_prompt = tmp.path().join("base.md");
            std::fs::write(&base_prompt, "base").unwrap();
            let prd = write_prd(tmp.path(), &["FEAT-P1", "FEAT-P2"]);
            let progress = tmp.path().join("progress.txt");
            let mode = PermissionMode::Dangerous;
            let signal = SignalFlag::new();
            let project_cfg = crate::loop_engine::project_config::ProjectConfig::default();
            let prd_implicit: Vec<String> = Vec::new();
            let slot_paths = vec![tmp.path().to_path_buf(), tmp.path().to_path_buf()];

            let mut ctx = IterationContext::new(5);
            run_wave_iteration(
                build_wave_params(
                    &mut conn,
                    temp.path(),
                    tmp.path(),
                    &slot_paths,
                    &base_prompt,
                    &mode,
                    &signal,
                    &prd,
                    &progress,
                    2,
                    run_id,
                    &project_cfg,
                    &prd_implicit,
                ),
                &mut ctx,
            );

            let _ = std::fs::remove_file(&script);

            let log = std::fs::read_to_string(&progress).expect("progress file exists");
            assert!(
                log.contains("Iteration 1 Slot 0"),
                "progress must tag slot 0 in iteration 1 header, got: {log}"
            );
            assert!(
                log.contains("Iteration 1 Slot 1"),
                "progress must tag slot 1 in iteration 1 header, got: {log}"
            );
            assert!(
                log.contains("- Slot: 0"),
                "progress body must contain '- Slot: 0', got: {log}"
            );
            assert!(
                log.contains("- Slot: 1"),
                "progress body must contain '- Slot: 1', got: {log}"
            );
        }
    }

    // --- FEAT-002: merge-back failure halt-check tests ---
    //
    // These tests cover the wave-loop FEAT-002 reset/halt contract in
    // isolation, exercising `apply_merge_fail_reset_and_halt_check` directly
    // so we don't need to drive a full `run_loop` (which would require git,
    // Claude, and a multi-slot worktree harness — that level of coverage
    // belongs in `tests/` integration tests once the cross-cutting harness
    // exists for FEAT-001/003/004).

    fn insert_in_progress_task(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, started_at) VALUES \
             (?1, 'merge-fail test task', 'in_progress', 1, datetime('now'))",
            [id],
        )
        .unwrap();
    }

    /// AC: WaveOutcome.failed_merges is empty when no merge failures
    /// (e.g. preflight bail-out / no-eligible-tasks).
    #[test]
    fn test_wave_outcome_failed_merges_empty_by_default() {
        let outcome = WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: None,
            was_stopped: false,
            failed_merges: Vec::new(),
        };
        assert!(outcome.failed_merges.is_empty());
    }

    /// AC: WaveOutcome.failed_merges carries `(slot, task_id)` as a single
    /// `FailedMerge` value so the slot/task pairing is a type-level guarantee
    /// (no parallel arrays held lockstep by rustdoc).
    #[test]
    fn test_wave_outcome_failed_merges_pair_slot_with_task_id() {
        let outcome = WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: None,
            was_stopped: false,
            failed_merges: vec![
                FailedMerge {
                    slot: 1,
                    task_id: Some("FEAT-001".into()),
                },
                FailedMerge {
                    slot: 2,
                    task_id: Some("FEAT-002".into()),
                },
            ],
        };
        assert_eq!(outcome.failed_merges.len(), 2);
        assert_eq!(outcome.failed_merges[0].slot, 1);
        assert_eq!(
            outcome.failed_merges[0].task_id.as_deref(),
            Some("FEAT-001")
        );
        assert_eq!(outcome.failed_merges[1].slot, 2);
        assert_eq!(
            outcome.failed_merges[1].task_id.as_deref(),
            Some("FEAT-002")
        );
    }

    /// AC: ctx.consecutive_merge_fail_waves increments on a failed wave.
    #[test]
    fn test_consecutive_counter_increments_on_failure() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);

        let decision = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2, // default threshold
        );
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);
        assert_eq!(decision, MergeFailHaltDecision::Continue);
    }

    /// AC: counter resets to 0 on a fully-successful wave (failed_merges empty).
    #[test]
    fn test_consecutive_counter_resets_on_success() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 3;

        let decision =
            apply_merge_fail_reset_and_halt_check(&mut conn, &mut ctx, "feat/test", &[], 2);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);
        assert_eq!(decision, MergeFailHaltDecision::Continue);
    }

    /// AC: failed slot's task is reset back to `todo`.
    #[test]
    fn test_failed_slot_task_reset_to_todo() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        ctx.pending_slot_tasks.push("FEAT-001".to_string());

        apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2,
        );

        let status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001");
        assert_eq!(status, "todo");
        // pending_slot_tasks drained.
        assert!(!ctx.pending_slot_tasks.contains(&"FEAT-001".to_string()));
    }

    /// AC: threshold reached → Halt with non-zero exit and reason citing the
    /// counter / threshold values.
    #[test]
    fn test_halt_returned_when_threshold_reached() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 1; // already 1, the next hit makes it 2.

        let decision = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2,
        );
        match decision {
            MergeFailHaltDecision::Halt {
                exit_code,
                exit_reason,
            } => {
                assert_eq!(exit_code, 1);
                assert!(
                    exit_reason.contains("2 consecutive"),
                    "exit_reason should cite counter; got: {exit_reason}"
                );
                assert!(
                    exit_reason.contains("threshold=2"),
                    "exit_reason should cite threshold; got: {exit_reason}"
                );
            }
            _ => panic!("expected Halt, got {decision:?}"),
        }
        assert_eq!(ctx.consecutive_merge_fail_waves, 2);
    }

    /// AC: known-bad — verify reset happens BEFORE the threshold check, so a
    /// halted run still leaves the DB in a re-runnable state. Equivalent: the
    /// failed-slot task is `todo` even when the threshold was reached.
    #[test]
    fn test_reset_runs_before_halt_decision() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 0;

        // threshold 1 → halts on this very wave.
        let decision = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            1,
        );
        assert!(matches!(decision, MergeFailHaltDecision::Halt { .. }));

        // The reset must have happened despite the immediate halt.
        let status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001");
        assert_eq!(
            status, "todo",
            "AC: halted run must NOT leave any task in `in_progress` for the failed slots"
        );
    }

    /// AC: threshold = 0 → never halt (legacy behavior preserved). Counter
    /// still increments — operators can observe the cascade in logs without
    /// the loop aborting.
    #[test]
    fn test_threshold_zero_never_halts() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 100; // arbitrarily high.

        let decision = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            0, // threshold 0 = never halt.
        );
        assert_eq!(decision, MergeFailHaltDecision::Continue);
        assert_eq!(ctx.consecutive_merge_fail_waves, 101);
    }

    /// AC: failed_merges empty → no reset, counter cleared, Continue.
    #[test]
    fn test_empty_failed_merges_resets_counter_no_side_effects() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001"); // stays in_progress.
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 5;
        ctx.pending_slot_tasks.push("FEAT-001".to_string());

        let decision =
            apply_merge_fail_reset_and_halt_check(&mut conn, &mut ctx, "feat/test", &[], 2);
        assert_eq!(decision, MergeFailHaltDecision::Continue);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);
        // Did NOT touch unrelated in-progress task.
        let status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001");
        assert_eq!(status, "in_progress");
        // pending_slot_tasks NOT drained on the empty path.
        assert!(ctx.pending_slot_tasks.contains(&"FEAT-001".to_string()));
    }

    /// AC: multiple failed slots — every task is reset and drained from
    /// pending_slot_tasks.
    #[test]
    fn test_multiple_failed_slots_all_reset() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        insert_in_progress_task(&conn, "FEAT-002");
        let mut ctx = IterationContext::new(5);
        ctx.pending_slot_tasks.push("FEAT-001".to_string());
        ctx.pending_slot_tasks.push("FEAT-002".to_string());

        apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[
                FailedMerge {
                    slot: 1,
                    task_id: Some("FEAT-001".into()),
                },
                FailedMerge {
                    slot: 2,
                    task_id: Some("FEAT-002".into()),
                },
            ],
            5,
        );
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001"),
            "todo"
        );
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-002"),
            "todo"
        );
        assert!(ctx.pending_slot_tasks.is_empty());
    }

    /// AC: failure-mode — reset failures are non-fatal; threshold check still
    /// runs on the original failed_merges count. We can't easily inject a SQL
    /// error here, but we CAN verify that a slot whose task_id is `None`
    /// (e.g. claim never resolved) is silently skipped without panicking,
    /// AND the counter still increments + halt still triggers based on the
    /// full failed_merges count.
    #[test]
    fn test_reset_failure_modes_dont_skip_threshold_check() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        let mut ctx = IterationContext::new(5);

        // Two failed slots, neither has a resolved task_id.
        let decision = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[
                FailedMerge {
                    slot: 1,
                    task_id: None,
                },
                FailedMerge {
                    slot: 2,
                    task_id: None,
                },
            ],
            1, // threshold 1 → halt on first failure.
        );
        assert!(matches!(decision, MergeFailHaltDecision::Halt { .. }));
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);
    }

    /// AC: halt diagnostic message includes each failed slot's ephemeral
    /// branch name (verified indirectly via the canonical helper). Direct
    /// stderr capture is brittle; we verify the helper is consulted by
    /// reproducing its output for a known input.
    #[test]
    fn test_diagnostic_uses_ephemeral_slot_branch_helper() {
        // Sanity-check: the diagnostic format string in the helper must call
        // `worktree::ephemeral_slot_branch` (per CONTRACT AC). If a future
        // refactor inlines `format!()` instead, the names produced for slot 1
        // would still match — but the AC binds us to the helper, so the
        // fastest regression catch is auditing this single call site.
        let name = crate::loop_engine::worktree::ephemeral_slot_branch("feat/test", 1);
        assert_eq!(name, "feat/test-slot-1");
    }

    /// AC (Fix 3): when the deadlock guard fires but every blocking branch
    /// has an unparseable slot suffix, `handle_ephemeral_deadlock` MUST
    /// still produce at least one `FailedMerge` so
    /// `apply_merge_fail_reset_and_halt_check` increments the counter
    /// instead of resetting it. The sentinel index is `SYNTHETIC_DEADLOCK_SLOT`
    /// so the diagnostic step can recognize it and avoid synthesizing a
    /// `{branch}-slot-18446744073709551615` name.
    #[test]
    fn test_synthetic_deadlock_slot_sentinel_increments_counter() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 0;

        // Sentinel-only failed_merges (simulates the all-malformed deadlock
        // path). Generous threshold so we observe the increment without halt.
        let decision = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: SYNTHETIC_DEADLOCK_SLOT,
                task_id: None,
            }],
            5,
        );
        assert_eq!(decision, MergeFailHaltDecision::Continue);
        assert_eq!(
            ctx.consecutive_merge_fail_waves, 1,
            "synthetic-deadlock sentinel must still increment the counter"
        );
    }

    /// AC (Fix 3): when the threshold is reached on a sentinel-only failure
    /// wave, the halt diagnostic must NOT synthesize a meaningless
    /// `{branch}-slot-18446744073709551615` name. The reason field surfaces
    /// the counter/threshold; the `<malformed deadlock blocker>` placeholder
    /// flows to stderr (verified indirectly by ensuring no `usize::MAX`
    /// appears in the rendered exit_reason on this halt).
    #[test]
    fn test_synthetic_deadlock_slot_diagnostic_does_not_render_huge_name() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        let mut ctx = IterationContext::new(5);
        ctx.consecutive_merge_fail_waves = 0;

        // threshold=1 → halt on this very wave; sentinel-only failed_merges.
        let decision = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: SYNTHETIC_DEADLOCK_SLOT,
                task_id: None,
            }],
            1,
        );
        match decision {
            MergeFailHaltDecision::Halt {
                exit_code: _,
                exit_reason,
            } => {
                // The exit_reason itself is just the counter/threshold
                // summary, but the full diagnostic flowed to stderr. Assert
                // the reason does not contain the sentinel-rendered name
                // shape (defensive — catches an accidental future change
                // that puts the slot list back into exit_reason).
                assert!(
                    !exit_reason.contains(&usize::MAX.to_string()),
                    "exit_reason must not include usize::MAX-rendered name; got: {exit_reason}"
                );
            }
            _ => panic!("expected Halt, got {decision:?}"),
        }
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);
    }

    /// AC: counter is reset back to 0 by a successful wave AFTER a series
    /// of consecutive failures — i.e. one good wave breaks the cascade.
    #[test]
    fn test_consecutive_counter_lifecycle_failure_then_success() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        let mut ctx = IterationContext::new(5);

        // Wave 1: fail.
        apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            5, // generous threshold so we don't halt.
        );
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);

        // Wave 2: clean.
        apply_merge_fail_reset_and_halt_check(&mut conn, &mut ctx, "feat/test", &[], 5);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);
    }

    /// AC: IterationContext::new starts the FEAT-002 counter at 0.
    #[test]
    fn test_iteration_context_new_zeroes_consecutive_counter() {
        let ctx = IterationContext::new(5);
        assert_eq!(ctx.consecutive_merge_fail_waves, 0);
    }

    /// AC: full two-wave cascade — first failure increments without halting
    /// (default threshold = 2); second consecutive failure crosses threshold
    /// and halts. Both waves' failed-slot tasks must end up `todo`.
    #[test]
    fn test_two_consecutive_failures_halt_with_default_threshold() {
        let (_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        insert_in_progress_task(&conn, "FEAT-002");
        let mut ctx = IterationContext::new(5);
        ctx.pending_slot_tasks.push("FEAT-001".to_string());
        ctx.pending_slot_tasks.push("FEAT-002".to_string());

        // Wave 1: slot 1 merge fails for FEAT-001. Below threshold → continue.
        let d1 = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2, // default
        );
        assert_eq!(d1, MergeFailHaltDecision::Continue);
        assert_eq!(ctx.consecutive_merge_fail_waves, 1);
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001"),
            "todo",
            "Wave 1's failed-slot task must be reset to todo"
        );

        // Set FEAT-001 back to in_progress so wave 2 has something realistic
        // to reset (simulates the loop re-claiming the now-todo task).
        conn.execute(
            "UPDATE tasks SET status = 'in_progress' WHERE id = 'FEAT-001'",
            [],
        )
        .unwrap();
        ctx.pending_slot_tasks.push("FEAT-001".to_string());

        // Wave 2: slot 1 fails again. Counter hits threshold → Halt.
        let d2 = apply_merge_fail_reset_and_halt_check(
            &mut conn,
            &mut ctx,
            "feat/test",
            &[FailedMerge {
                slot: 1,
                task_id: Some("FEAT-001".into()),
            }],
            2,
        );
        match d2 {
            MergeFailHaltDecision::Halt {
                exit_code,
                exit_reason,
            } => {
                assert_eq!(exit_code, 1);
                assert!(exit_reason.contains("2 consecutive"));
            }
            _ => panic!("expected Halt, got {d2:?}"),
        }
        assert_eq!(ctx.consecutive_merge_fail_waves, 2);
        // CRITICAL: even on halt, the task must be back to todo so the next
        // run can re-claim it.
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001"),
            "todo",
            "AC: halted run must NOT leave any task in `in_progress` for the failed slots"
        );
    }

    // --- FEAT-003: post-merge reconcile wiring tests ---

    /// AC: drive the wave path's post-merge reconcile step with FEAT-001 in
    /// the merged-back range. Expect pending_slot_tasks drained of FEAT-001
    /// only (FEAT-002 retained), agg.tasks_completed bumped by 1, and
    /// agg.any_completed flipped to true. Pins the contract that the four
    /// terminal returns in `run_wave_iteration` (which read either
    /// `agg.tasks_completed` directly or the `let mut tasks_completed =
    /// agg.tasks_completed` shadow created BEFORE the external-git block)
    /// see the reconciled count — i.e. the reconcile call sits BEFORE the
    /// shadow, not after.
    #[test]
    fn test_post_merge_reconcile_drains_pending_and_bumps_agg() {
        use std::process::Command;

        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Feat one', 'in_progress', 1),
             ('FEAT-002', 'Feat two', 'in_progress', 1);",
        )
        .unwrap();
        insert_run(&conn, "run-1");

        // Slot 0 worktree with a commit whose body carries FEAT-001's
        // completion marker — the realistic "agent merged-back but never
        // flushed <completed>" shape.
        let repo = setup_git_repo();
        let pre_out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .expect("rev-parse HEAD");
        let pre = String::from_utf8_lossy(&pre_out.stdout).trim().to_string();
        let msg = "feat: implement thing\n\nfeat: FEAT-001-completed - Implement feature";
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", msg])
            .current_dir(repo.path())
            .output()
            .expect("create marker commit");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Feat one","passes":false,"priority":1},
                {"id":"FEAT-002","title":"Feat two","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        let mut ctx = IterationContext::new(5);
        ctx.pending_slot_tasks.push("FEAT-001".to_string());
        ctx.pending_slot_tasks.push("FEAT-002".to_string());

        let mut agg = WaveAggregator::new(2);
        let before_completed = agg.tasks_completed;

        apply_post_merge_reconcile(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
            &mut ctx,
            &mut agg,
        );

        assert_eq!(
            ctx.pending_slot_tasks,
            vec!["FEAT-002".to_string()],
            "FEAT-001 must be drained; FEAT-002 retained"
        );
        assert_eq!(
            agg.tasks_completed,
            before_completed + 1,
            "agg.tasks_completed must reflect the one reconciled task so terminal returns report it"
        );
        assert!(
            agg.any_completed,
            "agg.any_completed must flip so the all-tasks-done terminal can fire"
        );
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001"),
            "done",
            "the underlying reconcile must have marked FEAT-001 done"
        );
    }

    /// AC negative: empty reconciled Vec leaves agg, ctx, and DB untouched.
    /// Mirrors the "no marker in {pre..HEAD}" production path — the helper
    /// must not eat the crash-tracker success budget or drain unrelated
    /// pending slot tasks on a no-op call.
    #[test]
    fn test_post_merge_reconcile_no_match_is_noop() {
        use std::process::Command;

        let (_db_tmp, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        insert_in_progress_task(&conn, "FEAT-001");
        insert_run(&conn, "run-1");

        let repo = setup_git_repo();
        let pre_out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .expect("rev-parse HEAD");
        let pre = String::from_utf8_lossy(&pre_out.stdout).trim().to_string();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "chore: unrelated"])
            .current_dir(repo.path())
            .output()
            .expect("create commit");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Feat one","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        let mut ctx = IterationContext::new(5);
        ctx.pending_slot_tasks.push("FEAT-001".to_string());
        let mut agg = WaveAggregator::new(1);

        apply_post_merge_reconcile(
            repo.path(),
            &pre,
            &mut conn,
            "run-1",
            &prd_path,
            None,
            &mut ctx,
            &mut agg,
        );

        assert_eq!(agg.tasks_completed, 0);
        assert!(!agg.any_completed);
        assert_eq!(
            ctx.pending_slot_tasks,
            vec!["FEAT-001".to_string()],
            "no drain on no-match"
        );
        assert_eq!(
            crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001"),
            "in_progress"
        );
    }
}
