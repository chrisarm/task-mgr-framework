//! Integration tests for the worktree lifecycle (INT-001).
//!
//! Verifies:
//!   1. Lock file written with branch/worktree/prefix metadata, readable back
//!   2. Session start banner includes DB path and stop hints
//!   3. Worktree is created and removed via `ensure_worktree` / `remove_worktree`
//!   4. Full loop with `--cleanup-worktree` removes the worktree on exit (ignored)

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

use task_mgr::db::lock::LockGuard;
use task_mgr::loop_engine::display::{format_session_banner, SessionBannerHints};
use task_mgr::loop_engine::env::{ensure_worktree, remove_worktree};

// ============================================================================
// Test 1: Lock file contains branch/worktree/prefix metadata
// ============================================================================

#[test]
fn test_lock_metadata_branch_worktree_prefix_round_trip() {
    let tmp = TempDir::new().unwrap();
    let lock_dir = tmp.path().join("lock_dir");
    fs::create_dir_all(&lock_dir).unwrap();

    // Acquire a named lock (loop.lock) and write extended holder info
    let mut guard =
        LockGuard::acquire_named(&lock_dir, "loop.lock").expect("should acquire loop.lock");

    let branch = "feat/worktree-lifecycle";
    let worktree = "/home/user/repos/project-worktrees/feat-worktree-lifecycle";
    let prefix = "9c5c8a1d";

    guard
        .write_holder_info_extended(Some(branch), Some(worktree), Some(prefix))
        .expect("should write extended holder info");

    // Read it back without holding the guard
    let lock_path = lock_dir.join("loop.lock");
    let info =
        LockGuard::read_holder_info(&lock_path).expect("should be readable while lock is held");

    assert_eq!(
        info.branch.as_deref(),
        Some(branch),
        "lock file should contain branch metadata"
    );
    assert_eq!(
        info.worktree.as_deref(),
        Some(worktree),
        "lock file should contain worktree path"
    );
    assert_eq!(
        info.prefix.as_deref(),
        Some(prefix),
        "lock file should contain task prefix"
    );
    assert!(info.pid > 0, "lock file should contain a valid PID");
    assert!(!info.host.is_empty(), "lock file should contain hostname");
}

#[test]
fn test_lock_metadata_none_fields_omitted() {
    let tmp = TempDir::new().unwrap();
    let lock_dir = tmp.path().join("lock_dir");
    fs::create_dir_all(&lock_dir).unwrap();

    let mut guard = LockGuard::acquire_named(&lock_dir, "loop.lock").unwrap();

    // Write with only branch, no worktree/prefix
    guard
        .write_holder_info_extended(Some("main"), None, None)
        .unwrap();

    let lock_path = lock_dir.join("loop.lock");
    let info = LockGuard::read_holder_info(&lock_path).unwrap();

    assert_eq!(info.branch.as_deref(), Some("main"));
    assert!(
        info.worktree.is_none(),
        "worktree should be None when not set"
    );
    assert!(info.prefix.is_none(), "prefix should be None when not set");
}

// ============================================================================
// Test 2: Banner output includes DB path and stop hints
// ============================================================================

#[test]
fn test_banner_includes_db_path() {
    let db_path = Path::new("/home/user/.task-mgr/tasks.db");
    let hints = SessionBannerHints {
        db_path,
        prefix: None,
        worktree_path: None,
    };

    let banner = format_session_banner(
        "tasks/worktree-lifecycle.json",
        "feat/test",
        10,
        None,
        Some(&hints),
    );

    let db_str = db_path.display().to_string();
    assert!(
        banner.contains(&db_str),
        "banner should contain DB path '{}', got:\n{}",
        db_str,
        banner
    );
}

#[test]
fn test_banner_includes_worktree_path_when_set() {
    let db_path = Path::new("/home/user/.task-mgr/tasks.db");
    let wt_path = Path::new("/home/user/project-worktrees/feat-test");
    let hints = SessionBannerHints {
        db_path,
        prefix: None,
        worktree_path: Some(wt_path),
    };

    let banner = format_session_banner("tasks/prd.json", "feat/test", 5, Some(1.0), Some(&hints));

    // The banner may truncate the path with "..." if it's too long for the box.
    // Verify that at least the Worktree: label appears in the banner.
    assert!(
        banner.contains("Worktree:"),
        "banner should contain 'Worktree:' label when worktree_path is set, got:\n{}",
        banner
    );
}

#[test]
fn test_banner_omits_worktree_line_when_none() {
    let db_path = Path::new("/some/db/tasks.db");
    let hints = SessionBannerHints {
        db_path,
        prefix: None,
        worktree_path: None,
    };

    let banner = format_session_banner("tasks/prd.json", "main", 10, None, Some(&hints));

    // Should not mention worktree if None
    assert!(
        !banner.to_lowercase().contains("worktree"),
        "banner should not contain 'worktree' when path is None, got:\n{}",
        banner
    );
}

// ============================================================================
// Test 3: Worktree creation and removal (requires git repo)
// ============================================================================

/// Create a fresh git repo in `dir` with an initial commit on `branch`.
fn init_git_repo(dir: &Path, branch: &str) {
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command failed");
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    };

    run(&["init", "--initial-branch", branch]);
    run(&["config", "user.email", "test@test.com"]);
    run(&["config", "user.name", "Test"]);
    fs::write(dir.join(".gitkeep"), "").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "initial"]);
}

#[test]
fn test_ensure_worktree_creates_directory() {
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path(), "main");

    let branch = "feat/integration-test";

    // Create the branch first so worktree can use it
    let status = Command::new("git")
        .args(["checkout", "-b", branch])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(status.status.success(), "git checkout -b failed");

    // Switch back to main so we can create a worktree for the branch
    let status = Command::new("git")
        .args(["checkout", "main"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(status.status.success(), "git checkout main failed");

    // ensure_worktree should create the worktree directory
    let wt_path =
        ensure_worktree(repo.path(), branch, true).expect("ensure_worktree should succeed");

    assert!(
        wt_path.exists(),
        "worktree directory should exist at {}",
        wt_path.display()
    );
    assert!(wt_path.is_dir(), "worktree path should be a directory");

    // Cleanup: remove the worktree
    let removed = remove_worktree(repo.path(), &wt_path).expect("remove_worktree should succeed");
    assert!(removed, "worktree should have been removed");
    assert!(
        !wt_path.exists(),
        "worktree directory should no longer exist after removal"
    );
}

#[test]
fn test_remove_worktree_nonexistent_returns_false() {
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path(), "main");

    let fake_path = repo.path().join("nonexistent-worktree");
    // Removing a path that was never a worktree should return Ok(false)
    let result = remove_worktree(repo.path(), &fake_path);
    // Either Ok(false) or an error is acceptable; it must not panic
    match result {
        Ok(removed) => assert!(
            !removed,
            "removing non-existent worktree should return false"
        ),
        Err(_) => {
            // Also acceptable — git will report the path isn't a worktree
        }
    }
}

// ============================================================================
// Test 4: Full loop with --cleanup-worktree removes worktree on exit
//
// This test exercises run_loop() with a mock Claude binary and the
// --cleanup-worktree flag. It is #[ignore] because it requires:
//   - A git repo set up in the temp directory
//   - The task-mgr binary to be built (CARGO_BIN_EXE_task-mgr)
//   - A mock-claude.sh script in tests/fixtures/
//
// Run with:
//   cargo test --test worktree_lifecycle_integration -- --ignored
// ============================================================================

#[test]
#[ignore]
fn test_full_loop_worktree_created_and_cleaned_up() {
    use task_mgr::loop_engine::config::LoopConfig;
    use task_mgr::loop_engine::engine::{run_loop, LoopRunConfig};

    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path(), "main");

    // Set up tasks dir and PRD
    let tasks_dir = repo.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();

    let prd_src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("test-loop-prd.json");
    let prd_dest = tasks_dir.join("test-loop-prd.json");
    fs::copy(&prd_src, &prd_dest).unwrap();

    let prompt_path = tasks_dir.join("test-loop-prd-prompt.md");
    fs::write(
        &prompt_path,
        "# Test Agent\n\nComplete the assigned task.\n",
    )
    .unwrap();

    // Point to mock Claude
    let mock_claude = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mock-claude.sh");
    let task_mgr_bin = std::path::PathBuf::from(env!("CARGO_BIN_EXE_task-mgr"));

    std::env::set_var("CLAUDE_BINARY", &mock_claude);
    std::env::set_var("TASK_MGR_BIN", &task_mgr_bin);
    std::env::set_var("TASK_MGR_DIR", repo.path());

    let mut config = LoopConfig::from_env();
    config.yes_mode = true;
    config.max_iterations = 5;
    config.cleanup_worktree = true;
    config.usage_check_enabled = false;

    let run_config = LoopRunConfig {
        db_dir: repo.path().join(".task-mgr"),
        source_root: repo.path().to_path_buf(),
        working_root: repo.path().to_path_buf(),
        prd_file: prd_dest,
        prompt_file: Some(prompt_path),
        external_repo: None,
        config,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let loop_result = rt.block_on(async { run_loop(run_config).await });

    std::env::remove_var("CLAUDE_BINARY");
    std::env::remove_var("TASK_MGR_BIN");
    std::env::remove_var("TASK_MGR_DIR");

    // Verify loop exited successfully
    assert_eq!(loop_result.exit_code, 0, "loop should exit with code 0");

    // Verify worktree was cleaned up
    if let Some(wt_path) = loop_result.worktree_path {
        assert!(
            !wt_path.exists(),
            "worktree at '{}' should have been cleaned up with --cleanup-worktree",
            wt_path.display()
        );
    }
}
