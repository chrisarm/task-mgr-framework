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
//!
//! # Path identity (pin-19 / CONTRACT-001)
//!
//! [`remap_into_worktree`] and [`paths_identify`] are the SSoT for whether a
//! live JSON path is the same PRD file as a `prd_files` row across main
//! checkout vs linked worktree. Match (a) (JSON `taskPrefix` OR) is a
//! separate predicate — do not fold it into [`paths_identify`]. DB scan lives
//! in [`crate::commands::init::import::find_registered_task_lists`] (no
//! rusqlite in this module). See progress log `## CONTRACT-001`.
//!
//! [`worktree_root_at`] / [`worktree_root`] return the *current* worktree
//! toplevel (`git rev-parse --show-toplevel`), not [`main_repo_root_at`].

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

/// Return the canonical filesystem path of the *current* worktree's toplevel
/// for the git checkout containing `dir`, or `None` if `dir` is not inside a
/// git repo (or `git` is missing / errors out).
///
/// Uses `git rev-parse --show-toplevel` — the linked worktree's working tree,
/// not the main checkout. Do **not** confuse with [`main_repo_root_at`], which
/// walks `--git-common-dir` and always lands on the main worktree.
///
/// Both inputs and outputs are canonicalized so callers can rely on path
/// equality even when one side reaches the repo via a symlink.
pub fn worktree_root_at(dir: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let toplevel = String::from_utf8(output.stdout).ok()?;
    let toplevel = toplevel.trim();
    if toplevel.is_empty() {
        return None;
    }

    let path = PathBuf::from(toplevel);
    if !path.is_dir() {
        return None;
    }

    std::fs::canonicalize(&path).ok()
}

/// Return the current worktree toplevel for the current working directory.
pub fn worktree_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    worktree_root_at(&cwd)
}

/// Remap a registered PRD path from `source_root` into `worktree_root`.
///
/// Join relative `registered` to `source_root` first. Then
/// `strip_prefix(source_root)`; on hit return `worktree_root.join(rel)`.
/// On strip miss, return the resolved path unchanged.
///
/// Pure path math: no `exists()`, no basename search, no dest canonicalize.
/// Callers that need symlink-safe strip (e.g. startup Step 8.5) canonicalize
/// `source_root` before calling.
pub fn remap_into_worktree(registered: &Path, source_root: &Path, worktree_root: &Path) -> PathBuf {
    let resolved = if registered.is_absolute() {
        registered.to_path_buf()
    } else {
        source_root.join(registered)
    };
    match resolved.strip_prefix(source_root) {
        Ok(rel) => worktree_root.join(rel),
        Err(_) => resolved,
    }
}

/// Pin-19 path identity: (b)+(c) only.
///
/// Hit if `canonicalize(flag) == canonicalize(resolved)` **or**
/// `canonicalize(flag) == remap_into_worktree(registered, source_root, worktree_root)`,
/// where `resolved` is `registered` when absolute, else `source_root.join(registered)`.
///
/// Match (a) (JSON `taskPrefix` in `prd_metadata`) is a **separate** predicate
/// and must not be folded into this function.
///
/// Missing or unreadable `flag` fails closed (`false`); never panics.
pub fn paths_identify(
    flag: &Path,
    registered: &Path,
    source_root: &Path,
    worktree_root: &Path,
) -> bool {
    let Ok(canon_flag) = std::fs::canonicalize(flag) else {
        return false;
    };
    let resolved = if registered.is_absolute() {
        registered.to_path_buf()
    } else {
        source_root.join(registered)
    };
    // (b) same resolved path under source_root
    if let Ok(canon_resolved) = std::fs::canonicalize(&resolved)
        && canon_resolved == canon_flag
    {
        return true;
    }
    // (c) live path is the worktree remap of the registered row
    let remapped = remap_into_worktree(registered, source_root, worktree_root);
    canon_flag == remapped
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

    #[test]
    fn worktree_root_at_returns_none_outside_git() {
        let tmp = TempDir::new().unwrap();
        assert!(worktree_root_at(tmp.path()).is_none());
    }

    #[test]
    fn worktree_root_at_main_repo_returns_repo_path() {
        let tmp = init_git_repo();
        let got = worktree_root_at(tmp.path()).expect("Some");
        assert_eq!(got, std::fs::canonicalize(tmp.path()).unwrap());
    }

    #[test]
    fn worktree_root_at_linked_worktree_returns_worktree_not_main() {
        let tmp = init_git_repo();
        let wt_parent = TempDir::new().unwrap();
        let wt_path = wt_parent.path().join("wt");
        let status = PCmd::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "feat/toplevel",
                wt_path.to_str().unwrap(),
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());

        let got = worktree_root_at(&wt_path).expect("Some");
        assert_eq!(got, std::fs::canonicalize(&wt_path).unwrap());
        // Contrast: main_repo_root_at still points at the main checkout.
        assert_eq!(
            main_repo_root_at(&wt_path).expect("Some"),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
    }

    #[test]
    fn remap_dest_need_not_exist() {
        let source = PathBuf::from("/repo/main");
        let worktree = PathBuf::from("/repo/worktrees/feat");
        let registered = Path::new("tasks/foo.json");
        let got = remap_into_worktree(registered, &source, &worktree);
        assert_eq!(got, worktree.join("tasks/foo.json"));
        assert!(!got.exists(), "dest must not need to exist");
    }

    #[test]
    fn remap_main_checkout_unchanged() {
        let root = PathBuf::from("/repo/main");
        let registered = Path::new("tasks/foo.json");
        let got = remap_into_worktree(registered, &root, &root);
        assert_eq!(got, root.join("tasks/foo.json"));
    }

    #[test]
    fn remap_strip_miss_returns_resolved() {
        let source = PathBuf::from("/repo/main");
        let worktree = PathBuf::from("/repo/worktrees/feat");
        // Absolute path outside source_root — strip_prefix misses.
        let registered = Path::new("/other/tasks/foo.json");
        let got = remap_into_worktree(registered, &source, &worktree);
        assert_eq!(got, registered);
    }

    #[test]
    fn remap_after_caller_canonicalizes_symlink_source_root() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real-main");
        std::fs::create_dir_all(real.join("tasks")).unwrap();
        let link = tmp.path().join("link-main");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(not(unix))]
        {
            // Symlink test is unix-only; skip gracefully.
            return;
        }

        let worktree = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();

        // Caller canonicalizes source_root (startup Step 8.5), then remaps.
        let canonical_source = std::fs::canonicalize(&link).unwrap();
        let registered = Path::new("tasks/foo.json");
        let got = remap_into_worktree(registered, &canonical_source, &worktree);
        assert_eq!(got, worktree.join("tasks/foo.json"));
    }

    #[test]
    fn paths_identify_relative_registered_and_absolute_live() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main");
        let tasks = source.join("tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        let file = tasks.join("foo.json");
        std::fs::write(&file, "{}").unwrap();

        let registered = Path::new("tasks/foo.json");
        let flag = std::fs::canonicalize(&file).unwrap();
        assert!(paths_identify(
            &flag, registered, &source, &source // main checkout
        ));
    }

    #[test]
    fn paths_identify_source_and_worktree_as_one_file() {
        let tmp = TempDir::new().unwrap();
        let source_raw = tmp.path().join("main");
        let worktree_raw = tmp.path().join("wt");
        std::fs::create_dir_all(source_raw.join("tasks")).unwrap();
        std::fs::create_dir_all(worktree_raw.join("tasks")).unwrap();
        let src_file = source_raw.join("tasks/foo.json");
        let wt_file = worktree_raw.join("tasks/foo.json");
        std::fs::write(&src_file, "{}").unwrap();
        std::fs::write(&wt_file, "{}").unwrap();

        // Callers pass canonical roots so (c) PathBuf equality holds against
        // canonicalize(flag). Production startup already canonicalizes source_root.
        let source = std::fs::canonicalize(&source_raw).unwrap();
        let worktree = std::fs::canonicalize(&worktree_raw).unwrap();

        let registered = Path::new("tasks/foo.json");
        // Live path is the worktree copy — (c) hit.
        let flag = std::fs::canonicalize(&wt_file).unwrap();
        assert!(paths_identify(&flag, registered, &source, &worktree));
        // Live path is the source copy — (b) hit.
        let flag_src = std::fs::canonicalize(&src_file).unwrap();
        assert!(paths_identify(&flag_src, registered, &source, &worktree));
    }

    #[test]
    fn paths_identify_different_dirs_same_basename_do_not_match() {
        let tmp = TempDir::new().unwrap();
        let source_a = tmp.path().join("proj-a");
        let source_b = tmp.path().join("proj-b");
        std::fs::create_dir_all(source_a.join("tasks")).unwrap();
        std::fs::create_dir_all(source_b.join("tasks")).unwrap();
        let file_a = source_a.join("tasks/foo.json");
        let file_b = source_b.join("tasks/foo.json");
        std::fs::write(&file_a, "{}").unwrap();
        std::fs::write(&file_b, "{}").unwrap();

        let registered = Path::new("tasks/foo.json");
        let flag_b = std::fs::canonicalize(&file_b).unwrap();
        // Registered relative to A must not identify B's foo.json (basename trap).
        assert!(!paths_identify(&flag_b, registered, &source_a, &source_a));
    }

    #[test]
    fn paths_identify_missing_flag_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main");
        std::fs::create_dir_all(source.join("tasks")).unwrap();
        let missing = source.join("tasks/missing.json");
        let registered = Path::new("tasks/foo.json");
        assert!(!paths_identify(&missing, registered, &source, &source));
    }

    #[test]
    fn paths_identify_does_not_take_task_prefix() {
        // Compile-time / API contract: identity is path math only — no prefix
        // argument. Match (a) lives elsewhere. This test documents the boundary
        // by showing same-basename different-root paths do not hit via prefix.
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("main");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(source.join("tasks")).unwrap();
        std::fs::create_dir_all(other.join("tasks")).unwrap();
        std::fs::write(source.join("tasks/foo.json"), "{}").unwrap();
        let other_file = other.join("tasks/foo.json");
        std::fs::write(&other_file, r#"{"taskPrefix":"same-prefix"}"#).unwrap();

        let registered = Path::new("tasks/foo.json");
        let flag = std::fs::canonicalize(&other_file).unwrap();
        // Even if JSON carries a matching taskPrefix string, paths_identify
        // must not treat that as a hit (match (a) is separate).
        assert!(!paths_identify(&flag, registered, &source, &source));
    }

    /// Grep-guard: remap_into_worktree executable body must not call exists()
    /// or canonicalize. Doc comments may mention the forbidden words.
    #[test]
    fn remap_into_worktree_is_pure_no_exists_no_dest_canonicalize() {
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/git/mod.rs"));
        let start = src
            .find("pub fn remap_into_worktree(")
            .expect("remap_into_worktree present");
        let after_sig = &src[start..];
        let body_start = after_sig.find('{').expect("body");
        let rest = &after_sig[body_start..];
        let next = rest[1..]
            .find("\npub fn ")
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        let body = &rest[..next];
        // Strip // line comments so doc/prose mentions of canonicalize/exists
        // do not trip the guard.
        let code: String = body
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("exists("),
            "remap_into_worktree must not call exists()"
        );
        assert!(
            !code.contains("canonicalize"),
            "remap_into_worktree must not canonicalize dest"
        );
    }
}
