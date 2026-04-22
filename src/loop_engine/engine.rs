/// Autonomous agent loop engine: single iteration + full loop orchestrator.
///
/// `run_iteration()` orchestrates one complete cycle:
/// 1. Check .stop/.pause signals
/// 2. Build enriched prompt (task selection + context)
/// 3. Spawn Claude subprocess
/// 4. Analyze output to determine outcome
/// 5. Record learning feedback
/// 6. Handle reorder requests
///
/// `run_loop()` is the top-level orchestrator:
/// env setup → git validation → init PRD → run lifecycle → iterate → cleanup
///
/// The iteration context carries state between iterations (crash tracker,
/// stale tracker, session guidance, reorder hints, etc.).
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::TaskMgrResult;
use crate::commands::complete as complete_cmd;
use crate::commands::decisions::find_option;
use crate::commands::doctor::setup_checks::pre_check_loop_setup;
use crate::commands::doctor::setup_output::SetupSeverity;
use crate::commands::init::{PrefixMode, generate_prefix};
use crate::commands::run as run_cmd;
use crate::db::LockGuard;
use crate::db::prefix::{prefix_and, validate_prefix};
use crate::db::schema::key_decisions as key_decisions_db;
use crate::error::TaskMgrError;
use crate::loop_engine::branch;
use crate::loop_engine::calibrate;
use crate::loop_engine::claude;
use crate::loop_engine::config::{
    self, IterationOutcome, LoopConfig, PermissionMode, TASKS_JSON_DISALLOWED_TOOLS,
};
use crate::loop_engine::crash::CrashTracker;
use crate::loop_engine::deadline;
use crate::loop_engine::detection;
use crate::loop_engine::display;
use crate::loop_engine::env;
use crate::loop_engine::feedback;
use crate::loop_engine::git_reconcile::{
    check_git_for_task_completion, reconcile_external_git_completions, wrapper_commit,
};
use crate::loop_engine::guidance::SessionGuidance;
use crate::loop_engine::model;
use crate::loop_engine::monitor;
use crate::loop_engine::oauth;
use crate::loop_engine::output_parsing::{parse_completed_tasks, scan_output_for_completed_tasks};
use crate::loop_engine::prd_reconcile::{
    self as prd_reconcile, hash_file, mark_task_done, read_prd_metadata, reconcile_passes_with_db,
    update_prd_task_passes,
};
use crate::loop_engine::progress;
use crate::loop_engine::prompt::{self, BuildPromptParams};
use crate::loop_engine::signals::{self, SignalFlag, handle_human_review};
use crate::loop_engine::stale::StaleTracker;
use crate::loop_engine::status_queries::read_prd_hints;
use crate::loop_engine::usage::{self, UsageCheckResult};
use crate::loop_engine::watchdog;
use crate::loop_engine::worktree;
use crate::models::RunStatus;

/// Maximum consecutive reorder attempts before forcing algorithmic pick.
const MAX_CONSECUTIVE_REORDERS: u32 = 2;

/// Deprecation hint displayed at loop start when the claude CLI supports auto mode
/// but the user is not yet using it. Emitted to stderr once per session.
pub(crate) const AUTO_MODE_DEPRECATION_HINT: &str = concat!(
    "\x1b[33m[hint]\x1b[0m ",
    "The current permission model will be deprecated in a future release. ",
    "Set LOOP_ENABLE_AUTO_MODE=true to switch to auto mode. ",
    "Your current settings continue to work in the meantime."
);

/// Parameters for usage API monitoring within an iteration.
#[derive(Debug, Clone)]
pub struct UsageParams {
    /// Whether usage checking is enabled.
    pub enabled: bool,
    /// Usage percentage threshold (0-100) to trigger wait.
    pub threshold: u8,
    /// Fallback wait time in seconds when no reset time is available.
    pub fallback_wait: u64,
}

impl UsageParams {
    /// Create a disabled usage params (skips all checks).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            threshold: 92,
            fallback_wait: 300,
        }
    }
}

/// Parameters for a single iteration of the agent loop.
///
/// Groups the read-only parameters that `run_iteration()` needs,
/// keeping the mutable `IterationContext` as a separate argument.
pub struct IterationParams<'a> {
    /// Database connection
    pub conn: &'a Connection,
    /// Database directory (--dir flag, for task selection queries)
    pub db_dir: &'a Path,
    /// Git repository root (for source scanning, monitoring)
    pub project_root: &'a Path,
    /// Tasks directory (for signal files)
    pub tasks_dir: &'a Path,
    /// Current iteration number (1-based)
    pub iteration: u32,
    /// Maximum number of iterations
    pub max_iterations: u32,
    /// Current run ID
    pub run_id: &'a str,
    /// Path to base prompt.md file
    pub base_prompt_path: &'a Path,
    /// Optional path to steering.md
    pub steering_path: Option<&'a Path>,
    /// Delay between iterations
    pub inter_iteration_delay: Duration,
    /// Shared signal flag for SIGINT/SIGTERM
    pub signal_flag: &'a SignalFlag,
    /// Total elapsed seconds since loop start
    pub elapsed_secs: u64,
    /// Enable verbose output
    pub verbose: bool,
    /// Usage API monitoring parameters
    pub usage_params: &'a UsageParams,
    /// Optional path to PRD JSON file
    pub prd_path: Option<&'a Path>,
    /// Optional task prefix for ID normalization
    pub task_prefix: Option<&'a str>,
    /// Default model from PRD metadata (threaded from run_loop via PrdMetadata).
    pub default_model: Option<&'a str>,
    /// Default model from the per-project config (`.task-mgr/config.json`).
    pub project_default_model: Option<&'a str>,
    /// Default model from the per-user config (`$XDG_CONFIG_HOME/task-mgr/config.json`).
    pub user_default_model: Option<&'a str>,
    /// Permission mode for Claude subprocess invocation.
    pub permission_mode: &'a PermissionMode,
    /// Paths to sibling PRD JSON files (batch mode only, empty otherwise).
    pub batch_sibling_prds: &'a [PathBuf],
}

/// Result of a single iteration.
#[derive(Debug)]
pub struct IterationResult {
    /// What happened this iteration
    pub outcome: IterationOutcome,
    /// ID of the task that was attempted (if any)
    pub task_id: Option<String>,
    /// Files modified by the task (from task metadata)
    pub files_modified: Vec<String>,
    /// Whether the loop should stop after this iteration
    pub should_stop: bool,
    /// Claude's stdout output (for output-based completion detection)
    pub output: String,
    /// Effective model used for this iteration (post-crash-escalation).
    /// None for early exits (signal, rate-limit, etc.).
    pub effective_model: Option<String>,
    /// Effective `--effort` level used for this iteration, derived from task difficulty.
    /// None when difficulty is unset/unknown or for early exits.
    pub effective_effort: Option<&'static str>,
    /// Number of key decisions extracted and stored this iteration.
    pub key_decisions_count: u32,
}

/// Mutable context carried between iterations.
pub struct IterationContext {
    /// Last commit hash from previous iteration
    pub last_commit: Option<String>,
    /// Files modified in previous iteration
    pub last_files: Vec<String>,
    /// Accumulated session guidance from pause interactions
    pub session_guidance: SessionGuidance,
    /// Crash tracker for exponential backoff
    pub crash_tracker: CrashTracker,
    /// Stale iteration tracker
    pub stale_tracker: StaleTracker,
    /// Task ID hint from a reorder request
    pub reorder_hint: Option<String>,
    /// Count of consecutive reorders
    pub reorder_count: u32,
    /// Task ID from the previous iteration. Loop-thread-local — no concurrency concern.
    /// Used by crash escalation logic (FEAT-007) to detect same-task consecutive crashes.
    pub last_task_id: Option<String>,
    /// Whether the previous iteration crashed. Loop-thread-local — no concurrency concern.
    /// Used by crash escalation logic (FEAT-007) to trigger model escalation.
    pub last_was_crash: bool,
    /// Per-task effort overrides set after `Crash(PromptTooLong)`. Keys are
    /// task IDs, values are the effort level to use on the next attempt in
    /// place of the difficulty-derived default.
    pub effort_overrides: std::collections::HashMap<String, &'static str>,
    /// Per-task model overrides set after `Crash(PromptTooLong)` when effort
    /// downgrade is exhausted. Escalates to the 1M-context model variant so
    /// the task can fit in the larger context window. Uses `String` values
    /// (not `&'static str`) to allow future dynamic model IDs.
    pub model_overrides: std::collections::HashMap<String, String>,
}

impl IterationContext {
    /// Create a new iteration context with default state.
    pub fn new(max_crashes: u32) -> Self {
        Self {
            last_commit: None,
            last_files: Vec::new(),
            session_guidance: SessionGuidance::new(),
            crash_tracker: CrashTracker::new(max_crashes),
            stale_tracker: StaleTracker::default(),
            reorder_hint: None,
            reorder_count: 0,
            last_task_id: None,
            last_was_crash: false,
            effort_overrides: std::collections::HashMap::new(),
            model_overrides: std::collections::HashMap::new(),
        }
    }
}

/// Run a single iteration of the agent loop.
///
/// Returns `IterationResult` describing the outcome and whether to stop.
pub fn run_iteration(
    ctx: &mut IterationContext,
    params: &IterationParams,
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

    // Step 4: Build prompt (selects and claims task)
    let session_guidance_text = ctx.session_guidance.format_for_prompt();
    let prompt_params = BuildPromptParams {
        dir: params.db_dir,
        project_root: params.project_root,
        conn: params.conn,
        after_files: &ctx.last_files,
        run_id: Some(params.run_id),
        iteration: params.iteration,
        reorder_hint: effective_reorder_hint.as_deref(),
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
    };

    let prompt_result = match prompt::build_prompt(&prompt_params) {
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

            let (mid_pfx_clause, mid_pfx_param) = prefix_and(params.task_prefix);
            let mid_recovery_sql = format!(
                "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress' {mid_pfx_clause}"
            );
            let mid_params: Vec<&dyn rusqlite::types::ToSql> = match &mid_pfx_param {
                Some(p) => vec![p],
                None => vec![],
            };
            let recovered = params
                .conn
                .execute(&mid_recovery_sql, mid_params.as_slice())
                .unwrap_or(0);

            if recovered > 0 {
                eprintln!(
                    "Auto-recovered {} stale in_progress task(s), retrying task selection...",
                    recovered
                );
                // Retry build_prompt once with the same params
                match prompt::build_prompt(&prompt_params) {
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
    let effective_model = {
        let resolved = prompt_result.resolved_model.as_deref();
        let after_crash_escalation = match check_crash_escalation(
            ctx.last_task_id.as_deref(),
            &task_id,
            ctx.last_was_crash,
            resolved,
        ) {
            Some(escalated) => {
                let old = resolved.unwrap_or("(default)");
                eprintln!("Crash escalation: {} → {}", old, escalated);
                Some(escalated)
            }
            None => prompt_result.resolved_model,
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

    // Step 5: Print iteration header (with post-escalation effective_model + effort)
    display::print_iteration_header(
        params.iteration,
        params.max_iterations,
        &task_id,
        params.elapsed_secs,
        effective_model.as_deref(),
        effort,
    );

    // Step 6: Start activity monitor, spawn Claude subprocess, stop monitor.
    // Timeout is intentionally derived from the primary task's difficulty, not
    // the cluster — synergy partners don't lengthen wall-clock inactivity budgets.
    let monitor_handle = monitor::start_monitor(params.project_root);
    let timeout_config = watchdog::TimeoutConfig::from_difficulty(
        prompt_result.task_difficulty.as_deref(),
        Arc::clone(&monitor_handle.last_activity_epoch),
    );
    let claude_result = claude::spawn_claude(
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
            ..Default::default()
        },
    );
    monitor::stop_monitor(monitor_handle);
    claude::cleanup_ghost_sessions();
    let claude_result = claude_result?;

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
        });
    }

    // Step 6.5: Detect if Claude was killed by SIGINT/SIGTERM (exit 130/143).
    // Claude may be the terminal foreground group, so Ctrl+C goes to it instead
    // of us. Propagate the signal to our flag so the loop stops cleanly.
    if matches!(claude_result.exit_code, 130 | 143) {
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
                });
            }
        }
    }

    // Step 7.7: Extract learnings from output (best-effort, opt-out via env var)
    // Prefer the structured conversation from stream-json mode; fall back to raw output.
    let learning_source = claude_conversation.as_deref().unwrap_or(&claude_output);
    if !crate::learnings::ingestion::is_extraction_disabled() && !learning_source.is_empty() {
        match crate::learnings::ingestion::extract_learnings_from_output(
            params.conn,
            learning_source,
            Some(&task_id),
            Some(params.run_id),
            Some(params.db_dir),
            Some(params.signal_flag),
        ) {
            Ok(r) if r.learnings_extracted > 0 => {
                eprintln!(
                    "Extracted {} learning(s) from output",
                    r.learnings_extracted
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("Warning: learning extraction failed: {}", e),
        }
    }

    // Step 8: Record learning feedback
    if let Err(e) = feedback::record_iteration_feedback(params.conn, &shown_learning_ids, &outcome)
    {
        eprintln!("Warning: failed to record iteration feedback: {}", e);
    }

    // Step 8.5: Handle PromptTooLong — downgrade next-attempt effort and reset task.
    // The Claude CLI exceeded the model context window; crash-tracker backoff still
    // runs via update_trackers below, but we pre-seed a lower effort for the retry
    // and reset the task so the next iteration picks it up fresh rather than leaving
    // it wedged in_progress.
    if matches!(
        outcome,
        IterationOutcome::Crash(config::CrashType::PromptTooLong)
    ) {
        let has_recovery = match model::downgrade_effort(effort) {
            Some(next) => {
                eprintln!(
                    "Prompt is too long for {} at effort {} — downgrading to {} and resetting task",
                    task_id,
                    effort.unwrap_or("(default)"),
                    next,
                );
                ctx.effort_overrides.insert(task_id.clone(), next);
                true
            }
            None => {
                // Effort floor reached — try escalating to the 1M context model
                match model::to_1m_model(effective_model.as_deref()) {
                    Some(m1m) => {
                        eprintln!(
                            "Prompt is too long for {} at effort {} — effort floor reached, escalating to 1M context model ({}) and resetting task",
                            task_id,
                            effort.unwrap_or("(default)"),
                            m1m,
                        );
                        ctx.model_overrides.insert(task_id.clone(), m1m.to_string());
                        true
                    }
                    None => false,
                }
            }
        };

        if has_recovery {
            // Reset to todo so the next iteration retries with the downgraded
            // effort or escalated model.
            match params.conn.execute(
                "UPDATE tasks SET status = 'todo', started_at = NULL \
                 WHERE id = ? AND status = 'in_progress'",
                [&task_id],
            ) {
                Ok(1) => eprintln!("Reset task {} to todo after prompt overflow", task_id),
                Ok(_) => {}
                Err(e) => eprintln!(
                    "Warning: failed to reset task {} after prompt overflow: {}",
                    task_id, e,
                ),
            }
        } else {
            // All recovery options exhausted — block the task so it doesn't
            // consume crash-tracker budget and abort the entire loop.
            eprintln!(
                "Prompt is too long for {} at effort {} — all recovery exhausted (effort floor + 1M model); blocking task",
                task_id,
                effort.unwrap_or("(default)"),
            );
            match params.conn.execute(
                "UPDATE tasks SET status = 'blocked' \
                 WHERE id = ? AND status = 'in_progress'",
                [&task_id],
            ) {
                Ok(1) => eprintln!("Blocked task {} — prompt overflow unrecoverable", task_id),
                Ok(_) => {}
                Err(e) => eprintln!(
                    "Warning: failed to block task {} after prompt overflow: {}",
                    task_id, e,
                ),
            }
        }
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

    // Step 11.5: Update crash escalation context for next iteration
    ctx.last_task_id = Some(task_id.clone());
    ctx.last_was_crash = matches!(outcome, IterationOutcome::Crash(_));

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
    })
}

/// Result returned by `run_loop()`.
///
/// Carries the exit code and (when applicable) the worktree path so that
/// callers can perform post-loop cleanup.
#[derive(Debug)]
pub struct LoopResult {
    /// Exit code to pass to the process (0 = success, 1 = error, etc.)
    pub exit_code: i32,
    /// Worktree path used for this run.
    ///
    /// `Some` only when the loop actually created/reused a worktree (i.e.
    /// `use_worktrees = true` and a branch was specified). `None` when running
    /// directly in source_root or when no branch was configured.
    pub worktree_path: Option<PathBuf>,
    /// Branch name used for this run, from PRD metadata.
    ///
    /// Read by the batch runner to advance the chain — the next PRD branches from this.
    /// `None` on early-return error paths or when no branch was configured in the PRD.
    pub branch_name: Option<String>,
    /// True when the loop exited because a `.stop` file was detected.
    ///
    /// The engine consumes the signal file before returning, so callers that need
    /// to react to a mid-run stop (e.g. `run_batch`) must use this flag instead of
    /// re-checking the file system.
    pub was_stopped: bool,
}

/// Configuration for running the loop, built from CLI args + env.
pub struct LoopRunConfig {
    /// Database directory (--dir flag, default ".task-mgr/")
    ///
    /// Always resolves to `{source_root}/.task-mgr/` - the database stays
    /// in the original repo even when using worktrees.
    pub db_dir: PathBuf,
    /// Original git repository root (from `git rev-parse --show-toplevel`)
    ///
    /// Contains PRD files, prompts, progress.txt, and `.task-mgr/` database.
    /// This is where path resolution for PRD/prompt files happens.
    pub source_root: PathBuf,
    /// Working directory for Claude subprocess
    ///
    /// When using worktrees, this is the worktree path.
    /// When not using worktrees, this equals `source_root`.
    /// Claude runs here and makes code changes here.
    pub working_root: PathBuf,
    /// Path to PRD JSON file
    pub prd_file: PathBuf,
    /// Optional path to prompt file (default: derived from PRD)
    pub prompt_file: Option<PathBuf>,
    /// Loop configuration (thresholds, delays, etc.)
    pub config: LoopConfig,
    /// Optional path to external git repo for commit scanning (CLI override)
    pub external_repo: Option<PathBuf>,
    /// Paths to OTHER PRD JSON files in the batch (empty for single-PRD runs).
    /// Used to inject sibling PRD context into MILESTONE task prompts.
    pub batch_sibling_prds: Vec<PathBuf>,
    /// Base git ref for this run's worktree.
    ///
    /// When `Some`, passed as `start_point` to `ensure_worktree()` so the branch
    /// is created from the specified ref instead of HEAD. Set by the batch runner
    /// when `--chain` is active. `None` for standalone runs and chain=false batch runs.
    pub chain_base: Option<String>,
    /// Prefix mode for task ID namespacing during `init()`.
    ///
    /// `Auto` (default for single and batch runs): generates a deterministic prefix
    /// from `md5(branchName:filename)[:8]`, ensuring loop→batch continuity.
    /// `Explicit(prefix)`: CLI `--prefix` override.
    /// `Disabled`: no prefix (CLI `--no-prefix` flag).
    pub prefix_mode: PrefixMode,
}

/// Expected global skills for task-mgr loop workflows.
///
/// These skills (`.md` files in `~/.claude/commands/`) provide slash commands
/// that wrap common task-mgr operations for interactive Claude Code sessions.
const EXPECTED_GLOBAL_SKILLS: &[&str] = &[
    "tm-apply",
    "tm-learn",
    "tm-recall",
    "tm-invalidate",
    "tm-status",
    "tm-next",
];

/// Check if task-mgr global Claude Code skills are installed.
///
/// Prints a warning with installation instructions if any are missing.
/// Non-blocking — the loop continues regardless.
fn check_global_skills(source_root: &Path) {
    let home = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h),
        Err(_) => return, // Can't determine home dir; skip check silently
    };
    let global_dir = home.join(".claude").join("commands");

    let missing: Vec<&str> = EXPECTED_GLOBAL_SKILLS
        .iter()
        .filter(|name| !global_dir.join(format!("{}.md", name)).exists())
        .copied()
        .collect();

    if missing.is_empty() {
        return;
    }

    let repo_skill_dir = source_root.join(".claude").join("commands");
    let has_repo_copies = missing
        .iter()
        .any(|name| repo_skill_dir.join(format!("{}.md", name)).exists());

    eprintln!(
        "Warning: {} task-mgr skill(s) not found in ~/.claude/commands/: {}",
        missing.len(),
        missing.join(", ")
    );

    if has_repo_copies {
        eprintln!("  Install from this repo:");
        for name in &missing {
            let src = repo_skill_dir.join(format!("{}.md", name));
            if src.exists() {
                eprintln!("    cp {} {}/", src.display(), global_dir.display());
            }
        }
    } else {
        eprintln!(
            "  These skills provide /tm-learn, /tm-recall, /tm-invalidate, /tm-status, /tm-next"
        );
        eprintln!("  See the task-mgr README for installation instructions.");
    }
    eprintln!();
}

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
    // Step 1: Load environment
    env::load_env();

    // Step 1.5: Check for global Claude Code skills
    check_global_skills(&run_config.source_root);

    // Step 2: Validate git repo (source_root is the original repo)
    if let Err(e) = env::validate_git_repo(&run_config.source_root) {
        eprintln!("Error: {}", e);
        eprintln!("Hint: Run task-mgr from within a git repository.");
        return LoopResult {
            exit_code: 1,
            worktree_path: None,
            branch_name: None,
            was_stopped: false,
        };
    }

    // Step 3: Resolve paths (PRD, prompt, progress live in source_root)
    let mut paths = match env::resolve_paths(
        &run_config.prd_file,
        run_config.prompt_file.as_deref(),
        &run_config.source_root,
        None,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error resolving paths: {}", e);
            eprintln!(
                "Hint: Check that the PRD file path is correct relative to your project root."
            );
            return LoopResult {
                exit_code: 1,
                worktree_path: None,
                branch_name: None,
                was_stopped: false,
            };
        }
    };

    // Propagate resolved absolute path so all downstream code (init, prefix
    // generation, hash, etc.) uses the actual file location — which may be in
    // a sibling worktree rather than the local source_root.
    run_config.prd_file = paths.prd_file.clone();

    // Step 4: Ensure directories exist (in db_dir)
    if let Err(e) = env::ensure_directories(&run_config.db_dir) {
        eprintln!("Error creating directories: {}", e);
        return LoopResult {
            exit_code: 1,
            worktree_path: None,
            branch_name: None,
            was_stopped: false,
        };
    }

    // Step 4.5: Acquire exclusive loop lock — prevents concurrent loops on same DB.
    // Must be before any DB mutations (init, migrations, recovery).
    // Separate from tasks.db.lock (short-lived per-command) so read-only commands
    // like `status` and `stats` are not blocked.
    //
    // Read the PRD's taskPrefix BEFORE acquiring the lock so we can use a
    // per-prefix lock file (loop-{prefix}.lock) that allows concurrent loops
    // on different PRDs. Falls back to "loop.lock" when prefix is unknown.
    // Read both hints in a single file parse.
    let prd_hints = read_prd_hints(&run_config.prd_file);
    let pre_lock_branch = prd_hints.branch_name;
    let pre_lock_prefix: Option<String> = match &run_config.prefix_mode {
        // Explicit prefix (batch mode): use it directly, skip PRD hints.
        PrefixMode::Explicit(p) => Some(p.clone()),
        // Disabled: no prefix at all.
        PrefixMode::Disabled => None,
        // Auto: always generate deterministically from branchName + filename.
        // The JSON's taskPrefix field is ignored to prevent mismatch bugs.
        PrefixMode::Auto => {
            let filename = run_config
                .prd_file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            Some(generate_prefix(pre_lock_branch.as_deref(), filename))
        }
    }
    .and_then(|p| {
        // Only use prefix if it is safe for filenames
        if validate_prefix(&p).is_ok() {
            Some(p)
        } else {
            None
        }
    });
    let lock_name = match &pre_lock_prefix {
        Some(p) => format!("loop-{p}.lock"),
        None => "loop.lock".to_string(),
    };
    let prd_display = run_config.prd_file.display();
    let mut loop_lock = match LockGuard::acquire_named(&run_config.db_dir, &lock_name) {
        Ok(guard) => guard,
        Err(e) => {
            match &pre_lock_prefix {
                Some(p) => {
                    eprintln!(
                        "Error: cannot start loop for {prd_display} — another loop is already running (prefix={p}). {e}"
                    );
                    eprintln!(
                        "Hint: Each PRD gets its own lock file (loop-{{prefix}}.lock). If the other PRD is still running, wait for it to finish."
                    );
                }
                None => {
                    eprintln!(
                        "Error: cannot start loop for {prd_display} — another loop is already running on the global lock. {e}"
                    );
                    eprintln!(
                        "Hint: Each PRD uses its own lock file (loop-{{prefix}}.lock). If both PRDs lack taskPrefix, they collide on the global lock."
                    );
                }
            }
            return LoopResult {
                exit_code: 1,
                worktree_path: None,
                branch_name: None,
                was_stopped: false,
            };
        }
    };

    // Step 4.55: Enrich lock file with prefix/branch immediately after acquisition.
    // pre_lock_prefix and pre_lock_branch are already known from step 4.5.
    if let Err(e) = loop_lock.write_holder_info_extended(
        pre_lock_branch.as_deref(),
        run_config.working_root.to_str(),
        pre_lock_prefix.as_deref(),
    ) {
        eprintln!(
            "Warning: failed to write extended lock metadata: {} (continuing)",
            e
        );
    }

    // Step 4.6: Detect branch change (archive previous PRD if branch switched)
    match branch::detect_branch_change(
        &run_config.source_root,
        &run_config.db_dir,
        &paths.tasks_dir,
        run_config.config.yes_mode,
        pre_lock_prefix.as_deref(),
    ) {
        Ok(true) => {
            eprintln!("Branch change handled, continuing with new branch setup");
        }
        Ok(false) => {} // No change or first run
        Err(e) => {
            eprintln!(
                "Warning: branch change detection failed: {} (continuing)",
                e
            );
        }
    }

    // Step 5: Initialize PRD (creates schema + imports tasks, idempotent)
    // Uses run_config.prefix_mode: Auto for single runs, Explicit for batch mode.
    if let Err(e) = crate::commands::init(
        &run_config.db_dir,
        &[&run_config.prd_file],
        false, // force
        true,  // append
        true,  // update_existing
        false, // dry_run
        run_config.prefix_mode.clone(),
    ) {
        eprintln!("Error initializing PRD: {}", e);
        return LoopResult {
            exit_code: 1,
            worktree_path: None,
            branch_name: None,
            was_stopped: false,
        };
    }

    // Step 5.5: PRD hash — computed after worktree setup (step 8.5) since
    // Claude edits the worktree copy, not the source_root copy.
    #[allow(unused_assignments)]
    let mut prd_hash = String::new();

    // Step 6: Open DB connection (after init to ensure schema exists)
    let mut conn = match crate::db::open_connection(&run_config.db_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error opening database: {}", e);
            return LoopResult {
                exit_code: 1,
                worktree_path: None,
                branch_name: None,
                was_stopped: false,
            };
        }
    };

    if run_config.config.verbose {
        let canonical = run_config.db_dir.join("tasks.db");
        eprintln!("[verbose] Database path: {}", canonical.display());
        eprintln!(
            "[verbose] Source root:   {}",
            run_config.source_root.display()
        );
        eprintln!(
            "[verbose] Working root:  {}",
            run_config.working_root.display()
        );
    }

    // Step 6.5: Run any pending migrations (e.g. v4 adds external_git_repo column)
    if let Err(e) = crate::db::run_migrations(&mut conn) {
        eprintln!("Warning: failed to run migrations: {} (continuing)", e);
    }

    // Step 6.55: Reuse the prefix already determined at step 4.5 — no second file read.
    // pre_lock_prefix holds either the PRD's explicit taskPrefix or the deterministic
    // auto-generated value (same algorithm as init), so it matches after step 5 runs.
    let early_task_prefix: Option<String> = pre_lock_prefix.clone();

    // Step 6.6: Recover stale in_progress tasks from previous crashed/killed runs.
    // Safe because we hold the per-prefix loop lock — no other loop with the same
    // prefix can be running. (Loops on different prefixes CAN run concurrently.)
    // Recovery is scoped by prefix so concurrent loops don't reset each other.
    let (recovery_pfx_clause, recovery_pfx_param) = prefix_and(early_task_prefix.as_deref());
    let recovery_sql = format!(
        "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress' {recovery_pfx_clause}"
    );
    let recovery_params: Vec<&dyn rusqlite::types::ToSql> = match &recovery_pfx_param {
        Some(p) => vec![p as &dyn rusqlite::types::ToSql],
        None => vec![],
    };
    match conn.execute(&recovery_sql, recovery_params.as_slice()) {
        Ok(count) if count > 0 => {
            eprintln!(
                "Recovered {} stale in_progress task(s) from previous run",
                count
            );
        }
        Ok(_) => {}
        Err(e) => {
            // Hard error: if recovery fails, the loop will deadlock on blocked dependencies
            eprintln!("Error: failed to reset stale tasks: {}", e);
            return LoopResult {
                exit_code: 1,
                worktree_path: None,
                branch_name: None,
                was_stopped: false,
            };
        }
    }

    // Step 6.7: Auto-retire stale learnings at session start so recall quality
    // is high from the first task. Uses default thresholds (90 days, 10 shows, 5% rate).
    match crate::commands::curate::curate_retire(&conn, Default::default()) {
        Ok(result) if result.learnings_retired > 0 => {
            eprintln!(
                "Auto-retired {} stale learning(s) at session start",
                result.learnings_retired
            );
        }
        Ok(_) => {} // nothing to retire
        Err(e) => {
            eprintln!("Warning: auto-retire learnings failed: {} (continuing)", e);
        }
    }

    // Step 7: Read PRD metadata for branch name, task count, and external repo
    let prd_metadata = match read_prd_metadata(&conn, early_task_prefix.as_deref()) {
        Ok(meta) => meta,
        Err(e) => {
            eprintln!("Error reading PRD metadata: {}", e);
            return LoopResult {
                exit_code: 1,
                worktree_path: None,
                branch_name: None,
                was_stopped: false,
            };
        }
    };
    let branch_name = prd_metadata.branch_name;
    let task_count = prd_metadata.task_count;
    let task_prefix = prd_metadata.task_prefix;
    let default_model = prd_metadata.default_model;
    // Config-level defaults: fall below PRD default in the resolution chain.
    // The loop engine never prompts — it runs non-interactively — so these
    // are pure reads. Users pin a default via `task-mgr init` or
    // `task-mgr models set-default`.
    let project_default_model =
        crate::loop_engine::project_config::read_project_config(&run_config.db_dir).default_model;
    let user_default_model = crate::loop_engine::user_config::read_user_config().default_model;

    // Step 7.05: Now that task_prefix is known, re-derive per-PRD progress file.
    if let Some(ref pfx) = task_prefix {
        paths.progress_file = paths.tasks_dir.join(format!("progress-{}.txt", pfx));
    }

    // Step 7.1: Reconcile tasks that have passes: true in PRD but are not done in DB.
    // This catches tasks completed in a previous run where the DB status was never
    // updated (e.g., rate limit interrupted git detection, or loop exit reset them).
    reconcile_passes_with_db(&conn, &run_config.prd_file, task_prefix.as_deref());

    // Step 7.2: Setup pre-check for new task lists only.
    // "New" = no tasks are done yet (first-ever run, or all tasks were reset).
    // Non-blocking: prints a yellow warning banner but always continues.
    {
        let (pfx_clause, pfx_param) = prefix_and(task_prefix.as_deref());
        let done_sql = format!("SELECT COUNT(*) FROM tasks WHERE status = 'done' {pfx_clause}");
        let done_params: Vec<&dyn rusqlite::types::ToSql> = match &pfx_param {
            Some(p) => vec![p],
            None => vec![],
        };
        let done_count: i64 = conn
            .query_row(&done_sql, done_params.as_slice(), |row| row.get(0))
            .unwrap_or(0);
        let is_new_task_list = done_count == 0;

        if is_new_task_list && let Ok(home) = std::env::var("HOME") {
            let global_dir = PathBuf::from(home).join(".claude");
            let checks = pre_check_loop_setup(&global_dir);
            let blockers: Vec<_> = checks
                .iter()
                .filter(|c| c.severity == SetupSeverity::Blocker)
                .collect();
            if !blockers.is_empty() {
                eprintln!(
                    "\x1b[33m⚠ Setup warning: {} blocker(s) detected in ~/.claude/settings.json:\x1b[0m",
                    blockers.len()
                );
                for b in &blockers {
                    eprintln!("  \x1b[33m•\x1b[0m {}", b.message);
                    if let Some(ref fix) = b.fix_command {
                        eprintln!("    Fix: {fix}");
                    }
                }
                eprintln!("\x1b[33m  The loop will continue but tool calls may be blocked.\x1b[0m");
                eprintln!("  Run `task-mgr doctor --setup` for a full audit.");
                eprintln!();
            }
        }
    }

    // Resolve external git repo path: CLI flag overrides PRD metadata
    let external_repo_path: Option<PathBuf> = run_config
        .external_repo
        .clone()
        .or_else(|| prd_metadata.external_git_repo.map(PathBuf::from))
        .map(|p| {
            if p.is_absolute() {
                p
            } else {
                run_config.source_root.join(&p)
            }
        });

    // Step 8: Determine working_root (worktree or source_root)
    // If using worktrees and a branch is specified, create/use a worktree.
    // Otherwise, check out the branch in source_root (old behavior).
    // Track whether we actually set up a worktree so we can clean it up later.
    let mut actual_worktree_path: Option<PathBuf> = None;
    let working_root = if let Some(ref branch) = branch_name {
        if run_config.config.use_worktrees {
            // Create or reuse worktree for this branch
            match worktree::ensure_worktree(
                &run_config.source_root,
                branch,
                run_config.config.yes_mode,
                run_config.chain_base.as_deref(),
            ) {
                Ok(wt_path) => {
                    actual_worktree_path = Some(wt_path.clone());
                    wt_path
                }
                Err(e) => {
                    eprintln!("Error setting up worktree: {}", e);
                    return LoopResult {
                        exit_code: 1,
                        worktree_path: None,
                        branch_name: None,
                        was_stopped: false,
                    };
                }
            }
        } else {
            // Old behavior: checkout branch in source_root
            if let Err(e) =
                env::ensure_branch(&run_config.source_root, branch, run_config.config.yes_mode)
            {
                eprintln!("Error: {}", e);
                return LoopResult {
                    exit_code: 1,
                    worktree_path: None,
                    branch_name: None,
                    was_stopped: false,
                };
            }
            run_config.source_root.clone()
        }
    } else {
        // No branch specified, use source_root as working directory
        run_config.source_root.clone()
    };

    // Step 8.4: Ensure task files exist in the worktree.
    // If using a worktree, copy PRD JSON, prompt, and PRD markdown from source_root
    // if they don't already exist in the worktree.
    if working_root != run_config.source_root {
        let canonical_source = run_config
            .source_root
            .canonicalize()
            .unwrap_or_else(|_| run_config.source_root.clone());

        let copy_if_missing = |src: &Path| {
            if let Ok(rel) = src.strip_prefix(&canonical_source) {
                let dest = working_root.join(rel);
                if !dest.exists() && src.exists() {
                    if let Some(parent) = dest.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = std::fs::copy(src, &dest) {
                        eprintln!(
                            "Warning: failed to copy {} to worktree: {}",
                            rel.display(),
                            e
                        );
                    } else {
                        eprintln!("Copied {} to worktree", rel.display());
                    }
                }
            }
        };

        // PRD JSON (task list)
        copy_if_missing(&paths.prd_file);

        // Prompt file
        copy_if_missing(&paths.prompt_file);

        // PRD markdown (from prdFile field in JSON, if present)
        if let Ok(content) = std::fs::read_to_string(&paths.prd_file)
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(prd_md) = json.get("prdFile").and_then(|v| v.as_str())
        {
            let prd_md_path = paths
                .prd_file
                .parent()
                .unwrap_or(&paths.prd_file)
                .join(prd_md);
            copy_if_missing(&prd_md_path);
        }
    }

    // Step 8.5: Compute live PRD path (worktree copy if using worktrees, else source_root)
    // Claude edits the worktree copy, so hash checks and re-imports must use that path.
    // paths.prd_file is canonicalized by resolve_paths(); canonicalize source_root too
    // so strip_prefix works reliably (e.g. symlinks resolved on both sides).
    let live_prd_file = if working_root != run_config.source_root {
        let canonical_source = run_config
            .source_root
            .canonicalize()
            .unwrap_or_else(|_| run_config.source_root.clone());
        if let Ok(rel) = paths.prd_file.strip_prefix(&canonical_source) {
            working_root.join(rel)
        } else {
            eprintln!(
                "Warning: could not remap PRD to worktree (prd={}, source={})",
                paths.prd_file.display(),
                canonical_source.display()
            );
            paths.prd_file.clone()
        }
    } else {
        paths.prd_file.clone()
    };
    // If using a worktree, re-import from the worktree PRD to pick up any tasks
    // that were added in the worktree but not in source_root (e.g., tasks created
    // by Claude during a previous run that only exist in the worktree copy).
    if live_prd_file != run_config.prd_file
        && live_prd_file.exists()
        && let Err(e) = crate::commands::init(
            &run_config.db_dir,
            &[&live_prd_file],
            false, // force
            true,  // append
            true,  // update_existing
            false, // dry_run
            run_config.prefix_mode.clone(),
        )
    {
        eprintln!("Warning: worktree PRD re-import failed: {} (continuing)", e);
    }
    prd_hash = hash_file(&live_prd_file);
    // Override paths.prd_file so all iteration code (mark_task_done, reconcile, etc.)
    // reads/writes the worktree copy, not the source_root copy.
    paths.prd_file = live_prd_file.clone();

    // Step 9: Check uncommitted changes (in working_root)
    if let Err(e) = env::check_uncommitted_changes(&working_root, run_config.config.yes_mode) {
        eprintln!("Error: {}", e);
        return LoopResult {
            exit_code: 1,
            worktree_path: actual_worktree_path,
            branch_name: None,
            was_stopped: false,
        };
    }

    // Step 10: Calculate max iterations
    let max_iterations = if run_config.config.max_iterations > 0 {
        run_config.config.max_iterations as u32
    } else {
        config::auto_max_iterations(task_count) as u32
    };

    // Step 11: Create deadline if hours specified
    let prd_basename = run_config
        .prd_file
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if let Some(hours) = run_config.config.hours
        && let Err(e) = deadline::create_deadline(&paths.tasks_dir, &prd_basename, hours)
    {
        eprintln!("Error creating deadline: {}", e);
        return LoopResult {
            exit_code: 1,
            worktree_path: actual_worktree_path,
            branch_name: None,
            was_stopped: false,
        };
    }

    // Step 12: Begin run session
    let begin_result = match run_cmd::begin(&conn) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error beginning run: {}", e);
            deadline::cleanup_deadline(&paths.tasks_dir, &prd_basename);
            return LoopResult {
                exit_code: 1,
                worktree_path: actual_worktree_path,
                branch_name: None,
                was_stopped: false,
            };
        }
    };
    let run_id = begin_result.run_id;

    // Step 12.5: Reconcile external git completions at startup
    // Catches tasks completed in prior runs that are still marked incomplete
    if let Some(ref ext_repo) = external_repo_path {
        let count = reconcile_external_git_completions(
            ext_repo,
            &mut conn,
            &run_id,
            &paths.prd_file,
            task_prefix.as_deref(),
            run_config.config.external_git_scan_depth,
        );
        if count > 0 {
            eprintln!(
                "Startup reconciliation: marked {} task(s) done from external repo",
                count
            );
        }
    }

    // Step 12.7: Display any deferred key decisions from previous sessions
    match key_decisions_db::get_all_pending_decisions(&conn) {
        Ok(decisions) if !decisions.is_empty() => {
            eprintln!(
                "\n\x1b[33m⚑ {} deferred key decision(s) from previous sessions:\x1b[0m",
                decisions.len()
            );
            for d in &decisions {
                let task_ctx = d
                    .task_id
                    .as_deref()
                    .map(|t| format!(" [task: {}]", t))
                    .unwrap_or_default();
                eprintln!("  • {}{}", d.title, task_ctx);
                eprintln!("    {}", d.description);
            }
            eprintln!();
        }
        Ok(_) => {}
        Err(e) => {
            // Non-fatal: pre-v12 DB won't have this table
            eprintln!("Note: could not query deferred key decisions: {}", e);
        }
    }

    // Step 13: Install signal handler
    let signal_flag = SignalFlag::new();
    setup_signal_handler(signal_flag.clone());

    // Step 14: Resolve steering.md path
    let steering_path = paths.tasks_dir.join("steering.md");
    let steering = if steering_path.exists() {
        Some(steering_path.as_path())
    } else {
        None
    };

    // Step 15: Resolve permission mode (needed for banner hint below).
    // Resolved once at startup; re-checked each iteration for hot-reload.
    let mut permission_mode = config::resolve_permission_mode(&run_config.db_dir);

    if run_config.config.verbose {
        eprintln!("[verbose] Permission mode: {}", permission_mode);
    }

    // Step 15.5: Print session banner
    let branch_display = branch_name.as_deref().unwrap_or("(unknown)");
    let db_path = run_config.db_dir.join("tasks.db");
    let banner_hints = display::SessionBannerHints {
        db_path: &db_path,
        prefix: task_prefix.as_deref(),
        worktree_path: actual_worktree_path.as_deref(),
    };
    display::print_session_banner(
        &prd_basename,
        branch_display,
        max_iterations,
        run_config.config.hours,
        Some(&banner_hints),
    );

    // Step 15.6: Print auto-mode availability hint if applicable.
    // Fires when LOOP_AUTO_MODE_AVAILABLE=true and user is NOT already in Auto mode.
    // Informs the user that the current permission model will be deprecated.
    if let Ok(val) = std::env::var("LOOP_AUTO_MODE_AVAILABLE")
        && config::parse_bool_value(&val) == Some(true)
        && !matches!(permission_mode, config::PermissionMode::Auto { .. })
    {
        eprintln!("{}", AUTO_MODE_DEPRECATION_HINT);
    }

    // Step 15.7: Log requires_human task count so the user knows pauses are coming
    {
        let review_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE requires_human = 1 AND status != 'done'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if review_count > 0 {
            eprintln!("{} task(s) require human review", review_count);
        }
    }

    // Step 16: Build usage params
    let usage_params = UsageParams {
        enabled: run_config.config.usage_check_enabled,
        threshold: run_config.config.usage_threshold,
        fallback_wait: run_config.config.usage_fallback_wait,
    };

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
    while iteration < max_iterations as u32 {
        iteration += 1; // 1-based, incremented at top
        // Pre-iteration: refresh OAuth token if usage checking enabled
        if usage_params.enabled {
            oauth::ensure_valid_token();
        }

        // Check deadline
        if deadline::check_deadline(&paths.tasks_dir, &prd_basename) {
            eprintln!("Deadline reached, stopping loop");
            exit_reason = "deadline reached".to_string();
            exit_code = 0;
            break;
        }

        // Hot-reload permission mode: re-resolve each iteration so config.json
        // edits mid-loop take effect without restarting.
        let iter_permission_mode = config::resolve_permission_mode(&run_config.db_dir);
        if iter_permission_mode != permission_mode {
            eprintln!(
                "\x1b[36m[info]\x1b[0m Permission mode changed: {} → {}",
                permission_mode, iter_permission_mode
            );
            permission_mode = iter_permission_mode;
        }

        // Re-import PRD if Claude modified it during the previous iteration.
        // Use live_prd_file (worktree copy) since Claude edits in the worktree.
        let current_hash = hash_file(&live_prd_file);
        if current_hash != prd_hash {
            eprintln!("PRD file changed, re-importing tasks...");
            if let Err(e) = crate::commands::init(
                &run_config.db_dir,
                &[&live_prd_file],
                false, // force
                true,  // append
                true,  // update_existing
                false, // dry_run
                run_config.prefix_mode.clone(),
            ) {
                eprintln!("Warning: PRD re-import failed: {} (continuing)", e);
            }
            prd_hash = current_hash;
        }

        let elapsed = start_time.elapsed().as_secs();

        let iteration_params = IterationParams {
            conn: &conn,
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
        };

        let mut result = match run_iteration(&mut ctx, &iteration_params) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Iteration error: {}", e);
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

        // Log progress
        progress::log_iteration(
            &paths.progress_file,
            iteration,
            result.task_id.as_deref(),
            &result.outcome,
            &result.files_modified,
            result.effective_model.as_deref(),
            result.effective_effort,
        );

        // Extract and store key decisions (non-fatal: DB errors are warnings only)
        let key_decisions = detection::extract_key_decisions(&result.output);
        let mut kd_count: u32 = 0;
        for decision in &key_decisions {
            match key_decisions_db::insert_key_decision(
                &conn,
                &run_id,
                result.task_id.as_deref(),
                i64::from(iteration),
                decision,
            ) {
                Ok(_) => kd_count += 1,
                Err(e) => eprintln!(
                    "Warning: failed to store key decision '{}': {}",
                    decision.title, e
                ),
            }
        }
        result.key_decisions_count = kd_count;

        // Track last claimed task for cleanup on exit
        last_claimed_task = result.task_id.clone();
        if matches!(result.outcome, IterationOutcome::Completed) {
            last_claimed_task = None;
        }

        // Update run with last files
        if let Err(e) = run_cmd::update(
            &conn,
            &run_id,
            ctx.last_commit.as_deref(),
            Some(&result.files_modified),
        ) {
            eprintln!("Warning: failed to update run: {}", e);
        }

        // Record epoch before completion detection so we can later identify tasks
        // completed this iteration (used for human review triggering).
        let completion_epoch_start: i64 = conn
            .query_row("SELECT CAST(strftime('%s', 'now') AS INTEGER)", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);

        // Side-band `<task-status>` tag dispatch (FEAT-003).
        // Parses every tag regardless of claimed task_id, dispatches through
        // the existing command handlers, and (for Done) syncs PRD JSON.
        // Runs BEFORE `<completed>` detection so a Done tag that maps to the
        // claimed task sets task_marked_done_this_iteration below via outcome.
        let status_updates = detection::extract_status_updates(&result.output);
        let status_updates_applied = if !status_updates.is_empty() {
            apply_status_updates(
                &mut conn,
                &status_updates,
                Some(&run_id),
                Some(&paths.prd_file),
                task_prefix.as_deref(),
                Some(&paths.progress_file),
                Some(&run_config.db_dir),
            )
        } else {
            0
        };

        // Check for task completion via multiple detection paths.
        // Priority: <completed> tags > git commit > output scan > already-complete
        if let Some(ref task_id) = result.task_id
            && !matches!(result.outcome, IterationOutcome::Empty)
        {
            let mut task_marked_done_this_iteration = false;

            // If a <task-status>...:done</task-status> referenced the claimed
            // task, treat it identically to a <completed> tag: mark the task
            // as done for this iteration, clear claim tracking, and push the
            // outcome to Completed so stale/crash trackers reset.
            if status_updates_applied > 0
                && status_updates.iter().any(|u| {
                    matches!(u.status, detection::TaskStatusChange::Done) && &u.task_id == task_id
                })
            {
                task_marked_done_this_iteration = true;
                last_claimed_task = None;
                tasks_completed += 1;
                result.outcome = IterationOutcome::Completed;
                ctx.crash_tracker.record_success();
                eprintln!(
                    "Task {} completed (detected from <task-status> tag)",
                    task_id,
                );
            }

            // Primary: parse <completed> tags from output
            let completed_tags = parse_completed_tasks(&result.output);
            if !completed_tags.is_empty() {
                for completed_id in &completed_tags {
                    if let Ok(()) = mark_task_done(
                        &mut conn,
                        completed_id,
                        &run_id,
                        None,
                        &paths.prd_file,
                        task_prefix.as_deref(),
                    ) {
                        if completed_id == task_id {
                            last_claimed_task = None;
                            task_marked_done_this_iteration = true;
                        }
                        tasks_completed += 1;
                        result.outcome = IterationOutcome::Completed;
                        ctx.crash_tracker.record_success();
                        eprintln!(
                            "Task {} completed (detected from <completed> tag)",
                            completed_id
                        );
                    }
                }
            }

            // Fallback 1: git commit detection (only if no <completed> tags found)
            if completed_tags.is_empty() {
                if let Some(commit_hash) = check_git_for_task_completion(
                    &working_root,
                    task_id,
                    run_config.config.git_scan_depth,
                ) {
                    // Mark task done in DB
                    let task_ids = [task_id.clone()];
                    match complete_cmd::complete(
                        &mut conn,
                        &task_ids,
                        Some(&run_id),
                        Some(&commit_hash),
                        false, // force
                    ) {
                        Ok(_) => {
                            last_claimed_task = None;
                            tasks_completed += 1;
                            task_marked_done_this_iteration = true;

                            // Override outcome so stale/crash trackers reset — task was actually completed
                            result.outcome = IterationOutcome::Completed;
                            ctx.crash_tracker.record_success();

                            // Update PRD JSON to set passes: true
                            if let Err(e) = update_prd_task_passes(
                                &paths.prd_file,
                                task_id,
                                true,
                                task_prefix.as_deref(),
                            ) {
                                eprintln!(
                                    "Warning: failed to update PRD for task {}: {}",
                                    task_id, e
                                );
                            } else {
                                eprintln!(
                                    "Task {} completed (commit {})",
                                    task_id,
                                    &commit_hash[..7.min(commit_hash.len())]
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: failed to mark task {} as done in DB: {}",
                                task_id, e
                            );
                        }
                    }
                } else {
                    // Fallback: scan Claude's output for ANY completed task IDs.
                    // Claude may complete the claimed task or others in a single iteration,
                    // and commits happen in a different repo (e.g. restaurant_agent_ex/).
                    let completed_ids = scan_output_for_completed_tasks(
                        &result.output,
                        &conn,
                        task_prefix.as_deref(),
                    );
                    for completed_id in &completed_ids {
                        let ids = [completed_id.clone()];
                        match complete_cmd::complete(
                            &mut conn,
                            &ids,
                            Some(&run_id),
                            None, // no commit hash — different repo
                            false,
                        ) {
                            Ok(_) => {
                                // Clear tracker if the claimed task was completed via output scan
                                if result.task_id.as_deref() == Some(completed_id.as_str()) {
                                    last_claimed_task = None;
                                    task_marked_done_this_iteration = true;
                                }

                                tasks_completed += 1;

                                // Override outcome so stale/crash trackers reset — task was actually completed
                                result.outcome = IterationOutcome::Completed;
                                ctx.crash_tracker.record_success();

                                if let Err(e) = update_prd_task_passes(
                                    &paths.prd_file,
                                    completed_id,
                                    true,
                                    task_prefix.as_deref(),
                                ) {
                                    eprintln!(
                                        "Warning: failed to update PRD for task {}: {}",
                                        completed_id, e
                                    );
                                } else {
                                    eprintln!(
                                        "Task {} completed (detected from output)",
                                        completed_id
                                    );
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "Warning: failed to mark task {} as done: {}",
                                    completed_id, e
                                );
                            }
                        }
                    }
                }
            } // end: if completed_tags.is_empty()

            // Final fallback: Claude reports the task as "already complete" without committing.
            // This catches tasks completed in a prior run where the DB was never updated.
            // Use task_marked_done_this_iteration (not outcome) to avoid skipping when
            // <promise>COMPLETE</promise> set outcome to Completed but no prior path marked the task done.
            if !task_marked_done_this_iteration
                && detection::is_task_reported_already_complete(
                    &result.output,
                    task_id,
                    task_prefix.as_deref(),
                )
                && let Ok(()) = mark_task_done(
                    &mut conn,
                    task_id,
                    &run_id,
                    None,
                    &paths.prd_file,
                    task_prefix.as_deref(),
                )
            {
                last_claimed_task = None;
                tasks_completed += 1;
                result.outcome = IterationOutcome::Completed;
                ctx.crash_tracker.record_success();
                eprintln!("Task {} completed (reported as already done)", task_id);
            }

            // Wrapper commit: if task was completed but no git commit exists
            // (Claude couldn't commit in scoped permission mode), commit on its behalf.
            if task_marked_done_this_iteration
                && check_git_for_task_completion(
                    &working_root,
                    task_id,
                    run_config.config.git_scan_depth,
                )
                .is_none()
                && let Some(hash) = wrapper_commit(&working_root, task_id, "loop wrapper commit")
            {
                ctx.last_commit = Some(hash);
            }
        }

        // Post-iteration: reconcile external git completions
        // Catches tasks completed in the current iteration (and any missed from prior)
        if let Some(ref ext_repo) = external_repo_path
            && !matches!(result.outcome, IterationOutcome::Empty)
        {
            let count = reconcile_external_git_completions(
                ext_repo,
                &mut conn,
                &run_id,
                &paths.prd_file,
                task_prefix.as_deref(),
                run_config.config.external_git_scan_depth,
            );
            if count > 0 {
                tasks_completed += count as u32;

                // Override outcome so stale/crash trackers reset — task was actually completed
                result.outcome = IterationOutcome::Completed;
                ctx.crash_tracker.record_success();

                eprintln!(
                    "Post-iteration reconciliation: marked {} task(s) done",
                    count
                );
                // Clear tracker if the claimed task was reconciled as done
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

        // Trigger human review for requires_human tasks completed this iteration.
        // Queries by timestamp to capture all detection paths (tags, git, output scan,
        // external reconciliation). Pre-completed tasks have older timestamps and are skipped.
        if !matches!(result.outcome, IterationOutcome::Empty) {
            trigger_human_reviews(
                &conn,
                HumanReviewParams {
                    completion_epoch_start,
                    iteration,
                    session_guidance: &mut ctx.session_guidance,
                    prd_file: &paths.prd_file,
                    task_prefix: task_prefix.as_deref(),
                    default_model: default_model.as_deref(),
                    permission_mode: &permission_mode,
                },
            );
        }

        // Track iteration count (skip reorders and rate limits)
        match result.outcome {
            IterationOutcome::Reorder(_) | IterationOutcome::RateLimit => {
                // Don't count against iteration budget
                iteration -= 1;
            }
            IterationOutcome::Completed => {
                iterations_completed += 1;
            }
            _ => {
                iterations_completed += 1;
            }
        }

        // Retry tracking: increment consecutive_failures for non-Completed task failures.
        // Excluded: Empty (no task attempted), Reorder (not a failure), RateLimit (external).
        if let Some(ref task_id) = result.task_id
            && !matches!(
                result.outcome,
                IterationOutcome::Completed
                    | IterationOutcome::Empty
                    | IterationOutcome::Reorder(_)
                    | IterationOutcome::RateLimit
            )
            && let Err(e) = handle_task_failure(&mut conn, task_id, iteration as i64)
        {
            eprintln!("Warning: failed to start retry tracking transaction: {}", e);
        }

        // Track consecutive stale iterations and abort if stuck
        if matches!(result.outcome, IterationOutcome::NoEligibleTasks) {
            ctx.stale_tracker.check("stale", "stale"); // same hash → increment
            if ctx.stale_tracker.should_abort() {
                eprintln!(
                    "Aborting: no eligible tasks after {} consecutive stale iterations",
                    ctx.stale_tracker.count()
                );
                exit_code = 1;
                exit_reason = format!(
                    "no eligible tasks after {} consecutive stale iterations",
                    ctx.stale_tracker.count()
                );
                break;
            }
        } else {
            ctx.stale_tracker.check("a", "b"); // different hash → reset
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
        match conn.execute(
            "UPDATE tasks SET status = 'todo', started_at = NULL WHERE id = ? AND status = 'in_progress'",
            [task_id],
        ) {
            Ok(1) => eprintln!("Reset uncompleted task {} to todo", task_id),
            Ok(_) => {} // Already completed by reconciliation or status changed
            Err(e) => eprintln!("Warning: failed to reset task {}: {}", task_id, e),
        }
    }

    // Step 18: Record session guidance if any
    record_session_guidance(
        &ctx.session_guidance,
        &paths.progress_file,
        run_config.config.yes_mode,
    );

    // Step 19: End run session
    if let Err(e) = run_cmd::end(&conn, &run_id, final_run_status) {
        eprintln!("Warning: failed to end run: {}", e);
    }

    // Step 20: Recalibrate weights if completed
    if final_run_status == RunStatus::Completed {
        on_run_completed(&conn, task_prefix.as_deref());
    }

    // Step 21: Cleanup
    deadline::cleanup_deadline(&paths.tasks_dir, &prd_basename);
    signals::cleanup_signal_files_for_prefix(&paths.tasks_dir, task_prefix.as_deref());

    // Step 21.5: Worktree cleanup (if a worktree was used)
    if let Some(ref wt_path) = actual_worktree_path {
        let should_cleanup = if run_config.config.cleanup_worktree {
            // --cleanup-worktree flag: always attempt removal
            true
        } else if run_config.config.yes_mode {
            // --yes without --cleanup-worktree: keep worktree (auto-keep)
            false
        } else {
            // Interactive: prompt user
            eprint!("Remove worktree at '{}'? [y/N] ", wt_path.display());
            let mut response = String::new();
            let _ = std::io::stdin().read_line(&mut response);
            matches!(response.trim().to_lowercase().as_str(), "y" | "yes")
        };

        if should_cleanup {
            match worktree::remove_worktree(&run_config.source_root, wt_path) {
                Ok(true) => eprintln!("Worktree '{}' removed.", wt_path.display()),
                Ok(false) => eprintln!(
                    "Warning: worktree '{}' has uncommitted changes — not removed.",
                    wt_path.display()
                ),
                Err(e) => eprintln!(
                    "Warning: failed to remove worktree '{}': {} — continuing.",
                    wt_path.display(),
                    e
                ),
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
    }
}

/// Context parameters for `trigger_human_reviews`.
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
                eprintln!("Warning: could not execute human review query: {}", e);
                vec![]
            }
        },
        Err(e) => {
            eprintln!("Warning: could not prepare human review query: {}", e);
            vec![]
        }
    }
}

/// Trigger interactive human review for any `requires_human` tasks completed this iteration.
///
/// Queries tasks completed at or after `completion_epoch_start` to capture all detection
/// paths (tags, git, output scan, external reconciliation). For each such task, calls
/// `handle_human_review` and — if feedback was provided — calls `mutate_prd_from_feedback`
/// to update downstream tasks.
fn trigger_human_reviews(conn: &Connection, params: HumanReviewParams<'_>) {
    let HumanReviewParams {
        completion_epoch_start,
        iteration,
        session_guidance,
        prd_file,
        task_prefix,
        default_model,
        permission_mode,
    } = params;

    let review_tasks = query_human_review_tasks(conn, completion_epoch_start);

    for (task_id, title, notes, timeout) in review_tasks {
        let had_feedback = handle_human_review(
            io::BufReader::new(io::stdin()),
            &task_id,
            &title,
            notes.as_deref(),
            iteration,
            session_guidance,
            timeout,
        );
        if had_feedback {
            let feedback = session_guidance.last_text().unwrap_or("").to_string();
            prd_reconcile::mutate_prd_from_feedback(
                prd_file,
                &feedback,
                conn,
                task_prefix,
                default_model,
                permission_mode,
            );
        }
    }
}

/// Query pending key decisions for the run and prompt the user to resolve or defer each.
///
/// In yes_mode, all decisions are auto-deferred without prompting.
/// This function is a no-op when there are no pending decisions.
fn prompt_pending_key_decisions(conn: &Connection, run_id: &str, yes_mode: bool) {
    let decisions = match key_decisions_db::get_pending_decisions(conn, run_id) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Warning: failed to query pending key decisions: {}", e);
            return;
        }
    };

    if decisions.is_empty() {
        return;
    }

    if yes_mode {
        for decision in &decisions {
            if let Err(e) = key_decisions_db::defer_decision(conn, decision.id) {
                eprintln!("Warning: failed to defer decision {}: {}", decision.id, e);
            }
        }
        eprintln!(
            "Auto-deferred {} key decision(s) (yes_mode).",
            decisions.len()
        );
        return;
    }

    eprintln!(
        "\n╔══════════════════════════════════════════════════╗\
         \n║         KEY DECISIONS REQUIRING YOUR INPUT        ║\
         \n╚══════════════════════════════════════════════════╝"
    );

    for decision in &decisions {
        loop {
            eprintln!("\n┌─ Decision: {}", decision.title);
            eprintln!("│  {}", decision.description);
            eprintln!("│");
            for (i, opt) in decision.options.iter().enumerate() {
                let letter = (b'A' + i as u8) as char;
                eprintln!("│  {}) {} — {}", letter, opt.label, opt.description);
            }
            eprintln!("│  S) Skip (defer to next session)");
            eprint!("└─ Your choice: ");

            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_err() {
                // stdin unavailable — defer
                eprintln!("\nWarning: could not read stdin, deferring decision.");
                let _ = key_decisions_db::defer_decision(conn, decision.id);
                break;
            }

            let trimmed = input.trim().to_lowercase();

            if trimmed.is_empty() || trimmed == "s" || trimmed == "skip" {
                if let Err(e) = key_decisions_db::defer_decision(conn, decision.id) {
                    eprintln!("Warning: failed to defer decision: {}", e);
                } else {
                    eprintln!("Decision deferred.");
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
                        eprintln!("Warning: failed to resolve decision: {}", e);
                    } else {
                        eprintln!("Decision resolved: {}", resolution);
                    }
                    break;
                }
                Err(_) => {
                    eprintln!(
                        "Invalid choice — enter a letter (A–{}) or S to skip.",
                        (b'A' + decision.options.len() as u8 - 1) as char
                    );
                }
            }
        }
    }
}

/// Install SIGINT, SIGTERM, and SIGQUIT handlers that set the signal flag.
///
/// Uses `signal-hook` to register OS-level signal handlers that set an
/// `AtomicBool` directly from signal context — no async polling needed.
/// This works even when the tokio runtime thread is blocked in synchronous I/O
/// (e.g., reading Claude subprocess stdout).
///
/// Second Ctrl+C restores the default handler, which force-kills immediately.
fn setup_signal_handler(signal_flag: SignalFlag) {
    let flag = signal_flag.inner();

    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGINT, SIGQUIT, SIGTERM};

        // First SIGINT sets the flag; second SIGINT restores default (immediate kill).
        // Both registrations are needed: `register` sets the flag, and
        // `register_conditional_default` emulates the default handler when
        // the flag is already true.
        if let Err(e) = signal_hook::flag::register(SIGINT, flag.clone()) {
            eprintln!("Warning: failed to install SIGINT handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register_conditional_default(SIGINT, flag.clone()) {
            eprintln!(
                "Warning: failed to install SIGINT conditional default: {}",
                e
            );
        }
        if let Err(e) = signal_hook::flag::register(SIGTERM, flag.clone()) {
            eprintln!("Warning: failed to install SIGTERM handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register(SIGQUIT, flag) {
            eprintln!("Warning: failed to install SIGQUIT handler: {}", e);
        }
    }

    #[cfg(not(unix))]
    {
        use signal_hook::consts::SIGINT;
        if let Err(e) = signal_hook::flag::register(SIGINT, flag.clone()) {
            eprintln!("Warning: failed to install SIGINT handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register_conditional_default(SIGINT, flag) {
            eprintln!(
                "Warning: failed to install SIGINT conditional default: {}",
                e
            );
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
                eprintln!(
                    "Calibrated selection weights: file_overlap={}, priority_base={}",
                    weights.file_overlap, weights.priority_base
                );
            }
        }
        Err(e) => {
            eprintln!("Warning: weight calibration failed: {}", e);
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
        eprint!("Session guidance was recorded. Save to progress.txt? (y/N) ");
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => {
                let trimmed = input.trim().to_lowercase();
                if trimmed != "y" && trimmed != "yes" {
                    eprintln!("Session guidance discarded.");
                    return;
                }
            }
            Err(_) => {
                // stdin not available (non-interactive), skip
                eprintln!("Warning: could not read stdin, skipping guidance recording");
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
                eprintln!(
                    "Warning: could not write session guidance to {}: {}",
                    progress_path.display(),
                    e
                );
            } else {
                eprintln!("Session guidance saved to {}", progress_path.display());
            }
        }
        Err(e) => {
            eprintln!(
                "Warning: could not open progress file {}: {}",
                progress_path.display(),
                e
            );
        }
    }
}

/// Dispatch a list of `<task-status>` side-band updates.
///
/// For each update:
/// 1. Call the existing status command handler (`complete`, `fail`, `skip`,
///    `irrelevant`, `unblock`, or `reset_tasks`) — NEVER bypass via raw SQL.
/// 2. On a successful `Done` transition, flip the matching PRD JSON entry's
///    `passes` field to `true` (symmetric with `task-mgr add`'s DB + JSON
///    sync). JSON-sync failures log a warning but do NOT roll back the DB,
///    mirroring `add.rs` behavior.
///
/// Dispatch failures (e.g. task not `in_progress` when `done` is claimed —
/// learning [1475]) are logged to stderr with the task id + status and the
/// loop continues to the next tag. Never silently swallow errors.
///
/// Returns the number of updates that dispatched successfully.
pub fn apply_status_updates(
    conn: &mut Connection,
    updates: &[detection::TaskStatusUpdate],
    run_id: Option<&str>,
    prd_path: Option<&Path>,
    task_prefix: Option<&str>,
    progress_path: Option<&Path>,
    db_dir: Option<&Path>,
) -> u32 {
    use detection::TaskStatusChange;

    let mut applied: u32 = 0;
    for update in updates {
        let task_ids = [update.task_id.clone()];
        let dispatch: Result<(), TaskMgrError> = match update.status {
            TaskStatusChange::Done => {
                // Auto-claim unclaimed tasks: todo -> in_progress -> done
                let current: Result<String, _> = conn.query_row(
                    "SELECT status FROM tasks WHERE id = ?",
                    [update.task_id.as_str()],
                    |row| row.get(0),
                );
                if let Ok(ref s) = current
                    && s == "todo"
                {
                    let _ = conn.execute(
                        "UPDATE tasks SET status = 'in_progress', \
                         started_at = datetime('now'), \
                         updated_at = datetime('now') \
                         WHERE id = ? AND status = 'todo'",
                        [update.task_id.as_str()],
                    );
                    if let Some(rid) = run_id {
                        let iter: i64 = conn
                            .query_row(
                                "SELECT COALESCE(MAX(iteration), 0) + 1 FROM run_tasks WHERE run_id = ?",
                                [rid],
                                |row| row.get(0),
                            )
                            .unwrap_or(1);
                        let _ = conn.execute(
                            "INSERT OR IGNORE INTO run_tasks (run_id, task_id, iteration, status) \
                             VALUES (?, ?, ?, 'started')",
                            rusqlite::params![rid, update.task_id, iter],
                        );
                    }
                }
                complete_cmd::complete(conn, &task_ids, run_id, None, false).map(|_| ())
            }
            TaskStatusChange::Failed => crate::commands::fail(
                conn,
                &task_ids,
                None,
                crate::cli::FailStatus::Blocked,
                run_id,
                false,
            )
            .map(|_| ()),
            TaskStatusChange::Skipped => {
                crate::commands::skip(conn, &task_ids, "<task-status> tag", run_id).map(|_| ())
            }
            TaskStatusChange::Irrelevant => {
                crate::commands::irrelevant(conn, &task_ids, "<task-status> tag", run_id, None)
                    .map(|_| ())
            }
            TaskStatusChange::Unblock => {
                crate::commands::unblock(conn, &update.task_id).map(|_| ())
            }
            TaskStatusChange::Reset => {
                crate::commands::reset::reset_tasks(conn, &task_ids).map(|_| ())
            }
        };

        match dispatch {
            Ok(()) => {
                applied += 1;
                // Only Done flips PRD JSON `passes` — other transitions leave
                // `passes: false` unchanged.
                if matches!(update.status, TaskStatusChange::Done) {
                    if let Some(path) = prd_path
                        && let Err(e) =
                            update_prd_task_passes(path, &update.task_id, true, task_prefix)
                    {
                        eprintln!(
                            "Warning: <task-status> dispatched {} to done in DB but PRD JSON sync failed ({}): {}",
                            update.task_id,
                            path.display(),
                            e,
                        );
                    }
                    // Milestone hook: when a MILESTONE-* task flips to done,
                    // append a summary block to progress-*.txt covering every
                    // entry since the last milestone summary, with crash-
                    // avoidance recommendations for any task that crashed/
                    // overflowed ≥2 times in the window.
                    // Hyphen-anchored to avoid false matches like
                    // `PRE-MILESTONE-NOTES` or `MILESTONEISH-1`.
                    let is_milestone = update.task_id.contains("-MILESTONE-")
                        || update.task_id.starts_with("MILESTONE-")
                        || update.task_id == "MILESTONE"
                        || update.task_id.ends_with("-MILESTONE");
                    if is_milestone && let Some(pp) = progress_path {
                        progress::summarize_milestone(pp, &update.task_id, db_dir);
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: <task-status>{}:{:?}</task-status> dispatch failed: {}",
                    update.task_id, update.status, e,
                );
            }
        }
    }
    applied
}

/// Check whether crash recovery should escalate the model for this iteration.
///
/// Returns `Some(escalated_model)` when BOTH conditions are met:
/// 1. The current task is the same as the previous iteration's task
/// 2. The previous iteration crashed (`last_was_crash`)
///
/// When `resolved_model` is `None`, assumes `SONNET_MODEL` baseline
/// and escalates to `OPUS_MODEL` (architect decision: None crash → opus).
///
/// Escalation is independent of `CrashTracker` backoff logic.
// TODO(FEAT-007): Implement escalation logic
pub fn check_crash_escalation(
    last_task_id: Option<&str>,
    current_task_id: &str,
    last_was_crash: bool,
    resolved_model: Option<&str>,
) -> Option<String> {
    // Escalation requires BOTH same task AND previous crash
    if !last_was_crash {
        return None;
    }
    if last_task_id != Some(current_task_id) {
        return None;
    }
    // None / empty / whitespace model: assume sonnet baseline, escalate to opus
    match normalize_baseline(resolved_model) {
        None => Some(model::OPUS_MODEL.to_string()),
        Some(m) => model::escalate_model(Some(m)),
    }
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

/// Escalate the model for a task in the DB when consecutive failures reach the threshold.
///
/// Follows the same sonnet-baseline pattern as `check_crash_escalation`:
/// - `None` or empty model assumes sonnet baseline → escalates to opus.
/// - Sonnet → opus, Haiku → sonnet, Opus → opus (no-op at ceiling).
///
/// Returns `Some(new_model)` if escalation fired, `None` if below threshold or
/// the model tier is unknown. The DB is updated in-place when `Some` is returned.
pub fn escalate_task_model_if_needed(
    conn: &Connection,
    task_id: &str,
    new_count: i32,
) -> TaskMgrResult<Option<String>> {
    if !should_escalate_for_consecutive_failures(new_count) {
        return Ok(None);
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
    Ok(escalated)
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
pub fn auto_block_task(
    conn: &Connection,
    task_id: &str,
    consecutive_failures: i32,
    current_iteration: i64,
) -> TaskMgrResult<()> {
    let msg = format!(
        "Auto-blocked after {} consecutive failures (task: {})",
        consecutive_failures, task_id
    );
    conn.execute(
        "UPDATE tasks SET status = 'blocked', last_error = ?, blocked_at_iteration = ?, updated_at = datetime('now') WHERE id = ?",
        rusqlite::params![msg, current_iteration, task_id],
    )?;
    Ok(())
}

/// Increment consecutive failure count, escalate model tier if needed, and auto-block if the
/// task has exhausted its retry budget. All DB writes are wrapped in a single transaction.
///
/// `current_iteration` is used to set `blocked_at_iteration` on auto-blocked tasks for
/// decay tracking. Escalation is skipped when auto-block fires on the same iteration
/// (the escalated model would never be used).
pub fn handle_task_failure(
    conn: &mut Connection,
    task_id: &str,
    current_iteration: i64,
) -> TaskMgrResult<()> {
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

    // Only escalate if auto-block won't immediately follow (escalated model would never be used)
    if !should_auto_block(new_count, max_retries)
        && let Err(e) = escalate_task_model_if_needed(&tx, task_id, new_count)
    {
        eprintln!("Warning: failed to escalate model for {}: {}", task_id, e);
    }

    if should_auto_block(new_count, max_retries) {
        if let Err(e) = auto_block_task(&tx, task_id, new_count, current_iteration) {
            eprintln!("Warning: failed to auto-block task {}: {}", task_id, e);
        } else {
            eprintln!(
                "Auto-blocked task {} after {} consecutive failures",
                task_id, new_count
            );
        }
    }

    tx.commit()?;

    Ok(())
}

/// Build an `IterationResult` for a prompt overflow, logging the error to stderr.
fn prompt_overflow_result(critical_size: usize, budget: usize, task_id: String) -> IterationResult {
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
    }
}

/// Probe whether the CLI rate limit has been lifted by spawning a minimal Claude call.
///
/// Sends `claude -p "." --print --max-turns 1 --no-session-persistence` and checks
/// whether the output still contains rate-limit patterns. Returns `true` if the
/// limit appears to be lifted (Claude responds without a rate-limit error).
fn probe_rate_limit_lifted(permission_mode: &PermissionMode) -> bool {
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
fn update_trackers(ctx: &mut IterationContext, outcome: &IterationOutcome) -> bool {
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

    // --- IterationContext tests ---

    #[test]
    fn test_iteration_context_new() {
        let ctx = IterationContext::new(5);
        assert!(ctx.last_commit.is_none());
        assert!(ctx.last_files.is_empty());
        assert!(ctx.session_guidance.is_empty());
        assert!(ctx.reorder_hint.is_none());
        assert_eq!(ctx.reorder_count, 0);
        assert!(ctx.last_task_id.is_none());
        assert!(!ctx.last_was_crash);
    }

    // --- IterationResult tests ---

    #[test]
    fn test_iteration_result_fields() {
        let result = IterationResult {
            outcome: IterationOutcome::Completed,
            task_id: Some("FEAT-001".to_string()),
            files_modified: vec!["src/lib.rs".to_string()],
            should_stop: false,
            output: String::new(),
            effective_model: None,
            effective_effort: None,
            key_decisions_count: 0,
        };
        assert_eq!(result.task_id, Some("FEAT-001".to_string()));
        assert!(!result.should_stop);
    }

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

    // --- MAX_CONSECUTIVE_REORDERS constant ---

    #[test]
    fn test_max_consecutive_reorders_is_2() {
        assert_eq!(MAX_CONSECUTIVE_REORDERS, 2);
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
        ctx.stale_tracker.check("stale", "stale");
        assert!(
            !ctx.stale_tracker.should_abort(),
            "1 stale should not abort"
        );

        // Second stale
        ctx.stale_tracker.check("stale", "stale");
        assert!(
            !ctx.stale_tracker.should_abort(),
            "2 stale should not abort"
        );

        // Third stale
        ctx.stale_tracker.check("stale", "stale");
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
        ctx.stale_tracker.check("stale", "stale");
        ctx.stale_tracker.check("stale", "stale");
        assert_eq!(ctx.stale_tracker.count(), 2);

        // Non-stale resets
        ctx.stale_tracker.check("a", "b");
        assert_eq!(
            ctx.stale_tracker.count(),
            0,
            "Non-stale outcome should reset tracker"
        );
        assert!(!ctx.stale_tracker.should_abort());

        // One more stale — not enough to abort
        ctx.stale_tracker.check("stale", "stale");
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

    // --- check_crash_escalation tests ---
    //
    // Active tests validate the no-escalation paths (pass against stub).
    // Tests below verify FEAT-007 crash escalation behavior.

    /// First iteration: no previous task context, no crash — no escalation.
    #[test]
    fn test_crash_escalation_first_iteration_no_crash() {
        let result = check_crash_escalation(None, "FEAT-001", false, Some(SONNET_MODEL));
        assert_eq!(
            result, None,
            "first iteration without crash must not escalate"
        );
    }

    /// First iteration with crash: no previous task to compare — no escalation.
    /// Edge case: last_task_id=None means we can't determine same-task.
    #[test]
    fn test_crash_escalation_first_iteration_with_crash() {
        let result = check_crash_escalation(None, "FEAT-001", true, Some(SONNET_MODEL));
        assert_eq!(
            result, None,
            "first iteration crash has no previous task context, cannot escalate"
        );
    }

    /// Same task but no crash — no escalation.
    #[test]
    fn test_crash_escalation_same_task_no_crash() {
        let result =
            check_crash_escalation(Some("FEAT-001"), "FEAT-001", false, Some(SONNET_MODEL));
        assert_eq!(result, None, "same task without crash must not escalate");
    }

    /// Different task with crash — no escalation (crash on a different task
    /// does not carry forward).
    #[test]
    fn test_crash_escalation_different_task_with_crash() {
        let result = check_crash_escalation(Some("FEAT-001"), "FEAT-002", true, Some(SONNET_MODEL));
        assert_eq!(
            result, None,
            "crash on different task must not escalate for new task"
        );
    }

    /// AC: same task + crash + haiku model → escalate to sonnet.
    #[test]

    fn test_crash_escalation_haiku_to_sonnet() {
        let result = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(HAIKU_MODEL));
        assert_eq!(
            result,
            Some(SONNET_MODEL.to_string()),
            "haiku crash on same task must escalate to sonnet"
        );
    }

    /// AC: same task + crash + sonnet model → escalate to opus.
    #[test]

    fn test_crash_escalation_sonnet_to_opus() {
        let result = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(SONNET_MODEL));
        assert_eq!(
            result,
            Some(OPUS_MODEL.to_string()),
            "sonnet crash on same task must escalate to opus"
        );
    }

    /// AC: same task + crash + already opus → stays opus (ceiling, no panic).
    #[test]

    fn test_crash_escalation_opus_ceiling() {
        let result = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(OPUS_MODEL));
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
        let result = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, None);
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
            let result = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(bad));
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
        let no_crash =
            check_crash_escalation(Some("FEAT-001"), "FEAT-001", false, Some(SONNET_MODEL));
        assert_eq!(no_crash, None, "same task without crash must NOT escalate");

        // Only crash (different task) — must NOT escalate
        let diff_task =
            check_crash_escalation(Some("FEAT-001"), "FEAT-002", true, Some(SONNET_MODEL));
        assert_eq!(diff_task, None, "crash on different task must NOT escalate");

        // BOTH conditions — MUST escalate
        let both = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(SONNET_MODEL));
        assert_eq!(
            both,
            Some(OPUS_MODEL.to_string()),
            "same task + crash MUST escalate"
        );
    }

    // ===== TEST-004: Comprehensive crash recovery escalation tests =====

    /// AC: Crash on task A, success on task A, crash on task A again.
    /// After success, last_was_crash is false, so the next crash escalates from
    /// the base model (not from the previously escalated model).
    #[test]
    fn test_crash_escalation_success_resets_escalation() {
        // First crash: haiku → sonnet
        let first = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(HAIKU_MODEL));
        assert_eq!(first, Some(SONNET_MODEL.to_string()));

        // Success: last_was_crash becomes false. Simulating with false:
        let after_success =
            check_crash_escalation(Some("FEAT-001"), "FEAT-001", false, first.as_deref());
        assert_eq!(
            after_success, None,
            "After success, no crash escalation should occur"
        );

        // Crash again on same task with original base model:
        // In real flow, resolved_model would come from build_prompt fresh (haiku again)
        let second_crash =
            check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(HAIKU_MODEL));
        assert_eq!(
            second_crash,
            Some(SONNET_MODEL.to_string()),
            "After success reset, crash escalates from base model again"
        );
    }

    /// AC: Crash on task A, then crash on task B → no escalation for task B.
    /// The crash escalation is task-scoped.
    #[test]
    fn test_crash_escalation_task_boundary_isolation() {
        // Crash on task A: haiku → sonnet
        let crash_a = check_crash_escalation(Some("TASK-A"), "TASK-A", true, Some(HAIKU_MODEL));
        assert_eq!(crash_a, Some(SONNET_MODEL.to_string()));

        // Now task B is selected. last_task_id is "TASK-A", current is "TASK-B".
        // Even though last_was_crash is true, different task → no escalation.
        let crash_b = check_crash_escalation(Some("TASK-A"), "TASK-B", true, Some(HAIKU_MODEL));
        assert_eq!(
            crash_b, None,
            "Crash escalation must not carry across task boundaries"
        );
    }

    /// AC: Crash escalation is independent of CrashTracker backoff count.
    /// check_crash_escalation doesn't inspect CrashTracker — it only uses
    /// last_task_id, current_task_id, last_was_crash, and resolved_model.
    #[test]
    fn test_crash_escalation_independent_of_crash_tracker() {
        // Regardless of how many times CrashTracker has recorded crashes,
        // check_crash_escalation only cares about the 4 parameters.
        // Same inputs produce same outputs — no hidden state.
        let result1 = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(HAIKU_MODEL));
        let result2 = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(HAIKU_MODEL));
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
        // First crash: haiku → sonnet
        let first = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, Some(HAIKU_MODEL));
        assert_eq!(
            first,
            Some(SONNET_MODEL.to_string()),
            "first crash: haiku → sonnet"
        );

        // Second crash: feed escalated model back in (sonnet → opus)
        let second = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, first.as_deref());
        assert_eq!(
            second,
            Some(OPUS_MODEL.to_string()),
            "second crash: sonnet → opus"
        );

        // Third crash: opus → opus (ceiling)
        let third = check_crash_escalation(Some("FEAT-001"), "FEAT-001", true, second.as_deref());
        assert_eq!(
            third,
            Some(OPUS_MODEL.to_string()),
            "third crash: opus stays at ceiling"
        );
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

    // --- Signal flag propagation from Claude exit code ---

    #[test]
    fn test_signal_flag_set_on_exit_code_130() {
        let flag = SignalFlag::new();
        assert!(!flag.is_signaled());

        // Simulate what run_iteration does when Claude exits with 130 (SIGINT)
        let exit_code = 130;
        if matches!(exit_code, 130 | 143) {
            flag.set();
        }
        assert!(flag.is_signaled(), "Exit code 130 should set signal flag");
    }

    #[test]
    fn test_signal_flag_set_on_exit_code_143() {
        let flag = SignalFlag::new();
        assert!(!flag.is_signaled());

        let exit_code = 143;
        if matches!(exit_code, 130 | 143) {
            flag.set();
        }
        assert!(flag.is_signaled(), "Exit code 143 should set signal flag");
    }

    #[test]
    fn test_signal_flag_not_set_on_normal_exit_codes() {
        for exit_code in [0, 1, 127, 137, 139] {
            let flag = SignalFlag::new();
            if matches!(exit_code, 130 | 143) {
                flag.set();
            }
            assert!(
                !flag.is_signaled(),
                "Exit code {} should not set signal flag",
                exit_code
            );
        }
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
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures, max_retries) VALUES ('T-001', 'Test', 'in_progress', 3, 3)",
            [],
        )
        .unwrap();

        auto_block_task(&conn, "T-001", 3, 1).unwrap();

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

        let result = escalate_task_model_if_needed(&conn, "T-001", 2).unwrap();
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

        let result = escalate_task_model_if_needed(&conn, "T-001", 2).unwrap();
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

    /// Task with None model at 2 consecutive failures → model set to opus (sonnet baseline).
    #[test]
    fn test_model_escalation_none_model_to_opus() {
        let (_dir, conn) = setup_test_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, consecutive_failures) VALUES ('T-001', 'Test', 'in_progress', 0)",
            [],
        )
        .unwrap();

        let result = escalate_task_model_if_needed(&conn, "T-001", 2).unwrap();
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

        let result = escalate_task_model_if_needed(&conn, "T-001", 1).unwrap();
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

    // --- apply_status_updates dispatcher tests (FEAT-003) ---
    //
    // These exercise the DB side of the side-band <task-status> path. The
    // engine's in-iteration wiring (outcome flip, tasks_completed bump, claim
    // clearing) is covered by the iteration-level tests elsewhere in this
    // file; here we cover the pure dispatcher contract: command dispatch,
    // PRD JSON sync, warning-on-state-violation.

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
    fn write_minimal_prd(dir: &std::path::Path, ids: &[&str]) -> PathBuf {
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
        let applied =
            apply_status_updates(&mut conn, &updates, None, Some(&prd_path), None, None, None);
        assert_eq!(applied, 1);

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
        let applied =
            apply_status_updates(&mut conn, &updates, None, Some(&prd_path), None, None, None);
        assert_eq!(applied, 1, "todo task must be auto-claimed then completed");

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
        let applied = apply_status_updates(
            &mut conn,
            &updates,
            Some("run-1"),
            Some(&prd_path),
            None,
            None,
            None,
        );
        assert_eq!(applied, 1);

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
        let applied =
            apply_status_updates(&mut conn, &updates, None, Some(&prd_path), None, None, None);
        assert_eq!(applied, 1);

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
        let applied =
            apply_status_updates(&mut conn, &updates, None, Some(&prd_path), None, None, None);
        assert_eq!(
            applied, 1,
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
        let applied =
            apply_status_updates(&mut conn, &updates, None, Some(&prd_path), None, None, None);
        assert_eq!(applied, 1);

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
        let applied = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            Some(&progress_path),
            None,
        );
        assert_eq!(applied, 1);

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
        let applied = apply_status_updates(
            &mut conn,
            &updates,
            None,
            Some(&prd_path),
            None,
            Some(&progress_path),
            None,
        );
        assert_eq!(applied, 1);

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
        let applied =
            apply_status_updates(&mut conn, &updates, None, Some(&prd_path), None, None, None);
        assert_eq!(applied, 1, "one dispatch failed, one succeeded");

        let status_b: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'FEAT-B'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status_b, "done");
    }
}
