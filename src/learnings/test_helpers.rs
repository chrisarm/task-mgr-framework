//! Shared test helpers for learnings module tests.

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, migrations::run_migrations, open_connection};

/// Create a temporary database with the full schema and all migrations applied.
pub(crate) fn setup_db() -> (TempDir, Connection) {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (temp_dir, conn)
}

/// Sets `retired_at = datetime('now')` on a learning (simulates a prior retirement).
pub(crate) fn retire_learning(conn: &Connection, id: i64) {
    conn.execute(
        "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
        [id],
    )
    .expect("retire_learning");
}

/// Insert a task and associate file paths with it in task_files.
pub(crate) fn insert_task_with_files(conn: &Connection, task_id: &str, files: &[&str]) {
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES (?1, 'Test Task')",
        [task_id],
    )
    .unwrap();
    for file in files {
        conn.execute(
            "INSERT INTO task_files (task_id, file_path) VALUES (?1, ?2)",
            rusqlite::params![task_id, file],
        )
        .unwrap();
    }
}
