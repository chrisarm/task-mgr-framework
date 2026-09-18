//! Linear startup phase of the autonomous loop (WS-3.2).
//!
//! Extracted verbatim from `orchestrator::run_loop` (Steps 1–16): environment
//! load, git/PRD validation, exclusive loop-lock acquisition, DB open +
//! migrations + stale recovery, PRD-metadata read, worktree / parallel-slot
//! setup, run-session begin, signal-handler install, and banner/usage setup.
//! [`initialize_loop`] returns a fully-populated [`LoopInitContext`] the
//! orchestrator threads into its iteration loop, or — on any startup failure —
//! the exact [`LoopResult`] the inline code returned at that point.
//!
//! This is a pure, behavior-neutral move: the relocated logic is byte-for-byte
//! identical to the prior inline startup phase (the only mechanical changes are
//! `return LoopResult { .. }` → `return Err(LoopResult { .. })` and owning the
//! resolved `steering.md` path so it can cross the function boundary).
//!
//! **Signal-handler ownership**: the `SignalFlag` is constructed and armed here
//! (via `setup_signal_handler`) and handed to the orchestrator on the context;
//! `run_loop` threads it through `WaveIterationParams` / `SlotIterationParams` /
//! `IterationContext` exactly as before. **Stale ephemeral reconcile (defense
//! layer #5)**: the `worktree::reconcile_stale_ephemeral_slots` call still runs
//! BEFORE `ensure_slot_worktrees` (CLAUDE.md Step 9.5).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use crate::commands::doctor::setup_checks::pre_check_loop_setup;
use crate::commands::doctor::setup_output::SetupSeverity;
use crate::commands::init::{PrdFile, PrefixMode, resolve_sticky_prefix};
use crate::commands::run as run_cmd;
use crate::db::LockGuard;
use crate::db::prefix::{prefix_and, validate_prefix};
use crate::db::schema::key_decisions as key_decisions_db;
use crate::error::TaskMgrError;
use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::branch;
use crate::loop_engine::config::{self, PermissionMode};
use crate::loop_engine::deadline;
use crate::loop_engine::display;
use crate::loop_engine::env;
use crate::loop_engine::git_reconcile::reconcile_external_git_completions;
use crate::loop_engine::model;
use crate::loop_engine::prd_reconcile::{hash_file, read_prd_metadata, reconcile_passes_with_db};
use crate::loop_engine::project_config::ProjectConfig;
use crate::loop_engine::signals::SignalFlag;
use crate::loop_engine::status_queries::read_prd_hints;
use crate::loop_engine::worktree;
use crate::output::ui;

use crate::loop_engine::engine::{
    AUTO_MODE_DEPRECATION_HINT, LoopResult, LoopRunConfig, UsageParams, claude_usage_check_enabled,
    read_prd_implicit_overlap_files,
};

/// Hand-off bundle produced by [`initialize_loop`] and consumed (destructured)
/// once at the top of `run_loop`.
///
/// Every field mirrors a same-named local the inline startup phase threaded
/// forward into the iteration loop and post-loop teardown; this struct only
/// carries them across the new function boundary and changes no logic.
pub(crate) struct LoopInitContext {
    /// Exclusive per-prefix loop lock. Held (unused) for the lifetime of the run
    /// so a concurrent loop on the same prefix cannot start; released on drop.
    pub(crate) loop_lock: LockGuard,
    /// Open DB connection: schema created, migrations applied, stale `in_progress`
    /// tasks recovered.
    pub(crate) conn: Connection,
    /// Resolved PRD / prompt / progress / tasks-dir paths. `prd_file` is already
    /// remapped to the worktree copy and `progress_file` to the per-prefix name.
    pub(crate) paths: env::ResolvedPaths,
    /// Hash of the live PRD file captured after worktree setup; the loop
    /// re-imports tasks when it changes.
    pub(crate) prd_hash: String,
    /// Live PRD path (worktree copy when worktrees are in use, else source_root copy).
    pub(crate) live_prd_file: PathBuf,
    /// Branch name from PRD metadata (`None` when no branch is configured).
    pub(crate) branch_name: Option<String>,
    /// Task ID prefix for this PRD (`None` when prefixing is disabled).
    pub(crate) task_prefix: Option<String>,
    /// PRD-level default model (resolution rung above the project/user defaults).
    pub(crate) default_model: Option<String>,
    /// Project config loaded once at startup and threaded through every wave.
    pub(crate) project_config: ProjectConfig,
    /// PRD-side implicit-overlap file basenames (cached; extends the baseline list).
    pub(crate) prd_implicit_overlap_files: Vec<String>,
    /// Resolved external git repo path (CLI flag overrides PRD metadata).
    pub(crate) external_repo_path: Option<PathBuf>,
    /// Worktree actually created/reused this run (`Some` only when one was set up).
    pub(crate) actual_worktree_path: Option<PathBuf>,
    /// Working directory for Claude (the worktree path, or `source_root`).
    pub(crate) working_root: PathBuf,
    /// True when parallel-wave execution is active (branch + worktrees + --parallel>1).
    pub(crate) parallel_active: bool,
    /// Slot worktree paths (empty unless `parallel_active`).
    pub(crate) slot_worktree_paths: Vec<PathBuf>,
    /// Resolved iteration ceiling for this run.
    pub(crate) max_iterations: u32,
    /// PRD file stem, used for deadline files and the final banner.
    pub(crate) prd_basename: String,
    /// Run session id from `run_cmd::begin`.
    pub(crate) run_id: String,
    /// Armed SIGINT/SIGTERM/SIGQUIT flag.
    pub(crate) signal_flag: SignalFlag,
    /// `steering.md` path when it exists, else `None` (existence checked once here).
    pub(crate) steering: Option<PathBuf>,
    /// Permission mode resolved at startup (re-checked each iteration for hot-reload).
    pub(crate) permission_mode: PermissionMode,
    /// Usage-API monitoring parameters.
    pub(crate) usage_params: UsageParams,
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

    ui::emit(&format!(
        "Warning: {} task-mgr skill(s) not found in ~/.claude/commands/: {}",
        missing.len(),
        missing.join(", ")
    ));

    if has_repo_copies {
        ui::emit("  Install from this repo:");
        for name in &missing {
            let src = repo_skill_dir.join(format!("{}.md", name));
            if src.exists() {
                ui::emit(&format!(
                    "    cp {} {}/",
                    src.display(),
                    global_dir.display()
                ));
            }
        }
    } else {
        ui::emit(
            "  These skills provide /tm-learn, /tm-recall, /tm-invalidate, /tm-status, /tm-next",
        );
        ui::emit("  See the task-mgr README for installation instructions.");
    }
    ui::emit("");
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
            tracing::warn!("failed to install SIGINT handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register_conditional_default(SIGINT, flag.clone()) {
            tracing::warn!("failed to install SIGINT conditional default: {}", e);
        }
        if let Err(e) = signal_hook::flag::register(SIGTERM, flag.clone()) {
            tracing::warn!("failed to install SIGTERM handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register(SIGQUIT, flag) {
            tracing::warn!("failed to install SIGQUIT handler: {}", e);
        }
    }

    #[cfg(not(unix))]
    {
        use signal_hook::consts::SIGINT;
        if let Err(e) = signal_hook::flag::register(SIGINT, flag.clone()) {
            tracing::warn!("failed to install SIGINT handler: {}", e);
        }
        if let Err(e) = signal_hook::flag::register_conditional_default(SIGINT, flag) {
            tracing::warn!("failed to install SIGINT conditional default: {}", e);
        }
    }
}

/// Run the linear startup phase (Steps 1–16) of the autonomous loop.
///
/// On success, returns a fully-populated [`LoopInitContext`]. On any startup
/// failure, returns `Err(LoopResult)` carrying the exact exit code (and worktree
/// path, where applicable) the inline `run_loop` body returned at that point, so
/// the caller can propagate it unchanged. Pure extraction — no behavior change.
pub(crate) fn initialize_loop(
    run_config: &mut LoopRunConfig,
) -> Result<LoopInitContext, LoopResult> {
    // Step 1: Load environment
    env::load_env();

    // Step 1.5: Check for global Claude Code skills
    check_global_skills(&run_config.source_root);

    // Step 2: Validate git repo (source_root is the original repo)
    if let Err(e) = env::validate_git_repo(&run_config.source_root) {
        ui::emit_err(&format!("Error: {}", e));
        ui::emit("Hint: Run task-mgr from within a git repository.");
        return Err(LoopResult {
            exit_code: 1,
            ..Default::default()
        });
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
            ui::emit_err(&format!("Error resolving paths: {}", e));
            ui::emit(
                "Hint: Check that the PRD file path is correct relative to your project root.",
            );
            return Err(LoopResult {
                exit_code: 1,
                ..Default::default()
            });
        }
    };

    // Propagate resolved absolute path so all downstream code (init, prefix
    // generation, hash, etc.) uses the actual file location — which may be in
    // a sibling worktree rather than the local source_root.
    run_config.prd_file = paths.prd_file.clone();

    // Step 4: Ensure directories exist (in db_dir)
    if let Err(e) = env::ensure_directories(&run_config.db_dir) {
        ui::emit_err(&format!("Error creating directories: {}", e));
        return Err(LoopResult {
            exit_code: 1,
            ..Default::default()
        });
    }

    // Step 4.5: Resolve sticky prefix under tasks.db.lock, then acquire
    // loop-{resolved}.lock. Identity lookup needs the DB before the loop lock
    // so the lock name matches the prefix init will actually run (learning [1486]).
    // Separate from tasks.db.lock (short-lived) so read-only commands are not
    // blocked for the duration of the run.
    let prd_hints = read_prd_hints(&run_config.prd_file);
    let pre_lock_branch = prd_hints.branch_name;
    let prd_display = run_config.prd_file.display();
    let pre_lock_prefix: Option<String> = {
        let _db_lock = match LockGuard::acquire(&run_config.db_dir) {
            Ok(guard) => guard,
            Err(e) => {
                ui::emit_err(&format!(
                    "Error: cannot resolve prefix for {prd_display} — database lock held. {e}"
                ));
                return Err(LoopResult {
                    exit_code: 1,
                    ..Default::default()
                });
            }
        };
        let conn = match crate::db::open_and_migrate(&run_config.db_dir) {
            Ok(c) => c,
            Err(e) => {
                ui::emit_err(&format!("Error opening database for prefix resolve: {e}"));
                return Err(LoopResult {
                    exit_code: 1,
                    ..Default::default()
                });
            }
        };
        // worktree not set up yet — identity against source_root is correct.
        match resolve_loop_pre_lock_prefix(
            &conn,
            &run_config.prd_file,
            &run_config.prefix_mode,
            &run_config.source_root,
            &run_config.source_root,
        ) {
            Ok(prefix) => prefix,
            Err(e) => {
                ui::emit_err(&format!("Error resolving loop prefix: {e}"));
                return Err(LoopResult {
                    exit_code: 1,
                    ..Default::default()
                });
            }
        }
    }
    .filter(|p| validate_prefix(p).is_ok());
    let lock_name = match &pre_lock_prefix {
        Some(p) => format!("loop-{p}.lock"),
        None => "loop.lock".to_string(),
    };
    let mut loop_lock = match LockGuard::acquire_named(&run_config.db_dir, &lock_name) {
        Ok(guard) => guard,
        Err(e) => {
            match &pre_lock_prefix {
                Some(p) => {
                    ui::emit_err(&format!(
                        "Error: cannot start loop for {prd_display} — another loop is already running (prefix={p}). {e}"
                    ));
                    ui::emit(
                        "Hint: Each PRD gets its own lock file (loop-{prefix}.lock). If the other PRD is still running, wait for it to finish.",
                    );
                }
                None => {
                    ui::emit_err(&format!(
                        "Error: cannot start loop for {prd_display} — another loop is already running on the global lock. {e}"
                    ));
                    ui::emit(
                        "Hint: Each PRD uses its own lock file (loop-{prefix}.lock). If both PRDs lack taskPrefix, they collide on the global lock.",
                    );
                }
            }
            return Err(LoopResult {
                exit_code: 1,
                ..Default::default()
            });
        }
    };

    // Step 4.55: Enrich lock file with prefix/branch immediately after acquisition.
    // pre_lock_prefix and pre_lock_branch are already known from step 4.5.
    if let Err(e) = loop_lock.write_holder_info_extended(
        pre_lock_branch.as_deref(),
        run_config.working_root.to_str(),
        pre_lock_prefix.as_deref(),
    ) {
        tracing::warn!("failed to write extended lock metadata: {} (continuing)", e);
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
            ui::emit("Branch change handled, continuing with new branch setup");
        }
        Ok(false) => {} // No change or first run
        Err(e) => {
            tracing::warn!("branch change detection failed: {} (continuing)", e);
        }
    }

    // Step 5: Initialize PRD (creates schema + imports tasks, idempotent)
    // Uses run_config.prefix_mode: Auto for single runs, Explicit for batch mode.
    // Pass explicit roots — loop source_root ≠ db_dir; Default InitOpts is test-only.
    if let Err(e) = crate::commands::init_with_opts(
        &run_config.db_dir,
        &[&run_config.prd_file],
        false, // force
        true,  // append
        true,  // update_existing
        false, // dry_run
        run_config.prefix_mode.clone(),
        crate::commands::InitOpts {
            source_root: Some(run_config.source_root.clone()),
            worktree_root: Some(run_config.source_root.clone()),
        },
    ) {
        ui::emit_err(&format!("Error initializing PRD: {}", e));
        return Err(LoopResult {
            exit_code: 1,
            ..Default::default()
        });
    }

    // Step 5.5: PRD hash — computed after worktree setup (step 8.5) since
    // Claude edits the worktree copy, not the source_root copy.
    #[allow(unused_assignments)]
    let mut prd_hash = String::new();

    // Step 6: Open DB connection (after init to ensure schema exists)
    let mut conn = match crate::db::open_connection(&run_config.db_dir) {
        Ok(c) => c,
        Err(e) => {
            ui::emit_err(&format!("Error opening database: {}", e));
            return Err(LoopResult {
                exit_code: 1,
                ..Default::default()
            });
        }
    };

    if run_config.config.verbose {
        let canonical = run_config.db_dir.join("tasks.db");
        ui::emit(&format!("[verbose] Database path: {}", canonical.display()));
        ui::emit(&format!(
            "[verbose] Source root:   {}",
            run_config.source_root.display()
        ));
        ui::emit(&format!(
            "[verbose] Working root:  {}",
            run_config.working_root.display()
        ));
    }

    // Step 6.5: Run any pending migrations (e.g. v4 adds external_git_repo column)
    if let Err(e) = crate::db::run_migrations(&mut conn) {
        tracing::warn!("failed to run migrations: {} (continuing)", e);
    }

    // Step 6.55: Reuse the prefix already determined at step 4.5 — no second file read.
    // pre_lock_prefix comes from the shared sticky resolver (same as init), so the
    // lock name matches the prefix that actually runs after step 5.
    let early_task_prefix: Option<String> = pre_lock_prefix.clone();

    // Step 6.6: Recover stale in_progress tasks from previous crashed/killed runs.
    // Safe because we hold the per-prefix loop lock — no other loop with the same
    // prefix can be running. (Loops on different prefixes CAN run concurrently.)
    // Recovery is scoped by prefix so concurrent loops don't reset each other.
    match TaskLifecycle::new(&mut conn).recover_in_progress_for_prefix(early_task_prefix.as_deref())
    {
        Ok(count) if count > 0 => {
            ui::emit(&format!(
                "Recovered {} stale in_progress task(s) from previous run",
                count
            ));
        }
        Ok(_) => {}
        Err(e) => {
            // Hard error: if recovery fails, the loop will deadlock on blocked dependencies
            ui::emit_err(&format!("Error: failed to reset stale tasks: {}", e));
            return Err(LoopResult {
                exit_code: 1,
                ..Default::default()
            });
        }
    }

    // Step 6.7: Auto-retire stale learnings at session start so recall quality
    // is high from the first task. Uses default thresholds (90 days, 10 shows, 5% rate).
    match crate::commands::curate::curate_retire(&conn, Default::default()) {
        Ok(result) if result.learnings_retired > 0 => {
            ui::emit(&format!(
                "Auto-retired {} stale learning(s) at session start",
                result.learnings_retired
            ));
        }
        Ok(_) => {} // nothing to retire
        Err(e) => {
            tracing::warn!("auto-retire learnings failed: {} (continuing)", e);
        }
    }

    // Step 7: Read PRD metadata for branch name, task count, and external repo
    let prd_metadata = match read_prd_metadata(&conn, early_task_prefix.as_deref()) {
        Ok(meta) => meta,
        Err(e) => {
            ui::emit_err(&format!("Error reading PRD metadata: {}", e));
            return Err(LoopResult {
                exit_code: 1,
                ..Default::default()
            });
        }
    };
    let branch_name = prd_metadata.branch_name;
    let task_count = prd_metadata.task_count;
    let task_prefix = prd_metadata.task_prefix;
    let default_model = prd_metadata.default_model;
    // FR-002 hard break: a PRD-level `default_model` is parsed/stored/exported
    // verbatim but is ignored by model resolution under the provider-first
    // `models`/`routing` config. Warn once at loop run so the operator knows.
    if default_model.is_some() {
        crate::output::warn(
            "PRD metadata `default_model` is ignored under the models config; use \
             models.anchor / routing instead",
        );
    }
    // Config-level defaults: fall below PRD default in the resolution chain.
    // The loop engine never prompts — it runs non-interactively — so these
    // are pure reads. Users pin a default via `task-mgr init` or
    // `task-mgr models set-default`.
    //
    // Fix 2 from /review-loop: load the full `ProjectConfig` once at the
    // start of the run and thread it through `WaveParams` instead of
    // re-reading + re-parsing `.task-mgr/config.json` from every wave
    // (FEAT-003 implicit-overlap pull, FEAT-002 halt-threshold check, the
    // merge-resolver settings, the FEAT-005 reconcile threshold). Mid-loop
    // edits to the file are NOT picked up; operators restart the loop to
    // apply config changes — matching every other run-scoped knob.
    let project_config =
        crate::loop_engine::project_config::read_project_config(&run_config.db_dir);
    // Same caching rationale for the PRD-side `implicit_overlap_files`
    // override. Field is rare and small (a list of file basenames), so
    // an empty Vec when the PRD JSON is absent / malformed is safe.
    let prd_implicit_overlap_files = read_prd_implicit_overlap_files(paths.prd_file.as_path());

    // Step 7.05: Now that task_prefix is known, re-derive per-PRD progress file.
    if let Some(ref pfx) = task_prefix {
        paths.progress_file = paths.tasks_dir.join(format!("progress-{}.txt", pfx));
    }

    // Step 7.1: Reconcile tasks that have passes: true in PRD but are not done in DB.
    // This catches tasks completed in a previous run where the DB status was never
    // updated (e.g., rate limit interrupted git detection, or loop exit reset them).
    reconcile_passes_with_db(&mut conn, &run_config.prd_file, task_prefix.as_deref());

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
                ui::emit(&ui::yellow(&format!(
                    "⚠ Setup warning: {} blocker(s) detected in ~/.claude/settings.json:",
                    blockers.len()
                )));
                for b in &blockers {
                    ui::emit(&format!("  {} {}", ui::yellow("•"), b.message));
                    if let Some(ref fix) = b.fix_command {
                        ui::emit(&format!("    Fix: {fix}"));
                    }
                }
                ui::emit(&ui::yellow(
                    "  The loop will continue but tool calls may be blocked.",
                ));
                ui::emit("  Run `task-mgr doctor --setup` for a full audit.");
                ui::emit("");
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
                    ui::emit_err(&format!("Error setting up worktree: {}", e));
                    return Err(LoopResult {
                        exit_code: 1,
                        ..Default::default()
                    });
                }
            }
        } else {
            // Old behavior: checkout branch in source_root
            if let Err(e) =
                env::ensure_branch(&run_config.source_root, branch, run_config.config.yes_mode)
            {
                ui::emit_err(&format!("Error: {}", e));
                return Err(LoopResult {
                    exit_code: 1,
                    ..Default::default()
                });
            }
            run_config.source_root.clone()
        }
    } else {
        // No branch specified, use source_root as working directory
        run_config.source_root.clone()
    };

    // Steps 8.4–8.5: worktree bootstrap + live PRD path.
    // One canonicalize(source_root); all dest path math via `git::remap_into_worktree`
    // (no inline strip_prefix remap). exists()/copy and the re-import exists() gate
    // below are worktree policy — not CLI `choose_cli_write_path` (pin 15).
    let live_prd_file = if working_root != run_config.source_root {
        let canonical_source = run_config
            .source_root
            .canonicalize()
            .unwrap_or_else(|_| run_config.source_root.clone());

        // Step 8.4: copy PRD JSON / prompt / PRD markdown into the worktree when missing.
        let copy_if_missing = |src: &Path| {
            let dest = crate::git::remap_into_worktree(src, &canonical_source, &working_root);
            // Strip miss returns resolved unchanged — only copy when remapped into wt.
            let Ok(rel) = dest.strip_prefix(&working_root) else {
                return;
            };
            if !dest.exists() && src.exists() {
                if let Some(parent) = dest.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::copy(src, &dest) {
                    tracing::warn!("failed to copy {} to worktree: {}", rel.display(), e);
                } else {
                    ui::emit(&format!("Copied {} to worktree", rel.display()));
                }
            }
        };

        copy_if_missing(&paths.prd_file);
        copy_if_missing(&paths.prompt_file);
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

        // Step 8.5: unconditional live PRD remap (path math only).
        let remapped =
            crate::git::remap_into_worktree(&paths.prd_file, &canonical_source, &working_root);
        if remapped == paths.prd_file {
            tracing::warn!(
                "could not remap PRD to worktree (prd={}, source={})",
                paths.prd_file.display(),
                canonical_source.display()
            );
        }
        remapped
    } else {
        paths.prd_file.clone()
    };
    // If using a worktree, re-import from the worktree PRD to pick up any tasks
    // that were added in the worktree but not in source_root (e.g., tasks created
    // by Claude during a previous run that only exist in the worktree copy).
    if live_prd_file != run_config.prd_file
        && live_prd_file.exists()
        && let Err(e) = crate::commands::init_with_opts(
            &run_config.db_dir,
            &[&live_prd_file],
            false, // force
            true,  // append
            true,  // update_existing
            false, // dry_run
            run_config.prefix_mode.clone(),
            crate::commands::InitOpts {
                source_root: Some(run_config.source_root.clone()),
                worktree_root: Some(working_root.clone()),
            },
        )
    {
        tracing::warn!("worktree PRD re-import failed: {} (continuing)", e);
    }
    prd_hash = hash_file(&live_prd_file);
    // Override paths.prd_file so all iteration code (mark_task_done, reconcile, etc.)
    // reads/writes the worktree copy, not the source_root copy.
    paths.prd_file = live_prd_file.clone();

    // Step 9: Check uncommitted changes (in working_root)
    if let Err(e) = env::check_uncommitted_changes(&working_root, run_config.config.yes_mode) {
        ui::emit_err(&format!("Error: {}", e));
        return Err(LoopResult {
            exit_code: 1,
            worktree_path: actual_worktree_path,
            ..Default::default()
        });
    }

    // Step 9.5: Parallel wave setup (FEAT-010).
    // Wave execution requires a branch (for ephemeral slot branches) AND
    // worktrees enabled. If the user asked for --parallel > 1 but either
    // pre-condition is missing, we warn and silently fall back to the
    // sequential path so the loop still makes progress instead of failing.
    let parallel_requested = run_config.config.parallel_slots > 1;
    let (parallel_active, slot_worktree_paths) = if parallel_requested {
        match (branch_name.as_ref(), run_config.config.use_worktrees) {
            (Some(branch), true) => {
                // FEAT-005: clean up any `{branch}-slot-N` left over from a
                // prior loop crash before we try to (re)create slot worktrees.
                // Aborts startup on dirty / un-merged anomalies; otherwise
                // returns Ok and leaves the path clear for `ensure_slot_worktrees`.
                let halt_threshold = project_config.merge_fail_halt_threshold;
                // Reconcile auto-recovery (FEAT-005): try to merge stale
                // ephemerals back into the base branch using the same
                // preflight + LlmMergeResolver path live waves take. Owned
                // strings live for the duration of the reconcile call only;
                // the synthetic `run_id` is good enough for stash-tag
                // disambiguation because real run-id allocation is downstream
                // (Step 12 `run_cmd::begin`). The signal flag is fresh — no
                // handler has been installed yet at this point in startup, so
                // SIGINT/SIGTERM during the brief recovery window proceeds via
                // the spawned provider CLI's own signal handling.
                let recovery_signal_flag = SignalFlag::new();
                // Provider-agnostic plan from the project's models block
                // (primary + optional fallback, standard tier). Same helper
                // live waves use so startup and wave merge-back cannot drift.
                let resolved_models =
                    model::resolve_models_config(&project_config.models, &project_config.routing);
                let recovery_plan = model::merge_resolver_plan(&resolved_models);
                // Own model strings for the duration of the reconcile call
                // (plan borrows from resolved_models which is local).
                let recovery_primary_model = recovery_plan.primary.model.map(str::to_string);
                let recovery_fallback_model = recovery_plan
                    .fallback
                    .and_then(|f| f.model.map(str::to_string));
                let recovery_effort = project_config
                    .merge_resolver_effort
                    .clone()
                    .unwrap_or_else(|| "medium".to_string());
                let recovery_timeout =
                    Duration::from_secs(project_config.merge_resolver_timeout_secs.unwrap_or(600));
                // FEAT-006: progress file name for unioning a recovered
                // slot's progress into slot 0 before its branch is deleted.
                let recovery_progress_fname = branch::progress_file_name(task_prefix.as_deref());
                let recovery_cfg = worktree::AutoRecoveryConfig {
                    primary_provider: recovery_plan.primary.provider,
                    primary_model: recovery_primary_model.as_deref(),
                    fallback_provider: recovery_plan.fallback.map(|f| f.provider),
                    fallback_model: recovery_fallback_model.as_deref(),
                    effort: recovery_effort.as_str(),
                    timeout: recovery_timeout,
                    signal_flag: recovery_signal_flag.inner(),
                    db_dir: Some(run_config.db_dir.as_path()),
                    tasks_dir: Some(paths.tasks_dir.as_path()),
                    run_id: "startup-reconcile",
                    stash_limit: project_config.slot_stash_limit,
                    progress_file_name: recovery_progress_fname.as_str(),
                };
                if let Err(e) = worktree::reconcile_stale_ephemeral_slots(
                    &run_config.source_root,
                    branch,
                    halt_threshold,
                    Some(&recovery_cfg),
                ) {
                    ui::emit_err(&format!(
                        "Error: stale ephemeral-slot reconcile aborted startup: {}",
                        e
                    ));
                    return Err(LoopResult {
                        exit_code: 1,
                        worktree_path: actual_worktree_path,
                        ..Default::default()
                    });
                }
                match worktree::ensure_slot_worktrees(
                    &run_config.source_root,
                    branch,
                    run_config.config.parallel_slots,
                ) {
                    Ok(paths) => {
                        ui::emit(&format!(
                            "Parallel mode active: {} slots ({} ephemeral branches)",
                            run_config.config.parallel_slots,
                            run_config.config.parallel_slots.saturating_sub(1)
                        ));
                        (true, paths)
                    }
                    Err(e) => {
                        ui::emit_err(&format!(
                            "Warning: failed to set up slot worktrees: {} — falling back to sequential",
                            e
                        ));
                        (false, Vec::new())
                    }
                }
            }
            (None, _) => {
                ui::emit_err(&format!(
                    "Warning: --parallel {} requires a branchName in the PRD; falling back to sequential",
                    run_config.config.parallel_slots
                ));
                (false, Vec::new())
            }
            (Some(_), false) => {
                ui::emit_err(&format!(
                    "Warning: --parallel {} requires use_worktrees=true; falling back to sequential",
                    run_config.config.parallel_slots
                ));
                (false, Vec::new())
            }
        }
    } else {
        (false, Vec::new())
    };

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
        ui::emit_err(&format!("Error creating deadline: {}", e));
        return Err(LoopResult {
            exit_code: 1,
            worktree_path: actual_worktree_path,
            ..Default::default()
        });
    }

    // Step 12: Begin run session
    let begin_result = match run_cmd::begin(&conn) {
        Ok(r) => r,
        Err(e) => {
            ui::emit_err(&format!("Error beginning run: {}", e));
            deadline::cleanup_deadline(&paths.tasks_dir, &prd_basename);
            return Err(LoopResult {
                exit_code: 1,
                worktree_path: actual_worktree_path,
                ..Default::default()
            });
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
        )
        .len();
        if count > 0 {
            ui::emit(&format!(
                "Startup reconciliation: marked {} task(s) done from external repo",
                count
            ));
        }
    }

    // Step 12.7: Display any deferred key decisions from previous sessions
    match key_decisions_db::get_all_pending_decisions(&conn) {
        Ok(decisions) if !decisions.is_empty() => {
            ui::emit(&format!(
                "\n\x1b[33m⚑ {} deferred key decision(s) from previous sessions:\x1b[0m",
                decisions.len()
            ));
            for d in &decisions {
                let task_ctx = d
                    .task_id
                    .as_deref()
                    .map(|t| format!(" [task: {}]", t))
                    .unwrap_or_default();
                ui::emit(&format!("  • {}{}", d.title, task_ctx));
                ui::emit(&format!("    {}", d.description));
            }
            ui::emit("");
        }
        Ok(_) => {}
        Err(e) => {
            // Non-fatal: pre-v12 DB won't have this table — an expected benign
            // condition on old DBs, not an operator-actionable warning.
            tracing::debug!("could not query deferred key decisions: {}", e);
        }
    }

    // Step 13: Install signal handler
    let signal_flag = SignalFlag::new();
    setup_signal_handler(signal_flag.clone());

    // Step 14: Resolve steering.md path
    let steering_path = paths.tasks_dir.join("steering.md");
    let steering = if steering_path.exists() {
        Some(steering_path)
    } else {
        None
    };

    // Step 15: Resolve permission mode (needed for banner hint below).
    // Resolved once at startup; re-checked each iteration for hot-reload.
    let permission_mode = config::resolve_permission_mode(&run_config.db_dir);

    if run_config.config.verbose {
        ui::emit(&format!("[verbose] Permission mode: {}", permission_mode));
    }

    // Step 15.5: Print session banner
    let branch_display = branch_name.as_deref().unwrap_or("(unknown)");
    let db_path = run_config.db_dir.join("tasks.db");
    let banner_hints = display::SessionBannerHints {
        db_path: &db_path,
        prefix: task_prefix.as_deref(),
        worktree_path: actual_worktree_path.as_deref(),
        tasks_dir: Some(paths.tasks_dir.as_path()),
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
        ui::emit(AUTO_MODE_DEPRECATION_HINT);
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
            ui::emit(&format!("{} task(s) require human review", review_count));
        }
    }

    // Step 16: Build usage params
    let usage_check_enabled = claude_usage_check_enabled(
        run_config.config.usage_check_enabled,
        &project_config.models,
        &project_config.routing,
    );
    if run_config.config.usage_check_enabled && !usage_check_enabled {
        ui::emit("Skipping Claude usage/OAuth pre-check (Claude provider disabled)");
    }
    // Floors: CLI > env > usagePolicy > factory 2 / 1. Horizon CLI overlays
    // stay Option so Ask-wait re-eval can re-apply them after a disk re-read.
    let env_remaining = std::env::var("LOOP_USAGE_REMAINING_MIN")
        .ok()
        .and_then(|v| v.parse::<u8>().ok());
    let env_weekly = std::env::var("LOOP_USAGE_REMAINING_MIN_WEEKLY")
        .ok()
        .and_then(|v| v.parse::<u8>().ok());
    let floors = crate::loop_engine::quota::RemainingFloors {
        other: crate::loop_engine::config::resolve_remaining_floor(
            run_config.config.usage_remaining_min_cli,
            env_remaining,
            project_config.usage_policy.remaining_min_percent,
        ),
        weekly: crate::loop_engine::config::resolve_remaining_floor(
            run_config.config.usage_remaining_min_weekly_cli,
            env_weekly,
            project_config.usage_policy.remaining_min_weekly_percent,
        ),
    };
    let usage_params = UsageParams {
        enabled: usage_check_enabled,
        floors,
        fallback_wait: run_config.config.usage_fallback_wait,
        ask_ttl_override: run_config.config.use_other_models_ttl,
        wait_if_reset_within_cli: run_config.config.wait_if_reset_within_cli,
        stop_if_reset_beyond_cli: run_config.config.stop_if_reset_beyond_cli,
    };

    Ok(LoopInitContext {
        loop_lock,
        conn,
        paths,
        prd_hash,
        live_prd_file,
        branch_name,
        task_prefix,
        default_model,
        project_config,
        prd_implicit_overlap_files,
        external_repo_path,
        actual_worktree_path,
        working_root,
        parallel_active,
        slot_worktree_paths,
        max_iterations,
        prd_basename,
        run_id,
        signal_flag,
        steering,
        permission_mode,
        usage_params,
    })
}

/// Resolve the prefix for `loop-{prefix}.lock` via the shared sticky resolver.
///
/// Caller must hold `tasks.db.lock` and pass an open/migrated connection.
/// Uses `dry_run = true` so JSON restore is deferred to init under the loop lock.
///
/// Refuses `PrefixMode::Auto` on a registered NULL identity (NULL cannot be
/// Auto-run). First `loop init --no-prefix` registration remains allowed via
/// init's Disabled path.
fn resolve_loop_pre_lock_prefix(
    conn: &Connection,
    prd_file: &Path,
    prefix_mode: &PrefixMode,
    source_root: &Path,
    worktree_root: &Path,
) -> Result<Option<String>, TaskMgrError> {
    let content = std::fs::read_to_string(prd_file).map_err(|e| {
        TaskMgrError::IoError(std::io::Error::new(
            e.kind(),
            format!("Failed to read {}: {}", prd_file.display(), e),
        ))
    })?;
    let prd: PrdFile = serde_json::from_str(&content)?;
    let (prefix, sticky_id) = resolve_sticky_prefix(
        conn,
        prd_file,
        &prd,
        prefix_mode,
        source_root,
        worktree_root,
        true, // dry_run: no JSON write before loop lock
    )?;
    if matches!(prefix_mode, PrefixMode::Auto) && prefix.is_none() && sticky_id.is_some() {
        return Err(TaskMgrError::InvalidState {
            resource_type: "PRD".to_string(),
            id: prd_file.display().to_string(),
            expected: "non-NULL registered prefix for Auto loop run".to_string(),
            actual: "registered NULL identity (from loop init --no-prefix); \
                     Auto loop run refuses — re-init with --prefix/--force, or run with --no-prefix"
                .to_string(),
        });
    }
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init::{InitOpts, PrefixMode, generate_prefix, init_with_opts};
    use crate::db::open_connection;
    use std::fs;
    use tempfile::TempDir;

    fn write_minimal_prd(path: &Path, prefix: Option<&str>, branch: &str) {
        let prefix_field = match prefix {
            Some(p) => format!(r#""taskPrefix": "{p}","#),
            None => String::new(),
        };
        fs::write(
            path,
            format!(
                r#"{{
  "project": "pre-lock-test",
  {prefix_field}
  "branchName": "{branch}",
  "userStories": [
    {{
      "id": "TASK-001",
      "title": "One",
      "description": "d",
      "acceptanceCriteria": ["a"],
      "priority": 1,
      "passes": false,
      "dependsOn": []
    }}
  ]
}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn test_pre_lock_matches_sticky_registered_prefix_not_fresh_hash() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_path_buf();
        let db_dir = project.join(".task-mgr");
        fs::create_dir_all(&db_dir).unwrap();
        let json_path = project.join("prd.json");
        write_minimal_prd(&json_path, None, "feat/original");

        init_with_opts(
            &db_dir,
            &[&json_path],
            false,
            false,
            false,
            false,
            PrefixMode::Explicit("FROZEN01".into()),
            InitOpts {
                source_root: Some(project.clone()),
                worktree_root: Some(project.clone()),
            },
        )
        .unwrap();

        // Simulate branchName change that would mint a different generate_prefix.
        write_minimal_prd(&json_path, Some("FROZEN01"), "feat/renamed");
        let would_hash = generate_prefix(Some("feat/renamed"), "prd.json");
        assert_ne!(would_hash, "FROZEN01");

        let conn = open_connection(&db_dir).unwrap();
        let pre_lock =
            resolve_loop_pre_lock_prefix(&conn, &json_path, &PrefixMode::Auto, &project, &project)
                .unwrap();
        assert_eq!(pre_lock.as_deref(), Some("FROZEN01"));

        // Post-init sticky restore keeps the same prefix (no second row).
        init_with_opts(
            &db_dir,
            &[&json_path],
            false,
            true,
            true,
            false,
            PrefixMode::Auto,
            InitOpts {
                source_root: Some(project.clone()),
                worktree_root: Some(project.clone()),
            },
        )
        .unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let stored: String = conn
            .query_row("SELECT task_prefix FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "FROZEN01");
        assert_eq!(pre_lock.as_deref(), Some(stored.as_str()));
    }

    #[test]
    fn test_pre_lock_auto_on_registered_null_refuses() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_path_buf();
        let db_dir = project.join(".task-mgr");
        fs::create_dir_all(&db_dir).unwrap();
        let json_path = project.join("prd.json");
        write_minimal_prd(&json_path, None, "feat/null-auto");

        init_with_opts(
            &db_dir,
            &[&json_path],
            false,
            false,
            false,
            false,
            PrefixMode::Disabled,
            InitOpts {
                source_root: Some(project.clone()),
                worktree_root: Some(project.clone()),
            },
        )
        .unwrap();

        let conn = open_connection(&db_dir).unwrap();
        let err =
            resolve_loop_pre_lock_prefix(&conn, &json_path, &PrefixMode::Auto, &project, &project)
                .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("NULL") || msg.contains("null") || msg.contains("--no-prefix"),
            "expected NULL/Auto refuse message, got: {msg}"
        );
    }

    #[test]
    fn test_pre_lock_explicit_mismatch_refuses() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_path_buf();
        let db_dir = project.join(".task-mgr");
        fs::create_dir_all(&db_dir).unwrap();
        let json_path = project.join("prd.json");
        write_minimal_prd(&json_path, None, "feat/mismatch");

        init_with_opts(
            &db_dir,
            &[&json_path],
            false,
            false,
            false,
            false,
            PrefixMode::Explicit("KEEPME01".into()),
            InitOpts {
                source_root: Some(project.clone()),
                worktree_root: Some(project.clone()),
            },
        )
        .unwrap();

        let conn = open_connection(&db_dir).unwrap();
        let err = resolve_loop_pre_lock_prefix(
            &conn,
            &json_path,
            &PrefixMode::Explicit("OTHER001".into()),
            &project,
            &project,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("KEEPME01") || msg.contains("Explicit"),
            "expected mismatch refuse, got: {msg}"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "mismatch must not mint a second row");
    }

    #[test]
    fn test_step_8_4_and_8_5_use_remap_into_worktree_not_inline_strip_prefix() {
        let src = fs::read_to_string(file!()).unwrap();

        fn block_between<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
            let from = src.find(start).unwrap_or_else(|| panic!("{start} present"));
            let rest = &src[from..];
            let to = rest
                .find(end)
                .unwrap_or_else(|| panic!("{end} follows {start}"));
            &rest[..to]
        }

        let block_84 = block_between(&src, "// Step 8.4:", "// Step 8.5:");
        assert!(
            block_84.contains("remap_into_worktree"),
            "Step 8.4 must call remap_into_worktree"
        );
        // dest.strip_prefix(working_root) for log display is OK; forbid remapper-shaped strip.
        assert!(
            !block_84.contains("strip_prefix(&canonical_source)"),
            "Step 8.4 must not inline strip_prefix(source_root); remap owns that"
        );

        let block_85 = block_between(&src, "// Step 8.5:", "// Step 9:");
        assert!(
            block_85.contains("remap_into_worktree"),
            "Step 8.5 must call remap_into_worktree"
        );
        assert!(
            !block_85.contains("strip_prefix"),
            "Step 8.5 must not inline strip_prefix(source_root); remap owns that"
        );
    }

    #[test]
    fn test_startup_generate_prefix_only_via_sticky_resolver() {
        let src = fs::read_to_string(file!()).unwrap();
        // Production body must not call generate_prefix; first-reg hash lives
        // inside resolve_sticky_prefix (init). Tests may still mention it.
        let prod = src.split("#[cfg(test)]").next().unwrap();
        assert!(
            !prod.contains("generate_prefix"),
            "startup production code must not call generate_prefix; use resolve_sticky_prefix"
        );
        assert!(
            prod.contains("resolve_sticky_prefix") || prod.contains("resolve_loop_pre_lock_prefix"),
            "startup must share the sticky resolver"
        );
    }

    #[test]
    fn test_orchestrator_style_reimport_restores_prefix_one_row() {
        let tmp = TempDir::new().unwrap();
        let project = tmp.path().to_path_buf();
        let db_dir = project.join(".task-mgr");
        fs::create_dir_all(&db_dir).unwrap();
        let json_path = project.join("prd.json");
        write_minimal_prd(&json_path, None, "feat/reimport");

        init_with_opts(
            &db_dir,
            &[&json_path],
            false,
            false,
            false,
            false,
            PrefixMode::Explicit("ORCH0001".into()),
            InitOpts {
                source_root: Some(project.clone()),
                worktree_root: Some(project.clone()),
            },
        )
        .unwrap();

        // Operator (or Claude) edits only taskPrefix — orchestrator re-imports via init.
        write_minimal_prd(&json_path, Some("WRONGPRE"), "feat/reimport");
        init_with_opts(
            &db_dir,
            &[&json_path],
            false,
            true,
            true,
            false,
            PrefixMode::Auto,
            InitOpts {
                source_root: Some(project.clone()),
                worktree_root: Some(project.clone()),
            },
        )
        .unwrap();

        let conn = open_connection(&db_dir).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let stored: String = conn
            .query_row("SELECT task_prefix FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "ORCH0001");
        let json = fs::read_to_string(&json_path).unwrap();
        assert!(
            json.contains(r#""taskPrefix": "ORCH0001""#)
                || json.contains(r#""taskPrefix":"ORCH0001""#),
            "JSON must be restored to registered prefix, got: {json}"
        );
    }
}
