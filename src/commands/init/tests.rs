//! Tests for the init command.

use super::*;
use crate::db::open_connection;
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
    assert_eq!(result.relationships_imported, 2); // 1 synergy + 1 dependency
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
    assert_eq!(result.relationships_imported, 4);

    let conn = open_connection(temp_dir.path()).unwrap();
    let count: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM task_relationships WHERE task_id = 'US-001'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 4);
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
    assert_eq!(result.relationships_imported, 2);
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
    assert_eq!(preview.relationships, 2);

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

    // US-001 has synergy with US-002 -> P3-US-001 synergy with P3-US-002
    let syn: String = conn
        .query_row(
            "SELECT related_id FROM task_relationships WHERE task_id = 'P3-US-001' AND rel_type = 'synergyWith'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(syn, "P3-US-002");
}

#[test]
fn test_init_auto_prefix_from_json_field() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "test",
        "taskPrefix": "P5",
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

    assert_eq!(result.prefix_applied, Some("P5".to_string()));

    let conn = open_connection(temp_dir.path()).unwrap();
    let id: String = conn
        .query_row("SELECT id FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(id, "P5-US-001");
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
    let json = r#"{
        "id": "US-001",
        "title": "Task with model",
        "priority": 1,
        "passes": false,
        "model": "claude-sonnet-4-6",
        "difficulty": "high",
        "escalationNote": "Retried after OOM"
    }"#;

    let story: super::parse::PrdUserStory = serde_json::from_str(json).unwrap();

    assert_eq!(story.model, Some("claude-sonnet-4-6".to_string()));
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
    let json = r#"{
        "project": "test",
        "model": "claude-haiku-4-5-20251001",
        "userStories": [
            {"id": "US-001", "title": "Task", "priority": 1, "passes": false}
        ]
    }"#;

    let prd: super::parse::PrdFile = serde_json::from_str(json).unwrap();
    assert_eq!(prd.model, Some("claude-haiku-4-5-20251001".to_string()));
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
    let json = r#"{
        "project": "test",
        "userStories": [
            {
                "id": "US-001",
                "title": "Model task",
                "priority": 1,
                "passes": false,
                "model": "claude-opus-4-6",
                "difficulty": "high",
                "escalationNote": "Bumped from sonnet after failure"
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
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'US-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();

    assert_eq!(model, Some("claude-opus-4-6".to_string()));
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
    let json = r#"{
        "project": "model-test",
        "model": "claude-sonnet-4-6",
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

    assert_eq!(default_model, Some("claude-sonnet-4-6".to_string()));
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

    // These imports will be needed after the implementation lands:
    //   use crate::commands::init::import::{drop_existing_data, insert_prd_file, insert_prd_metadata};
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
        }
    }

    // -----------------------------------------------------------------------
    // insert_prd_metadata: upsert by task_prefix, returns i64
    // -----------------------------------------------------------------------

    #[test]
    #[ignore = "RED-PHASE: insert_prd_metadata must return TaskMgrResult<i64> (new row id). \
                Also requires migration v9 to remove CHECK(id=1) singleton constraint. \
                Un-ignore after SS-FEAT changes the return type."]
    fn test_insert_prd_metadata_new_prefix_returns_id() {
        let (_dir, conn) = setup_migrated_db();
        let prd = make_prd("project-one", Some("P1"));
        // After implementation: returns the newly inserted row's id.
        todo!(
            "needs insert_prd_metadata(conn, prd, raw_json) -> TaskMgrResult<i64> \
             and migration v9 (CHECK(id=1) removed)"
        );
        // Intended assertions (fill in after signature change):
        //   let id = insert_prd_metadata(&conn, &prd, None).unwrap();
        //   assert!(id > 0, "returned id must be positive");
        //   let count: i64 = conn
        //       .query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0))
        //       .unwrap();
        //   assert_eq!(count, 1);
    }

    #[test]
    #[ignore = "RED-PHASE: insert_prd_metadata must upsert by task_prefix. \
                Calling it twice with the same prefix must update (not duplicate) the row. \
                Requires migration v9 UNIQUE(task_prefix) constraint + ON CONFLICT upsert."]
    fn test_insert_prd_metadata_upsert_existing_prefix() {
        let (_dir, conn) = setup_migrated_db();
        todo!(
            "needs insert_prd_metadata to upsert via ON CONFLICT(task_prefix) DO UPDATE. \
             After two calls with prefix='P1': SELECT COUNT(*) FROM prd_metadata must equal 1, \
             and SELECT project FROM prd_metadata WHERE task_prefix='P1' must be the second value."
        );
        // Intended:
        //   let prd1 = make_prd("project-original", Some("P1"));
        //   let prd2 = make_prd("project-updated", Some("P1"));
        //   insert_prd_metadata(&conn, &prd1, None).unwrap();
        //   insert_prd_metadata(&conn, &prd2, None).unwrap();
        //   let count: i64 = conn.query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0)).unwrap();
        //   assert_eq!(count, 1, "upsert must not create a duplicate row");
        //   let project: String = conn
        //       .query_row("SELECT project FROM prd_metadata WHERE task_prefix='P1'", [], |r| r.get(0))
        //       .unwrap();
        //   assert_eq!(project, "project-updated");
    }

    #[test]
    #[ignore = "RED-PHASE: two distinct prefixes must produce two separate prd_metadata rows. \
                Requires migration v9 (multi-row support) and insert_prd_metadata returning i64."]
    fn test_insert_prd_metadata_two_different_prefixes_creates_two_rows() {
        let (_dir, conn) = setup_migrated_db();
        todo!(
            "needs migration v9 and updated insert_prd_metadata. \
             After inserting P1 and P2 PRDs, SELECT COUNT(*) FROM prd_metadata must equal 2."
        );
        // Intended:
        //   let prd1 = make_prd("project-one", Some("P1"));
        //   let prd2 = make_prd("project-two", Some("P2"));
        //   let id1 = insert_prd_metadata(&conn, &prd1, None).unwrap();
        //   let id2 = insert_prd_metadata(&conn, &prd2, None).unwrap();
        //   assert_ne!(id1, id2, "distinct prefixes must yield distinct row ids");
        //   let count: i64 = conn.query_row("SELECT COUNT(*) FROM prd_metadata", [], |r| r.get(0)).unwrap();
        //   assert_eq!(count, 2);
    }

    // -----------------------------------------------------------------------
    // insert_prd_file: dynamic prd_id parameter
    // -----------------------------------------------------------------------

    #[test]
    #[ignore = "RED-PHASE: insert_prd_file must accept prd_id: i64 instead of hardcoding 1. \
                Current signature: insert_prd_file(conn, file_path, file_type). \
                Required signature: insert_prd_file(conn, prd_id: i64, file_path, file_type). \
                Un-ignore after SS-FEAT changes the signature."]
    fn test_insert_prd_file_uses_dynamic_prd_id() {
        let (_dir, conn) = setup_migrated_db();
        todo!(
            "needs insert_prd_file(conn, prd_id: i64, file_path: &str, file_type: &str). \
             After impl: insert_prd_file(&conn, 42, 'tasks/prd.json', 'task_list') must insert \
             a row with prd_id=42, not prd_id=1."
        );
        // Intended:
        //   // First insert a prd_metadata row with a known id (e.g. via direct SQL or insert_prd_metadata)
        //   conn.execute("INSERT INTO prd_metadata (id, project) VALUES (42, 'proj')", []).unwrap();
        //   insert_prd_file(&conn, 42, "tasks/prd.json", "task_list").unwrap();
        //   let prd_id: i64 = conn
        //       .query_row("SELECT prd_id FROM prd_files WHERE file_path='tasks/prd.json'", [], |r| r.get(0))
        //       .unwrap();
        //   assert_eq!(prd_id, 42, "prd_id must match the value passed in, not hardcoded 1");
    }

    // -----------------------------------------------------------------------
    // drop_existing_data: scoped prefix filtering
    // -----------------------------------------------------------------------

    #[test]
    #[ignore = "RED-PHASE: drop_existing_data must accept prefix: Option<&str>. \
                With Some(prefix), only tasks whose id starts with '<prefix>-' are deleted. \
                Requires new function signature: drop_existing_data(conn, prefix: Option<&str>)."]
    fn test_drop_existing_data_scoped_deletes_only_prefix_tasks() {
        let (_dir, conn) = setup_migrated_db();
        todo!(
            "needs drop_existing_data(conn, prefix: Option<&str>). \
             Setup: insert tasks P1-US-001 and P2-US-001. \
             Act: drop_existing_data(&conn, Some('P1')). \
             Assert: P1-US-001 deleted, P2-US-001 still present."
        );
        // Intended:
        //   conn.execute("INSERT INTO tasks (id, title, status, priority, acceptance_criteria) \
        //       VALUES ('P1-US-001','T1','todo',1,'[]')", []).unwrap();
        //   conn.execute("INSERT INTO tasks (id, title, status, priority, acceptance_criteria) \
        //       VALUES ('P2-US-001','T2','todo',1,'[]')", []).unwrap();
        //   drop_existing_data(&conn, Some("P1")).unwrap();
        //   let p1: i64 = conn.query_row(
        //       "SELECT COUNT(*) FROM tasks WHERE id LIKE 'P1-%'", [], |r| r.get(0)).unwrap();
        //   let p2: i64 = conn.query_row(
        //       "SELECT COUNT(*) FROM tasks WHERE id LIKE 'P2-%'", [], |r| r.get(0)).unwrap();
        //   assert_eq!(p1, 0, "P1 tasks must be deleted");
        //   assert_eq!(p2, 1, "P2 tasks must be preserved");
    }

    /// Known-bad discriminator: after inserting P1 and P2 tasks, a scoped
    /// force-delete of P1 must leave all P2 tasks intact.
    #[test]
    #[ignore = "RED-PHASE: known-bad discriminator — scoped --force on P1 must not touch P2. \
                Requires drop_existing_data(conn, Some('P1')) support."]
    fn test_cross_prd_force_delete_leaves_other_prd_intact() {
        let (_dir, conn) = setup_migrated_db();
        todo!(
            "needs drop_existing_data(conn, Some('P1')). \
             Setup: insert P1-US-001 and P2-US-001 (and P2-US-002 with a relationship). \
             Act: drop_existing_data(&conn, Some('P1')). \
             Assert: P2 tasks, task_files, and task_relationships all survive."
        );
        // Intended:
        //   // Insert P1 task with file + relationship
        //   conn.execute("INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
        //       VALUES ('P1-US-001','P1T','todo',10,'[]')", []).unwrap();
        //   conn.execute("INSERT INTO task_files (task_id,file_path) VALUES ('P1-US-001','a.rs')", []).unwrap();
        //   // Insert P2 tasks with relationship
        //   conn.execute("INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
        //       VALUES ('P2-US-001','P2T1','todo',10,'[]')", []).unwrap();
        //   conn.execute("INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
        //       VALUES ('P2-US-002','P2T2','todo',20,'[]')", []).unwrap();
        //   conn.execute("INSERT INTO task_relationships (task_id,related_id,rel_type) \
        //       VALUES ('P2-US-002','P2-US-001','dependsOn')", []).unwrap();
        //   drop_existing_data(&conn, Some("P1")).unwrap();
        //   let p2_count: i64 = conn.query_row(
        //       "SELECT COUNT(*) FROM tasks WHERE id LIKE 'P2-%'", [], |r| r.get(0)).unwrap();
        //   let p2_rel: i64 = conn.query_row(
        //       "SELECT COUNT(*) FROM task_relationships WHERE task_id LIKE 'P2-%'", [], |r| r.get(0)).unwrap();
        //   assert_eq!(p2_count, 2, "both P2 tasks must survive scoped P1 delete");
        //   assert_eq!(p2_rel, 1, "P2 relationships must survive");
    }

    #[test]
    #[ignore = "RED-PHASE: drop_existing_data(conn, None) must preserve legacy all-wipe behavior. \
                Requires new signature: drop_existing_data(conn, prefix: Option<&str>)."]
    fn test_drop_existing_data_none_prefix_wipes_everything() {
        let (_dir, conn) = setup_migrated_db();
        todo!(
            "needs drop_existing_data(conn, prefix: Option<&str>). \
             With None, must delete ALL tasks from all PRDs (same as current behavior). \
             Assert: SELECT COUNT(*) FROM tasks = 0 after drop with None prefix."
        );
        // Intended:
        //   conn.execute("INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
        //       VALUES ('P1-US-001','T1','todo',1,'[]')", []).unwrap();
        //   conn.execute("INSERT INTO tasks (id,title,status,priority,acceptance_criteria) \
        //       VALUES ('P2-US-001','T2','todo',1,'[]')", []).unwrap();
        //   drop_existing_data(&conn, None).unwrap();
        //   let count: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        //   assert_eq!(count, 0, "None-prefix drop must wipe all tasks");
    }

    #[test]
    #[ignore = "RED-PHASE: scoped drop_existing_data must NOT delete learnings. \
                Learnings are not PRD-scoped and must survive a scoped --force. \
                Requires drop_existing_data(conn, prefix: Option<&str>)."]
    fn test_drop_existing_data_scoped_preserves_learnings() {
        let (_dir, conn) = setup_migrated_db();
        todo!(
            "needs drop_existing_data(conn, Some('P1')). \
             Setup: insert a learning directly. \
             Act: drop_existing_data(&conn, Some('P1')). \
             Assert: SELECT COUNT(*) FROM learnings still equals the pre-delete count."
        );
        // Intended:
        //   conn.execute(
        //       "INSERT INTO learnings (title, content, outcome, confidence) \
        //        VALUES ('test learning', 'content', 'success', 'high')",
        //       [],
        //   ).unwrap();
        //   drop_existing_data(&conn, Some("P1")).unwrap();
        //   let count: i64 = conn.query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0)).unwrap();
        //   assert_eq!(count, 1, "learnings must not be deleted by scoped --force");
    }

    #[test]
    #[ignore = "RED-PHASE: scoped drop_existing_data must only delete the matching prd_metadata row. \
                After deleting P1, the P2 prd_metadata row must remain. \
                Requires migration v9 (multi-row prd_metadata) + new drop_existing_data signature."]
    fn test_drop_existing_data_scoped_preserves_other_prd_metadata() {
        let (_dir, conn) = setup_migrated_db();
        todo!(
            "needs migration v9 + drop_existing_data(conn, Some('P1')). \
             Setup: insert prd_metadata rows for P1 and P2 (requires multi-row support from v9). \
             Act: drop_existing_data(&conn, Some('P1')). \
             Assert: P2 prd_metadata row still exists."
        );
        // Intended (after migration v9):
        //   conn.execute("INSERT INTO prd_metadata (id,project,task_prefix) VALUES (1,'proj-one','P1')", []).unwrap();
        //   conn.execute("INSERT INTO prd_metadata (id,project,task_prefix) VALUES (2,'proj-two','P2')", []).unwrap();
        //   drop_existing_data(&conn, Some("P1")).unwrap();
        //   let count: i64 = conn.query_row(
        //       "SELECT COUNT(*) FROM prd_metadata WHERE task_prefix='P2'", [], |r| r.get(0)).unwrap();
        //   assert_eq!(count, 1, "P2 prd_metadata must survive scoped P1 delete");
    }
}
