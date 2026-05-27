//! Outer autonomous loop orchestration.
//!
//! Carved out of `engine.rs` (PRD 02, FEAT-005). This module owns the
//! top-level `run_loop` — env setup → git validation → init PRD → run
//! lifecycle → iterate (dispatching to the sequential `iteration::run_iteration`
//! or the parallel `wave_scheduler::run_parallel_wave` at the iteration
//! boundary) → auto-review → cleanup — plus the run-lifecycle helpers it owns:
//! `check_global_skills`, `setup_signal_handler`, `on_run_completed`,
//! `record_session_guidance`, `prompt_pending_key_decisions`,
//! `trigger_human_reviews`, and `query_human_review_tasks`.
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
//! **Signal handler ownership**: `run_loop` constructs the `SignalFlag`, arms
//! it via `setup_signal_handler`, and threads it through
//! `WaveIterationParams` / `SlotIterationParams` / `IterationContext` exactly
//! as before. **Stale ephemeral reconcile (defense layer #5)**: the
//! `worktree::reconcile_stale_ephemeral_slots` call runs BEFORE
//! `ensure_slot_worktrees` (CLAUDE.md Step 9.5) and is preserved here.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::commands::decisions::find_option;
use crate::commands::doctor::setup_checks::pre_check_loop_setup;
use crate::commands::doctor::setup_output::SetupSeverity;
use crate::commands::init::{PrefixMode, generate_prefix};
use crate::commands::run as run_cmd;
use crate::db::LockGuard;
use crate::db::prefix::{prefix_and, validate_prefix};
use crate::db::schema::key_decisions as key_decisions_db;
use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::branch;
use crate::loop_engine::calibrate;
use crate::loop_engine::config::{self, IterationOutcome, PermissionMode};
use crate::loop_engine::deadline;
use crate::loop_engine::display;
use crate::loop_engine::env;
use crate::loop_engine::git_reconcile::{
    check_git_for_task_completion, reconcile_external_git_completions, wrapper_commit,
};
use crate::loop_engine::guidance::SessionGuidance;
use crate::loop_engine::iteration_pipeline;
use crate::loop_engine::model;
use crate::loop_engine::oauth;
use crate::loop_engine::prd_reconcile::{
    self as prd_reconcile, hash_file, read_prd_metadata, reconcile_passes_with_db,
};
use crate::loop_engine::progress;
use crate::loop_engine::signals::{self, SignalFlag, handle_human_review};
use crate::loop_engine::status_queries::read_prd_hints;
use crate::loop_engine::wave_scheduler::classify_drained_queue;
use crate::loop_engine::worktree;
use crate::models::RunStatus;

use crate::loop_engine::engine::*;

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
            ..Default::default()
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
                ..Default::default()
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
            ..Default::default()
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
                ..Default::default()
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
            ..Default::default()
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
                ..Default::default()
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
    match TaskLifecycle::new(&mut conn).recover_in_progress_for_prefix(early_task_prefix.as_deref())
    {
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
                ..Default::default()
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
                ..Default::default()
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
    let project_default_model = project_config.default_model.clone();
    let user_default_model = crate::loop_engine::user_config::read_user_config().default_model;
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
                        ..Default::default()
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
                    ..Default::default()
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
            ..Default::default()
        };
    }

    // Step 9.5: Parallel wave setup (FEAT-010).
    // Wave execution requires a branch (for ephemeral slot branches) AND
    // worktrees enabled. If the user asked for --parallel > 1 but either
    // pre-condition is missing, we warn and silently fall back to the
    // sequential path so the loop still makes progress instead of failing.
    let parallel_requested = run_config.config.parallel_slots > 1;
    let (mut parallel_active, slot_worktree_paths) = if parallel_requested {
        match (branch_name.as_ref(), run_config.config.use_worktrees) {
            (Some(branch), true) => {
                // FEAT-005: clean up any `{branch}-slot-N` left over from a
                // prior loop crash before we try to (re)create slot worktrees.
                // Aborts startup on dirty / un-merged anomalies; otherwise
                // returns Ok and leaves the path clear for `ensure_slot_worktrees`.
                let halt_threshold = project_config.merge_fail_halt_threshold;
                // Reconcile auto-recovery (FEAT-005): try to merge stale
                // ephemerals back into the base branch using the same
                // preflight + ClaudeMergeResolver path live waves take. Owned
                // strings live for the duration of the reconcile call only;
                // the synthetic `run_id` is good enough for stash-tag
                // disambiguation because real run-id allocation is downstream
                // (Step 12 `run_cmd::begin`). The signal flag is fresh — no
                // handler has been installed yet at this point in startup, so
                // SIGINT/SIGTERM during the brief recovery window proceeds via
                // the spawned Claude's own signal handling.
                let recovery_signal_flag = SignalFlag::new();
                let recovery_model = project_default_model
                    .as_deref()
                    .filter(|m| !m.trim().is_empty())
                    .unwrap_or(model::SONNET_MODEL)
                    .to_string();
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
                    model: recovery_model.as_str(),
                    effort: recovery_effort.as_str(),
                    claude_timeout: recovery_timeout,
                    signal_flag: recovery_signal_flag.inner(),
                    db_dir: Some(run_config.db_dir.as_path()),
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
                    eprintln!(
                        "Error: stale ephemeral-slot reconcile aborted startup: {}",
                        e
                    );
                    return LoopResult {
                        exit_code: 1,
                        worktree_path: actual_worktree_path,
                        ..Default::default()
                    };
                }
                match worktree::ensure_slot_worktrees(
                    &run_config.source_root,
                    branch,
                    run_config.config.parallel_slots,
                ) {
                    Ok(paths) => {
                        eprintln!(
                            "Parallel mode active: {} slots ({} ephemeral branches)",
                            run_config.config.parallel_slots,
                            run_config.config.parallel_slots.saturating_sub(1)
                        );
                        (true, paths)
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: failed to set up slot worktrees: {} — falling back to sequential",
                            e
                        );
                        (false, Vec::new())
                    }
                }
            }
            (None, _) => {
                eprintln!(
                    "Warning: --parallel {} requires a branchName in the PRD; falling back to sequential",
                    run_config.config.parallel_slots
                );
                (false, Vec::new())
            }
            (Some(_), false) => {
                eprintln!(
                    "Warning: --parallel {} requires use_worktrees=true; falling back to sequential",
                    run_config.config.parallel_slots
                );
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
        eprintln!("Error creating deadline: {}", e);
        return LoopResult {
            exit_code: 1,
            worktree_path: actual_worktree_path,
            ..Default::default()
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
                ..Default::default()
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
                eprintln!(
                    "Warning: parallel_active=true but branch_name is None; \
                     falling through to sequential iteration"
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
            };
            let outcome = run_wave_iteration(wave_params, &mut ctx);
            tasks_completed += outcome.tasks_completed;
            if outcome.iteration_consumed {
                iterations_completed += 1;
            }
            if outcome.was_stopped {
                was_stopped = true;
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
            eprintln!("Warning: failed to update run: {}", e);
        }

        // Record epoch before completion detection so we can later identify tasks
        // completed this iteration (used for human review triggering).
        let completion_epoch_start: i64 = conn
            .query_row("SELECT CAST(strftime('%s', 'now') AS INTEGER)", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);

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

        // Wrapper commit: if the claimed task was completed but no git commit
        // exists (Claude couldn't commit in scoped permission mode), commit on
        // its behalf.
        if claimed_was_completed
            && let Some(ref task_id) = result.task_id
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
                &mut conn,
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
        // FEAT-007: also exclude Crash(GrokAuthFailure) — an xAI auth lapse is an operator
        // problem, not a task failure; incrementing here would push a healthy task toward
        // auto_block_task with a misleading reason.
        if let Some(ref task_id) = result.task_id
            && !matches!(
                result.outcome,
                IterationOutcome::Completed
                    | IterationOutcome::Empty
                    | IterationOutcome::Reorder(_)
                    | IterationOutcome::RateLimit
                    | IterationOutcome::Crash(config::CrashType::GrokAuthFailure)
            )
            && let Err(e) = handle_task_failure(
                &mut conn,
                task_id,
                iteration as i64,
                &mut ctx,
                project_config.fallback_runner.as_ref(),
                project_config.primary_runner.as_ref(),
            )
        {
            eprintln!("Warning: failed to start retry tracking transaction: {}", e);
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
                eprintln!("{}", drained.reason);
                exit_code = drained.exit_code;
                exit_reason = drained.reason;
                final_run_status = drained.run_status;
                break;
            }
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
        eprintln!("Warning: failed to end run: {}", e);
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
        eprintln!(
            "Warning: cleanup_slot_worktrees failed: {} — leaving slot worktrees intact",
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
        tasks_completed,
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

#[cfg(test)]
#[allow(deprecated)] // FEAT-010: tests exercise the deprecated apply_status_updates / auto_block_task shims directly.
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
