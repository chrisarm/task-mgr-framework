//! Task Manager CLI entry point.
//!
//! This module handles argument parsing and command dispatch.
//! Output formatting and helper functions are in the `handlers` module.

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;

use task_mgr::TaskMgrError;
use task_mgr::cli::{
    BatchCommand, BatchResolve, Cli, Commands, CurateAction, DecisionAction, EnhanceCommand,
    LoopCommand, LoopResolve, MigrateAction, ModelsAction, OutputFormat, RunAction,
    WorktreesAction, resolve_batch_command, resolve_loop_command,
};
use task_mgr::commands::{
    LearnParams, LearningsListParams, RecallCmdParams, ReviewOptions, add, apply_learning,
    audit_setup, auto_unblock_all, begin, complete, count_resettable_tasks, decline_decision_cmd,
    doctor, end, export, fail, format_doctor_verbose, format_init_verbose, format_next_verbose,
    format_recall_verbose, get_reviewable_tasks, history, history_detail, import_learnings, init,
    invalidate_learning, irrelevant, learn, list, list_decisions, list_learnings, migrate_all,
    migrate_down_cmd, migrate_status, migrate_up_cmd, next, recall, reset_all_tasks, reset_tasks,
    resolve_decision_cmd, revert_decision_cmd, show, skip, stats, unblock, unskip, update,
    worktrees_list, worktrees_prune, worktrees_remove,
};
use task_mgr::db::{DbDirSource, LockGuard, ResolvedDbDir, open_connection, resolve_db_dir};
use task_mgr::handlers::{
    convert_run_end_status, generate_completions, generate_man_pages, output_migrate_result,
    output_result,
};
use task_mgr::learnings::{EditLearningParams, delete_learning, edit_learning};

/// Derive the project root from the git repository root.
///
/// Uses `git rev-parse --show-toplevel` to find the repo root,
/// which handles invocation from subdirectories correctly.
fn get_project_root() -> Result<PathBuf, TaskMgrError> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| TaskMgrError::io_error(".", "finding git repository root", e))?;

    if !output.status.success() {
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git repository".to_string(),
            id: ".".to_string(),
            expected: "a git repository".to_string(),
            actual: "not inside a git repository. Run 'git init' first.".to_string(),
        });
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(root))
}

/// Args passed to [`dispatch_init`] — fields mirror `Commands::Init`'s clap
/// args one-for-one so the call site stays a straight `Args { … }` shuffle.
struct DispatchInitArgs {
    from_json: Vec<PathBuf>,
    enhance: bool,
    force: bool,
    append: bool,
    update_existing: bool,
    dry_run: bool,
    prefix: Option<String>,
    no_prefix: bool,
}

/// Resolve the project root for `init_project`.
///
/// `init_project(dir)` appends `.task-mgr/`, so we need the *parent* of the
/// resolved DB dir. `resolve_db_dir` guarantees `cli.dir` is absolute, so
/// `.parent()` returns `Some(_)` except in the pathological case of `/`.
fn project_root_for_init(cli_dir: &Path) -> PathBuf {
    cli_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| cli_dir.to_path_buf())
}

/// Dispatch the top-level `task-mgr init` command into its three modes:
///
/// 1. `from_json` empty, `force` set → reject with exit 2 (project-level
///    init has no destructive form).
/// 2. `from_json` empty → project-level scaffolding via
///    [`task_mgr::commands::init::init_project`]. If `--enhance`, also run
///    `enhance agents --create --profile full`. Emit a stderr hint pointing
///    at `task-mgr enhance agents`.
/// 3. `from_json` non-empty → deprecated top-level shim. Emit a stderr
///    deprecation notice, run `init_project` first (so the model picker
///    fires for operators who don't yet have a default model resolved),
///    then dispatch the PRD payload through `LoopCommand::Init` (N==1) or
///    `BatchCommand::Init` (N>1). The `--enhance` flag is silently
///    ignored on this path; a stderr note explains.
///
/// The shim and the canonical `loop init` / `batch init` arms call
/// [`task_mgr::commands::init`] with the same `&[PathBuf]` shape — the
/// PRD-import code path is single-rooted regardless of how the user
/// reached it.
fn dispatch_init(
    db_dir: &Path,
    verbose: bool,
    format: OutputFormat,
    args: DispatchInitArgs,
) -> Result<(), TaskMgrError> {
    // Mode 1: reject `--force` on project-level init before any disk write.
    if args.from_json.is_empty() && args.force {
        eprintln!(
            "error: `task-mgr init --force` is not supported. Project-level init has no \
             destructive form. To reset, `rm -rf .task-mgr/` and re-run `task-mgr init`."
        );
        process::exit(2);
    }

    if args.from_json.is_empty() {
        dispatch_init_project(db_dir, format, args.enhance)
    } else {
        dispatch_init_shim(db_dir, verbose, format, args)
    }
}

/// Mode 2: project-level scaffold. Optionally runs `enhance agents` when
/// `--enhance` is set.
fn dispatch_init_project(
    db_dir: &Path,
    format: OutputFormat,
    enhance: bool,
) -> Result<(), TaskMgrError> {
    use task_mgr::commands::init::init_project;

    let project_root = project_root_for_init(db_dir);
    let _lock = LockGuard::acquire(db_dir)?;
    let result = init_project(&project_root)?;

    if enhance {
        use task_mgr::commands::enhance::templates::EnhanceProfile;
        use task_mgr::commands::{AgentsParams, enhance_agents};
        let enhance_result = enhance_agents(AgentsParams {
            targets: Vec::new(),
            dry_run: false,
            create: true,
            profile: EnhanceProfile::Full,
            cwd: project_root,
        })?;
        if !enhance_result.no_errors() {
            eprintln!(
                "warning: `task-mgr init --enhance` completed with one or more file \
                 errors; see messages above"
            );
        }
    } else {
        eprintln!(
            "Initialized .task-mgr/. Run `task-mgr enhance agents` to add task-mgr \
             workflow guidance to CLAUDE.md / AGENTS.md (or rerun with `--enhance`)."
        );
    }

    output_result(&result, format);
    Ok(())
}

/// Mode 3: deprecated top-level shim. Runs `init_project` first (so the
/// model picker fires before PRD-import work; FAILURE-MODE contract), then
/// dispatches the PRD payload through the canonical `init()` path.
fn dispatch_init_shim(
    db_dir: &Path,
    verbose: bool,
    format: OutputFormat,
    args: DispatchInitArgs,
) -> Result<(), TaskMgrError> {
    use task_mgr::commands::init::init_project;

    eprintln!(
        "DEPRECATED: `task-mgr init --from-json X` keeps working indefinitely but the \
         canonical form is `task-mgr loop init X` (single PRD) or `task-mgr batch init \
         <glob>...` (multi)."
    );
    if args.enhance {
        eprintln!(
            "note: --enhance is ignored on the deprecated --from-json path; run \
             `task-mgr init --enhance` separately to update CLAUDE.md / AGENTS.md."
        );
    }

    let project_root = project_root_for_init(db_dir);
    {
        let _lock = LockGuard::acquire(db_dir)?;
        init_project(&project_root)?;
    }

    let prefix_mode =
        task_mgr::commands::init::PrefixMode::from_cli_flags(args.no_prefix, args.prefix);

    // Identical call shape to LoopCommand::Init / BatchCommand::Init dispatch arms.
    let _lock = LockGuard::acquire(db_dir)?;
    let result = init(
        db_dir,
        &args.from_json,
        args.force,
        args.append,
        args.update_existing,
        args.dry_run,
        prefix_mode,
    )?;

    if verbose {
        eprint!("{}", format_init_verbose(&result));
    }
    output_result(&result, format);
    Ok(())
}

fn main() {
    let mut cli = Cli::parse();

    // Make machine-readable output mode observable to library helpers that
    // emit informational stderr notes (e.g., `loop_engine::env::resolve_paths`
    // sibling-worktree fallback). The `human_output_enabled()` predicate
    // keys off this var. Set before any thread spawns so it propagates to
    // every subprocess.
    if cli.format == OutputFormat::Json {
        // SAFETY: single-threaded context — no other thread can observe the
        // mutation. clap parsing runs synchronously above.
        unsafe { std::env::set_var("TASK_MGR_FORMAT", "json") };
    }

    // Resolve --dir to a canonical absolute path *once*, so every subcommand
    // inherits the same DB directory. See `src/db/path.rs` for the rules and
    // the worktree bug this fixes (spawned subprocesses creating stray
    // `<worktree>/.task-mgr/`).
    //
    // clap's derive macro doesn't surface `ValueSource` cleanly for global
    // args, so detect provenance from the actual env / argv inputs.
    let env_provided = std::env::var_os("TASK_MGR_DIR").is_some();
    let cli_provided = std::env::args()
        .skip(1)
        .any(|a| a == "--dir" || a.starts_with("--dir="));
    let was_explicit = env_provided || cli_provided;
    let from_env = env_provided && !cli_provided;

    let resolved = resolve_db_dir(&cli.dir, was_explicit, from_env);

    // Stray-DB guard: if we just redirected the default away from the
    // user's cwd-default location AND a tasks.db already exists at that
    // cwd-default location, warn loudly. Prevents "where did my tasks go"
    // confusion for users with pre-existing stray worktree DBs from before
    // this fix shipped.
    if resolved.source == DbDirSource::WorktreeAnchored
        && let Ok(cwd) = std::env::current_dir()
    {
        let cwd_default = cwd.join(&cli.dir);
        if cwd_default != resolved.path && cwd_default.join("tasks.db").exists() {
            eprintln!(
                "\x1b[33m[warn]\x1b[0m task-mgr: ignoring stray DB at {} \u{2014} \
                 using main-repo DB at {} instead. Move or delete the stray DB \
                 (or pass --dir / set TASK_MGR_DIR) to silence this warning.",
                cwd_default.display(),
                resolved.path.display(),
            );
        }
    }

    cli.dir = resolved.path.clone();

    if let Err(e) = run(cli, resolved) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}

fn run(cli: Cli, resolved_db_dir: ResolvedDbDir) -> Result<(), TaskMgrError> {
    match cli.command {
        Commands::Init {
            from_json,
            enhance,
            force,
            append,
            update_existing,
            dry_run,
            prefix,
            no_prefix,
        } => dispatch_init(
            &cli.dir,
            cli.verbose,
            cli.format,
            DispatchInitArgs {
                from_json,
                enhance,
                force,
                append,
                update_existing,
                dry_run,
                prefix,
                no_prefix,
            },
        ),

        Commands::List {
            status,
            file,
            task_type,
            include_archived,
            prefix,
            prd,
        } => {
            let resolved_prefix = match (prefix, prd) {
                (Some(p), _) => Some(p),
                (_, Some(prd_path)) => {
                    task_mgr::loop_engine::status_queries::read_task_prefix_from_prd(&prd_path)
                        .ok_or_else(|| TaskMgrError::InvalidState {
                            resource_type: "PRD file".to_string(),
                            id: prd_path.display().to_string(),
                            expected: "a JSON file with a \"taskPrefix\" field".to_string(),
                            actual: "missing or unreadable taskPrefix".to_string(),
                        })?
                        .into()
                }
                _ => None,
            };
            let result = list(
                &cli.dir,
                status,
                file.as_deref(),
                task_type.as_deref(),
                include_archived,
                resolved_prefix.as_deref(),
            )?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Show { task_id } => {
            let result = show(&cli.dir, &task_id)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Next {
            after_files,
            claim,
            run_id,
            decay_threshold,
            prefix,
            parallel: _,
        } => {
            let _lock = if claim || decay_threshold > 0 {
                Some(LockGuard::acquire(&cli.dir)?)
            } else {
                None
            };

            if decay_threshold > 0 {
                let conn = open_connection(&cli.dir)?;
                let decayed = task_mgr::commands::next::apply_decay(
                    &conn,
                    decay_threshold,
                    cli.verbose,
                    None,
                )?;
                if !decayed.is_empty() && cli.verbose {
                    eprintln!(
                        "[verbose] Decayed {} blocked/skipped task(s) back to todo",
                        decayed.len()
                    );
                    for (task_id, old_status) in &decayed {
                        eprintln!("[verbose]   - {} (was {})", task_id, old_status);
                    }
                }
            }

            let files = after_files.unwrap_or_default();
            let result = next(
                &cli.dir,
                &files,
                claim,
                run_id.as_deref(),
                cli.verbose,
                prefix.as_deref(),
            )?;

            if cli.verbose {
                eprint!("{}", format_next_verbose(&result));
            }
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Complete {
            task_ids,
            run_id,
            commit,
            force,
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let mut conn = open_connection(&cli.dir)?;
            let result = complete(
                &mut conn,
                &task_ids,
                run_id.as_deref(),
                commit.as_deref(),
                force,
            )?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Fail {
            task_ids,
            run_id,
            error,
            status,
            force,
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let mut conn = open_connection(&cli.dir)?;
            let result = fail(
                &mut conn,
                &task_ids,
                error.as_deref(),
                status,
                run_id.as_deref(),
                force,
            )?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Run { action } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;

            match action {
                RunAction::Begin => {
                    let result = begin(&conn)?;
                    output_result(&result, cli.format);
                }
                RunAction::Update {
                    run_id,
                    last_commit,
                    last_files,
                } => {
                    let result = update(
                        &conn,
                        &run_id,
                        last_commit.as_deref(),
                        last_files.as_deref(),
                    )?;
                    output_result(&result, cli.format);
                }
                RunAction::End { run_id, status } => {
                    let run_status = convert_run_end_status(status);
                    let result = end(&conn, &run_id, run_status)?;
                    output_result(&result, cli.format);
                }
            }
            Ok(())
        }

        Commands::Export {
            to_json,
            with_progress,
            learnings_file,
        } => {
            let result = export(&cli.dir, &to_json, with_progress, learnings_file.as_deref())?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Doctor {
            auto_fix,
            dry_run,
            decay_threshold,
            reconcile_git,
            setup,
        } => {
            // Determine which checks to run:
            // --setup alone: run only setup checks (text or JSON)
            // No DB flags + text mode: run both DB and setup checks
            // No DB flags + JSON mode: run only DB checks (avoid invalid multi-JSON output)
            // DB-specific flags without --setup: run only DB checks
            let no_db_flags = !auto_fix && !dry_run && !reconcile_git;
            let run_setup = setup || (no_db_flags && cli.format == OutputFormat::Text);
            let run_db = !setup;

            // Run setup audit (printed directly; not JSON-routed when combined with DB output)
            if run_setup {
                // Derive project root from the db_dir: ".task-mgr" -> "."
                let project_dir = cli
                    .dir
                    .parent()
                    .map(|p| {
                        if p == std::path::Path::new("") {
                            std::path::Path::new(".")
                        } else {
                            p
                        }
                    })
                    .unwrap_or(std::path::Path::new("."));
                let setup_result = audit_setup(project_dir, auto_fix);
                output_result(&setup_result, cli.format);
            }

            // Run DB health checks
            if run_db {
                let _lock = if (auto_fix || reconcile_git) && !dry_run {
                    Some(LockGuard::acquire(&cli.dir)?)
                } else {
                    None
                };

                let conn = open_connection(&cli.dir)?;
                let result = doctor(
                    &conn,
                    auto_fix,
                    dry_run,
                    decay_threshold,
                    reconcile_git,
                    &cli.dir,
                )?;

                if cli.verbose {
                    eprint!("{}", format_doctor_verbose(&result));
                }
                output_result(&result, cli.format);
            }
            Ok(())
        }

        Commands::Skip {
            task_ids,
            reason,
            run_id,
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let mut conn = open_connection(&cli.dir)?;
            let result = skip(&mut conn, &task_ids, &reason, run_id.as_deref())?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Irrelevant {
            task_ids,
            reason,
            run_id,
            learning_id,
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let mut conn = open_connection(&cli.dir)?;
            let result = irrelevant(
                &mut conn,
                &task_ids,
                &reason,
                run_id.as_deref(),
                learning_id,
            )?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Learn {
            outcome,
            title,
            content,
            task_id,
            run_id,
            root_cause,
            solution,
            files,
            task_types,
            errors,
            tags,
            confidence,
            supersedes,
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;

            let params = LearnParams {
                outcome,
                title,
                content,
                task_id,
                run_id,
                root_cause,
                solution,
                files,
                task_types,
                errors,
                tags,
                confidence,
                supersedes,
            };

            let result = learn(&conn, Some(&cli.dir), params)?;

            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Recall {
            query,
            for_task,
            tags,
            outcome,
            limit,
            include_superseded,
            allow_degraded,
        } => {
            use task_mgr::loop_engine::project_config::read_project_config;

            let conn = open_connection(&cli.dir)?;
            let proj_config = read_project_config(&cli.dir);
            let reranker = proj_config.resolved_reranker_config();

            let params = RecallCmdParams {
                query,
                for_task,
                tags,
                outcome,
                limit,
                ollama_url: proj_config.ollama_url,
                embedding_model: proj_config.embedding_model,
                include_superseded,
                reranker_url: reranker.as_ref().map(|(u, _, _)| u.clone()),
                reranker_model: reranker.as_ref().map(|(_, m, _)| m.clone()),
                reranker_over_fetch: reranker.map(|(_, _, n)| n),
                allow_degraded,
            };

            let result = recall(&conn, params)?;

            if cli.verbose {
                eprint!("{}", format_recall_verbose(&result));
            }
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Learnings { recent } => {
            let conn = open_connection(&cli.dir)?;
            let params = LearningsListParams { recent };
            let result = list_learnings(&conn, params)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::ApplyLearning { learning_id } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;
            let result = apply_learning(&conn, learning_id)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::InvalidateLearning { learning_id } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;
            let result = invalidate_learning(&conn, learning_id)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Unblock { task_id } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;
            let result = unblock(&conn, &task_id)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Unskip { task_id } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;
            let result = unskip(&conn, &task_id)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Add {
            json,
            stdin,
            priority,
            depended_on_by,
        } => {
            let input_json = if let Some(j) = json {
                j
            } else if stdin {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| TaskMgrError::io_error("stdin", "reading JSON input", e))?;
                buf
            } else {
                return Err(TaskMgrError::invalid_state(
                    "add",
                    "input",
                    "either --json or --stdin",
                    "neither provided",
                ));
            };
            let result = add(&cli.dir, &input_json, priority, &depended_on_by)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Reset { task_ids, all, yes } => {
            if all {
                let _lock = LockGuard::acquire(&cli.dir)?;
                let conn = open_connection(&cli.dir)?;

                if !yes {
                    let count = count_resettable_tasks(&conn)?;
                    if count == 0 {
                        let result = reset_all_tasks(&conn)?;
                        output_result(&result, cli.format);
                        return Ok(());
                    }

                    eprintln!(
                        "This will reset {} task(s) to todo status.\n\
                        Use --yes (-y) to confirm.",
                        count
                    );
                    return Err(TaskMgrError::invalid_state(
                        "reset",
                        "--all",
                        "confirmed (--yes flag)",
                        "not confirmed",
                    ));
                }

                let result = reset_all_tasks(&conn)?;
                output_result(&result, cli.format);
                Ok(())
            } else {
                if task_ids.is_empty() {
                    return Err(TaskMgrError::invalid_state(
                        "reset",
                        "arguments",
                        "task IDs or --all flag",
                        "neither provided",
                    ));
                }

                let _lock = LockGuard::acquire(&cli.dir)?;
                let conn = open_connection(&cli.dir)?;
                let result = reset_tasks(&conn, &task_ids)?;
                output_result(&result, cli.format);
                Ok(())
            }
        }

        Commands::Stats => {
            let result = stats(&cli.dir)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::History {
            limit,
            run_id,
            include_archived,
        } => {
            if let Some(rid) = run_id {
                let result = history_detail(&cli.dir, &rid)?;
                output_result(&result, cli.format);
            } else {
                let result = history(&cli.dir, limit, include_archived)?;
                output_result(&result, cli.format);
            }
            Ok(())
        }

        Commands::DeleteLearning { learning_id, yes } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;

            if !yes {
                let learning = task_mgr::learnings::get_learning(&conn, learning_id)?
                    .ok_or_else(|| TaskMgrError::learning_not_found(learning_id.to_string()))?;

                eprintln!(
                    "This will delete learning #{}: \"{}\"\n\
                    Use --yes (-y) to confirm.",
                    learning_id, learning.title
                );
                return Err(TaskMgrError::invalid_state(
                    "delete-learning",
                    "confirmation",
                    "confirmed (--yes flag)",
                    "not confirmed",
                ));
            }

            let result = delete_learning(&conn, learning_id)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::EditLearning {
            learning_id,
            title,
            content,
            solution,
            root_cause,
            confidence,
            add_tags,
            remove_tags,
            add_files,
            remove_files,
            add_task_types,
            remove_task_types,
            add_errors,
            remove_errors,
            supersedes,
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;

            let model_confidence = confidence.map(|c| match c {
                task_mgr::cli::Confidence::High => task_mgr::models::Confidence::High,
                task_mgr::cli::Confidence::Medium => task_mgr::models::Confidence::Medium,
                task_mgr::cli::Confidence::Low => task_mgr::models::Confidence::Low,
            });

            let params = EditLearningParams {
                title,
                content,
                solution,
                root_cause,
                confidence: model_confidence,
                add_tags,
                remove_tags,
                add_files,
                remove_files,
                add_task_types,
                remove_task_types,
                add_errors,
                remove_errors,
                supersedes,
            };

            let result = edit_learning(&conn, learning_id, params)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Review {
            blocked,
            skipped,
            auto,
        } => {
            let options = ReviewOptions {
                blocked_only: blocked,
                skipped_only: skipped,
                auto_unblock: auto,
            };

            if auto {
                let _lock = LockGuard::acquire(&cli.dir)?;
                let conn = open_connection(&cli.dir)?;
                let result = auto_unblock_all(&conn, &options)?;
                output_result(&result, cli.format);
            } else {
                let conn = open_connection(&cli.dir)?;
                let result = get_reviewable_tasks(&conn, &options)?;
                output_result(&result, cli.format);
            }
            Ok(())
        }

        Commands::Decisions { action } => {
            match action {
                DecisionAction::List { all } => {
                    let conn = open_connection(&cli.dir)?;
                    let result = list_decisions(&conn, all)?;
                    output_result(&result, cli.format);
                }
                DecisionAction::Resolve {
                    decision_id,
                    option,
                } => {
                    let _lock = LockGuard::acquire(&cli.dir)?;
                    let conn = open_connection(&cli.dir)?;
                    let result = resolve_decision_cmd(&conn, decision_id, &option)?;
                    output_result(&result, cli.format);
                }
                DecisionAction::Decline {
                    decision_id,
                    reason,
                } => {
                    let _lock = LockGuard::acquire(&cli.dir)?;
                    let conn = open_connection(&cli.dir)?;
                    let result = decline_decision_cmd(&conn, decision_id, reason.as_deref())?;
                    output_result(&result, cli.format);
                }
                DecisionAction::Revert { decision_id } => {
                    let _lock = LockGuard::acquire(&cli.dir)?;
                    let conn = open_connection(&cli.dir)?;
                    let result = revert_decision_cmd(&conn, decision_id)?;
                    output_result(&result, cli.format);
                }
            }
            Ok(())
        }

        Commands::Migrate { action } => {
            match action {
                MigrateAction::Status => {
                    let conn = open_connection(&cli.dir)?;
                    let result = migrate_status(&conn)?;
                    output_result(&result, cli.format);
                }
                MigrateAction::Up => {
                    let _lock = LockGuard::acquire(&cli.dir)?;
                    let mut conn = open_connection(&cli.dir)?;
                    let result = migrate_up_cmd(&mut conn)?;
                    output_migrate_result(&result, cli.format, "up");
                }
                MigrateAction::Down => {
                    let _lock = LockGuard::acquire(&cli.dir)?;
                    let mut conn = open_connection(&cli.dir)?;
                    let result = migrate_down_cmd(&mut conn)?;
                    output_migrate_result(&result, cli.format, "down");
                }
                MigrateAction::All => {
                    let _lock = LockGuard::acquire(&cli.dir)?;
                    let mut conn = open_connection(&cli.dir)?;
                    let result = migrate_all(&mut conn)?;
                    output_migrate_result(&result, cli.format, "all");
                }
            }
            Ok(())
        }

        Commands::Completions { shell } => {
            generate_completions(shell);
            Ok(())
        }

        Commands::ManPages {
            output_dir,
            name,
            list,
        } => {
            generate_man_pages(output_dir.as_deref(), name.as_deref(), list)?;
            Ok(())
        }

        Commands::Loop {
            cmd,
            prd_file,
            prompt_file,
            yes,
            hours,
            verbose,
            no_worktree,
            external_repo,
            cleanup_worktree,
            parallel,
            no_auto_review,
            auto_review,
        } => {
            // Resolve nested-vs-flat into a canonical LoopCommand via the
            // shared helper. Flat-form synthesizes Run and emits a one-line
            // stderr notice; no positional + no subcommand prints help.
            let resolved = match resolve_loop_command(
                cmd,
                prd_file,
                prompt_file,
                yes,
                hours,
                verbose,
                no_worktree,
                external_repo,
                cleanup_worktree,
                parallel,
                no_auto_review,
                auto_review,
            ) {
                LoopResolve::Nested(child) => child,
                LoopResolve::Flat(child) => {
                    eprintln!(
                        "DEPRECATED: prefer `task-mgr loop run ...`. \
                         The flat form will continue to work indefinitely."
                    );
                    child
                }
                LoopResolve::PrintHelp => {
                    use clap::CommandFactory;
                    let mut cmd = Cli::command();
                    let _ = cmd.find_subcommand_mut("loop").map(|s| s.print_help());
                    process::exit(2);
                }
            };

            match resolved {
                LoopCommand::Init {
                    prd_file,
                    force,
                    append,
                    update_existing,
                    dry_run,
                    prefix,
                    no_prefix,
                } => {
                    let prefix_mode =
                        task_mgr::commands::init::PrefixMode::from_cli_flags(no_prefix, prefix);
                    let _lock = LockGuard::acquire(&cli.dir)?;
                    let result = init(
                        &cli.dir,
                        &[prd_file],
                        force,
                        append,
                        update_existing,
                        dry_run,
                        prefix_mode,
                    )?;
                    if cli.verbose {
                        eprint!("{}", format_init_verbose(&result));
                    }
                    output_result(&result, cli.format);
                    Ok(())
                }
                LoopCommand::Run {
                    prd_file,
                    prompt_file,
                    yes,
                    hours,
                    verbose,
                    no_worktree,
                    external_repo,
                    cleanup_worktree,
                    parallel,
                    no_auto_review,
                    auto_review,
                } => {
                    let project_root = get_project_root()?;

                    let mut config = task_mgr::loop_engine::config::LoopConfig::from_env();
                    config.yes_mode = yes;
                    config.hours = hours;
                    config.verbose = verbose || cli.verbose;
                    config.use_worktrees = !no_worktree;
                    config.cleanup_worktree = cleanup_worktree;
                    config.parallel_slots = parallel;

                    // Auto-review hook needs the PRD path after run_loop consumes
                    // run_config; clone before the move.
                    let prd_file_for_review = prd_file.clone();

                    // `cli.dir` is already absolute (resolved in `main()` via
                    // `resolve_db_dir`, which anchors a relative default against
                    // the main repo root when invoked from a worktree). No
                    // further per-arm massaging needed.
                    let run_config = task_mgr::loop_engine::engine::LoopRunConfig {
                        db_dir: cli.dir.clone(),
                        source_root: project_root.clone(),
                        working_root: project_root, // May be updated by run_loop if using worktrees
                        prd_file,
                        prompt_file,
                        config,
                        external_repo,
                        batch_sibling_prds: vec![],
                        chain_base: None,
                        prefix_mode: task_mgr::commands::init::PrefixMode::Auto,
                    };

                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| {
                            TaskMgrError::io_error("tokio runtime", "creating async runtime", e)
                        })?;

                    let loop_result = rt.block_on(async {
                        task_mgr::loop_engine::engine::run_loop(run_config).await
                    });

                    // Auto-review hook — must run AFTER run_loop returns so that
                    // any worktree-cleanup the engine did is reflected in
                    // `loop_result.worktree_path` when `maybe_fire` checks it.
                    // Errors are swallowed inside `maybe_fire`; this can never
                    // change the loop's exit code.
                    let project_config =
                        task_mgr::loop_engine::project_config::read_project_config(&cli.dir);
                    let launcher = task_mgr::loop_engine::auto_review::ProcessLauncher;
                    task_mgr::loop_engine::auto_review::maybe_fire(
                        &project_config,
                        auto_review,
                        no_auto_review,
                        &loop_result,
                        &prd_file_for_review,
                        &launcher,
                    );

                    process::exit(loop_result.exit_code);
                }
            }
        }

        Commands::Status {
            prd_file,
            verbose,
            prefix,
        } => {
            let result = task_mgr::loop_engine::status::show_status(
                &cli.dir,
                prd_file.as_deref(),
                verbose || cli.verbose,
                prefix.as_deref(),
            )?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Batch {
            cmd,
            patterns,
            max_iterations,
            yes,
            keep_worktrees,
            chain,
            parallel,
            no_auto_review,
            auto_review,
        } => {
            // Resolve nested-vs-flat into a canonical BatchCommand via the
            // shared helper. Flat-form (cmd: None, !patterns.is_empty()) is
            // the deprecated shim.
            let resolved = match resolve_batch_command(
                cmd,
                patterns,
                max_iterations,
                yes,
                keep_worktrees,
                chain,
                parallel,
                no_auto_review,
                auto_review,
            ) {
                BatchResolve::Nested(child) => child,
                BatchResolve::Flat(child) => {
                    eprintln!(
                        "DEPRECATED: prefer `task-mgr batch run ...`. \
                         The flat form will continue to work indefinitely."
                    );
                    child
                }
                BatchResolve::PrintHelp => {
                    use clap::CommandFactory;
                    let mut cmd = Cli::command();
                    let _ = cmd.find_subcommand_mut("batch").map(|s| s.print_help());
                    process::exit(2);
                }
            };

            match resolved {
                BatchCommand::Init {
                    patterns,
                    force,
                    append,
                    update_existing,
                    dry_run,
                    prefix,
                    no_prefix,
                } => {
                    let prefix_mode =
                        task_mgr::commands::init::PrefixMode::from_cli_flags(no_prefix, prefix);
                    // Expand glob patterns to concrete PRD file paths.
                    // `expand_patterns` already errors when no files match, so
                    // an empty result is unreachable here. Keeping this arm a
                    // thin shell around `init()` keeps the PRD-import path in
                    // one place (commands::init::init).
                    let paths = task_mgr::loop_engine::batch::expand_patterns(&patterns)?;
                    let _lock = LockGuard::acquire(&cli.dir)?;
                    let result = init(
                        &cli.dir,
                        &paths,
                        force,
                        append,
                        update_existing,
                        dry_run,
                        prefix_mode,
                    )?;
                    if cli.verbose {
                        eprint!("{}", format_init_verbose(&result));
                    }
                    output_result(&result, cli.format);
                    Ok(())
                }
                BatchCommand::Run {
                    patterns,
                    max_iterations,
                    yes,
                    keep_worktrees,
                    chain,
                    parallel,
                    no_auto_review,
                    auto_review,
                } => {
                    let project_root = get_project_root()?;

                    // `cli.dir` is already absolute (resolved in `main()`).
                    let db_dir = cli.dir.clone();

                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| {
                            TaskMgrError::io_error("tokio runtime", "creating async runtime", e)
                        })?;

                    let result = rt.block_on(async {
                        task_mgr::loop_engine::batch::run_batch(
                            &patterns,
                            max_iterations,
                            yes,
                            &db_dir,
                            &project_root,
                            cli.verbose,
                            keep_worktrees,
                            chain,
                            parallel,
                            auto_review,
                            no_auto_review,
                        )
                        .await
                    });

                    // Exit with code 1 if any PRDs failed, 0 otherwise
                    let exit_code = if result.failed > 0 { 1 } else { 0 };
                    process::exit(exit_code);
                }
            }
        }

        Commands::ImportLearnings {
            from_json,
            reset_stats,
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let result = import_learnings(&cli.dir, &from_json, reset_stats)?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Archive {
            dry_run,
            all,
            branch,
        } => {
            let branch_filter = if all {
                None
            } else if let Some(b) = branch {
                Some(b)
            } else {
                Some(task_mgr::loop_engine::env::get_current_branch(&cli.dir)?)
            };
            let result = task_mgr::loop_engine::archive::run_archive(
                &cli.dir,
                dry_run,
                branch_filter.as_deref(),
            )?;
            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Enhance { cmd } => {
            use task_mgr::commands::{
                AgentsParams, ShowParams, StripParams, enhance_agents, enhance_show, enhance_strip,
            };
            let cwd = std::env::current_dir().map_err(|e| {
                TaskMgrError::io_error(".", "resolving current working directory", e)
            })?;
            let result = match cmd {
                EnhanceCommand::Agents {
                    target,
                    dry_run,
                    create,
                    profile,
                } => enhance_agents(AgentsParams {
                    targets: target,
                    dry_run,
                    create,
                    profile,
                    cwd,
                })?,
                EnhanceCommand::Show { profile } => enhance_show(ShowParams { profile })?,
                EnhanceCommand::Strip { target, dry_run } => enhance_strip(StripParams {
                    targets: target,
                    dry_run,
                    cwd,
                })?,
            };
            output_result(&result, cli.format);
            if !result.dry_run && !result.any_success() {
                process::exit(2);
            }
            Ok(())
        }

        Commands::Models { action } => {
            use task_mgr::commands::models::{
                ListOpts, SetDefaultOpts, UnsetDefaultOpts, handle_list, handle_set_default,
                handle_show, handle_unset_default,
            };
            match action {
                ModelsAction::List { remote, refresh } => {
                    handle_list(&cli.dir, ListOpts { remote, refresh })?;
                }
                ModelsAction::SetDefault { model, project } => {
                    handle_set_default(&cli.dir, SetDefaultOpts { model, project })?;
                }
                ModelsAction::UnsetDefault { project } => {
                    handle_unset_default(&cli.dir, UnsetDefaultOpts { project })?;
                }
                ModelsAction::Show => {
                    handle_show(&cli.dir, resolved_db_dir.source)?;
                }
            }
            Ok(())
        }

        Commands::Worktrees { action } => {
            let project_root = get_project_root()?;
            match action {
                WorktreesAction::List => {
                    let result = worktrees_list(&cli.dir, &project_root)?;
                    output_result(&result, cli.format);
                }
                WorktreesAction::Prune => {
                    let result = worktrees_prune(&cli.dir, &project_root)?;
                    output_result(&result, cli.format);
                }
                WorktreesAction::Remove { target } => {
                    let result = worktrees_remove(&cli.dir, &project_root, &target)?;
                    output_result(&result, cli.format);
                }
            }
            Ok(())
        }

        Commands::Curate { action } => {
            use task_mgr::commands::curate::enrich::curate_enrich;
            use task_mgr::commands::curate::{
                DedupParams, EmbedParams, EnrichParams, RetireParams, curate_count, curate_dedup,
                curate_embed, curate_retire, curate_unretire,
            };
            use task_mgr::learnings::embeddings::{DEFAULT_EMBEDDING_MODEL, DEFAULT_OLLAMA_URL};
            use task_mgr::loop_engine::project_config::read_project_config;
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;
            match action {
                CurateAction::Retire {
                    dry_run,
                    min_age_days,
                    min_shows,
                    max_rate,
                } => {
                    let params = RetireParams {
                        dry_run,
                        min_age_days,
                        min_shows,
                        max_rate,
                    };
                    let result = curate_retire(&conn, params)?;
                    output_result(&result, cli.format);
                }
                CurateAction::Unretire { learning_ids } => {
                    let result = curate_unretire(&conn, learning_ids)?;
                    output_result(&result, cli.format);
                }
                CurateAction::Enrich {
                    dry_run,
                    batch_size,
                    field,
                } => {
                    let field_filter = field
                        .map(|s| {
                            let id = s.clone();
                            s.parse().map_err(|e: String| TaskMgrError::InvalidState {
                                resource_type: "curate enrich --field".to_string(),
                                id,
                                expected:
                                    "applies_to_files, applies_to_task_types, or applies_to_errors"
                                        .to_string(),
                                actual: e,
                            })
                        })
                        .transpose()?;
                    let params = EnrichParams {
                        dry_run,
                        batch_size,
                        field_filter,
                    };
                    let result = curate_enrich(&conn, params)?;
                    output_result(&result, cli.format);
                }
                CurateAction::Dedup {
                    dry_run,
                    threshold,
                    batch_size,
                    concurrency,
                    reset_dismissals,
                    pair_mode,
                } => {
                    let proj_config = read_project_config(&cli.dir);
                    let embed_model = proj_config
                        .embedding_model
                        .unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string());
                    let dedup_model = proj_config.dedup_model.unwrap_or_else(|| {
                        task_mgr::commands::curate::types::DEFAULT_DEDUP_MODEL.to_string()
                    });
                    let params = DedupParams {
                        dry_run,
                        threshold,
                        batch_size,
                        concurrency,
                        embed_model,
                        model: dedup_model,
                        db_dir: Some(cli.dir.clone()),
                        reset_dismissals,
                        pair_mode,
                    };
                    let result = curate_dedup(&conn, params)?;
                    output_result(&result, cli.format);
                }
                CurateAction::Count => {
                    let result = curate_count(&conn)?;
                    output_result(&result, cli.format);
                }
                CurateAction::Embed { force, status } => {
                    let proj_config = read_project_config(&cli.dir);
                    let ollama_url = proj_config
                        .ollama_url
                        .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string());
                    let model = proj_config
                        .embedding_model
                        .unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string());
                    let params = EmbedParams {
                        force,
                        status,
                        ollama_url,
                        model,
                    };
                    let result = curate_embed(&conn, params)?;
                    output_result(&result, cli.format);
                }
            }
            Ok(())
        }

        Commands::ExtractLearnings {
            from_output,
            task_id,
            run_id,
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;

            let output = std::fs::read_to_string(&from_output).map_err(|e| {
                TaskMgrError::io_error(from_output.display().to_string(), "reading output file", e)
            })?;

            let result = task_mgr::learnings::ingestion::extract_learnings_from_output(
                &conn,
                &output,
                task_id.as_deref(),
                run_id.as_deref(),
                Some(&cli.dir),
                None, // CLI invocation — no shared signal flag; Ctrl-C kills this process directly
            )?;

            if result.learnings_extracted > 0 {
                println!(
                    "Extracted {} learning(s) with IDs: {:?}",
                    result.learnings_extracted, result.learning_ids
                );
            } else {
                println!("No learnings extracted from output.");
            }
            Ok(())
        }
    }
}
