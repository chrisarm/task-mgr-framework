//! Worktree lifecycle management command.
//!
//! Provides list, prune, and remove actions for git worktrees managed by task-mgr.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
                locked.push(wt);
            }
        }
    }
    locked
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
        // Heuristic: skip if path == source_root
        if wt.path == source_root {
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

    remove_worktree(source_root, &worktree_path)?;

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
                out.push_str(&format!("{:<50} {:<30} {}\n", "PATH", "BRANCH", "LOCK"));
                out.push_str(&format!("{:-<50} {:-<30} {:-<8}\n", "", "", ""));
                for wt in &result.worktrees {
                    let branch = wt.branch.as_deref().unwrap_or("(detached)");
                    out.push_str(&format!(
                        "{:<50} {:<30} {}\n",
                        wt.path.display(),
                        branch,
                        wt.lock_status
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
