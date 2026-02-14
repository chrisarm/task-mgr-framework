//! Integration tests for error handling.
//!
//! These tests verify that proper errors are returned in edge cases
//! and that error messages are helpful to users.

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use task_mgr::cli::FailStatus;
use task_mgr::commands::{complete, fail, init, show, skip};
use task_mgr::db::{create_schema, open_connection, LockGuard};
use task_mgr::error::TaskMgrError;

/// Get the path to the sample PRD fixture file.
fn sample_prd_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_prd.json")
}

/// Set up a fresh database with schema.
fn setup_db() -> (TempDir, rusqlite::Connection) {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    (temp_dir, conn)
}

// =============================================================================
// Test: invalid JSON PRD files produce helpful errors
// =============================================================================

#[test]
fn test_init_with_nonexistent_file() {
    let temp_dir = TempDir::new().unwrap();
    let nonexistent = temp_dir.path().join("does_not_exist.json");

    let result = init::init(temp_dir.path(), &[nonexistent], false, false, false, false, init::PrefixMode::Disabled);

    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error message should mention the file and the issue
    assert!(
        msg.contains("I/O error") || msg.contains("Failed to read"),
        "Error should indicate file read failure: {}",
        msg
    );
    assert!(
        msg.contains("does_not_exist.json"),
        "Error should mention the file name: {}",
        msg
    );
}

#[test]
fn test_init_with_invalid_json() {
    let temp_dir = TempDir::new().unwrap();
    let invalid_json = temp_dir.path().join("invalid.json");

    // Write invalid JSON content
    fs::write(&invalid_json, "{ not valid json }").unwrap();

    let result = init::init(temp_dir.path(), &[invalid_json], false, false, false, false, init::PrefixMode::Disabled);

    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error message should indicate JSON parsing failure
    assert!(
        msg.contains("JSON error"),
        "Error should indicate JSON parsing failure: {}",
        msg
    );
}

#[test]
fn test_init_with_missing_required_fields() {
    let temp_dir = TempDir::new().unwrap();
    let incomplete_json = temp_dir.path().join("incomplete.json");

    // Write JSON missing required fields (no "project" field)
    fs::write(&incomplete_json, r#"{"userStories": []}"#).unwrap();

    let result = init::init(
        temp_dir.path(),
        &[incomplete_json],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    );

    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error message should indicate which field is missing
    assert!(
        msg.contains("JSON error"),
        "Error should indicate JSON parsing failure: {}",
        msg
    );
    assert!(
        msg.contains("project") || msg.contains("missing field"),
        "Error should mention the missing field: {}",
        msg
    );
}

#[test]
fn test_init_with_invalid_story_structure() {
    let temp_dir = TempDir::new().unwrap();
    let bad_story_json = temp_dir.path().join("bad_story.json");

    // Write JSON with invalid story structure (missing required story fields)
    fs::write(
        &bad_story_json,
        r#"{
            "project": "test",
            "userStories": [
                {"title": "Missing id and other fields"}
            ]
        }"#,
    )
    .unwrap();

    let result = init::init(
        temp_dir.path(),
        &[bad_story_json],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    );

    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error message should indicate JSON parsing failure
    assert!(
        msg.contains("JSON error"),
        "Error should indicate JSON parsing failure: {}",
        msg
    );
}

// =============================================================================
// Test: lock acquisition failures show holder PID
// =============================================================================

#[test]
fn test_lock_acquisition_shows_holder_pid() {
    let temp_dir = TempDir::new().unwrap();

    // Acquire the lock first
    let _guard1 = LockGuard::acquire(temp_dir.path()).unwrap();
    let our_pid = std::process::id();

    // Try to acquire again - should fail
    let result = LockGuard::acquire(temp_dir.path());

    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error message should mention lock
    assert!(
        msg.contains("Lock error") || msg.contains("locked"),
        "Error should mention lock: {}",
        msg
    );

    // Error message should include the holder PID
    assert!(
        msg.contains(&our_pid.to_string()),
        "Error should contain holder PID {}: {}",
        our_pid,
        msg
    );
}

#[test]
fn test_lock_released_allows_new_acquisition() {
    let temp_dir = TempDir::new().unwrap();

    // Acquire and drop lock
    {
        let _guard = LockGuard::acquire(temp_dir.path()).unwrap();
        // Lock held here
    }
    // Lock released after scope ends

    // Should be able to acquire again
    let result = LockGuard::acquire(temp_dir.path());
    assert!(
        result.is_ok(),
        "Should be able to acquire lock after previous holder dropped: {:?}",
        result.err()
    );
}

// =============================================================================
// Test: task not found errors
// =============================================================================

#[test]
fn test_show_nonexistent_task() {
    let temp_dir = TempDir::new().unwrap();

    // Initialize with sample PRD
    init::init(
        temp_dir.path(),
        &[sample_prd_path()],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Try to show a task that doesn't exist
    let result = show::show(temp_dir.path(), "NONEXISTENT-999");

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Should be a NotFound error
    match &err {
        TaskMgrError::NotFound { resource_type, id } => {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "NONEXISTENT-999");
        }
        _ => panic!("Expected NotFound error, got: {:?}", err),
    }

    // Message should be helpful
    let msg = err.to_string();
    assert!(
        msg.contains("not found"),
        "Error message should say not found: {}",
        msg
    );
    assert!(
        msg.contains("NONEXISTENT-999"),
        "Error message should include task ID: {}",
        msg
    );
}

#[test]
fn test_complete_nonexistent_task() {
    let (temp_dir, _conn) = setup_db();

    // Initialize with sample PRD
    init::init(
        temp_dir.path(),
        &[sample_prd_path()],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Reopen connection for mutable access
    let mut conn = open_connection(temp_dir.path()).unwrap();

    // Try to complete a task that doesn't exist
    let result = complete::complete(
        &mut conn,
        &["NONEXISTENT-999".to_string()],
        None,
        None,
        false,
    );

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Should be a NotFound error
    match &err {
        TaskMgrError::NotFound { resource_type, id } => {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "NONEXISTENT-999");
        }
        _ => panic!("Expected NotFound error, got: {:?}", err),
    }
}

#[test]
fn test_fail_nonexistent_task() {
    let (temp_dir, mut conn) = setup_db();

    // Initialize with sample PRD
    init::init(
        temp_dir.path(),
        &[sample_prd_path()],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Try to fail a task that doesn't exist
    let result = fail::fail(
        &mut conn,
        &["NONEXISTENT-999".to_string()],
        Some("test error"),
        FailStatus::Blocked,
        None,
        false,
    );

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Should be a NotFound error
    match &err {
        TaskMgrError::NotFound { resource_type, id } => {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "NONEXISTENT-999");
        }
        _ => panic!("Expected NotFound error, got: {:?}", err),
    }
}

#[test]
fn test_skip_nonexistent_task() {
    let (temp_dir, mut conn) = setup_db();

    // Initialize with sample PRD
    init::init(
        temp_dir.path(),
        &[sample_prd_path()],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Try to skip a task that doesn't exist
    let result = skip::skip(
        &mut conn,
        &["NONEXISTENT-999".to_string()],
        "test reason",
        None,
    );

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Should be a NotFound error
    match &err {
        TaskMgrError::NotFound { resource_type, id } => {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "NONEXISTENT-999");
        }
        _ => panic!("Expected NotFound error, got: {:?}", err),
    }
}

// =============================================================================
// Test: duplicate task handling
// =============================================================================

#[test]
fn test_init_duplicate_task_ids_across_files() {
    let temp_dir = TempDir::new().unwrap();

    // Create two JSON files with the same task ID
    let file1 = temp_dir.path().join("file1.json");
    let file2 = temp_dir.path().join("file2.json");

    fs::write(
        &file1,
        r#"{
            "project": "test",
            "userStories": [
                {"id": "TASK-001", "title": "Task 1", "priority": 1, "passes": false}
            ]
        }"#,
    )
    .unwrap();

    fs::write(
        &file2,
        r#"{
            "project": "test",
            "userStories": [
                {"id": "TASK-001", "title": "Duplicate Task 1", "priority": 1, "passes": false}
            ]
        }"#,
    )
    .unwrap();

    // Try to import both files
    let result = init::init(temp_dir.path(), &[file1, file2], false, false, false, false, init::PrefixMode::Disabled);

    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error should mention duplicate task
    assert!(
        msg.contains("TASK-001") || msg.contains("Duplicate"),
        "Error should mention the duplicate task: {}",
        msg
    );
}

#[test]
fn test_init_without_force_fails_on_existing_data() {
    let temp_dir = TempDir::new().unwrap();

    // First import
    init::init(
        temp_dir.path(),
        &[sample_prd_path()],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Try to import again without --force or --append
    let result = init::init(
        temp_dir.path(),
        &[sample_prd_path()],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    );

    // Should fail because data already exists (either UNIQUE constraint or explicit duplicate check)
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error message should indicate duplicate or existing data issue
    // The actual error is a UNIQUE constraint failure at the database level
    assert!(
        msg.contains("Duplicate") || msg.contains("UNIQUE") || msg.contains("constraint"),
        "Error should indicate duplicate/existing data: {}",
        msg
    );
}

// =============================================================================
// Test: invalid dependency handling
// =============================================================================

#[test]
fn test_init_with_invalid_dependency() {
    let temp_dir = TempDir::new().unwrap();
    let bad_deps_json = temp_dir.path().join("bad_deps.json");

    // Write JSON with a dependency to a non-existent task
    fs::write(
        &bad_deps_json,
        r#"{
            "project": "test",
            "userStories": [
                {
                    "id": "TASK-002",
                    "title": "Task with bad dependency",
                    "priority": 1,
                    "passes": false,
                    "dependsOn": ["NONEXISTENT-999"]
                }
            ]
        }"#,
    )
    .unwrap();

    let result = init::init(
        temp_dir.path(),
        &[bad_deps_json],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    );

    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error should mention the invalid dependency
    assert!(
        msg.contains("NONEXISTENT-999") || msg.contains("dependency"),
        "Error should mention the invalid dependency: {}",
        msg
    );
}

// =============================================================================
// Test: invalid state transitions
// =============================================================================

#[test]
fn test_skip_already_done_task() {
    let (_temp_dir, mut conn) = setup_db();

    // Create a task that's already done
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('TASK-DONE', 'Done Task', 'done')",
        [],
    )
    .unwrap();

    // Try to skip a done task
    let result = skip::skip(
        &mut conn,
        &["TASK-DONE".to_string()],
        "trying to skip done task",
        None,
    );

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Should be an InvalidState error
    match &err {
        TaskMgrError::InvalidState {
            resource_type,
            id,
            expected,
            actual,
        } => {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "TASK-DONE");
            assert_eq!(actual, "done");
            assert!(
                expected.contains("todo") || expected.contains("in_progress"),
                "Expected should mention valid states: {}",
                expected
            );
        }
        _ => panic!("Expected InvalidState error, got: {:?}", err),
    }
}

// =============================================================================
// Test: database connection errors are handled gracefully
// =============================================================================

#[test]
fn test_open_connection_with_invalid_path() {
    // Try to open a database in a directory that doesn't exist and can't be created
    // On Unix, /dev/null is not a directory
    let result = open_connection(std::path::Path::new("/dev/null/impossible/path"));

    // Should fail with an I/O or database error
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();

    // Error should be helpful
    assert!(
        msg.contains("I/O error") || msg.contains("Database error"),
        "Error should indicate path issue: {}",
        msg
    );
}

// =============================================================================
// Test: run not found errors
// =============================================================================

#[test]
fn test_complete_with_nonexistent_run_id() {
    let (_temp_dir, mut conn) = setup_db();

    // Create a todo task
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('TASK-001', 'Test Task', 'in_progress')",
        [],
    )
    .unwrap();

    // Try to complete with a non-existent run ID
    let result = complete::complete(
        &mut conn,
        &["TASK-001".to_string()],
        Some("nonexistent-run-id"),
        None,
        false,
    );

    // This might succeed (run_tasks just won't be updated) or fail
    // depending on implementation. If it fails, it should be a NotFound error.
    if let Err(err) = result {
        let msg = err.to_string();
        // If there's an error, it should be about the run
        assert!(
            msg.contains("Run") || msg.contains("run"),
            "Error should mention run: {}",
            msg
        );
    }
    // If it succeeds, that's also acceptable behavior (run tracking is optional)
}

// =============================================================================
// Test: empty database edge cases
// =============================================================================

#[test]
fn test_show_on_empty_database() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Try to show a task on empty database
    let result = show::show(temp_dir.path(), "ANY-TASK");

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Should be a NotFound error
    match &err {
        TaskMgrError::NotFound { resource_type, id } => {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "ANY-TASK");
        }
        _ => panic!("Expected NotFound error, got: {:?}", err),
    }
}

#[test]
fn test_complete_on_empty_database() {
    let (_temp_dir, mut conn) = setup_db();

    // Try to complete a task on empty database
    let result = complete::complete(&mut conn, &["ANY-TASK".to_string()], None, None, false);

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Should be a NotFound error
    match &err {
        TaskMgrError::NotFound { resource_type, id } => {
            assert_eq!(resource_type, "Task");
            assert_eq!(id, "ANY-TASK");
        }
        _ => panic!("Expected NotFound error, got: {:?}", err),
    }
}
