//! Outer autonomous loop orchestration.
//!
//! Carved out of `engine.rs` (PRD 02, FEAT-005). This module owns the
//! top-level `run_loop` — env setup → git validation → init PRD → run
//! lifecycle → iterate (dispatching to the sequential `iteration::run_iteration`
//! or the parallel `wave_scheduler::run_parallel_wave` at the iteration
//! boundary) → auto-review → cleanup — plus the run-lifecycle helpers it owns:
//! `on_run_completed`, `record_session_guidance`, `prompt_pending_key_decisions`,
//! `trigger_human_reviews`, and `query_human_review_tasks`.
//!
//! The linear startup phase (Steps 1–16: env/git/PRD validation, lock + DB open,
//! worktree / parallel-slot setup, run-session begin, signal-handler install,
//! banner/usage setup) lives in [`crate::loop_engine::startup`]; `run_loop`
//! calls `startup::initialize_loop` once, destructures the returned
//! [`LoopInitContext`], and runs the iteration loop + post-loop teardown.
//!
//! The shared hand-off data types (`IterationContext`, `LoopRunConfig`,
//! `LoopResult`, the slot/wave structs, etc.), the per-iteration
//! runner-resolution helpers (`resolve_effective_runner`,
//! `apply_review_model_override`), and the `<task-status>` dispatcher
//! (`apply_status_updates`, consumed by `iteration_pipeline`) remain in
//! `engine.rs` and are glob-imported here. `engine.rs` re-exports `run_loop`
//! and `on_run_completed` `pub` so the external import paths callers and
//! integration tests rely on stay valid (FR-008); `mod.rs` also re-exports
//! `run_loop` as the canonical public entry point.
//!
//! **Signal handler ownership**: `startup::initialize_loop` constructs the
//! `SignalFlag` and arms it; `run_loop` receives it on the `LoopInitContext`
//! and threads it through `WaveIterationParams` / `SlotIterationParams` /
//! `IterationContext` exactly as before.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::commands::decisions::find_option;
use crate::commands::run as run_cmd;
use crate::db::schema::key_decisions as key_decisions_db;
use crate::loop_engine::calibrate;
use crate::loop_engine::config::{self, IterationOutcome, PermissionMode};
use crate::loop_engine::deadline;
use crate::loop_engine::display;
use crate::loop_engine::guidance::SessionGuidance;
use crate::loop_engine::iteration_pipeline;
use crate::loop_engine::oauth;
use crate::loop_engine::prd_reconcile::hash_file;
use crate::loop_engine::progress;
use crate::loop_engine::reactions;
use crate::loop_engine::signals;
use crate::loop_engine::startup::{self, LoopInitContext};
use crate::loop_engine::wave_scheduler::classify_drained_queue;
use crate::loop_engine::worktree;
use crate::models::RunStatus;
use crate::output::ui;

use crate::loop_engine::engine::*;

/// Run the full autonomous agent loop.
///
/// This is the top-level orchestrator called from `main.rs`:
/// 1. Load .env and validate git repo
/// 2. Resolve paths and open DB
/// 3. Read PRD metadata (branch name, task count)
/// 4. Begin a run session
/// 5. Create deadline if hours specified
/// 6. Install signal handlers
/// 7. Iterate until done, blocked, max iterations, or signal
/// 8. End run, cleanup, return exit code
///
/// # Exit codes
/// - 0: success (all tasks complete) or graceful stop
/// - 1: error, max crashes, max stale, or max iterations reached
/// - 2: blocked
/// - 130: SIGINT
/// - 143: SIGTERM
pub async fn run_loop(mut run_config: LoopRunConfig) -> LoopResult {
    // Steps 1–16 (env/git/PRD validation, lock + DB open + migrations + stale
    // recovery, worktree / parallel-slot setup, run-session begin, signal-handler
    // install, banner/usage setup) live in `startup::initialize_loop`. On any
    // startup failure it returns the exact `LoopResult` (exit code + worktree
    // path) the inline code returned at that point — propagate it unchanged.
    let LoopInitContext {
        // Held (unused) for the lifetime of the run so a concurrent same-prefix
        // loop cannot start; released on drop at the end of `run_loop`.
        loop_lock: _loop_lock,
        mut conn,
        paths,
        mut prd_hash,
        live_prd_file,
        branch_name,
        task_prefix,
        default_model,
        project_config,
        project_default_model,
        user_default_model,
        prd_implicit_overlap_files,
        external_repo_path,
        actual_worktree_path,
        working_root,
        mut parallel_active,
        slot_worktree_paths,
        max_iterations,
        prd_basename,
        run_id,
        signal_flag,
        steering,
        mut permission_mode,
        usage_params,
    } = match startup::initialize_loop(&mut run_config) {
        Ok(init) => init,
        Err(early_exit) => return early_exit,
    };
    // `steering.md` existence was checked once in startup; borrow the owned path
    // as the `Option<&Path>` the iteration params expect.
    let steering = steering.as_deref();

    // Step 17: Run iteration loop
    let start_time = Instant::now();
    let inter_iteration_delay = Duration::from_secs(run_config.config.iteration_delay_secs);
    let mut ctx = IterationContext::new(run_config.config.max_crashes as u32);
    let mut iterations_completed: u32 = 0;
    let mut tasks_completed: u32 = 0;
    let mut last_claimed_task: Option<String> = None;
    let mut exit_code: i32 = 0;
    let mut exit_reason = String::from("max iterations reached");
    let mut final_run_status = RunStatus::Aborted;
    let mut was_stopped = false; // set true only when a .stop file halted the loop

    // Rotate progress file before starting iterations to bound context size
    progress::rotate_progress(&paths.progress_file);

    let mut iteration: u32 = 0;
    while iteration < max_iterations {
        iteration += 1; // 1-based, incremented at top
        // Pre-iteration: refresh OAuth token if usage checking enabled
        if usage_params.enabled {
            oauth::ensure_valid_token();
        }

        // Check deadline
        if deadline::check_deadline(&paths.tasks_dir, &prd_basename) {
            ui::emit("Deadline reached, stopping loop");
            exit_reason = "deadline reached".to_string();
            exit_code = 0;
            break;
        }

        // Hot-reload permission mode: re-resolve each iteration so config.json
        // edits mid-loop take effect without restarting.
        let iter_permission_mode = config::resolve_permission_mode(&run_config.db_dir);
        if iter_permission_mode != permission_mode {
            ui::emit(&format!(
                "\x1b[36m[info]\x1b[0m Permission mode changed: {} → {}",
                permission_mode, iter_permission_mode
            ));
            permission_mode = iter_permission_mode;
        }

        // Re-import PRD if Claude modified it during the previous iteration.
        // Use live_prd_file (worktree copy) since Claude edits in the worktree.
        let current_hash = hash_file(&live_prd_file);
        if current_hash != prd_hash {
            ui::emit("PRD file changed, re-importing tasks...");
            if let Err(e) = crate::commands::init(
                &run_config.db_dir,
                &[&live_prd_file],
                false, // force
                true,  // append
                true,  // update_existing
                false, // dry_run
                run_config.prefix_mode.clone(),
            ) {
                tracing::warn!("PRD re-import failed: {} (continuing)", e);
            }
            prd_hash = current_hash;
        }

        let elapsed = start_time.elapsed().as_secs();

        // Parallel wave dispatch (FEAT-010). When `parallel_active` we run
        // a wave of slot iterations and skip the rest of the sequential
        // post-processing — `run_wave_iteration` performs its own per-slot
        // logging, status dispatch, completion detection, crash policy, and
        // terminal-condition checks. The outer loop only needs to track the
        // counters and decide when to break.
        if parallel_active {
            // Invariant: `parallel_active` is only set when `branch_name` is
            // Some (see step 9.5). `debug_assert!` traps in tests if a future
            // change breaks the invariant; release builds keep the graceful
            // sequential fallthrough rather than panicking on an inconsistency.
            debug_assert!(
                branch_name.is_some(),
                "parallel_active=true must imply branch_name is Some"
            );
            let Some(branch) = branch_name.as_deref() else {
                ui::emit_err(
                    "Warning: parallel_active=true but branch_name is None; \
                     falling through to sequential iteration",
                );
                parallel_active = false;
                continue;
            };
            // Materialize wave-scope inputs that need stable lifetimes for the
            // borrowed fields on `WaveIterationParams`.
            let wave_session_guidance = ctx.session_guidance.format_for_prompt();
            let wave_params = WaveIterationParams {
                conn: &mut conn,
                db_dir: &run_config.db_dir,
                source_root: &run_config.source_root,
                branch,
                parallel_slots: run_config.config.parallel_slots,
                slot_worktree_paths: &slot_worktree_paths,
                iteration,
                max_iterations,
                elapsed_secs: elapsed,
                run_id: &run_id,
                base_prompt_path: &paths.prompt_file,
                permission_mode: &permission_mode,
                signal_flag: &signal_flag,
                default_model: default_model.as_deref(),
                project_default_model: project_default_model.as_deref(),
                user_default_model: user_default_model.as_deref(),
                verbose: run_config.config.verbose,
                task_prefix: task_prefix.as_deref(),
                prd_path: paths.prd_file.as_path(),
                progress_path: paths.progress_file.as_path(),
                tasks_dir: paths.tasks_dir.as_path(),
                external_repo_path: external_repo_path.as_deref(),
                external_git_scan_depth: run_config.config.external_git_scan_depth,
                inter_iteration_delay,
                steering_path: steering,
                session_guidance: &wave_session_guidance,
                prd_implicit_overlap_files: &prd_implicit_overlap_files,
                project_config: &project_config,
                usage_params: &usage_params,
            };
            let outcome = run_wave_iteration(wave_params, &mut ctx);
            tasks_completed += outcome.tasks_completed;
            // FEAT-013: the iteration-budget rule lives in one helper shared
            // with the sequential path below. A non-consuming wave (the
            // FEAT-006 B2 rate-limit retry) gives back the loop-bound iteration
            // so a persistently rate-limited account doesn't burn its budget on
            // waits; a consuming wave advances `iterations_completed`. Routing
            // both branches through `account_iteration_budget` keeps the two
            // execution paths from drifting on the rule.
            reactions::account_iteration_budget(reactions::IterationBudgetParams {
                iteration: &mut iteration,
                iterations_completed: &mut iterations_completed,
                consumes_budget: outcome.iteration_consumed,
            });
            if outcome.was_stopped {
                was_stopped = true;
            }

            // FEAT-006 B3: a rate-limit retry wave returned BEFORE merge-back
            // carrying no merge outcomes. Skip the FEAT-002 reset/halt check —
            // running it with this wave's empty `failed_merges` would zero the
            // cascade-halt streak (`consecutive_merge_fail_waves`). The wave
            // also has no terminal, so `continue` straight to the next iteration.
            if outcome.rate_limited_retry {
                continue;
            }

            // FEAT-002: reset/halt contract on parallel-slot merge-back
            // failures. Logic lives in `apply_merge_fail_reset_and_halt_check`
            // so it can be unit-tested in isolation.
            let halt_threshold = project_config.merge_fail_halt_threshold;
            if let MergeFailHaltDecision::Halt {
                exit_code: halt_code,
                exit_reason: halt_reason,
            } = apply_merge_fail_reset_and_halt_check(
                &mut conn,
                &mut ctx,
                branch,
                &outcome.failed_merges,
                halt_threshold,
            ) {
                exit_code = halt_code;
                exit_reason = halt_reason;
                break;
            }

            if let Some(t) = outcome.terminal {
                exit_code = t.exit_code;
                exit_reason = t.reason;
                if let Some(s) = t.run_status {
                    final_run_status = s;
                }
                break;
            }
            // Suppress unused-elapsed warning when the sequential branch is
            // skipped — the value is recomputed next iteration anyway.
            let _ = elapsed;
            continue;
        }

        let mut iteration_params = IterationParams {
            conn: &mut conn,
            db_dir: &run_config.db_dir,
            project_root: &working_root,
            tasks_dir: &paths.tasks_dir,
            iteration,
            max_iterations,
            run_id: &run_id,
            base_prompt_path: &paths.prompt_file,
            steering_path: steering,
            inter_iteration_delay,
            signal_flag: &signal_flag,
            elapsed_secs: elapsed,
            verbose: run_config.config.verbose,
            usage_params: &usage_params,
            prd_path: Some(paths.prd_file.as_path()),
            task_prefix: task_prefix.as_deref(),
            default_model: default_model.as_deref(),
            project_default_model: project_default_model.as_deref(),
            user_default_model: user_default_model.as_deref(),
            permission_mode: &permission_mode,
            batch_sibling_prds: &run_config.batch_sibling_prds,
            project_config: &project_config,
        };

        let mut result = match run_iteration(&mut ctx, &mut iteration_params) {
            Ok(r) => r,
            Err(e) => {
                ui::emit_err(&format!("Iteration error: {}", e));
                exit_code = 1;
                exit_reason = format!("iteration error: {}", e);
                break;
            }
        };

        // Early exit on signal — skip all post-processing (git checks,
        // reconciliation, etc.) to respond to Ctrl+C immediately.
        if signal_flag.is_signaled() {
            exit_code = 130;
            exit_reason = "signal received".to_string();
            break;
        }

        // Track the claimed task before the pipeline runs. Cleared below if
        // the pipeline reports the claimed task as completed.
        last_claimed_task = result.task_id.clone();

        // Update run with last files (stays at the call site — pipeline only
        // covers post-Claude completion / learning bookkeeping).
        if let Err(e) = run_cmd::update(
            &conn,
            &run_id,
            ctx.last_commit.as_deref(),
            Some(&result.files_modified),
        ) {
            tracing::warn!("failed to update run: {}", e);
        }

        // Run the shared post-Claude pipeline: progress logging, key-decision
        // extraction, `<task-status>` dispatch, completion ladder
        // (status-tag → completed-tag → git/scan → already-complete fallback),
        // learning extraction, bandit feedback, and per-task crash-tracking.
        // Wrapper-commit, external-git reconciliation, and human-review
        // triggering stay at this call site (FEAT-005).
        let processing_outcome =
            iteration_pipeline::process_iteration_output(iteration_pipeline::ProcessingParams {
                conn: &mut conn,
                run_id: &run_id,
                iteration,
                task_id: result.task_id.as_deref(),
                output: &result.output,
                conversation: result.conversation.as_deref(),
                shown_learning_ids: &result.shown_learning_ids,
                outcome: &mut result.outcome,
                working_root: &working_root,
                git_scan_depth: run_config.config.git_scan_depth,
                skip_git_completion_detection: false,
                prd_path: &paths.prd_file,
                task_prefix: task_prefix.as_deref(),
                progress_path: &paths.progress_file,
                db_dir: &run_config.db_dir,
                signal_flag: &signal_flag,
                ctx: &mut ctx,
                files_modified: &result.files_modified,
                effective_model: result.effective_model.as_deref(),
                effective_effort: result.effective_effort,
                slot_index: None,
            });
        tasks_completed += processing_outcome.tasks_completed;
        result.key_decisions_count = processing_outcome.key_decisions_count;

        // Clear `last_claimed_task` only if the pipeline marked the claimed
        // task itself as completed (any branch of the completion ladder).
        // Cross-task `<completed>Y</completed>` completions do NOT clear it —
        // the claimed task may still be in flight.
        let claimed_was_completed = result
            .task_id
            .as_ref()
            .map(|tid| {
                processing_outcome
                    .completed_task_ids
                    .iter()
                    .any(|c| c == tid)
            })
            .unwrap_or(false);
        if claimed_was_completed {
            last_claimed_task = None;
        }

        // Post-completion reactions (#8 wrapper-commit, #9 external-git shadow,
        // #10 human-review) — converged into `reactions::post_completion::
        // react_to_completions` so the wave path fires the identical set
        // (FEAT-010). Skipped on an `Empty` iteration: nothing completed, which
        // matches the pre-convergence guards (the external-git + human-review
        // blocks already short-circuited on `Empty`, and the wrapper-commit was
        // gated on `claimed_was_completed`, false whenever the outcome is Empty).
        if !matches!(result.outcome, IterationOutcome::Empty) {
            let pc_params = reactions::post_completion::PostCompletionParams {
                run_id: &run_id,
                iteration,
                working_root: working_root.as_path(),
                prd_file: &paths.prd_file,
                task_prefix: task_prefix.as_deref(),
                default_model: default_model.as_deref(),
                permission_mode: &permission_mode,
                external_repo_path: external_repo_path.as_deref(),
                external_git_scan_depth: run_config.config.external_git_scan_depth as u32,
                wrapper_commit: true,
            };
            let pc_outcome = reactions::post_completion::react_to_completions(
                &mut conn,
                &processing_outcome.completed_task_ids,
                &pc_params,
                &mut ctx.session_guidance,
            );

            if let Some(hash) = pc_outcome.wrapper_commit_hash {
                ctx.last_commit = Some(hash);
            }

            // Fold any external-git completions into iteration accounting — the
            // same bookkeeping the inline external-git block did before the
            // convergence.
            if !pc_outcome.external_reconciled.is_empty() {
                let count = pc_outcome.external_reconciled.len();
                tasks_completed += count as u32;

                // Override outcome so stale/crash trackers reset — tasks were
                // actually completed.
                result.outcome = IterationOutcome::Completed;
                ctx.crash_tracker.record_success();

                ui::emit(&format!(
                    "Post-iteration reconciliation: marked {} task(s) done",
                    count
                ));
                // Clear tracker if the claimed task was reconciled as done.
                if let Some(ref claimed) = last_claimed_task {
                    let status: Option<String> = conn
                        .query_row(
                            "SELECT status FROM tasks WHERE id = ?",
                            [claimed.as_str()],
                            |row| row.get(0),
                        )
                        .ok();
                    if status.as_deref() == Some("done") {
                        last_claimed_task = None;
                    }
                }
            }
        }

        // FEAT-014: sequential transient-backend reaction (account-global).
        // The single sequential call site of `react_to_transient` — the
        // convergence point for BOTH origins of a `TransientBackend` outcome:
        // the Claude path (`analyze_output` inside `run_iteration`) and the Grok
        // path (the `TaskMgrError::TransientBackend` early-return mapped in
        // `iteration.rs`, which never reaches a `run_iteration`-internal
        // reaction). Called unconditionally so a non-transient iteration resets
        // the attempt counter via the reaction's `None` branch. Runs AFTER the
        // pipeline + external-git reconcile (so a task completed out-of-band is
        // not backed off) and BEFORE the budget/retry-tracking below.
        //
        // - `WaitedAndRetry`: the outcome stays `TransientBackend`, so the
        //   budget match gives the iteration back and the retry-tracking guard
        //   skips it — identical to `RateLimit` (B2/B3).
        // - `Escalate`: the outcome is rewritten to `Crash(RuntimeError)` so it
        //   falls through to the crash/abort path (budget consumed +
        //   `handle_task_failure` runs).
        // - `Stop`: `.stop` during the backoff → stop the loop (terminal exit).
        let transient_reaction = {
            let items = [reactions::account::OutputReactionItem {
                task_id: result.task_id.as_deref(),
                outcome: &result.outcome,
                output: &result.output,
            }];
            let tparams = reactions::account::TransientReactionParams {
                tasks_dir: paths.tasks_dir.as_path(),
                prefix: task_prefix.as_deref().unwrap_or(""),
                run_id: &run_id,
                max_attempts: reactions::account::TRANSIENT_MAX_ATTEMPTS,
                base_wait_secs: reactions::account::TRANSIENT_BACKOFF_BASE_SECS,
                max_wait_secs: reactions::account::TRANSIENT_BACKOFF_MAX_SECS,
            };
            reactions::account::react_to_transient(
                &mut conn,
                &items,
                &tparams,
                &mut ctx.transient_backend_attempts,
            )
        };
        match transient_reaction {
            reactions::account::TransientReaction::None
            | reactions::account::TransientReaction::WaitedAndRetry => {}
            reactions::account::TransientReaction::Stop => {
                result.should_stop = true;
            }
            reactions::account::TransientReaction::Escalate => {
                result.outcome = IterationOutcome::Crash(config::CrashType::RuntimeError);
            }
        }

        // Track iteration count (skip reorders, rate limits, and the
        // transient-backend WaitedAndRetry — all give back the iteration so a
        // persistently unavailable backend / rate-limited account doesn't burn
        // its budget on waits). FEAT-013: the budget rule itself lives in
        // `account_iteration_budget`, shared with the wave path above so the
        // give-back and the stat advance cannot drift between the two paths.
        let consumes_budget = !matches!(
            result.outcome,
            IterationOutcome::Reorder(_)
                | IterationOutcome::RateLimit
                | IterationOutcome::TransientBackend { .. }
        );
        reactions::account_iteration_budget(reactions::IterationBudgetParams {
            iteration: &mut iteration,
            iterations_completed: &mut iterations_completed,
            consumes_budget,
        });

        // Retry tracking: increment consecutive_failures for non-Completed task failures.
        // Excluded: Empty (no task attempted), Reorder (not a failure), RateLimit (external).
        // FEAT-007: also exclude Crash(GrokAuthFailure) — an xAI auth lapse is an operator
        // problem, not a task failure; incrementing here would push a healthy task toward
        // auto_block_task with a misleading reason.
        // FEAT-014: exclude TransientBackend — a 5xx/overloaded WaitedAndRetry must not burn
        // crash budget. (On escalation the reaction already rewrote the outcome to
        // Crash(RuntimeError) above, which IS tracked here — the crash/abort path.)
        if let Some(ref task_id) = result.task_id
            && !matches!(
                result.outcome,
                IterationOutcome::Completed
                    | IterationOutcome::Empty
                    | IterationOutcome::Reorder(_)
                    | IterationOutcome::RateLimit
                    | IterationOutcome::TransientBackend { .. }
                    | IterationOutcome::Crash(config::CrashType::GrokAuthFailure)
                    | IterationOutcome::Crash(config::CrashType::CodexAuthFailure)
            )
            && let Err(e) = crate::loop_engine::recovery::handle_task_failure_with_runner(
                &mut conn,
                task_id,
                iteration as i64,
                &mut ctx,
                result.effective_runner,
                project_config.fallback_runner.as_ref(),
                project_config.primary_runner.as_ref(),
                project_default_model.as_deref(),
                user_default_model.as_deref(),
            )
        {
            tracing::warn!("failed to start retry tracking transaction: {}", e);
        }

        // Track consecutive stale iterations and abort if stuck
        if matches!(result.outcome, IterationOutcome::NoEligibleTasks) {
            // Drained-but-stuck short-circuit (shared with the wave path): if
            // no schedulable work remains and only blocked/skipped tasks are
            // left, exit immediately with the classifier's named verdict
            // instead of spinning to the stale-abort threshold. `classify`
            // never returns the clean (exit 0) variant here — `run_iteration`
            // already returns `Completed` for that case — but we apply whatever
            // it reports so the two paths share one source of truth.
            if let Some(drained) = classify_drained_queue(&conn, task_prefix.as_deref()) {
                ui::emit(&drained.reason);
                exit_code = drained.exit_code;
                exit_reason = drained.reason;
                final_run_status = drained.run_status;
                break;
            }
            ctx.stale_tracker.mark_stale();
            if ctx.stale_tracker.should_abort() {
                ui::emit_err(&format!(
                    "Aborting: no eligible tasks after {} consecutive stale iterations",
                    ctx.stale_tracker.count()
                ));
                exit_code = 1;
                exit_reason = format!(
                    "no eligible tasks after {} consecutive stale iterations",
                    ctx.stale_tracker.count()
                );
                break;
            }
        } else {
            ctx.stale_tracker.reset_progress();
        }

        // Check for terminal outcomes
        if result.should_stop {
            match &result.outcome {
                IterationOutcome::Completed => {
                    exit_code = 0;
                    exit_reason = "all tasks complete".to_string();
                    final_run_status = RunStatus::Completed;
                }
                IterationOutcome::Blocked => {
                    exit_code = 2;
                    exit_reason = "blocked".to_string();
                }
                IterationOutcome::Crash(_) => {
                    exit_code = 1;
                    exit_reason = "too many crashes".to_string();
                }
                IterationOutcome::Empty if signal_flag.is_signaled() => {
                    // Determine SIGINT vs SIGTERM — we can't distinguish,
                    // so default to SIGINT (130) since that's more common
                    exit_code = 130;
                    exit_reason = "signal received".to_string();
                }
                IterationOutcome::Empty => {
                    // Stop signal file or other empty exit
                    exit_code = 0;
                    exit_reason = "stop signal".to_string();
                    was_stopped = true;
                }
                IterationOutcome::PromptOverflow => {
                    exit_code = 3;
                    exit_reason = "prompt overflow — critical sections exceed budget".to_string();
                }
                _ => {
                    exit_code = 1;
                    exit_reason = "stopped".to_string();
                }
            }
            break;
        }
    }

    // Step 17.5: Reset uncompleted claimed task so it's not stuck in_progress for next run
    if let Some(ref task_id) = last_claimed_task {
        reset_task_to_todo(&mut conn, task_id, "uncompleted task");
    }

    // Step 17.6: Reset any parallel-mode slot tasks still pending. Sequential
    // mode is fully covered by step 17.5 above; the wave path tracks every
    // claimed task in `ctx.pending_slot_tasks` and removes it on `done`, so
    // anything remaining was claimed but never closed (deadline / max-iter
    // exit, slot crash, or output without a `<completed>` tag).
    //
    // Clone the IDs out of ctx so the mutable borrow on conn doesn't conflict
    // with the immutable borrow on ctx.pending_slot_tasks across iterations.
    let pending_slot_task_ids: Vec<String> = ctx
        .pending_slot_tasks
        .iter()
        .filter(|t| Some(*t) != last_claimed_task.as_ref())
        .cloned()
        .collect();
    for task_id in &pending_slot_task_ids {
        reset_task_to_todo(&mut conn, task_id, "uncompleted slot task");
    }

    // Step 18: Record session guidance if any
    record_session_guidance(
        &ctx.session_guidance,
        &paths.progress_file,
        run_config.config.yes_mode,
    );

    // Step 19: End run session
    if let Err(e) = run_cmd::end(&conn, &run_id, final_run_status) {
        tracing::warn!("failed to end run: {}", e);
    }

    // Step 20: Recalibrate weights if completed
    if final_run_status == RunStatus::Completed {
        on_run_completed(&conn, task_prefix.as_deref());
    }

    // Step 21: Cleanup
    deadline::cleanup_deadline(&paths.tasks_dir, &prd_basename);
    signals::cleanup_signal_files_for_prefix(&paths.tasks_dir, task_prefix.as_deref());

    // Step 21.4: Slot worktree cleanup (parallel mode only).
    // Removes ephemeral slot worktrees (slots 1+) and their branches. Slot 0
    // is the loop's main branch worktree and is handled by step 21.5 below.
    // Always runs on shutdown so a crash does not leak stray worktrees.
    if parallel_active
        && let Some(ref branch) = branch_name
        && let Err(e) = worktree::cleanup_slot_worktrees(
            &run_config.source_root,
            branch,
            run_config.config.parallel_slots,
        )
    {
        tracing::warn!(
            "cleanup_slot_worktrees failed: {} — leaving slot worktrees intact",
            e
        );
    }

    // Step 21.5: Worktree cleanup (if a worktree was used)
    if let Some(ref wt_path) = actual_worktree_path {
        let should_cleanup = if run_config.config.cleanup_worktree {
            // --cleanup-worktree flag: always attempt removal
            true
        } else if run_config.config.yes_mode {
            // --yes without --cleanup-worktree: keep worktree (auto-keep)
            false
        } else {
            // Interactive: prompt user (no trailing newline — reply on same line)
            ui::prompt(&format!(
                "Remove worktree at '{}'? [y/N] ",
                wt_path.display()
            ));
            let mut response = String::new();
            let _ = std::io::stdin().read_line(&mut response);
            matches!(response.trim().to_lowercase().as_str(), "y" | "yes")
        };

        if should_cleanup {
            match worktree::remove_worktree(&run_config.source_root, wt_path) {
                Ok(true) => ui::emit(&format!("Worktree '{}' removed.", wt_path.display())),
                Ok(false) => ui::emit_err(&format!(
                    "Warning: worktree '{}' has uncommitted changes — not removed.",
                    wt_path.display()
                )),
                Err(e) => ui::emit_err(&format!(
                    "Warning: failed to remove worktree '{}': {} — continuing.",
                    wt_path.display(),
                    e
                )),
            }
        }
    }

    // Step 21.7: Prompt user to resolve pending key decisions (skip on SIGINT or yes_mode)
    if exit_code != 130 {
        prompt_pending_key_decisions(&conn, &run_id, run_config.config.yes_mode);
    }

    // Step 22: Print final banner
    let total_elapsed = start_time.elapsed().as_secs();
    display::print_final_banner(
        iterations_completed,
        tasks_completed,
        total_elapsed,
        &exit_reason,
        &prd_basename,
    );

    LoopResult {
        exit_code,
        worktree_path: actual_worktree_path,
        branch_name: branch_name.clone(),
        was_stopped,
        tasks_completed,
    }
}

/// Context parameters for the deprecated `trigger_human_reviews` shim.
#[allow(dead_code)] // only the deprecated shim below references this
struct HumanReviewParams<'a> {
    completion_epoch_start: i64,
    iteration: u32,
    session_guidance: &'a mut SessionGuidance,
    prd_file: &'a Path,
    task_prefix: Option<&'a str>,
    default_model: Option<&'a str>,
    permission_mode: &'a PermissionMode,
}

/// Query tasks that need human review for the current iteration.
///
/// Returns `(id, title, notes, timeout_secs)` tuples for all `requires_human=1` tasks
/// with `status='done'` and `completed_at >= epoch_start`. This captures every completion
/// path (tag detection, git commit, output scan, external reconciliation) because they all
/// write the same DB state; the caller filters by timestamp to skip pre-completed tasks.
///
/// Exposed as `pub(crate)` so tests can verify query semantics without stdin interaction.
pub(crate) fn query_human_review_tasks(
    conn: &Connection,
    epoch_start: i64,
) -> Vec<(String, String, Option<String>, Option<u32>)> {
    match conn.prepare(
        "SELECT id, title, notes, human_review_timeout \
         FROM tasks \
         WHERE requires_human = 1 AND status = 'done' \
         AND CAST(strftime('%s', completed_at) AS INTEGER) >= ?",
    ) {
        Ok(mut stmt) => match stmt.query_map([epoch_start], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?
                    .and_then(|v| u32::try_from(v).ok()),
            ))
        }) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                tracing::warn!("could not execute human review query: {}", e);
                vec![]
            }
        },
        Err(e) => {
            tracing::warn!("could not prepare human review query: {}", e);
            vec![]
        }
    }
}

/// Trigger interactive human review for any `requires_human` tasks completed
/// this iteration.
///
/// **Relocated (FEAT-010).** The human-review reaction now lives in
/// [`reactions::post_completion::react_to_completions`], which BOTH execution
/// paths route through — the wave path gained human review as an intentional
/// behavior addition. This function is retained ONLY as the `#[deprecated]`
/// timestamp-query shim the CONTRACT-001 single-home lock pins: the three engine
/// files (`iteration.rs`/`wave_scheduler.rs`/`slot.rs`) carry
/// `#![deny(deprecated)]`, so copy-pasting human review back into one path fails
/// to compile. It translates its legacy `completed_at >= epoch` selection into
/// the input-driven id set and delegates — no human-review logic lives here
/// anymore. No production caller remains (hence `#[allow(dead_code)]`); the
/// sole legitimate callers of the reaction are the two coordinator call sites.
#[deprecated(note = "FEAT-010: human review relocated to \
            reactions::post_completion::react_to_completions (input-driven). \
            Retained as the single-home lock marker; not called from any engine file.")]
#[allow(dead_code)]
fn trigger_human_reviews(conn: &mut Connection, params: HumanReviewParams<'_>) {
    let HumanReviewParams {
        completion_epoch_start,
        iteration,
        session_guidance,
        prd_file,
        task_prefix,
        default_model,
        permission_mode,
    } = params;

    // Translate the legacy timestamp selection into the input-driven id set the
    // coordinator consumes, then delegate. `run_id` / `working_root` /
    // `external_*` are unused on this human-review-only path (`wrapper_commit =
    // false`, `external_repo_path = None`) — exactly what this shim did before.
    let completed_ids: Vec<String> = query_human_review_tasks(conn, completion_epoch_start)
        .into_iter()
        .map(|(id, _, _, _)| id)
        .collect();
    let pc_params = reactions::post_completion::PostCompletionParams {
        run_id: "",
        iteration,
        working_root: Path::new("."),
        prd_file,
        task_prefix,
        default_model,
        permission_mode,
        external_repo_path: None,
        external_git_scan_depth: 0,
        wrapper_commit: false,
    };
    let _ = reactions::post_completion::react_to_completions(
        conn,
        &completed_ids,
        &pc_params,
        session_guidance,
    );
}

/// Query pending key decisions for the run and prompt the user to resolve or defer each.
///
/// In yes_mode, all decisions are auto-deferred without prompting.
/// This function is a no-op when there are no pending decisions.
fn prompt_pending_key_decisions(conn: &Connection, run_id: &str, yes_mode: bool) {
    let decisions = match key_decisions_db::get_pending_decisions(conn, run_id) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("failed to query pending key decisions: {}", e);
            return;
        }
    };

    if decisions.is_empty() {
        return;
    }

    if yes_mode {
        for decision in &decisions {
            if let Err(e) = key_decisions_db::defer_decision(conn, decision.id) {
                tracing::warn!("failed to defer decision {}: {}", decision.id, e);
            }
        }
        ui::emit(&format!(
            "Auto-deferred {} key decision(s) (yes_mode).",
            decisions.len()
        ));
        return;
    }

    ui::emit(
        "\n╔══════════════════════════════════════════════════╗\
         \n║         KEY DECISIONS REQUIRING YOUR INPUT        ║\
         \n╚══════════════════════════════════════════════════╝",
    );

    for decision in &decisions {
        loop {
            ui::emit(&format!("\n┌─ Decision: {}", decision.title));
            ui::emit(&format!("│  {}", decision.description));
            ui::emit("│");
            for (i, opt) in decision.options.iter().enumerate() {
                let letter = (b'A' + i as u8) as char;
                ui::emit(&format!(
                    "│  {}) {} — {}",
                    letter, opt.label, opt.description
                ));
            }
            ui::emit("│  S) Skip (defer to next session)");
            ui::prompt("└─ Your choice: ");

            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_err() {
                // stdin unavailable — defer
                ui::emit_err("\nWarning: could not read stdin, deferring decision.");
                let _ = key_decisions_db::defer_decision(conn, decision.id);
                break;
            }

            let trimmed = input.trim().to_lowercase();

            if trimmed.is_empty() || trimmed == "s" || trimmed == "skip" {
                if let Err(e) = key_decisions_db::defer_decision(conn, decision.id) {
                    tracing::warn!("failed to defer decision: {}", e);
                } else {
                    ui::emit("Decision deferred.");
                }
                break;
            }

            // Match letter or label substring to an option
            match find_option(&decision.options, &trimmed) {
                Ok(opt) => {
                    let resolution = format!("{}: {}", opt.label, opt.description);
                    if let Err(e) =
                        key_decisions_db::resolve_decision(conn, decision.id, &resolution)
                    {
                        tracing::warn!("failed to resolve decision: {}", e);
                    } else {
                        ui::emit(&format!("Decision resolved: {}", resolution));
                    }
                    break;
                }
                Err(_) => {
                    ui::emit_err(&format!(
                        "Invalid choice — enter a letter (A–{}) or S to skip.",
                        (b'A' + decision.options.len() as u8 - 1) as char
                    ));
                }
            }
        }
    }
}

/// Called after a run ends with Completed status to recalibrate selection weights.
///
/// Analyzes historical task outcomes and adjusts the scoring weights used by
/// `select_next_task()`. Errors are logged but do not propagate (best-effort).
pub fn on_run_completed(conn: &Connection, task_prefix: Option<&str>) {
    match calibrate::recalibrate_weights(conn, task_prefix) {
        Ok(weights) => {
            let defaults = calibrate::SelectionWeights::default();
            if weights != defaults {
                ui::emit(&format!(
                    "Calibrated selection weights: file_overlap={}, priority_base={}",
                    weights.file_overlap, weights.priority_base
                ));
            }
        }
        Err(e) => {
            tracing::warn!("weight calibration failed: {}", e);
        }
    }
}

/// Record accumulated session guidance to progress.txt on loop exit.
///
/// In interactive mode (not --yes), prompts the user before saving.
/// In --yes mode, auto-saves without prompting.
/// Does nothing if no guidance was recorded during the session.
fn record_session_guidance(guidance: &SessionGuidance, progress_path: &Path, yes_mode: bool) {
    if guidance.is_empty() {
        return;
    }

    // In interactive mode, ask the user
    if !yes_mode {
        ui::prompt("Session guidance was recorded. Save to progress.txt? (y/N) ");
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => {
                let trimmed = input.trim().to_lowercase();
                if trimmed != "y" && trimmed != "yes" {
                    ui::emit("Session guidance discarded.");
                    return;
                }
            }
            Err(_) => {
                // stdin not available (non-interactive), skip
                tracing::warn!("could not read stdin, skipping guidance recording");
                return;
            }
        }
    }

    let formatted = guidance.format_for_recording();
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(progress_path)
    {
        Ok(mut file) => {
            if let Err(e) = io::Write::write_all(&mut file, formatted.as_bytes()) {
                tracing::warn!(
                    "could not write session guidance to {}: {}",
                    progress_path.display(),
                    e
                );
            } else {
                ui::emit(&format!(
                    "Session guidance saved to {}",
                    progress_path.display()
                ));
            }
        }
        Err(e) => {
            tracing::warn!(
                "could not open progress file {}: {}",
                progress_path.display(),
                e
            );
        }
    }
}

#[cfg(test)]
#[allow(deprecated)] // FEAT-010: tests exercise the deprecated apply_status_updates shim directly.
mod tests {
    use super::*;
    use crate::loop_engine::detection;
    use crate::loop_engine::test_utils::{EnvGuard, setup_test_db};

    // --- pre_lock_prefix fallback tests ---

    #[test]
    fn test_pre_lock_prefix_fallback_matches_generate_prefix() {
        use crate::commands::init::generate_prefix;
        use crate::loop_engine::status_queries::read_branch_name_from_prd;
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("my-prd.json");
        // PRD without taskPrefix but with branchName
        fs::write(
            &prd_path,
            r#"{"branchName": "feat/test-branch", "description": "test"}"#,
        )
        .unwrap();

        let branch = read_branch_name_from_prd(&prd_path);
        let filename = prd_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let fallback = generate_prefix(branch.as_deref(), filename);
        // Also verify generate_prefix called directly with the same inputs matches
        let expected = generate_prefix(Some("feat/test-branch"), "my-prd.json");
        assert_eq!(fallback, expected);
    }

    #[test]
    fn test_pre_lock_prefix_uses_task_prefix_when_present() {
        use crate::loop_engine::status_queries::read_branch_name_from_prd;
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("my-prd.json");
        fs::write(
            &prd_path,
            r#"{"taskPrefix": "abc12345", "branchName": "feat/test"}"#,
        )
        .unwrap();

        // When taskPrefix is present, read_task_prefix_from_prd returns it
        // and or_else branch must not run
        let task_prefix = crate::loop_engine::status_queries::read_task_prefix_from_prd(&prd_path);
        assert_eq!(task_prefix, Some("abc12345".to_string()));

        // or_else would only run if task_prefix is None
        let branch = read_branch_name_from_prd(&prd_path);
        // verify or_else branch not needed — task_prefix is Some
        let result = task_prefix.or_else(|| {
            let b = branch.clone();
            let filename = prd_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            Some(crate::commands::init::generate_prefix(
                b.as_deref(),
                filename,
            ))
        });
        assert_eq!(result, Some("abc12345".to_string()));
    }

    // --- on_run_completed tests ---

    #[test]
    fn test_on_run_completed_no_panic_on_empty_db() {
        let (_temp_dir, conn) = setup_test_db();

        // Should not panic even with no data
        on_run_completed(&conn, None);
    }

    // --- record_session_guidance tests ---

    #[test]
    fn test_record_session_guidance_empty_does_nothing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let guidance = SessionGuidance::new();

        record_session_guidance(&guidance, &progress_path, true);

        // File should not be created
        assert!(!progress_path.exists());
    }

    #[test]
    fn test_record_session_guidance_yes_mode_auto_saves() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        let mut guidance = SessionGuidance::new();
        guidance.add(3, "Focus on error handling".to_string());

        record_session_guidance(&guidance, &progress_path, true);

        assert!(progress_path.exists());
        let content = std::fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("Session Guidance"));
        assert!(content.contains("[Iteration 3] Focus on error handling"));
        assert!(content.contains("---"));
    }

    #[test]
    fn test_record_session_guidance_yes_mode_appends_to_existing() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");
        std::fs::write(&progress_path, "# Existing content\n").unwrap();

        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test guidance".to_string());

        record_session_guidance(&guidance, &progress_path, true);

        let content = std::fs::read_to_string(&progress_path).unwrap();
        assert!(content.starts_with("# Existing content\n"));
        assert!(content.contains("Session Guidance"));
        assert!(content.contains("Test guidance"));
    }

    #[test]
    fn test_record_session_guidance_yes_mode_multiple_entries() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let progress_path = temp_dir.path().join("progress.txt");

        let mut guidance = SessionGuidance::new();
        guidance.add(1, "First".to_string());
        guidance.add(5, "Second".to_string());
        guidance.add(10, "Third".to_string());

        record_session_guidance(&guidance, &progress_path, true);

        let content = std::fs::read_to_string(&progress_path).unwrap();
        assert!(content.contains("[Iteration 1] First"));
        assert!(content.contains("[Iteration 5] Second"));
        assert!(content.contains("[Iteration 10] Third"));
    }

    #[test]
    fn test_record_session_guidance_invalid_path_does_not_panic() {
        let mut guidance = SessionGuidance::new();
        guidance.add(1, "Test".to_string());

        // Writing to a non-existent directory — should not panic
        record_session_guidance(&guidance, Path::new("/nonexistent/dir/progress.txt"), true);
    }

    // --- startup recovery tests ---

    #[test]
    fn test_startup_recovery_resets_stale_tasks() {
        let (_temp_dir, conn) = crate::loop_engine::test_utils::setup_test_db();

        // Insert tasks in various states
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority, started_at) VALUES
             ('T-001', 'Stale task', 'in_progress', 1, datetime('now', '-1 hour')),
             ('T-002', 'Normal todo', 'todo', 2, NULL),
             ('T-003', 'Done task', 'done', 3, datetime('now', '-2 hours'));",
        )
        .unwrap();

        // Run the same recovery SQL used in run_loop
        let count = conn
            .execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
                [],
            )
            .unwrap();

        assert_eq!(count, 1, "Should reset exactly 1 in_progress task");

        // Verify T-001 is now todo
        let status = crate::loop_engine::test_utils::get_task_status(&conn, "T-001");
        assert_eq!(status, "todo");
    }

    #[test]
    fn test_startup_recovery_preserves_done_tasks() {
        let (_temp_dir, conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('T-001', 'Done task', 'done', 1),
             ('T-002', 'Irrelevant task', 'irrelevant', 2);",
        )
        .unwrap();

        let count = conn
            .execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
                [],
            )
            .unwrap();

        assert_eq!(count, 0, "Should not touch done or irrelevant tasks");

        // Verify statuses unchanged
        let status1 = crate::loop_engine::test_utils::get_task_status(&conn, "T-001");
        let status2 = crate::loop_engine::test_utils::get_task_status(&conn, "T-002");
        assert_eq!(status1, "done");
        assert_eq!(status2, "irrelevant");
    }

    #[test]
    fn test_startup_recovery_clears_started_at() {
        let (_temp_dir, conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority, started_at) VALUES
             ('T-001', 'Stale task', 'in_progress', 1, datetime('now'));",
        )
        .unwrap();

        // Verify started_at is set before recovery
        let before: Option<String> = conn
            .query_row(
                "SELECT started_at FROM tasks WHERE id = 'T-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(before.is_some(), "started_at should be set before recovery");

        // Run recovery
        conn.execute(
            "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
            [],
        )
        .unwrap();

        // Verify started_at is cleared
        let after: Option<String> = conn
            .query_row(
                "SELECT started_at FROM tasks WHERE id = 'T-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(after.is_none(), "started_at should be NULL after recovery");
    }

    // --- Stale tracker wiring tests ---

    #[test]
    fn test_stale_abort_after_consecutive_stale_outcomes() {
        // Simulates the outer loop's stale tracker wiring:
        // 3 consecutive Stale outcomes should trigger abort.
        let mut ctx = IterationContext::new(5);

        // First stale
        ctx.stale_tracker.mark_stale();
        assert!(
            !ctx.stale_tracker.should_abort(),
            "1 stale should not abort"
        );

        // Second stale
        ctx.stale_tracker.mark_stale();
        assert!(
            !ctx.stale_tracker.should_abort(),
            "2 stale should not abort"
        );

        // Third stale
        ctx.stale_tracker.mark_stale();
        assert!(
            ctx.stale_tracker.should_abort(),
            "3 consecutive stale should abort"
        );
    }

    #[test]
    fn test_stale_tracker_resets_on_non_stale_outcome() {
        // Non-Stale outcomes reset the stale tracker, preventing abort.
        let mut ctx = IterationContext::new(5);

        // Two stale
        ctx.stale_tracker.mark_stale();
        ctx.stale_tracker.mark_stale();
        assert_eq!(ctx.stale_tracker.count(), 2);

        // Non-stale resets
        ctx.stale_tracker.reset_progress();
        assert_eq!(
            ctx.stale_tracker.count(),
            0,
            "Non-stale outcome should reset tracker"
        );
        assert!(!ctx.stale_tracker.should_abort());

        // One more stale — not enough to abort
        ctx.stale_tracker.mark_stale();
        assert_eq!(ctx.stale_tracker.count(), 1);
        assert!(!ctx.stale_tracker.should_abort());
    }

    #[test]
    fn test_stale_recovery_resets_in_progress_tasks() {
        // Verifies the SQL recovery logic: in_progress tasks get reset to todo.

        let (_temp_dir, conn) = setup_test_db();

        // Insert tasks: one in_progress (stale), one blocked, one done
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Stale task', 'in_progress', 1),
             ('FEAT-002', 'Blocked task', 'blocked', 2),
             ('FEAT-003', 'Done task', 'done', 3);",
        )
        .unwrap();

        // Simulate the auto-recovery SQL from run_iteration
        let recovered = conn
            .execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
                [],
            )
            .unwrap();

        assert_eq!(recovered, 1, "Should recover exactly 1 in_progress task");

        // Verify the task was reset
        let status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-001");
        assert_eq!(status, "todo", "in_progress task should be reset to todo");

        // Verify other tasks are unaffected
        let blocked_status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-002");
        assert_eq!(
            blocked_status, "blocked",
            "Blocked task should be unaffected"
        );

        let done_status = crate::loop_engine::test_utils::get_task_status(&conn, "FEAT-003");
        assert_eq!(done_status, "done", "Done task should be unaffected");
    }

    // =====================================================================
    // Prefix-scoped engine query tests (SS-SS-TEST-001)
    //
    // Each test sets up two PRDs (P1-*, P2-*) in the same DB and verifies
    // that engine queries respect prefix boundaries.
    // =====================================================================

    /// Helper: insert P1 and P2 tasks into the test DB.
    ///
    /// P1 tasks: P1-TASK-001 (in_progress), P1-TASK-002 (todo), P1-TASK-003 (done)
    /// P2 tasks: P2-TASK-001 (in_progress), P2-TASK-002 (todo)
    fn insert_dual_prd_tasks(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority, started_at) VALUES
             ('P1-TASK-001', 'P1 stale task',   'in_progress', 1, datetime('now', '-1 hour')),
             ('P1-TASK-002', 'P1 todo task',     'todo',        2, NULL),
             ('P1-TASK-003', 'P1 done task',     'done',        3, NULL),
             ('P2-TASK-001', 'P2 stale task',    'in_progress', 1, datetime('now', '-1 hour')),
             ('P2-TASK-002', 'P2 todo task',     'todo',        2, NULL);",
        )
        .unwrap();
    }

    /// Build the full SQL and params for a prefix-scoped query, then call `execute_fn`.
    ///
    /// Eliminates the `prefix_and` → `format!` → params-Vec boilerplate shared by
    /// the initial-recovery and remaining-count prefix scope tests.
    fn run_with_prefix<T>(
        sql_template: &str,
        prefix: Option<&str>,
        execute_fn: impl FnOnce(&str, &[&dyn rusqlite::types::ToSql]) -> T,
    ) -> T {
        use crate::db::prefix::prefix_and;
        let (pfx_clause, pfx_param) = prefix_and(prefix);
        let sql = format!("{sql_template} {pfx_clause}");
        let params: Vec<&dyn rusqlite::types::ToSql> = match &pfx_param {
            Some(p) => vec![p],
            None => vec![],
        };
        execute_fn(&sql, params.as_slice())
    }

    // --- Initial recovery scoping ---

    #[test]
    fn test_initial_recovery_resets_only_p1_in_progress() {
        let (_temp_dir, conn) = setup_test_db();
        insert_dual_prd_tasks(&conn);

        // Simulate initial recovery with P1 prefix (as done in run_loop)
        let count = run_with_prefix(
            "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
            Some("P1"),
            |sql, params| conn.execute(sql, params).unwrap(),
        );

        assert_eq!(count, 1, "Should reset only P1's in_progress task");

        // P1-TASK-001 should now be todo
        let p1_status = crate::loop_engine::test_utils::get_task_status(&conn, "P1-TASK-001");
        assert_eq!(p1_status, "todo");

        // P2-TASK-001 must still be in_progress — untouched by P1 recovery
        let p2_status = crate::loop_engine::test_utils::get_task_status(&conn, "P2-TASK-001");
        assert_eq!(
            p2_status, "in_progress",
            "P2 task must not be affected by P1 recovery"
        );
    }

    #[test]
    fn test_initial_recovery_none_prefix_resets_all_in_progress() {
        let (_temp_dir, conn) = setup_test_db();
        insert_dual_prd_tasks(&conn);

        // None prefix → no WHERE clause addition → resets all in_progress
        let count = run_with_prefix(
            "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
            None,
            |sql, params| conn.execute(sql, params).unwrap(),
        );

        assert_eq!(
            count, 2,
            "None prefix should reset all in_progress tasks (backwards compat)"
        );
    }

    // --- Remaining count scoping ---

    #[test]
    fn test_remaining_count_scoped_to_p1() {
        let (_temp_dir, conn) = setup_test_db();
        insert_dual_prd_tasks(&conn);

        // Count remaining (not done/irrelevant) for P1 only
        let remaining: i64 = run_with_prefix(
            "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL",
            Some("P1"),
            |sql, params| conn.query_row(sql, params, |row| row.get(0)).unwrap(),
        );

        // P1 has in_progress + todo = 2 remaining (P1-TASK-003 is done)
        assert_eq!(remaining, 2, "P1 remaining should be 2 (not counting P2)");
    }

    #[test]
    fn test_remaining_count_none_prefix_counts_all() {
        let (_temp_dir, conn) = setup_test_db();
        insert_dual_prd_tasks(&conn);

        let remaining: i64 = run_with_prefix(
            "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL",
            None,
            |sql, params| conn.query_row(sql, params, |row| row.get(0)).unwrap(),
        );

        // 4 tasks total (P1: 2 + P2: 2), done is excluded
        assert_eq!(remaining, 4, "None prefix should count all remaining tasks");
    }

    // --- Auto-mode hint condition tests ---
    // Tests verify the conditional logic that controls when the hint fires.
    // Uses HINT_ENV_MUTEX to serialise env-var mutations across parallel tests.

    use std::sync::Mutex;
    static HINT_ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Mirrors the inline hint condition in run_loop() so the logic can be unit-tested.
    fn hint_should_fire(mode: &config::PermissionMode) -> bool {
        if let Ok(val) = std::env::var("LOOP_AUTO_MODE_AVAILABLE") {
            config::parse_bool_value(&val) == Some(true)
                && !matches!(mode, config::PermissionMode::Auto { .. })
        } else {
            false
        }
    }

    use super::AUTO_MODE_DEPRECATION_HINT as HINT_MSG;

    #[test]
    fn test_hint_fires_when_available_true_and_mode_scoped() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("LOOP_AUTO_MODE_AVAILABLE", "true");
        let mode = config::PermissionMode::text_only();
        let fires = hint_should_fire(&mode);
        assert!(
            fires,
            "Hint should fire when available=true and mode=Scoped"
        );
    }

    #[test]
    fn test_hint_fires_when_available_true_and_mode_dangerous() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("LOOP_AUTO_MODE_AVAILABLE", "true");
        let mode = config::PermissionMode::Dangerous;
        let fires = hint_should_fire(&mode);
        assert!(
            fires,
            "Hint should fire when available=true and mode=Dangerous"
        );
    }

    #[test]
    fn test_hint_does_not_fire_when_available_unset() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("LOOP_AUTO_MODE_AVAILABLE");
        let mode = config::PermissionMode::text_only();
        assert!(
            !hint_should_fire(&mode),
            "Hint must not fire when env var is unset"
        );
    }

    #[test]
    fn test_hint_does_not_fire_when_available_false() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("LOOP_AUTO_MODE_AVAILABLE", "false");
        let mode = config::PermissionMode::text_only();
        let fires = hint_should_fire(&mode);
        assert!(!fires, "Hint must not fire when available=false");
    }

    #[test]
    fn test_hint_does_not_fire_when_mode_is_auto() {
        let _guard = HINT_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("LOOP_AUTO_MODE_AVAILABLE", "true");
        let mode = config::PermissionMode::Auto {
            allowed_tools: None,
        };
        let fires = hint_should_fire(&mode);
        assert!(!fires, "Hint must not fire when mode is already Auto");
    }

    #[test]
    fn test_hint_message_contains_enable_auto_mode_env_var() {
        assert!(
            HINT_MSG.contains("LOOP_ENABLE_AUTO_MODE=true"),
            "Hint must mention LOOP_ENABLE_AUTO_MODE=true env var"
        );
    }

    #[test]
    fn test_hint_message_uses_yellow_ansi_prefix() {
        // Yellow ANSI escape: \x1b[33m
        assert!(
            HINT_MSG.contains("\x1b[33m"),
            "Hint must use yellow ANSI color code \\x1b[33m"
        );
        assert!(HINT_MSG.contains("[hint]"), "Hint must have [hint] prefix");
    }

    #[test]
    fn test_hint_message_says_deprecated() {
        assert!(
            HINT_MSG.contains("will be deprecated"),
            "Hint must mention that the permission model will be deprecated"
        );
    }

    #[test]
    fn test_hint_message_says_current_settings_continue() {
        assert!(
            HINT_MSG.contains("current settings continue"),
            "Hint must reassure users that their current settings continue to work"
        );
    }

    // --- query_human_review_tasks tests (TEST-001) ---

    /// Helper: insert a task with requires_human flag and a specific completed_at timestamp.
    ///
    /// `completed_at` is an ISO-8601 string (e.g. `datetime('now', '-10 seconds')` evaluated
    /// beforehand, or a literal like `"2020-01-01T00:00:00"`).
    fn insert_requires_human_task(
        conn: &Connection,
        id: &str,
        requires_human: i32,
        completed_at: &str,
    ) {
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, requires_human, completed_at) \
             VALUES (?, ?, 'done', 10, ?, ?)",
            rusqlite::params![id, format!("Task {id}"), requires_human, completed_at],
        )
        .unwrap();
    }

    /// Returns the current Unix epoch as i64.
    fn now_epoch() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// Task with requires_human=0 must NOT appear in query results (criterion 8).
    #[test]
    fn test_human_review_query_no_requires_human_skipped() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        insert_requires_human_task(&conn, "T-NRH", 0, "2099-01-01T12:00:00");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert!(
            tasks.is_empty(),
            "requires_human=0 task must not be returned by human review query"
        );
    }

    /// Task with requires_human=1 and recent completed_at must be returned (criteria 1-4).
    ///
    /// All completion detection paths (tag, git commit, output scan, external reconciliation)
    /// write the same DB state: status='done' + completed_at=<now>. The query selects by
    /// timestamp, so this single test covers all four detection paths.
    #[test]
    fn test_human_review_query_recent_completion_returned() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        // completed_at in the future (well after epoch) simulates "completed this iteration"
        insert_requires_human_task(&conn, "T-RH", 1, "2099-01-01T12:00:00");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert_eq!(tasks.len(), 1, "one requires_human=1 task must be returned");
        assert_eq!(tasks[0].0, "T-RH");
        assert_eq!(tasks[0].1, "Task T-RH");
    }

    /// Task completed before epoch (pre-completed at import) must be skipped (criterion 6).
    #[test]
    fn test_human_review_query_precompeted_task_skipped() {
        let (_dir, conn) = setup_test_db();
        // epoch = now; completed_at = far in the past → completed_at epoch < epoch
        let epoch = now_epoch();
        insert_requires_human_task(&conn, "T-OLD", 1, "2000-01-01T00:00:00");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert!(
            tasks.is_empty(),
            "task completed before epoch (pre-completed at import) must be skipped"
        );
    }

    /// Multiple requires_human=1 tasks completed this iteration must all be returned (criterion 7).
    ///
    /// Each task in the returned list will be passed to handle_human_review in trigger_human_reviews,
    /// so returning all tasks here guarantees each gets reviewed.
    #[test]
    fn test_human_review_query_multiple_tasks_all_returned() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        insert_requires_human_task(&conn, "T-A", 1, "2099-01-01T12:00:00");
        insert_requires_human_task(&conn, "T-B", 1, "2099-01-01T12:00:01");
        insert_requires_human_task(&conn, "T-C", 1, "2099-01-01T12:00:02");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert_eq!(
            tasks.len(),
            3,
            "all three requires_human=1 tasks must be returned"
        );
        let ids: Vec<&str> = tasks.iter().map(|(id, _, _, _)| id.as_str()).collect();
        assert!(ids.contains(&"T-A"), "T-A must be in results");
        assert!(ids.contains(&"T-B"), "T-B must be in results");
        assert!(ids.contains(&"T-C"), "T-C must be in results");
    }

    /// Mix of requires_human=1 and requires_human=0: only the flagged task is returned.
    #[test]
    fn test_human_review_query_mixed_flags_only_flagged_returned() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        insert_requires_human_task(&conn, "T-YES", 1, "2099-01-01T12:00:00");
        insert_requires_human_task(&conn, "T-NO", 0, "2099-01-01T12:00:00");

        let tasks = query_human_review_tasks(&conn, epoch);
        assert_eq!(
            tasks.len(),
            1,
            "only requires_human=1 task must be returned"
        );
        assert_eq!(tasks[0].0, "T-YES");
    }

    /// yes_mode does NOT suppress human review for requiresHuman tasks (criterion 5).
    ///
    /// `query_human_review_tasks` (and by extension `trigger_human_reviews`) has no
    /// yes_mode parameter — the review is unconditional. This test documents that a
    /// requires_human=1 task is always returned regardless of run configuration.
    #[test]
    fn test_human_review_yes_mode_not_gated() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        insert_requires_human_task(&conn, "T-BATCH", 1, "2099-01-01T12:00:00");

        // Simulate yes_mode=true: query_human_review_tasks takes no yes_mode parameter,
        // so it always returns requiresHuman tasks — yes_mode cannot suppress the review.
        let tasks = query_human_review_tasks(&conn, epoch);
        assert_eq!(
            tasks.len(),
            1,
            "requiresHuman task must be returned even in yes_mode (no mode gate in query)"
        );
    }

    /// Task with status != 'done' must not be returned even if requires_human=1.
    #[test]
    fn test_human_review_query_non_done_status_skipped() {
        let (_dir, conn) = setup_test_db();
        let epoch = now_epoch() - 100;
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, requires_human) \
             VALUES ('T-IP', 'Task T-IP', 'in_progress', 10, 1)",
            [],
        )
        .unwrap();

        let tasks = query_human_review_tasks(&conn, epoch);
        assert!(
            tasks.is_empty(),
            "in_progress task must not trigger human review (status != 'done')"
        );
    }

    // --- apply_status_updates dispatcher tests (FEAT-003) ---
    //
    // These exercise the DB side of the side-band <task-status> path. The
    // engine's in-iteration wiring (outcome flip, tasks_completed bump, claim
    // clearing) is covered by the iteration-level tests elsewhere in this
    // file; here we cover the pure dispatcher contract: command dispatch,
    // PRD JSON sync, warning-on-state-violation.

    /// Count entries in an `apply_status_updates` result whose dispatch
    /// succeeded — preserves the legacy "applied" semantics for tests written
    /// against the old `u32` return type.
    fn applied_count(results: &[(String, detection::TaskStatusChange, bool)]) -> u32 {
        results.iter().filter(|(_, _, ok)| *ok).count() as u32
    }

    /// Seed a minimal task row. `status` is set verbatim so tests can simulate
    /// pre-claimed (in_progress) vs unclaimed (todo) state machines.
    fn seed_task_with_status(conn: &Connection, id: &str, status: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, priority, status) VALUES (?1, 't', 50, ?2)",
            rusqlite::params![id, status],
        )
        .unwrap();
    }

    /// Write a minimal PRD JSON with a `userStories` array containing the
    /// given ids (each with `passes: false`). Returns the path.
    fn write_minimal_prd(dir: &std::path::Path, ids: &[&str]) -> std::path::PathBuf {
        use serde_json::json;
        let stories: Vec<_> = ids
            .iter()
            .map(|id| json!({"id": id, "title": "t", "priority": 50, "passes": false}))
            .collect();
        let doc = json!({"userStories": stories});
        let path = dir.join("test-prd.json");
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        path
    }

    #[test]
    fn test_apply_status_update_marks_task_done_after_claim() {
        // Seeds task as in_progress (as if claimed), runs dispatcher with a
        // Done update, asserts DB transitions to done.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-001", "in_progress");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-001"]);

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-001".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-001'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "done", "Done dispatch must transition DB status");
    }

    #[test]
    fn test_apply_status_update_todo_task_auto_claimed_and_completed() {
        // Seeds task as todo (NOT claimed). Dispatching Done should auto-claim
        // (todo -> in_progress) then complete (in_progress -> done).
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-002", "todo");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-002"]);

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-002".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            applied_count(&results),
            1,
            "todo task must be auto-claimed then completed"
        );

        let (status, started_at): (String, Option<String>) = conn
            .query_row(
                "SELECT status, started_at FROM tasks WHERE id = 'FEAT-002'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "done");
        assert!(started_at.is_some(), "started_at must be set by auto-claim",);
    }

    #[test]
    fn test_apply_status_update_todo_auto_claim_writes_run_tasks() {
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-010", "todo");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-010"]);

        // Create a run so run_tasks linkage can be written.
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-010".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            Some("run-1"),
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let linked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_tasks WHERE run_id = 'run-1' AND task_id = 'FEAT-010'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 1, "auto-claim must link task to run");
    }

    #[test]
    fn test_apply_status_update_done_flips_prd_json_passes() {
        // Two tasks in PRD; only FEAT-001 is dispatched. Verify FEAT-001's
        // passes flips true and the other task's entry is untouched.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-001", "in_progress");
        seed_task_with_status(&conn, "FEAT-002", "todo");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-001", "FEAT-002"]);

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-001".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let prd: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&prd_path).unwrap()).unwrap();
        let stories = prd.get("userStories").unwrap().as_array().unwrap();
        assert_eq!(stories.len(), 2);
        let by_id = |id: &str| {
            stories
                .iter()
                .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))
                .unwrap()
        };
        assert_eq!(
            by_id("FEAT-001").get("passes").and_then(|v| v.as_bool()),
            Some(true),
            "dispatched task's passes must flip to true",
        );
        assert_eq!(
            by_id("FEAT-002").get("passes").and_then(|v| v.as_bool()),
            Some(false),
            "unaffected task's passes must stay false",
        );
    }

    #[test]
    fn test_apply_status_update_json_sync_failure_does_not_rollback_db() {
        // Read-only PRD path: update_prd_task_passes will fail at the rename,
        // but the DB transition has already committed. Warning is logged
        // (stderr — not asserted here) and the DB state stands.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-003", "in_progress");
        // Point PRD at a non-existent path under the temp dir so the read
        // fails — mirrors the "missing PRD" failure mode.
        let prd_path = temp_dir.path().join("nonexistent.json");

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-003".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            applied_count(&results),
            1,
            "DB dispatch succeeded even though PRD sync failed",
        );

        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-003'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            status, "done",
            "DB transition must stand after JSON failure"
        );
    }

    #[test]
    fn test_apply_status_update_task_missing_from_prd_json() {
        // Task exists in DB but NOT in PRD userStories. DB transition must
        // succeed; JSON is left unchanged; no panic.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-004", "in_progress");
        // PRD has only SEED-001 — FEAT-004 is absent.
        let prd_path = write_minimal_prd(temp_dir.path(), &["SEED-001"]);
        let before = std::fs::read_to_string(&prd_path).unwrap();

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-004".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-004'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");

        // PRD JSON content unchanged.
        let after = std::fs::read_to_string(&prd_path).unwrap();
        assert_eq!(before, after, "PRD JSON must be unchanged when task absent");
    }

    #[test]
    fn test_apply_status_update_milestone_done_writes_summary_to_progress_file() {
        // Pre-seed progress.txt with two iteration entries, then dispatch
        // <task-status>MILESTONE-1:done</task-status>. The hook must rewrite
        // progress.txt so the raw entries are replaced by a summary block.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "MILESTONE-1", "in_progress");
        let prd_path = write_minimal_prd(temp_dir.path(), &["MILESTONE-1"]);

        let progress_path = temp_dir.path().join("progress-test.txt");
        let initial = "\n## 2026-01-01 - Iteration 1\n- Task: FEAT-001\n- Model: (default)\n- Effort: medium\n- Outcome: Completed\n- Files: (none)\n---\n\n## 2026-01-01 - Iteration 2\n- Task: FEAT-002\n- Model: (default)\n- Effort: medium\n- Outcome: Completed\n- Files: (none)\n---\n";
        std::fs::write(&progress_path, initial).unwrap();

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "MILESTONE-1".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            Some(&progress_path),
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let after = std::fs::read_to_string(&progress_path).unwrap();
        assert!(
            after.contains("Milestone Summary: MILESTONE-1"),
            "milestone hook must append a summary block"
        );
        assert!(
            !after.contains("Iteration 1") && !after.contains("Iteration 2"),
            "raw iteration entries must be replaced by the summary"
        );
        assert!(
            after.contains("FEAT-001") && after.contains("FEAT-002"),
            "completed task IDs must survive in the summary's task list"
        );
    }

    #[test]
    fn test_apply_status_update_non_milestone_done_does_not_touch_progress_file() {
        // A regular FEAT-* Done dispatch must NOT trigger the milestone hook,
        // even when a progress_path is supplied.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-100", "in_progress");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-100"]);

        let progress_path = temp_dir.path().join("progress-test.txt");
        let initial = "\n## 2026-01-01 - Iteration 1\n- Task: FEAT-100\n- Model: (default)\n- Effort: medium\n- Outcome: Completed\n- Files: (none)\n---\n";
        std::fs::write(&progress_path, initial).unwrap();

        let updates = vec![detection::TaskStatusUpdate {
            task_id: "FEAT-100".to_string(),
            status: detection::TaskStatusChange::Done,
        }];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            Some(&progress_path),
            None,
            None,
        );
        assert_eq!(applied_count(&results), 1);

        let after = std::fs::read_to_string(&progress_path).unwrap();
        assert_eq!(
            after, initial,
            "non-milestone Done dispatch must leave progress file untouched"
        );
    }

    #[test]
    fn test_apply_status_update_continues_past_failed_dispatch() {
        // Two updates: the first targets a nonexistent task (dispatch fails),
        // the second targets an in_progress task (dispatch succeeds). The engine
        // must log + continue, not abort on the first failure.
        let (temp_dir, mut conn) = setup_test_db();
        seed_task_with_status(&conn, "FEAT-B", "in_progress");
        let prd_path = write_minimal_prd(temp_dir.path(), &["FEAT-B"]);

        let updates = vec![
            detection::TaskStatusUpdate {
                task_id: "NONEXISTENT-999".to_string(),
                status: detection::TaskStatusChange::Done,
            },
            detection::TaskStatusUpdate {
                task_id: "FEAT-B".to_string(),
                status: detection::TaskStatusChange::Done,
            },
        ];
        let results = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            applied_count(&results),
            1,
            "one dispatch failed, one succeeded"
        );

        let status_b: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-B'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status_b, "done");
    }
}
