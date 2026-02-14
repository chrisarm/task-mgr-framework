//! Tests for database schema creation.

use super::*;
use crate::db::open_connection;
use tempfile::TempDir;

#[test]
fn test_create_schema_succeeds() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();

    let result = create_schema(&conn);
    assert!(result.is_ok());
}

#[test]
fn test_create_schema_is_idempotent() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();

    // Call create_schema twice - should succeed both times
    create_schema(&conn).unwrap();
    let result = create_schema(&conn);
    assert!(result.is_ok());
}

#[test]
fn test_tasks_table_structure() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Insert a minimal task
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();

    // Verify defaults are applied
    let (status, priority, error_count): (String, i32, i32) = conn
        .query_row(
            "SELECT status, priority, error_count FROM tasks WHERE id = 'US-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(status, "todo");
    assert_eq!(priority, 50);
    assert_eq!(error_count, 0);
}

#[test]
fn test_tasks_status_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Valid statuses should work
    let valid_statuses = [
        "todo",
        "in_progress",
        "done",
        "blocked",
        "skipped",
        "irrelevant",
    ];
    for (i, status) in valid_statuses.iter().enumerate() {
        let id = format!("VALID-{}", i);
        let result = conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES (?, 'Test', ?)",
            [&id, *status],
        );
        assert!(result.is_ok(), "Status '{}' should be valid", status);
    }

    // Invalid status should fail
    let result = conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('INVALID-001', 'Test', 'invalid_status')",
        [],
    );
    assert!(result.is_err(), "Invalid status should be rejected");
}

#[test]
fn test_task_files_foreign_key() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create a task first
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();

    // Insert a file reference
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('US-001', 'src/main.rs')",
        [],
    )
    .unwrap();

    // Trying to insert for non-existent task should fail (foreign key)
    let result = conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('NONEXISTENT', 'src/lib.rs')",
        [],
    );
    assert!(result.is_err(), "Foreign key constraint should be enforced");
}

#[test]
fn test_task_files_unique_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();

    // First insert should succeed
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('US-001', 'src/main.rs')",
        [],
    )
    .unwrap();

    // Duplicate should fail
    let result = conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('US-001', 'src/main.rs')",
        [],
    );
    assert!(result.is_err(), "Duplicate file path should be rejected");
}

#[test]
fn test_task_relationships_structure() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create tasks
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Task 1'), ('US-002', 'Task 2')",
        [],
    )
    .unwrap();

    // Insert relationships
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-002', 'US-001', 'dependsOn')",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-001', 'US-002', 'synergyWith')",
        [],
    )
    .unwrap();

    // Verify count
    let count: i32 = conn
        .query_row("SELECT COUNT(*) FROM task_relationships", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn test_task_relationships_type_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Task 1'), ('US-002', 'Task 2')",
        [],
    )
    .unwrap();

    // Valid relationship types should work
    let valid_types = ["dependsOn", "synergyWith", "batchWith", "conflictsWith"];
    for (i, rel_type) in valid_types.iter().enumerate() {
        let task_id = format!("US-00{}", i + 3);
        conn.execute(
            &format!(
                "INSERT INTO tasks (id, title) VALUES ('{}', 'Task')",
                task_id
            ),
            [],
        )
        .unwrap();
        let result = conn.execute(
            "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES (?, 'US-001', ?)",
            [&task_id, *rel_type],
        );
        assert!(
            result.is_ok(),
            "Relationship type '{}' should be valid",
            rel_type
        );
    }

    // Invalid type should fail
    let result = conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-001', 'US-002', 'invalidType')",
        [],
    );
    assert!(result.is_err(), "Invalid rel_type should be rejected");
}

#[test]
fn test_cascade_delete_task_files() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create task with files
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES ('US-001', 'src/main.rs')",
        [],
    )
    .unwrap();

    // Delete task
    conn.execute("DELETE FROM tasks WHERE id = 'US-001'", [])
        .unwrap();

    // Files should be deleted via cascade
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_files WHERE task_id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_cascade_delete_task_relationships() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create tasks with relationship
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Task 1'), ('US-002', 'Task 2')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO task_relationships (task_id, related_id, rel_type) VALUES ('US-002', 'US-001', 'dependsOn')",
        [],
    )
    .unwrap();

    // Delete source task
    conn.execute("DELETE FROM tasks WHERE id = 'US-002'", [])
        .unwrap();

    // Relationship should be deleted via cascade
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'US-002'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_indexes_exist() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Query sqlite_master to verify indexes exist
    let status_index_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_tasks_status'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    let priority_index_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_tasks_priority'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    assert!(status_index_exists, "idx_tasks_status should exist");
    assert!(priority_index_exists, "idx_tasks_priority should exist");
}

#[test]
fn test_runs_table_structure() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Insert a minimal run
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();

    // Verify defaults are applied
    let (status, iteration_count): (String, i32) = conn
        .query_row(
            "SELECT status, iteration_count FROM runs WHERE run_id = 'run-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(status, "active");
    assert_eq!(iteration_count, 0);
}

#[test]
fn test_runs_status_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Valid statuses should work
    let valid_statuses = ["active", "completed", "aborted"];
    for (i, status) in valid_statuses.iter().enumerate() {
        let run_id = format!("run-{}", i + 1);
        let result = conn.execute(
            "INSERT INTO runs (run_id, status) VALUES (?, ?)",
            [&run_id, *status],
        );
        assert!(result.is_ok(), "Status '{}' should be valid", status);
    }

    // Invalid status should fail
    let result = conn.execute(
        "INSERT INTO runs (run_id, status) VALUES ('run-invalid', 'invalid_status')",
        [],
    );
    assert!(result.is_err(), "Invalid status should be rejected");
}

#[test]
fn test_run_tasks_table_structure() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create prerequisite task and run
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();

    // Insert a run_task
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('run-001', 'US-001', 1)",
        [],
    )
    .unwrap();

    // Verify defaults
    let status: String = conn
        .query_row(
            "SELECT status FROM run_tasks WHERE run_id = 'run-001' AND task_id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(status, "started");
}

#[test]
fn test_run_tasks_status_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create prerequisite data
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();

    // Valid statuses should work
    let valid_statuses = ["started", "completed", "failed", "skipped"];
    for (i, status) in valid_statuses.iter().enumerate() {
        let result = conn.execute(
            "INSERT INTO run_tasks (run_id, task_id, iteration, status) VALUES ('run-001', 'US-001', ?, ?)",
            rusqlite::params![i as i32 + 1, *status],
        );
        assert!(result.is_ok(), "Status '{}' should be valid", status);
    }

    // Invalid status should fail
    let result = conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration, status) VALUES ('run-001', 'US-001', 99, 'invalid')",
        [],
    );
    assert!(result.is_err(), "Invalid status should be rejected");
}

#[test]
fn test_run_tasks_foreign_key_run() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create task but no run
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();

    // Trying to insert for non-existent run should fail
    let result = conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('nonexistent-run', 'US-001', 1)",
        [],
    );
    assert!(
        result.is_err(),
        "Foreign key constraint on run_id should be enforced"
    );
}

#[test]
fn test_run_tasks_foreign_key_task() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create run but no task
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();

    // Trying to insert for non-existent task should fail
    let result = conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('run-001', 'nonexistent-task', 1)",
        [],
    );
    assert!(
        result.is_err(),
        "Foreign key constraint on task_id should be enforced"
    );
}

#[test]
fn test_run_tasks_unique_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create prerequisite data
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();

    // First insert should succeed
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('run-001', 'US-001', 1)",
        [],
    )
    .unwrap();

    // Duplicate (same run_id, task_id, iteration) should fail
    let result = conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('run-001', 'US-001', 1)",
        [],
    );
    assert!(
        result.is_err(),
        "Duplicate run_task entry should be rejected"
    );

    // Same task in same run but different iteration should succeed
    let result = conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('run-001', 'US-001', 2)",
        [],
    );
    assert!(result.is_ok(), "Different iteration should be allowed");
}

#[test]
fn test_cascade_delete_run() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create run with run_tasks
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('run-001', 'US-001', 1)",
        [],
    )
    .unwrap();

    // Delete run
    conn.execute("DELETE FROM runs WHERE run_id = 'run-001'", [])
        .unwrap();

    // run_tasks should be deleted via cascade
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM run_tasks WHERE run_id = 'run-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_cascade_delete_task_from_run_tasks() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create run with run_tasks
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration) VALUES ('run-001', 'US-001', 1)",
        [],
    )
    .unwrap();

    // Delete task
    conn.execute("DELETE FROM tasks WHERE id = 'US-001'", [])
        .unwrap();

    // run_tasks should be deleted via cascade
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM run_tasks WHERE task_id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_runs_indexes_exist() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Query sqlite_master to verify indexes exist
    let runs_status_index_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_runs_status'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    let run_tasks_run_id_index_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_run_tasks_run_id'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    assert!(runs_status_index_exists, "idx_runs_status should exist");
    assert!(
        run_tasks_run_id_index_exists,
        "idx_run_tasks_run_id should exist"
    );
}

// ============ Learnings table tests ============

#[test]
fn test_learnings_table_structure() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Insert a minimal learning
    conn.execute(
        "INSERT INTO learnings (outcome, title, content) VALUES ('success', 'Test Learning', 'Some content')",
        [],
    )
    .unwrap();

    // Verify defaults are applied
    let (confidence, times_shown, times_applied): (String, i32, i32) = conn
        .query_row(
            "SELECT confidence, times_shown, times_applied FROM learnings WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(confidence, "medium");
    assert_eq!(times_shown, 0);
    assert_eq!(times_applied, 0);
}

#[test]
fn test_learnings_outcome_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Valid outcomes should work
    let valid_outcomes = ["failure", "success", "workaround", "pattern"];
    for outcome in valid_outcomes.iter() {
        let result = conn.execute(
            "INSERT INTO learnings (outcome, title, content) VALUES (?, 'Test', 'Content')",
            [*outcome],
        );
        assert!(result.is_ok(), "Outcome '{}' should be valid", outcome);
    }

    // Invalid outcome should fail
    let result = conn.execute(
        "INSERT INTO learnings (outcome, title, content) VALUES ('invalid_outcome', 'Test', 'Content')",
        [],
    );
    assert!(result.is_err(), "Invalid outcome should be rejected");
}

#[test]
fn test_learnings_confidence_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Valid confidence levels should work
    let valid_confidences = ["high", "medium", "low"];
    for confidence in valid_confidences.iter() {
        let result = conn.execute(
            "INSERT INTO learnings (outcome, title, content, confidence) VALUES ('success', 'Test', 'Content', ?)",
            [*confidence],
        );
        assert!(
            result.is_ok(),
            "Confidence '{}' should be valid",
            confidence
        );
    }

    // Invalid confidence should fail
    let result = conn.execute(
        "INSERT INTO learnings (outcome, title, content, confidence) VALUES ('success', 'Test', 'Content', 'invalid')",
        [],
    );
    assert!(result.is_err(), "Invalid confidence should be rejected");
}

#[test]
fn test_learnings_foreign_key_task() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create a task
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();

    // Learning with valid task_id should work
    conn.execute(
        "INSERT INTO learnings (task_id, outcome, title, content) VALUES ('US-001', 'success', 'Test', 'Content')",
        [],
    )
    .unwrap();

    // Learning with invalid task_id should fail (foreign key)
    let result = conn.execute(
        "INSERT INTO learnings (task_id, outcome, title, content) VALUES ('NONEXISTENT', 'success', 'Test', 'Content')",
        [],
    );
    assert!(
        result.is_err(),
        "Foreign key constraint on task_id should be enforced"
    );
}

#[test]
fn test_learnings_foreign_key_run() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create a run
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();

    // Learning with valid run_id should work
    conn.execute(
        "INSERT INTO learnings (run_id, outcome, title, content) VALUES ('run-001', 'success', 'Test', 'Content')",
        [],
    )
    .unwrap();

    // Learning with invalid run_id should fail (foreign key)
    let result = conn.execute(
        "INSERT INTO learnings (run_id, outcome, title, content) VALUES ('nonexistent-run', 'success', 'Test', 'Content')",
        [],
    );
    assert!(
        result.is_err(),
        "Foreign key constraint on run_id should be enforced"
    );
}

#[test]
fn test_learnings_set_null_on_task_delete() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create task and learning
    conn.execute(
        "INSERT INTO tasks (id, title) VALUES ('US-001', 'Test Task')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO learnings (task_id, outcome, title, content) VALUES ('US-001', 'success', 'Test', 'Content')",
        [],
    )
    .unwrap();

    // Delete the task
    conn.execute("DELETE FROM tasks WHERE id = 'US-001'", [])
        .unwrap();

    // Learning should still exist but task_id should be NULL
    let task_id: Option<String> = conn
        .query_row("SELECT task_id FROM learnings WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        task_id.is_none(),
        "task_id should be NULL after task deletion"
    );
}

#[test]
fn test_learnings_set_null_on_run_delete() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create run and learning
    conn.execute("INSERT INTO runs (run_id) VALUES ('run-001')", [])
        .unwrap();
    conn.execute(
        "INSERT INTO learnings (run_id, outcome, title, content) VALUES ('run-001', 'success', 'Test', 'Content')",
        [],
    )
    .unwrap();

    // Delete the run
    conn.execute("DELETE FROM runs WHERE run_id = 'run-001'", [])
        .unwrap();

    // Learning should still exist but run_id should be NULL
    let run_id: Option<String> = conn
        .query_row("SELECT run_id FROM learnings WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(run_id.is_none(), "run_id should be NULL after run deletion");
}

#[test]
fn test_learnings_optional_fields() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Insert learning with all optional fields
    conn.execute(
        r#"INSERT INTO learnings (
            outcome, title, content, root_cause, solution,
            applies_to_files, applies_to_task_types, applies_to_errors
        ) VALUES (
            'failure', 'Test Failure', 'Something went wrong',
            'Bad configuration', 'Fix the config',
            '["src/main.rs", "src/lib.rs"]',
            '["US-", "FIX-"]',
            '["connection refused", "timeout"]'
        )"#,
        [],
    )
    .unwrap();

    // Verify all fields are stored
    let (root_cause, solution): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT root_cause, solution FROM learnings WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(root_cause, Some("Bad configuration".to_string()));
    assert_eq!(solution, Some("Fix the config".to_string()));
}

// ============ Learning tags table tests ============

#[test]
fn test_learning_tags_table_structure() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create a learning first
    conn.execute(
        "INSERT INTO learnings (outcome, title, content) VALUES ('success', 'Test', 'Content')",
        [],
    )
    .unwrap();

    // Add tags
    conn.execute(
        "INSERT INTO learning_tags (learning_id, tag) VALUES (1, 'rust')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO learning_tags (learning_id, tag) VALUES (1, 'database')",
        [],
    )
    .unwrap();

    // Verify count
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM learning_tags WHERE learning_id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn test_learning_tags_unique_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create a learning
    conn.execute(
        "INSERT INTO learnings (outcome, title, content) VALUES ('success', 'Test', 'Content')",
        [],
    )
    .unwrap();

    // First tag insert should succeed
    conn.execute(
        "INSERT INTO learning_tags (learning_id, tag) VALUES (1, 'rust')",
        [],
    )
    .unwrap();

    // Duplicate tag should fail
    let result = conn.execute(
        "INSERT INTO learning_tags (learning_id, tag) VALUES (1, 'rust')",
        [],
    );
    assert!(result.is_err(), "Duplicate tag should be rejected");

    // Same tag on different learning should succeed
    conn.execute(
        "INSERT INTO learnings (outcome, title, content) VALUES ('pattern', 'Test 2', 'Content 2')",
        [],
    )
    .unwrap();
    let result = conn.execute(
        "INSERT INTO learning_tags (learning_id, tag) VALUES (2, 'rust')",
        [],
    );
    assert!(
        result.is_ok(),
        "Same tag on different learning should be allowed"
    );
}

#[test]
fn test_learning_tags_foreign_key() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Trying to insert tag for non-existent learning should fail
    let result = conn.execute(
        "INSERT INTO learning_tags (learning_id, tag) VALUES (999, 'rust')",
        [],
    );
    assert!(result.is_err(), "Foreign key constraint should be enforced");
}

#[test]
fn test_learning_tags_cascade_delete() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Create learning with tags
    conn.execute(
        "INSERT INTO learnings (outcome, title, content) VALUES ('success', 'Test', 'Content')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO learning_tags (learning_id, tag) VALUES (1, 'rust')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO learning_tags (learning_id, tag) VALUES (1, 'database')",
        [],
    )
    .unwrap();

    // Delete learning
    conn.execute("DELETE FROM learnings WHERE id = 1", [])
        .unwrap();

    // Tags should be deleted via cascade
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM learning_tags WHERE learning_id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_learnings_indexes_exist() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Query sqlite_master to verify indexes exist
    let indexes = [
        "idx_learnings_outcome",
        "idx_learnings_task_id",
        "idx_learnings_created_at",
        "idx_learning_tags_learning_id",
        "idx_learning_tags_tag",
    ];

    for index_name in indexes.iter() {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?",
                [*index_name],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(exists, "Index '{}' should exist", index_name);
    }
}

// ============ prd_metadata table tests ============

#[test]
fn test_prd_metadata_table_structure() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Insert prd metadata
    conn.execute(
        r#"INSERT INTO prd_metadata (id, project, branch_name, description)
           VALUES (1, 'task-mgr', 'main', 'Test project')"#,
        [],
    )
    .unwrap();

    // Verify stored values
    let (project, branch_name, description): (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT project, branch_name, description FROM prd_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(project, "task-mgr");
    assert_eq!(branch_name, Some("main".to_string()));
    assert_eq!(description, Some("Test project".to_string()));
}

#[test]
fn test_prd_metadata_single_row_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // First insert should succeed
    conn.execute(
        "INSERT INTO prd_metadata (id, project) VALUES (1, 'task-mgr')",
        [],
    )
    .unwrap();

    // Second row with id=2 should fail (CHECK constraint)
    let result = conn.execute(
        "INSERT INTO prd_metadata (id, project) VALUES (2, 'other-project')",
        [],
    );
    assert!(result.is_err(), "Only one row (id=1) should be allowed");
}

#[test]
fn test_prd_metadata_stores_json_fields() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Insert with JSON fields
    let priority_philosophy = r#"{"description": "Test hierarchy"}"#;
    let global_acceptance = r#"{"criteria": ["No warnings"]}"#;
    let review_guidelines = r#"{"critical": "1-10"}"#;
    let raw_json = r#"{"project": "test"}"#;

    conn.execute(
        r#"INSERT INTO prd_metadata (id, project, priority_philosophy, global_acceptance_criteria, review_guidelines, raw_json)
           VALUES (1, 'task-mgr', ?, ?, ?, ?)"#,
        [priority_philosophy, global_acceptance, review_guidelines, raw_json],
    )
    .unwrap();

    // Verify JSON fields are stored correctly
    let stored_raw: String = conn
        .query_row(
            "SELECT raw_json FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_raw, raw_json);
}

// ============ global_state table tests ============

#[test]
fn test_global_state_initialized() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // global_state should be initialized with default values
    let (id, iteration_counter): (i32, i32) = conn
        .query_row(
            "SELECT id, iteration_counter FROM global_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(id, 1);
    assert_eq!(iteration_counter, 0);
}

#[test]
fn test_global_state_single_row_constraint() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Trying to insert second row should fail (CHECK constraint on id=1)
    let result = conn.execute(
        "INSERT INTO global_state (id, iteration_counter) VALUES (2, 0)",
        [],
    );
    assert!(result.is_err(), "Only one row (id=1) should be allowed");
}

#[test]
fn test_global_state_update_iteration() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Increment iteration counter
    conn.execute(
        "UPDATE global_state SET iteration_counter = iteration_counter + 1 WHERE id = 1",
        [],
    )
    .unwrap();

    let counter: i32 = conn
        .query_row(
            "SELECT iteration_counter FROM global_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(counter, 1);
}

#[test]
fn test_global_state_tracks_last_ids() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Update last_task_id and last_run_id
    conn.execute(
        "UPDATE global_state SET last_task_id = 'US-001', last_run_id = 'run-abc' WHERE id = 1",
        [],
    )
    .unwrap();

    let (last_task, last_run): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT last_task_id, last_run_id FROM global_state WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(last_task, Some("US-001".to_string()));
    assert_eq!(last_run, Some("run-abc".to_string()));
}

#[test]
fn test_global_state_insert_or_ignore_idempotent() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    // Update the counter
    conn.execute(
        "UPDATE global_state SET iteration_counter = 5 WHERE id = 1",
        [],
    )
    .unwrap();

    // Call create_schema again (should not reset counter due to INSERT OR IGNORE)
    create_schema(&conn).unwrap();

    let counter: i32 = conn
        .query_row(
            "SELECT iteration_counter FROM global_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        counter, 5,
        "Counter should not be reset by re-running create_schema"
    );
}

// ============ Remaining indexes tests ============

#[test]
fn test_task_files_indexes_exist() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    let indexes = ["idx_task_files_task_id", "idx_task_files_file_path"];

    for index_name in indexes.iter() {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?",
                [*index_name],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(exists, "Index '{}' should exist", index_name);
    }
}

#[test]
fn test_task_relationships_indexes_exist() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    let indexes = [
        "idx_task_relationships_task_id",
        "idx_task_relationships_related_id",
        "idx_task_relationships_rel_type",
    ];

    for index_name in indexes.iter() {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?",
                [*index_name],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(exists, "Index '{}' should exist", index_name);
    }
}

#[test]
fn test_run_tasks_task_id_index_exists() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_run_tasks_task_id'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(exists, "idx_run_tasks_task_id should exist");
}

#[test]
fn test_learnings_run_id_index_exists() {
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();

    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_learnings_run_id'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(exists, "idx_learnings_run_id should exist");
}

#[test]
fn test_schema_creation_in_fresh_database() {
    // This test verifies the complete schema can be created from scratch
    let temp_dir = TempDir::new().unwrap();
    let conn = open_connection(temp_dir.path()).unwrap();

    // Create schema from scratch
    let result = create_schema(&conn);
    assert!(result.is_ok(), "Schema creation should succeed");

    // Verify all tables exist
    let tables = [
        "tasks",
        "task_files",
        "task_relationships",
        "runs",
        "run_tasks",
        "learnings",
        "learning_tags",
        "prd_metadata",
        "global_state",
    ];

    for table_name in tables.iter() {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
                [*table_name],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(exists, "Table '{}' should exist", table_name);
    }

    // Verify all indexes exist
    let all_indexes = [
        "idx_tasks_status",
        "idx_tasks_priority",
        "idx_tasks_status_priority",
        "idx_runs_status",
        "idx_run_tasks_run_id",
        "idx_run_tasks_task_id",
        "idx_learnings_outcome",
        "idx_learnings_task_id",
        "idx_learnings_created_at",
        "idx_learnings_run_id",
        "idx_learning_tags_learning_id",
        "idx_learning_tags_tag",
        "idx_task_files_task_id",
        "idx_task_files_file_path",
        "idx_task_relationships_task_id",
        "idx_task_relationships_related_id",
        "idx_task_relationships_rel_type",
        "idx_task_relationships_type_taskid",
    ];

    for index_name in all_indexes.iter() {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='index' AND name=?",
                [*index_name],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(exists, "Index '{}' should exist", index_name);
    }

    // Verify global_state is initialized
    let count: i32 = conn
        .query_row("SELECT COUNT(*) FROM global_state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "global_state should have exactly one row");
}
