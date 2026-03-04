/// Shared test helpers for loop_engine test modules.
///
/// Consolidates duplicated test setup code (setup_test_db, insert_test_learning,
/// and common DB insert helpers) used across feedback.rs, calibrate.rs, prompt.rs,
/// and engine.rs tests.
use rusqlite::{params, Connection};
use std::path::PathBuf;
use tempfile::TempDir;

use crate::db::migrations::run_migrations;
use crate::db::{create_schema, open_connection};
use crate::learnings::crud::{record_learning, RecordLearningParams};
use crate::models::{Confidence, LearningOutcome};

/// Set up a test database with schema and migrations.
///
/// Returns `(TempDir, Connection)` — the TempDir must be kept alive
/// for the duration of the test to prevent the database file from
/// being deleted.
pub fn setup_test_db() -> (TempDir, Connection) {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (temp_dir, conn)
}

/// Insert a test learning with standard defaults and return its ID.
///
/// This base version does NOT call `record_learning_shown()`.
/// For feedback tests that need window stats initialized, call
/// `bandit::record_learning_shown()` separately after this.
pub fn insert_test_learning(conn: &Connection, title: &str) -> i64 {
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: title.to_string(),
        content: "Test learning content".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    record_learning(conn, params).unwrap().learning_id
}

/// Insert a task into the test database.
pub fn insert_task(conn: &Connection, id: &str, title: &str, status: &str, priority: i32) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)",
        params![id, title, status, priority],
    )
    .unwrap();
}

/// Insert a done task (convenience wrapper for calibration tests).
pub fn insert_done_task(conn: &Connection, id: &str) {
    insert_task(conn, id, "Test task", "done", 10);
}

/// Insert a task with description and acceptance criteria.
pub fn insert_task_full(
    conn: &Connection,
    id: &str,
    title: &str,
    status: &str,
    priority: i32,
    description: &str,
    criteria: &[&str],
) {
    let criteria_json = serde_json::to_string(criteria).unwrap();
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, description, acceptance_criteria) VALUES (?, ?, ?, ?, ?, ?)",
        params![id, title, status, priority, description, criteria_json],
    )
    .unwrap();
}

/// Insert a task_files entry.
pub fn insert_task_file(conn: &Connection, task_id: &str, file_path: &str) {
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES (?, ?)",
        params![task_id, file_path],
    )
    .unwrap();
}

/// Insert a task relationship.
pub fn insert_relationship(conn: &Connection, task_id: &str, related_id: &str, rel_type: &str) {
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?, ?, ?)",
        params![task_id, related_id, rel_type],
    )
    .unwrap();
}

/// Insert a run record.
pub fn insert_run(conn: &Connection, run_id: &str) {
    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES (?, 'active')",
        params![run_id],
    )
    .unwrap();
}

/// Insert a run_task record.
pub fn insert_run_task(conn: &Connection, run_id: &str, task_id: &str, iteration: i32) {
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES (?, ?, 'completed', ?)",
        params![run_id, task_id, iteration],
    )
    .unwrap();
}

/// Initialize a temporary git repository with a single commit.
///
/// Creates a temp directory with `git init -b main`, user config, a README.md,
/// and an initial commit. Returns `(TempDir, PathBuf)` — the TempDir must be
/// kept alive for the duration of the test to prevent the directory from being
/// deleted.
pub fn init_test_repo() -> (TempDir, PathBuf) {
    use std::fs;
    use std::process::Command;

    let tmp = TempDir::new().expect("create temp dir");
    let repo = tmp.path().to_path_buf();
    Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&repo)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&repo)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&repo)
        .output()
        .expect("git config name");
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

/// Set up a temporary git repository for testing.
///
/// Delegates to [`init_test_repo`] and returns only the TempDir for callers
/// that access the path via `tmp.path()`.
pub fn setup_git_repo() -> TempDir {
    let (tmp, _) = init_test_repo();
    tmp
}
