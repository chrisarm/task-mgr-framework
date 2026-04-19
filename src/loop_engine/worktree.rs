//! Worktree lifecycle management for the loop engine.
//!
//! Provides functions to sanitize branch names, compute worktree paths,
//! detect worktree context, parse git worktree output, and create/remove
//! git worktrees with proper cleanup.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{TaskMgrError, TaskMgrResult};

use super::env::{get_current_branch, prompt_user_yn};

/// Silently ignore errors (best-effort cleanup).
fn cleanup_empty_dir(path: &Path) {
    if path.exists()
        && let Ok(mut entries) = std::fs::read_dir(path)
        && entries.next().is_none()
    {
        let _ = std::fs::remove_dir(path);
    }
}

/// Replace `/`, spaces, and other problematic characters with `-`.
fn sanitize_branch_name(branch_name: &str) -> String {
    branch_name
        .chars()
        .map(|c| match c {
            '/' | ' ' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => c,
        })
        .collect()
}

/// Return `{repo-parent}/{repo-name}-worktrees/{sanitized-branch-name}/`.
pub(crate) fn compute_worktree_path(project_root: &Path, branch_name: &str) -> PathBuf {
    let repo_name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    let parent = project_root.parent().unwrap_or(project_root);
    let worktrees_dir = parent.join(format!("{}-worktrees", repo_name));
    let sanitized = sanitize_branch_name(branch_name);

    worktrees_dir.join(sanitized)
}

fn is_inside_worktree(dir: &Path) -> TaskMgrResult<bool> {
    crate::git::is_inside_worktree_at(dir)
        .map_err(|e| TaskMgrError::io_error(dir.display().to_string(), "running git rev-parse", e))
}

/// Return a list of (worktree_path, branch_name) tuples.
pub(crate) fn parse_worktree_list(output: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // Save previous worktree if any
            if let Some(p) = current_path.take() {
                worktrees.push((p, current_branch.take()));
            }
            current_path = Some(PathBuf::from(path));
            current_branch = None;
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = Some(branch.to_string());
        }
    }

    // Don't forget the last one
    if let Some(p) = current_path {
        worktrees.push((p, current_branch));
    }

    worktrees
}

/// Create a worktree at `{repo-parent}/{repo-name}-worktrees/{sanitized-branch}/`
/// if one doesn't already exist for this branch.
///
/// # Arguments
///
/// * `project_root` - Path to the main git repository
/// * `branch_name` - Target branch name
/// * `yes_mode` - If false, prompts user before creating worktree
/// * `start_point` - Optional git ref to branch from when creating a NEW branch.
///   Passed as `-- <start_point>` to prevent flag injection. Ignored if the branch
///   already exists.
///
/// # Returns
///
/// Path to the worktree directory (existing or newly created).
///
/// # Errors
///
/// Returns an error if:
/// - Git commands fail
/// - User declines to create worktree (interactive mode)
/// - Already inside a worktree for a different branch
pub fn ensure_worktree(
    project_root: &Path,
    branch_name: &str,
    yes_mode: bool,
    start_point: Option<&str>,
) -> TaskMgrResult<PathBuf> {
    // Check if we're already inside a worktree
    if is_inside_worktree(project_root)? {
        let current = get_current_branch(project_root)?;
        if current == branch_name {
            // Already in the correct worktree, use it as-is
            return Ok(project_root.to_path_buf());
        } else {
            return Err(TaskMgrError::InvalidState {
                resource_type: "Git worktree".to_string(),
                id: project_root.display().to_string(),
                expected: format!("worktree for branch '{}'", branch_name),
                actual: format!(
                    "already inside worktree for branch '{}'. \
                     Run from the main repository or the correct worktree.",
                    current
                ),
            });
        }
    }

    let worktree_path = compute_worktree_path(project_root, branch_name);

    // Check if worktree already exists via git worktree list
    let list_output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(
                project_root.display().to_string(),
                "running git worktree list",
                e,
            )
        })?;

    if list_output.status.success() {
        let list_str = String::from_utf8_lossy(&list_output.stdout);
        let worktrees = parse_worktree_list(&list_str);

        // Check if a worktree for this branch already exists
        for (path, branch) in &worktrees {
            if branch.as_deref() == Some(branch_name) {
                // Found existing worktree for this branch
                if path.exists() {
                    eprintln!(
                        "Using existing worktree for '{}' at {}",
                        branch_name,
                        path.display()
                    );
                    return Ok(path.clone());
                }
            }
        }

        // Check if our target path is already a worktree (but maybe for a different branch)
        if worktree_path.exists() && worktree_path.join(".git").exists() {
            // It's a worktree, check which branch
            let wt_branch = get_current_branch(&worktree_path)?;
            if wt_branch == branch_name {
                eprintln!(
                    "Using existing worktree for '{}' at {}",
                    branch_name,
                    worktree_path.display()
                );
                return Ok(worktree_path);
            } else {
                return Err(TaskMgrError::InvalidState {
                    resource_type: "Git worktree".to_string(),
                    id: worktree_path.display().to_string(),
                    expected: format!("worktree for branch '{}'", branch_name),
                    actual: format!(
                        "worktree exists but is on branch '{}'. \
                         Remove it with: git worktree remove {}",
                        wt_branch,
                        worktree_path.display()
                    ),
                });
            }
        }
    }

    // Need to create the worktree
    if !yes_mode {
        eprintln!(
            "Creating git worktree for branch '{}' at {}",
            branch_name,
            worktree_path.display()
        );
        if !prompt_user_yn("Create worktree? [y/N] ")? {
            return Err(TaskMgrError::InvalidState {
                resource_type: "User confirmation".to_string(),
                id: "worktree creation".to_string(),
                expected: "user approved worktree creation".to_string(),
                actual: "user declined".to_string(),
            });
        }
    } else {
        eprintln!(
            "Creating worktree for '{}' at {}",
            branch_name,
            worktree_path.display()
        );
    }

    // Create parent directory for worktrees; track if we created it so we can
    // remove it on failure (avoids leaving orphan directories behind).
    let worktrees_parent = worktree_path.parent().unwrap_or(&worktree_path);
    let parent_created = if !worktrees_parent.exists() {
        std::fs::create_dir_all(worktrees_parent).map_err(|e| {
            TaskMgrError::io_error(
                worktrees_parent.display().to_string(),
                "creating worktrees directory",
                e,
            )
        })?;
        true
    } else {
        false
    };

    // Check if branch exists
    let branch_exists = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", branch_name),
        ])
        .current_dir(project_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| {
            TaskMgrError::io_error(
                project_root.display().to_string(),
                "checking if branch exists",
                e,
            )
        })?
        .success();

    // Create worktree
    let create_result = if branch_exists {
        // Branch exists, create worktree for existing branch
        Command::new("git")
            .args([
                "worktree",
                "add",
                worktree_path.to_str().unwrap_or_default(),
                branch_name,
            ])
            .current_dir(project_root)
            .output()
            .map_err(|e| {
                TaskMgrError::io_error(
                    project_root.display().to_string(),
                    "running git worktree add",
                    e,
                )
            })?
    } else {
        // Branch doesn't exist, create new branch in worktree.
        // The `--` separator before start_point prevents flag injection from
        // malicious ref values (e.g. "--exec=...").
        let mut args = vec![
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap_or_default(),
        ];
        if let Some(sp) = start_point {
            args.push("--");
            args.push(sp);
        }
        Command::new("git")
            .args(&args)
            .current_dir(project_root)
            .output()
            .map_err(|e| {
                TaskMgrError::io_error(
                    project_root.display().to_string(),
                    "running git worktree add -b",
                    e,
                )
            })?
    };

    if !create_result.status.success() {
        let stderr = String::from_utf8_lossy(&create_result.stderr);

        // Clean up empty parent dir if we just created it (avoids orphan dirs).
        if parent_created {
            cleanup_empty_dir(worktrees_parent);
        }

        // Prune any stale worktree entries git may have recorded before failing.
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(project_root)
            .output();

        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktree".to_string(),
            id: branch_name.to_string(),
            expected: "successful worktree creation".to_string(),
            actual: format!("git error: {}", stderr.trim()),
        });
    }

    eprintln!("Created worktree at {}", worktree_path.display());
    Ok(worktree_path)
}

/// Remove a git worktree.
///
/// Returns `Ok(true)` if the worktree was removed, `Ok(false)` if skipped due to
/// uncommitted changes, and `Err` if the path does not exist or git commands fail.
///
/// After removal, if the parent directory is empty (no other worktrees remain),
/// it is also removed.
pub fn remove_worktree(project_root: &Path, worktree_path: &Path) -> TaskMgrResult<bool> {
    if !worktree_path.exists() {
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktree".to_string(),
            id: worktree_path.display().to_string(),
            expected: "worktree path to exist".to_string(),
            actual: "path does not exist".to_string(),
        });
    }

    let path_str = worktree_path.to_string_lossy();
    let output = Command::new("git")
        .args(["worktree", "remove", path_str.as_ref()])
        .current_dir(project_root)
        .output()
        .map_err(|e| {
            TaskMgrError::io_error(
                project_root.display().to_string(),
                "running git worktree remove",
                e,
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // git exits non-zero with this message when the worktree has dirty changes
        if stderr.contains("contains modified or untracked files") {
            eprintln!(
                "warning: skipping removal of dirty worktree at {} (uncommitted changes)",
                worktree_path.display()
            );
            return Ok(false);
        }
        return Err(TaskMgrError::InvalidState {
            resource_type: "Git worktree".to_string(),
            id: worktree_path.display().to_string(),
            expected: "successful worktree removal".to_string(),
            actual: format!("git error: {}", stderr.trim()),
        });
    }

    // Prune stale worktree metadata from git's internal tracking
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(project_root)
        .output();

    // Remove empty parent dir (the {repo}-worktrees/ container)
    if let Some(parent) = worktree_path.parent() {
        cleanup_empty_dir(parent);
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::test_utils::setup_git_repo_with_file;
    use std::fs;
    use std::process::Command;

    #[test]
    fn test_sanitize_branch_name_replaces_slashes() {
        assert_eq!(sanitize_branch_name("feature/auth"), "feature-auth");
        assert_eq!(
            sanitize_branch_name("feat/user/auth-v2"),
            "feat-user-auth-v2"
        );
    }

    #[test]
    fn test_sanitize_branch_name_replaces_spaces() {
        assert_eq!(sanitize_branch_name("my branch"), "my-branch");
        assert_eq!(sanitize_branch_name("my  branch"), "my--branch");
    }

    #[test]
    fn test_sanitize_branch_name_replaces_windows_forbidden_chars() {
        assert_eq!(sanitize_branch_name("a:b*c?d"), "a-b-c-d");
        assert_eq!(sanitize_branch_name("a<b>c|d"), "a-b-c-d");
        assert_eq!(sanitize_branch_name("a\"b\\c"), "a-b-c");
    }

    #[test]
    fn test_sanitize_branch_name_preserves_valid_chars() {
        assert_eq!(sanitize_branch_name("simple-branch"), "simple-branch");
        assert_eq!(sanitize_branch_name("branch_name"), "branch_name");
        assert_eq!(sanitize_branch_name("v1.2.3"), "v1.2.3");
    }

    #[test]
    fn test_compute_worktree_path_basic() {
        let project_root = Path::new("/home/user/myproject");
        let path = compute_worktree_path(project_root, "feature/auth");

        assert_eq!(
            path,
            PathBuf::from("/home/user/myproject-worktrees/feature-auth")
        );
    }

    #[test]
    fn test_compute_worktree_path_simple_branch() {
        let project_root = Path::new("/home/user/myproject");
        let path = compute_worktree_path(project_root, "main");

        assert_eq!(path, PathBuf::from("/home/user/myproject-worktrees/main"));
    }

    #[test]
    fn test_parse_worktree_list_empty() {
        let output = "";
        let worktrees = parse_worktree_list(output);
        assert!(worktrees.is_empty());
    }

    #[test]
    fn test_parse_worktree_list_single_worktree() {
        let output = "worktree /home/user/project\nHEAD abc123\nbranch refs/heads/main\n";
        let worktrees = parse_worktree_list(output);

        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].0, PathBuf::from("/home/user/project"));
        assert_eq!(worktrees[0].1, Some("main".to_string()));
    }

    #[test]
    fn test_parse_worktree_list_multiple_worktrees() {
        let output = "\
worktree /home/user/project
HEAD abc123
branch refs/heads/main

worktree /home/user/project-worktrees/feature-auth
HEAD def456
branch refs/heads/feature/auth

worktree /home/user/project-worktrees/detached
HEAD ghi789
detached
";
        let worktrees = parse_worktree_list(output);

        assert_eq!(worktrees.len(), 3);
        assert_eq!(worktrees[0].0, PathBuf::from("/home/user/project"));
        assert_eq!(worktrees[0].1, Some("main".to_string()));
        assert_eq!(
            worktrees[1].0,
            PathBuf::from("/home/user/project-worktrees/feature-auth")
        );
        assert_eq!(worktrees[1].1, Some("feature/auth".to_string()));
        assert_eq!(
            worktrees[2].0,
            PathBuf::from("/home/user/project-worktrees/detached")
        );
        assert_eq!(worktrees[2].1, None); // detached HEAD has no branch
    }

    #[test]
    fn test_ensure_worktree_creates_new_worktree() {
        let tmp = setup_git_repo_with_file();

        // Create a new worktree for a new branch
        let result = ensure_worktree(tmp.path(), "feature/test-wt", true, None);
        assert!(
            result.is_ok(),
            "Should create worktree for new branch: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        assert!(
            wt_path.exists(),
            "Worktree path should exist: {}",
            wt_path.display()
        );
        assert!(
            wt_path.join(".git").exists(),
            "Worktree should have .git file"
        );

        // Verify the worktree is on the correct branch
        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(current, "feature/test-wt");
    }

    #[test]
    fn test_ensure_worktree_reuses_existing_worktree() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let result1 = ensure_worktree(tmp.path(), "feature/reuse-test", true, None);
        assert!(result1.is_ok());
        let wt_path1 = result1.unwrap();

        // Call again - should reuse the same worktree
        let result2 = ensure_worktree(tmp.path(), "feature/reuse-test", true, None);
        assert!(result2.is_ok());
        let wt_path2 = result2.unwrap();

        assert_eq!(
            wt_path1, wt_path2,
            "Should return same path for existing worktree"
        );
    }

    #[test]
    fn test_ensure_worktree_for_existing_branch() {
        let tmp = setup_git_repo_with_file();

        // Create a branch without a worktree
        Command::new("git")
            .args(["branch", "existing-branch"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");

        // Create worktree for the existing branch
        let result = ensure_worktree(tmp.path(), "existing-branch", true, None);
        assert!(
            result.is_ok(),
            "Should create worktree for existing branch: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(current, "existing-branch");
    }

    #[test]
    fn test_ensure_worktree_path_contains_sanitized_branch_name() {
        let tmp = setup_git_repo_with_file();

        let result = ensure_worktree(tmp.path(), "feature/nested/branch", true, None);
        assert!(result.is_ok());

        let wt_path = result.unwrap();
        let path_str = wt_path.to_string_lossy();

        // Path should have sanitized branch name (slashes -> dashes)
        assert!(
            path_str.contains("feature-nested-branch"),
            "Worktree path should contain sanitized branch name, got: {}",
            path_str
        );
    }

    #[test]
    fn test_ensure_worktree_from_inside_correct_worktree_returns_same_path() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let result1 = ensure_worktree(tmp.path(), "feature/inside-test", true, None);
        assert!(result1.is_ok());
        let wt_path = result1.unwrap();

        // Now call ensure_worktree from inside the worktree for the same branch
        let result2 = ensure_worktree(&wt_path, "feature/inside-test", true, None);
        assert!(
            result2.is_ok(),
            "Should succeed when called from inside correct worktree: {:?}",
            result2.err()
        );

        assert_eq!(
            result2.unwrap(),
            wt_path,
            "Should return the worktree path when called from inside it"
        );
    }

    #[test]
    fn test_ensure_worktree_from_inside_wrong_worktree_fails() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let result1 = ensure_worktree(tmp.path(), "feature/wt-one", true, None);
        assert!(result1.is_ok());
        let wt_path = result1.unwrap();

        // Now call ensure_worktree from inside the worktree but for a different branch
        let result2 = ensure_worktree(&wt_path, "feature/wt-two", true, None);
        assert!(
            result2.is_err(),
            "Should fail when called from inside worktree for wrong branch"
        );

        let err = result2.unwrap_err().to_string();
        assert!(
            err.contains("already inside worktree"),
            "Error should mention being inside a worktree, got: {}",
            err
        );
    }

    #[test]
    fn test_is_inside_worktree_false_for_main_repo() {
        let tmp = setup_git_repo_with_file();

        let result = is_inside_worktree(tmp.path());
        assert!(result.is_ok());
        assert!(
            !result.unwrap(),
            "Main repo should not be detected as worktree"
        );
    }

    #[test]
    fn test_is_inside_worktree_true_for_actual_worktree() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let result1 = ensure_worktree(tmp.path(), "feature/detect-test", true, None);
        assert!(result1.is_ok());
        let wt_path = result1.unwrap();

        let result = is_inside_worktree(&wt_path);
        assert!(result.is_ok());
        assert!(result.unwrap(), "Worktree should be detected as worktree");
    }

    // --- TEST-INIT-001: remove_worktree() and early exit cleanup ---

    #[test]
    fn test_remove_worktree_clean_returns_true_and_path_removed() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree to remove
        let wt_path =
            ensure_worktree(tmp.path(), "feature/cleanup-me", true, None).expect("create worktree");
        assert!(wt_path.exists(), "Worktree should exist before removal");

        let result = remove_worktree(tmp.path(), &wt_path);
        assert!(
            result.is_ok(),
            "remove_worktree on clean worktree should return Ok: {:?}",
            result.err()
        );
        assert_eq!(
            result.unwrap(),
            true,
            "remove_worktree on clean worktree should return Ok(true)"
        );
        assert!(
            !wt_path.exists(),
            "Worktree path should no longer exist after removal"
        );
    }

    #[test]
    fn test_remove_worktree_dirty_returns_false_and_path_preserved() {
        let tmp = setup_git_repo_with_file();

        // Create a worktree
        let wt_path =
            ensure_worktree(tmp.path(), "feature/dirty-wt", true, None).expect("create worktree");

        // Dirty the worktree
        fs::write(wt_path.join("dirty.txt"), "uncommitted content").expect("write dirty file");

        let result = remove_worktree(tmp.path(), &wt_path);
        assert!(
            result.is_ok(),
            "remove_worktree on dirty worktree should return Ok (skip with warning): {:?}",
            result.err()
        );
        assert_eq!(
            result.unwrap(),
            false,
            "remove_worktree on dirty worktree should return Ok(false)"
        );
        assert!(
            wt_path.exists(),
            "Dirty worktree path should still exist (was skipped)"
        );
    }

    #[test]
    fn test_remove_worktree_removes_empty_parent_dir() {
        let tmp = setup_git_repo_with_file();

        // Create a single worktree (will be the only one in the parent dir)
        let wt_path =
            ensure_worktree(tmp.path(), "feature/last-wt", true, None).expect("create worktree");
        let parent = wt_path
            .parent()
            .expect("worktree has parent dir")
            .to_path_buf();
        assert!(parent.exists(), "Parent dir should exist");

        // Remove the only worktree — parent should be removed too (now empty)
        let result = remove_worktree(tmp.path(), &wt_path).expect("remove_worktree should succeed");
        assert!(result, "Should have removed the worktree");

        assert!(
            !parent.exists(),
            "Empty parent dir should be removed after last worktree is gone: {:?}",
            parent
        );
    }

    #[test]
    fn test_remove_worktree_non_empty_parent_dir_preserved() {
        let tmp = setup_git_repo_with_file();

        // Create two worktrees in the same parent dir
        let wt1 = ensure_worktree(tmp.path(), "feature/wt-alpha", true, None).expect("create wt1");
        let wt2 = ensure_worktree(tmp.path(), "feature/wt-beta", true, None).expect("create wt2");

        let parent = wt1.parent().expect("wt1 has parent").to_path_buf();
        assert_eq!(
            wt1.parent().unwrap(),
            wt2.parent().unwrap(),
            "Both worktrees should share a parent dir"
        );

        // Remove only one — parent should NOT be removed (wt2 still there)
        let result = remove_worktree(tmp.path(), &wt1).expect("remove wt1");
        assert!(result, "Should have removed wt1");

        assert!(
            parent.exists(),
            "Parent dir should NOT be removed when other worktrees remain"
        );
        assert!(wt2.exists(), "wt2 should still exist");
    }

    // Known-bad discriminator: non-existent path is an error, not Ok(true)
    #[test]
    fn test_remove_worktree_non_existent_path_returns_error() {
        let tmp = setup_git_repo_with_file();

        let nonexistent = tmp.path().join("does-not-exist");
        assert!(!nonexistent.exists(), "Path should not exist for this test");

        let result = remove_worktree(tmp.path(), &nonexistent);
        assert!(
            result.is_err(),
            "remove_worktree with non-existent path should return Err, not Ok(true): {:?}",
            result.ok()
        );
    }

    #[test]
    fn test_ensure_worktree_cleans_up_empty_parent_on_git_add_failure() {
        // When git worktree add fails after creating the parent dir,
        // ensure_worktree should remove the empty parent dir (not leave orphan dirs).
        //
        // To trigger git worktree add failure: use a branch name containing ".."
        // which git forbids in ref names (ref name rules: no consecutive dots).
        let tmp = setup_git_repo_with_file();

        // "feature/bad..ref" contains ".." which git rejects as an invalid ref name.
        // The parent dir {repo}-worktrees/ will be created by ensure_worktree
        // (it won't pre-exist) and must be removed after the git failure.
        let branch = "feature/bad..ref";
        let wt_path = compute_worktree_path(tmp.path(), branch);
        let parent = wt_path.parent().expect("has parent").to_path_buf();

        // Parent must not exist before the call (so ensure_worktree creates it).
        assert!(
            !parent.exists(),
            "parent dir should not pre-exist before the test"
        );

        let result = ensure_worktree(tmp.path(), branch, true, None);
        assert!(
            result.is_err(),
            "ensure_worktree with invalid git ref name should fail"
        );

        // Parent dir must not be left as an orphan
        assert!(
            !parent.exists(),
            "Empty parent dir should be cleaned up after git worktree add failure: {:?}",
            parent
        );
    }

    // --- TEST-001: Comprehensive tests for remove_worktree() and early exit cleanup ---

    /// Test: remove_worktree() on worktree with staged but uncommitted changes.
    /// Staged changes are "modified" from git's perspective, so `git worktree remove`
    /// should refuse and return Ok(false) (skip with warning).
    #[test]
    fn test_remove_worktree_staged_changes_returns_false() {
        let tmp = setup_git_repo_with_file();

        let wt_path = ensure_worktree(tmp.path(), "feature/staged-changes", true, None)
            .expect("create worktree");

        // Stage a new file in the worktree without committing
        let new_file = wt_path.join("staged.txt");
        fs::write(&new_file, "staged content").expect("write staged file");
        Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(&wt_path)
            .output()
            .expect("git add");

        // Verify the file is staged
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&wt_path)
            .output()
            .expect("git status");
        let status_str = String::from_utf8_lossy(&status.stdout);
        assert!(
            status_str.contains("staged.txt"),
            "File should be staged: {}",
            status_str
        );

        // remove_worktree should skip dirty (staged) worktree
        let result = remove_worktree(tmp.path(), &wt_path);
        assert!(
            result.is_ok(),
            "remove_worktree with staged changes should return Ok (skip): {:?}",
            result.err()
        );
        assert_eq!(
            result.unwrap(),
            false,
            "remove_worktree with staged changes should return Ok(false)"
        );
        assert!(
            wt_path.exists(),
            "Worktree with staged changes should be preserved"
        );
    }

    /// Test: remove_worktree() when the worktree path was deleted out-of-band (directory is gone
    /// but git may still have it in its worktree list). The path no longer exists on disk.
    #[test]
    fn test_remove_worktree_out_of_band_delete_returns_error() {
        let tmp = setup_git_repo_with_file();

        // Create a real worktree first so git knows about it
        let wt_path = ensure_worktree(tmp.path(), "feature/out-of-band", true, None)
            .expect("create worktree");
        assert!(wt_path.exists(), "Worktree should exist initially");

        // Simulate out-of-band deletion: manually remove the directory without going through git
        fs::remove_dir_all(&wt_path).expect("manual rm -rf of worktree dir");
        assert!(
            !wt_path.exists(),
            "Worktree directory should be gone after manual delete"
        );

        // remove_worktree should return Err because the path is already gone
        let result = remove_worktree(tmp.path(), &wt_path);
        assert!(
            result.is_err(),
            "remove_worktree on out-of-band-deleted path should return Err, got: {:?}",
            result.ok()
        );
    }

    /// Test: parent dir is preserved when it contains a regular file (non-empty for reasons
    /// other than worktrees). cleanup_empty_dir is best-effort and must not remove non-empty dirs.
    #[test]
    fn test_remove_worktree_parent_with_extra_file_not_removed() {
        let tmp = setup_git_repo_with_file();

        let wt_path = ensure_worktree(tmp.path(), "feature/wt-with-sibling", true, None)
            .expect("create worktree");
        let parent = wt_path.parent().expect("worktree has parent").to_path_buf();

        // Place a regular file in the parent dir (simulates user-created content)
        let extra_file = parent.join("README.txt");
        fs::write(&extra_file, "some user content").expect("write extra file");

        // Remove the worktree
        let result = remove_worktree(tmp.path(), &wt_path).expect("remove worktree");
        assert!(result, "Should have removed the worktree");

        // Parent dir must NOT be removed because it still has README.txt
        assert!(
            parent.exists(),
            "Parent dir should be preserved when it contains extra files"
        );
        assert!(
            extra_file.exists(),
            "Extra file in parent should be preserved"
        );
    }

    /// Parameterized-style test: remove_worktree() behavior for various git states.
    /// Tests clean worktree (Ok(true)), dirty with untracked (Ok(false)),
    /// and dirty with modified tracked file (Ok(false)).
    #[test]
    fn test_remove_worktree_git_state_table() {
        struct TestCase {
            name: &'static str,
            // mutate fn: receives the worktree path, sets up git state
            setup: fn(&std::path::Path),
            expected_ok: bool,
            expected_value: bool,
            path_removed: bool,
        }

        let cases: &[TestCase] = &[
            TestCase {
                name: "clean worktree",
                setup: |_| {},
                expected_ok: true,
                expected_value: true,
                path_removed: true,
            },
            TestCase {
                name: "dirty: untracked file",
                setup: |wt| {
                    fs::write(wt.join("new_untracked.txt"), "data").expect("write untracked");
                },
                expected_ok: true,
                expected_value: false,
                path_removed: false,
            },
            TestCase {
                name: "dirty: modified tracked file",
                setup: |wt| {
                    // file.txt was committed in setup_git_repo via the main repo,
                    // but the worktree has its own copy of the repo state.
                    // We need to create a file that was previously committed in this worktree.
                    fs::write(wt.join("new_tracked.txt"), "original").expect("write tracked");
                    Command::new("git")
                        .args(["add", "new_tracked.txt"])
                        .current_dir(wt)
                        .output()
                        .expect("git add");
                    Command::new("git")
                        .args(["commit", "-m", "add tracked"])
                        .current_dir(wt)
                        .output()
                        .expect("git commit");
                    // Now modify it without committing
                    fs::write(wt.join("new_tracked.txt"), "modified").expect("modify tracked");
                },
                expected_ok: true,
                expected_value: false,
                path_removed: false,
            },
        ];

        for case in cases {
            let tmp = setup_git_repo_with_file();
            let branch = format!("feature/state-test-{}", case.name.replace([':', ' '], "-"));
            let wt_path = ensure_worktree(tmp.path(), &branch, true, None)
                .unwrap_or_else(|e| panic!("[{}] create worktree: {:?}", case.name, e));

            (case.setup)(&wt_path);

            let result = remove_worktree(tmp.path(), &wt_path);
            assert_eq!(
                result.is_ok(),
                case.expected_ok,
                "[{}] expected is_ok()={}, got: {:?}",
                case.name,
                case.expected_ok,
                result
            );
            if case.expected_ok {
                assert_eq!(
                    result.unwrap(),
                    case.expected_value,
                    "[{}] expected Ok({})",
                    case.name,
                    case.expected_value
                );
            }
            assert_eq!(
                !wt_path.exists(),
                case.path_removed,
                "[{}] path_removed={} but path exists={}",
                case.name,
                case.path_removed,
                wt_path.exists()
            );
        }
    }

    #[test]
    fn test_ensure_worktree_runs_git_prune_on_partial_failure() {
        // When git worktree add creates a partial entry then fails, ensure_worktree
        // should call `git worktree prune` so stale entries don't accumulate.
        //
        // This is difficult to trigger deterministically (requires a mid-operation
        // failure). The test validates the behavior via state inspection: after a
        // forced failure, `git worktree list` should not contain stale entries.
        let tmp = setup_git_repo_with_file();

        // Simulate partial failure: create the worktree directory with content so
        // git worktree add refuses it (git rejects non-empty target directories).
        let wt_path = compute_worktree_path(tmp.path(), "feature/prune-test");
        let parent = wt_path.parent().expect("has parent");
        fs::create_dir_all(&wt_path).expect("create dir to cause conflict");
        // Put a file inside so git refuses to use this non-empty directory.
        fs::write(wt_path.join("dummy.txt"), "block").expect("write dummy file");

        let result = ensure_worktree(tmp.path(), "feature/prune-test", true, None);
        assert!(
            result.is_err(),
            "ensure_worktree should fail when directory already exists"
        );

        // After failure, run git worktree list and verify no stale entry for the path
        let list_output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(tmp.path())
            .output()
            .expect("git worktree list");
        let list_str = String::from_utf8_lossy(&list_output.stdout);

        // The failed worktree should not appear in the list (prune was called)
        let wt_str = wt_path.to_string_lossy();
        assert!(
            !list_str.contains(wt_str.as_ref()),
            "Stale worktree entry should be pruned after partial failure, got list: {}",
            list_str
        );

        // Clean up
        let _ = fs::remove_dir_all(parent);
    }

    // --- CHAIN-001: start_point parameter tests ---

    /// Regression test: start_point=None must produce identical behavior to before the
    /// parameter was added (new branch from HEAD).
    #[test]
    fn test_ensure_worktree_start_point_none_creates_from_head() {
        let tmp = setup_git_repo_with_file();

        let result = ensure_worktree(tmp.path(), "feat/from-head", true, None);
        assert!(
            result.is_ok(),
            "start_point=None should create worktree from HEAD: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        assert!(wt_path.exists(), "Worktree path should exist");

        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(current, "feat/from-head");
    }

    /// start_point=Some("branch-a") must create the new branch rooted at branch-a's commits.
    /// The `--` separator in git args is what enables this (prevents flag injection).
    #[test]
    fn test_ensure_worktree_start_point_some_creates_branch_from_ref() {
        let tmp = setup_git_repo_with_file();

        // Create branch-a and add a distinguishing commit on it.
        Command::new("git")
            .args(["checkout", "-b", "branch-a"])
            .current_dir(tmp.path())
            .output()
            .expect("checkout -b branch-a");
        fs::write(tmp.path().join("branch-a-marker.txt"), "branch-a content")
            .expect("write marker");
        Command::new("git")
            .args(["add", "branch-a-marker.txt"])
            .current_dir(tmp.path())
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "branch-a unique commit"])
            .current_dir(tmp.path())
            .output()
            .expect("git commit on branch-a");

        // Return to main so we can create the new worktree from the main repo.
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(tmp.path())
            .output()
            .expect("checkout main");

        // Create branch-b from branch-a via ensure_worktree with start_point.
        let result = ensure_worktree(tmp.path(), "feat/branch-b", true, Some("branch-a"));
        assert!(
            result.is_ok(),
            "start_point=Some('branch-a') should succeed: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        // Verify branch-b worktree contains branch-a's distinguishing file.
        assert!(
            wt_path.join("branch-a-marker.txt").exists(),
            "branch-b should contain branch-a-marker.txt (start_point was branch-a)"
        );
        // Verify it is a new branch, not a checkout of branch-a.
        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(
            current, "feat/branch-b",
            "Worktree should be on branch feat/branch-b, not branch-a"
        );
    }

    /// When the branch already exists, start_point must be silently ignored.
    /// The existing-branch git command path (`git worktree add <path> <branch>`) does
    /// not receive start_point — this test confirms no error is raised and the result
    /// is on the existing branch.
    #[test]
    fn test_ensure_worktree_start_point_ignored_for_existing_branch() {
        let tmp = setup_git_repo_with_file();

        // Create branch "preexisting" without a worktree.
        Command::new("git")
            .args(["branch", "preexisting"])
            .current_dir(tmp.path())
            .output()
            .expect("create branch");

        // Call with start_point=Some("nonexistent-ref") — if start_point were passed to
        // the existing-branch path it would cause a git error; succeeding proves it's ignored.
        let result = ensure_worktree(tmp.path(), "preexisting", true, Some("nonexistent-ref"));
        assert!(
            result.is_ok(),
            "start_point should be ignored when branch already exists: {:?}",
            result.err()
        );

        let wt_path = result.unwrap();
        let current = get_current_branch(&wt_path).expect("get branch");
        assert_eq!(
            current, "preexisting",
            "Worktree should be on the existing branch"
        );
    }

    /// Integration test: creating a worktree with `start_point` from another branch
    /// must cause the new worktree to contain the source branch's commits.
    ///
    /// Known-bad discriminator: if `start_point` is silently ignored, `feat/phase-2`
    /// would branch from HEAD (main) and would NOT contain `phase1-marker.txt` —
    /// this test catches that failure.
    #[test]
    fn test_ensure_worktree_chain_preserves_commits() {
        let tmp = setup_git_repo_with_file();

        // Step 1: create feat/phase-1 worktree from HEAD (main)
        let phase1_path = ensure_worktree(tmp.path(), "feat/phase-1", true, None)
            .expect("create feat/phase-1 worktree");
        assert!(phase1_path.exists(), "phase-1 worktree must exist on disk");

        // Step 2: make a unique commit in the phase-1 worktree
        fs::write(phase1_path.join("phase1-marker.txt"), "phase1 content")
            .expect("write phase1-marker.txt");
        Command::new("git")
            .args(["add", "phase1-marker.txt"])
            .current_dir(&phase1_path)
            .output()
            .expect("git add phase1-marker.txt");
        Command::new("git")
            .args(["commit", "-m", "phase-1 unique commit"])
            .current_dir(&phase1_path)
            .output()
            .expect("git commit in phase-1");

        // Step 3: create feat/phase-2 branched from feat/phase-1 via start_point
        let phase2_path = ensure_worktree(tmp.path(), "feat/phase-2", true, Some("feat/phase-1"))
            .expect("create feat/phase-2 worktree from feat/phase-1");
        assert!(phase2_path.exists(), "phase-2 worktree must exist on disk");

        // Acceptance: phase-2 must contain phase-1's marker file
        assert!(
            phase2_path.join("phase1-marker.txt").exists(),
            "phase-2 must contain phase1-marker.txt (inherited via start_point=feat/phase-1)"
        );

        // Acceptance: phase-2's git log must include phase-1's commit message
        let log_output = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&phase2_path)
            .output()
            .expect("git log in phase-2");
        let log_str = String::from_utf8_lossy(&log_output.stdout);
        assert!(
            log_str.contains("phase-1 unique commit"),
            "phase-2 git log must contain phase-1's commit message, got: {}",
            log_str
        );

        // Negative: phase-2 must be a NEW branch, not a detached HEAD or checkout of phase-1
        let branch_output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&phase2_path)
            .output()
            .expect("git rev-parse HEAD in phase-2");
        let branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();
        assert_eq!(
            branch, "feat/phase-2",
            "phase-2 worktree must be on branch 'feat/phase-2', not '{}'",
            branch
        );
    }
}
