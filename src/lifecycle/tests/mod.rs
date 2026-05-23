//! Lifecycle test module.

#![cfg(test)]

mod apply_tests;
mod decay_tests;
mod reconcile_repair_tests;
mod recovery_tests;

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::{create_schema, migrations::run_migrations, open_connection};
use crate::lifecycle::TaskLifecycle;
use crate::models::TaskStatus;

fn setup_test_db() -> (TempDir, Connection) {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (temp_dir, conn)
}

fn insert_task(conn: &Connection, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?, 'Test Task', ?, 10)",
        rusqlite::params![id, status],
    )
    .unwrap();
}

fn get_task_status(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |row| {
        row.get(0)
    })
    .ok()
}

fn get_started_at(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT started_at FROM tasks WHERE id = ?", [id], |row| {
        row.get(0)
    })
    .ok()
    .flatten()
}

// --- try_claim tests ---

#[test]
fn try_claim_todo_allowed_succeeds_when_todo() {
    let (_dir, mut conn) = setup_test_db();
    insert_task(&conn, "T-001", "todo");

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-001", &[TaskStatus::Todo]).unwrap()
    };

    assert!(result, "should return true when task was todo");
    assert_eq!(
        get_task_status(&conn, "T-001").as_deref(),
        Some("in_progress")
    );
}

#[test]
fn try_claim_todo_allowed_sets_started_at() {
    let (_dir, mut conn) = setup_test_db();
    insert_task(&conn, "T-002", "todo");

    {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-002", &[TaskStatus::Todo]).unwrap();
    }

    assert!(
        get_started_at(&conn, "T-002").is_some(),
        "started_at should be set after claim"
    );
}

#[test]
fn try_claim_todo_allowed_returns_false_when_in_progress() {
    let (_dir, mut conn) = setup_test_db();
    insert_task(&conn, "T-003", "in_progress");

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-003", &[TaskStatus::Todo]).unwrap()
    };

    assert!(
        !result,
        "should return false when task is already in_progress"
    );
    assert_eq!(
        get_task_status(&conn, "T-003").as_deref(),
        Some("in_progress"),
        "row must be untouched"
    );
}

#[test]
fn try_claim_todo_allowed_returns_false_when_done() {
    let (_dir, mut conn) = setup_test_db();
    insert_task(&conn, "T-004", "done");

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-004", &[TaskStatus::Todo]).unwrap()
    };

    assert!(!result);
    assert_eq!(get_task_status(&conn, "T-004").as_deref(), Some("done"));
}

#[test]
fn try_claim_missing_task_returns_false() {
    let (_dir, mut conn) = setup_test_db();

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        // No error for missing row — rows_affected == 0 → Ok(false)
        lc.try_claim("DOES-NOT-EXIST", &[TaskStatus::Todo]).unwrap()
    };
    assert!(!result);
}

#[test]
fn try_claim_empty_allowed_returns_false_without_db_write() {
    let (_dir, mut conn) = setup_test_db();
    insert_task(&conn, "T-005", "todo");

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-005", &[]).unwrap()
    };

    assert!(!result, "empty allowed slice must return false");
    // Row must be untouched — still 'todo'
    assert_eq!(get_task_status(&conn, "T-005").as_deref(), Some("todo"));
}

#[test]
fn try_claim_todo_or_in_progress_allowed_succeeds_when_todo() {
    let (_dir, mut conn) = setup_test_db();
    insert_task(&conn, "T-006", "todo");

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-006", &[TaskStatus::Todo, TaskStatus::InProgress])
            .unwrap()
    };

    assert!(result);
    assert_eq!(
        get_task_status(&conn, "T-006").as_deref(),
        Some("in_progress")
    );
}

#[test]
fn try_claim_todo_or_in_progress_allowed_succeeds_when_in_progress() {
    let (_dir, mut conn) = setup_test_db();
    insert_task(&conn, "T-007", "in_progress");

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-007", &[TaskStatus::Todo, TaskStatus::InProgress])
            .unwrap()
    };

    assert!(result, "idempotent re-claim should succeed");
    assert_eq!(
        get_task_status(&conn, "T-007").as_deref(),
        Some("in_progress")
    );
}

#[test]
fn try_claim_slot_resumption_refreshes_started_at() {
    let (_dir, mut conn) = setup_test_db();
    // Insert with a known past started_at
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, started_at) VALUES (?, 'Test', 'in_progress', 10, '2000-01-01 00:00:00')",
        ["T-008"],
    )
    .unwrap();

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-008", &[TaskStatus::Todo, TaskStatus::InProgress])
            .unwrap()
    };

    assert!(result);
    let started_at = get_started_at(&conn, "T-008").expect("started_at must be set");
    assert_ne!(
        started_at, "2000-01-01 00:00:00",
        "started_at must be refreshed to now"
    );
}

#[test]
fn try_claim_todo_or_in_progress_returns_false_when_done() {
    let (_dir, mut conn) = setup_test_db();
    insert_task(&conn, "T-009", "done");

    let result = {
        let lc = TaskLifecycle::new(&mut conn);
        lc.try_claim("T-009", &[TaskStatus::Todo, TaskStatus::InProgress])
            .unwrap()
    };

    assert!(!result, "done is not in the allowed set");
    assert_eq!(get_task_status(&conn, "T-009").as_deref(), Some("done"));
}
