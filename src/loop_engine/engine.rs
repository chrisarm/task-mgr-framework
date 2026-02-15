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
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::commands::complete as complete_cmd;
use crate::commands::run as run_cmd;
use crate::db::LockGuard;
use crate::loop_engine::branch;
use crate::loop_engine::calibrate;
use crate::loop_engine::claude;
use crate::loop_engine::config::{self, IterationOutcome, LoopConfig};
use crate::loop_engine::crash::CrashTracker;
use crate::loop_engine::deadline;
use crate::loop_engine::detection;
use crate::loop_engine::display;
use crate::loop_engine::env;
use crate::loop_engine::feedback;
use crate::loop_engine::monitor;
use crate::loop_engine::oauth;
use crate::loop_engine::progress;
use crate::loop_engine::prompt::{self, BuildPromptParams};
use crate::loop_engine::signals::{self, SessionGuidance, SignalFlag};
use crate::loop_engine::stale::StaleTracker;
use crate::loop_engine::usage::{self, UsageCheckResult};
use crate::models::RunStatus;
use crate::TaskMgrResult;

/// Maximum consecutive reorder attempts before forcing algorithmic pick.
const MAX_CONSECUTIVE_REORDERS: u32 = 2;

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
        }
    }
}

/// Run a single iteration of the agent loop.
///
/// Returns `IterationResult` describing the outcome and whether to stop.
///
/// # Arguments
///
/// * `ctx` - Mutable iteration context carrying state between iterations
/// * `conn` - Database connection
/// * `db_dir` - Database directory (--dir flag, for task selection queries)
/// * `project_root` - Git repository root (for source scanning, monitoring)
/// * `tasks_dir` - Tasks directory (for signal files)
/// * `iteration` - Current iteration number (1-based)
/// * `max_iterations` - Maximum number of iterations
/// * `run_id` - Current run ID
/// * `base_prompt_path` - Path to base prompt.md file
/// * `steering_path` - Optional path to steering.md
/// * `inter_iteration_delay` - Delay between iterations
/// * `signal_flag` - Shared signal flag for SIGINT/SIGTERM
/// * `elapsed_secs` - Total elapsed seconds since loop start
/// * `verbose` - Enable verbose output
/// * `usage_params` - Usage API monitoring parameters
// TODO: Refactor run_iteration parameters into an IterationParams struct
#[allow(clippy::too_many_arguments)]
pub fn run_iteration(
    ctx: &mut IterationContext,
    conn: &Connection,
    db_dir: &Path,
    project_root: &Path,
    tasks_dir: &Path,
    iteration: u32,
    max_iterations: u32,
    run_id: &str,
    base_prompt_path: &Path,
    steering_path: Option<&Path>,
    inter_iteration_delay: Duration,
    signal_flag: &SignalFlag,
    elapsed_secs: u64,
    verbose: bool,
    usage_params: &UsageParams,
) -> TaskMgrResult<IterationResult> {
    // Step 0: Check for SIGINT/SIGTERM
    if signal_flag.is_signaled() {
        eprintln!("Signal received, stopping loop...");
        return Ok(IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: None,
            files_modified: vec![],
            should_stop: true,
            output: String::new(),
        });
    }

    // Step 1: Check file-based signals
    if signals::check_stop_signal(tasks_dir) {
        eprintln!("Stop signal detected (.stop file found)");
        return Ok(IterationResult {
            outcome: IterationOutcome::Empty,
            task_id: None,
            files_modified: vec![],
            should_stop: true,
            output: String::new(),
        });
    }

    if signals::check_pause_signal(tasks_dir) {
        signals::handle_pause(tasks_dir, iteration, &mut ctx.session_guidance);
    }

    // Step 1.5: Pre-iteration usage check
    if usage_params.enabled {
        let check_result = usage::check_and_wait(
            usage_params.threshold,
            tasks_dir,
            usage_params.fallback_wait,
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
        dir: db_dir,
        project_root,
        conn,
        after_files: &ctx.last_files,
        run_id: Some(run_id),
        iteration,
        reorder_hint: effective_reorder_hint.as_deref(),
        session_guidance: &session_guidance_text,
        base_prompt_path,
        steering_path,
        verbose,
    };

    let prompt_result = match prompt::build_prompt(&prompt_params)? {
        Some(result) => result,
        None => {
            // No eligible task found — check if truly all done or just temporarily unavailable
            let remaining: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM tasks WHERE status NOT IN ('done', 'irrelevant')",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            if remaining == 0 {
                eprintln!("All tasks complete!");
                return Ok(IterationResult {
                    outcome: IterationOutcome::Completed,
                    task_id: None,
                    files_modified: vec![],
                    should_stop: true,
                    output: String::new(),
                });
            } else {
                eprintln!(
                    "No eligible tasks right now ({} remaining in todo/in-progress/blocked). Treating as stale.",
                    remaining
                );
                return Ok(IterationResult {
                    outcome: IterationOutcome::Stale,
                    task_id: None,
                    files_modified: vec![],
                    should_stop: false,
                    output: String::new(),
                });
            }
        }
    };

    let task_id = prompt_result.task_id.clone();
    let task_files = prompt_result.task_files.clone();
    let shown_learning_ids = prompt_result.shown_learning_ids.clone();

    // Step 5: Print iteration header
    display::print_iteration_header(iteration, max_iterations, &task_id, elapsed_secs);

    // Step 6: Start activity monitor, spawn Claude subprocess, stop monitor
    let monitor_handle = monitor::start_monitor(project_root);
    let claude_result = claude::spawn_claude(&prompt_result.prompt, Some(signal_flag), Some(project_root));
    monitor::stop_monitor(monitor_handle);
    let claude_result = claude_result?;

    // Step 7: Analyze output
    let claude_output = claude_result.output;
    let outcome =
        detection::analyze_output(&claude_output, claude_result.exit_code, project_root);

    // Step 7.5: On rate-limit detection, trigger usage wait and mark as non-counting
    if outcome == IterationOutcome::RateLimit && usage_params.enabled {
        eprintln!("Rate limit detected in output, checking usage API...");
        let check_result = usage::check_and_wait(
            usage_params.threshold,
            tasks_dir,
            usage_params.fallback_wait,
        );
        if check_result == UsageCheckResult::StopSignaled {
            return Ok(IterationResult {
                outcome: IterationOutcome::RateLimit,
                task_id: Some(task_id),
                files_modified: task_files,
                should_stop: true,
                output: String::new(),
            });
        }
    }

    // Step 7.7: Extract learnings from output (best-effort, opt-out via env var)
    if !crate::learnings::ingestion::is_extraction_disabled() && !claude_output.is_empty() {
        match crate::learnings::ingestion::extract_learnings_from_output(
            conn,
            &claude_output,
            Some(&task_id),
            Some(run_id),
        ) {
            Ok(r) if r.learnings_extracted > 0 => {
                eprintln!("Extracted {} learning(s) from output", r.learnings_extracted);
            }
            Ok(_) => {}
            Err(e) => eprintln!("Warning: learning extraction failed: {}", e),
        }
    }

    // Step 8: Record learning feedback
    if let Err(e) = feedback::record_iteration_feedback(conn, &shown_learning_ids, &outcome) {
        eprintln!("Warning: failed to record iteration feedback: {}", e);
    }

    // Step 9: Update trackers based on outcome
    let should_stop = update_trackers(ctx, &outcome);

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

    // Step 12: Inter-iteration delay (skip if stopping)
    if !should_stop && !inter_iteration_delay.is_zero() {
        thread::sleep(inter_iteration_delay);
    }

    Ok(IterationResult {
        outcome,
        task_id: Some(task_id),
        files_modified: task_files,
        should_stop,
        output: claude_output,
    })
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
pub async fn run_loop(run_config: LoopRunConfig) -> i32 {
    // Step 1: Load environment
    env::load_env();

    // Step 2: Validate git repo (source_root is the original repo)
    if let Err(e) = env::validate_git_repo(&run_config.source_root) {
        eprintln!("Error: {}", e);
        eprintln!("Hint: Run task-mgr from within a git repository.");
        return 1;
    }

    // Step 3: Resolve paths (PRD, prompt, progress live in source_root)
    let paths = match env::resolve_paths(
        &run_config.prd_file,
        run_config.prompt_file.as_deref(),
        &run_config.source_root,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error resolving paths: {}", e);
            eprintln!(
                "Hint: Check that the PRD file path is correct relative to your project root."
            );
            return 1;
        }
    };

    // Step 4: Ensure directories exist (in source_root)
    if let Err(e) = env::ensure_directories(&run_config.source_root) {
        eprintln!("Error creating directories: {}", e);
        return 1;
    }

    // Step 4.5: Acquire exclusive loop lock — prevents concurrent loops on same DB.
    // Must be before any DB mutations (init, migrations, recovery).
    // Separate from tasks.db.lock (short-lived per-command) so read-only commands
    // like `status` and `stats` are not blocked.
    let _loop_lock = match LockGuard::acquire_named(&run_config.db_dir, "loop.lock") {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!(
                "Error: another loop is already running on this database. {}",
                e
            );
            return 1;
        }
    };

    // Step 4.6: Detect branch change (archive previous PRD if branch switched)
    match branch::detect_branch_change(
        &run_config.source_root,
        &paths.tasks_dir,
        run_config.config.yes_mode,
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
    // Uses Auto prefix mode: reads taskPrefix from JSON, or auto-generates one
    if let Err(e) = crate::commands::init(
        &run_config.db_dir,
        &[&run_config.prd_file],
        false, // force
        true,  // append
        true,  // update_existing
        false, // dry_run
        crate::commands::init::PrefixMode::Auto,
    ) {
        eprintln!("Error initializing PRD: {}", e);
        return 1;
    }

    // Step 5.5: Compute initial PRD hash for change detection during iterations
    let mut prd_hash = hash_file(&run_config.prd_file);

    // Step 6: Open DB connection (after init to ensure schema exists)
    let mut conn = match crate::db::open_connection(&run_config.db_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error opening database: {}", e);
            return 1;
        }
    };

    // Step 6.5: Run any pending migrations (e.g. v4 adds external_git_repo column)
    if let Err(e) = crate::db::run_migrations(&mut conn) {
        eprintln!("Warning: failed to run migrations: {} (continuing)", e);
    }

    // Step 6.6: Recover stale in_progress tasks from previous crashed/killed runs.
    // Safe because we hold the exclusive loop lock — no other loop can be running.
    match conn.execute(
        "UPDATE tasks SET status = 'todo', started_at = NULL WHERE status = 'in_progress'",
        [],
    ) {
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
            return 1;
        }
    }

    // Step 7: Read PRD metadata for branch name, task count, and external repo
    let prd_metadata = match read_prd_metadata(&conn) {
        Ok(meta) => meta,
        Err(e) => {
            eprintln!("Error reading PRD metadata: {}", e);
            return 1;
        }
    };
    let branch_name = prd_metadata.branch_name;
    let task_count = prd_metadata.task_count;
    let task_prefix = prd_metadata.task_prefix;

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
    let working_root = if let Some(ref branch) = branch_name {
        if run_config.config.use_worktrees {
            // Create or reuse worktree for this branch
            match env::ensure_worktree(
                &run_config.source_root,
                branch,
                run_config.config.yes_mode,
            ) {
                Ok(wt_path) => wt_path,
                Err(e) => {
                    eprintln!("Error setting up worktree: {}", e);
                    return 1;
                }
            }
        } else {
            // Old behavior: checkout branch in source_root
            if let Err(e) =
                env::ensure_branch(&run_config.source_root, branch, run_config.config.yes_mode)
            {
                eprintln!("Error: {}", e);
                return 1;
            }
            run_config.source_root.clone()
        }
    } else {
        // No branch specified, use source_root as working directory
        run_config.source_root.clone()
    };

    // Step 9: Check uncommitted changes (in working_root)
    if let Err(e) = env::check_uncommitted_changes(&working_root, run_config.config.yes_mode) {
        eprintln!("Error: {}", e);
        return 1;
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

    if let Some(hours) = run_config.config.hours {
        if let Err(e) = deadline::create_deadline(&paths.tasks_dir, &prd_basename, hours) {
            eprintln!("Error creating deadline: {}", e);
            return 1;
        }
    }

    // Step 12: Begin run session
    let begin_result = match run_cmd::begin(&conn) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error beginning run: {}", e);
            deadline::cleanup_deadline(&paths.tasks_dir, &prd_basename);
            return 1;
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
        );
        if count > 0 {
            eprintln!(
                "Startup reconciliation: marked {} task(s) done from external repo",
                count
            );
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

    // Step 15: Print session banner
    let branch_display = branch_name.as_deref().unwrap_or("(unknown)");
    display::print_session_banner(
        &prd_basename,
        branch_display,
        max_iterations,
        run_config.config.hours,
    );

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

    for iteration in 1..=max_iterations {
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

        // Re-import PRD if Claude modified it during the previous iteration
        let current_hash = hash_file(&run_config.prd_file);
        if current_hash != prd_hash {
            eprintln!("PRD file changed, re-importing tasks...");
            if let Err(e) = crate::commands::init(
                &run_config.db_dir,
                &[&run_config.prd_file],
                false, // force
                true,  // append
                true,  // update_existing
                false, // dry_run
                crate::commands::init::PrefixMode::Auto,
            ) {
                eprintln!("Warning: PRD re-import failed: {} (continuing)", e);
            }
            prd_hash = current_hash;
        }

        let elapsed = start_time.elapsed().as_secs();

        let result = match run_iteration(
            &mut ctx,
            &conn,
            &run_config.db_dir,
            &working_root,
            &paths.tasks_dir,
            iteration,
            max_iterations,
            &run_id,
            &paths.prompt_file,
            steering,
            inter_iteration_delay,
            &signal_flag,
            elapsed,
            run_config.config.verbose,
            &usage_params,
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Iteration error: {}", e);
                exit_code = 1;
                exit_reason = format!("iteration error: {}", e);
                break;
            }
        };

        // Log progress
        progress::log_iteration(
            &paths.progress_file,
            iteration,
            result.task_id.as_deref(),
            &result.outcome,
            &result.files_modified,
        );

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

        // Check git for task completion: if recent commit contains task ID, mark done
        if let Some(ref task_id) = result.task_id {
            if !matches!(
                result.outcome,
                IterationOutcome::Crash(_) | IterationOutcome::Empty | IterationOutcome::RateLimit
            ) {
                if let Some(commit_hash) = check_git_for_task_completion(&working_root, task_id, task_prefix.as_deref()) {
                    // Mark task done in DB
                    let task_ids = [task_id.clone()];
                    if let Err(e) = complete_cmd::complete(
                        &mut conn,
                        &task_ids,
                        Some(&run_id),
                        Some(&commit_hash),
                        false, // force
                    ) {
                        eprintln!("Warning: failed to mark task {} as done in DB: {}", task_id, e);
                    }
                    last_claimed_task = None;

                    tasks_completed += 1;

                    // Update PRD JSON to set passes: true
                    if let Err(e) = update_prd_task_passes(&paths.prd_file, task_id, true, task_prefix.as_deref()) {
                        eprintln!("Warning: failed to update PRD for task {}: {}", task_id, e);
                    } else {
                        eprintln!(
                            "Task {} completed (commit {})",
                            task_id,
                            &commit_hash[..7.min(commit_hash.len())]
                        );
                    }
                } else {
                    // Fallback: scan Claude's output for ANY completed task IDs.
                    // Claude may complete the claimed task or others in a single iteration,
                    // and commits happen in a different repo (e.g. restaurant_agent_ex/).
                    let completed_ids =
                        scan_output_for_completed_tasks(&result.output, &conn, task_prefix.as_deref());
                    for completed_id in &completed_ids {
                        let ids = [completed_id.clone()];
                        if let Err(e) = complete_cmd::complete(
                            &mut conn,
                            &ids,
                            Some(&run_id),
                            None, // no commit hash — different repo
                            false,
                        ) {
                            eprintln!(
                                "Warning: failed to mark task {} as done: {}",
                                completed_id, e
                            );
                        }

                        // Clear tracker if the claimed task was completed via output scan
                        if result.task_id.as_deref() == Some(completed_id.as_str()) {
                            last_claimed_task = None;
                        }

                        tasks_completed += 1;

                        if let Err(e) =
                            update_prd_task_passes(&paths.prd_file, completed_id, true, task_prefix.as_deref())
                        {
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
                }
            }
        }

        // Post-iteration: reconcile external git completions
        // Catches tasks completed in the current iteration (and any missed from prior)
        if let Some(ref ext_repo) = external_repo_path {
            if !matches!(
                result.outcome,
                IterationOutcome::Crash(_) | IterationOutcome::Empty | IterationOutcome::RateLimit
            ) {
                let count = reconcile_external_git_completions(
                    ext_repo,
                    &mut conn,
                    &run_id,
                    &paths.prd_file,
                    task_prefix.as_deref(),
                );
                if count > 0 {
                    tasks_completed += count as u32;
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
        }

        // Track iteration count (skip reorders and rate limits)
        match result.outcome {
            IterationOutcome::Reorder(_) | IterationOutcome::RateLimit => {
                // Don't count against iteration budget
            }
            IterationOutcome::Completed => {
                iterations_completed += 1;
            }
            _ => {
                iterations_completed += 1;
            }
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
        on_run_completed(&conn);
    }

    // Step 21: Cleanup
    deadline::cleanup_deadline(&paths.tasks_dir, &prd_basename);
    signals::cleanup_signal_files(&paths.tasks_dir);

    // Step 22: Print final banner
    let total_elapsed = start_time.elapsed().as_secs();
    display::print_final_banner(
        iterations_completed,
        tasks_completed,
        total_elapsed,
        &exit_reason,
    );

    exit_code
}

/// PRD metadata read from the database.
struct PrdMetadata {
    branch_name: Option<String>,
    task_count: usize,
    external_git_repo: Option<String>,
    task_prefix: Option<String>,
}

/// Read branch name, task count, external_git_repo, and task_prefix from prd_metadata and tasks tables.
fn read_prd_metadata(conn: &Connection) -> TaskMgrResult<PrdMetadata> {
    let (branch_name, external_git_repo, task_prefix): (
        Option<String>,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT branch_name, external_git_repo, task_prefix FROM prd_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap_or((None, None, None));

    let task_count: usize = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get::<_, i64>(0))
        .map(|c| c as usize)
        .unwrap_or(0);

    Ok(PrdMetadata {
        branch_name,
        task_count,
        external_git_repo,
        task_prefix,
    })
}

/// Install SIGINT and SIGTERM handlers that set the signal flag.
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
        use signal_hook::consts::{SIGINT, SIGTERM};

        // First signal sets the flag; second signal restores default (immediate kill)
        if let Err(e) = signal_hook::flag::register_conditional_default(SIGINT, flag.clone()) {
            eprintln!("Warning: failed to install SIGINT handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register(SIGTERM, flag) {
            eprintln!("Warning: failed to install SIGTERM handler: {}", e);
        }
    }

    #[cfg(not(unix))]
    {
        // signal-hook supports SIGINT on all platforms (including Windows via SetConsoleCtrlHandler)
        use signal_hook::consts::SIGINT;
        if let Err(e) = signal_hook::flag::register_conditional_default(SIGINT, flag) {
            eprintln!("Warning: failed to install SIGINT handler: {}", e);
        }
    }
}

/// Called after a run ends with Completed status to recalibrate selection weights.
///
/// Analyzes historical task outcomes and adjusts the scoring weights used by
/// `select_next_task()`. Errors are logged but do not propagate (best-effort).
pub fn on_run_completed(conn: &Connection) {
    match calibrate::recalibrate_weights(conn) {
        Ok(weights) => {
            let defaults = calibrate::SelectionWeights::default();
            if weights != defaults {
                eprintln!(
                    "Calibrated selection weights: file_overlap={}, synergy={}, conflict={}, priority_base={}",
                    weights.file_overlap, weights.synergy, weights.conflict, weights.priority_base
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

/// Update a task's `passes` field in the PRD JSON file.
///
/// Reads the PRD, finds the task by ID, updates `passes`, and writes back atomically.
/// Also tries the base ID (prefix stripped) since PRD JSON has unprefixed IDs while DB has prefixed IDs.
fn update_prd_task_passes(
    prd_path: &Path,
    task_id: &str,
    passes: bool,
    task_prefix: Option<&str>,
) -> crate::TaskMgrResult<()> {
    use std::fs;

    // Read the PRD file
    let content = fs::read_to_string(prd_path).map_err(|e| {
        crate::TaskMgrError::IoErrorWithContext {
            file_path: prd_path.display().to_string(),
            operation: "reading PRD file".to_string(),
            source: e,
        }
    })?;

    // Parse as generic JSON Value to preserve structure
    let mut prd: serde_json::Value = serde_json::from_str(&content)?;

    // Try full ID first, then base ID (prefix-stripped) since PRD JSON stores unprefixed IDs
    let base_id = strip_task_prefix(task_id, task_prefix);

    // Find and update the task in userStories
    let updated = if let Some(stories) = prd.get_mut("userStories").and_then(|v| v.as_array_mut()) {
        let mut found = false;
        for story in stories.iter_mut() {
            let story_id = story.get("id").and_then(|v| v.as_str());
            if story_id == Some(task_id) || story_id == Some(base_id) {
                story["passes"] = serde_json::Value::Bool(passes);
                found = true;
                break;
            }
        }
        found
    } else {
        false
    };

    if !updated {
        return Err(crate::TaskMgrError::NotFound {
            resource_type: "Task in PRD".to_string(),
            id: task_id.to_string(),
        });
    }

    // Write back atomically
    let tmp_path = prd_path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(&prd)?;
    fs::write(&tmp_path, &json).map_err(|e| crate::TaskMgrError::IoErrorWithContext {
        file_path: tmp_path.display().to_string(),
        operation: "writing temp PRD file".to_string(),
        source: e,
    })?;
    fs::rename(&tmp_path, prd_path).map_err(|e| crate::TaskMgrError::IoErrorWithContext {
        file_path: prd_path.display().to_string(),
        operation: "renaming temp PRD file".to_string(),
        source: e,
    })?;

    Ok(())
}

/// Strip the auto-generated task prefix from a DB task ID to recover the base ID.
///
/// e.g., `strip_task_prefix("aeb10a1f-FIX-001", Some("aeb10a1f"))` → `"FIX-001"`
///       `strip_task_prefix("P5.1-FIX-001", Some("P5.1"))` → `"FIX-001"`
///       `strip_task_prefix("FIX-001", None)` → `"FIX-001"`
fn strip_task_prefix<'a>(task_id: &'a str, prefix: Option<&str>) -> &'a str {
    match prefix {
        Some(pfx) => {
            let with_dash = format!("{}-", pfx);
            task_id.strip_prefix(&with_dash).unwrap_or(task_id)
        }
        None => task_id,
    }
}

/// Check Claude's output for evidence the task was completed (commit message containing task ID).
///
/// Fallback for when Claude commits in a different repo than the working directory.
/// Looks for the task ID in brackets, e.g. `[FEAT-005]` in a commit message.
/// Also tries the base ID (prefix stripped) as a fallback.
fn check_output_for_task_completion(
    output: &str,
    task_id: &str,
    task_prefix: Option<&str>,
) -> bool {
    let pattern = format!("[{}]", task_id);
    if output.contains(&pattern) {
        return true;
    }
    // Fallback: try base ID without prefix
    let base_id = strip_task_prefix(task_id, task_prefix);
    if base_id != task_id {
        let base_pattern = format!("[{}]", base_id);
        output.contains(&base_pattern)
    } else {
        false
    }
}

/// Scan Claude's output for any completed task IDs from the database.
///
/// Returns a list of task IDs found in the output (in bracket format like `[FEAT-005]`).
/// This catches cases where Claude completes tasks other than the one that was claimed,
/// or completes multiple tasks in a single iteration.
fn scan_output_for_completed_tasks(
    output: &str,
    conn: &Connection,
    task_prefix: Option<&str>,
) -> Vec<String> {
    let mut completed = Vec::new();

    // Query all non-done task IDs
    let mut stmt = match conn.prepare(
        "SELECT id FROM tasks WHERE status NOT IN ('done', 'irrelevant')",
    ) {
        Ok(s) => s,
        Err(_) => return completed,
    };

    let task_ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    for task_id in task_ids {
        if check_output_for_task_completion(output, &task_id, task_prefix) {
            completed.push(task_id);
        }
    }

    completed
}

/// Scan recent commits in an external git repo for task completion evidence.
///
/// Queries all incomplete task IDs from the DB, then checks recent git commits
/// in the external repo for any that contain a task ID (case-insensitive).
/// Matches are marked as done and the PRD JSON is updated.
///
/// Returns the number of tasks reconciled.
fn reconcile_external_git_completions(
    external_repo: &Path,
    conn: &mut Connection,
    run_id: &str,
    prd_path: &Path,
    task_prefix: Option<&str>,
) -> usize {
    use std::process::Command;

    // Validate the external repo exists
    if !external_repo.exists() {
        eprintln!(
            "Warning: external git repo not found at {}, skipping reconciliation",
            external_repo.display()
        );
        return 0;
    }

    // Get recent commits from external repo (50 should cover recent work)
    let output = match Command::new("git")
        .args(["log", "--oneline", "-50"])
        .current_dir(external_repo)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        Ok(o) => {
            eprintln!(
                "Warning: git log failed in {}: {}",
                external_repo.display(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return 0;
        }
        Err(e) => {
            eprintln!(
                "Warning: could not run git in {}: {}",
                external_repo.display(),
                e
            );
            return 0;
        }
    };

    if output.is_empty() {
        return 0;
    }

    let commit_lines_upper = output.to_uppercase();

    // Query all incomplete task IDs
    let mut stmt = match conn.prepare(
        "SELECT id FROM tasks WHERE status NOT IN ('done', 'irrelevant')",
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Warning: could not query tasks for reconciliation: {}", e);
            return 0;
        }
    };

    let task_ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .ok()
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    drop(stmt);

    let mut reconciled = 0;

    for task_id in &task_ids {
        let task_id_upper = task_id.to_uppercase();
        let base_id_upper = strip_task_prefix(task_id, task_prefix).to_uppercase();
        if commit_lines_upper.contains(&task_id_upper)
            || (task_id_upper != base_id_upper && commit_lines_upper.contains(&base_id_upper))
        {
            // Mark as done
            let ids = [task_id.clone()];
            if let Err(e) = complete_cmd::complete(
                conn,
                &ids,
                Some(run_id),
                None,  // no specific commit hash from oneline
                true,  // force: allow any status → done
            ) {
                // Likely already done or invalid transition — skip silently
                if run_id.is_empty() {
                    eprintln!("Warning: reconciliation failed for {}: {}", task_id, e);
                }
                continue;
            }

            // Update PRD JSON
            if let Err(e) = update_prd_task_passes(prd_path, task_id, true, task_prefix) {
                eprintln!(
                    "Warning: failed to update PRD for reconciled task {}: {}",
                    task_id, e
                );
            }

            eprintln!("Reconciled task {} (found in external repo commits)", task_id);
            reconciled += 1;
        }
    }

    reconciled
}

/// Check if the most recent git commit contains the task ID.
///
/// Returns the commit hash if found, None otherwise.
/// Looks for patterns like `[TASK-001]`, `feat: TASK-001`, `TASK-001:`, etc.
/// Also tries the base ID (prefix stripped) as a fallback.
fn check_git_for_task_completion(
    project_root: &Path,
    task_id: &str,
    task_prefix: Option<&str>,
) -> Option<String> {
    use std::process::Command;

    // Get the most recent commit (hash and message)
    let output = Command::new("git")
        .args(["log", "-1", "--format=%H %s"])
        .current_dir(project_root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let line = line.trim();

    if line.is_empty() {
        return None;
    }

    // Split into hash and message
    let (hash, message) = line.split_once(' ')?;

    // Check if message contains the task ID (case-insensitive)
    let message_upper = message.to_uppercase();
    let task_id_upper = task_id.to_uppercase();
    let base_id_upper = strip_task_prefix(task_id, task_prefix).to_uppercase();

    if message_upper.contains(&task_id_upper)
        || (task_id_upper != base_id_upper && message_upper.contains(&base_id_upper))
    {
        Some(hash.to_string())
    } else {
        None
    }
}

/// Compute an MD5 hash of a file's contents.
///
/// Returns the hex-encoded hash, or an empty string if the file cannot be read.
/// An empty string means "unknown" — the next call will re-hash and detect any change.
fn hash_file(path: &Path) -> String {
    std::fs::read(path)
        .map(|bytes| format!("{:x}", md5::compute(&bytes)))
        .unwrap_or_default()
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
        IterationOutcome::Stale => {
            // Stale detection handled by the outer loop via stale_tracker.check()
            false
        }
        IterationOutcome::Empty => {
            ctx.crash_tracker.record_crash();
            ctx.crash_tracker.should_abort()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- IterationContext tests ---

    #[test]
    fn test_iteration_context_new() {
        let ctx = IterationContext::new(5);
        assert!(ctx.last_commit.is_none());
        assert!(ctx.last_files.is_empty());
        assert!(ctx.session_guidance.is_empty());
        assert!(ctx.reorder_hint.is_none());
        assert_eq!(ctx.reorder_count, 0);
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

    // --- check_output_for_task_completion tests ---

    #[test]
    fn test_check_output_finds_task_id_in_brackets() {
        let output = "Some output\nfeat: [FEAT-005] Implement Tool Declarations module\nMore output";
        assert!(check_output_for_task_completion(output, "FEAT-005", None));
    }

    #[test]
    fn test_check_output_returns_false_when_not_found() {
        let output = "Some output without any task references";
        assert!(!check_output_for_task_completion(output, "FEAT-005", None));
    }

    #[test]
    fn test_check_output_requires_brackets() {
        // Task ID without brackets should NOT match
        let output = "feat: FEAT-005 Implement something";
        assert!(!check_output_for_task_completion(output, "FEAT-005", None));
    }

    #[test]
    fn test_check_output_empty_output() {
        assert!(!check_output_for_task_completion("", "FEAT-005", None));
    }

    // --- scan_output_for_completed_tasks tests ---

    #[test]
    fn test_scan_output_finds_multiple_task_ids() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        // Insert some tasks
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'todo', 1),
             ('FEAT-002', 'Task 2', 'in_progress', 2),
             ('FEAT-003', 'Task 3', 'done', 3),
             ('FEAT-004', 'Task 4', 'todo', 4);",
        )
        .unwrap();

        let output = "feat: [FEAT-001] First task\nfeat: [FEAT-002] Second task\nfeat: [FEAT-003] Already done";
        let completed = scan_output_for_completed_tasks(output, &conn, None);

        // Should find FEAT-001 and FEAT-002 (not done), skip FEAT-003 (already done), miss FEAT-004 (not in output)
        assert_eq!(completed.len(), 2);
        assert!(completed.contains(&"FEAT-001".to_string()));
        assert!(completed.contains(&"FEAT-002".to_string()));
    }

    #[test]
    fn test_scan_output_returns_empty_when_no_matches() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES ('FEAT-001', 'Task 1', 'todo', 1);",
        )
        .unwrap();

        let output = "No task IDs in brackets here";
        let completed = scan_output_for_completed_tasks(output, &conn, None);
        assert!(completed.is_empty());
    }

    // --- check_git_for_task_completion tests ---

    fn git_commit(dir: &std::path::Path, msg: &str) {
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", msg])
            .current_dir(dir)
            .output()
            .expect("create commit");
    }

    #[test]
    fn test_check_git_completion_finds_task_id_in_commit() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: [SEC-H005] Add feature");

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", None);
        assert!(result.is_some(), "Should find task ID in commit message");
    }

    #[test]
    fn test_check_git_completion_case_insensitive() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: SEC-h005 lowercase");

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", None);
        assert!(result.is_some(), "Should find task ID case-insensitively");
    }

    #[test]
    fn test_check_git_completion_returns_none_when_not_found() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: unrelated commit");

        let result = check_git_for_task_completion(temp_dir.path(), "SEC-H005", None);
        assert!(result.is_none(), "Should return None when task ID not in commit");
    }

    #[test]
    fn test_check_git_completion_returns_commit_hash() {
        let temp_dir = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(temp_dir.path(), "feat: TASK-001 test");

        let result = check_git_for_task_completion(temp_dir.path(), "TASK-001", None);
        assert!(result.is_some());
        let hash = result.unwrap();
        assert_eq!(hash.len(), 40, "Should return full commit hash");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "Hash should be hex");
    }

    // --- update_prd_task_passes tests ---

    #[test]
    fn test_update_prd_task_passes_sets_true() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");

        let prd = r#"{
            "project": "Test",
            "userStories": [
                {"id": "TASK-001", "title": "Test", "passes": false},
                {"id": "TASK-002", "title": "Other", "passes": false}
            ]
        }"#;
        std::fs::write(&prd_path, prd).unwrap();

        update_prd_task_passes(&prd_path, "TASK-001", true, None).unwrap();

        let content = std::fs::read_to_string(&prd_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let task = &parsed["userStories"][0];
        assert_eq!(task["passes"], true);
        // Other task unchanged
        assert_eq!(parsed["userStories"][1]["passes"], false);
    }

    #[test]
    fn test_update_prd_task_passes_not_found() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");

        let prd = r#"{"project": "Test", "userStories": []}"#;
        std::fs::write(&prd_path, prd).unwrap();

        let result = update_prd_task_passes(&prd_path, "NONEXISTENT", true, None);
        assert!(result.is_err());
    }

    // --- MAX_CONSECUTIVE_REORDERS constant ---

    #[test]
    fn test_max_consecutive_reorders_is_2() {
        assert_eq!(MAX_CONSECUTIVE_REORDERS, 2);
    }

    // --- on_run_completed tests ---

    #[test]
    fn test_on_run_completed_no_panic_on_empty_db() {
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();

        // Should not panic even with no data
        on_run_completed(&conn);
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

    // --- reconcile_external_git_completions tests ---

    #[test]
    fn test_reconcile_nonexistent_repo_returns_zero() {
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();
        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[]}"#,
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            Path::new("/nonexistent/repo"),
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );
        assert_eq!(count, 0);
    }

    #[test]
    fn test_reconcile_finds_completed_tasks_in_external_repo() {
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        // Insert tasks
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'todo', 1),
             ('FEAT-002', 'Task 2', 'in_progress', 2),
             ('FEAT-003', 'Task 3', 'done', 3);",
        )
        .unwrap();

        // Create external git repo with commits containing task IDs
        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "feat: FEAT-001 Implement feature");
        git_commit(ext_repo.path(), "feat: FEAT-003 Already done task");

        // Create PRD file
        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Task 1","passes":false,"priority":1},
                {"id":"FEAT-002","title":"Task 2","passes":false,"priority":2},
                {"id":"FEAT-003","title":"Task 3","passes":true,"priority":3}
            ]}"#,
        )
        .unwrap();

        // Insert a run so complete_cmd works
        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        // Should find FEAT-001 (todo → done), skip FEAT-003 (already done)
        // FEAT-002 is in_progress but not in commits
        assert_eq!(count, 1);

        // Verify FEAT-001 is now done
        let status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'FEAT-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "done");

        // Verify PRD was updated
        let prd_content = std::fs::read_to_string(&prd_path).unwrap();
        let prd: serde_json::Value = serde_json::from_str(&prd_content).unwrap();
        assert_eq!(prd["userStories"][0]["passes"], true);
    }

    #[test]
    fn test_reconcile_case_insensitive() {
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('SEC-H005', 'Security task', 'todo', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "feat: sec-h005 lowercase commit");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"SEC-H005","title":"Security task","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert_eq!(count, 1, "Should match case-insensitively");
    }

    #[test]
    fn test_reconcile_empty_repo_returns_zero() {
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'todo', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        // No additional commits beyond the initial one from setup_git_repo

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Task 1","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert_eq!(count, 0, "No matching commits should mean no reconciliation");
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
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'T-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
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
        let status1: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'T-001'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let status2: String = conn
            .query_row("SELECT status FROM tasks WHERE id = 'T-002'", [], |row| {
                row.get(0)
            })
            .unwrap();
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

    // --- hash_file tests ---

    #[test]
    fn test_hash_file_returns_consistent_hash() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("test.json");
        std::fs::write(&path, r#"{"tasks": [1, 2, 3]}"#).unwrap();

        let hash1 = hash_file(&path);
        let hash2 = hash_file(&path);
        assert_eq!(hash1, hash2, "Same content should produce same hash");
        assert!(!hash1.is_empty(), "Hash should not be empty for readable file");
    }

    #[test]
    fn test_hash_file_detects_change() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let path = temp_dir.path().join("test.json");

        std::fs::write(&path, r#"{"tasks": [1, 2]}"#).unwrap();
        let hash_before = hash_file(&path);

        std::fs::write(&path, r#"{"tasks": [1, 2, 3]}"#).unwrap();
        let hash_after = hash_file(&path);

        assert_ne!(hash_before, hash_after, "Different content should produce different hash");
    }

    #[test]
    fn test_hash_file_missing_file_returns_empty() {
        let hash = hash_file(Path::new("/nonexistent/file.json"));
        assert!(hash.is_empty(), "Missing file should return empty string");
    }

    // ======================================================================
    // Regression tests: tasks_completed counter accuracy
    //
    // Prior to the fix, tasks_completed only incremented when
    // IterationOutcome::Completed was returned (i.e. Claude output
    // <promise>COMPLETE</promise>). Tasks completed via git detection,
    // output scanning, or external repo reconciliation were never counted,
    // causing the final banner to always show "Tasks completed: 0".
    //
    // These tests verify that each completion path returns accurate counts
    // that the loop can use for the tasks_completed counter.
    // ======================================================================

    #[test]
    fn test_git_completion_returns_some_for_matching_commit() {
        // Regression: git-based detection returns Some(hash) which the loop
        // uses to increment tasks_completed by 1.
        let repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(repo.path(), "feat: P3-FEAT-001 Implement CallSupervisor");

        let result = check_git_for_task_completion(repo.path(), "P3-FEAT-001", None);
        assert!(
            result.is_some(),
            "Git detection should return Some for matching commit — loop increments counter by 1"
        );
    }

    #[test]
    fn test_output_scan_counts_multiple_completed_tasks() {
        // Regression: output scanning may find N>1 tasks completed in a single
        // iteration. The loop should increment tasks_completed by N, not 0.
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('P3-FEAT-001', 'Task 1', 'todo', 1),
             ('P3-FEAT-002', 'Task 2', 'todo', 2),
             ('P3-FEAT-003', 'Task 3', 'todo', 3);",
        )
        .unwrap();

        let output = "Completed [P3-FEAT-001] and [P3-FEAT-002] in same iteration\n\
                      Also finished [P3-FEAT-003] as a bonus";
        let completed = scan_output_for_completed_tasks(output, &conn, None);

        assert_eq!(
            completed.len(),
            3,
            "Output scan should find all 3 tasks — loop increments counter by 3"
        );
    }

    #[test]
    fn test_reconciliation_counts_multiple_tasks_accurately() {
        // Regression: reconciliation returns count of newly-completed tasks.
        // The loop should add this count (as u32) to tasks_completed.
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('P3-FEAT-001', 'Task 1', 'todo', 1),
             ('P3-FEAT-002', 'Task 2', 'todo', 2),
             ('P3-FEAT-003', 'Task 3', 'todo', 3);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "feat: P3-FEAT-001 Implement CallSupervisor");
        git_commit(ext_repo.path(), "feat: P3-FEAT-002 Implement CallActor");
        git_commit(ext_repo.path(), "feat: P3-FEAT-003 Implement BargeIn");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"P3-FEAT-001","title":"Task 1","passes":false,"priority":1},
                {"id":"P3-FEAT-002","title":"Task 2","passes":false,"priority":2},
                {"id":"P3-FEAT-003","title":"Task 3","passes":false,"priority":3}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert_eq!(
            count, 3,
            "Reconciliation should return 3 — loop adds this to tasks_completed"
        );
    }

    #[test]
    fn test_reconciliation_skips_already_done_tasks_no_double_count() {
        // Regression: if git detection already marked a task done earlier in the
        // same iteration, reconciliation should NOT re-count it. The query
        // filters `status NOT IN ('done', 'irrelevant')`.
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        // FEAT-001 already done (as if git detection marked it), FEAT-002 still todo
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'done', 1),
             ('FEAT-002', 'Task 2', 'todo', 2);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "feat: FEAT-001 Already done");
        git_commit(ext_repo.path(), "feat: FEAT-002 New completion");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FEAT-001","title":"Task 1","passes":true,"priority":1},
                {"id":"FEAT-002","title":"Task 2","passes":false,"priority":2}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            None,
        );

        assert_eq!(
            count, 1,
            "Should only count FEAT-002 (new) not FEAT-001 (already done) — no double counting"
        );
    }

    #[test]
    fn test_output_scan_skips_already_done_tasks() {
        // Regression: output scanning only finds non-done tasks, so completing
        // a task via git detection then running output scan won't double-count.
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('FEAT-001', 'Task 1', 'done', 1),
             ('FEAT-002', 'Task 2', 'todo', 2);",
        )
        .unwrap();

        let output = "Completed [FEAT-001] and [FEAT-002]";
        let completed = scan_output_for_completed_tasks(output, &conn, None);

        assert_eq!(completed.len(), 1, "Should only find FEAT-002, not already-done FEAT-001");
        assert_eq!(completed[0], "FEAT-002");
    }

    // ======================================================================
    // Task prefix stripping and prefix-aware completion detection tests
    // ======================================================================

    #[test]
    fn test_strip_task_prefix_with_uuid() {
        assert_eq!(
            strip_task_prefix("aeb10a1f-FIX-001", Some("aeb10a1f")),
            "FIX-001"
        );
    }

    #[test]
    fn test_strip_task_prefix_with_human_prefix() {
        assert_eq!(
            strip_task_prefix("P5.1-FIX-001", Some("P5.1")),
            "FIX-001"
        );
    }

    #[test]
    fn test_strip_task_prefix_no_prefix() {
        assert_eq!(strip_task_prefix("FIX-001", None), "FIX-001");
    }

    #[test]
    fn test_strip_task_prefix_mismatch() {
        // Prefix doesn't match — returns original
        assert_eq!(
            strip_task_prefix("OTHER-FIX-001", Some("aeb10a1f")),
            "OTHER-FIX-001"
        );
    }

    #[test]
    fn test_reconciliation_matches_base_id() {
        // DB has prefixed ID "aeb10a1f-FIX-001", external repo commit uses "FIX-001"
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('aeb10a1f-FIX-001', 'Fix bug', 'todo', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "fix: FIX-001 Fix the bug");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FIX-001","title":"Fix bug","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            Some("aeb10a1f"),
        );

        assert_eq!(count, 1, "Should match base ID FIX-001 in commit even though DB has aeb10a1f-FIX-001");

        // Verify task is done
        let status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE id = 'aeb10a1f-FIX-001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "done");

        // Verify PRD was updated (via base ID fallback)
        let prd_content = std::fs::read_to_string(&prd_path).unwrap();
        let prd: serde_json::Value = serde_json::from_str(&prd_content).unwrap();
        assert_eq!(prd["userStories"][0]["passes"], true);
    }

    #[test]
    fn test_reconciliation_matches_full_id() {
        // Commit uses full prefixed ID — should still match
        let (_temp_dir, mut conn) = crate::loop_engine::test_utils::setup_test_db();

        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('aeb10a1f-FIX-001', 'Fix bug', 'todo', 1);",
        )
        .unwrap();

        let ext_repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(ext_repo.path(), "fix: aeb10a1f-FIX-001 Fix the bug");

        let prd_dir = tempfile::TempDir::new().unwrap();
        let prd_path = prd_dir.path().join("prd.json");
        std::fs::write(
            &prd_path,
            r#"{"project":"test","userStories":[
                {"id":"FIX-001","title":"Fix bug","passes":false,"priority":1}
            ]}"#,
        )
        .unwrap();

        conn.execute(
            "INSERT INTO runs (run_id, status) VALUES ('run-1', 'active')",
            [],
        )
        .unwrap();

        let count = reconcile_external_git_completions(
            ext_repo.path(),
            &mut conn,
            "run-1",
            &prd_path,
            Some("aeb10a1f"),
        );

        assert_eq!(count, 1, "Should match full prefixed ID in commit");
    }

    #[test]
    fn test_output_scan_matches_base_id() {
        // DB has prefixed ID, output has unprefixed bracket tag
        use crate::loop_engine::test_utils::setup_test_db;

        let (_temp_dir, conn) = setup_test_db();
        conn.execute_batch(
            "INSERT INTO tasks (id, title, status, priority) VALUES
             ('aeb10a1f-FIX-001', 'Fix bug', 'todo', 1);",
        )
        .unwrap();

        let output = "Completed [FIX-001] successfully";
        let completed = scan_output_for_completed_tasks(output, &conn, Some("aeb10a1f"));

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0], "aeb10a1f-FIX-001");
    }

    #[test]
    fn test_git_completion_matches_base_id() {
        // DB has "uuid-FIX-001", commit has "FIX-001"
        let repo = crate::loop_engine::test_utils::setup_git_repo();
        git_commit(repo.path(), "feat: FIX-001 implement feature");

        let result = check_git_for_task_completion(
            repo.path(),
            "aeb10a1f-FIX-001",
            Some("aeb10a1f"),
        );
        assert!(result.is_some(), "Should match base ID FIX-001 in commit");
    }

    #[test]
    fn test_update_prd_passes_with_prefix() {
        // PRD has unprefixed "FIX-001", called with prefixed "aeb10a1f-FIX-001"
        let temp_dir = tempfile::TempDir::new().unwrap();
        let prd_path = temp_dir.path().join("prd.json");

        let prd = r#"{
            "project": "Test",
            "userStories": [
                {"id": "FIX-001", "title": "Fix bug", "passes": false}
            ]
        }"#;
        std::fs::write(&prd_path, prd).unwrap();

        update_prd_task_passes(&prd_path, "aeb10a1f-FIX-001", true, Some("aeb10a1f")).unwrap();

        let content = std::fs::read_to_string(&prd_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["userStories"][0]["passes"], true, "Should update via base ID fallback");
    }

    #[test]
    fn test_check_output_matches_base_id() {
        // Output has "[FIX-001]", DB ID is "aeb10a1f-FIX-001"
        let output = "feat: [FIX-001] Fix the bug";
        assert!(check_output_for_task_completion(
            output,
            "aeb10a1f-FIX-001",
            Some("aeb10a1f"),
        ));
    }
}
