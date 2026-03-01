//! Tests for the export module.

use super::*;
use crate::commands::init;
use crate::commands::init::PrefixMode;
use crate::db::{create_schema, run_migrations};
use std::fs;
use tempfile::TempDir;

use progress::calculate_statistics;

fn create_test_prd() -> String {
    r#"{
        "project": "test-project",
        "branchName": "main",
        "description": "Test project description",
        "priorityPhilosophy": {"key": "value"},
        "globalAcceptanceCriteria": {"criteria": ["No warnings"]},
        "reviewGuidelines": {"critical": "1-10"},
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

#[test]
fn test_export_basic() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    // Import first
    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // Export
    let export_path = temp_dir.path().join("exported.json");
    let result = export(temp_dir.path(), &export_path, false, None).unwrap();

    assert_eq!(result.tasks_exported, 2);
    assert!(result.progress_file.is_none());
    assert!(result.learnings_file.is_none());
    assert!(export_path.exists());
}

#[test]
fn test_export_with_progress() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    let result = export(temp_dir.path(), &export_path, true, None).unwrap();

    assert_eq!(result.tasks_exported, 2);
    assert!(result.progress_file.is_some());
    assert_eq!(result.runs_exported, Some(0));
    assert_eq!(result.learnings_exported, Some(0));

    let progress_path = temp_dir.path().join("progress.json");
    assert!(progress_path.exists());
}

#[test]
fn test_export_with_learnings_file() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    let learnings_path = temp_dir.path().join("learnings.json");
    let result = export(temp_dir.path(), &export_path, false, Some(&learnings_path)).unwrap();

    assert!(result.learnings_file.is_some());
    assert!(learnings_path.exists());
}

#[test]
fn test_export_preserves_metadata() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    // Read and verify exported JSON
    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    assert_eq!(exported.project, "test-project");
    assert_eq!(exported.branch_name, Some("main".to_string()));
    assert_eq!(
        exported.description,
        Some("Test project description".to_string())
    );
    assert!(exported.priority_philosophy.is_some());
}

#[test]
fn test_export_maps_status_to_passes() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    // US-001 was passes: false -> status: todo -> passes: false
    let us001 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-001")
        .unwrap();
    assert!(!us001.passes);

    // US-002 was passes: true -> status: done -> passes: true
    let us002 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-002")
        .unwrap();
    assert!(us002.passes);
}

#[test]
fn test_export_sorts_arrays_alphabetically() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    // Check touchesFiles are sorted
    let us001 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-001")
        .unwrap();
    assert_eq!(us001.touches_files, vec!["src/lib.rs", "src/main.rs"]);
}

#[test]
fn test_export_tasks_ordered_by_id() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    // Verify ordering
    assert_eq!(exported.user_stories[0].id, "US-001");
    assert_eq!(exported.user_stories[1].id, "US-002");
}

#[test]
fn test_export_empty_database() {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    let export_path = temp_dir.path().join("exported.json");
    let result = export(temp_dir.path(), &export_path, false, None).unwrap();

    assert_eq!(result.tasks_exported, 0);
    assert!(export_path.exists());

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: ExportedPrd = serde_json::from_str(&exported_json).unwrap();
    assert_eq!(exported.project, "unknown");
    assert!(exported.user_stories.is_empty());
}

#[test]
fn test_export_preserves_relationships() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    let us001 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-001")
        .unwrap();
    assert_eq!(us001.synergy_with, vec!["US-002"]);

    let us002 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-002")
        .unwrap();
    assert_eq!(us002.depends_on, vec!["US-001"]);
}

#[test]
fn test_export_preserves_acceptance_criteria() {
    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, create_test_prd()).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    let us001 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-001")
        .unwrap();
    assert_eq!(
        us001.acceptance_criteria,
        vec!["Criterion 1", "Criterion 2"]
    );
}

#[test]
fn test_format_text_basic() {
    let result = ExportResult {
        prd_file: "/path/to/exported.json".to_string(),
        tasks_exported: 10,
        progress_file: None,
        learnings_file: None,
        learnings_exported: None,
        runs_exported: None,
    };

    let text = format_text(&result);
    assert!(text.contains("Exported PRD to: /path/to/exported.json"));
    assert!(text.contains("Tasks exported: 10"));
}

#[test]
fn test_format_text_with_progress() {
    let result = ExportResult {
        prd_file: "/path/to/exported.json".to_string(),
        tasks_exported: 10,
        progress_file: Some("/path/to/progress.json".to_string()),
        learnings_file: None,
        learnings_exported: Some(5),
        runs_exported: Some(3),
    };

    let text = format_text(&result);
    assert!(text.contains("Progress exported to: /path/to/progress.json"));
    assert!(text.contains("Runs exported: 3"));
    assert!(text.contains("Learnings exported: 5"));
}

#[test]
fn test_atomic_write() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("test.json");

    let data = serde_json::json!({"key": "value"});
    write_json_atomic(&path, &data).unwrap();

    assert!(path.exists());
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("\"key\": \"value\""));

    // Temp file should not exist
    let tmp_path = path.with_extension("json.tmp");
    assert!(!tmp_path.exists());
}

#[test]
fn test_calculate_statistics() {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    // Insert some tasks
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('US-001', 'Done Task', 'done')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('US-002', 'Todo Task', 'todo')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES ('US-003', 'Blocked Task', 'blocked')",
        [],
    )
    .unwrap();

    let stats = calculate_statistics(&conn).unwrap();

    assert_eq!(stats.total_tasks, 3);
    assert_eq!(stats.completed_tasks, 1);
    assert_eq!(stats.pending_tasks, 1);
    assert_eq!(stats.blocked_tasks, 1);
    assert!((stats.completion_percentage - 33.333).abs() < 0.01);
}

// ============================================================================
// Model selection round-trip tests (parse -> import -> export)
// ============================================================================

#[test]
fn test_round_trip_preserves_model_fields() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "round-trip-test",
        "model": "claude-sonnet-4-6",
        "userStories": [
            {
                "id": "US-001",
                "title": "Task with model",
                "priority": 1,
                "passes": false,
                "model": "claude-opus-4-6",
                "difficulty": "high",
                "escalationNote": "Bumped after compile failure"
            },
            {
                "id": "US-002",
                "title": "Task without model",
                "priority": 2,
                "passes": false
            }
        ]
    }"#;

    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, json).unwrap();

    // Import
    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    // Export
    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    // Re-parse exported JSON
    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    // Verify PRD-level model preserved
    assert_eq!(
        exported.model,
        Some("claude-sonnet-4-6".to_string()),
        "model should survive round-trip"
    );

    // Verify US-001 model fields preserved
    let us001 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-001")
        .expect("US-001 should be exported");
    assert_eq!(us001.model, Some("claude-opus-4-6".to_string()));
    assert_eq!(us001.difficulty, Some("high".to_string()));
    assert_eq!(
        us001.escalation_note,
        Some("Bumped after compile failure".to_string())
    );

    // Verify US-002 has None for model fields
    let us002 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-002")
        .expect("US-002 should be exported");
    assert_eq!(us002.model, None);
    assert_eq!(us002.difficulty, None);
    assert_eq!(us002.escalation_note, None);
}

#[test]
fn test_round_trip_model_fields_omitted_from_json_when_none() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "omission-test",
        "userStories": [
            {"id": "US-001", "title": "Plain task", "priority": 1, "passes": false}
        ]
    }"#;

    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, json).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    // Verify that None fields are omitted from JSON output (skip_serializing_if)
    let exported_json = fs::read_to_string(&export_path).unwrap();
    assert!(
        !exported_json.contains("\"model\""),
        "model:null should be omitted from JSON"
    );
    assert!(
        !exported_json.contains("\"difficulty\""),
        "difficulty:null should be omitted from JSON"
    );
    assert!(
        !exported_json.contains("\"escalationNote\""),
        "escalationNote:null should be omitted from JSON"
    );
    assert!(
        !exported_json.contains("\"defaultModel\""),
        "defaultModel:null should be omitted from JSON"
    );
}

#[test]
fn test_export_preserves_model_fields_in_json_format() {
    let temp_dir = TempDir::new().unwrap();
    let json = r#"{
        "project": "json-format-test",
        "model": "claude-haiku-4-5-20251001",
        "userStories": [
            {
                "id": "US-001",
                "title": "Formatted task",
                "priority": 1,
                "passes": false,
                "model": "claude-sonnet-4-6",
                "difficulty": "medium",
                "escalationNote": "Test note"
            }
        ]
    }"#;

    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, json).unwrap();

    init::init(
        temp_dir.path(),
        &[&json_path],
        false,
        false,
        false,
        false,
        PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    // Verify JSON uses camelCase keys (from serde rename_all)
    let exported_json = fs::read_to_string(&export_path).unwrap();
    assert!(
        exported_json.contains("\"escalationNote\""),
        "exported JSON should use camelCase escalationNote"
    );
    assert!(
        exported_json.contains("\"model\""),
        "exported JSON should contain model field"
    );
}

// ========== TEST-INIT-001: retired_at Filtering Tests ==========
//
// Tests verify retired learnings are excluded from export queries.
// All tests are #[ignore] until FEAT-001 and FEAT-002 are implemented.
//
// Query locations covered:
//  10. Export load_learnings (progress::load_learnings)
//  11. Export calculate_statistics (progress::calculate_statistics)

use crate::learnings::test_helpers::retire_learning as retire_learning_export;

#[test]
fn test_retired_excluded_from_export_load_learnings() {
    // AC: retired learning excluded from export load_learnings query
    use crate::learnings::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let temp_dir = TempDir::new().unwrap();
    let mut conn = crate::db::open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    // Insert a retired learning
    let params = RecordLearningParams {
        outcome: LearningOutcome::Success,
        title: "Retired export target".to_string(),
        content: "Should not appear in export".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Medium,
    };
    let result = record_learning(&conn, params).unwrap();
    retire_learning_export(&conn, result.learning_id);

    // Insert an active learning
    let active_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "Active export learning".to_string(),
        content: "Should appear in export".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    record_learning(&conn, active_params).unwrap();

    let learnings = progress::load_learnings(&conn).unwrap();

    assert_eq!(
        learnings.len(),
        1,
        "load_learnings must exclude retired learning"
    );
    assert_eq!(
        learnings[0].title, "Active export learning",
        "only the active learning must appear in export"
    );
}

#[test]
fn test_retired_excluded_from_export_calculate_statistics() {
    // AC: retired learning excluded from export calculate_statistics outcome counts
    use crate::learnings::{record_learning, RecordLearningParams};
    use crate::models::{Confidence, LearningOutcome};

    let temp_dir = TempDir::new().unwrap();
    let mut conn = crate::db::open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    // One active success learning
    let params = RecordLearningParams {
        outcome: LearningOutcome::Success,
        title: "Active stat".to_string(),
        content: "Should be counted".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::High,
    };
    record_learning(&conn, params).unwrap();

    // One retired failure learning (must NOT be counted)
    let retired_params = RecordLearningParams {
        outcome: LearningOutcome::Failure,
        title: "Retired stat".to_string(),
        content: "Must not be counted".to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: None,
        applies_to_task_types: None,
        applies_to_errors: None,
        tags: None,
        confidence: Confidence::Low,
    };
    let retired_result = record_learning(&conn, retired_params).unwrap();
    retire_learning_export(&conn, retired_result.learning_id);

    let stats = calculate_statistics(&conn).unwrap();

    assert_eq!(
        stats.total_learnings, 1,
        "calculate_statistics total must exclude retired (expected 1, got {})",
        stats.total_learnings
    );
    // Failure count must be 0 since the only failure is retired
    assert_eq!(
        stats.learnings_by_outcome.failures, 0,
        "retired failure learning must not be counted in calculate_statistics outcome breakdown"
    );
}
