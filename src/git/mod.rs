//! Shared git helpers used by both `db::path` (DB-dir resolution) and
//! `loop_engine::worktree` (worktree lifecycle).
//!
//! These wrap the `git` CLI rather than linking libgit2 — task-mgr already
//! shells out to git everywhere else and we want behavior identical to what
//! the user sees from their shell.
//!
//! Submodules are out of scope: `git rev-parse --git-common-dir` returns the
//! common dir of the *enclosing* git dir, which for a submodule's worktree
//! resolves to the submodule's `.git` (not the superproject's). For the
//! task-mgr DB-resolution use case that's the correct answer (each
//! submodule is its own project), but `main_repo_root` does not attempt to
//! cross submodule boundaries.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Return the canonical filesystem path of the *main* repository root for the
/// git checkout containing `dir`, or `None` if `dir` is not inside a git repo
/// (or `git` is missing / errors out).
///
/// This is the parent of `git rev-parse --git-common-dir`. The "common dir"
/// is shared across all worktrees of a repository; its parent is the working
/// tree of the main worktree (the one where `.git` is a real directory, not
/// a `.git` file pointing into `worktrees/<name>/`).
///
/// Both inputs and outputs are canonicalized so callers can rely on path
/// equality even when one side reaches the repo via a symlink.
pub fn main_repo_root_at(dir: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let common_dir = String::from_utf8(output.stdout).ok()?;
    let common_dir = common_dir.trim();
    if common_dir.is_empty() {
        return None;
    }

    let common = PathBuf::from(common_dir);
    let parent = common.parent()?;
    if !parent.is_dir() {
        return None;
    }

    std::fs::canonicalize(parent).ok()
}

/// Return the main repository root for the current working directory.
pub fn main_repo_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    main_repo_root_at(&cwd)
}

/// Detect whether `dir` is inside a git worktree (i.e. a linked worktree
/// created via `git worktree add`, not the main worktree).
///
/// Returns `Ok(false)` for the main worktree, for non-git directories, or
/// when `git` exits with a non-zero status.
pub fn is_inside_worktree_at(dir: &Path) -> std::io::Result<bool> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .output()?;

    if !output.status.success() {
        return Ok(false);
    }

    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Linked worktree git-dir looks like: /path/to/main/.git/worktrees/<name>
    Ok(git_dir.contains("/worktrees/") || git_dir.contains("\\worktrees\\"))
}

/// Detect whether the current working directory is inside a linked worktree.
///
/// Returns `false` on any error (missing git, cwd unreadable, etc.) — the
/// caller will then fall back to default cwd-relative behavior.
pub fn is_inside_worktree() -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    is_inside_worktree_at(&cwd).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as PCmd;
    use tempfile::TempDir;

    fn init_git_repo() -> TempDir {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path();
        let run = |args: &[&str]| {
            let status = PCmd::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git");
            assert!(status.success(), "git {:?}", args);
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["commit", "--allow-empty", "-m", "init"]);
        tmp
    }

    #[test]
    fn main_repo_root_at_returns_none_outside_git() {
        let tmp = TempDir::new().unwrap();
        assert!(main_repo_root_at(tmp.path()).is_none());
    }

    #[test]
    fn main_repo_root_at_main_repo_returns_repo_path() {
        let tmp = init_git_repo();
        let got = main_repo_root_at(tmp.path()).expect("Some");
        assert_eq!(got, std::fs::canonicalize(tmp.path()).unwrap());
    }

    #[test]
    fn main_repo_root_at_subdirectory_returns_repo_path() {
        let tmp = init_git_repo();
        let sub = tmp.path().join("nested/deeper");
        std::fs::create_dir_all(&sub).unwrap();
        let got = main_repo_root_at(&sub).expect("Some");
        assert_eq!(got, std::fs::canonicalize(tmp.path()).unwrap());
    }

    #[test]
    fn main_repo_root_at_worktree_returns_main_repo_path() {
        let tmp = init_git_repo();
        let wt_parent = TempDir::new().unwrap();
        let wt_path = wt_parent.path().join("wt");
        let status = PCmd::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "feat/test",
                wt_path.to_str().unwrap(),
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());

        let got = main_repo_root_at(&wt_path).expect("Some");
        assert_eq!(got, std::fs::canonicalize(tmp.path()).unwrap());
    }

    #[test]
    fn is_inside_worktree_at_main_repo_is_false() {
        let tmp = init_git_repo();
        assert!(!is_inside_worktree_at(tmp.path()).unwrap());
    }

    #[test]
    fn is_inside_worktree_at_linked_worktree_is_true() {
        let tmp = init_git_repo();
        let wt_parent = TempDir::new().unwrap();
        let wt_path = wt_parent.path().join("wt");
        let status = PCmd::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "feat/inside",
                wt_path.to_str().unwrap(),
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());

        assert!(is_inside_worktree_at(&wt_path).unwrap());
    }
}
