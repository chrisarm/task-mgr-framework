//! Tests for the fail command.

use super::*;
use crate::TaskMgrError;
use crate::cli::FailStatus;
use crate::db::{create_schema, open_connection};
use crate::models::TaskStatus;
use tempfile::TempDir;

fn setup_test_db() -> (TempDir, rusqlite::Connection) {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    (temp_dir, conn)
}

fn insert_test_task(conn: &rusqlite::Connection, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, error_count) VALUES (?, 'Test Task', ?, 10, 0)",
        rusqlite::params![id, status],
    )
    .unwrap();
}

#[test]
fn test_fail_todo_task_requires_force() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "todo");

    // Without force, should return InvalidTransition error
    let result = fail(
        &mut conn,
        &["US-001".to_string()],
        Some("Missing dependency"),
        FailStatus::Blocked,
        None,
        false,
    );

    assert!(result.is_err());
    match result {
        Err(TaskMgrError::InvalidTransition {
            task_id, from, to, ..
        }) => {
            assert_eq!(task_id, "US-001");
            assert_eq!(from, "todo");
            assert_eq!(to, "blocked");
        }
        _ => panic!("Expected InvalidTransition error"),
    }
}

#[test]
fn test_fail_todo_task_with_force() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-001", "todo");

    // With force=true, should succeed
    let result = fail(
        &mut conn,
        &["US-001".to_string()],
        Some("Missing dependency"),
        FailStatus::Blocked,
        None,
        true,
    )
    .unwrap();

    assert_eq!(result.tasks.len(), 1);
    let task = &result.tasks[0];
    assert_eq!(task.task_id, "US-001");
    assert_eq!(task.previous_status, TaskStatus::Todo);
    assert_eq!(task.new_status, TaskStatus::Blocked);
    assert_eq!(task.error, Some("Missing dependency".to_string()));
    assert_eq!(task.error_count, 1);

    // Verify status was updated
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "blocked");
}

#[test]
fn test_fail_in_progress_task() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-002", "in_progress");

    let result = fail(
        &mut conn,
        &["US-002".to_string()],
        Some("External API down"),
        FailStatus::Blocked,
        None,
        false, // No force needed for in_progress -> blocked
    )
    .unwrap();

    let task = &result.tasks[0];
    assert_eq!(task.previous_status, TaskStatus::InProgress);
    assert_eq!(task.new_status, TaskStatus::Blocked);
}

#[test]
fn test_fail_with_skipped_status() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-003", "in_progress");

    let result = fail(
        &mut conn,
        &["US-003".to_string()],
        Some("Out of scope"),
        FailStatus::Skipped,
        None,
        false,
    )
    .unwrap();

    assert_eq!(result.tasks[0].new_status, TaskStatus::Skipped);

    // Verify status in DB
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-003'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "skipped");
}

#[test]
fn test_fail_with_irrelevant_status() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-004", "in_progress");

    let result = fail(
        &mut conn,
        &["US-004".to_string()],
        Some("Requirements changed"),
        FailStatus::Irrelevant,
        None,
        false,
    )
    .unwrap();

    assert_eq!(result.tasks[0].new_status, TaskStatus::Irrelevant);

    // Verify status in DB
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-004'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "irrelevant");
}

#[test]
fn test_fail_without_error_message() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-005", "in_progress");

    let result = fail(
        &mut conn,
        &["US-005".to_string()],
        None,
        FailStatus::Blocked,
        None,
        false,
    )
    .unwrap();

    let task = &result.tasks[0];
    assert!(task.error.is_none());
    assert_eq!(task.error_count, 1);

    // Verify last_error is NULL
    let last_error: Option<String> = conn
        .query_row(
            "SELECT last_error FROM tasks WHERE id = 'US-005'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(last_error.is_none());
}

#[test]
fn test_fail_done_task_fails() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-006", "done");

    let result = fail(
        &mut conn,
        &["US-006".to_string()],
        Some("Should fail"),
        FailStatus::Blocked,
        None,
        false,
    );

    assert!(result.is_err());
    match result {
        Err(TaskMgrError::InvalidTransition { .. }) => {}
        _ => panic!("Expected InvalidTransition error"),
    }
}

#[test]
fn test_fail_nonexistent_task() {
    let (_dir, mut conn) = setup_test_db();

    let result = fail(
        &mut conn,
        &["NONEXISTENT".to_string()],
        Some("Should fail"),
        FailStatus::Blocked,
        None,
        false,
    );

    assert!(result.is_err());
    match result {
        Err(TaskMgrError::NotFound { .. }) => {}
        _ => panic!("Expected NotFound error"),
    }
}

#[test]
fn test_fail_increments_error_count() {
    let (_dir, mut conn) = setup_test_db();
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, error_count) VALUES ('US-007', 'Test', 'in_progress', 10, 3)",
        [],
    )
    .unwrap();

    let result = fail(
        &mut conn,
        &["US-007".to_string()],
        Some("Another error"),
        FailStatus::Blocked,
        None,
        false,
    )
    .unwrap();

    assert_eq!(result.tasks[0].error_count, 4);

    // Verify in DB
    let error_count: i32 = conn
        .query_row(
            "SELECT error_count FROM tasks WHERE id = 'US-007'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(error_count, 4);
}

#[test]
fn test_fail_preserves_existing_notes() {
    let (_dir, mut conn) = setup_test_db();
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority, notes, error_count) VALUES ('US-008', 'Test', 'in_progress', 10, 'Existing notes', 0)",
        [],
    )
    .unwrap();

    fail(
        &mut conn,
        &["US-008".to_string()],
        Some("New error"),
        FailStatus::Blocked,
        None,
        false,
    )
    .unwrap();

    let notes: String = conn
        .query_row("SELECT notes FROM tasks WHERE id = 'US-008'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(notes.contains("Existing notes"));
    assert!(notes.contains("[BLOCKED] New error"));
}

#[test]
fn test_fail_with_run_id() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-009", "in_progress");

    // Create a run and run_task entry
    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES ('run-123', 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-123', 'US-009', 'started', 1)",
        [],
    )
    .unwrap();

    fail(
        &mut conn,
        &["US-009".to_string()],
        Some("Blocked error"),
        FailStatus::Blocked,
        Some("run-123"),
        false,
    )
    .unwrap();

    // Verify run_tasks was updated with 'failed' status for blocked
    let run_task_status: String = conn
        .query_row(
            "SELECT status FROM run_tasks WHERE run_id = 'run-123' AND task_id = 'US-009'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(run_task_status, "failed");
}

#[test]
fn test_fail_skipped_with_run_id() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-010", "in_progress");

    // Create a run and run_task entry
    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES ('run-456', 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, status, iteration) VALUES ('run-456', 'US-010', 'started', 1)",
        [],
    )
    .unwrap();

    fail(
        &mut conn,
        &["US-010".to_string()],
        Some("Skipping"),
        FailStatus::Skipped,
        Some("run-456"),
        false,
    )
    .unwrap();

    // Verify run_tasks was updated with 'skipped' status
    let run_task_status: String = conn
        .query_row(
            "SELECT status FROM run_tasks WHERE run_id = 'run-456' AND task_id = 'US-010'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(run_task_status, "skipped");
}

#[test]
fn test_fail_multiple_tasks() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-011", "in_progress");
    insert_test_task(&conn, "US-012", "in_progress");
    insert_test_task(&conn, "US-013", "in_progress");

    let result = fail(
        &mut conn,
        &[
            "US-011".to_string(),
            "US-012".to_string(),
            "US-013".to_string(),
        ],
        Some("Batch failure"),
        FailStatus::Blocked,
        None,
        false,
    )
    .unwrap();

    assert_eq!(result.failed_count, 3);
    assert_eq!(result.tasks.len(), 3);

    // Verify all tasks are blocked
    for task_id in ["US-011", "US-012", "US-013"] {
        let status: String = conn
            .query_row("SELECT status FROM tasks WHERE id = ?", [task_id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "blocked");
    }
}

#[test]
fn test_fail_multiple_rolls_back_on_error() {
    let (_dir, mut conn) = setup_test_db();
    insert_test_task(&conn, "US-014", "in_progress");
    // US-015 doesn't exist, should cause failure

    let result = fail(
        &mut conn,
        &["US-014".to_string(), "US-015".to_string()],
        Some("Should fail"),
        FailStatus::Blocked,
        None,
        false,
    );

    // Should fail because US-015 doesn't exist
    assert!(result.is_err());

    // US-014 should be rolled back to in_progress (transaction failed)
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-014'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "in_progress");
}

#[test]
fn test_format_text_single_task() {
    let result = FailResult {
        tasks: vec![TaskFailResult {
            task_id: "US-001".to_string(),
            previous_status: TaskStatus::InProgress,
            new_status: TaskStatus::Blocked,
            error: Some("External issue".to_string()),
            error_count: 2,
            next_steps: Some("Use `task-mgr doctor` to check for stale blocked tasks.".to_string()),
        }],
        failed_count: 1,
        run_id: None,
    };

    let text = format_text(&result);
    assert!(text.contains("Marked task US-001 as blocked"));
    assert!(text.contains("was in_progress"));
    assert!(text.contains("Error: External issue"));
    assert!(text.contains("Error count: 2"));
    assert!(text.contains("Next steps:"));
}

#[test]
fn test_format_text_multiple_tasks() {
    let result = FailResult {
        tasks: vec![
            TaskFailResult {
                task_id: "US-001".to_string(),
                previous_status: TaskStatus::InProgress,
                new_status: TaskStatus::Blocked,
                error: Some("Error 1".to_string()),
                error_count: 1,
                next_steps: None,
            },
            TaskFailResult {
                task_id: "US-002".to_string(),
                previous_status: TaskStatus::InProgress,
                new_status: TaskStatus::Blocked,
                error: Some("Error 2".to_string()),
                error_count: 1,
                next_steps: None,
            },
        ],
        failed_count: 2,
        run_id: Some("run-123".to_string()),
    };

    let text = format_text(&result);
    assert!(text.contains("Marked 2 task(s) as failed"));
    assert!(text.contains("US-001"));
    assert!(text.contains("US-002"));
    assert!(text.contains("Run: run-123"));
}

#[test]
fn test_format_text_without_error() {
    let result = FailResult {
        tasks: vec![TaskFailResult {
            task_id: "US-002".to_string(),
            previous_status: TaskStatus::Todo,
            new_status: TaskStatus::Skipped,
            error: None,
            error_count: 1,
            next_steps: Some("Skipped tasks can be picked up later.".to_string()),
        }],
        failed_count: 1,
        run_id: None,
    };

    let text = format_text(&result);
    assert!(text.contains("Marked task US-002 as skipped"));
    assert!(!text.contains("Error:"));
    assert!(text.contains("Error count: 1"));
}

#[test]
fn test_next_steps_for_each_status() {
    let (_dir, mut conn) = setup_test_db();

    // Test blocked (use in_progress so no force needed)
    insert_test_task(&conn, "BLOCKED-1", "in_progress");
    let result = fail(
        &mut conn,
        &["BLOCKED-1".to_string()],
        None,
        FailStatus::Blocked,
        None,
        false,
    )
    .unwrap();
    assert!(
        result.tasks[0]
            .next_steps
            .as_ref()
            .unwrap()
            .contains("doctor")
    );

    // Test skipped
    insert_test_task(&conn, "SKIP-1", "in_progress");
    let result = fail(
        &mut conn,
        &["SKIP-1".to_string()],
        None,
        FailStatus::Skipped,
        None,
        false,
    )
    .unwrap();
    assert!(
        result.tasks[0]
            .next_steps
            .as_ref()
            .unwrap()
            .contains("picked up later")
    );

    // Test irrelevant
    insert_test_task(&conn, "IRREL-1", "in_progress");
    let result = fail(
        &mut conn,
        &["IRREL-1".to_string()],
        None,
        FailStatus::Irrelevant,
        None,
        false,
    )
    .unwrap();
    assert!(
        result.tasks[0]
            .next_steps
            .as_ref()
            .unwrap()
            .contains("permanently excluded")
    );
}
