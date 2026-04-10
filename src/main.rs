//! Task Manager CLI entry point.
//!
//! This module handles argument parsing and command dispatch.
//! Output formatting and helper functions are in the `handlers` module.

use std::path::PathBuf;
use std::process;

use clap::Parser;

use task_mgr::cli::{
    Cli, Commands, CurateAction, DecisionAction, MigrateAction, RunAction, WorktreesAction,
};
use task_mgr::commands::{
    apply_learning, auto_unblock_all, begin, complete, count_resettable_tasks,
    decline_decision_cmd, doctor, end, export, fail, format_doctor_verbose, format_init_verbose,
    format_next_verbose, format_recall_verbose, get_reviewable_tasks, history, history_detail,
    import_learnings, init, invalidate_learning, irrelevant, learn, list, list_decisions,
    list_learnings, migrate_all, migrate_down_cmd, migrate_status, migrate_up_cmd, next, recall,
    reset_all_tasks, reset_tasks, resolve_decision_cmd, revert_decision_cmd, show, skip, stats,
    unblock, unskip, update, worktrees_list, worktrees_prune, worktrees_remove, LearnParams,
    LearningsListParams, RecallCmdParams, ReviewOptions,
};
use task_mgr::db::{open_connection, LockGuard};
use task_mgr::handlers::{
    convert_run_end_status, generate_completions, generate_man_pages, output_migrate_result,
    output_result,
};
use task_mgr::learnings::{delete_learning, edit_learning, EditLearningParams};
use task_mgr::TaskMgrError;

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

fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), TaskMgrError> {
    match cli.command {
        Commands::Init {
            from_json,
            force,
            append,
            update_existing,
            dry_run,
            prefix,
            no_prefix,
        } => {
            let prefix_mode = if no_prefix {
                task_mgr::commands::init::PrefixMode::Disabled
            } else if let Some(p) = prefix {
                task_mgr::commands::init::PrefixMode::Explicit(p)
            } else {
                task_mgr::commands::init::PrefixMode::Auto
            };
            let _lock = LockGuard::acquire(&cli.dir)?;
            let result = init(
                &cli.dir,
                &from_json,
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

        Commands::List {
            status,
            file,
            task_type,
            include_archived,
        } => {
            let result = list(
                &cli.dir,
                status,
                file.as_deref(),
                task_type.as_deref(),
                include_archived,
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
        } => {
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
        } => {
            let _lock = LockGuard::acquire(&cli.dir)?;
            let conn = open_connection(&cli.dir)?;

            // Capture for post-insert embedding (title/content are moved into params).
            let embed_title = title.clone();
            let embed_content = content.clone();

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
            };

            let result = learn(&conn, params)?;

            // Best-effort: embed the new learning if Ollama is available.
            task_mgr::learnings::embeddings::try_embed_learning(
                &conn,
                &cli.dir,
                result.learning_id,
                &embed_title,
                &embed_content,
            );

            output_result(&result, cli.format);
            Ok(())
        }

        Commands::Recall {
            query,
            for_task,
            tags,
            outcome,
            limit,
        } => {
            use task_mgr::loop_engine::project_config::read_project_config;

            let conn = open_connection(&cli.dir)?;
            let proj_config = read_project_config(&cli.dir);

            let params = RecallCmdParams {
                query,
                for_task,
                tags,
                outcome,
                limit,
                ollama_url: proj_config.ollama_url,
                embedding_model: proj_config.embedding_model,
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
            prd_file,
            prompt_file,
            yes,
            hours,
            verbose,
            no_worktree,
            external_repo,
            cleanup_worktree,
        } => {
            let project_root = get_project_root()?;

            let mut config = task_mgr::loop_engine::config::LoopConfig::from_env();
            config.yes_mode = yes;
            config.hours = hours;
            config.verbose = verbose || cli.verbose;
            config.use_worktrees = !no_worktree;
            config.cleanup_worktree = cleanup_worktree;

            // Anchor db_dir to source_root so worktrees don't create a separate DB.
            // If the user passed an absolute --dir, respect it; otherwise resolve
            // the relative default (".task-mgr") against the project root.
            let db_dir = if cli.dir.is_relative() {
                project_root.join(&cli.dir)
            } else {
                cli.dir.clone()
            };

            let run_config = task_mgr::loop_engine::engine::LoopRunConfig {
                db_dir,
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

            let result =
                rt.block_on(async { task_mgr::loop_engine::engine::run_loop(run_config).await });

            process::exit(result.exit_code);
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
            patterns,
            max_iterations,
            yes,
            keep_worktrees,
            chain,
        } => {
            let project_root = get_project_root()?;

            // Anchor db_dir to source_root so worktrees don't create a separate DB.
            // Same resolution as the `loop` command.
            let db_dir = if cli.dir.is_relative() {
                project_root.join(&cli.dir)
            } else {
                cli.dir.clone()
            };

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
                )
                .await
            });

            // Exit with code 1 if any PRDs failed, 0 otherwise
            let exit_code = if result.failed > 0 { 1 } else { 0 };
            process::exit(exit_code);
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
                curate_dedup, curate_embed, curate_retire, curate_unretire, DedupParams,
                EmbedParams, EnrichParams, RetireParams,
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
                    };
                    let result = curate_dedup(&conn, params)?;
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
