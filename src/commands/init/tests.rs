//! Tests for the init command.

use super::*;
use crate::db::open_connection;
use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
use std::fs;
use tempfile::TempDir;

/// Create a minimal valid PRD JSON for testing.
fn create_test_prd() -> String {
    r#"{
        "project": "test-project",
        "branchName": "main",
        "description": "Test project description",
        "userStories": [
            {
                "id": "US-001",
                "title": "First Task",
                "description": "Description of first task",
                "priority": 1,
                "passes": false,
                "notes": "Some notes",
                "acceptanceCriteria": ["Criterion 1", "Criterion 2"],
                "touchesFiles": ["src/main.rs", "src/lib.rs"],
                "dependsOn": [],
                "synergyWith": ["US-002"],
                "batchWith": [],
                "conflictsWith": []
            },
            {
                "id": "US-002",
                "title": "Second Task",
                "description": "Description of second task",
                "priority": 2,
                "passes": true,
                "acceptanceCriteria": ["Criterion A"],
                "touchesFiles": ["src/lib.rs"],
                "dependsOn": ["US-001"],
                "synergyWith": [],
                "batchWith": [],
                "conflictsWith": []
            }
        ]
    }"#
    .to_string()
}

// All existing tests use PrefixMode::Disabled to preserve original behavior

#[test]
fn test_init_fresh_database() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    let result = init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert!(result.fresh_import);
    assert_eq!(result.tasks_imported, 2);
    assert_eq!(result.tasks_updated, 0);
    assert_eq!(result.tasks_skipped, 0);
    assert_eq!(result.files_imported, 3); // main.rs, lib.rs x2
    assert_eq!(result.relationships_imported, 1); // 1 dependency (synergyWith ignored)
    assert!(result.prefix_applied.is_none());
}

#[test]
fn test_init_with_force() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    // First import
    init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // Second import with force should replace
    let result = init(
        temp_dir.path(),
        &[&json_path],
        true,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert!(result.fresh_import);
    assert_eq!(result.tasks_imported, 2);
}

#[test]
fn test_init_without_force_fails_on_duplicate() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    // First import
    init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // Second import without force should fail (duplicate tasks)
    let result = init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
}

#[test]
fn test_init_append_mode() {
    let temp_dir = TempDir::new().unwrap();

    // First file
    let json1 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path1 = temp_dir.path().join("p1.json");
    fs::write(&path1, json1).unwrap();

    // Second file with new task
    let json2 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-002", "title": "Task 2", "priority": 2, "passes": false}
        ]
    }"#;
    let path2 = temp_dir.path().join("p2.json");
    fs::write(&path2, json2).unwrap();

    // Import first file
    init(
        temp_dir.path(),
        &[&path1],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // Append second file
    let result = init(
        temp_dir.path(),
        &[&path2],
        false,
        true,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert!(!result.fresh_import);
    assert_eq!(result.tasks_imported, 1);
}

#[test]
fn test_init_append_skips_existing() {
    let temp_dir = TempDir::new().unwrap();

    // Initial PRD
    let json1 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path1 = temp_dir.path().join("p1.json");
    fs::write(&path1, json1).unwrap();

    // Second PRD with same task ID
    let json2 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1 Updated", "priority": 1, "passes": false}
        ]
    }"#;
    let path2 = temp_dir.path().join("p2.json");
    fs::write(&path2, json2).unwrap();

    // Import first
    init(
        temp_dir.path(),
        &[&path1],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // Append should skip existing
    let result = init(
        temp_dir.path(),
        &[&path2],
        false,
        true,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert_eq!(result.tasks_imported, 0);
    assert_eq!(result.tasks_updated, 0);
    assert_eq!(result.tasks_skipped, 1);
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("US-001"));
}

#[test]
fn test_init_multiple_files() {
    let temp_dir = TempDir::new().unwrap();

    let json1 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let json2 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-002", "title": "Task 2", "priority": 2, "passes": false}
        ]
    }"#;

    let path1 = temp_dir.path().join("p1.json");
    let path2 = temp_dir.path().join("p2.json");
    fs::write(&path1, json1).unwrap();
    fs::write(&path2, json2).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path1, &path2],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert_eq!(result.tasks_imported, 2);
}

#[test]
fn test_init_duplicate_across_files_fails() {
    let temp_dir = TempDir::new().unwrap();

    let json1 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let json2 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1 Duplicate", "priority": 1, "passes": false}
        ]
    }"#;

    let path1 = temp_dir.path().join("p1.json");
    let path2 = temp_dir.path().join("p2.json");
    fs::write(&path1, json1).unwrap();
    fs::write(&path2, json2).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path1, &path2],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
}

#[test]
fn test_init_passes_maps_to_done() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Passing", "priority": 1, "passes": true},
            {"id": "US-002", "title": "Not Passing", "priority": 2, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // Verify status mapping
    let conn = open_connection(temp_dir.path()).unwrap();
    let status1: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-001'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let status2: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'US-002'", [], |row| {
            row.get(0)
        })
        .unwrap();

    assert_eq!(status1, "done");
    assert_eq!(status2, "todo");
}

#[test]
fn test_init_stores_prd_metadata() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "my-project",
        "branchName": "feature/test",
        "description": "My project description",
        "priorityPhilosophy": {"key": "value"},
        "userStories": [
            {"id": "US-001", "title": "Task", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let (project, branch): (String, Option<String>) = conn
        .query_row(
            "SELECT project, branch_name FROM prd_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(project, "my-project");
    assert_eq!(branch, Some("feature/test".to_string()));
}

#[test]
fn test_init_stores_acceptance_criteria() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task",
                "priority": 1,
                "passes": false,
                "acceptanceCriteria": ["First", "Second", "Third"]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let criteria_json: String = conn
        .query_row(
            "SELECT acceptance_criteria FROM tasks WHERE id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let criteria: Vec<String> = serde_json::from_str(&criteria_json).unwrap();
    assert_eq!(criteria, vec!["First", "Second", "Third"]);
}

#[test]
fn test_init_file_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("nonexistent.json");

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
}

#[test]
fn test_init_invalid_json() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("invalid.json");
    fs::write(&path, "not valid json").unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
}

#[test]
fn test_init_stores_relationships() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-000",
                "title": "Task 0",
                "priority": 0,
                "passes": false
            },
            {
                "id": "US-001",
                "title": "Task 1",
                "priority": 1,
                "passes": false,
                "dependsOn": ["US-000"],
                "synergyWith": ["US-002"],
                "batchWith": ["US-003"],
                "conflictsWith": ["US-004"]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();
    // Only dependsOn is imported; synergyWith/batchWith/conflictsWith are deprecated and ignored.
    assert_eq!(result.relationships_imported, 1);

    let conn = open_connection(temp_dir.path()).unwrap();
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn test_init_append_update_existing() {
    let temp_dir = TempDir::new().unwrap();

    // Initial PRD
    let json1 = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Original Title",
                "priority": 1,
                "passes": false,
                "touchesFiles": ["old.rs"]
            }
        ]
    }"#;
    let path1 = temp_dir.path().join("p1.json");
    fs::write(&path1, json1).unwrap();

    // Second PRD with updated task
    let json2 = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Updated Title",
                "priority": 2,
                "passes": false,
                "touchesFiles": ["new.rs"]
            }
        ]
    }"#;
    let path2 = temp_dir.path().join("p2.json");
    fs::write(&path2, json2).unwrap();

    // Import first
    init(
        temp_dir.path(),
        &[&path1],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // Append with update-existing
    let result = init(
        temp_dir.path(),
        &[&path2],
        false,
        true,
        true,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert_eq!(result.tasks_imported, 0);
    assert_eq!(result.tasks_updated, 1);
    assert_eq!(result.tasks_skipped, 0);

    // Verify task was updated
    let conn = open_connection(temp_dir.path()).unwrap();
    let (title, priority): (String, i32) = conn
        .query_row(
            "SELECT title, priority FROM tasks WHERE id = 'US-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(title, "Updated Title");
    assert_eq!(priority, 2);

    // Verify files were replaced
    let file: String = conn
        .query_row(
            "SELECT file_path FROM task_files WHERE task_id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(file, "new.rs");
}

#[test]
fn test_init_dependency_validation_fails() {
    let temp_dir = TempDir::new().unwrap();

    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task 1",
                "priority": 1,
                "passes": false,
                "dependsOn": ["US-NONEXISTENT"]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("US-NONEXISTENT"));
}

#[test]
fn test_init_cross_file_dependency_resolves() {
    let temp_dir = TempDir::new().unwrap();

    let json1 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let json2 = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-002",
                "title": "Task 2",
                "priority": 2,
                "passes": false,
                "dependsOn": ["US-001"]
            }
        ]
    }"#;

    let path1 = temp_dir.path().join("p1.json");
    let path2 = temp_dir.path().join("p2.json");
    fs::write(&path1, json1).unwrap();
    fs::write(&path2, json2).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path1, &path2],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().relationships_imported, 1);
}

#[test]
fn test_init_append_with_existing_dependency() {
    let temp_dir = TempDir::new().unwrap();

    let json1 = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path1 = temp_dir.path().join("p1.json");
    fs::write(&path1, json1).unwrap();
    init(
        temp_dir.path(),
        &[&path1],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let json2 = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-002",
                "title": "Task 2",
                "priority": 2,
                "passes": false,
                "dependsOn": ["US-001"]
            }
        ]
    }"#;
    let path2 = temp_dir.path().join("p2.json");
    fs::write(&path2, json2).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path2],
        false,
        true,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_ok());
    let result = result.unwrap();
    assert_eq!(result.tasks_imported, 1);
    assert_eq!(result.relationships_imported, 1);
}

#[test]
fn test_init_verifies_both_phases_present() {
    let temp_dir = TempDir::new().unwrap();

    let json1 = r#"{
        "project": "test",
        "userStories": [
            {"id": "P1-001", "title": "Phase 1 Task", "priority": 1, "passes": false}
        ]
    }"#;
    let json2 = r#"{
        "project": "test",
        "userStories": [
            {"id": "P2-001", "title": "Phase 2 Task", "priority": 10, "passes": false}
        ]
    }"#;

    let path1 = temp_dir.path().join("p1.json");
    let path2 = temp_dir.path().join("p2.json");
    fs::write(&path1, json1).unwrap();
    fs::write(&path2, json2).unwrap();

    init(
        temp_dir.path(),
        &[&path1],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();
    init(
        temp_dir.path(),
        &[&path2],
        false,
        true,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let count: i32 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);

    let ids: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM tasks ORDER BY id").unwrap();
        let id_iter = stmt.query_map([], |row| row.get(0)).unwrap();
        id_iter.map(|r| r.unwrap()).collect()
    };
    assert_eq!(ids, vec!["P1-001", "P2-001"]);
}

#[test]
fn test_init_dry_run_does_not_modify_database() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    let result = init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        true,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert!(result.dry_run);
    assert_eq!(result.tasks_imported, 2);
    assert_eq!(result.files_imported, 3);
    assert_eq!(result.relationships_imported, 1); // synergyWith ignored
    assert!(result.would_delete.is_none());

    let conn = open_connection(temp_dir.path()).unwrap();
    let count: i32 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_init_dry_run_with_force_shows_delete_preview() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let task_count: i32 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(task_count, 2);

    let result = init(
        temp_dir.path(),
        &[&json_path],
        true,
        false,
        false,
        true,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert!(result.dry_run);
    assert!(result.would_delete.is_some());
    let preview = result.would_delete.unwrap();
    assert_eq!(preview.tasks, 2);
    assert_eq!(preview.files, 3);
    assert_eq!(preview.relationships, 1); // synergyWith ignored

    let count_after: i32 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count_after, 2);
}

#[test]
fn test_init_dry_run_validates_dependencies() {
    let temp_dir = TempDir::new().unwrap();

    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task 1",
                "priority": 1,
                "passes": false,
                "dependsOn": ["US-NONEXISTENT"]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        true,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("US-NONEXISTENT"));
}

// SECURITY: Path traversal tests

#[test]
fn test_init_rejects_absolute_paths_in_touches_files() {
    let temp_dir = TempDir::new().unwrap();

    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task with absolute path",
                "priority": 1,
                "passes": false,
                "touchesFiles": ["/etc/passwd"]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Unsafe path"));
    assert!(err.contains("/etc/passwd"));
    assert!(err.contains("absolute paths"));
    assert!(err.contains("US-001"));
}

#[test]
fn test_init_rejects_path_traversal_in_touches_files() {
    let temp_dir = TempDir::new().unwrap();

    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task with path traversal",
                "priority": 1,
                "passes": false,
                "touchesFiles": ["../../../etc/passwd"]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Unsafe path"));
    assert!(err.contains("parent directory traversal"));
}

#[test]
fn test_init_rejects_home_directory_in_touches_files() {
    let temp_dir = TempDir::new().unwrap();

    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task with home path",
                "priority": 1,
                "passes": false,
                "touchesFiles": ["~/.ssh/id_rsa"]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("home directory paths"));
}

#[test]
fn test_init_allows_valid_relative_paths_in_touches_files() {
    let temp_dir = TempDir::new().unwrap();

    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task with valid paths",
                "priority": 1,
                "passes": false,
                "touchesFiles": [
                    "src/main.rs",
                    "./src/lib.rs",
                    "tests/fixtures/sample.json",
                    ".github/workflows/ci.yml"
                ]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap().files_imported, 4);
}

#[test]
fn test_init_dry_run_still_validates_paths() {
    let temp_dir = TempDir::new().unwrap();

    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task with bad path",
                "priority": 1,
                "passes": false,
                "touchesFiles": ["/etc/shadow"]
            }
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        true,
        PrefixMode::Disabled,
    );
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Unsafe path"));
}

// ============================================================================
// Prefix tests
// ============================================================================

#[test]
fn test_init_explicit_prefix_applied_to_ids() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    let result = init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("P3".to_string()),
    )
    .unwrap();

    assert_eq!(result.tasks_imported, 2);
    assert_eq!(result.prefix_applied, Some("P3".to_string()));

    // Verify IDs in database are prefixed
    let conn = open_connection(temp_dir.path()).unwrap();
    let ids: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM tasks ORDER BY id").unwrap();
        let id_iter = stmt.query_map([], |row| row.get(0)).unwrap();
        id_iter.map(|r| r.unwrap()).collect()
    };
    assert_eq!(ids, vec!["P3-US-001", "P3-US-002"]);
}

#[test]
fn test_init_explicit_prefix_applied_to_relationships() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("P3".to_string()),
    )
    .unwrap();

    // Verify relationships reference prefixed IDs
    let conn = open_connection(temp_dir.path()).unwrap();

    // US-002 depends on US-001 -> P3-US-002 depends on P3-US-001
    let dep: String = conn
        .query_row(
            "SELECT related_id FROM task_relationships WHERE task_id = 'P3-US-002' AND rel_type = 'dependsOn'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dep, "P3-US-001");

    // synergyWith is deprecated and must NOT be stored in the DB
    let syn_count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships WHERE rel_type = 'synergyWith'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(syn_count, 0, "synergyWith rows should not be inserted");
}

#[test]
fn test_init_auto_prefix_ignores_json_field() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "branchName": "feat/test",
        "taskPrefix": "P5",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    // Auto mode always generates from branchName + filename, ignoring JSON taskPrefix
    let expected_prefix = generate_prefix(Some("feat/test"), "prd.json");

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Auto,
    )
    .unwrap();

    assert_eq!(result.prefix_applied, Some(expected_prefix.clone()));

    let conn = open_connection(temp_dir.path()).unwrap();
    let id: String = conn
        .query_row("SELECT id FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(id, format!("{}-US-001", expected_prefix));
}

#[test]
fn test_init_auto_prefix_generates_hash_when_absent() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Auto,
    )
    .unwrap();

    // Should have generated a prefix
    assert!(result.prefix_applied.is_some());
    let prefix = result.prefix_applied.unwrap();
    assert_eq!(prefix.len(), 8); // First 8 chars of UUID

    // Verify the prefix was written back to the JSON file
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains(&format!("\"taskPrefix\": \"{}\"", prefix)));

    // Verify the task ID in DB uses the prefix
    let conn = open_connection(temp_dir.path()).unwrap();
    let id: String = conn
        .query_row("SELECT id FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(id, format!("{}-US-001", prefix));
}

#[test]
fn test_init_auto_prefix_dry_run_does_not_write_json() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        true, // dry_run
        PrefixMode::Auto,
    )
    .unwrap();

    assert!(result.prefix_applied.is_some());
    assert!(result.dry_run);

    // JSON file should NOT have taskPrefix written
    let content = fs::read_to_string(&path).unwrap();
    assert!(!content.contains("taskPrefix"));
}

#[test]
fn test_init_disabled_prefix_no_modification() {
    let temp_dir = TempDir::new().unwrap();
    // Even with taskPrefix in JSON, Disabled mode ignores it
    let json = r#"{
        "project": "test",
        "taskPrefix": "IGNORED",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    assert!(result.prefix_applied.is_none());

    let conn = open_connection(temp_dir.path()).unwrap();
    let id: String = conn
        .query_row("SELECT id FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(id, "US-001"); // No prefix applied
}

#[test]
fn test_init_explicit_prefix_overrides_json_field() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "taskPrefix": "JSON",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let result = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("CLI".to_string()),
    )
    .unwrap();

    assert_eq!(result.prefix_applied, Some("CLI".to_string()));

    let conn = open_connection(temp_dir.path()).unwrap();
    let id: String = conn
        .query_row("SELECT id FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(id, "CLI-US-001"); // CLI prefix wins over JSON
}

#[test]
fn test_init_prefix_stable_across_reimports() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    // First import: auto-generates prefix and writes to JSON
    let result1 = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Auto,
    )
    .unwrap();
    let prefix1 = result1.prefix_applied.unwrap();

    // Force re-import: should read the same prefix from JSON
    let result2 = init(
        temp_dir.path(),
        &[&path],
        true,
        false,
        false,
        false,
        PrefixMode::Auto,
    )
    .unwrap();
    let prefix2 = result2.prefix_applied.unwrap();

    assert_eq!(
        prefix1, prefix2,
        "Prefix should be stable across re-imports"
    );
}

// ============================================================================
// Model selection field tests (parse, import, round-trip)
// ============================================================================

#[test]
fn test_parse_prd_user_story_with_model_difficulty_escalation() {
    let json = format!(
        r#"{{
        "id": "US-001",
        "title": "Task with model",
        "priority": 1,
        "passes": false,
        "model": "{SONNET_MODEL}",
        "difficulty": "high",
        "escalationNote": "Retried after OOM"
    }}"#
    );

    let story: super::parse::PrdUserStory = serde_json::from_str(&json).unwrap();

    assert_eq!(story.model, Some(SONNET_MODEL.to_string()));
    assert_eq!(story.difficulty, Some("high".to_string()));
    assert_eq!(story.escalation_note, Some("Retried after OOM".to_string()));
}

#[test]
fn test_parse_prd_backward_compat_without_model_fields() {
    let json = r#"{
        "id": "US-001",
        "title": "Legacy task",
        "priority": 1,
        "passes": false
    }"#;

    let story: super::parse::PrdUserStory = serde_json::from_str(json).unwrap();

    assert_eq!(story.model, None, "model should default to None");
    assert_eq!(story.difficulty, None, "difficulty should default to None");
    assert_eq!(
        story.escalation_note, None,
        "escalation_note should default to None"
    );
}

/// Known-bad discriminator: escalationNote must use camelCase in JSON.
/// A naive snake_case key ("escalation_note") should NOT deserialize into the field.
#[test]
fn test_parse_escalation_note_requires_camel_case() {
    // This JSON uses snake_case "escalation_note" — should NOT match
    let json = r#"{
        "id": "US-001",
        "title": "Task",
        "priority": 1,
        "passes": false,
        "escalation_note": "This should not parse"
    }"#;

    let story: super::parse::PrdUserStory = serde_json::from_str(json).unwrap();
    assert_eq!(
        story.escalation_note, None,
        "snake_case escalation_note must NOT deserialize — only camelCase escalationNote works"
    );
}

/// Positive test: camelCase escalationNote DOES work.
#[test]
fn test_parse_escalation_note_camel_case_works() {
    let json = r#"{
        "id": "US-001",
        "title": "Task",
        "priority": 1,
        "passes": false,
        "escalationNote": "This should parse"
    }"#;

    let story: super::parse::PrdUserStory = serde_json::from_str(json).unwrap();
    assert_eq!(story.escalation_note, Some("This should parse".to_string()));
}

#[test]
fn test_parse_prd_file_with_model() {
    let json = format!(
        r#"{{
        "project": "test",
        "model": "{HAIKU_MODEL}",
        "userStories": [
            {{"id": "US-001", "title": "Task", "priority": 1, "passes": false}}
        ]
    }}"#
    );

    let prd: super::parse::PrdFile = serde_json::from_str(&json).unwrap();
    assert_eq!(prd.model, Some(HAIKU_MODEL.to_string()));
}

#[test]
fn test_parse_prd_file_backward_compat_without_model() {
    let json = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task", "priority": 1, "passes": false}
        ]
    }"#;

    let prd: super::parse::PrdFile = serde_json::from_str(json).unwrap();
    assert_eq!(prd.model, None, "model should default to None");
}

#[test]
fn test_insert_task_with_model_difficulty_escalation_note() {
    let temp_dir = TempDir::new().unwrap();
    let json = format!(
        r#"{{
        "project": "test",
        "userStories": [
            {{
                "id": "US-001",
                "title": "Model task",
                "priority": 1,
                "passes": false,
                "model": "{OPUS_MODEL}",
                "difficulty": "high",
                "escalationNote": "Bumped from sonnet after failure"
            }}
        ]
    }}"#
    );
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, &json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'US-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(model, Some(OPUS_MODEL.to_string()));
    assert_eq!(difficulty, Some("high".to_string()));
    assert_eq!(
        escalation_note,
        Some("Bumped from sonnet after failure".to_string())
    );
}

#[test]
fn test_insert_task_without_model_fields_stores_null() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Plain task", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'US-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(model, None);
    assert_eq!(difficulty, None);
    assert_eq!(escalation_note, None);
}

#[test]
fn test_insert_prd_metadata_with_model() {
    let temp_dir = TempDir::new().unwrap();
    let json = format!(
        r#"{{
        "project": "model-test",
        "model": "{SONNET_MODEL}",
        "userStories": [
            {{"id": "US-001", "title": "Task", "priority": 1, "passes": false}}
        ]
    }}"#
    );
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, &json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(default_model, Some(SONNET_MODEL.to_string()));
}

#[test]
fn test_insert_prd_metadata_without_default_model() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "no-model",
        "userStories": [
            {"id": "US-001", "title": "Task", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(default_model, None);
}

// --- max_retries import tests ---

/// No maxRetries in JSON → task gets default of 3.
#[test]
fn test_insert_task_max_retries_defaults_to_3() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let max_retries: i64 = conn
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(max_retries, 3, "tasks without maxRetries must default to 3");
}

/// Per-task maxRetries overrides PRD defaultMaxRetries.
#[test]
fn test_insert_task_per_task_max_retries_overrides_prd_default() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "defaultMaxRetries": 5,
        "userStories": [
            {"id": "US-001", "title": "Override", "priority": 1, "passes": false, "maxRetries": 2},
            {"id": "US-002", "title": "Default", "priority": 2, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let mr1: i64 = conn
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mr2: i64 = conn
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = 'US-002'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(mr1, 2, "per-task maxRetries=2 must override PRD default=5");
    assert_eq!(mr2, 5, "task without maxRetries must use PRD default=5");
}

/// PRD defaultMaxRetries stored in prd_metadata.
#[test]
fn test_insert_prd_metadata_stores_default_max_retries() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "defaultMaxRetries": 7,
        "userStories": [
            {"id": "US-001", "title": "Task", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let default_max_retries: Option<i64> = conn
        .query_row(
            "SELECT default_max_retries FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        default_max_retries,
        Some(7),
        "prd_metadata.default_max_retries must store PRD defaultMaxRetries"
    );
}

/// Old JSON without maxRetries/defaultMaxRetries: all tasks default to 3, prd_metadata is NULL.
#[test]
fn test_insert_task_old_json_no_max_retries_fields() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "legacy",
        "userStories": [
            {"id": "US-001", "title": "Old task", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();
    let task_mr: i64 = conn
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let prd_mr: Option<i64> = conn
        .query_row(
            "SELECT default_max_retries FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(task_mr, 3, "legacy task must default to max_retries=3");
    assert_eq!(
        prd_mr, None,
        "legacy PRD must have NULL default_max_retries"
    );
}

// --- Deterministic prefix generation tests ---

#[test]
fn test_generate_prefix_deterministic() {
    let p1 = super::generate_prefix(Some("feat/my-branch"), "prd.json");
    let p2 = super::generate_prefix(Some("feat/my-branch"), "prd.json");
    assert_eq!(p1, p2, "Same inputs must produce same prefix");
    assert_eq!(p1.len(), 8);
    assert!(p1.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_generate_prefix_different_branches_differ() {
    let p1 = super::generate_prefix(Some("feat/branch-a"), "prd.json");
    let p2 = super::generate_prefix(Some("feat/branch-b"), "prd.json");
    assert_ne!(
        p1, p2,
        "Different branches should produce different prefixes"
    );
}

#[test]
fn test_generate_prefix_different_filenames_differ() {
    let p1 = super::generate_prefix(Some("main"), "phase1.json");
    let p2 = super::generate_prefix(Some("main"), "phase2.json");
    assert_ne!(
        p1, p2,
        "Different filenames should produce different prefixes"
    );
}

#[test]
fn test_generate_prefix_none_branch_equals_empty_branch() {
    let p1 = super::generate_prefix(None, "prd.json");
    let p2 = super::generate_prefix(Some(""), "prd.json");
    assert_eq!(p1, p2, "None and empty branch should be equivalent");
    assert_eq!(p1.len(), 8);
}

#[test]
fn test_generate_prefix_known_values() {
    // Pinned: echo -n "feat/test:prd.json" | md5sum | cut -c1-8
    let p = super::generate_prefix(Some("feat/test"), "prd.json");
    assert_eq!(p, "34c5194b");

    // Pinned: echo -n ":prd.json" | md5sum | cut -c1-8
    let p_no_branch = super::generate_prefix(None, "prd.json");
    assert_eq!(p_no_branch, "f8676724");
}

#[test]
fn test_prefix_id_adds_prefix() {
    assert_eq!(super::prefix_id("P1", "FEAT-001"), "P1-FEAT-001");
}

#[test]
fn test_prefix_id_idempotent_when_already_prefixed() {
    // If the ID already starts with the prefix, don't double it
    assert_eq!(
        super::prefix_id("KBTEST", "KBTEST-FEAT-001"),
        "KBTEST-FEAT-001"
    );
    assert_eq!(super::prefix_id("P1", "P1-US-003"), "P1-US-003");
}

#[test]
fn test_prefix_id_does_not_match_partial_prefix() {
    // "P1-" must not match "P10-FEAT-001" — the dash separator prevents it
    assert_eq!(super::prefix_id("P1", "P10-FEAT-001"), "P1-P10-FEAT-001");
}

#[test]
fn test_init_auto_prefix_dry_run_deterministic() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "branchName": "feat/dry",
        "userStories": [
            {"id": "US-001", "title": "Task 1", "priority": 1, "passes": false}
        ]
    }"#;
    let path = temp_dir.path().join("prd.json");
    fs::write(&path, json).unwrap();

    let r1 = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        true,
        PrefixMode::Auto,
    )
    .unwrap();

    // Dry-run doesn't write back, so second call also generates
    let r2 = init(
        temp_dir.path(),
        &[&path],
        false,
        false,
        false,
        true,
        PrefixMode::Auto,
    )
    .unwrap();

    assert_eq!(
        r1.prefix_applied, r2.prefix_applied,
        "Dry-run should produce deterministic prefix"
    );
}

// ============================================================================
// SS-SS-TEST-INIT-004: TDD tests for upsert-by-task_prefix and scoped
// drop_existing_data() — RED PHASE (written before implementation).
//
// All tests below are #[ignore]d pending:
//   1. Migration v9 (removal of CHECK(id=1) singleton from prd_metadata)
//   2. insert_prd_metadata returning TaskMgrResult<i64> and upserting by task_prefix
//   3. insert_prd_file accepting a prd_id: i64 parameter
//   4. drop_existing_data accepting prefix: Option<&str>
// ============================================================================

#[cfg(test)]
mod scoped_import_tests {
    use tempfile::TempDir;

    use crate::commands::init::import::{drop_existing_data, insert_prd_file, insert_prd_metadata};
    use crate::commands::init::parse::PrdFile;
    use crate::db::open_connection;

    /// Create a migrated in-memory database (all migrations including v9).
    fn setup_migrated_db() -> (TempDir, rusqlite::Connection) {
        let temp_dir = TempDir::new().unwrap();
        let mut conn = open_connection(temp_dir.path()).unwrap();
        crate::db::run_migrations(&mut conn).unwrap();
        (temp_dir, conn)
    }

    /// Build a minimal PrdFile for testing.
    fn make_prd(project: &str, task_prefix: Option<&str>) -> PrdFile {
        PrdFile {
            project: project.to_string(),
            branch_name: Some("main".to_string()),
            description: None,
            priority_philosophy: None,
            global_acceptance_criteria: None,
            review_guidelines: None,
            user_stories: vec![],
            external_git_repo: None,
            task_prefix: task_prefix.map(|s| s.to_string()),
            prd_file: None,
            model: None,
            default_max_retries: None,
            implicit_overlap_files: None,
        }
    }

    // -----------------------------------------------------------------------
    // insert_prd_metadata: upsert by task_prefix, returns i64
    // -----------------------------------------------------------------------

    #[test]
    fn test_insert_prd_metadata_new_prefix_returns_id() {
        let (_dir, conn) = setup_migrated_db();
        let prd = make_prd("project-one", Some("P1"));
        let id = insert_prd_metadata(&conn, &prd, None).unwrap();
        assert!(id > 0, "returned id must be positive");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_insert_prd_metadata_upsert_existing_prefix() {
        let (_dir, conn) = setup_migrated_db();
        let prd1 = make_prd("project-original", Some("P1"));
        let prd2 = make_prd("project-updated", Some("P1"));
        insert_prd_metadata(&conn, &prd1, None).unwrap();
        insert_prd_metadata(&conn, &prd2, None).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "upsert must not create a duplicate row");
        let project: String = conn
            .query_row(
                "SELECT project FROM prd_metadata WHERE task_prefix='P1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(project, "project-updated");
    }

    #[test]
    fn test_insert_prd_metadata_two_different_prefixes_creates_two_rows() {
        let (_dir, conn) = setup_migrated_db();
        let prd1 = make_prd("project-one", Some("P1"));
        let prd2 = make_prd("project-two", Some("P2"));
        let id1 = insert_prd_metadata(&conn, &prd1, None).unwrap();
        let id2 = insert_prd_metadata(&conn, &prd2, None).unwrap();
        assert_ne!(id1, id2, "distinct prefixes must yield distinct row ids");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_insert_prd_metadata_upsert_returns_correct_id_for_prd_files() {
        // Regression: last_insert_rowid() returned 0 on ON CONFLICT DO UPDATE,
        // causing register_prd_files to fail with FOREIGN KEY constraint.
        let (_dir, conn) = setup_migrated_db();
        let prd = make_prd("project-v1", Some("P1"));
        let id1 = insert_prd_metadata(&conn, &prd, None).unwrap();
        assert!(id1 > 0, "first insert must return positive id");

        // Upsert same prefix — must return the SAME id, not 0
        let prd_v2 = make_prd("project-v2", Some("P1"));
        let id2 = insert_prd_metadata(&conn, &prd_v2, None).unwrap();
        assert_eq!(id1, id2, "upsert must return the existing row id, not 0");

        // The returned id must be valid for FK-constrained inserts
        insert_prd_file(&conn, id2, ".task-mgr/tasks/test.json", "task_list")
            .expect("insert_prd_file must succeed with upserted prd_id");
    }

    // -----------------------------------------------------------------------
    // insert_prd_file: dynamic prd_id parameter
    // -----------------------------------------------------------------------

    #[test]
    fn test_insert_prd_file_uses_dynamic_prd_id() {
        let (_dir, conn) = setup_migrated_db();
        let prd = make_prd("proj", Some("PX"));
        let prd_id = insert_prd_metadata(&conn, &prd, None).unwrap();
        insert_prd_file(&conn, prd_id, ".task-mgr/tasks/prd.json", "task_list").unwrap();
        let stored_prd_id: i64 = conn
            .query_row(
                "SELECT prd_id FROM prd_files WHERE file_path='.task-mgr/tasks/prd.json'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored_prd_id, prd_id,
            "prd_id must match the value passed in, not hardcoded 1"
        );
    }

    // -----------------------------------------------------------------------
    // drop_existing_data: scoped prefix filtering
    // -----------------------------------------------------------------------

    #[test]
    fn test_drop_existing_data_scoped_deletes_only_prefix_tasks() {
        let (_dir, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, acceptance_criteria) \
             VALUES ('P1-US-001','T1','todo',1,'[]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, status, priority, acceptance_criteria) \
             VALUES ('P2-US-001','T2','todo',1,'[]')",
            [],
        )
        .unwrap();
        drop_existing_data(&conn, Some("P1")).unwrap();
        let p1: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'P1-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let p2: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'P2-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(p1, 0, "P1 tasks must be deleted");
        assert_eq!(p2, 1, "P2 tasks must be preserved");
    }

    /// Known-bad discriminator: after inserting P1 and P2 tasks, a scoped
    /// force-delete of P1 must leave all P2 tasks intact.
    #[test]
    fn test_cross_prd_force_delete_leaves_other_prd_intact() {
        let (_dir, conn) = setup_migrated_db();
        // Insert P1 task with file
        conn.execute(
            "INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
             VALUES ('P1-US-001','P1T','todo',10,'[]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_files (task_id,file_path) VALUES ('P1-US-001','a.rs')",
            [],
        )
        .unwrap();
        // Insert P2 tasks with relationship
        conn.execute(
            "INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
             VALUES ('P2-US-001','P2T1','todo',10,'[]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
             VALUES ('P2-US-002','P2T2','todo',20,'[]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_relationships (task_id,related_id,rel_type) \
             VALUES ('P2-US-002','P2-US-001','dependsOn')",
            [],
        )
        .unwrap();
        drop_existing_data(&conn, Some("P1")).unwrap();
        let p2_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'P2-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let p2_rel: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM task_relationships WHERE task_id LIKE 'P2-%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(p2_count, 2, "both P2 tasks must survive scoped P1 delete");
        assert_eq!(p2_rel, 1, "P2 relationships must survive");
    }

    #[test]
    fn test_drop_existing_data_none_prefix_wipes_everything() {
        let (_dir, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
             VALUES ('P1-US-001','T1','todo',1,'[]')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
             VALUES ('P2-US-001','T2','todo',1,'[]')",
            [],
        )
        .unwrap();
        drop_existing_data(&conn, None).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "None-prefix drop must wipe all tasks");
    }

    #[test]
    fn test_drop_existing_data_scoped_preserves_learnings() {
        let (_dir, conn) = setup_migrated_db();
        conn.execute(
            "INSERT INTO learnings (title, content, outcome, confidence) \
             VALUES ('test learning', 'content', 'success', 'high')",
            [],
        )
        .unwrap();
        drop_existing_data(&conn, Some("P1")).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "learnings must not be deleted by scoped --force");
    }

    #[test]
    fn test_drop_existing_data_scoped_preserves_other_prd_metadata() {
        let (_dir, conn) = setup_migrated_db();
        let prd1 = make_prd("proj-one", Some("P1"));
        let prd2 = make_prd("proj-two", Some("P2"));
        insert_prd_metadata(&conn, &prd1, None).unwrap();
        insert_prd_metadata(&conn, &prd2, None).unwrap();
        drop_existing_data(&conn, Some("P1")).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix='P2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "P2 prd_metadata must survive scoped P1 delete");
    }
}

// ============================================================================
// SS-SS-TEST-003: Full init() flow tests for multi-PRD import, upsert, and
// scoped --force behavior.
// ============================================================================

mod multi_prd_import_tests {
    use crate::commands::init::{PrefixMode, init};
    use crate::db::open_connection;
    use std::fs;
    use tempfile::TempDir;

    fn make_prd_json(task_prefix: &str, task_id: &str) -> String {
        format!(
            r#"{{
                "project": "test-{task_prefix}",
                "taskPrefix": "{task_prefix}",
                "userStories": [
                    {{"id": "{task_id}", "title": "Task {task_id}", "priority": 1, "passes": false}}
                ]
            }}"#
        )
    }

    // AC 1: Import P1 then P2 — both prd_metadata rows must exist, and each
    // prd_files row must link to the correct prd_id for its PRD.
    #[test]
    fn test_import_p1_then_p2_both_metadata_rows_exist() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = temp_dir.path().join("p1.json");
        let path2 = temp_dir.path().join("p2.json");
        fs::write(&path1, make_prd_json("P1", "US-001")).unwrap();
        fs::write(&path2, make_prd_json("P2", "US-001")).unwrap();

        init(
            temp_dir.path(),
            &[&path1],
            false,
            false,
            false,
            false,
            PrefixMode::Explicit("P1".to_string()),
        )
        .unwrap();

        init(
            temp_dir.path(),
            &[&path2],
            false,
            true, // append
            false,
            false,
            PrefixMode::Explicit("P2".to_string()),
        )
        .unwrap();

        let conn = open_connection(temp_dir.path()).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 2,
            "importing two PRDs must create two prd_metadata rows"
        );

        let p1_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM prd_metadata WHERE task_prefix = 'P1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let p2_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM prd_metadata WHERE task_prefix = 'P2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(p1_exists, "prd_metadata row for P1 must exist");
        assert!(p2_exists, "prd_metadata row for P2 must exist");
    }

    // AC 1 (continued): prd_files for each PRD must link to the correct prd_id.
    #[test]
    fn test_import_p1_then_p2_prd_files_correct_associations() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = temp_dir.path().join("p1.json");
        let path2 = temp_dir.path().join("p2.json");
        fs::write(&path1, make_prd_json("P1", "US-001")).unwrap();
        fs::write(&path2, make_prd_json("P2", "US-001")).unwrap();

        init(
            temp_dir.path(),
            &[&path1],
            false,
            false,
            false,
            false,
            PrefixMode::Explicit("P1".to_string()),
        )
        .unwrap();
        init(
            temp_dir.path(),
            &[&path2],
            false,
            true,
            false,
            false,
            PrefixMode::Explicit("P2".to_string()),
        )
        .unwrap();

        let conn = open_connection(temp_dir.path()).unwrap();

        // Retrieve the prd_id for each PRD
        let p1_id: i64 = conn
            .query_row(
                "SELECT id FROM prd_metadata WHERE task_prefix = 'P1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let p2_id: i64 = conn
            .query_row(
                "SELECT id FROM prd_metadata WHERE task_prefix = 'P2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(p1_id, p2_id, "each PRD must have a distinct prd_id");

        // prd_files for p1.json must link to p1_id only
        // file_path is stored as the full path (or relative to tasks/) — use LIKE for portability
        let p1_file_prd_id: i64 = conn
            .query_row(
                "SELECT prd_id FROM prd_files WHERE file_path LIKE '%p1.json' AND file_type = 'task_list'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            p1_file_prd_id, p1_id,
            "p1.json prd_files row must link to P1's prd_id"
        );

        // prd_files for p2.json must link to p2_id only
        let p2_file_prd_id: i64 = conn
            .query_row(
                "SELECT prd_id FROM prd_files WHERE file_path LIKE '%p2.json' AND file_type = 'task_list'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            p2_file_prd_id, p2_id,
            "p2.json prd_files row must link to P2's prd_id"
        );
    }

    // AC 2: Import P1 twice (with --force on second import).
    // The prd_metadata row must be updated (not duplicated), and prd_files must
    // not be doubled.
    #[test]
    fn test_import_p1_twice_metadata_not_duplicated() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = temp_dir.path().join("p1.json");
        fs::write(&path1, make_prd_json("P1", "US-001")).unwrap();

        init(
            temp_dir.path(),
            &[&path1],
            false,
            false,
            false,
            false,
            PrefixMode::Auto,
        )
        .unwrap();

        // Re-import with --force (scoped to P1)
        init(
            temp_dir.path(),
            &[&path1],
            true, // force
            false,
            false,
            false,
            PrefixMode::Auto,
        )
        .unwrap();

        let conn = open_connection(temp_dir.path()).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "re-importing P1 with --force must not duplicate prd_metadata"
        );
    }

    #[test]
    fn test_import_p1_twice_prd_files_not_duplicated() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = temp_dir.path().join("p1.json");
        fs::write(&path1, make_prd_json("P1", "US-001")).unwrap();

        init(
            temp_dir.path(),
            &[&path1],
            false,
            false,
            false,
            false,
            PrefixMode::Auto,
        )
        .unwrap();

        let conn = open_connection(temp_dir.path()).unwrap();
        let count_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_files", [], |r| r.get(0))
            .unwrap();

        // Re-import with --force
        init(
            temp_dir.path(),
            &[&path1],
            true, // force
            false,
            false,
            false,
            PrefixMode::Auto,
        )
        .unwrap();

        let count_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM prd_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count_before, count_after,
            "re-importing P1 with --force must not duplicate prd_files entries"
        );
    }

    // AC 3: After importing P1 and P2, --force P1 must delete only P1's data.
    // P2 tasks, relationships, and prd_metadata must survive.
    #[test]
    fn test_force_p1_deletes_only_p1_leaves_p2_intact() {
        let temp_dir = TempDir::new().unwrap();
        let path1 = temp_dir.path().join("p1.json");
        let path2 = temp_dir.path().join("p2.json");
        fs::write(&path1, make_prd_json("P1", "US-001")).unwrap();
        fs::write(&path2, make_prd_json("P2", "US-001")).unwrap();

        // Import both PRDs
        init(
            temp_dir.path(),
            &[&path1],
            false,
            false,
            false,
            false,
            PrefixMode::Explicit("P1".to_string()),
        )
        .unwrap();
        init(
            temp_dir.path(),
            &[&path2],
            false,
            true, // append
            false,
            false,
            PrefixMode::Explicit("P2".to_string()),
        )
        .unwrap();

        // Force re-import P1 only
        init(
            temp_dir.path(),
            &[&path1],
            true, // force
            false,
            false,
            false,
            PrefixMode::Explicit("P1".to_string()),
        )
        .unwrap();

        let conn = open_connection(temp_dir.path()).unwrap();

        // P2 task must still exist
        let p2_task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'P2-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            p2_task_count, 1,
            "P2 task must survive scoped --force of P1"
        );

        // P2 prd_metadata must still exist
        let p2_meta_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix = 'P2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            p2_meta_count, 1,
            "P2 prd_metadata must survive scoped --force of P1"
        );

        // P1 task must be re-imported (force deleted then re-inserted)
        let p1_task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'P1-%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            p1_task_count, 1,
            "P1 task must be re-imported after --force"
        );
    }
}

// ============================================================================
// init_project tests
// ============================================================================

#[test]
fn test_init_project_fresh_creates_db_and_config() {
    let project_dir = TempDir::new().unwrap();
    let result = super::init_project(project_dir.path()).unwrap();

    assert!(
        result.created_dirs,
        "should report dir creation on first call"
    );
    assert!(
        result.created_config,
        "should report config creation on first call"
    );
    assert!(result.fresh_import, "fresh_import mirrors created_dirs");

    let db_path = project_dir.path().join(".task-mgr").join("tasks.db");
    assert!(db_path.exists(), ".task-mgr/tasks.db must exist");

    let config_path = project_dir.path().join(".task-mgr").join("config.json");
    assert!(config_path.exists(), ".task-mgr/config.json must exist");

    let config_str = fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
    assert_eq!(
        config.get("version").and_then(|v| v.as_u64()),
        Some(1),
        "config must have version >= 1"
    );
}

#[test]
fn test_init_project_preserves_existing_config_fields() {
    let project_dir = TempDir::new().unwrap();
    let db_dir = project_dir.path().join(".task-mgr");
    fs::create_dir_all(&db_dir).unwrap();
    fs::write(db_dir.join("config.json"), r#"{"customField": "keepme"}"#).unwrap();

    super::init_project(project_dir.path()).unwrap();

    let config_str = fs::read_to_string(db_dir.join("config.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
    assert_eq!(
        config.get("customField").and_then(|v| v.as_str()),
        Some("keepme"),
        "existing customField must be preserved"
    );
    assert_eq!(
        config.get("version").and_then(|v| v.as_u64()),
        Some(1),
        "version default must be filled in for missing key"
    );
}

#[test]
fn test_init_project_idempotent() {
    let project_dir = TempDir::new().unwrap();

    // First call — creates everything
    super::init_project(project_dir.path()).unwrap();

    let config_path = project_dir.path().join(".task-mgr").join("config.json");
    let content_after_first = fs::read(&config_path).unwrap();

    // Second call — should be a no-op as far as created_* flags go
    let result2 = super::init_project(project_dir.path()).unwrap();
    assert!(
        !result2.created_dirs,
        "created_dirs must be false on second call"
    );
    assert!(
        !result2.created_config,
        "created_config must be false on second call"
    );

    let content_after_second = fs::read(&config_path).unwrap();
    assert_eq!(
        content_after_first, content_after_second,
        "config.json contents must be byte-identical between calls"
    );
}

#[test]
fn test_init_project_leaves_tasks_json_untouched() {
    let project_dir = TempDir::new().unwrap();
    let tasks_dir = project_dir.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();
    let foo_path = tasks_dir.join("foo.json");
    let foo_content = br#"{"id":"SOME-001","title":"stub"}"#;
    fs::write(&foo_path, foo_content).unwrap();

    let before = fs::read(&foo_path).unwrap();
    super::init_project(project_dir.path()).unwrap();
    let after = fs::read(&foo_path).unwrap();

    assert_eq!(
        before, after,
        "tasks/foo.json must be byte-identical before and after init_project"
    );
}

#[test]
fn test_init_project_empty_task_mgr_dir() {
    let project_dir = TempDir::new().unwrap();
    // Pre-create empty .task-mgr/ (no DB, no config)
    fs::create_dir_all(project_dir.path().join(".task-mgr")).unwrap();

    let result1 = super::init_project(project_dir.path()).unwrap();
    // Dir existed already, so created_dirs is false; but config is new
    assert!(
        !result1.created_dirs,
        "dir existed — created_dirs must be false"
    );
    assert!(
        result1.created_config,
        "config didn't exist — created_config must be true"
    );

    assert!(
        project_dir
            .path()
            .join(".task-mgr")
            .join("tasks.db")
            .exists()
    );
    assert!(
        project_dir
            .path()
            .join(".task-mgr")
            .join("config.json")
            .exists()
    );

    // Second call: no-op
    let result2 = super::init_project(project_dir.path()).unwrap();
    assert!(!result2.created_dirs);
    assert!(!result2.created_config);
}

#[test]
fn test_init_project_non_tty_no_default_model() {
    // In test environments stdin/stderr are not TTYs, so the picker must be skipped.
    let project_dir = TempDir::new().unwrap();
    super::init_project(project_dir.path()).unwrap();

    let config_path = project_dir.path().join(".task-mgr").join("config.json");
    let config_str = fs::read_to_string(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
    assert!(
        config.get("defaultModel").is_none(),
        "picker must not fire in non-TTY environment; defaultModel must be absent"
    );
}

#[test]
fn test_init_project_does_not_create_tasks_subdir() {
    let project_dir = TempDir::new().unwrap();
    super::init_project(project_dir.path()).unwrap();

    let tasks_subdir = project_dir.path().join(".task-mgr").join("tasks");
    assert!(
        !tasks_subdir.exists(),
        "init_project must NOT create .task-mgr/tasks/ — that belongs to loop/batch init"
    );
}
