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
fn test_init_auto_prefix_generates_uuid_when_absent() {
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
fn test_parse_prd_file_with_default_model() {
    let json = r#"{
        "project": "test",
        "defaultModel": "claude-haiku-4-5-20251001",
        "userStories": [
            {"id": "US-001", "title": "Task", "priority": 1, "passes": false}
        ]
    }"#;

    let prd: super::parse::PrdFile = serde_json::from_str(json).unwrap();
    assert_eq!(
        prd.default_model,
        Some("claude-haiku-4-5-20251001".to_string())
    );
}

#[test]
fn test_parse_prd_file_backward_compat_without_default_model() {
    let json = r#"{
        "project": "test",
        "userStories": [
            {"id": "US-001", "title": "Task", "priority": 1, "passes": false}
        ]
    }"#;

    let prd: super::parse::PrdFile = serde_json::from_str(json).unwrap();
    assert_eq!(
        prd.default_model, None,
        "default_model should default to None"
    );
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
fn test_insert_prd_metadata_with_default_model() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "model-test",
        "defaultModel": "claude-sonnet-4-6",
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
