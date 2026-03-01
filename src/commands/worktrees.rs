//! Worktree lifecycle management command.
//!
//! Provides list, prune, and remove actions for git worktrees managed by task-mgr.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use fs2::FileExt;

use crate::db::LockGuard;
use crate::error::{TaskMgrError, TaskMgrResult};
use crate::loop_engine::env::{parse_worktree_list, remove_worktree};

/// Status of a worktree's active lock.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LockStatus {
    /// An active loop.lock is held for this worktree.
    Locked,
    /// No active lock found for this worktree.
    Unlocked,
}

impl std::fmt::Display for LockStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockStatus::Locked => write!(f, "LOCKED"),
            LockStatus::Unlocked => write!(f, "unlocked"),
        }
    }
}

/// Summary of a single git worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// Absolute path to the worktree directory.
    pub path: PathBuf,
    /// Branch checked out in this worktree, if any.
    pub branch: Option<String>,
    /// Whether an active lock is held for this worktree.
    pub lock_status: LockStatus,
}

/// Result of a worktrees command action.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorktreesResult {
    /// Action that was performed.
    pub action: String,
    /// Worktrees encountered (all for list, affected for prune/remove).
    pub worktrees: Vec<WorktreeInfo>,
    /// Human-readable summary message.
    pub message: String,
}

// ============================================================================
// Lock file scanning
// ============================================================================

/// Read all .lock files from db_dir and collect the worktree paths they claim.
///
/// Returns a set of absolute worktree path strings that have an active lock file.
/// Skips lock files that cannot be parsed.
fn locked_worktree_paths(db_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(db_dir) else {
        return vec![];
    };

    let mut locked = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        if let Some(info) = LockGuard::read_holder_info(&path) {
            if let Some(wt) = info.worktree {
                // Verify the flock is actually held (not stale from SIGKILL)
                if is_flock_held(&path) {
                    locked.push(wt);
                }
            }
        }
    }
    locked
}

/// Check if a file has an active flock held by another process.
///
/// Attempts a non-blocking exclusive lock. If it succeeds, the lock file is stale
/// (no process holds the flock). If it fails with WouldBlock, a process holds it.
fn is_flock_held(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            // Lock acquired — it was stale. Release immediately.
            let _ = file.unlock();
            false
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(_) => false,
    }
}

/// Determine the lock status for a worktree path given the set of locked paths.
fn lock_status_for(worktree_path: &Path, locked_paths: &[String]) -> LockStatus {
    let path_str = worktree_path.to_string_lossy();
    if locked_paths.iter().any(|lp| lp == path_str.as_ref()) {
        LockStatus::Locked
    } else {
        LockStatus::Unlocked
    }
}

// ============================================================================
// Commands
// ============================================================================

/// List all git worktrees with branch, path, and lock status.
///
/// # Arguments
/// * `db_dir` - Path to the .task-mgr directory (for lock file scanning).
/// * `source_root` - Path to the main git repository.
pub fn list(db_dir: &Path, source_root: &Path) -> TaskMgrResult<WorktreesResult> {
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(source_root)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(
                source_root.display().to_string(),
                "listing git worktrees",
                e,
            )
        })?;

    if !output.status.success() {
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktrees".to_string(),
            id: source_root.display().to_string(),
            expected: "successful git worktree list".to_string(),
            actual: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let parsed = parse_worktree_list(&raw);
    let locked_paths = locked_worktree_paths(db_dir);

    let worktrees: Vec<WorktreeInfo> = parsed
        .into_iter()
        .map(|(path, branch)| {
            let lock_status = lock_status_for(&path, &locked_paths);
            WorktreeInfo {
                path,
                branch,
                lock_status,
            }
        })
        .collect();

    let count = worktrees.len();
    Ok(WorktreesResult {
        action: "list".to_string(),
        worktrees,
        message: format!("{} worktree(s) found", count),
    })
}

/// Prune worktrees with no active lock by removing them and then running `git worktree prune`.
///
/// # Arguments
/// * `db_dir` - Path to the .task-mgr directory.
/// * `source_root` - Path to the main git repository.
pub fn prune(db_dir: &Path, source_root: &Path) -> TaskMgrResult<WorktreesResult> {
    let list_result = list(db_dir, source_root)?;

    let mut removed = Vec::new();
    let mut skipped = Vec::new();

    for wt in &list_result.worktrees {
        // Skip the main worktree (always first, always unlocked but keep it)
        // Canonicalize to handle trailing slashes, symlinks, mount differences
        if wt.path.canonicalize().ok() == source_root.canonicalize().ok() {
            continue;
        }

        if wt.lock_status == LockStatus::Locked {
            skipped.push(wt.clone());
            continue;
        }

        // Remove unlocked worktree; skip if dirty
        match remove_worktree(source_root, &wt.path) {
            Ok(true) => removed.push(wt.clone()),
            Ok(false) => {
                // dirty — treat as skipped
                skipped.push(wt.clone());
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to remove worktree {}: {}",
                    wt.path.display(),
                    e
                );
                skipped.push(wt.clone());
            }
        }
    }

    let removed_count = removed.len();
    let skipped_count = skipped.len();

    Ok(WorktreesResult {
        action: "prune".to_string(),
        worktrees: removed,
        message: format!(
            "Pruned {} worktree(s); skipped {} (locked or dirty)",
            removed_count, skipped_count
        ),
    })
}

/// Remove a specific worktree by path or branch name.
///
/// # Arguments
/// * `db_dir` - Path to the .task-mgr directory.
/// * `source_root` - Path to the main git repository.
/// * `target` - Path or branch name of the worktree to remove.
pub fn remove(db_dir: &Path, source_root: &Path, target: &str) -> TaskMgrResult<WorktreesResult> {
    let list_result = list(db_dir, source_root)?;

    // Find by exact path match or branch name match
    let wt = list_result
        .worktrees
        .iter()
        .find(|w| w.path.to_string_lossy() == target || w.branch.as_deref() == Some(target));

    let wt = wt.ok_or_else(|| TaskMgrError::InvalidState {
        resource_type: "Git worktree".to_string(),
        id: target.to_string(),
        expected: "a known worktree path or branch name".to_string(),
        actual: "no matching worktree found".to_string(),
    })?;

    if wt.lock_status == LockStatus::Locked {
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktree".to_string(),
            id: target.to_string(),
            expected: "unlocked worktree".to_string(),
            actual: "worktree has an active lock — stop the loop first".to_string(),
        });
    }

    let worktree_path = wt.path.clone();
    let wt_info = wt.clone();

    let removed = remove_worktree(source_root, &worktree_path)?;
    if !removed {
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktree".to_string(),
            id: target.to_string(),
            expected: "clean worktree".to_string(),
            actual: "worktree has uncommitted changes — not removed".to_string(),
        });
    }

    Ok(WorktreesResult {
        action: "remove".to_string(),
        worktrees: vec![wt_info],
        message: format!("Removed worktree at {}", worktree_path.display()),
    })
}

// ============================================================================
// Text formatting
// ============================================================================

/// Format a `WorktreesResult` as human-readable text.
pub fn format_text(result: &WorktreesResult) -> String {
    let mut out = String::new();

    match result.action.as_str() {
        "list" => {
            if result.worktrees.is_empty() {
                out.push_str("No worktrees found.\n");
            } else {
                let path_width = result
                    .worktrees
                    .iter()
                    .map(|wt| wt.path.to_string_lossy().len())
                    .max()
                    .unwrap_or(4) // "PATH".len()
                    .max(50);
                out.push_str(&format!(
                    "{:<pw$} {:<30} {}\n",
                    "PATH",
                    "BRANCH",
                    "LOCK",
                    pw = path_width
                ));
                out.push_str(&format!(
                    "{:-<pw$} {:-<30} {:-<8}\n",
                    "",
                    "",
                    "",
                    pw = path_width
                ));
                for wt in &result.worktrees {
                    let branch = wt.branch.as_deref().unwrap_or("(detached)");
                    out.push_str(&format!(
                        "{:<pw$} {:<30} {}\n",
                        wt.path.display(),
                        branch,
                        wt.lock_status,
                        pw = path_width
                    ));
                }
            }
        }
        "prune" => {
            out.push_str(&format!("{}\n", result.message));
            for wt in &result.worktrees {
                out.push_str(&format!("  removed: {}\n", wt.path.display()));
            }
        }
        "remove" => {
            out.push_str(&format!("{}\n", result.message));
        }
        _ => {
            out.push_str(&format!("{}\n", result.message));
        }
    }

    out
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::process::Command;
    use tempfile::TempDir;

    // ── helpers ────────────────────────────────────────────────────────────────

    /// Initialize a temporary git repository with a single commit.
    fn init_test_repo() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("create temp dir");
        let repo = tmp.path().to_path_buf();
        Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&repo)
            .output()
            .ok();
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&repo)
            .output()
            .ok();
        fs::write(repo.join("README.md"), "# Test").expect("write README");
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo)
            .output()
            .expect("git commit");
        (tmp, repo)
    }

    /// Write a lock file that claims the given worktree path is locked.
    /// Does NOT hold an flock — useful for testing stale lock detection.
    fn write_lock_file(lock_path: &Path, worktree_path: &str) {
        let content = format!(
            "12345@testhost\nbranch=test\nworktree={}\nprefix=test\n",
            worktree_path
        );
        fs::write(lock_path, content).expect("write lock file");
    }

    /// Write a lock file AND hold an exclusive flock on it.
    /// Returns the File handle — the flock is released when dropped.
    fn write_lock_file_with_flock(lock_path: &Path, worktree_path: &str) -> File {
        use std::io::Write;
        let content = format!(
            "12345@testhost\nbranch=test\nworktree={}\nprefix=test\n",
            worktree_path
        );
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(lock_path)
            .expect("open lock file");
        file.write_all(content.as_bytes()).expect("write lock file");
        file.sync_all().expect("sync lock file");
        fs2::FileExt::lock_exclusive(&file).expect("acquire flock");
        file
    }

    // ── lock_status_for ────────────────────────────────────────────────────────

    #[test]
    fn test_lock_status_for_locked() {
        let wt_path = PathBuf::from("/some/worktree/path");
        let locked = vec!["/some/worktree/path".to_string()];
        assert_eq!(lock_status_for(&wt_path, &locked), LockStatus::Locked);
    }

    #[test]
    fn test_lock_status_for_unlocked() {
        let wt_path = PathBuf::from("/some/worktree/path");
        let locked = vec!["/different/path".to_string()];
        assert_eq!(lock_status_for(&wt_path, &locked), LockStatus::Unlocked);
    }

    #[test]
    fn test_lock_status_for_empty_locked_set() {
        let wt_path = PathBuf::from("/any/path");
        assert_eq!(lock_status_for(&wt_path, &[]), LockStatus::Unlocked);
    }

    // ── locked_worktree_paths ──────────────────────────────────────────────────

    #[test]
    fn test_locked_worktree_paths_reads_lock_files() {
        let tmp = TempDir::new().expect("create temp dir");
        let worktree_path = "/path/to/my/worktree";
        let _flock = write_lock_file_with_flock(&tmp.path().join("loop.lock"), worktree_path);

        let paths = locked_worktree_paths(tmp.path());
        assert!(
            paths.contains(&worktree_path.to_string()),
            "Expected locked path to appear, got: {:?}",
            paths
        );
    }

    #[test]
    fn test_locked_worktree_paths_treats_stale_lock_as_unlocked() {
        let tmp = TempDir::new().expect("create temp dir");
        let worktree_path = "/path/to/stale/worktree";
        // Write lock file WITHOUT holding flock (simulates SIGKILL'd process)
        write_lock_file(&tmp.path().join("loop.lock"), worktree_path);

        let paths = locked_worktree_paths(tmp.path());
        assert!(
            paths.is_empty(),
            "Stale lock (no flock held) should be treated as unlocked, got: {:?}",
            paths
        );
    }

    #[test]
    fn test_locked_worktree_paths_ignores_non_lock_files() {
        let tmp = TempDir::new().expect("create temp dir");
        fs::write(tmp.path().join("tasks.db"), "db content").expect("write db");
        let paths = locked_worktree_paths(tmp.path());
        assert!(paths.is_empty(), "Non-.lock files should be ignored");
    }

    #[test]
    fn test_locked_worktree_paths_empty_dir() {
        let tmp = TempDir::new().expect("create temp dir");
        let paths = locked_worktree_paths(tmp.path());
        assert!(paths.is_empty());
    }

    // ── format_text ───────────────────────────────────────────────────────────

    #[test]
    fn test_format_text_list_shows_branch_path_lock() {
        let result = WorktreesResult {
            action: "list".to_string(),
            worktrees: vec![
                WorktreeInfo {
                    path: PathBuf::from("/repo/main"),
                    branch: Some("main".to_string()),
                    lock_status: LockStatus::Unlocked,
                },
                WorktreeInfo {
                    path: PathBuf::from("/repo-worktrees/feat"),
                    branch: Some("feat/cool".to_string()),
                    lock_status: LockStatus::Locked,
                },
            ],
            message: "2 worktree(s) found".to_string(),
        };

        let text = format_text(&result);
        assert!(text.contains("/repo/main"), "should contain main path");
        assert!(text.contains("main"), "should contain main branch");
        assert!(
            text.contains("/repo-worktrees/feat"),
            "should contain worktree path"
        );
        assert!(text.contains("feat/cool"), "should contain worktree branch");
        assert!(text.contains("LOCKED"), "should show LOCKED status");
        assert!(text.contains("unlocked"), "should show unlocked status");
    }

    #[test]
    fn test_format_text_list_empty() {
        let result = WorktreesResult {
            action: "list".to_string(),
            worktrees: vec![],
            message: "0 worktree(s) found".to_string(),
        };
        let text = format_text(&result);
        assert!(text.contains("No worktrees found"));
    }

    #[test]
    fn test_format_text_list_detached_head() {
        let result = WorktreesResult {
            action: "list".to_string(),
            worktrees: vec![WorktreeInfo {
                path: PathBuf::from("/repo/main"),
                branch: None,
                lock_status: LockStatus::Unlocked,
            }],
            message: "1 worktree(s) found".to_string(),
        };
        let text = format_text(&result);
        assert!(
            text.contains("(detached)"),
            "detached HEAD should show '(detached)'"
        );
    }

    #[test]
    fn test_format_text_prune_shows_removed() {
        let result = WorktreesResult {
            action: "prune".to_string(),
            worktrees: vec![WorktreeInfo {
                path: PathBuf::from("/repo-worktrees/old-feat"),
                branch: Some("old-feat".to_string()),
                lock_status: LockStatus::Unlocked,
            }],
            message: "Pruned 1 worktree(s); skipped 0 (locked or dirty)".to_string(),
        };
        let text = format_text(&result);
        assert!(text.contains("Pruned 1 worktree(s)"));
        assert!(text.contains("removed:"));
        assert!(text.contains("old-feat"));
    }

    #[test]
    fn test_format_text_remove_shows_message() {
        let result = WorktreesResult {
            action: "remove".to_string(),
            worktrees: vec![WorktreeInfo {
                path: PathBuf::from("/repo-worktrees/feat"),
                branch: Some("feat".to_string()),
                lock_status: LockStatus::Unlocked,
            }],
            message: "Removed worktree at /repo-worktrees/feat".to_string(),
        };
        let text = format_text(&result);
        assert!(text.contains("Removed worktree at"));
    }

    // ── list (git integration) ─────────────────────────────────────────────────

    #[test]
    fn test_list_shows_correct_branch_path_lock_status() {
        let (tmp, repo) = init_test_repo();
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).expect("create db dir");

        let result = list(&db_dir, &repo).expect("list should succeed");
        assert!(
            !result.worktrees.is_empty(),
            "should find at least the main worktree"
        );
        assert_eq!(result.action, "list");

        // Main worktree should be present and unlocked
        let main_wt = &result.worktrees[0];
        assert!(
            main_wt.path.exists(),
            "main worktree path should exist on disk"
        );
        assert_eq!(
            main_wt.lock_status,
            LockStatus::Unlocked,
            "main worktree should be unlocked with no lock files"
        );
        assert!(
            main_wt.branch.is_some(),
            "main worktree should have a branch"
        );
    }

    #[test]
    fn test_list_shows_locked_when_lock_file_present() {
        let (tmp, repo) = init_test_repo();
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).expect("create db dir");

        // Write a lock file claiming the repo path is active, with an actual flock
        let repo_str = repo.to_string_lossy().to_string();
        let _flock = write_lock_file_with_flock(&db_dir.join("loop.lock"), &repo_str);

        let result = list(&db_dir, &repo).expect("list should succeed");
        let main_wt = result.worktrees.iter().find(|w| w.path == repo);
        assert!(main_wt.is_some(), "should find main worktree by path");
        assert_eq!(
            main_wt.unwrap().lock_status,
            LockStatus::Locked,
            "should show LOCKED when lock file claims this worktree"
        );
    }

    // ── prune (git integration) ────────────────────────────────────────────────

    #[test]
    fn test_prune_skips_locked_worktrees() {
        let (tmp, repo) = init_test_repo();
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).expect("create db dir");

        // Create a second worktree
        let wt_path = tmp.path().join("feat-wt");
        Command::new("git")
            .args(["branch", "feat/test-prune"])
            .current_dir(&repo)
            .output()
            .expect("git branch");
        Command::new("git")
            .args([
                "worktree",
                "add",
                wt_path.to_str().expect("valid path"),
                "feat/test-prune",
            ])
            .current_dir(&repo)
            .output()
            .expect("git worktree add");

        // Lock the worktree via a lock file with actual flock
        let wt_str = wt_path.to_string_lossy().to_string();
        let _flock = write_lock_file_with_flock(&db_dir.join("loop.lock"), &wt_str);

        let result = prune(&db_dir, &repo).expect("prune should succeed");

        // The locked worktree should NOT appear in the removed list
        let removed_paths: Vec<&PathBuf> = result.worktrees.iter().map(|w| &w.path).collect();
        assert!(
            !removed_paths.iter().any(|p| **p == wt_path),
            "locked worktree should not be pruned, removed: {:?}",
            removed_paths
        );
        // The worktree should still exist on disk
        assert!(
            wt_path.exists(),
            "locked worktree directory should still exist after prune"
        );
        assert!(
            result.message.contains("skipped"),
            "prune message should mention skipped count: {}",
            result.message
        );
    }

    // ── remove (git integration) ───────────────────────────────────────────────

    #[test]
    fn test_remove_by_branch_name() {
        let (tmp, repo) = init_test_repo();
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).expect("create db dir");

        let wt_path = tmp.path().join("remove-branch-wt");
        Command::new("git")
            .args(["branch", "feat/remove-test"])
            .current_dir(&repo)
            .output()
            .expect("git branch");
        Command::new("git")
            .args([
                "worktree",
                "add",
                wt_path.to_str().expect("valid path"),
                "feat/remove-test",
            ])
            .current_dir(&repo)
            .output()
            .expect("git worktree add");

        assert!(wt_path.exists(), "worktree should exist before remove");

        let result = remove(&db_dir, &repo, "feat/remove-test")
            .expect("remove by branch name should succeed");
        assert_eq!(result.action, "remove");
        assert!(
            result.message.contains("Removed worktree"),
            "message: {}",
            result.message
        );
    }

    #[test]
    fn test_remove_by_path() {
        let (tmp, repo) = init_test_repo();
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).expect("create db dir");

        let wt_path = tmp.path().join("remove-path-wt");
        Command::new("git")
            .args(["branch", "feat/remove-by-path"])
            .current_dir(&repo)
            .output()
            .expect("git branch");
        Command::new("git")
            .args([
                "worktree",
                "add",
                wt_path.to_str().expect("valid path"),
                "feat/remove-by-path",
            ])
            .current_dir(&repo)
            .output()
            .expect("git worktree add");

        assert!(wt_path.exists(), "worktree should exist before remove");

        let wt_str = wt_path.to_string_lossy().to_string();
        let result = remove(&db_dir, &repo, &wt_str).expect("remove by path should succeed");
        assert_eq!(result.action, "remove");
        assert!(
            !result.worktrees.is_empty(),
            "result should contain the removed worktree"
        );
    }

    #[test]
    fn test_remove_locked_worktree_returns_error() {
        let (tmp, repo) = init_test_repo();
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).expect("create db dir");

        let wt_path = tmp.path().join("locked-wt");
        Command::new("git")
            .args(["branch", "feat/locked-remove"])
            .current_dir(&repo)
            .output()
            .expect("git branch");
        Command::new("git")
            .args([
                "worktree",
                "add",
                wt_path.to_str().expect("valid path"),
                "feat/locked-remove",
            ])
            .current_dir(&repo)
            .output()
            .expect("git worktree add");

        // Lock the worktree with actual flock
        let wt_str = wt_path.to_string_lossy().to_string();
        let _flock = write_lock_file_with_flock(&db_dir.join("loop.lock"), &wt_str);

        let result = remove(&db_dir, &repo, "feat/locked-remove");
        assert!(result.is_err(), "should fail to remove locked worktree");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("active lock"),
            "error should mention active lock: {}",
            err
        );
    }

    #[test]
    fn test_remove_nonexistent_target_returns_error() {
        let (tmp, repo) = init_test_repo();
        let db_dir = tmp.path().join(".task-mgr");
        fs::create_dir_all(&db_dir).expect("create db dir");

        let result = remove(&db_dir, &repo, "nonexistent-branch");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no matching worktree"),
            "error should say no matching worktree: {}",
            err
        );
    }
}
