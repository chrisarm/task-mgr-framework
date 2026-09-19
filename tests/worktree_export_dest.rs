//! Live-path tests for export `--to-json` dest identity across a linked
//! worktree (US-006 / FEAT-010).
//!
//! Pattern mirrors `tests/worktree_db_resolution.rs` (spawn `git worktree
//! add` + real `CARGO_BIN_EXE_task-mgr`); do **not** claim these via the
//! verify-task-mgr sandbox (sandboxes are not linked worktrees).
//!
//! Covers:
//! 1. Worktree registered dest without `--force` → refuse; bytes identical
//!    on worktree + main JSON.
//! 2. Same dest + `--force` → worktree file replaced (lossy dump); main
//!    JSON unchanged (`--to-json` PATH never remapped away).
//! 3. Unregistered `--to-json` from worktree cwd → no `--force` required.
//! 4. DB anchoring unchanged: rows stay in main-repo `.task-mgr`.
//! 5. `--from-json` of the worktree file scopes the dump **source**; dest
//!    remains the `--to-json` PATH.

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

const LIVE_PRD_JSON: &str = r#"{
  "project": "live-export-dest",
  "branchName": "feat/export-dest",
  "taskPrefix": "LIVE",
  "extraKeepMe": true,
  "userStories": [
    {"id": "SEED-001", "title": "seed", "priority": 50, "passes": false}
  ]
}"#;

/// Init git repo, write `tasks/foo.json`, `task-mgr init` + `loop init --prefix LIVE`.
/// Forces relative `prd_files` path for pin-19 (c) remap math.
fn setup_repo_with_registered_prd() -> (TempDir, PathBuf) {
    let repo = init_repo();
    let tasks = repo.path().join("tasks");
    std::fs::create_dir_all(&tasks).unwrap();
    let prd = tasks.join("foo.json");
    std::fs::write(&prd, LIVE_PRD_JSON).unwrap();
    // Commit so worktree add copies the JSON.
    git(repo.path(), &["add", "tasks/foo.json"]);
    git(repo.path(), &["commit", "-m", "add prd"]);

    let init_out = Command::new(task_mgr_bin())
        .current_dir(repo.path())
        .args(["init"])
        .env_remove("TASK_MGR_DIR")
        .env_remove("TASK_MGR_ACTIVE_PREFIX")
        .output()
        .expect("spawn init");
    assert!(
        init_out.status.success(),
        "task-mgr init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );

    let loop_out = Command::new(task_mgr_bin())
        .current_dir(repo.path())
        .args(["loop", "init", "--prefix", "LIVE", "tasks/foo.json"])
        .env_remove("TASK_MGR_DIR")
        .env_remove("TASK_MGR_ACTIVE_PREFIX")
        .output()
        .expect("spawn loop init");
    assert!(
        loop_out.status.success(),
        "task-mgr loop init failed: {}",
        String::from_utf8_lossy(&loop_out.stderr)
    );

    // Prefer the relative `tasks/foo.json` form for remap math.
    let conn = rusqlite::Connection::open(repo.path().join(".task-mgr/tasks.db")).unwrap();
    conn.execute(
        "UPDATE prd_files SET file_path = 'tasks/foo.json' WHERE file_type = 'task_list'",
        [],
    )
    .unwrap();

    (repo, prd)
}

/// Run `task-mgr export` with the given cwd and args. Returns (stdout, stderr, status).
fn run_export(cwd: &Path, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let out = Command::new(task_mgr_bin())
        .current_dir(cwd)
        .arg("export")
        .args(args)
        .env_remove("TASK_MGR_DIR")
        .env_remove("TASK_MGR_ACTIVE_PREFIX")
        .output()
        .expect("spawn task-mgr export");
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
fn worktree_registered_dest_without_force_refuses_bytes_identical() {
    let (repo, main_prd) = setup_repo_with_registered_prd();
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "exp-refuse");
    let wt_prd = wt.join("tasks/foo.json");
    assert!(wt_prd.is_file(), "worktree must carry tasks/foo.json");

    // Re-seed distinctive extra key on the worktree copy (loop init may rewrite).
    std::fs::write(&wt_prd, LIVE_PRD_JSON).unwrap();
    let wt_before = std::fs::read(&wt_prd).unwrap();
    let main_before = std::fs::read(&main_prd).unwrap();

    let (stdout, stderr, status) = run_export(
        &wt,
        &[
            "--to-json",
            wt_prd.to_str().unwrap(),
            "--from-json",
            wt_prd.to_str().unwrap(),
        ],
    );
    assert!(
        !status.success(),
        "registered worktree dest without --force must refuse: stdout={stdout} stderr={stderr}"
    );
    let err = format!("{stdout}{stderr}");
    assert!(
        err.contains("--force") && err.to_lowercase().contains("dump"),
        "refuse must name --force and dump-not-merge: {err}"
    );

    assert_eq!(
        std::fs::read(&wt_prd).unwrap(),
        wt_before,
        "worktree dest bytes must be identical after refuse"
    );
    assert_eq!(
        std::fs::read(&main_prd).unwrap(),
        main_before,
        "main JSON must be unchanged after refuse"
    );
    assert_task_in_db(&repo.path().join(".task-mgr"), "LIVE-SEED-001");
    assert!(
        !wt.join(".task-mgr").exists(),
        "DB must stay on main-repo .task-mgr"
    );
}

#[test]
fn worktree_registered_dest_with_force_replaces_lossy_main_unchanged() {
    let (repo, main_prd) = setup_repo_with_registered_prd();
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "exp-force");
    let wt_prd = wt.join("tasks/foo.json");
    assert!(wt_prd.is_file(), "worktree must carry tasks/foo.json");

    // Distinctive extra keys that a lossy ExportedPrd dump must strip.
    std::fs::write(&wt_prd, LIVE_PRD_JSON).unwrap();
    let main_before = std::fs::read(&main_prd).unwrap();

    let (stdout, stderr, status) = run_export(
        &wt,
        &[
            "--to-json",
            wt_prd.to_str().unwrap(),
            "--from-json",
            wt_prd.to_str().unwrap(),
            "--force",
        ],
    );
    assert!(
        status.success(),
        "export --force onto worktree dest failed: stdout={stdout} stderr={stderr}"
    );

    // Dest PATH is the worktree path (never remapped to main).
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains(wt_prd.to_str().unwrap())
            || combined.contains(&wt_prd.display().to_string()),
        "export result must report the worktree --to-json PATH, got: {combined}"
    );

    let wt_after = std::fs::read_to_string(&wt_prd).unwrap();
    assert!(
        !wt_after.contains("extraKeepMe"),
        "force dump must not preserve extra keys: {wt_after}"
    );
    assert!(
        !wt_after.contains("taskPrefix"),
        "ExportedPrd must stay lossy (no taskPrefix): {wt_after}"
    );
    assert!(
        wt_after.contains("live-export-dest"),
        "force dump must write scoped project metadata: {wt_after}"
    );
    assert!(
        wt_after.contains("SEED-001"),
        "force dump must include LIVE seed task: {wt_after}"
    );

    assert_eq!(
        std::fs::read(&main_prd).unwrap(),
        main_before,
        "main JSON must be unchanged when --to-json is the worktree PATH"
    );
    assert_task_in_db(&repo.path().join(".task-mgr"), "LIVE-SEED-001");
    assert!(
        !wt.join(".task-mgr").exists(),
        "DB must stay on main-repo .task-mgr"
    );
}

#[test]
fn worktree_unregistered_dest_writes_without_force() {
    let (repo, main_prd) = setup_repo_with_registered_prd();
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "exp-unreg");
    let wt_prd = wt.join("tasks/foo.json");
    assert!(wt_prd.is_file());

    let main_before = std::fs::read(&main_prd).unwrap();
    let wt_prd_before = std::fs::read(&wt_prd).unwrap();

    // Existing unregistered file under the worktree — identity miss.
    let dump = wt.join("tasks/scratch-export.json");
    std::fs::write(&dump, r#"{"scratch":true}"#).unwrap();

    let (stdout, stderr, status) = run_export(
        &wt,
        &[
            "--to-json",
            dump.to_str().unwrap(),
            "--from-json",
            wt_prd.to_str().unwrap(),
        ],
    );
    assert!(
        status.success(),
        "unregistered dest must not require --force: stdout={stdout} stderr={stderr}"
    );

    let after = std::fs::read_to_string(&dump).unwrap();
    assert!(
        !after.contains("scratch"),
        "export must overwrite unregistered dest contents: {after}"
    );
    assert!(
        after.contains("live-export-dest") && after.contains("SEED-001"),
        "dump must carry LIVE scoped content: {after}"
    );

    assert_eq!(
        std::fs::read(&main_prd).unwrap(),
        main_before,
        "main JSON unchanged for unregistered dest"
    );
    assert_eq!(
        std::fs::read(&wt_prd).unwrap(),
        wt_prd_before,
        "registered worktree JSON unchanged when dest is elsewhere"
    );
    assert_task_in_db(&repo.path().join(".task-mgr"), "LIVE-SEED-001");
    assert!(
        !wt.join(".task-mgr").exists(),
        "DB must stay on main-repo .task-mgr"
    );
}

#[test]
fn worktree_from_json_scopes_source_dest_is_to_json_path() {
    let (repo, main_prd) = setup_repo_with_registered_prd();
    let wt_parent = TempDir::new().unwrap();
    let wt = add_worktree(repo.path(), wt_parent.path(), "exp-from-json");
    let wt_prd = wt.join("tasks/foo.json");
    assert!(wt_prd.is_file());

    let main_before = std::fs::read(&main_prd).unwrap();
    let wt_prd_before = std::fs::read(&wt_prd).unwrap();

    // Dest is a fresh path (not the registered task-list) — no --force.
    let dest = wt.join("tasks/scoped-out.json");
    assert!(!dest.exists());

    let (stdout, stderr, status) = run_export(
        &wt,
        &[
            "--to-json",
            dest.to_str().unwrap(),
            "--from-json",
            wt_prd.to_str().unwrap(),
        ],
    );
    assert!(
        status.success(),
        "export --from-json worktree pin failed: stdout={stdout} stderr={stderr}"
    );

    assert!(
        dest.is_file(),
        "--to-json PATH must be the write target (never remapped away)"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains(dest.to_str().unwrap()) || combined.contains(&dest.display().to_string()),
        "export must report the --to-json dest PATH, got: {combined}"
    );

    let exported: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dest).unwrap()).unwrap();
    assert_eq!(
        exported["project"].as_str(),
        Some("live-export-dest"),
        "from-json pin must scope metadata to LIVE PRD"
    );
    let stories = exported["userStories"].as_array().expect("userStories");
    assert_eq!(stories.len(), 1);
    assert!(
        stories[0]["id"].as_str().unwrap_or("").contains("SEED-001"),
        "scoped dump must include LIVE seed: {stories:?}"
    );

    assert_eq!(
        std::fs::read(&main_prd).unwrap(),
        main_before,
        "main JSON unchanged when dest is a separate --to-json PATH"
    );
    assert_eq!(
        std::fs::read(&wt_prd).unwrap(),
        wt_prd_before,
        "worktree registered JSON unchanged when dest is separate"
    );
    assert_task_in_db(&repo.path().join(".task-mgr"), "LIVE-SEED-001");
    assert!(
        !wt.join(".task-mgr").exists(),
        "DB must stay on main-repo .task-mgr"
    );
}
