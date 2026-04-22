//! CLI tests for task-mgr.
//!
//! This module contains all unit tests for CLI argument parsing,
//! verifying that commands are parsed correctly with all flag combinations.

use std::path::PathBuf;

use clap::{CommandFactory, Parser};

use super::Cli;
use super::commands::{Commands, MigrateAction, RunAction};
use super::enums::{
    Confidence, FailStatus, LearningOutcome, OutputFormat, RunEndStatus, Shell, TaskStatusFilter,
};

#[test]
fn verify_cli() {
    // Verify that the CLI can be parsed correctly
    Cli::command().debug_assert();
}

#[test]
fn test_default_dir() {
    // Clap's `env = "TASK_MGR_DIR"` attribute means an ambient env var (e.g.
    // set by an outer `task-mgr loop` shell) will shadow the default. Remove
    // it for this test to assert the compile-time default specifically.
    unsafe { std::env::remove_var("TASK_MGR_DIR") };
    let cli = Cli::parse_from(["task-mgr", "list"]);
    assert_eq!(cli.dir, PathBuf::from(".task-mgr"));
}

#[test]
fn test_custom_dir() {
    let cli = Cli::parse_from(["task-mgr", "--dir", "/custom/path", "list"]);
    assert_eq!(cli.dir, PathBuf::from("/custom/path"));
}

#[test]
fn test_default_format() {
    let cli = Cli::parse_from(["task-mgr", "list"]);
    assert_eq!(cli.format, OutputFormat::Text);
}

#[test]
fn test_json_format() {
    let cli = Cli::parse_from(["task-mgr", "--format", "json", "list"]);
    assert_eq!(cli.format, OutputFormat::Json);
}

// Init command tests
#[test]
fn test_init_with_from_json() {
    let cli = Cli::parse_from(["task-mgr", "init", "--from-json", "prd.json"]);
    match cli.command {
        Commands::Init {
            from_json,
            force,
            append,
            update_existing,
            dry_run,
            ..
        } => {
            assert_eq!(from_json, vec![PathBuf::from("prd.json")]);
            assert!(!force);
            assert!(!append);
            assert!(!update_existing);
            assert!(!dry_run);
        }
        _ => panic!("Expected Init command"),
    }
}

#[test]
fn test_init_with_force() {
    let cli = Cli::parse_from(["task-mgr", "init", "--from-json", "prd.json", "--force"]);
    match cli.command {
        Commands::Init {
            from_json,
            force,
            append,
            update_existing,
            dry_run,
            ..
        } => {
            assert_eq!(from_json, vec![PathBuf::from("prd.json")]);
            assert!(force);
            assert!(!append);
            assert!(!update_existing);
            assert!(!dry_run);
        }
        _ => panic!("Expected Init command"),
    }
}

#[test]
fn test_init_with_append() {
    let cli = Cli::parse_from(["task-mgr", "init", "--from-json", "prd.json", "--append"]);
    match cli.command {
        Commands::Init {
            from_json,
            force,
            append,
            update_existing,
            dry_run,
            ..
        } => {
            assert_eq!(from_json, vec![PathBuf::from("prd.json")]);
            assert!(!force);
            assert!(append);
            assert!(!update_existing);
            assert!(!dry_run);
        }
        _ => panic!("Expected Init command"),
    }
}

#[test]
fn test_init_with_append_and_update_existing() {
    let cli = Cli::parse_from([
        "task-mgr",
        "init",
        "--from-json",
        "prd.json",
        "--append",
        "--update-existing",
    ]);
    match cli.command {
        Commands::Init {
            from_json,
            force,
            append,
            update_existing,
            dry_run,
            ..
        } => {
            assert_eq!(from_json, vec![PathBuf::from("prd.json")]);
            assert!(!force);
            assert!(append);
            assert!(update_existing);
            assert!(!dry_run);
        }
        _ => panic!("Expected Init command"),
    }
}

#[test]
fn test_init_multiple_files() {
    let cli = Cli::parse_from([
        "task-mgr",
        "init",
        "--from-json",
        "p1.json",
        "--from-json",
        "p2.json",
    ]);
    match cli.command {
        Commands::Init {
            from_json,
            force,
            append,
            update_existing,
            dry_run,
            ..
        } => {
            assert_eq!(
                from_json,
                vec![PathBuf::from("p1.json"), PathBuf::from("p2.json")]
            );
            assert!(!force);
            assert!(!append);
            assert!(!update_existing);
            assert!(!dry_run);
        }
        _ => panic!("Expected Init command"),
    }
}

#[test]
fn test_init_with_dry_run() {
    let cli = Cli::parse_from([
        "task-mgr",
        "init",
        "--from-json",
        "prd.json",
        "--force",
        "--dry-run",
    ]);
    match cli.command {
        Commands::Init {
            from_json,
            force,
            append,
            update_existing,
            dry_run,
            ..
        } => {
            assert_eq!(from_json, vec![PathBuf::from("prd.json")]);
            assert!(force);
            assert!(!append);
            assert!(!update_existing);
            assert!(dry_run);
        }
        _ => panic!("Expected Init command"),
    }
}

// List command tests
#[test]
fn test_list_no_filters() {
    let cli = Cli::parse_from(["task-mgr", "list"]);
    match cli.command {
        Commands::List {
            status,
            file,
            task_type,
            ..
        } => {
            assert!(status.is_none());
            assert!(file.is_none());
            assert!(task_type.is_none());
        }
        _ => panic!("Expected List command"),
    }
}

#[test]
fn test_list_with_status_filter() {
    let cli = Cli::parse_from(["task-mgr", "list", "--status", "todo"]);
    match cli.command {
        Commands::List { status, .. } => {
            assert_eq!(status, Some(TaskStatusFilter::Todo));
        }
        _ => panic!("Expected List command"),
    }
}

#[test]
fn test_list_with_file_filter() {
    let cli = Cli::parse_from(["task-mgr", "list", "--file", "src/*.rs"]);
    match cli.command {
        Commands::List { file, .. } => {
            assert_eq!(file, Some("src/*.rs".to_string()));
        }
        _ => panic!("Expected List command"),
    }
}

#[test]
fn test_list_with_task_type_filter() {
    let cli = Cli::parse_from(["task-mgr", "list", "--task-type", "US-"]);
    match cli.command {
        Commands::List { task_type, .. } => {
            assert_eq!(task_type, Some("US-".to_string()));
        }
        _ => panic!("Expected List command"),
    }
}

// Show command tests
#[test]
fn test_show() {
    let cli = Cli::parse_from(["task-mgr", "show", "US-001"]);
    match cli.command {
        Commands::Show { task_id } => {
            assert_eq!(task_id, "US-001");
        }
        _ => panic!("Expected Show command"),
    }
}

// Next command tests
#[test]
fn test_next_no_flags() {
    let cli = Cli::parse_from(["task-mgr", "next"]);
    match cli.command {
        Commands::Next {
            after_files,
            claim,
            run_id,
            decay_threshold,
            prefix,
            parallel,
        } => {
            assert!(after_files.is_none());
            assert!(!claim);
            assert!(run_id.is_none());
            assert_eq!(decay_threshold, 32);
            assert!(prefix.is_none());
            assert_eq!(parallel, 1);
        }
        _ => panic!("Expected Next command"),
    }
}

#[test]
fn test_next_with_after_files() {
    let cli = Cli::parse_from(["task-mgr", "next", "--after-files", "a.rs,b.rs"]);
    match cli.command {
        Commands::Next { after_files, .. } => {
            assert_eq!(
                after_files,
                Some(vec!["a.rs".to_string(), "b.rs".to_string()])
            );
        }
        _ => panic!("Expected Next command"),
    }
}

#[test]
fn test_next_with_claim() {
    let cli = Cli::parse_from(["task-mgr", "next", "--claim"]);
    match cli.command {
        Commands::Next { claim, .. } => {
            assert!(claim);
        }
        _ => panic!("Expected Next command"),
    }
}

#[test]
fn test_next_with_run_id() {
    let cli = Cli::parse_from(["task-mgr", "next", "--run-id", "abc-123"]);
    match cli.command {
        Commands::Next { run_id, .. } => {
            assert_eq!(run_id, Some("abc-123".to_string()));
        }
        _ => panic!("Expected Next command"),
    }
}

#[test]
fn test_next_with_prefix() {
    let cli = Cli::parse_from(["task-mgr", "next", "--prefix", "P1"]);
    match cli.command {
        Commands::Next { prefix, .. } => {
            assert_eq!(prefix, Some("P1".to_string()));
        }
        _ => panic!("Expected Next command"),
    }
}

// Complete command tests
#[test]
fn test_complete_single_task() {
    let cli = Cli::parse_from(["task-mgr", "complete", "US-001"]);
    match cli.command {
        Commands::Complete {
            task_ids,
            run_id,
            commit,
            force,
        } => {
            assert_eq!(task_ids, vec!["US-001"]);
            assert!(run_id.is_none());
            assert!(commit.is_none());
            assert!(!force); // Default is false
        }
        _ => panic!("Expected Complete command"),
    }
}

#[test]
fn test_complete_multiple_tasks() {
    let cli = Cli::parse_from(["task-mgr", "complete", "US-001", "US-002", "US-003"]);
    match cli.command {
        Commands::Complete { task_ids, .. } => {
            assert_eq!(task_ids, vec!["US-001", "US-002", "US-003"]);
        }
        _ => panic!("Expected Complete command"),
    }
}

#[test]
fn test_complete_with_run_id_and_commit() {
    let cli = Cli::parse_from([
        "task-mgr", "complete", "US-001", "--run-id", "run-123", "--commit", "abc123",
    ]);
    match cli.command {
        Commands::Complete {
            task_ids,
            run_id,
            commit,
            ..
        } => {
            assert_eq!(task_ids, vec!["US-001"]);
            assert_eq!(run_id, Some("run-123".to_string()));
            assert_eq!(commit, Some("abc123".to_string()));
        }
        _ => panic!("Expected Complete command"),
    }
}

#[test]
fn test_complete_with_force() {
    let cli = Cli::parse_from(["task-mgr", "complete", "US-001", "--force"]);
    match cli.command {
        Commands::Complete { force, .. } => {
            assert!(force);
        }
        _ => panic!("Expected Complete command"),
    }
}

// Fail command tests
#[test]
fn test_fail_default_status() {
    let cli = Cli::parse_from(["task-mgr", "fail", "US-001"]);
    match cli.command {
        Commands::Fail {
            task_ids,
            run_id,
            error,
            status,
            force,
        } => {
            assert_eq!(task_ids, vec!["US-001".to_string()]);
            assert!(run_id.is_none());
            assert!(error.is_none());
            assert_eq!(status, FailStatus::Blocked);
            assert!(!force); // Default is false
        }
        _ => panic!("Expected Fail command"),
    }
}

#[test]
fn test_fail_with_force() {
    let cli = Cli::parse_from(["task-mgr", "fail", "US-001", "--force"]);
    match cli.command {
        Commands::Fail { force, .. } => {
            assert!(force);
        }
        _ => panic!("Expected Fail command"),
    }
}

#[test]
fn test_fail_with_error() {
    let cli = Cli::parse_from([
        "task-mgr",
        "fail",
        "US-001",
        "--error",
        "Missing dependency",
    ]);
    match cli.command {
        Commands::Fail { error, .. } => {
            assert_eq!(error, Some("Missing dependency".to_string()));
        }
        _ => panic!("Expected Fail command"),
    }
}

#[test]
fn test_fail_with_skipped_status() {
    let cli = Cli::parse_from(["task-mgr", "fail", "US-001", "--status", "skipped"]);
    match cli.command {
        Commands::Fail { status, .. } => {
            assert_eq!(status, FailStatus::Skipped);
        }
        _ => panic!("Expected Fail command"),
    }
}

#[test]
fn test_fail_with_irrelevant_status() {
    let cli = Cli::parse_from(["task-mgr", "fail", "US-001", "--status", "irrelevant"]);
    match cli.command {
        Commands::Fail { status, .. } => {
            assert_eq!(status, FailStatus::Irrelevant);
        }
        _ => panic!("Expected Fail command"),
    }
}

#[test]
fn test_fail_with_all_options() {
    let cli = Cli::parse_from([
        "task-mgr",
        "fail",
        "US-001",
        "--run-id",
        "run-456",
        "--error",
        "Blocked by external issue",
        "--status",
        "blocked",
    ]);
    match cli.command {
        Commands::Fail {
            task_ids,
            run_id,
            error,
            status,
            ..
        } => {
            assert_eq!(task_ids, vec!["US-001".to_string()]);
            assert_eq!(run_id, Some("run-456".to_string()));
            assert_eq!(error, Some("Blocked by external issue".to_string()));
            assert_eq!(status, FailStatus::Blocked);
        }
        _ => panic!("Expected Fail command"),
    }
}

#[test]
fn test_fail_multiple_tasks() {
    let cli = Cli::parse_from([
        "task-mgr",
        "fail",
        "US-001",
        "US-002",
        "US-003",
        "--error",
        "Batch failure",
    ]);
    match cli.command {
        Commands::Fail {
            task_ids, error, ..
        } => {
            assert_eq!(
                task_ids,
                vec![
                    "US-001".to_string(),
                    "US-002".to_string(),
                    "US-003".to_string()
                ]
            );
            assert_eq!(error, Some("Batch failure".to_string()));
        }
        _ => panic!("Expected Fail command"),
    }
}

// Run command tests
#[test]
fn test_run_begin() {
    let cli = Cli::parse_from(["task-mgr", "run", "begin"]);
    match cli.command {
        Commands::Run { action } => {
            assert!(matches!(action, RunAction::Begin));
        }
        _ => panic!("Expected Run command"),
    }
}

#[test]
fn test_run_update_with_run_id() {
    let cli = Cli::parse_from(["task-mgr", "run", "update", "--run-id", "run-123"]);
    match cli.command {
        Commands::Run { action } => match action {
            RunAction::Update {
                run_id,
                last_commit,
                last_files,
            } => {
                assert_eq!(run_id, "run-123");
                assert!(last_commit.is_none());
                assert!(last_files.is_none());
            }
            _ => panic!("Expected Update action"),
        },
        _ => panic!("Expected Run command"),
    }
}

#[test]
fn test_run_update_with_all_options() {
    let cli = Cli::parse_from([
        "task-mgr",
        "run",
        "update",
        "--run-id",
        "run-456",
        "--last-commit",
        "abc123def",
        "--last-files",
        "src/main.rs,src/lib.rs",
    ]);
    match cli.command {
        Commands::Run { action } => match action {
            RunAction::Update {
                run_id,
                last_commit,
                last_files,
            } => {
                assert_eq!(run_id, "run-456");
                assert_eq!(last_commit, Some("abc123def".to_string()));
                assert_eq!(
                    last_files,
                    Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()])
                );
            }
            _ => panic!("Expected Update action"),
        },
        _ => panic!("Expected Run command"),
    }
}

#[test]
fn test_run_end_completed() {
    let cli = Cli::parse_from([
        "task-mgr",
        "run",
        "end",
        "--run-id",
        "run-789",
        "--status",
        "completed",
    ]);
    match cli.command {
        Commands::Run { action } => match action {
            RunAction::End { run_id, status } => {
                assert_eq!(run_id, "run-789");
                assert_eq!(status, RunEndStatus::Completed);
            }
            _ => panic!("Expected End action"),
        },
        _ => panic!("Expected Run command"),
    }
}

#[test]
fn test_run_end_aborted() {
    let cli = Cli::parse_from([
        "task-mgr", "run", "end", "--run-id", "run-999", "--status", "aborted",
    ]);
    match cli.command {
        Commands::Run { action } => match action {
            RunAction::End { run_id, status } => {
                assert_eq!(run_id, "run-999");
                assert_eq!(status, RunEndStatus::Aborted);
            }
            _ => panic!("Expected End action"),
        },
        _ => panic!("Expected Run command"),
    }
}

// Export command tests
#[test]
fn test_export_basic() {
    let cli = Cli::parse_from(["task-mgr", "export", "--to-json", "output.json"]);
    match cli.command {
        Commands::Export {
            to_json,
            with_progress,
            learnings_file,
        } => {
            assert_eq!(to_json, PathBuf::from("output.json"));
            assert!(!with_progress);
            assert!(learnings_file.is_none());
        }
        _ => panic!("Expected Export command"),
    }
}

#[test]
fn test_export_with_progress() {
    let cli = Cli::parse_from([
        "task-mgr",
        "export",
        "--to-json",
        "output.json",
        "--with-progress",
    ]);
    match cli.command {
        Commands::Export {
            to_json,
            with_progress,
            learnings_file,
        } => {
            assert_eq!(to_json, PathBuf::from("output.json"));
            assert!(with_progress);
            assert!(learnings_file.is_none());
        }
        _ => panic!("Expected Export command"),
    }
}

#[test]
fn test_export_with_learnings_file() {
    let cli = Cli::parse_from([
        "task-mgr",
        "export",
        "--to-json",
        "output.json",
        "--learnings-file",
        "learnings.json",
    ]);
    match cli.command {
        Commands::Export {
            to_json,
            with_progress,
            learnings_file,
        } => {
            assert_eq!(to_json, PathBuf::from("output.json"));
            assert!(!with_progress);
            assert_eq!(learnings_file, Some(PathBuf::from("learnings.json")));
        }
        _ => panic!("Expected Export command"),
    }
}

#[test]
fn test_export_with_all_options() {
    let cli = Cli::parse_from([
        "task-mgr",
        "export",
        "--to-json",
        "prd.json",
        "--with-progress",
        "--learnings-file",
        "learnings.json",
    ]);
    match cli.command {
        Commands::Export {
            to_json,
            with_progress,
            learnings_file,
        } => {
            assert_eq!(to_json, PathBuf::from("prd.json"));
            assert!(with_progress);
            assert_eq!(learnings_file, Some(PathBuf::from("learnings.json")));
        }
        _ => panic!("Expected Export command"),
    }
}

// Doctor command tests
#[test]
fn test_doctor_no_flags() {
    let cli = Cli::parse_from(["task-mgr", "doctor"]);
    match cli.command {
        Commands::Doctor {
            auto_fix,
            dry_run,
            decay_threshold,
            reconcile_git,
            setup,
        } => {
            assert!(!auto_fix);
            assert!(!dry_run);
            assert_eq!(decay_threshold, 32);
            assert!(!reconcile_git);
            assert!(!setup);
        }
        _ => panic!("Expected Doctor command"),
    }
}

#[test]
fn test_doctor_with_auto_fix() {
    let cli = Cli::parse_from(["task-mgr", "doctor", "--auto-fix"]);
    match cli.command {
        Commands::Doctor {
            auto_fix,
            dry_run,
            decay_threshold,
            reconcile_git,
            setup,
        } => {
            assert!(auto_fix);
            assert!(!dry_run);
            assert_eq!(decay_threshold, 32);
            assert!(!reconcile_git);
            assert!(!setup);
        }
        _ => panic!("Expected Doctor command"),
    }
}

#[test]
fn test_doctor_with_dry_run() {
    let cli = Cli::parse_from(["task-mgr", "doctor", "--dry-run"]);
    match cli.command {
        Commands::Doctor {
            auto_fix,
            dry_run,
            decay_threshold,
            reconcile_git,
            setup,
        } => {
            assert!(!auto_fix);
            assert!(dry_run);
            assert_eq!(decay_threshold, 32);
            assert!(!reconcile_git);
            assert!(!setup);
        }
        _ => panic!("Expected Doctor command"),
    }
}

#[test]
fn test_doctor_with_auto_fix_and_dry_run() {
    let cli = Cli::parse_from(["task-mgr", "doctor", "--auto-fix", "--dry-run"]);
    match cli.command {
        Commands::Doctor {
            auto_fix,
            dry_run,
            decay_threshold,
            reconcile_git,
            setup,
        } => {
            assert!(auto_fix);
            assert!(dry_run);
            assert_eq!(decay_threshold, 32);
            assert!(!reconcile_git);
            assert!(!setup);
        }
        _ => panic!("Expected Doctor command"),
    }
}

#[test]
fn test_doctor_with_reconcile_git() {
    let cli = Cli::parse_from(["task-mgr", "doctor", "--reconcile-git", "--auto-fix"]);
    match cli.command {
        Commands::Doctor {
            auto_fix,
            dry_run,
            decay_threshold,
            reconcile_git,
            setup,
        } => {
            assert!(auto_fix);
            assert!(!dry_run);
            assert_eq!(decay_threshold, 32);
            assert!(reconcile_git);
            assert!(!setup);
        }
        _ => panic!("Expected Doctor command"),
    }
}

#[test]
fn test_doctor_with_setup_flag() {
    let cli = Cli::parse_from(["task-mgr", "doctor", "--setup"]);
    match cli.command {
        Commands::Doctor {
            auto_fix,
            dry_run,
            decay_threshold,
            reconcile_git,
            setup,
        } => {
            assert!(!auto_fix);
            assert!(!dry_run);
            assert_eq!(decay_threshold, 32);
            assert!(!reconcile_git);
            assert!(setup);
        }
        _ => panic!("Expected Doctor command"),
    }
}

// Learn command tests
#[test]
fn test_learn_minimal() {
    let cli = Cli::parse_from([
        "task-mgr",
        "learn",
        "--outcome",
        "failure",
        "--title",
        "Test learning",
        "--content",
        "This is the content",
    ]);
    match cli.command {
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
            assert_eq!(outcome, LearningOutcome::Failure);
            assert_eq!(title, "Test learning");
            assert_eq!(content, "This is the content");
            assert!(task_id.is_none());
            assert!(run_id.is_none());
            assert!(root_cause.is_none());
            assert!(solution.is_none());
            assert!(files.is_none());
            assert!(task_types.is_none());
            assert!(errors.is_none());
            assert!(tags.is_none());
            assert_eq!(confidence, Confidence::Medium); // default
            assert!(supersedes.is_none());
        }
        _ => panic!("Expected Learn command"),
    }
}

#[test]
fn test_learn_with_all_options() {
    let cli = Cli::parse_from([
        "task-mgr",
        "learn",
        "--outcome",
        "success",
        "--title",
        "Learned pattern",
        "--content",
        "Pattern details",
        "--task-id",
        "US-001",
        "--run-id",
        "run-123",
        "--root-cause",
        "Missing dependency",
        "--solution",
        "Added the dep",
        "--files",
        "src/main.rs,src/lib.rs",
        "--task-types",
        "US-,FIX-",
        "--errors",
        "E0001,E0002",
        "--tags",
        "rust,cli",
        "--confidence",
        "high",
    ]);
    match cli.command {
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
            assert_eq!(outcome, LearningOutcome::Success);
            assert_eq!(title, "Learned pattern");
            assert_eq!(content, "Pattern details");
            assert_eq!(task_id, Some("US-001".to_string()));
            assert_eq!(run_id, Some("run-123".to_string()));
            assert_eq!(root_cause, Some("Missing dependency".to_string()));
            assert_eq!(solution, Some("Added the dep".to_string()));
            assert_eq!(
                files,
                Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()])
            );
            assert_eq!(
                task_types,
                Some(vec!["US-".to_string(), "FIX-".to_string()])
            );
            assert_eq!(errors, Some(vec!["E0001".to_string(), "E0002".to_string()]));
            assert_eq!(tags, Some(vec!["rust".to_string(), "cli".to_string()]));
            assert_eq!(confidence, Confidence::High);
            assert!(supersedes.is_none());
        }
        _ => panic!("Expected Learn command"),
    }
}

#[test]
fn test_learn_outcome_variants() {
    for (outcome_str, expected) in [
        ("failure", LearningOutcome::Failure),
        ("success", LearningOutcome::Success),
        ("workaround", LearningOutcome::Workaround),
        ("pattern", LearningOutcome::Pattern),
    ] {
        let cli = Cli::parse_from([
            "task-mgr",
            "learn",
            "--outcome",
            outcome_str,
            "--title",
            "t",
            "--content",
            "c",
        ]);
        match cli.command {
            Commands::Learn { outcome, .. } => {
                assert_eq!(outcome, expected);
            }
            _ => panic!("Expected Learn command"),
        }
    }
}

#[test]
fn test_learn_confidence_variants() {
    for (conf_str, expected) in [
        ("high", Confidence::High),
        ("medium", Confidence::Medium),
        ("low", Confidence::Low),
    ] {
        let cli = Cli::parse_from([
            "task-mgr",
            "learn",
            "--outcome",
            "pattern",
            "--title",
            "t",
            "--content",
            "c",
            "--confidence",
            conf_str,
        ]);
        match cli.command {
            Commands::Learn { confidence, .. } => {
                assert_eq!(confidence, expected);
            }
            _ => panic!("Expected Learn command"),
        }
    }
}

#[test]
fn test_learn_with_supersedes() {
    let cli = Cli::parse_from([
        "task-mgr",
        "learn",
        "--outcome",
        "pattern",
        "--title",
        "Replacement",
        "--content",
        "Content",
        "--supersedes",
        "42",
    ]);
    match cli.command {
        Commands::Learn { supersedes, .. } => {
            assert_eq!(supersedes, Some(42));
        }
        _ => panic!("Expected Learn command"),
    }
}

// Recall command tests
#[test]
fn test_recall_minimal() {
    let cli = Cli::parse_from(["task-mgr", "recall"]);
    match cli.command {
        Commands::Recall {
            query,
            for_task,
            tags,
            outcome,
            limit,
            include_superseded,
        } => {
            assert!(query.is_none());
            assert!(for_task.is_none());
            assert!(tags.is_none());
            assert!(outcome.is_none());
            assert_eq!(limit, 5); // default
            assert!(!include_superseded, "default must exclude superseded");
        }
        _ => panic!("Expected Recall command"),
    }
}

#[test]
fn test_recall_with_query() {
    let cli = Cli::parse_from(["task-mgr", "recall", "--query", "database connection"]);
    match cli.command {
        Commands::Recall { query, .. } => {
            assert_eq!(query, Some("database connection".to_string()));
        }
        _ => panic!("Expected Recall command"),
    }
}

#[test]
fn test_recall_with_for_task() {
    let cli = Cli::parse_from(["task-mgr", "recall", "--for-task", "US-005"]);
    match cli.command {
        Commands::Recall { for_task, .. } => {
            assert_eq!(for_task, Some("US-005".to_string()));
        }
        _ => panic!("Expected Recall command"),
    }
}

#[test]
fn test_recall_with_tags() {
    let cli = Cli::parse_from(["task-mgr", "recall", "--tags", "rust,cli,error"]);
    match cli.command {
        Commands::Recall { tags, .. } => {
            assert_eq!(
                tags,
                Some(vec![
                    "rust".to_string(),
                    "cli".to_string(),
                    "error".to_string()
                ])
            );
        }
        _ => panic!("Expected Recall command"),
    }
}

#[test]
fn test_recall_with_outcome() {
    let cli = Cli::parse_from(["task-mgr", "recall", "--outcome", "failure"]);
    match cli.command {
        Commands::Recall { outcome, .. } => {
            assert_eq!(outcome, Some(LearningOutcome::Failure));
        }
        _ => panic!("Expected Recall command"),
    }
}

#[test]
fn test_recall_with_limit() {
    let cli = Cli::parse_from(["task-mgr", "recall", "--limit", "10"]);
    match cli.command {
        Commands::Recall { limit, .. } => {
            assert_eq!(limit, 10);
        }
        _ => panic!("Expected Recall command"),
    }
}

#[test]
fn test_recall_with_all_options() {
    let cli = Cli::parse_from([
        "task-mgr",
        "recall",
        "--query",
        "SQL error",
        "--for-task",
        "US-010",
        "--tags",
        "database,error",
        "--outcome",
        "workaround",
        "--limit",
        "3",
    ]);
    match cli.command {
        Commands::Recall {
            query,
            for_task,
            tags,
            outcome,
            limit,
            include_superseded,
        } => {
            assert_eq!(query, Some("SQL error".to_string()));
            assert_eq!(for_task, Some("US-010".to_string()));
            assert_eq!(
                tags,
                Some(vec!["database".to_string(), "error".to_string()])
            );
            assert_eq!(outcome, Some(LearningOutcome::Workaround));
            assert_eq!(limit, 3);
            assert!(!include_superseded);
        }
        _ => panic!("Expected Recall command"),
    }
}

#[test]
fn test_recall_include_superseded_flag() {
    let cli = Cli::parse_from(["task-mgr", "recall", "--include-superseded"]);
    match cli.command {
        Commands::Recall {
            include_superseded, ..
        } => {
            assert!(
                include_superseded,
                "--include-superseded must set the flag to true"
            );
        }
        _ => panic!("Expected Recall command"),
    }
}

// Learnings command tests
#[test]
fn test_learnings_no_flags() {
    let cli = Cli::parse_from(["task-mgr", "learnings"]);
    match cli.command {
        Commands::Learnings { recent } => {
            assert!(recent.is_none());
        }
        _ => panic!("Expected Learnings command"),
    }
}

#[test]
fn test_learnings_with_recent() {
    let cli = Cli::parse_from(["task-mgr", "learnings", "--recent", "10"]);
    match cli.command {
        Commands::Learnings { recent } => {
            assert_eq!(recent, Some(10));
        }
        _ => panic!("Expected Learnings command"),
    }
}

// Apply-learning command tests
#[test]
fn test_apply_learning() {
    let cli = Cli::parse_from(["task-mgr", "apply-learning", "42"]);
    match cli.command {
        Commands::ApplyLearning { learning_id } => {
            assert_eq!(learning_id, 42);
        }
        _ => panic!("Expected ApplyLearning command"),
    }
}

// Skip command tests
#[test]
fn test_skip_with_reason() {
    let cli = Cli::parse_from([
        "task-mgr",
        "skip",
        "US-001",
        "--reason",
        "Deferring to next sprint",
    ]);
    match cli.command {
        Commands::Skip {
            task_ids,
            reason,
            run_id,
        } => {
            assert_eq!(task_ids, vec!["US-001".to_string()]);
            assert_eq!(reason, "Deferring to next sprint");
            assert!(run_id.is_none());
        }
        _ => panic!("Expected Skip command"),
    }
}

#[test]
fn test_skip_with_run_id() {
    let cli = Cli::parse_from([
        "task-mgr",
        "skip",
        "US-002",
        "--reason",
        "Need more info",
        "--run-id",
        "run-123",
    ]);
    match cli.command {
        Commands::Skip {
            task_ids,
            reason,
            run_id,
        } => {
            assert_eq!(task_ids, vec!["US-002".to_string()]);
            assert_eq!(reason, "Need more info");
            assert_eq!(run_id, Some("run-123".to_string()));
        }
        _ => panic!("Expected Skip command"),
    }
}

#[test]
fn test_skip_multiple_tasks() {
    let cli = Cli::parse_from([
        "task-mgr",
        "skip",
        "US-001",
        "US-002",
        "US-003",
        "--reason",
        "Batch skip",
    ]);
    match cli.command {
        Commands::Skip {
            task_ids, reason, ..
        } => {
            assert_eq!(
                task_ids,
                vec![
                    "US-001".to_string(),
                    "US-002".to_string(),
                    "US-003".to_string()
                ]
            );
            assert_eq!(reason, "Batch skip");
        }
        _ => panic!("Expected Skip command"),
    }
}

// Irrelevant command tests
#[test]
fn test_irrelevant_with_reason() {
    let cli = Cli::parse_from([
        "task-mgr",
        "irrelevant",
        "US-003",
        "--reason",
        "Requirements changed",
    ]);
    match cli.command {
        Commands::Irrelevant {
            task_ids,
            reason,
            run_id,
            learning_id,
        } => {
            assert_eq!(task_ids, vec!["US-003".to_string()]);
            assert_eq!(reason, "Requirements changed");
            assert!(run_id.is_none());
            assert!(learning_id.is_none());
        }
        _ => panic!("Expected Irrelevant command"),
    }
}

#[test]
fn test_irrelevant_with_all_options() {
    let cli = Cli::parse_from([
        "task-mgr",
        "irrelevant",
        "US-004",
        "--reason",
        "Feature dropped",
        "--run-id",
        "run-456",
        "--learning-id",
        "42",
    ]);
    match cli.command {
        Commands::Irrelevant {
            task_ids,
            reason,
            run_id,
            learning_id,
        } => {
            assert_eq!(task_ids, vec!["US-004".to_string()]);
            assert_eq!(reason, "Feature dropped");
            assert_eq!(run_id, Some("run-456".to_string()));
            assert_eq!(learning_id, Some(42));
        }
        _ => panic!("Expected Irrelevant command"),
    }
}

#[test]
fn test_irrelevant_multiple_tasks() {
    let cli = Cli::parse_from([
        "task-mgr",
        "irrelevant",
        "US-001",
        "US-002",
        "US-003",
        "--reason",
        "Batch irrelevant",
    ]);
    match cli.command {
        Commands::Irrelevant {
            task_ids, reason, ..
        } => {
            assert_eq!(
                task_ids,
                vec![
                    "US-001".to_string(),
                    "US-002".to_string(),
                    "US-003".to_string()
                ]
            );
            assert_eq!(reason, "Batch irrelevant");
        }
        _ => panic!("Expected Irrelevant command"),
    }
}

// Verbose flag tests
#[test]
fn test_default_verbose() {
    let cli = Cli::parse_from(["task-mgr", "list"]);
    assert!(!cli.verbose);
}

#[test]
fn test_verbose_short_flag() {
    let cli = Cli::parse_from(["task-mgr", "-v", "list"]);
    assert!(cli.verbose);
}

#[test]
fn test_verbose_long_flag() {
    let cli = Cli::parse_from(["task-mgr", "--verbose", "list"]);
    assert!(cli.verbose);
}

#[test]
fn test_verbose_after_command() {
    // Global flags should work after command name too
    let cli = Cli::parse_from(["task-mgr", "list", "--verbose"]);
    assert!(cli.verbose);
}

#[test]
fn test_verbose_with_format() {
    let cli = Cli::parse_from(["task-mgr", "-v", "--format", "json", "list"]);
    assert!(cli.verbose);
    assert_eq!(cli.format, OutputFormat::Json);
}

// Unblock command tests
#[test]
fn test_unblock_command() {
    let cli = Cli::parse_from(["task-mgr", "unblock", "US-001"]);
    match cli.command {
        Commands::Unblock { task_id } => {
            assert_eq!(task_id, "US-001");
        }
        _ => panic!("Expected Unblock command"),
    }
}

// Unskip command tests
#[test]
fn test_unskip_command() {
    let cli = Cli::parse_from(["task-mgr", "unskip", "US-002"]);
    match cli.command {
        Commands::Unskip { task_id } => {
            assert_eq!(task_id, "US-002");
        }
        _ => panic!("Expected Unskip command"),
    }
}

// Reset command tests
#[test]
fn test_reset_single_task() {
    let cli = Cli::parse_from(["task-mgr", "reset", "US-001"]);
    match cli.command {
        Commands::Reset { task_ids, all, yes } => {
            assert_eq!(task_ids, vec!["US-001"]);
            assert!(!all);
            assert!(!yes);
        }
        _ => panic!("Expected Reset command"),
    }
}

#[test]
fn test_reset_multiple_tasks() {
    let cli = Cli::parse_from(["task-mgr", "reset", "US-001", "US-002", "US-003"]);
    match cli.command {
        Commands::Reset { task_ids, all, yes } => {
            assert_eq!(task_ids, vec!["US-001", "US-002", "US-003"]);
            assert!(!all);
            assert!(!yes);
        }
        _ => panic!("Expected Reset command"),
    }
}

#[test]
fn test_reset_all() {
    let cli = Cli::parse_from(["task-mgr", "reset", "--all"]);
    match cli.command {
        Commands::Reset { task_ids, all, yes } => {
            assert!(task_ids.is_empty());
            assert!(all);
            assert!(!yes);
        }
        _ => panic!("Expected Reset command"),
    }
}

#[test]
fn test_reset_all_with_yes() {
    let cli = Cli::parse_from(["task-mgr", "reset", "--all", "--yes"]);
    match cli.command {
        Commands::Reset { task_ids, all, yes } => {
            assert!(task_ids.is_empty());
            assert!(all);
            assert!(yes);
        }
        _ => panic!("Expected Reset command"),
    }
}

#[test]
fn test_reset_all_with_short_yes() {
    let cli = Cli::parse_from(["task-mgr", "reset", "--all", "-y"]);
    match cli.command {
        Commands::Reset { task_ids, all, yes } => {
            assert!(task_ids.is_empty());
            assert!(all);
            assert!(yes);
        }
        _ => panic!("Expected Reset command"),
    }
}

// Stats command tests
#[test]
fn test_stats_command() {
    let cli = Cli::parse_from(["task-mgr", "stats"]);
    assert!(matches!(cli.command, Commands::Stats));
}

#[test]
fn test_stats_with_json_format() {
    let cli = Cli::parse_from(["task-mgr", "--format", "json", "stats"]);
    assert!(matches!(cli.command, Commands::Stats));
    assert_eq!(cli.format, OutputFormat::Json);
}

// History command tests
#[test]
fn test_history_default() {
    let cli = Cli::parse_from(["task-mgr", "history"]);
    match cli.command {
        Commands::History { limit, run_id, .. } => {
            assert_eq!(limit, 10);
            assert!(run_id.is_none());
        }
        _ => panic!("Expected History command"),
    }
}

#[test]
fn test_history_with_limit() {
    let cli = Cli::parse_from(["task-mgr", "history", "--limit", "25"]);
    match cli.command {
        Commands::History { limit, run_id, .. } => {
            assert_eq!(limit, 25);
            assert!(run_id.is_none());
        }
        _ => panic!("Expected History command"),
    }
}

#[test]
fn test_history_with_run_id() {
    let cli = Cli::parse_from(["task-mgr", "history", "--run-id", "run-abc123"]);
    match cli.command {
        Commands::History { limit, run_id, .. } => {
            assert_eq!(limit, 10); // default
            assert_eq!(run_id, Some("run-abc123".to_string()));
        }
        _ => panic!("Expected History command"),
    }
}

#[test]
fn test_history_with_json_format() {
    let cli = Cli::parse_from(["task-mgr", "--format", "json", "history"]);
    match cli.command {
        Commands::History { limit, run_id, .. } => {
            assert_eq!(limit, 10);
            assert!(run_id.is_none());
        }
        _ => panic!("Expected History command"),
    }
    assert_eq!(cli.format, OutputFormat::Json);
}

// DeleteLearning command tests
#[test]
fn test_delete_learning_basic() {
    let cli = Cli::parse_from(["task-mgr", "delete-learning", "42"]);
    match cli.command {
        Commands::DeleteLearning { learning_id, yes } => {
            assert_eq!(learning_id, 42);
            assert!(!yes);
        }
        _ => panic!("Expected DeleteLearning command"),
    }
}

#[test]
fn test_delete_learning_with_yes() {
    let cli = Cli::parse_from(["task-mgr", "delete-learning", "42", "--yes"]);
    match cli.command {
        Commands::DeleteLearning { learning_id, yes } => {
            assert_eq!(learning_id, 42);
            assert!(yes);
        }
        _ => panic!("Expected DeleteLearning command"),
    }
}

#[test]
fn test_delete_learning_with_short_yes() {
    let cli = Cli::parse_from(["task-mgr", "delete-learning", "42", "-y"]);
    match cli.command {
        Commands::DeleteLearning { learning_id, yes } => {
            assert_eq!(learning_id, 42);
            assert!(yes);
        }
        _ => panic!("Expected DeleteLearning command"),
    }
}

#[test]
fn test_delete_learning_with_json_format() {
    let cli = Cli::parse_from([
        "task-mgr",
        "--format",
        "json",
        "delete-learning",
        "123",
        "-y",
    ]);
    match cli.command {
        Commands::DeleteLearning { learning_id, yes } => {
            assert_eq!(learning_id, 123);
            assert!(yes);
        }
        _ => panic!("Expected DeleteLearning command"),
    }
    assert_eq!(cli.format, OutputFormat::Json);
}

// EditLearning command tests
#[test]
fn test_edit_learning_basic() {
    let cli = Cli::parse_from(["task-mgr", "edit-learning", "42"]);
    match cli.command {
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
            ..
        } => {
            assert_eq!(learning_id, 42);
            assert!(title.is_none());
            assert!(content.is_none());
            assert!(solution.is_none());
            assert!(root_cause.is_none());
            assert!(confidence.is_none());
            assert!(add_tags.is_none());
            assert!(remove_tags.is_none());
            assert!(add_files.is_none());
            assert!(remove_files.is_none());
        }
        _ => panic!("Expected EditLearning command"),
    }
}

#[test]
fn test_edit_learning_with_title_and_content() {
    let cli = Cli::parse_from([
        "task-mgr",
        "edit-learning",
        "42",
        "--title",
        "New Title",
        "--content",
        "New content here",
    ]);
    match cli.command {
        Commands::EditLearning {
            learning_id,
            title,
            content,
            ..
        } => {
            assert_eq!(learning_id, 42);
            assert_eq!(title, Some("New Title".to_string()));
            assert_eq!(content, Some("New content here".to_string()));
        }
        _ => panic!("Expected EditLearning command"),
    }
}

#[test]
fn test_edit_learning_with_solution_and_root_cause() {
    let cli = Cli::parse_from([
        "task-mgr",
        "edit-learning",
        "42",
        "--solution",
        "Fixed the bug",
        "--root-cause",
        "Missing null check",
    ]);
    match cli.command {
        Commands::EditLearning {
            learning_id,
            solution,
            root_cause,
            ..
        } => {
            assert_eq!(learning_id, 42);
            assert_eq!(solution, Some("Fixed the bug".to_string()));
            assert_eq!(root_cause, Some("Missing null check".to_string()));
        }
        _ => panic!("Expected EditLearning command"),
    }
}

#[test]
fn test_edit_learning_with_confidence() {
    let cli = Cli::parse_from(["task-mgr", "edit-learning", "42", "--confidence", "high"]);
    match cli.command {
        Commands::EditLearning {
            learning_id,
            confidence,
            ..
        } => {
            assert_eq!(learning_id, 42);
            assert_eq!(confidence, Some(Confidence::High));
        }
        _ => panic!("Expected EditLearning command"),
    }
}

#[test]
fn test_edit_learning_with_tags() {
    let cli = Cli::parse_from([
        "task-mgr",
        "edit-learning",
        "42",
        "--add-tags",
        "rust,cli",
        "--remove-tags",
        "old-tag",
    ]);
    match cli.command {
        Commands::EditLearning {
            learning_id,
            add_tags,
            remove_tags,
            ..
        } => {
            assert_eq!(learning_id, 42);
            assert_eq!(add_tags, Some(vec!["rust".to_string(), "cli".to_string()]));
            assert_eq!(remove_tags, Some(vec!["old-tag".to_string()]));
        }
        _ => panic!("Expected EditLearning command"),
    }
}

#[test]
fn test_edit_learning_with_files() {
    let cli = Cli::parse_from([
        "task-mgr",
        "edit-learning",
        "42",
        "--add-files",
        "src/main.rs,src/lib.rs",
        "--remove-files",
        "old/path.rs",
    ]);
    match cli.command {
        Commands::EditLearning {
            learning_id,
            add_files,
            remove_files,
            ..
        } => {
            assert_eq!(learning_id, 42);
            assert_eq!(
                add_files,
                Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()])
            );
            assert_eq!(remove_files, Some(vec!["old/path.rs".to_string()]));
        }
        _ => panic!("Expected EditLearning command"),
    }
}

#[test]
fn test_edit_learning_with_all_options() {
    let cli = Cli::parse_from([
        "task-mgr",
        "edit-learning",
        "123",
        "--title",
        "Updated Title",
        "--content",
        "Updated content",
        "--solution",
        "New solution",
        "--root-cause",
        "New root cause",
        "--confidence",
        "low",
        "--add-tags",
        "new-tag",
        "--remove-tags",
        "old-tag",
        "--add-files",
        "new/file.rs",
        "--remove-files",
        "old/file.rs",
    ]);
    match cli.command {
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
            ..
        } => {
            assert_eq!(learning_id, 123);
            assert_eq!(title, Some("Updated Title".to_string()));
            assert_eq!(content, Some("Updated content".to_string()));
            assert_eq!(solution, Some("New solution".to_string()));
            assert_eq!(root_cause, Some("New root cause".to_string()));
            assert_eq!(confidence, Some(Confidence::Low));
            assert_eq!(add_tags, Some(vec!["new-tag".to_string()]));
            assert_eq!(remove_tags, Some(vec!["old-tag".to_string()]));
            assert_eq!(add_files, Some(vec!["new/file.rs".to_string()]));
            assert_eq!(remove_files, Some(vec!["old/file.rs".to_string()]));
        }
        _ => panic!("Expected EditLearning command"),
    }
}

#[test]
fn test_edit_learning_with_json_format() {
    let cli = Cli::parse_from([
        "task-mgr",
        "--format",
        "json",
        "edit-learning",
        "42",
        "--title",
        "Test",
    ]);
    match cli.command {
        Commands::EditLearning {
            learning_id, title, ..
        } => {
            assert_eq!(learning_id, 42);
            assert_eq!(title, Some("Test".to_string()));
        }
        _ => panic!("Expected EditLearning command"),
    }
    assert_eq!(cli.format, OutputFormat::Json);
}

#[test]
fn test_edit_learning_with_supersedes() {
    let cli = Cli::parse_from(["task-mgr", "edit-learning", "7", "--supersedes", "3"]);
    match cli.command {
        Commands::EditLearning {
            learning_id,
            supersedes,
            ..
        } => {
            assert_eq!(learning_id, 7);
            assert_eq!(supersedes, Some(3));
        }
        _ => panic!("Expected EditLearning command"),
    }
}

// Review command tests
#[test]
fn test_review_default() {
    let cli = Cli::parse_from(["task-mgr", "review"]);
    match cli.command {
        Commands::Review {
            blocked,
            skipped,
            auto,
        } => {
            assert!(!blocked);
            assert!(!skipped);
            assert!(!auto);
        }
        _ => panic!("Expected Review command"),
    }
}

#[test]
fn test_review_blocked_only() {
    let cli = Cli::parse_from(["task-mgr", "review", "--blocked"]);
    match cli.command {
        Commands::Review {
            blocked,
            skipped,
            auto,
        } => {
            assert!(blocked);
            assert!(!skipped);
            assert!(!auto);
        }
        _ => panic!("Expected Review command"),
    }
}

#[test]
fn test_review_skipped_only() {
    let cli = Cli::parse_from(["task-mgr", "review", "--skipped"]);
    match cli.command {
        Commands::Review {
            blocked,
            skipped,
            auto,
        } => {
            assert!(!blocked);
            assert!(skipped);
            assert!(!auto);
        }
        _ => panic!("Expected Review command"),
    }
}

#[test]
fn test_review_auto() {
    let cli = Cli::parse_from(["task-mgr", "review", "--auto"]);
    match cli.command {
        Commands::Review {
            blocked,
            skipped,
            auto,
        } => {
            assert!(!blocked);
            assert!(!skipped);
            assert!(auto);
        }
        _ => panic!("Expected Review command"),
    }
}

#[test]
fn test_review_auto_blocked_only() {
    let cli = Cli::parse_from(["task-mgr", "review", "--auto", "--blocked"]);
    match cli.command {
        Commands::Review {
            blocked,
            skipped,
            auto,
        } => {
            assert!(blocked);
            assert!(!skipped);
            assert!(auto);
        }
        _ => panic!("Expected Review command"),
    }
}

#[test]
fn test_review_with_json_format() {
    let cli = Cli::parse_from(["task-mgr", "--format", "json", "review"]);
    match cli.command {
        Commands::Review { .. } => {}
        _ => panic!("Expected Review command"),
    }
    assert_eq!(cli.format, OutputFormat::Json);
}

// Migrate command tests
#[test]
fn test_migrate_status() {
    let cli = Cli::parse_from(["task-mgr", "migrate", "status"]);
    match cli.command {
        Commands::Migrate { action } => {
            assert!(matches!(action, MigrateAction::Status));
        }
        _ => panic!("Expected Migrate command"),
    }
}

#[test]
fn test_migrate_up() {
    let cli = Cli::parse_from(["task-mgr", "migrate", "up"]);
    match cli.command {
        Commands::Migrate { action } => {
            assert!(matches!(action, MigrateAction::Up));
        }
        _ => panic!("Expected Migrate command"),
    }
}

#[test]
fn test_migrate_down() {
    let cli = Cli::parse_from(["task-mgr", "migrate", "down"]);
    match cli.command {
        Commands::Migrate { action } => {
            assert!(matches!(action, MigrateAction::Down));
        }
        _ => panic!("Expected Migrate command"),
    }
}

#[test]
fn test_migrate_all() {
    let cli = Cli::parse_from(["task-mgr", "migrate", "all"]);
    match cli.command {
        Commands::Migrate { action } => {
            assert!(matches!(action, MigrateAction::All));
        }
        _ => panic!("Expected Migrate command"),
    }
}

#[test]
fn test_migrate_with_json_format() {
    let cli = Cli::parse_from(["task-mgr", "--format", "json", "migrate", "status"]);
    match cli.command {
        Commands::Migrate { action } => {
            assert!(matches!(action, MigrateAction::Status));
        }
        _ => panic!("Expected Migrate command"),
    }
    assert_eq!(cli.format, OutputFormat::Json);
}

// Completions command tests
#[test]
fn test_completions_bash() {
    let cli = Cli::parse_from(["task-mgr", "completions", "bash"]);
    match cli.command {
        Commands::Completions { shell } => {
            assert_eq!(shell, Shell::Bash);
        }
        _ => panic!("Expected Completions command"),
    }
}

#[test]
fn test_completions_zsh() {
    let cli = Cli::parse_from(["task-mgr", "completions", "zsh"]);
    match cli.command {
        Commands::Completions { shell } => {
            assert_eq!(shell, Shell::Zsh);
        }
        _ => panic!("Expected Completions command"),
    }
}

#[test]
fn test_completions_fish() {
    let cli = Cli::parse_from(["task-mgr", "completions", "fish"]);
    match cli.command {
        Commands::Completions { shell } => {
            assert_eq!(shell, Shell::Fish);
        }
        _ => panic!("Expected Completions command"),
    }
}

#[test]
fn test_completions_powershell() {
    let cli = Cli::parse_from(["task-mgr", "completions", "powershell"]);
    match cli.command {
        Commands::Completions { shell } => {
            assert_eq!(shell, Shell::Powershell);
        }
        _ => panic!("Expected Completions command"),
    }
}

// ManPages command tests
#[test]
fn test_man_pages_list() {
    let cli = Cli::parse_from(["task-mgr", "man-pages", "--list"]);
    match cli.command {
        Commands::ManPages {
            output_dir,
            name,
            list,
        } => {
            assert!(output_dir.is_none());
            assert!(name.is_none());
            assert!(list);
        }
        _ => panic!("Expected ManPages command"),
    }
}

#[test]
fn test_man_pages_output_dir() {
    let cli = Cli::parse_from(["task-mgr", "man-pages", "--output-dir", "/tmp/man"]);
    match cli.command {
        Commands::ManPages {
            output_dir,
            name,
            list,
        } => {
            assert_eq!(output_dir, Some(PathBuf::from("/tmp/man")));
            assert!(name.is_none());
            assert!(!list);
        }
        _ => panic!("Expected ManPages command"),
    }
}

#[test]
fn test_man_pages_name() {
    let cli = Cli::parse_from(["task-mgr", "man-pages", "--name", "task-mgr-next"]);
    match cli.command {
        Commands::ManPages {
            output_dir,
            name,
            list,
        } => {
            assert!(output_dir.is_none());
            assert_eq!(name, Some("task-mgr-next".to_string()));
            assert!(!list);
        }
        _ => panic!("Expected ManPages command"),
    }
}

#[test]
fn test_man_pages_defaults() {
    let cli = Cli::parse_from(["task-mgr", "man-pages"]);
    match cli.command {
        Commands::ManPages {
            output_dir,
            name,
            list,
        } => {
            assert!(output_dir.is_none());
            assert!(name.is_none());
            assert!(!list);
        }
        _ => panic!("Expected ManPages command"),
    }
}

// =============================================================================
// Loop command tests (TEST-INIT-004)
// =============================================================================

#[test]
fn test_loop_with_prd_file_and_yes() {
    let cli = Cli::parse_from(["task-mgr", "loop", ".task-mgr/tasks/prd.json", "--yes"]);
    match cli.command {
        Commands::Loop {
            prd_file,
            prompt_file,
            yes,
            hours,
            verbose,
            no_worktree,
            external_repo,
            cleanup_worktree,
            parallel,
        } => {
            assert_eq!(prd_file, PathBuf::from(".task-mgr/tasks/prd.json"));
            assert!(prompt_file.is_none());
            assert!(yes);
            assert!(hours.is_none());
            assert!(!verbose);
            assert!(!no_worktree);
            assert!(external_repo.is_none());
            assert!(!cleanup_worktree);
            assert_eq!(parallel, 1);
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_with_hours() {
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--hours",
        "4.5",
        "--yes",
    ]);
    match cli.command {
        Commands::Loop {
            prd_file,
            hours,
            yes,
            ..
        } => {
            assert_eq!(prd_file, PathBuf::from(".task-mgr/tasks/prd.json"));
            assert_eq!(hours, Some(4.5));
            assert!(yes);
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_with_prompt_file() {
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--prompt-file",
        ".task-mgr/tasks/custom-prompt.md",
        "--yes",
    ]);
    match cli.command {
        Commands::Loop {
            prd_file,
            prompt_file,
            ..
        } => {
            assert_eq!(prd_file, PathBuf::from(".task-mgr/tasks/prd.json"));
            assert_eq!(
                prompt_file,
                Some(PathBuf::from(".task-mgr/tasks/custom-prompt.md"))
            );
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_with_verbose() {
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--yes",
        "--verbose",
    ]);
    match cli.command {
        Commands::Loop { verbose, .. } => {
            assert!(verbose);
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_minimal() {
    // Loop requires prd_file positional arg
    let cli = Cli::parse_from(["task-mgr", "loop", ".task-mgr/tasks/prd.json"]);
    match cli.command {
        Commands::Loop {
            prd_file,
            prompt_file,
            yes,
            hours,
            verbose,
            no_worktree,
            external_repo,
            ..
        } => {
            assert_eq!(prd_file, PathBuf::from(".task-mgr/tasks/prd.json"));
            assert!(prompt_file.is_none());
            assert!(!yes);
            assert!(hours.is_none());
            assert!(!verbose);
            assert!(!no_worktree);
            assert!(external_repo.is_none());
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_with_all_options() {
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--prompt-file",
        ".task-mgr/tasks/prompt.md",
        "--yes",
        "--hours",
        "2.0",
        "--verbose",
    ]);
    match cli.command {
        Commands::Loop {
            prd_file,
            prompt_file,
            yes,
            hours,
            verbose,
            no_worktree,
            external_repo,
            ..
        } => {
            assert_eq!(prd_file, PathBuf::from(".task-mgr/tasks/prd.json"));
            assert_eq!(
                prompt_file,
                Some(PathBuf::from(".task-mgr/tasks/prompt.md"))
            );
            assert!(yes);
            assert_eq!(hours, Some(2.0));
            assert!(verbose);
            assert!(!no_worktree);
            assert!(external_repo.is_none());
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_with_no_worktree() {
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--yes",
        "--no-worktree",
    ]);
    match cli.command {
        Commands::Loop { no_worktree, .. } => {
            assert!(no_worktree, "--no-worktree should set flag to true");
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_with_external_repo() {
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--yes",
        "--external-repo",
        "../restaurant_agent_ex",
    ]);
    match cli.command {
        Commands::Loop { external_repo, .. } => {
            assert_eq!(external_repo, Some(PathBuf::from("../restaurant_agent_ex")));
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_missing_prd_file_fails() {
    // Missing required positional arg should fail
    let result = Cli::try_parse_from(["task-mgr", "loop"]);
    assert!(result.is_err());
}

#[test]
fn test_loop_hours_zero() {
    // hours=0 is syntactically valid (semantic validation happens at runtime)
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--hours",
        "0",
    ]);
    match cli.command {
        Commands::Loop { hours, .. } => {
            assert_eq!(hours, Some(0.0));
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_hours_large() {
    // hours=169 is syntactically valid (semantic validation at runtime, max 168)
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--hours",
        "169",
    ]);
    match cli.command {
        Commands::Loop { hours, .. } => {
            assert_eq!(hours, Some(169.0));
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_hours_fractional() {
    let cli = Cli::parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--hours",
        "0.25",
    ]);
    match cli.command {
        Commands::Loop { hours, .. } => {
            assert_eq!(hours, Some(0.25));
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_loop_hours_negative_fails() {
    // Negative hours should fail to parse as f64 with clap
    // (clap parses -1 as a flag, not a value, so this should fail)
    let result = Cli::try_parse_from([
        "task-mgr",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--hours",
        "-1",
    ]);
    // clap may interpret -1 as a flag rather than a value; either error is fine
    assert!(result.is_err());
}

#[test]
fn test_loop_short_yes_flag() {
    let cli = Cli::parse_from(["task-mgr", "loop", ".task-mgr/tasks/prd.json", "-y"]);
    match cli.command {
        Commands::Loop { yes, .. } => {
            assert!(yes);
        }
        _ => panic!("Expected Loop command"),
    }
}

// =============================================================================
// Status command tests (TEST-INIT-004)
// =============================================================================

#[test]
fn test_status_no_args() {
    let cli = Cli::parse_from(["task-mgr", "status"]);
    match cli.command {
        Commands::Status {
            prd_file, verbose, ..
        } => {
            assert!(prd_file.is_none());
            assert!(!verbose);
        }
        _ => panic!("Expected Status command"),
    }
}

#[test]
fn test_status_with_prd_file() {
    let cli = Cli::parse_from(["task-mgr", "status", ".task-mgr/tasks/prd.json"]);
    match cli.command {
        Commands::Status {
            prd_file, verbose, ..
        } => {
            assert_eq!(prd_file, Some(PathBuf::from(".task-mgr/tasks/prd.json")));
            assert!(!verbose);
        }
        _ => panic!("Expected Status command"),
    }
}

#[test]
fn test_status_with_verbose() {
    let cli = Cli::parse_from(["task-mgr", "status", "-v"]);
    match cli.command {
        Commands::Status {
            prd_file, verbose, ..
        } => {
            assert!(prd_file.is_none());
            assert!(verbose);
        }
        _ => panic!("Expected Status command"),
    }
}

#[test]
fn test_status_with_verbose_long() {
    let cli = Cli::parse_from(["task-mgr", "status", "--verbose"]);
    match cli.command {
        Commands::Status { verbose, .. } => {
            assert!(verbose);
        }
        _ => panic!("Expected Status command"),
    }
}

#[test]
fn test_status_with_prd_file_and_verbose() {
    let cli = Cli::parse_from(["task-mgr", "status", ".task-mgr/tasks/prd.json", "-v"]);
    match cli.command {
        Commands::Status {
            prd_file, verbose, ..
        } => {
            assert_eq!(prd_file, Some(PathBuf::from(".task-mgr/tasks/prd.json")));
            assert!(verbose);
        }
        _ => panic!("Expected Status command"),
    }
}

// =============================================================================
// Batch command tests (TEST-INIT-004)
// =============================================================================

#[test]
fn test_batch_with_pattern_and_yes() {
    let cli = Cli::parse_from(["task-mgr", "batch", ".task-mgr/tasks/*.json", "--yes"]);
    match cli.command {
        Commands::Batch {
            patterns,
            max_iterations,
            yes,
            ..
        } => {
            assert_eq!(patterns, vec![".task-mgr/tasks/*.json"]);
            assert!(max_iterations.is_none());
            assert!(yes);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_with_max_iterations() {
    let cli = Cli::parse_from([
        "task-mgr",
        "batch",
        ".task-mgr/tasks/*.json",
        "-n",
        "10",
        "-y",
    ]);
    match cli.command {
        Commands::Batch {
            patterns,
            max_iterations,
            yes,
            ..
        } => {
            assert_eq!(patterns, vec![".task-mgr/tasks/*.json"]);
            assert_eq!(max_iterations, Some(10));
            assert!(yes);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_multiple_patterns() {
    let cli = Cli::parse_from([
        "task-mgr",
        "batch",
        "tasks/rag-01.json",
        "tasks/rag-02.json",
        "--yes",
    ]);
    match cli.command {
        Commands::Batch { patterns, yes, .. } => {
            assert_eq!(patterns, vec!["tasks/rag-01.json", "tasks/rag-02.json"]);
            assert!(yes);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_minimal() {
    // Batch requires at least one pattern positional arg
    let cli = Cli::parse_from(["task-mgr", "batch", ".task-mgr/tasks/*.json"]);
    match cli.command {
        Commands::Batch {
            patterns,
            max_iterations,
            yes,
            ..
        } => {
            assert_eq!(patterns, vec![".task-mgr/tasks/*.json"]);
            assert!(max_iterations.is_none());
            assert!(!yes);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_missing_pattern_fails() {
    let result = Cli::try_parse_from(["task-mgr", "batch"]);
    assert!(result.is_err());
}

#[test]
fn test_batch_keep_worktrees_flag() {
    let cli = Cli::parse_from([
        "task-mgr",
        "batch",
        ".task-mgr/tasks/*.json",
        "--yes",
        "--keep-worktrees",
    ]);
    match cli.command {
        Commands::Batch {
            keep_worktrees,
            yes,
            ..
        } => {
            assert!(keep_worktrees);
            assert!(yes);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_keep_worktrees_defaults_false() {
    let cli = Cli::parse_from(["task-mgr", "batch", ".task-mgr/tasks/*.json"]);
    match cli.command {
        Commands::Batch { keep_worktrees, .. } => {
            assert!(!keep_worktrees);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_short_yes_flag() {
    let cli = Cli::parse_from(["task-mgr", "batch", ".task-mgr/tasks/*.json", "-y"]);
    match cli.command {
        Commands::Batch { yes, .. } => {
            assert!(yes);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_chain_flag_true() {
    let cli = Cli::parse_from([
        "task-mgr",
        "batch",
        "tasks/stage-*.json",
        "--chain",
        "--yes",
    ]);
    match cli.command {
        Commands::Batch { chain, yes, .. } => {
            assert!(chain);
            assert!(yes);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_chain_defaults_false() {
    let cli = Cli::parse_from(["task-mgr", "batch", "tasks/stage-*.json"]);
    match cli.command {
        Commands::Batch { chain, .. } => {
            assert!(!chain);
        }
        _ => panic!("Expected Batch command"),
    }
}

#[test]
fn test_batch_chain_with_yes_and_keep_worktrees() {
    let cli = Cli::parse_from([
        "task-mgr",
        "batch",
        "tasks/stage-*.json",
        "--chain",
        "--yes",
        "--keep-worktrees",
    ]);
    match cli.command {
        Commands::Batch {
            chain,
            yes,
            keep_worktrees,
            ..
        } => {
            assert!(chain);
            assert!(yes);
            assert!(keep_worktrees);
        }
        _ => panic!("Expected Batch command"),
    }
}

// =============================================================================
// Archive command tests (TEST-INIT-004)
// =============================================================================

#[test]
fn test_archive_defaults() {
    let cli = Cli::parse_from(["task-mgr", "archive"]);
    match cli.command {
        Commands::Archive {
            dry_run,
            all,
            branch,
        } => {
            assert!(!dry_run);
            assert!(!all);
            assert!(branch.is_none());
        }
        _ => panic!("Expected Archive command"),
    }
}

#[test]
fn test_archive_with_dry_run() {
    let cli = Cli::parse_from(["task-mgr", "archive", "--dry-run"]);
    match cli.command {
        Commands::Archive {
            dry_run,
            all,
            branch,
        } => {
            assert!(dry_run);
            assert!(!all);
            assert!(branch.is_none());
        }
        _ => panic!("Expected Archive command"),
    }
}

#[test]
fn test_archive_with_all_flag() {
    let cli = Cli::parse_from(["task-mgr", "archive", "--all"]);
    match cli.command {
        Commands::Archive {
            dry_run,
            all,
            branch,
        } => {
            assert!(!dry_run);
            assert!(all);
            assert!(branch.is_none());
        }
        _ => panic!("Expected Archive command"),
    }
}

#[test]
fn test_archive_with_json_format() {
    let cli = Cli::parse_from(["task-mgr", "--format", "json", "archive"]);
    match cli.command {
        Commands::Archive {
            dry_run,
            all,
            branch,
        } => {
            assert!(!dry_run);
            assert!(!all);
            assert!(branch.is_none());
        }
        _ => panic!("Expected Archive command"),
    }
    assert_eq!(cli.format, OutputFormat::Json);
}

#[test]
fn test_archive_with_branch_flag() {
    let cli = Cli::parse_from(["task-mgr", "archive", "--branch", "feat/x"]);
    match cli.command {
        Commands::Archive {
            dry_run,
            all,
            branch,
        } => {
            assert!(!dry_run);
            assert!(!all);
            assert_eq!(branch, Some("feat/x".to_string()));
        }
        _ => panic!("Expected Archive command"),
    }
}

#[test]
fn test_archive_branch_conflicts_with_all() {
    let result = Cli::try_parse_from(["task-mgr", "archive", "--branch", "feat/x", "--all"]);
    assert!(result.is_err(), "--branch and --all must conflict");
}

// =============================================================================
// Cross-command edge cases (TEST-INIT-004)
// =============================================================================

#[test]
fn test_loop_with_global_verbose_flag() {
    // Test that the global -v flag doesn't conflict with loop's --verbose
    let cli = Cli::parse_from([
        "task-mgr",
        "-v",
        "loop",
        ".task-mgr/tasks/prd.json",
        "--verbose",
    ]);
    assert!(cli.verbose); // global verbose
    match cli.command {
        Commands::Loop { verbose, .. } => {
            assert!(verbose); // loop-specific verbose
        }
        _ => panic!("Expected Loop command"),
    }
}

#[test]
fn test_status_verbose_does_not_conflict_with_global() {
    // Status -v is its own flag, distinct from global -v
    let cli = Cli::parse_from(["task-mgr", "status", "-v"]);
    match cli.command {
        Commands::Status { verbose, .. } => {
            assert!(verbose);
        }
        _ => panic!("Expected Status command"),
    }
}
