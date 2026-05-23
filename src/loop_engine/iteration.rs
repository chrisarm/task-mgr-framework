//! Sequential per-task iteration body.
//!
//! Carved out of `engine.rs` (PRD 02, FEAT-004). This module owns
//! `run_iteration` — the single-task execution path that drives the
//! sequential loop: signal/usage/backoff pre-checks, prompt build (with the
//! mid-run auto-recovery sweep), crash/overflow/review model resolution, the
//! `runner::dispatch` spawn, output analysis, the `PromptTooLong` recovery
//! ladder hand-off, and tracker updates.
//!
//! The hand-off data types (`IterationContext`, `IterationParams`,
//! `IterationResult`) and the per-iteration runner-resolution helpers
//! (`resolve_effective_runner`, `apply_review_model_override`) remain in
//! `engine.rs` and are imported here — `run_loop` and the inline engine test
//! modules also consume them, so moving them would widen the carve's blast
//! radius. The leaf recovery primitives come from `recovery.rs` (FEAT-002:
//! `check_crash_escalation`, `check_override_invalidation`,
//! `prompt_overflow_result`, `probe_rate_limit_lifted`, `update_trackers`).
//!
//! `engine.rs` re-exports `run_iteration` `pub` so the external import path
//! `task_mgr::loop_engine::engine::run_iteration` integration tests and callers
//! rely on stays valid (FR-008).
//!
//! **Order-before-resolve invariant (load-bearing)**: `check_override_invalidation`
//! runs at the TOP of the iteration, BEFORE `resolve_effective_runner`, so an
//! operator's out-of-band `tasks.model` edit clears the stale per-task recovery
//! channels before the spawn discriminant is derived. See
//! `src/loop_engine/CLAUDE.md` → "Operator escape valve". `run_iteration` still
//! hands its `IterationResult` to `iteration_pipeline::process_iteration_output`
//! at the `run_loop` call site (the shared post-Claude pipeline is invoked
//! after this function returns — see FR-006).

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::TaskMgrResult;
use crate::db::prefix::prefix_and;
use crate::error::TaskMgrError;
use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::claude;
use crate::loop_engine::config::{
    self, IterationOutcome, PermissionMode, TASKS_JSON_DISALLOWED_TOOLS,
};
use crate::loop_engine::detection;
use crate::loop_engine::display;
use crate::loop_engine::engine::{
    IterationContext, IterationParams, IterationResult, MAX_CONSECUTIVE_REORDERS,
    apply_review_model_override, resolve_effective_runner,
};
use crate::loop_engine::monitor;
use crate::loop_engine::overflow;
use crate::loop_engine::prd_reconcile::reconcile_passes_with_db;
use crate::loop_engine::prompt::{self, BuildPromptParams};
use crate::loop_engine::recovery::{
    check_crash_escalation, check_override_invalidation, probe_rate_limit_lifted,
    prompt_overflow_result, update_trackers,
};
use crate::loop_engine::runner;
use crate::loop_engine::signals;
use crate::loop_engine::usage::{self, UsageCheckResult};
use crate::loop_engine::watchdog;

/// Run a single iteration of the agent loop.
///
/// Returns `IterationResult` describing the outcome and whether to stop.
pub fn run_iteration(
    ctx: &mut IterationContext,
    params: &mut IterationParams<'_>,
) -> TaskMgrResult<IterationResult> {
    // Step 0: Check for SIGINT/SIGTERM
    if params.signal_flag.is_signaled() {
        eprintln!("Signal received, stopping loop...");
        return Ok(IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: None,
            files_modified: vec![],
            should_stop: true,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    // Step 1: Check file-based signals
    if signals::check_stop_signal(params.tasks_dir, params.task_prefix) {
        eprintln!("Stop signal detected (.stop file found)");
        return Ok(IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: None,
            files_modified: vec![],
            should_stop: true,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    if signals::check_pause_signal(params.tasks_dir, params.task_prefix) {
        signals::handle_pause(
            params.tasks_dir,
            params.iteration,
            &mut ctx.session_guidance,
            params.task_prefix,
        );
    }

    // Step 1.5: Pre-iteration usage check
    if params.usage_params.enabled {
        let check_result = usage::check_and_wait(
            params.usage_params.threshold,
            params.tasks_dir,
            params.usage_params.fallback_wait,
        );
        match check_result {
            UsageCheckResult::StopSignaled => {
                eprintln!("Stop signal during usage wait, exiting");
                return Ok(IterationResult {
                    outcome: IterationOutcome::Empty,
                    task_id: None,
                    files_modified: vec![],
                    should_stop: true,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                });
            }
            UsageCheckResult::ApiError(ref msg) => {
                eprintln!("Usage API warning: {} (continuing)", msg);
            }
            _ => {} // BelowThreshold, WaitedAndReset, Skipped — proceed
        }
    }

    // Step 2: Check crash tracker backoff
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
        return Ok(IterationResult {
            outcome: IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError),
            task_id: None,
            files_modified: vec![],
            should_stop: true,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    // Step 3: Force algorithmic pick if too many reorders
    let effective_reorder_hint = if ctx.reorder_count >= MAX_CONSECUTIVE_REORDERS {
        eprintln!(
            "Forcing algorithmic task selection after {} consecutive reorders",
            ctx.reorder_count
        );
        ctx.reorder_count = 0;
        None
    } else {
        ctx.reorder_hint.take()
    };

    // Step 4: Build prompt (selects and claims task).
    //
    // We call `build_prompt` up to twice: once initially, and once again after
    // the mid-run auto-recovery sweep (when the first call returned
    // `Ok(None)`). The sweep mutates `params.conn` via
    // `TaskLifecycle::recover_in_progress_for_prefix`, which would conflict
    // with a long-lived `BuildPromptParams` borrowing `params.conn`
    // immutably. Each `BuildPromptParams` is therefore constructed inline
    // and bound to a `let` so the temporary is dropped at the semicolon and
    // the immutable conn borrow is released before the lifecycle call.
    let session_guidance_text = ctx.session_guidance.format_for_prompt();
    let effective_reorder_hint_str = effective_reorder_hint.as_deref();

    let first_attempt = prompt::build_prompt(&BuildPromptParams {
        dir: params.db_dir,
        project_root: params.project_root,
        conn: params.conn,
        after_files: &ctx.last_files,
        run_id: Some(params.run_id),
        iteration: params.iteration,
        reorder_hint: effective_reorder_hint_str,
        session_guidance: &session_guidance_text,
        base_prompt_path: params.base_prompt_path,
        steering_path: params.steering_path,
        verbose: params.verbose,
        default_model: params.default_model,
        project_default_model: params.project_default_model,
        user_default_model: params.user_default_model,
        task_prefix: params.task_prefix,
        batch_sibling_prds: params.batch_sibling_prds,
        permission_mode: params.permission_mode,
    });

    let prompt_result = match first_attempt {
        Ok(Some(result)) => result,
        Ok(None) => {
            // No eligible task found — check if truly all done or just temporarily unavailable
            let (rem_pfx_clause, rem_pfx_param) = prefix_and(params.task_prefix);
            let rem_sql = format!(
                "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant') AND archived_at IS NULL {rem_pfx_clause}"
            );
            let rem_params: Vec<&dyn rusqlite::types::ToSql> = match &rem_pfx_param {
                Some(p) => vec![p],
                None => vec![],
            };
            let remaining: i64 = params
                .conn
                .query_row(&rem_sql, rem_params.as_slice(), |row| row.get(0))
                .unwrap_or(0);
            if remaining == 0 {
                eprintln!("All tasks complete!");
                return Ok(IterationResult {
                    outcome: IterationOutcome::Completed,
                    task_id: None,
                    files_modified: vec![],
                    should_stop: true,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                });
            }

            // Auto-recover: reset stale in_progress tasks to todo and retry.
            // Safe because we hold the exclusive loop.lock — no other loop is running.
            //
            // First, reconcile any tasks that have passes: true in the PRD.
            // These were completed but the DB status was never updated.
            if let Some(prd) = params.prd_path {
                reconcile_passes_with_db(params.conn, prd, params.task_prefix);
            }

            // Bulk in_progress → todo sweep routed through the lifecycle
            // service. `recover_in_progress_for_prefix(None)` matches the
            // legacy unscoped path; `Some(prefix)` mirrors the old
            // `prefix_and(...)`-scoped UPDATE.
            let recovered = TaskLifecycle::new(params.conn)
                .recover_in_progress_for_prefix(params.task_prefix)
                .unwrap_or(0);

            if recovered > 0 {
                eprintln!(
                    "Auto-recovered {} stale in_progress task(s), retrying task selection...",
                    recovered
                );
                // Retry build_prompt once with a fresh BuildPromptParams (the
                // previous temporary was dropped at the let above so the
                // conn re-borrow path is clean here).
                let retry_attempt = prompt::build_prompt(&BuildPromptParams {
                    dir: params.db_dir,
                    project_root: params.project_root,
                    conn: params.conn,
                    after_files: &ctx.last_files,
                    run_id: Some(params.run_id),
                    iteration: params.iteration,
                    reorder_hint: effective_reorder_hint_str,
                    session_guidance: &session_guidance_text,
                    base_prompt_path: params.base_prompt_path,
                    steering_path: params.steering_path,
                    verbose: params.verbose,
                    default_model: params.default_model,
                    project_default_model: params.project_default_model,
                    user_default_model: params.user_default_model,
                    task_prefix: params.task_prefix,
                    batch_sibling_prds: params.batch_sibling_prds,
                    permission_mode: params.permission_mode,
                });
                match retry_attempt {
                    Ok(Some(result)) => result,
                    Ok(None) => {
                        eprintln!(
                            "No eligible tasks after recovery ({} remaining). Treating as stale.",
                            remaining
                        );
                        return Ok(IterationResult {
                            outcome: IterationOutcome::NoEligibleTasks,
                            task_id: None,
                            files_modified: vec![],
                            should_stop: false,
                            output: String::new(),
                            effective_model: None,
                            effective_effort: None,
                            key_decisions_count: 0,
                            conversation: None,
                            shown_learning_ids: Vec::new(),
                        });
                    }
                    Err(TaskMgrError::PromptOverflow {
                        critical_size,
                        budget,
                        task_id,
                    }) => {
                        return Ok(prompt_overflow_result(critical_size, budget, task_id));
                    }
                    Err(e) => return Err(e),
                }
            } else {
                eprintln!(
                    "No eligible tasks right now ({} remaining in todo/in-progress/blocked). Treating as stale.",
                    remaining
                );
                return Ok(IterationResult {
                    outcome: IterationOutcome::NoEligibleTasks,
                    task_id: None,
                    files_modified: vec![],
                    should_stop: false,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                });
            }
        }
        Err(TaskMgrError::PromptOverflow {
            critical_size,
            budget,
            task_id,
        }) => {
            return Ok(prompt_overflow_result(critical_size, budget, task_id));
        }
        Err(e) => return Err(e),
    };

    let task_id = prompt_result.task_id.clone();
    let task_files = prompt_result.task_files.clone();
    let shown_learning_ids = prompt_result.shown_learning_ids.clone();

    // Step 4.5: Apply crash escalation and PromptTooLong model overrides
    let mut effective_model = {
        let resolved = prompt_result.resolved_model.as_deref();
        let after_crash_escalation =
            match check_crash_escalation(&ctx.crashed_last_iteration, &task_id, resolved) {
                Some(escalated) => {
                    let old = resolved.unwrap_or("(default)");
                    eprintln!("Crash escalation: {} → {}", old, escalated);
                    Some(escalated)
                }
                None => prompt_result.resolved_model.clone(),
            };
        // Apply per-task 1M model override from prior PromptTooLong recovery
        if let Some(override_model) = ctx.model_overrides.get(&task_id) {
            let old = after_crash_escalation.as_deref().unwrap_or("(default)");
            eprintln!(
                "Model override (prior prompt overflow): {} → {}",
                old, override_model,
            );
            Some(override_model.clone())
        } else {
            after_crash_escalation
        }
    };

    // FEAT-002: route review-class tasks to `reviewModel` after the crash /
    // overflow escalation block so escalation can't overwrite this routing.
    // The single `effective_model` here feeds both `resolve_effective_runner`
    // (runner selection) and the `--model` flag passed to the runner, so one
    // assignment keeps selection and dispatch in sync.
    if let Some(review_model_override) =
        apply_review_model_override(params.project_config.review_model.as_deref(), &task_id)
    {
        let old = effective_model.as_deref().unwrap_or("(default)");
        eprintln!(
            "Review-class routing: {} → {} (reviewModel)",
            old, review_model_override,
        );
        effective_model = Some(review_model_override);
    }

    // Use the cluster-wide effort computed by `build_prompt` — parallels the
    // cluster-wide `resolved_model` so both axes scale with the hardest task
    // in the synergy cluster. Apply any per-task override left by a prior
    // `PromptTooLong` crash on top.
    let base_effort = prompt_result.cluster_effort;
    let effort = ctx.effort_overrides.get(&task_id).copied().or(base_effort);
    if effort != base_effort {
        eprintln!(
            "Effort override (prior prompt overflow): {} → {}",
            base_effort.unwrap_or("(default)"),
            effort.unwrap_or("(default)"),
        );
    }

    // FEAT-008: operator escape valve — clear stale overrides if tasks.model changed.
    check_override_invalidation(ctx, params.conn, &task_id);

    // FEAT-005/009: resolve effective runner once per iteration (PRD §2.5
    // single source of truth). Placed before the banner so the "(via grok)"
    // annotation can be included in the iteration header.
    let effective_runner = resolve_effective_runner(ctx, &task_id, effective_model.as_deref());

    // Step 5: Print iteration header (with post-escalation effective_model + effort)
    eprintln!(
        "{}",
        display::format_iteration_banner_with_recovery(
            params.iteration,
            params.max_iterations,
            &task_id,
            params.elapsed_secs,
            effective_model.as_deref(),
            effort,
            &ctx.overflow_recovered,
            &ctx.overflow_original_model,
            effective_runner,
        )
    );

    // Step 6: Start activity monitor, spawn Claude subprocess, stop monitor.
    // Timeout is intentionally derived from the primary task's difficulty, not
    // the cluster — synergy partners don't lengthen wall-clock inactivity budgets.
    let monitor_handle = monitor::start_monitor(params.project_root, None);
    let timeout_config = watchdog::TimeoutConfig::from_difficulty(
        prompt_result.task_difficulty.as_deref(),
        Arc::clone(&monitor_handle.last_activity_epoch),
    );
    let claude_result = runner::dispatch(
        effective_runner,
        &prompt_result.prompt,
        params.permission_mode,
        claude::SpawnOpts {
            signal_flag: Some(params.signal_flag),
            working_dir: Some(params.project_root),
            model: effective_model.as_deref(),
            timeout: Some(timeout_config),
            stream_json: true,
            effort,
            disallowed_tools: Some(TASKS_JSON_DISALLOWED_TOOLS),
            db_dir: Some(params.db_dir),
            // PTY disabled: when Claude sees isatty(1)==true it switches to
            // "interactive" handling of rate limits (internal wait + retry)
            // instead of failing fast with an error. That breaks task-mgr's
            // own probe_rate_limit_lifted wait loop because Claude never
            // exits; the watchdog eventually SIGKILLs it ~30 min later and
            // we lose the whole iteration. Live streaming would be nice but
            // not at the cost of rate-limit handling — revisit later with
            // a mechanism that keeps Claude in non-interactive mode while
            // still getting per-line flushing.
            use_pty: false,
            target_task_id: Some(&task_id),
            active_prefix: params.task_prefix,
            // Each iteration's ai-title metadata stub otherwise clutters the
            // project's interactive resume picker. See claude.rs:119.
            cleanup_title_artifact: true,
            ..Default::default()
        },
    );
    monitor::stop_monitor(monitor_handle);
    claude::cleanup_ghost_sessions();
    // FEAT-007: surface TaskMgrError::GrokAuthFailure as a Crash(GrokAuthFailure)
    // outcome instead of bubbling out of the iteration. The retry-tracking site
    // in `run_loop` skips this variant so an xAI auth lapse never pushes a
    // healthy task toward `auto_block_task`.
    let claude_result = match claude_result {
        Ok(r) => r,
        Err(crate::error::TaskMgrError::GrokAuthFailure { hint }) => {
            eprintln!("Grok auth failure for task {}: {}", task_id, hint);
            return Ok(IterationResult {
                outcome: IterationOutcome::Crash(config::CrashType::GrokAuthFailure),
                task_id: Some(task_id),
                files_modified: task_files,
                should_stop: false,
                output: hint,
                effective_model,
                effective_effort: effort,
                key_decisions_count: 0,
                conversation: None,
                shown_learning_ids: Vec::new(),
            });
        }
        Err(e) => return Err(e),
    };

    // Step 6.1: Print hints for denied tools
    let denied_cmds = claude::extract_denied_commands(&claude_result.permission_denials);
    if !denied_cmds.is_empty() {
        let config_path = params.db_dir.join("config.json");
        let allowed_str = match params.permission_mode {
            PermissionMode::Scoped {
                allowed_tools: Some(t),
            }
            | PermissionMode::Auto {
                allowed_tools: Some(t),
            } => t.as_str(),
            _ => "",
        };
        for cmd in &denied_cmds {
            let pattern = format!("Bash({}:*)", cmd);
            if allowed_str.contains(&pattern) {
                // Tool is in the allowlist but Claude CLI still denied it —
                // likely user-level deny rules in ~/.claude/settings.json
                eprintln!(
                    "\x1b[33m[hint]\x1b[0m Tool denied: {} (already in --allowedTools \u{2014} \
                     check ~/.claude/settings.json or project .claude/settings.json for deny rules)",
                    cmd,
                );
            } else {
                eprintln!(
                    "\x1b[33m[hint]\x1b[0m Tool denied: {} \u{2014} to allow in future loops, add to {}:",
                    cmd,
                    config_path.display(),
                );
                eprintln!(
                    "       {{\"additionalAllowedTools\": [\"Bash({}:*)\"]}}",
                    cmd,
                );
            }
        }
    }

    // Step 6.1b: Targeted hints for Edit/Write denials on .task-mgr/tasks/*.json.
    // These are denied by --disallowedTools to prevent the agent from corrupting PRD JSON.
    // The agent should use `task-mgr add --stdin` or `<task-status>` tags instead.
    let tasks_json_denials = claude::extract_tasks_json_denials(&claude_result.permission_denials);
    for (tool, path) in &tasks_json_denials {
        match tool.as_str() {
            "Write" => eprintln!(
                "\x1b[33m[hint]\x1b[0m Tool denied: {} on {} \u{2014} \
                 use 'task-mgr init --from-json --append' to create new PRDs",
                tool, path,
            ),
            _ => eprintln!(
                "\x1b[33m[hint]\x1b[0m Tool denied: {} on {} \u{2014} \
                 use 'task-mgr add --stdin' or <task-status> tag instead",
                tool, path,
            ),
        }
    }

    // Step 6.5a: If iteration timed out, log and treat as a crash-like outcome
    if claude_result.timed_out {
        eprintln!(
            "Iteration timed out for task {} (difficulty: {})",
            task_id,
            prompt_result.task_difficulty.as_deref().unwrap_or("medium"),
        );
        return Ok(IterationResult {
            outcome: IterationOutcome::Crash(crate::loop_engine::config::CrashType::RuntimeError),
            task_id: Some(task_id),
            files_modified: task_files,
            should_stop: false,
            output: claude_result.output,
            effective_model,
            effective_effort: effort,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    // Step 6.5: Detect if Claude was killed by SIGINT/SIGTERM (exit 130/143).
    // Claude may be the terminal foreground group, so Ctrl+C goes to it instead
    // of us. Propagate the signal to our flag so the loop stops cleanly.
    //
    // Exception: if the watchdog fired the post-completion grace kill, the
    // SIGTERM (143) was issued internally as a successful-completion finalizer
    // — not an external Ctrl+C. Propagating it would end the whole loop (and
    // any chained PRDs) despite the task completing normally.
    if matches!(claude_result.exit_code, 130 | 143) && !claude_result.completion_killed {
        params.signal_flag.set();
    }

    // If signal arrived during Claude execution (either directly or via exit code
    // detection above), stop immediately. Without this, post-processing runs
    // before the signal is checked at the next iteration boundary.
    if params.signal_flag.is_signaled() {
        return Ok(IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: Some(task_id),
            files_modified: task_files,
            should_stop: true,
            output: claude_result.output,
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
            conversation: None,
            shown_learning_ids: Vec::new(),
        });
    }

    // Step 7: Analyze output
    let claude_conversation = claude_result.conversation;
    let claude_output = claude_result.output;
    let outcome =
        detection::analyze_output(&claude_output, claude_result.exit_code, params.project_root);

    // Step 7.5: On rate-limit detection, trigger usage wait and mark as non-counting
    if outcome == IterationOutcome::RateLimit {
        eprintln!("Rate limit detected in output, checking usage API...");

        let mut waited = false;

        // Try the usage API first (if enabled)
        if params.usage_params.enabled {
            let check_result = usage::check_and_wait(
                params.usage_params.threshold,
                params.tasks_dir,
                params.usage_params.fallback_wait,
            );
            match check_result {
                UsageCheckResult::StopSignaled => {
                    return Ok(IterationResult {
                        outcome: IterationOutcome::RateLimit,
                        task_id: Some(task_id),
                        files_modified: task_files,
                        should_stop: true,
                        output: String::new(),
                        effective_model: None,
                        effective_effort: None,
                        key_decisions_count: 0,
                        conversation: None,
                        shown_learning_ids: Vec::new(),
                    });
                }
                UsageCheckResult::WaitedAndReset => {
                    waited = true;
                }
                _ => {} // Skipped, BelowThreshold, ApiError — didn't actually wait
            }
        }

        // Fallback: if the usage API didn't wait, parse reset time from output
        if !waited {
            let wait_secs = usage::parse_reset_from_output(&claude_output).unwrap_or(0);
            eprintln!(
                "Usage API did not wait (CLI session limit). Falling back to output-parsed reset time ({})...",
                if wait_secs > 0 {
                    display::format_duration(wait_secs)
                } else {
                    format!("fallback {}s", params.usage_params.fallback_wait)
                }
            );
            let probe = || probe_rate_limit_lifted(params.permission_mode);
            let completed = usage::wait_for_usage_reset(
                wait_secs,
                params.tasks_dir,
                params.usage_params.fallback_wait,
                Some(&probe),
            );
            if !completed {
                return Ok(IterationResult {
                    outcome: IterationOutcome::RateLimit,
                    task_id: Some(task_id),
                    files_modified: task_files,
                    should_stop: true,
                    output: String::new(),
                    effective_model: None,
                    effective_effort: None,
                    key_decisions_count: 0,
                    conversation: None,
                    shown_learning_ids: Vec::new(),
                });
            }
        }
    }

    // Step 7.7 / Step 8 (extract_learnings_from_output, record_iteration_feedback)
    // were lifted into `iteration_pipeline::process_iteration_output` (FEAT-005).
    // The pipeline now runs from the call site (`run_loop`, `run_wave_iteration`)
    // after `run_iteration` returns. `shown_learning_ids` rides on
    // `IterationResult.shown_learning_ids` to reach the pipeline.

    // Step 8.5: Handle PromptTooLong — walk the four-state recovery ladder
    // and emit the diagnostics bundle (prompt dump + JSONL + rotation).
    //
    // The four rungs (first matching precondition wins, see
    // `overflow::handle_prompt_too_long`):
    //   1. `downgrade_effort`   — effort floor preserved at `high`.
    //   2. `escalate_below_opus` — `haiku → sonnet`, `sonnet → opus`.
    //   3. `to_1m_model`        — `opus → opus[1m]`.
    //   4. blocked              — no recovery left.
    //
    // Each rung emits a distinct stderr message that names the current task,
    // current effort/model, and the chosen action. The Blocked phrasing makes
    // it explicit that we are at `Opus[1M]` with `effort=high`, so users do
    // not chase a phantom "1M not tried" config. The crash-tracker backoff
    // still runs via update_trackers below; rungs 1-3 reset the task row to
    // `todo` (clearing `started_at`) so the next iteration retries with the
    // override applied, while rung 4 sets `blocked` so it doesn't consume
    // budget.
    if matches!(
        outcome,
        IterationOutcome::Crash(config::CrashType::PromptTooLong)
    ) {
        // FEAT-006/H3: use the primary effective_runner computed above (PRD §2.5
        // single-source rule — never re-derive). The outer binding from the
        // banner step is in scope here; shadowing it would be drift-prone.
        let _ = overflow::handle_prompt_too_long(
            ctx,
            params.conn,
            &task_id,
            effort,
            effective_model.as_deref(),
            &prompt_result,
            params.iteration,
            Some(params.run_id),
            params.db_dir,
            None,
            effective_runner,
            params.project_config,
        );
    }

    // Step 9: Update trackers based on outcome
    let mut should_stop = update_trackers(ctx, &outcome);

    // Step 10: Handle reorder
    if let IterationOutcome::Reorder(ref requested_task_id) = outcome {
        ctx.reorder_hint = Some(requested_task_id.clone());
        ctx.reorder_count += 1;
        eprintln!("Reorder requested: {}", requested_task_id);
    } else {
        ctx.reorder_count = 0;
    }

    // Step 11: Update last_files for next iteration scoring
    ctx.last_files = task_files.clone();

    // Step 12: Inter-iteration delay (skip if stopping or signaled)
    if !should_stop && !params.inter_iteration_delay.is_zero() && !params.signal_flag.is_signaled()
    {
        // Sleep in short intervals so we can respond to Ctrl+C promptly
        let deadline = std::time::Instant::now() + params.inter_iteration_delay;
        while std::time::Instant::now() < deadline {
            if params.signal_flag.is_signaled() {
                should_stop = true;
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    Ok(IterationResult {
        outcome,
        task_id: Some(task_id),
        files_modified: task_files,
        should_stop,
        output: claude_output,
        effective_model,
        effective_effort: effort,
        key_decisions_count: 0,
        conversation: claude_conversation,
        shown_learning_ids,
    })
}

#[cfg(test)]
mod tests {
    use crate::loop_engine::signals::SignalFlag;

    // --- Signal flag propagation from Claude exit code ---

    #[test]
    fn test_signal_flag_set_on_exit_code_130() {
        let flag = SignalFlag::new();
        assert!(!flag.is_signaled());

        // Simulate what run_iteration does when Claude exits with 130 (SIGINT)
        let exit_code = 130;
        let completion_killed = false;
        if matches!(exit_code, 130 | 143) && !completion_killed {
            flag.set();
        }
        assert!(flag.is_signaled(), "Exit code 130 should set signal flag");
    }

    #[test]
    fn test_signal_flag_set_on_exit_code_143() {
        let flag = SignalFlag::new();
        assert!(!flag.is_signaled());

        let exit_code = 143;
        let completion_killed = false;
        if matches!(exit_code, 130 | 143) && !completion_killed {
            flag.set();
        }
        assert!(flag.is_signaled(), "Exit code 143 should set signal flag");
    }

    #[test]
    fn test_signal_flag_not_set_on_normal_exit_codes() {
        for exit_code in [0, 1, 127, 137, 139] {
            let flag = SignalFlag::new();
            let completion_killed = false;
            if matches!(exit_code, 130 | 143) && !completion_killed {
                flag.set();
            }
            assert!(
                !flag.is_signaled(),
                "Exit code {} should not set signal flag",
                exit_code
            );
        }
    }

    /// Regression: post-completion grace kill sends SIGTERM (exit 143), but
    /// that's an internal finalizer — it must NOT propagate to the parent's
    /// signal flag, or the batch runner ends the whole loop + chained PRDs
    /// after every `<completed>` tag.
    #[test]
    fn test_signal_flag_not_set_on_completion_killed_143() {
        let flag = SignalFlag::new();
        let exit_code = 143;
        let completion_killed = true;
        if matches!(exit_code, 130 | 143) && !completion_killed {
            flag.set();
        }
        assert!(
            !flag.is_signaled(),
            "exit 143 from post-completion grace kill must not set signal flag"
        );
    }
}
