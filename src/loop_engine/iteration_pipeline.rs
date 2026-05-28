//! Shared post-Claude pipeline used by both the sequential `run_iteration`
//! path and the parallel-slot `process_slot_result` path.
//!
//! # Call sites
//!
//! `process_iteration_output` is invoked from exactly two places:
//!
//! - **Sequential** — `run_loop` in `src/loop_engine/engine.rs` (~line 3204),
//!   immediately after `run_iteration` returns. Replaces the inline post-Claude
//!   block that previously lived at engine.rs ~lines 2032-2113 in the
//!   pre-FEAT-005 layout.
//! - **Wave** — `process_slot_result` in `src/loop_engine/engine.rs` (~line
//!   1166), called by `run_wave_iteration` once per finished slot. Replaces
//!   the inline wave-mode glue that lived at engine.rs ~lines 1053-1246 before
//!   the unification.
//!
//! Keeping a single pipeline means wave mode can no longer silently skip
//! behaviors the sequential path treats as core (the original drift this PRD
//! exists to fix).
//!
//! # Pipeline steps (in order)
//!
//! 1. `progress::log_iteration` — appends a structured entry to
//!    `tasks/progress-<prefix>.txt` (model, effort, files, slot threaded via
//!    `ProcessingParams`).
//! 2. `<key-decision>` extraction + `key_decisions_db::insert_key_decision` —
//!    parses any decision tags emitted by Claude and persists them for later
//!    `tm-decisions` review.
//! 3. `<task-status>` tag dispatch via `engine::apply_status_updates` —
//!    routes `done`/`failed`/`skipped`/`irrelevant`/`blocked` updates through
//!    the `task-mgr` CLI.
//! 4. Completion ladder (first hit wins):
//!    `<task-status>:done` → `<completed>` tag → git commit detection (gated
//!    on `skip_git_completion_detection`) → output scan
//!    (`scan_output_for_completed_tasks`) →
//!    `is_task_reported_already_complete` fallback. The fallback fires in
//!    BOTH skip-git modes — that's the wave-mode parity fix the PRD calls out.
//! 5. `learnings::ingestion::extract_learnings_from_output` — opt-out via the
//!    `TASK_MGR_NO_EXTRACT_LEARNINGS=1` env var.
//! 6. `feedback::record_iteration_feedback` — bandit reward signal for the
//!    learnings that were actually shown to Claude this iteration.
//! 7. Per-task crash-tracking writes onto `IterationContext.crashed_last_iteration`
//!    (replaces the legacy `last_task_id` / `last_was_crash` scalars).
//!
//! # Out of scope
//!
//! Two kinds of post-Claude work stay out of this pipeline:
//!
//! 1. **Converged main-thread reactions** — wrapper-commit, external-git
//!    reconciliation, human-review trigger (`reactions::post_completion::
//!    react_to_completions`), the post-output rate-limit / transient-backend
//!    waits (`reactions::account::{react_to_outputs, react_to_transient}`), the
//!    pre-dispatch usage gate (`reactions::account::account_usage_gate`), the
//!    overflow ladder (`reactions::post_output::handle_overflow`), and the
//!    iteration-budget give-back (`reactions::account_iteration_budget`). These
//!    are NOT inline at the call sites any more: the reactions framework owns
//!    them in a single home that BOTH execution paths route through (see
//!    `src/loop_engine/CLAUDE.md` → "Reaction framework (shared)"). They live
//!    outside this pipeline because they need the outer-loop context (working
//!    tree, account-global wait/budget state) the per-iteration pipeline does
//!    not carry.
//! 2. **Genuinely call-site-local glue** — pause-signal handling and slot
//!    merge resolution stay inline at `run_loop` / `run_wave_iteration`; they
//!    own working-tree state (signals, the slot-0 merge worktree) that is not a
//!    per-iteration post-Claude concern.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;

use crate::commands::complete as complete_cmd;
use crate::db::schema::key_decisions as key_decisions_db;
use crate::loop_engine::config::IterationOutcome;
use crate::loop_engine::detection;
use crate::loop_engine::engine::IterationContext;
#[allow(deprecated)]
use crate::loop_engine::engine::apply_status_updates;
use crate::loop_engine::feedback;
use crate::loop_engine::git_reconcile::check_git_for_task_completion;
use crate::loop_engine::output_parsing::{parse_completed_tasks, scan_output_for_completed_tasks};
use crate::loop_engine::prd_reconcile::{mark_task_done, update_prd_task_passes};
use crate::loop_engine::progress;
use crate::loop_engine::signals::SignalFlag;
use crate::output::ui;

/// Aggregated results from one pass through the pipeline.
///
/// Mirrors the per-slot bookkeeping `process_slot_result` keeps today and
/// the per-iteration counters that `run_loop` accumulates in its sequential
/// post-Claude block.
#[derive(Debug, Default)]
pub struct ProcessingOutcome {
    /// Number of distinct task IDs the pipeline marked done in this pass.
    /// Deduped across `<task-status>:done`, `<completed>`, git-detection,
    /// output-scan, and the already-complete fallback branches.
    pub tasks_completed: u32,
    /// Every distinct task ID that the pipeline marked done in this pass.
    /// Includes the originally-claimed task AND any cross-task
    /// `<completed>Y</completed>` IDs the slot/iteration emitted.
    pub completed_task_ids: Vec<String>,
    /// Number of `<key-decision>` tags successfully extracted and stored.
    pub key_decisions_count: u32,
    /// Number of `<task-status>` tags successfully applied.
    pub status_updates_applied: u32,
    /// Number of new learnings extracted from output. Always 0 when the
    /// `TASK_MGR_NO_EXTRACT_LEARNINGS` env opt-out is in effect.
    pub learnings_extracted: usize,
}

/// Inputs to [`process_iteration_output`]. Carries every reference the
/// pipeline needs across both the sequential and wave call sites.
///
/// Lifetime `'a` ties every borrow together; the struct must always be moved
/// (consumed) into the function call. Holding `&mut Connection`,
/// `&mut IterationOutcome`, and `&mut IterationContext` simultaneously is
/// permitted because the caller hands those out once and never aliases them
/// during the call.
pub struct ProcessingParams<'a> {
    /// Database connection. `&mut` because `apply_status_updates` and
    /// `mark_task_done` take `&mut Connection`.
    pub conn: &'a mut Connection,
    /// Run ID for telemetry / completion provenance.
    pub run_id: &'a str,
    /// 1-based iteration number (used by progress logging and key-decisions
    /// insertion).
    pub iteration: u32,
    /// Task ID the iteration was claimed against, if any. `None` matches the
    /// "no claimed task" early return in the sequential path.
    pub task_id: Option<&'a str>,
    /// Raw stdout from Claude. Source for `<completed>` / `<task-status>` /
    /// `<key-decision>` parsing AND for the already-complete fallback.
    pub output: &'a str,
    /// Optional structured stream-json conversation (preferred input for
    /// learning extraction when present, falls back to `output` otherwise).
    pub conversation: Option<&'a str>,
    /// Learnings shown to Claude this iteration; threaded back from
    /// `PromptResult.shown_learning_ids` (sequential) or
    /// `SlotPromptBundle.shown_learning_ids` (wave).
    pub shown_learning_ids: &'a [i64],
    /// Mutable iteration outcome. The pipeline MAY upgrade this to
    /// `Completed` when retroactive completion is detected (see invariants).
    pub outcome: &'a mut IterationOutcome,
    /// Working directory used for git-commit detection. In wave mode this is
    /// the slot's ephemeral worktree (which has the commit but on a branch
    /// not yet merged — hence the skip flag).
    pub working_root: &'a Path,
    /// Number of `git log` entries to scan for the `-completed` suffix.
    pub git_scan_depth: usize,
    /// Wave mode passes `true` so the pipeline never inspects git history
    /// during the per-slot pass — git-commit detection runs once at the
    /// `run_wave_iteration` boundary after merges complete.
    /// Sequential mode passes `false`.
    ///
    /// Critical: the already-complete fallback MUST fire in both modes
    /// (this is the wave-mode parity fix the PRD calls out).
    pub skip_git_completion_detection: bool,
    /// Path to the PRD JSON for `passes: true` reconciliation via
    /// `update_prd_task_passes` and `mark_task_done`.
    pub prd_path: &'a Path,
    /// PRD task prefix (e.g. "5d1118de") for ID normalization.
    pub task_prefix: Option<&'a str>,
    /// Path to the per-PRD progress log so the pipeline can attribute
    /// status-tag dispatch.
    pub progress_path: &'a Path,
    /// `--dir` (DB directory) for embedding scheduling on extracted
    /// learnings via `LearningWriter`.
    pub db_dir: &'a Path,
    /// Signal flag, threaded through to `extract_learnings_from_output` so
    /// Ctrl-C aborts the extraction subprocess.
    pub signal_flag: &'a SignalFlag,
    /// Iteration context. The pipeline updates `crash_tracker` and the
    /// `crashed_last_iteration` per-task crash map.
    pub ctx: &'a mut IterationContext,
    /// Files modified by the iteration's task (from task metadata). Used
    /// only by `progress::log_iteration` — the pipeline does not consult
    /// this for completion detection.
    pub files_modified: &'a [String],
    /// Effective model used for this iteration (post-crash-escalation).
    /// Threaded into the progress log entry. `None` for early-exit paths.
    pub effective_model: Option<&'a str>,
    /// Effective `--effort` level for this iteration. Threaded into the
    /// progress log entry. `None` when difficulty is unset/unknown or for
    /// early-exit paths.
    pub effective_effort: Option<&'static str>,
    /// Slot index when the pipeline runs from a parallel wave; `None` for
    /// the sequential `run_loop` call site. Threaded into the progress log
    /// entry header (`Slot N`) so wave entries are distinguishable.
    pub slot_index: Option<usize>,
}

/// Run the shared post-Claude pipeline.
///
/// See module docs for the full list of behaviors. Returns a
/// [`ProcessingOutcome`] aggregating completion counts and side-effect
/// metrics; mutates `params.outcome` and `params.ctx` in place.
///
/// Intended for crate-internal use by `run_iteration` (sequential) and
/// `process_slot_result` (wave) once FEAT-005 / FEAT-006 wire the call
/// sites; surface is `pub` only so the integration test in
/// `tests/iteration_pipeline.rs` can pin the contract.
pub fn process_iteration_output(params: ProcessingParams<'_>) -> ProcessingOutcome {
    let ProcessingParams {
        conn,
        run_id,
        iteration,
        task_id,
        output,
        conversation,
        shown_learning_ids,
        outcome,
        working_root,
        git_scan_depth,
        skip_git_completion_detection,
        prd_path,
        task_prefix,
        progress_path,
        db_dir,
        signal_flag,
        ctx,
        files_modified,
        effective_model,
        effective_effort,
        slot_index,
    } = params;

    let mut result = ProcessingOutcome::default();
    // Dedup set: the same task ID may surface across multiple completion
    // branches in one pass (status-tag, completed-tag, git, scan, fallback).
    // Mirrors `counted_this_iteration` from engine.rs:3286 and the per-slot
    // `counted` HashSet from process_slot_result (engine.rs:1136).
    let mut completed_set: HashSet<String> = HashSet::new();

    // Step 1: progress log entry. FEAT-005 widened `ProcessingParams` so the
    // sequential call site no longer needs to log separately; FEAT-006 wires
    // the wave call site through the same path.
    progress::log_iteration(progress::LogIterationParams {
        progress_path,
        iteration,
        task_id,
        outcome,
        files: files_modified,
        model: effective_model,
        effort: effective_effort,
        slot: slot_index,
    });

    // Step 2: extract `<key-decision>` tags and persist.
    let key_decisions = detection::extract_key_decisions(output);
    for decision in &key_decisions {
        match key_decisions_db::insert_key_decision(
            conn,
            run_id,
            task_id,
            i64::from(iteration),
            decision,
        ) {
            Ok(_) => result.key_decisions_count += 1,
            Err(e) => tracing::warn!("failed to store key decision '{}': {}", decision.title, e),
        }
    }

    // Step 3: side-band `<task-status>` dispatch.
    let status_updates = detection::extract_status_updates(output);
    // FEAT-010: `apply_status_updates` is a deprecated shim around
    // `TaskLifecycle::apply`; this call site is documented in the prompt
    // ("Common Wiring Failures") and stays during the Phase 1 migration.
    #[allow(deprecated)]
    let status_results: Vec<(String, detection::TaskStatusChange, bool)> =
        if status_updates.is_empty() {
            Vec::new()
        } else {
            apply_status_updates(
                conn,
                &status_updates,
                Some(run_id),
                Some(prd_path),
                task_prefix,
                Some(progress_path),
                Some(db_dir),
                Some(ctx),
            )
        };
    // ProcessingOutcome.status_updates_applied preserves its external semantics:
    // count of dispatches that succeeded (applied=true).
    result.status_updates_applied = status_results.iter().filter(|(_, _, ok)| *ok).count() as u32;

    // Step 4: completion ladder for the claimed task.
    //
    // Unlike the legacy sequential gate (engine.rs:3279) which short-circuits
    // when `outcome == Empty`, the pipeline always runs the ladder when a
    // task_id is present — the test contract pins this so a `<completed>` tag
    // can retroactively flip an Empty outcome to Completed.
    if let Some(claimed_id) = task_id {
        let mut task_marked_done = false;

        // 4a: <task-status>...:done</task-status> per-entry success gate (M2 fix).
        //
        // The claimed task's specific (id, Done, true) tuple drives the outcome
        // flip. A peer's failure no longer falsely marks the claimed task done
        // (the global `applied > 0` flag was the bug — learning #2238).
        // Peer Done successes are recorded in `completed_task_ids` (so the
        // wave aggregator sees them) but do NOT flip outcome — the outcome
        // describes whether THIS iteration's claimed work landed.
        for (id, status, applied) in &status_results {
            if !*applied || !matches!(status, detection::TaskStatusChange::Done) {
                continue;
            }
            if id == claimed_id {
                task_marked_done = true;
                record_completion(claimed_id, &mut completed_set, &mut result, outcome);
                ui::emit(&format!(
                    "Task {} completed (detected from <task-status> tag)",
                    claimed_id
                ));
            } else if completed_set.insert(id.clone()) {
                result.tasks_completed += 1;
                result.completed_task_ids.push(id.clone());
                ui::emit(&format!(
                    "Peer task {} completed (detected from <task-status> tag)",
                    id
                ));
            }
        }

        // 4b: <completed> tags. Multiple tags may complete cross-task IDs
        // (peer tasks Claude finished alongside the claimed one).
        let completed_tags = parse_completed_tasks(output);
        for completed_id in &completed_tags {
            match mark_task_done(
                conn,
                completed_id,
                run_id,
                None,
                prd_path,
                task_prefix,
                Some(ctx),
            ) {
                Ok(()) => {
                    if completed_id == claimed_id {
                        task_marked_done = true;
                    }
                    record_completion(completed_id, &mut completed_set, &mut result, outcome);
                    ui::emit(&format!(
                        "Task {} completed (detected from <completed> tag)",
                        completed_id
                    ));
                }
                Err(e) => {
                    // Non-fatal: a duplicate `<completed>` after a status-tag
                    // dispatch already moved the row to `done` will fail
                    // here (transition guard). The dedup set above keeps the
                    // counters honest; the warning preserves visibility
                    // (tracing::warn! still shows on the console at WARN+).
                    tracing::warn!("mark_task_done({}) failed: {}", completed_id, e);
                }
            }
        }

        // 4c: git-commit + 4d: output-scan fallback (skip-git mode-aware).
        //
        // Sequential mode (skip_git=false): try git first; only scan output
        // when git found nothing. Wave mode (skip_git=true): never touch git
        // (the commit is on an unmerged ephemeral branch); always fall back
        // to output scan so cross-task completions still register.
        if completed_tags.is_empty() {
            let mut completion_recorded = false;

            if !skip_git_completion_detection
                && let Some(commit_hash) =
                    check_git_for_task_completion(working_root, claimed_id, git_scan_depth)
            {
                let task_ids = [claimed_id.to_string()];
                match complete_cmd::complete(
                    conn,
                    &task_ids,
                    Some(run_id),
                    Some(&commit_hash),
                    false,
                ) {
                    Ok(_) => {
                        task_marked_done = true;
                        completion_recorded = true;
                        record_completion(claimed_id, &mut completed_set, &mut result, outcome);
                        if let Err(e) =
                            update_prd_task_passes(prd_path, claimed_id, true, task_prefix)
                        {
                            tracing::warn!("failed to update PRD for task {}: {}", claimed_id, e);
                        } else {
                            ui::emit(&format!(
                                "Task {} completed (commit {})",
                                claimed_id,
                                &commit_hash[..7.min(commit_hash.len())]
                            ));
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to mark task {} as done in DB: {}", claimed_id, e);
                    }
                }
            }

            if !completion_recorded {
                let scanned = scan_output_for_completed_tasks(output, conn, task_prefix);
                for completed_id in &scanned {
                    let ids = [completed_id.clone()];
                    match complete_cmd::complete(conn, &ids, Some(run_id), None, false) {
                        Ok(_) => {
                            if completed_id == claimed_id {
                                task_marked_done = true;
                            }
                            record_completion(
                                completed_id,
                                &mut completed_set,
                                &mut result,
                                outcome,
                            );
                            if let Err(e) =
                                update_prd_task_passes(prd_path, completed_id, true, task_prefix)
                            {
                                tracing::warn!(
                                    "failed to update PRD for task {}: {}",
                                    completed_id,
                                    e
                                );
                            } else {
                                ui::emit(&format!(
                                    "Task {} completed (detected from output)",
                                    completed_id
                                ));
                            }
                        }
                        Err(e) => {
                            tracing::warn!("failed to mark task {} as done: {}", completed_id, e);
                        }
                    }
                }
            }
        }

        // 4e: already-complete fallback. Fires in BOTH skip-git modes — the
        // PRD parity fix that today's process_slot_result misses entirely.
        if !task_marked_done
            && detection::is_task_reported_already_complete(output, claimed_id, task_prefix)
            && let Ok(()) = mark_task_done(
                conn,
                claimed_id,
                run_id,
                None,
                prd_path,
                task_prefix,
                Some(ctx),
            )
        {
            record_completion(claimed_id, &mut completed_set, &mut result, outcome);
            ui::emit(&format!(
                "Task {} completed (reported as already done)",
                claimed_id
            ));
        }
    }

    // Completion ladder is done. Record success once if any task completed this
    // pass rather than once per completed task ID inside record_completion.
    if result.tasks_completed > 0 {
        ctx.crash_tracker.record_success();
    }

    // Step 5: extract learnings from the iteration output. Prefer the
    // structured stream-json conversation when present; fall back to the
    // raw stdout. The env opt-out keeps tests hermetic.
    let learning_source = conversation.unwrap_or(output);
    if !crate::learnings::ingestion::is_extraction_disabled() && !learning_source.is_empty() {
        match crate::learnings::ingestion::extract_learnings_from_output(
            conn,
            learning_source,
            task_id,
            Some(run_id),
            Some(db_dir),
            Some(signal_flag),
        ) {
            Ok(extraction) => {
                result.learnings_extracted = extraction.learnings_extracted;
                if extraction.learnings_extracted > 0 {
                    ui::emit(&format!(
                        "Extracted {} learning(s) from output",
                        extraction.learnings_extracted
                    ));
                }
            }
            Err(e) => tracing::warn!("learning extraction failed: {}", e),
        }
    }

    // Step 6: bandit feedback for shown learnings (gates on Completed
    // outcome internally, so we pass the post-mutation `outcome`).
    if let Err(e) = feedback::record_iteration_feedback(conn, shown_learning_ids, outcome) {
        tracing::warn!("failed to record iteration feedback: {}", e);
    }

    // Step 7: per-task crash-tracking write. Keys by task_id so the map size
    // is bounded by active task count (not iteration count) — contract from
    // the FEAT-007 AC.
    //
    // Skip when the claimed task went terminal this iteration: the completion
    // ladder OR apply_status_updates already pruned the entry (CODE-FIX-003).
    // Re-inserting here would undo that prune and keep a dead task's entry
    // in the map past its active lifetime.
    //
    // `went_terminal_via_done` covers the Done branch (completion ladder
    // populates `completed_task_ids`). `went_terminal_via_status` covers the
    // sibling terminal status-tags (`:failed`, `:skipped`, `:irrelevant`)
    // which `apply_status_updates` prunes but the completion ladder does not
    // record. Without this second arm, Step 7 would re-insert the entry the
    // status-tag dispatch just removed (Learning #2304).
    if let Some(claimed_id) = task_id {
        let went_terminal_via_done = result.completed_task_ids.iter().any(|id| id == claimed_id);
        let went_terminal_via_status = status_results.iter().any(|(id, status, applied)| {
            *applied
                && id == claimed_id
                && matches!(
                    status,
                    detection::TaskStatusChange::Done
                        | detection::TaskStatusChange::Failed
                        | detection::TaskStatusChange::Skipped
                        | detection::TaskStatusChange::Irrelevant
                )
        });
        if !went_terminal_via_done && !went_terminal_via_status {
            ctx.crashed_last_iteration.insert(
                claimed_id.to_string(),
                matches!(outcome, IterationOutcome::Crash(_)),
            );
        }
    }

    result
}

/// Apply the post-completion bookkeeping shared across every branch of the
/// completion ladder: dedup the task ID, increment counters, and mutate the
/// outcome to `Completed`. Crash-tracker reset happens once after the full
/// ladder via the `tasks_completed > 0` gate in `process_iteration_output`.
fn record_completion(
    task_id: &str,
    completed_set: &mut HashSet<String>,
    result: &mut ProcessingOutcome,
    outcome: &mut IterationOutcome,
) {
    if completed_set.insert(task_id.to_string()) {
        result.tasks_completed += 1;
        result.completed_task_ids.push(task_id.to_string());
    }
    *outcome = IterationOutcome::Completed;
}
