//! End-to-end tests for the worktree DB resolution fix.
//!
//! Spawns the real `task-mgr` binary from a worktree cwd (and various
//! permutations) and asserts the row lands in the *main* repository's DB,
//! never in a stray `<worktree>/.task-mgr/`. Covers:
//!
//! 1. `task-mgr add` from worktree root → main DB.
//! 2. `task-mgr add` from a subdirectory of the worktree → main DB.
//! 3. `TASK_MGR_DIR=<abs>` overrides everything → row lands at the env path.
//! 4. Explicit `--dir ./scratch` from a worktree → row lands at the
//!    *worktree*-local path (per-user decision: explicit wins literally).
//! 5. Symlinked worktree path → still resolves to the canonical main DB.
//! 6. Stray-DB warning fires when a pre-existing
//!    `<worktree>/.task-mgr/tasks.db` is shadowed by main-repo anchoring.
//! 7. `task-mgr models show` from a worktree prints the resolved db_dir
//!    plus its source label.
//!
//! These tests deliberately use the compiled binary (`CARGO_BIN_EXE_task-mgr`)
//! rather than calling the library directly — the bug only manifests in the
//! subprocess case where clap's env/default resolution fires from scratch.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn task_mgr_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_task-mgr"))
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .expect("git");
    assert!(status.success(), "git {:?}", args);
}

/// Initialize a git repo with one empty commit so worktrees can be added.
fn init_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "--initial-branch=main"]);
    git(tmp.path(), &["config", "user.email", "t@t"]);
    git(tmp.path(), &["config", "user.name", "t"]);
    git(tmp.path(), &["commit", "--allow-empty", "-m", "init"]);
    tmp
}

/// Bring a freshly-created DB at `<dir>/tasks.db` to the latest schema.
/// `task-mgr add` calls `open_connection` (not `open_and_migrate`), so
/// the test must seed the schema explicitly — this matches what
/// `task-mgr init --from-json` would do for a real user.
fn migrate_db(db_dir: &Path) {
    std::fs::create_dir_all(db_dir).unwrap();
    let out = Command::new(task_mgr_bin())
        .args(["--dir"])
        .arg(db_dir)
        .args(["migrate", "all"])
        .env_remove("TASK_MGR_DIR")
        .output()
        .expect("spawn task-mgr migrate");
    assert!(
        out.status.success(),
        "task-mgr migrate failed at {}: {}",
        db_dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Add a worktree to `repo` at `worktrees_dir/name` and return its path.
fn add_worktree(repo: &Path, worktrees_dir: &Path, name: &str) -> PathBuf {
    let wt = worktrees_dir.join(name);
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            &format!("feat/{}", name),
            wt.to_str().unwrap(),
        ],
    );
    wt
}

/// Build the JSON payload `task-mgr add --stdin` accepts.
fn add_payload(id: &str) -> String {
    serde_json::json!({
        "id": id,
        "title": format!("test task {id}"),
        "difficulty": "medium",
        "touchesFiles": [],
    })
    .to_string()
}

/// Run `task-mgr add --stdin <payload>` with the given cwd and env tweaks.
/// Returns (stdout, stderr, exit_status).
fn run_add(
    cwd: &Path,
    payload: &str,
    extra_env: &[(&str, &Path)],
    extra_args: &[&str],
) -> (String, String, std::process::ExitStatus) {
    use std::io::Write;
    use std::process::Stdio;

    let mut cmd = Command::new(task_mgr_bin());
    cmd.current_dir(cwd)
        .args(extra_args)
        .args(["add", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Don't let the user's TASK_MGR_DIR (or any other repo-relative env)
        // leak into the subprocess and confound the test.
        .env_remove("TASK_MGR_DIR");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().expect("spawn task-mgr");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait task-mgr");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status,
    )
}

/// Assert that `<dir>/tasks.db` contains a row with the given id.
fn assert_task_in_db(db_dir: &Path, id: &str) {
    let conn = rusqlite::Connection::open(db_dir.join("tasks.db"))
        .unwrap_or_else(|e| panic!("expected DB at {}: {e}", db_dir.join("tasks.db").display()));
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        count,
        1,
        "expected 1 row with id={id} in {}",
        db_dir.join("tasks.db").display()
    );
}

#[test]
fn add_from_worktree_root_lands_in_main_db() {
    let repo = init_repo();
    migrate_db(&repo.path().join(".task-mgr"));
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "from-root");

    let (_stdout, stderr, status) = run_add(&wt, &add_payload("WT-001"), &[], &[]);
    assert!(status.success(), "task-mgr add failed: {stderr}");

    assert_task_in_db(&repo.path().join(".task-mgr"), "WT-001");
    assert!(
        !wt.join(".task-mgr").exists(),
        "stray worktree DB should NOT have been created at {}",
        wt.join(".task-mgr").display()
    );
}

#[test]
fn add_from_worktree_subdirectory_lands_in_main_db() {
    let repo = init_repo();
    migrate_db(&repo.path().join(".task-mgr"));
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "from-sub");
    let nested = wt.join("nested/deeper");
    std::fs::create_dir_all(&nested).unwrap();

    let (_stdout, stderr, status) = run_add(&nested, &add_payload("WT-002"), &[], &[]);
    assert!(status.success(), "task-mgr add failed: {stderr}");

    assert_task_in_db(&repo.path().join(".task-mgr"), "WT-002");
    assert!(!wt.join(".task-mgr").exists());
    assert!(!nested.join(".task-mgr").exists());
}

#[test]
fn task_mgr_dir_env_overrides_to_arbitrary_path() {
    let repo = init_repo();
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "env-override");

    let custom = TempDir::new().unwrap();
    migrate_db(custom.path());
    let (_stdout, stderr, status) = run_add(
        &wt,
        &add_payload("WT-003"),
        &[("TASK_MGR_DIR", custom.path())],
        &[],
    );
    assert!(status.success(), "task-mgr add failed: {stderr}");

    assert_task_in_db(custom.path(), "WT-003");
    assert!(!wt.join(".task-mgr").exists());
    assert!(!repo.path().join(".task-mgr/tasks.db").exists());
}

#[test]
fn explicit_relative_dir_from_worktree_is_cwd_relative() {
    // Per-user decision: explicit `--dir` is honored literally, even from a
    // worktree. This is the "principle of least surprise" exit hatch — if
    // someone really wants a per-worktree scratch DB, they can ask for it.
    let repo = init_repo();
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "explicit");
    migrate_db(&wt.join("scratch"));

    let (_stdout, stderr, status) =
        run_add(&wt, &add_payload("WT-004"), &[], &["--dir", "./scratch"]);
    assert!(status.success(), "task-mgr add failed: {stderr}");

    assert_task_in_db(&wt.join("scratch"), "WT-004");
    // Default-path side effects must not occur when --dir is explicit.
    assert!(!wt.join(".task-mgr").exists());
    assert!(!repo.path().join(".task-mgr/tasks.db").exists());
}

#[cfg(unix)]
#[test]
fn symlinked_worktree_resolves_to_canonical_main_db() {
    let repo = init_repo();
    migrate_db(&repo.path().join(".task-mgr"));
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "symlinked");

    // Reach the worktree via a symlink in a third location.
    let link_parent = TempDir::new().unwrap();
    let link = link_parent.path().join("wt-link");
    std::os::unix::fs::symlink(&wt, &link).unwrap();

    let (_stdout, stderr, status) = run_add(&link, &add_payload("WT-005"), &[], &[]);
    assert!(status.success(), "task-mgr add failed: {stderr}");

    assert_task_in_db(&repo.path().join(".task-mgr"), "WT-005");
    assert!(!wt.join(".task-mgr").exists());
    assert!(!link.join(".task-mgr").exists());
}

#[test]
fn pre_existing_stray_db_triggers_warning_and_is_ignored() {
    let repo = init_repo();
    migrate_db(&repo.path().join(".task-mgr"));
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "stray");

    // Pre-create a stray DB at the cwd-default location with a sentinel row
    // so we can prove `task-mgr add` did NOT touch it.
    let stray_dir = wt.join(".task-mgr");
    migrate_db(&stray_dir);
    {
        let (_o, e, s) = run_add(
            &wt,
            &add_payload("STRAY-PRE"),
            &[("TASK_MGR_DIR", stray_dir.as_path())],
            &[],
        );
        assert!(s.success(), "stray-DB seeding failed: {e}");
    }

    // Now run a default-cwd `task-mgr add` from the worktree — should
    // anchor to main repo and emit a stray-DB warning on stderr.
    let (_stdout, stderr, status) = run_add(&wt, &add_payload("MAIN-001"), &[], &[]);
    assert!(status.success(), "task-mgr add failed: {stderr}");

    assert!(
        stderr.contains("stray DB"),
        "expected stray-DB warning on stderr, got: {stderr}"
    );

    // The new row landed in the main DB.
    assert_task_in_db(&repo.path().join(".task-mgr"), "MAIN-001");
    // The stray DB still has only its original row, untouched.
    let conn = rusqlite::Connection::open(stray_dir.join("tasks.db")).unwrap();
    let stray_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        stray_count, 1,
        "stray DB must be untouched (1 sentinel row only)"
    );
}

#[test]
fn models_show_from_worktree_prints_db_dir_and_source() {
    let repo = init_repo();
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "models-show");

    let out = Command::new(task_mgr_bin())
        .current_dir(&wt)
        .args(["models", "show"])
        .env_remove("TASK_MGR_DIR")
        .output()
        .expect("spawn task-mgr models show");
    assert!(
        out.status.success(),
        "task-mgr models show failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("db_dir:"),
        "models show must report db_dir, got: {stdout}"
    );
    assert!(
        stdout.contains("worktree-anchored"),
        "models show in a worktree must label source as worktree-anchored, got: {stdout}"
    );
    let canonical_main = std::fs::canonicalize(repo.path()).unwrap();
    assert!(
        stdout.contains(&canonical_main.join(".task-mgr").display().to_string())
            || stdout.contains(&repo.path().join(".task-mgr").display().to_string()),
        "models show must mention the main-repo .task-mgr path, got: {stdout}"
    );
}
