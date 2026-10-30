//! Env and user-config wiring for the worktree root.
//!
//! This binary links the library without `cfg(test)`, so `ensure_worktree`
//! reads `TASK_MGR_WORKTREE_ROOT` and `$XDG_CONFIG_HOME/task-mgr/config.json`.
//! Unit tests cannot: under `cfg(test)` those sources are ignored so a
//! developer config cannot move sibling-path fixtures.
//!
//! One sequential test owns the process env for the whole binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use task_mgr::loop_engine::worktree::{ensure_worktree, remove_worktree};

struct EnvRestore {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn capture(key: &'static str) -> Self {
        Self {
            key,
            prior: std::env::var_os(key),
        }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn init_git_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "test@test.com"]);
    run(&["config", "user.name", "Test"]);
    fs::write(path.join("README.md"), "# Test").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "init"]);
}

fn distinct_mount() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    for candidate in ["/dev/shm", "/tmp"] {
        let path = Path::new(candidate);
        let (Ok(meta), Some(parent)) = (fs::metadata(path), path.parent()) else {
            continue;
        };
        let Ok(parent_meta) = fs::metadata(parent) else {
            continue;
        };
        if meta.is_dir() && meta.dev() != parent_meta.dev() {
            return Some(path.to_path_buf());
        }
    }
    None
}

#[test]
fn worktree_root_env_and_user_config() {
    let _restore_xdg = EnvRestore::capture("XDG_CONFIG_HOME");
    let _restore_root = EnvRestore::capture("TASK_MGR_WORKTREE_ROOT");

    let xdg = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("XDG_CONFIG_HOME", xdg.path()) };
    unsafe { std::env::remove_var("TASK_MGR_WORKTREE_ROOT") };

    let cfg_path = xdg.path().join("task-mgr/config.json");
    fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    fs::write(&cfg_path, "{}\n").unwrap();

    let scratch = tempfile::tempdir().unwrap();
    let repo_name: std::ffi::OsString = format!(
        "wt-root-{}",
        scratch.path().file_name().unwrap().to_string_lossy()
    )
    .into();
    let repo = scratch.path().join(&repo_name);
    init_git_repo(&repo);

    let sibling = ensure_worktree(&repo, "feature/default", true, None).expect("sibling default");
    let expected_sibling = repo
        .parent()
        .unwrap()
        .join(format!("{}-worktrees", repo_name.to_string_lossy()))
        .join("feature-default");
    assert_eq!(sibling, expected_sibling);
    assert!(remove_worktree(&repo, &sibling).unwrap());

    let not_mount = tempfile::tempdir().unwrap();
    let plain = not_mount.path().join("plain-root");
    fs::create_dir(&plain).unwrap();
    fs::write(
        &cfg_path,
        format!(r#"{{"worktreeRoot":"{}"}}"#, plain.display()),
    )
    .unwrap();
    let refused = ensure_worktree(&repo, "feature/configured", true, None).unwrap_err();
    let refused_text = refused.to_string();
    assert!(
        refused_text.contains("same filesystem"),
        "config path must be honored, not silently ignored: {refused_text}"
    );
    assert!(
        refused_text.contains(&plain.display().to_string()),
        "{refused_text}"
    );
    assert!(!plain.join(&repo_name).exists());

    unsafe { std::env::set_var("TASK_MGR_WORKTREE_ROOT", "sibling") };
    let forced = ensure_worktree(&repo, "feature/forced-sibling", true, None)
        .expect("env sibling overrides config");
    let forced_suffix = format!(
        "{}-worktrees/feature-forced-sibling",
        repo_name.to_string_lossy()
    );
    assert!(forced.ends_with(&forced_suffix), "{}", forced.display());
    assert!(remove_worktree(&repo, &forced).unwrap());

    unsafe { std::env::set_var("TASK_MGR_WORKTREE_ROOT", "relative/path") };
    let relative = ensure_worktree(&repo, "feature/relative", true, None).unwrap_err();
    let relative_text = relative.to_string();
    assert!(
        relative_text.contains("TASK_MGR_WORKTREE_ROOT"),
        "{relative_text}"
    );
    assert!(relative_text.contains("absolute"), "{relative_text}");

    let missing = std::env::temp_dir().join(format!(
        "task-mgr-missing-worktree-root-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&missing);
    assert!(!missing.exists());
    unsafe { std::env::set_var("TASK_MGR_WORKTREE_ROOT", &missing) };
    let missing_err = ensure_worktree(&repo, "feature/missing", true, None).unwrap_err();
    let missing_text = missing_err.to_string();
    assert!(
        missing_text.contains(&missing.display().to_string()),
        "{missing_text}"
    );
    assert!(missing_text.contains("does not exist"), "{missing_text}");
    assert!(!missing.exists());

    if let Some(mount) = distinct_mount() {
        unsafe { std::env::set_var("TASK_MGR_WORKTREE_ROOT", &mount) };
        let created =
            ensure_worktree(&repo, "feature/mounted", true, None).expect("create under mount");
        let expected = mount.join(&repo_name).join("feature-mounted");
        assert_eq!(created, expected);
        let status = Command::new("git")
            .args(["-C"])
            .arg(&created)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert!(remove_worktree(&repo, &created).unwrap());
        assert!(!expected.exists());
        assert!(mount.exists());
    }
}
