//! Resolve `--dir` (and `TASK_MGR_DIR` env) to the canonical database directory.
//!
//! See `src/git/mod.rs` for the underlying worktree detection. The single
//! entry point [`resolve_db_dir`] is invoked once in `main.rs` immediately
//! after `Cli::parse()`, so every subcommand inherits the same resolved
//! path. This eliminates the original bug where only the `Loop`/`Batch`
//! arms anchored their DB path to the main repository — when Claude was
//! spawned from a worktree cwd and ran `task-mgr add`, that subprocess hit
//! the unanchored default and created a stray `<worktree>/.task-mgr/`.
//!
//! Resolution rules:
//!
//! 1. Absolute path → returned unchanged (clap-parsed CLI flag or env var).
//! 2. Relative + explicit `--dir` (`was_explicit == true`) → joined with
//!    cwd. Explicit overrides are honored literally; users running
//!    `task-mgr --dir ./scratch` from a worktree get a per-worktree DB by
//!    design.
//! 3. Relative + default value, currently inside a git worktree → joined
//!    with the *main* repo root (parent of `git --git-common-dir`).
//! 4. Otherwise (relative + default + not in git, or git missing) → joined
//!    with cwd.
//!
//! Sources are reported via [`DbDirSource`] so `models show` (and friends)
//! can surface which rule fired — useful when worktree weirdness recurs.

use std::path::{Path, PathBuf};

/// Where the resolved DB directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbDirSource {
    /// User passed `--dir` (absolute or relative-resolved-against-cwd).
    Cli,
    /// Pulled from `TASK_MGR_DIR` (absolute, set by the loop engine or user).
    Env,
    /// Default `.task-mgr` resolved against the main repo root because the
    /// caller is currently inside a linked worktree.
    WorktreeAnchored,
    /// Default resolved against cwd (no worktree, or git unavailable).
    CwdDefault,
}

impl DbDirSource {
    /// Short label used in diagnostic output, e.g. `models show`.
    pub fn label(&self) -> &'static str {
        match self {
            DbDirSource::Cli => "cli",
            DbDirSource::Env => "env",
            DbDirSource::WorktreeAnchored => "worktree-anchored",
            DbDirSource::CwdDefault => "cwd",
        }
    }
}

/// Result of resolving the DB directory: the absolute path plus the rule
/// that produced it.
#[derive(Debug, Clone)]
pub struct ResolvedDbDir {
    pub path: PathBuf,
    pub source: DbDirSource,
}

/// Resolve `raw` to an absolute DB directory.
///
/// `from_env` is true when clap pulled the value from `TASK_MGR_DIR` rather
/// than the command line. `was_explicit` is true when the value was
/// supplied at all (CLI or env), false when clap fell back to the
/// `default_value`. Together these distinguish all four resolution paths
/// without the caller needing to reach into clap's internals.
pub fn resolve_db_dir(raw: &Path, was_explicit: bool, from_env: bool) -> ResolvedDbDir {
    if raw.is_absolute() {
        let source = if from_env {
            DbDirSource::Env
        } else if was_explicit {
            DbDirSource::Cli
        } else {
            // Defensive: default is ".task-mgr" (relative), so this branch
            // is unreachable in normal use. If a deployment ever ships an
            // absolute default we still return Cli rather than a misleading
            // worktree-anchored label.
            DbDirSource::Cli
        };
        return ResolvedDbDir {
            path: raw.to_path_buf(),
            source,
        };
    }

    // Relative path. Honor explicit overrides literally (cwd-join).
    if was_explicit {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        return ResolvedDbDir {
            path: cwd.join(raw),
            source: if from_env {
                DbDirSource::Env
            } else {
                DbDirSource::Cli
            },
        };
    }

    // Default value: anchor to main repo root if we're inside a worktree.
    if let Some(root) = crate::git::main_repo_root() {
        return ResolvedDbDir {
            path: root.join(raw),
            source: DbDirSource::WorktreeAnchored,
        };
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ResolvedDbDir {
        path: cwd.join(raw),
        source: DbDirSource::CwdDefault,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as PCmd;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Tests in this module mutate process cwd; serialize them so they don't
    // race when run with `cargo test` (which uses multiple threads).
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    struct CwdGuard {
        original: PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CwdGuard {
        fn enter(dir: &Path) -> Self {
            let lock = CWD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let original = std::env::current_dir().expect("cwd");
            std::env::set_current_dir(dir).expect("set cwd");
            CwdGuard {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn init_git_repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path();
        let run = |args: &[&str]| {
            let status = PCmd::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .unwrap();
            assert!(status.success(), "git {:?}", args);
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["commit", "--allow-empty", "-m", "init"]);
        tmp
    }

    fn add_worktree(repo: &Path, name: &str) -> PathBuf {
        let parent = TempDir::new().unwrap();
        let wt = parent.path().join(name);
        let status = PCmd::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &format!("feat/{}", name),
                wt.to_str().unwrap(),
            ])
            .current_dir(repo)
            .status()
            .unwrap();
        assert!(status.success());
        // Leak the parent so the worktree path stays valid for the test.
        std::mem::forget(parent);
        wt
    }

    #[test]
    fn absolute_cli_path_returned_unchanged() {
        let tmp = TempDir::new().unwrap();
        let abs = tmp.path().join("custom");
        let r = resolve_db_dir(&abs, true, false);
        assert_eq!(r.path, abs);
        assert_eq!(r.source, DbDirSource::Cli);
    }

    #[test]
    fn absolute_env_path_reports_env_source() {
        let tmp = TempDir::new().unwrap();
        let abs = tmp.path().join("custom");
        let r = resolve_db_dir(&abs, true, true);
        assert_eq!(r.path, abs);
        assert_eq!(r.source, DbDirSource::Env);
    }

    #[test]
    fn explicit_relative_in_worktree_joins_cwd_not_main_repo() {
        let repo = init_git_repo();
        let wt = add_worktree(repo.path(), "explicit");
        let _g = CwdGuard::enter(&wt);

        let r = resolve_db_dir(Path::new("scratch"), true, false);
        assert_eq!(r.source, DbDirSource::Cli);
        // cwd join wins — explicit override is honored literally.
        assert_eq!(r.path, std::env::current_dir().unwrap().join("scratch"));
        // And specifically NOT the main-repo path.
        assert_ne!(
            r.path,
            std::fs::canonicalize(repo.path()).unwrap().join("scratch")
        );
    }

    #[test]
    fn default_relative_in_worktree_anchors_to_main_repo() {
        let repo = init_git_repo();
        let wt = add_worktree(repo.path(), "anchor");
        let _g = CwdGuard::enter(&wt);

        let r = resolve_db_dir(Path::new(".task-mgr"), false, false);
        assert_eq!(r.source, DbDirSource::WorktreeAnchored);
        assert_eq!(
            r.path,
            std::fs::canonicalize(repo.path())
                .unwrap()
                .join(".task-mgr"),
        );
    }

    #[test]
    fn default_relative_outside_git_joins_cwd() {
        let tmp = TempDir::new().unwrap();
        let _g = CwdGuard::enter(tmp.path());

        let r = resolve_db_dir(Path::new(".task-mgr"), false, false);
        assert_eq!(r.source, DbDirSource::CwdDefault);
        assert_eq!(r.path, std::env::current_dir().unwrap().join(".task-mgr"));
    }

    #[test]
    fn default_relative_in_main_worktree_anchors_to_self() {
        // Main worktree: `git_common_dir` parent equals cwd.
        let repo = init_git_repo();
        let _g = CwdGuard::enter(repo.path());

        let r = resolve_db_dir(Path::new(".task-mgr"), false, false);
        assert_eq!(r.source, DbDirSource::WorktreeAnchored);
        assert_eq!(
            r.path,
            std::fs::canonicalize(repo.path())
                .unwrap()
                .join(".task-mgr"),
        );
    }
}
