//! Branch change detection for the loop engine.
//!
//! Detects when the git branch has changed between loop runs,
//! archives the previous PRD, and resets progress.txt.

use std::path::Path;

use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::archive;
use crate::loop_engine::env;

use super::LAST_BRANCH_FILE;

/// Detect if the git branch has changed since the last loop run.
///
/// Reads `.last-branch` from `tasks_dir` and compares to the current branch.
/// If the branch has changed:
/// - In `yes_mode`: auto-archives the previous PRD and resets progress.txt
/// - In interactive mode: prompts the user before archiving
///
/// If `.last-branch` doesn't exist (first run), writes the current branch
/// and returns `Ok(false)`.
///
/// Updates `.last-branch` to the current branch after processing.
///
/// # Arguments
///
/// * `dir` - Project/git root for branch detection
/// * `db_dir` - Database directory (`.task-mgr/`) passed to `run_archive`
/// * `tasks_dir` - Tasks directory for `.last-branch` and `progress.txt`
/// * `yes_mode` - Auto-confirm prompts
///
/// # Returns
///
/// `Ok(true)` if a branch change was detected (and handled), `Ok(false)` otherwise.
///
/// # Errors
///
/// Returns an error if git branch detection fails or the user declines in interactive mode.
pub fn detect_branch_change(dir: &Path, db_dir: &Path, tasks_dir: &Path, yes_mode: bool) -> TaskMgrResult<bool> {
    let last_branch_path = tasks_dir.join(LAST_BRANCH_FILE);

    // Get current branch
    let current_branch = env::get_current_branch(dir)?;

    // If .last-branch doesn't exist, this is the first run
    if !last_branch_path.exists() {
        write_last_branch(&last_branch_path, &current_branch)?;
        return Ok(false);
    }

    // Read the previous branch
    let previous_branch = std::fs::read_to_string(&last_branch_path)
        .map_err(|e| {
            TaskMgrError::io_error(
                last_branch_path.display().to_string(),
                "reading .last-branch",
                e,
            )
        })?
        .trim()
        .to_string();

    // No change — nothing to do
    if previous_branch == current_branch {
        return Ok(false);
    }

    eprintln!(
        "Branch change detected: '{}' -> '{}'",
        previous_branch, current_branch
    );

    // Prompt user (or auto-approve in yes_mode)
    if !yes_mode && !env::prompt_user_yn("Archive previous PRD and reset progress? [y/N] ")? {
        // User declined — just update .last-branch and continue
        write_last_branch(&last_branch_path, &current_branch)?;
        eprintln!(
            "Skipping archive. Updated .last-branch to '{}'.",
            current_branch
        );
        return Ok(true);
    }

    // Archive the previous PRD (best-effort — don't block the loop on failure)
    // Use the previous branch as filter so only that branch's PRDs get archived.
    match archive::run_archive(db_dir, false, Some(&previous_branch)) {
        Ok(result) => {
            if !result.archived.is_empty() {
                eprintln!(
                    "Archived {} file(s) from previous branch '{}'",
                    result.archived.len(),
                    previous_branch
                );
            }
        }
        Err(e) => {
            eprintln!(
                "Warning: failed to archive previous PRD: {} (continuing)",
                e
            );
        }
    }

    // Reset progress.txt for the new branch
    let progress_path = tasks_dir.join("progress.txt");
    if progress_path.exists() {
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let header = format!(
            "# Claude Code Progress Log\nStarted: {}\nBranch changed from '{}' to '{}'\n---\n",
            timestamp, previous_branch, current_branch
        );
        std::fs::write(&progress_path, header).map_err(|e| {
            TaskMgrError::io_error(
                progress_path.display().to_string(),
                "resetting progress.txt",
                e,
            )
        })?;
        eprintln!("Reset progress.txt for new branch '{}'", current_branch);
    }

    // Update .last-branch
    write_last_branch(&last_branch_path, &current_branch)?;

    Ok(true)
}

/// Write the current branch name to .last-branch file.
fn write_last_branch(path: &Path, branch: &str) -> TaskMgrResult<()> {
    std::fs::write(path, format!("{}\n", branch))
        .map_err(|e| TaskMgrError::io_error(path.display().to_string(), "writing .last-branch", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    use super::super::LAST_BRANCH_FILE;

    /// Helper to set up a git repo with initial commit on a given branch.
    fn setup_git_repo(branch: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("create temp dir");
        Command::new("git")
            .args(["init", "-b", branch])
            .current_dir(tmp.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .output()
            .expect("git config name");
        fs::write(tmp.path().join("file.txt"), "content").expect("write file");
        Command::new("git")
            .args(["add", "."])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit");
        tmp
    }

    // --- write_last_branch ---

    #[test]
    fn test_write_last_branch_creates_file() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let path = tmp.path().join(LAST_BRANCH_FILE);

        write_last_branch(&path, "main").expect("write last branch");

        let content = fs::read_to_string(&path).expect("read");
        assert_eq!(content, "main\n");
    }

    #[test]
    fn test_write_last_branch_overwrites_existing() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let path = tmp.path().join(LAST_BRANCH_FILE);

        write_last_branch(&path, "old-branch").expect("first write");
        write_last_branch(&path, "new-branch").expect("second write");

        let content = fs::read_to_string(&path).expect("read");
        assert_eq!(content, "new-branch\n");
    }

    // --- detect_branch_change ---

    #[test]
    fn test_detect_branch_change_first_run_no_last_branch() {
        let tmp = setup_git_repo("main");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");

        let result = detect_branch_change(tmp.path(), tmp.path(), &tasks_dir, true).expect("detect");
        assert!(!result, "First run should return false (no change)");

        // .last-branch should be created
        let content = fs::read_to_string(tasks_dir.join(LAST_BRANCH_FILE)).expect("read");
        assert_eq!(content.trim(), "main");
    }

    #[test]
    fn test_detect_branch_change_same_branch_no_change() {
        let tmp = setup_git_repo("main");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");
        fs::write(tasks_dir.join(LAST_BRANCH_FILE), "main\n").expect("write .last-branch");

        let result = detect_branch_change(tmp.path(), tmp.path(), &tasks_dir, true).expect("detect");
        assert!(!result, "Same branch should return false");
    }

    #[test]
    fn test_detect_branch_change_detected_yes_mode() {
        let tmp = setup_git_repo("new-feature");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");

        // Set .last-branch to a different branch
        fs::write(tasks_dir.join(LAST_BRANCH_FILE), "old-branch\n").expect("write .last-branch");

        let result = detect_branch_change(tmp.path(), tmp.path(), &tasks_dir, true).expect("detect");
        assert!(result, "Branch change should return true");

        // .last-branch should be updated
        let content = fs::read_to_string(tasks_dir.join(LAST_BRANCH_FILE)).expect("read");
        assert_eq!(content.trim(), "new-feature");
    }

    #[test]
    fn test_detect_branch_change_resets_progress() {
        let tmp = setup_git_repo("new-feature");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");

        // Create a progress.txt with old content
        fs::write(
            tasks_dir.join("progress.txt"),
            "# Old Progress\nSome old content\n",
        )
        .expect("write progress");

        // Set .last-branch to a different branch
        fs::write(tasks_dir.join(LAST_BRANCH_FILE), "old-branch\n").expect("write .last-branch");

        detect_branch_change(tmp.path(), tmp.path(), &tasks_dir, true).expect("detect");

        // Progress should be reset (not old content)
        let content = fs::read_to_string(tasks_dir.join("progress.txt")).expect("read progress");
        assert!(
            content.contains("# Claude Code Progress Log"),
            "Should contain new header, got: {}",
            content
        );
        assert!(
            content.contains("Branch changed from 'old-branch' to 'new-feature'"),
            "Should contain branch change note, got: {}",
            content
        );
        assert!(
            !content.contains("Some old content"),
            "Old content should be gone"
        );
    }

    #[test]
    fn test_detect_branch_change_no_progress_file_does_not_fail() {
        let tmp = setup_git_repo("new-feature");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");

        // No progress.txt exists
        fs::write(tasks_dir.join(LAST_BRANCH_FILE), "old-branch\n").expect("write .last-branch");

        let result = detect_branch_change(tmp.path(), tmp.path(), &tasks_dir, true);
        assert!(
            result.is_ok(),
            "Should not fail without progress.txt: {:?}",
            result.err()
        );
        assert!(
            result.unwrap(),
            "Should still return true for branch change"
        );

        // progress.txt should NOT be created (it didn't exist before)
        assert!(
            !tasks_dir.join("progress.txt").exists(),
            "progress.txt should not be created when it didn't exist"
        );
    }

    #[test]
    fn test_detect_branch_change_empty_last_branch() {
        let tmp = setup_git_repo("main");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");

        // Empty .last-branch
        fs::write(tasks_dir.join(LAST_BRANCH_FILE), "").expect("write .last-branch");

        let result = detect_branch_change(tmp.path(), tmp.path(), &tasks_dir, true).expect("detect");
        // Empty string != "main", so this is a branch change
        assert!(result, "Empty .last-branch should trigger change detection");

        let content = fs::read_to_string(tasks_dir.join(LAST_BRANCH_FILE)).expect("read");
        assert_eq!(content.trim(), "main");
    }

    #[test]
    fn test_detect_branch_change_whitespace_trimmed() {
        let tmp = setup_git_repo("main");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");

        // .last-branch with trailing whitespace
        fs::write(tasks_dir.join(LAST_BRANCH_FILE), "  main  \n").expect("write .last-branch");

        let result = detect_branch_change(tmp.path(), tmp.path(), &tasks_dir, true).expect("detect");
        assert!(!result, "Should trim whitespace and detect same branch");
    }

    #[test]
    fn test_detect_branch_change_fails_outside_git_repo() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let tasks_dir = tmp.path().join("tasks");
        fs::create_dir_all(&tasks_dir).expect("create tasks dir");

        let result = detect_branch_change(tmp.path(), tmp.path(), &tasks_dir, true);
        assert!(
            result.is_err(),
            "Should fail outside git repo: {:?}",
            result.ok()
        );
    }
}
