//! Shared test helpers for learnings module tests.

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, open_connection};

/// Create a temporary database with the full schema applied.
pub(crate) fn setup_db() -> (TempDir, Connection) {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    (temp_dir, conn)
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
