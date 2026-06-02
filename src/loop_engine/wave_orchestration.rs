//! Wave-orchestration decision helpers.
//!
//! Relocated verbatim from `wave_scheduler.rs` (PRD harden-baseline-tier-routing,
//! WS-3.3 / REFACTOR-003) as a pure, behavior-neutral move. This module owns the
//! non-hot-path group-selection and safety logic that `run_wave_iteration`
//! consults BEFORE (and instead of) the parallel fan-out: the per-wave preflight
//! (`wave_preflight_check`) and the two empty-group handlers
//! (`handle_no_eligible_tasks`, `handle_ephemeral_deadlock`). The parallel
//! claim-and-spawn fan-out (`run_parallel_wave`) and the wave entry point
//! (`run_wave_iteration`) stay in `wave_scheduler.rs` and call these across the
//! module boundary.
//!
//! Behavior is byte-identical to the pre-carve `wave_scheduler.rs`: the FEAT-002
//! reset/halt contract and the `SYNTHETIC_DEADLOCK_SLOT` sentinel are preserved
//! exactly. Both `classify_drained_queue` (the shared sequential/wave drain
//! classifier) and the `SYNTHETIC_DEADLOCK_SLOT` constant remain owned by
//! `wave_scheduler.rs` and are imported here — they have other consumers in that
//! module (`run_wave_iteration`'s all-complete exit and
//! `apply_merge_fail_reset_and_halt_check`'s diagnostic), so moving them would
//! widen this carve's blast radius.
//!
//! **Reaction single-home lock (CONTRACT-001)**: `#![deny(deprecated)]` keeps a
//! direct call to any relocated reaction leaf a compile error here, exactly as in
//! the sibling engine files (`iteration.rs`, `wave_scheduler.rs`, `slot.rs`).
#![deny(deprecated)]

use std::thread;

use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::config::IterationOutcome;
use crate::loop_engine::display;
use crate::loop_engine::engine::{
    FailedMerge, IterationContext, WaveIterationParams, WaveOutcome, WaveTerminal,
};
use crate::loop_engine::prd_reconcile::reconcile_passes_with_db;
use crate::loop_engine::progress;
use crate::loop_engine::reactions;
use crate::loop_engine::signals;
use crate::loop_engine::usage::UsageCheckResult;
use crate::loop_engine::wave_scheduler::{SYNTHETIC_DEADLOCK_SLOT, classify_drained_queue};
use crate::output::ui;

/// Pre-wave preflight: signal/stop-file checks and crash backoff/abort.
/// Returns `Some(WaveOutcome)` when the wave should bail out before doing
/// any work, `None` when execution should proceed.
pub(super) fn wave_preflight_check(
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
            rate_limited_retry: false,
        });
    }
    if signals::check_stop_signal(params.tasks_dir, params.task_prefix) {
        ui::emit("Stop signal detected (.stop file found)");
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
            rate_limited_retry: false,
        });
    }

    // Pre-iteration usage gate (FEAT-003): account-global, so fire it EXACTLY
    // once per wave (not once per slot — that would issue N redundant API/DB
    // checks). The wave path previously LACKED this gate entirely: a
    // rate-limited account never waited before a wave dispatched, stranding
    // in-flight work. Routes through the SAME
    // `reactions::account::account_usage_gate` coordinator the sequential path
    // folds at `run_iteration` Step 1.5, so both paths agree on the
    // GateDecision for a given usage state. Ordered after the stop check and
    // before crash backoff to mirror the sequential Step ordering.
    if params.usage_params.enabled {
        match reactions::account::account_usage_gate(reactions::account::AccountUsageGateParams {
            threshold: params.usage_params.threshold,
            tasks_dir: params.tasks_dir,
            fallback_wait: params.usage_params.fallback_wait,
        }) {
            UsageCheckResult::StopSignaled => {
                ui::emit("Stop signal during usage wait, exiting");
                return Some(WaveOutcome {
                    tasks_completed: 0,
                    iteration_consumed: false,
                    terminal: Some(WaveTerminal {
                        exit_code: 0,
                        reason: "stop signal during usage wait".to_string(),
                        run_status: None,
                    }),
                    was_stopped: true,
                    failed_merges: Vec::new(),
                    rate_limited_retry: false,
                });
            }
            UsageCheckResult::ApiError(ref msg) => {
                tracing::warn!(msg = %msg, "usage API warning (continuing)");
            }
            // BelowThreshold, WaitedAndReset, Skipped — proceed with the wave.
            _ => {}
        }
    }

    // Crash backoff + abort. Identical contract to the sequential path so
    // learning [1005] (don't burn iterations on a wedged task) holds even
    // when every slot of the previous wave crashed.
    let backoff = ctx.crash_tracker.backoff_duration();
    if !backoff.is_zero() {
        ui::emit(&format!(
            "Crash backoff: waiting {} before retry...",
            display::format_duration(backoff.as_secs())
        ));
        thread::sleep(backoff);
    }
    if ctx.crash_tracker.should_abort() {
        ui::emit("Too many consecutive crashes, aborting loop");
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
            rate_limited_retry: false,
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
pub(super) fn handle_no_eligible_tasks(
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
        progress::log_iteration(progress::LogIterationParams {
            progress_path: params.progress_path,
            iteration: params.iteration,
            task_id: None,
            outcome: &IterationOutcome::Completed,
            files: &[],
            model: None,
            effort: None,
            slot: None,
        });
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
            rate_limited_retry: false,
        };
    }

    // (2) Tasks remain but none are eligible → attempt auto-recovery before
    // counting this as stale.
    reconcile_passes_with_db(params.conn, prd_path, task_prefix);
    let recovered = TaskLifecycle::new(params.conn)
        .recover_in_progress_for_prefix(task_prefix)
        .unwrap_or(0);
    if recovered > 0 {
        ui::emit(&format!(
            "Auto-recovered {} stale in_progress task(s), retrying task selection next wave...",
            recovered
        ));
        return WaveOutcome {
            tasks_completed: 0,
            iteration_consumed: true,
            terminal: None,
            was_stopped: false,
            failed_merges: Vec::new(),
            rate_limited_retry: false,
        };
    }

    // (3) Genuinely stuck: nothing eligible, nothing recoverable. Count toward
    // the stale-abort threshold exactly as before.
    ctx.stale_tracker.mark_stale();
    progress::log_iteration(progress::LogIterationParams {
        progress_path: params.progress_path,
        iteration: params.iteration,
        task_id: None,
        outcome: &IterationOutcome::NoEligibleTasks,
        files: &[],
        model: None,
        effort: None,
        slot: None,
    });
    if ctx.stale_tracker.should_abort() {
        ui::emit(&format!(
            "Aborting: no eligible tasks after {} consecutive stale iterations",
            ctx.stale_tracker.count()
        ));
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
            rate_limited_retry: false,
        };
    }
    WaveOutcome {
        tasks_completed: 0,
        iteration_consumed: true,
        terminal: None,
        was_stopped: false,
        failed_merges: Vec::new(),
        rate_limited_retry: false,
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
pub(super) fn handle_ephemeral_deadlock(
    params: &WaveIterationParams<'_>,
    ctx: &mut IterationContext,
    diagnostics: Vec<(String, Vec<String>)>,
) -> WaveOutcome {
    ctx.stale_tracker.mark_stale();
    progress::log_iteration(progress::LogIterationParams {
        progress_path: params.progress_path,
        iteration: params.iteration,
        task_id: None,
        outcome: &IterationOutcome::NoEligibleTasks,
        files: &[],
        model: None,
        effort: None,
        slot: None,
    });

    ui::emit(
        "Cross-wave deadlock: every eligible candidate is blocked by un-merged ephemeral branch(es). \
         Treating as merge-fail wave so the halt threshold can fire.",
    );
    for (cand_id, branches) in &diagnostics {
        ui::emit(&format!(
            "  {} blocked by: {}",
            cand_id,
            branches.join(", ")
        ));
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
                _ => ui::emit(&format!(
                    "Warning: skipping ephemeral branch with non-numeric / zero slot suffix: {}",
                    branch
                )),
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
        ui::emit(
            "warning: deadlock guard fired with no parseable ephemeral slot indices \
             — inserting synthetic halt slot so the threshold counter still increments",
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
        rate_limited_retry: false,
    }
}
