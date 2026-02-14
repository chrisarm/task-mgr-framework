//! Integration tests for import/export round-trip functionality.
//!
//! These tests verify that JSON PRD files can be imported and exported
//! with preserved structure and deterministic output.

use serde_json::Value;
use std::fs;
use tempfile::TempDir;

use task_mgr::commands::{complete, export, init};
use task_mgr::db::open_connection;

/// Get the path to the sample PRD fixture file.
fn sample_prd_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_prd.json")
}

/// Extract just the userStories from a PRD JSON for comparison.
fn extract_user_stories(prd: &Value) -> Vec<Value> {
    prd.get("userStories")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

#[test]
fn test_import_export_round_trip() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Read original PRD
    let original_json = fs::read_to_string(&prd_path).unwrap();
    let original: Value = serde_json::from_str(&original_json).unwrap();

    // Import the PRD
    let init_result =
        init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();
    assert!(
        init_result.tasks_imported > 0,
        "Should import at least one task"
    );

    // Export to a new file
    let export_path = temp_dir.path().join("exported.json");
    let export_result = export::export(temp_dir.path(), &export_path, false, None).unwrap();
    assert_eq!(
        export_result.tasks_exported, init_result.tasks_imported,
        "Exported task count should match imported count"
    );

    // Read exported PRD
    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();

    // Compare user stories (the main content)
    let original_stories = extract_user_stories(&original);
    let exported_stories = extract_user_stories(&exported);

    assert_eq!(
        original_stories.len(),
        exported_stories.len(),
        "Number of user stories should match"
    );

    // Verify key fields are preserved for each story
    for (orig, exp) in original_stories.iter().zip(exported_stories.iter()) {
        // ID should match
        assert_eq!(orig.get("id"), exp.get("id"), "Task IDs should match");

        // Title should match
        assert_eq!(
            orig.get("title"),
            exp.get("title"),
            "Task titles should match"
        );

        // Priority should match
        assert_eq!(
            orig.get("priority"),
            exp.get("priority"),
            "Task priorities should match"
        );

        // passes should match (after status mapping)
        assert_eq!(
            orig.get("passes"),
            exp.get("passes"),
            "Task passes status should match for task {:?}",
            orig.get("id")
        );
    }

    // Verify metadata is preserved
    assert_eq!(
        original.get("project"),
        exported.get("project"),
        "Project name should be preserved"
    );
    assert_eq!(
        original.get("branchName"),
        exported.get("branchName"),
        "Branch name should be preserved"
    );
}

#[test]
fn test_import_modify_export() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Read original to find a todo task
    let original_json = fs::read_to_string(&prd_path).unwrap();
    let original: Value = serde_json::from_str(&original_json).unwrap();

    // Find a task with passes: false
    let stories = extract_user_stories(&original);
    let todo_task = stories
        .iter()
        .find(|s| s.get("passes") == Some(&Value::Bool(false)))
        .expect("Should have at least one task with passes: false");
    let task_id = todo_task
        .get("id")
        .and_then(|v| v.as_str())
        .expect("Task should have an id");

    // Import the PRD
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Complete the task (use force=true since task is in todo status after import)
    let mut conn = open_connection(temp_dir.path()).unwrap();
    let complete_result =
        complete::complete(&mut conn, &[task_id.to_string()], None, None, true).unwrap();
    assert_eq!(complete_result.completed_count, 1);
    drop(conn);

    // Export
    let export_path = temp_dir.path().join("modified.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    // Read exported and verify the task is now passes: true
    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();

    let exported_stories = extract_user_stories(&exported);
    let modified_task = exported_stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(task_id))
        .expect("Task should exist in export");

    assert_eq!(
        modified_task.get("passes"),
        Some(&Value::Bool(true)),
        "Completed task should have passes: true in export"
    );

    // Other tasks should retain their original passes status
    for (orig, exp) in stories.iter().zip(exported_stories.iter()) {
        let orig_id = orig.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if orig_id != task_id {
            assert_eq!(
                orig.get("passes"),
                exp.get("passes"),
                "Unmodified task {} should retain original passes status",
                orig_id
            );
        }
    }
}

#[test]
fn test_import_with_force_replaces_data() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // First import
    let first_result =
        init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();
    assert!(first_result.tasks_imported > 0);

    // Modify a task in the database
    let conn = open_connection(temp_dir.path()).unwrap();
    conn.execute(
        "UPDATE tasks SET title = 'MODIFIED TITLE' WHERE id = (SELECT id FROM tasks LIMIT 1)",
        [],
    )
    .unwrap();
    drop(conn);

    // Re-import with --force
    let force_result =
        init::init(temp_dir.path(), &[&prd_path], true, false, false, false, init::PrefixMode::Disabled).unwrap();
    assert!(
        force_result.fresh_import,
        "--force should result in fresh import"
    );
    assert_eq!(
        force_result.tasks_imported, first_result.tasks_imported,
        "Should import same number of tasks"
    );

    // Export and verify the modification was replaced
    let export_path = temp_dir.path().join("after_force.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    assert!(
        !exported_json.contains("MODIFIED TITLE"),
        "--force should have replaced the modified data with original"
    );
}

#[test]
fn test_export_sorts_arrays_deterministically() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Import
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();

    // Export twice
    let export_path1 = temp_dir.path().join("export1.json");
    let export_path2 = temp_dir.path().join("export2.json");

    export::export(temp_dir.path(), &export_path1, false, None).unwrap();
    export::export(temp_dir.path(), &export_path2, false, None).unwrap();

    // Read both exports
    let json1 = fs::read_to_string(&export_path1).unwrap();
    let json2 = fs::read_to_string(&export_path2).unwrap();

    // They should be byte-for-byte identical (deterministic output)
    assert_eq!(
        json1, json2,
        "Multiple exports should produce identical JSON"
    );
}

#[test]
fn test_relationships_preserved_in_round_trip() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Read original
    let original_json = fs::read_to_string(&prd_path).unwrap();
    let original: Value = serde_json::from_str(&original_json).unwrap();

    // Import and export
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();
    let export_path = temp_dir.path().join("exported.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    // Read exported
    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();

    let original_stories = extract_user_stories(&original);
    let exported_stories = extract_user_stories(&exported);

    // Verify relationships are preserved for each story
    for (orig, exp) in original_stories.iter().zip(exported_stories.iter()) {
        let task_id = orig.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");

        // Check dependsOn (arrays should match after sorting)
        let orig_deps = orig
            .get("dependsOn")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let exp_deps = exp
            .get("dependsOn")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            orig_deps.len(),
            exp_deps.len(),
            "dependsOn count should match for task {}",
            task_id
        );

        // Check synergyWith
        let orig_syn = orig
            .get("synergyWith")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let exp_syn = exp
            .get("synergyWith")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            orig_syn.len(),
            exp_syn.len(),
            "synergyWith count should match for task {}",
            task_id
        );

        // Check batchWith
        let orig_batch = orig
            .get("batchWith")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let exp_batch = exp
            .get("batchWith")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            orig_batch.len(),
            exp_batch.len(),
            "batchWith count should match for task {}",
            task_id
        );

        // Check conflictsWith
        let orig_conflicts = orig
            .get("conflictsWith")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let exp_conflicts = exp
            .get("conflictsWith")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            orig_conflicts.len(),
            exp_conflicts.len(),
            "conflictsWith count should match for task {}",
            task_id
        );
    }
}

#[test]
fn test_touches_files_preserved_in_round_trip() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Read original
    let original_json = fs::read_to_string(&prd_path).unwrap();
    let original: Value = serde_json::from_str(&original_json).unwrap();

    // Import and export
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();
    let export_path = temp_dir.path().join("exported.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    // Read exported
    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();

    let original_stories = extract_user_stories(&original);
    let exported_stories = extract_user_stories(&exported);

    // Verify touchesFiles are preserved for each story
    for (orig, exp) in original_stories.iter().zip(exported_stories.iter()) {
        let task_id = orig.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");

        let orig_files: Vec<String> = orig
            .get("touchesFiles")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let exp_files: Vec<String> = exp
            .get("touchesFiles")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Files are sorted alphabetically in export
        let mut orig_files_sorted = orig_files.clone();
        orig_files_sorted.sort();

        assert_eq!(
            orig_files_sorted, exp_files,
            "touchesFiles should be preserved (and sorted) for task {}",
            task_id
        );
    }
}

#[test]
fn test_acceptance_criteria_preserved_in_round_trip() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Read original
    let original_json = fs::read_to_string(&prd_path).unwrap();
    let original: Value = serde_json::from_str(&original_json).unwrap();

    // Import and export
    init::init(temp_dir.path(), &[&prd_path], false, false, false, false, init::PrefixMode::Disabled).unwrap();
    let export_path = temp_dir.path().join("exported.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    // Read exported
    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();

    let original_stories = extract_user_stories(&original);
    let exported_stories = extract_user_stories(&exported);

    // Verify acceptanceCriteria are preserved for each story
    for (orig, exp) in original_stories.iter().zip(exported_stories.iter()) {
        let task_id = orig.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");

        let orig_criteria = orig.get("acceptanceCriteria");
        let exp_criteria = exp.get("acceptanceCriteria");

        assert_eq!(
            orig_criteria, exp_criteria,
            "acceptanceCriteria should be preserved for task {}",
            task_id
        );
    }
}
