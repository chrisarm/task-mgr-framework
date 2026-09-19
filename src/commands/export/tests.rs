//! Tests for the export module.

use super::*;
use crate::commands::init;
use crate::commands::init::PrefixMode;
use crate::db::{create_schema, run_migrations};
use crate::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
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

    // Only dependsOn survives export; synergy/batch/conflicts were removed.
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
    let json = format!(
        r#"{{
        "project": "round-trip-test",
        "model": "{SONNET_MODEL}",
        "userStories": [
            {{
                "id": "US-001",
                "title": "Task with model",
                "priority": 1,
                "passes": false,
                "model": "{OPUS_MODEL}",
                "difficulty": "high",
                "escalationNote": "Bumped after compile failure"
            }},
            {{
                "id": "US-002",
                "title": "Task without model",
                "priority": 2,
                "passes": false
            }}
        ]
    }}"#
    );

    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, &json).unwrap();

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
        Some(SONNET_MODEL.to_string()),
        "model should survive round-trip"
    );

    // Verify US-001 model fields preserved
    let us001 = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-001")
        .expect("US-001 should be exported");
    assert_eq!(us001.model, Some(OPUS_MODEL.to_string()));
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
    let json = format!(
        r#"{{
        "project": "json-format-test",
        "model": "{HAIKU_MODEL}",
        "userStories": [
            {{
                "id": "US-001",
                "title": "Formatted task",
                "priority": 1,
                "passes": false,
                "model": "{SONNET_MODEL}",
                "difficulty": "medium",
                "escalationNote": "Test note"
            }}
        ]
    }}"#
    );

    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, &json).unwrap();

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
    use crate::learnings::{RecordLearningParams, record_learning};
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
    use crate::learnings::{RecordLearningParams, record_learning};
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

// ============ requires_human / human_review_timeout export tests ============

/// ExportedUserStory with requires_human=None must not include the field in JSON.
#[test]
fn test_exported_story_requires_human_absent_when_none() {
    let story = prd::ExportedUserStory {
        id: "US-001".to_string(),
        title: "Test".to_string(),
        description: None,
        priority: 1,
        passes: false,
        notes: None,
        acceptance_criteria: vec![],
        review_scope: None,
        severity: None,
        source_review: None,
        touches_files: vec![],
        depends_on: vec![],
        model: None,
        difficulty: None,
        escalation_note: None,
        max_retries: 3,
        requires_human: None,
        human_review_timeout: None,
        completed_by_provider: None,
    };
    let json = serde_json::to_string(&story).unwrap();
    assert!(
        !json.contains("requiresHuman"),
        "requiresHuman must be absent from JSON when None"
    );
    assert!(
        !json.contains("humanReviewTimeout"),
        "humanReviewTimeout must be absent from JSON when None"
    );
}

/// ExportedUserStory with requires_human=Some(true) must include requiresHuman:true in JSON.
#[test]
fn test_exported_story_requires_human_true_in_json() {
    let story = prd::ExportedUserStory {
        id: "US-001".to_string(),
        title: "Test".to_string(),
        description: None,
        priority: 1,
        passes: false,
        notes: None,
        acceptance_criteria: vec![],
        review_scope: None,
        severity: None,
        source_review: None,
        touches_files: vec![],
        depends_on: vec![],
        model: None,
        difficulty: None,
        escalation_note: None,
        max_retries: 3,
        requires_human: Some(true),
        human_review_timeout: None,
        completed_by_provider: None,
    };
    let json = serde_json::to_string(&story).unwrap();
    assert!(
        json.contains("\"requiresHuman\":true"),
        "requiresHuman:true must be in JSON when Some(true)"
    );
}

/// ExportedUserStory with human_review_timeout=Some(60) must include humanReviewTimeout:60.
#[test]
fn test_exported_story_human_review_timeout_in_json() {
    let story = prd::ExportedUserStory {
        id: "US-001".to_string(),
        title: "Test".to_string(),
        description: None,
        priority: 1,
        passes: false,
        notes: None,
        acceptance_criteria: vec![],
        review_scope: None,
        severity: None,
        source_review: None,
        touches_files: vec![],
        depends_on: vec![],
        model: None,
        difficulty: None,
        escalation_note: None,
        max_retries: 3,
        requires_human: Some(true),
        human_review_timeout: Some(60),
        completed_by_provider: None,
    };
    let json = serde_json::to_string(&story).unwrap();
    assert!(
        json.contains("\"humanReviewTimeout\":60"),
        "humanReviewTimeout:60 must be in JSON when Some(60)"
    );
}

/// ExportedUserStory JSON round-trip: requires_human and human_review_timeout survive serde.
#[test]
fn test_exported_story_requires_human_round_trip() {
    let story = prd::ExportedUserStory {
        id: "US-001".to_string(),
        title: "Test".to_string(),
        description: None,
        priority: 1,
        passes: false,
        notes: None,
        acceptance_criteria: vec![],
        review_scope: None,
        severity: None,
        source_review: None,
        touches_files: vec![],
        depends_on: vec![],
        model: None,
        difficulty: None,
        escalation_note: None,
        max_retries: 3,
        requires_human: Some(true),
        human_review_timeout: Some(120),
        completed_by_provider: None,
    };
    let json = serde_json::to_string(&story).unwrap();
    let deserialized: prd::ExportedUserStory = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.requires_human, Some(true));
    assert_eq!(deserialized.human_review_timeout, Some(120));
}

/// Full import → export round-trip preserves requiresHuman and humanReviewTimeout.
/// Requires v15 DB columns — v15 migration is implemented, columns exist.
#[test]
fn test_export_round_trips_requires_human_field() {
    let prd_json = r#"{
        "project": "test-project",
        "userStories": [
            {
                "id": "US-001",
                "title": "Human Review Gate",
                "priority": 1,
                "passes": false,
                "requiresHuman": true,
                "humanReviewTimeout": 60
            }
        ]
    }"#;

    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, prd_json).unwrap();

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
    let exported: prd::ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    let story = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-001")
        .unwrap();
    assert_eq!(story.requires_human, Some(true));
    assert_eq!(story.human_review_timeout, Some(60));
}

// ============ completed_by_provider export tests (v20) ============

/// ExportedUserStory with completed_by_provider=None must omit the field from JSON.
#[test]
fn test_exported_story_completed_by_provider_absent_when_none() {
    let story = prd::ExportedUserStory {
        id: "US-001".to_string(),
        title: "Test".to_string(),
        description: None,
        priority: 1,
        passes: false,
        notes: None,
        acceptance_criteria: vec![],
        review_scope: None,
        severity: None,
        source_review: None,
        touches_files: vec![],
        depends_on: vec![],
        model: None,
        difficulty: None,
        escalation_note: None,
        max_retries: 3,
        requires_human: None,
        human_review_timeout: None,
        completed_by_provider: None,
    };
    let json = serde_json::to_string(&story).unwrap();
    assert!(
        !json.contains("completedByProvider"),
        "completedByProvider must be absent from JSON when None; got: {json}"
    );
}

/// ExportedUserStory with completed_by_provider=Some("claude") must include the field in JSON.
#[test]
fn test_exported_story_completed_by_provider_present_in_json() {
    let story = prd::ExportedUserStory {
        id: "US-001".to_string(),
        title: "Test".to_string(),
        description: None,
        priority: 1,
        passes: true,
        notes: None,
        acceptance_criteria: vec![],
        review_scope: None,
        severity: None,
        source_review: None,
        touches_files: vec![],
        depends_on: vec![],
        model: None,
        difficulty: None,
        escalation_note: None,
        max_retries: 3,
        requires_human: None,
        human_review_timeout: None,
        completed_by_provider: Some("claude".to_string()),
    };
    let json = serde_json::to_string(&story).unwrap();
    assert!(
        json.contains("\"completedByProvider\":\"claude\""),
        "completedByProvider:claude must be in JSON when Some(\"claude\"); got: {json}"
    );
}

/// Full DB round-trip: stamp completed_by_provider → export → JSON contains the field.
/// Historical rows (NULL) hydrate without error.
#[test]
fn test_export_round_trips_completed_by_provider() {
    use crate::commands::init;
    use crate::db::open_and_migrate;

    let prd_json = r#"{
        "project": "test-project",
        "userStories": [
            {
                "id": "US-001",
                "title": "Stamped Task",
                "priority": 1,
                "passes": false
            },
            {
                "id": "US-002",
                "title": "Unstamped Task",
                "priority": 2,
                "passes": false
            }
        ]
    }"#;

    let temp_dir = TempDir::new().unwrap();
    let json_path = temp_dir.path().join("prd.json");
    fs::write(&json_path, prd_json).unwrap();

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

    // Simulate the stamping that process_iteration_output does.
    let conn = open_and_migrate(temp_dir.path()).unwrap();
    conn.execute(
        "UPDATE tasks SET completed_by_provider = 'claude' WHERE id = 'US-001'",
        [],
    )
    .unwrap();
    drop(conn);

    let export_path = temp_dir.path().join("exported.json");
    export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: prd::ExportedPrd = serde_json::from_str(&exported_json).unwrap();

    let stamped = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-001")
        .unwrap();
    assert_eq!(
        stamped.completed_by_provider.as_deref(),
        Some("claude"),
        "stamped task must export completed_by_provider=claude"
    );

    // Historical NULL row hydrates without error and emits nothing.
    let unstamped = exported
        .user_stories
        .iter()
        .find(|s| s.id == "US-002")
        .unwrap();
    assert_eq!(
        unstamped.completed_by_provider, None,
        "unstamped task must export completed_by_provider=None"
    );
    assert!(
        !exported_json.contains("\"id\":\"US-002\",")
            || !exported_json.contains("completedByProvider"),
        "completedByProvider must not appear in JSON for NULL row"
    );
}

// ============================================================================
// FEAT-001: scoped load_tasks / load_prd_metadata + identity prd_id
// ============================================================================

fn seed_task(conn: &rusqlite::Connection, id: &str, title: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status) VALUES (?, ?, 'todo')",
        rusqlite::params![id, title],
    )
    .unwrap();
}

fn seed_prd_meta(
    conn: &rusqlite::Connection,
    id: i64,
    project: &str,
    branch: Option<&str>,
    task_prefix: Option<&str>,
) {
    conn.execute(
        "INSERT INTO prd_metadata (id, project, branch_name, task_prefix) VALUES (?, ?, ?, ?)",
        rusqlite::params![id, project, branch, task_prefix],
    )
    .unwrap();
}

fn seed_task_list_file(conn: &rusqlite::Connection, prd_id: i64, file_path: &std::path::Path) {
    conn.execute(
        "INSERT INTO prd_files (prd_id, file_path, file_type) VALUES (?, ?, 'task_list')",
        rusqlite::params![prd_id, file_path.to_str().unwrap()],
    )
    .unwrap();
}

#[test]
fn test_load_tasks_scoped_two_prefixes() {
    // Dual-PRD fixtures need unique story ids (learning #5605).
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    seed_prd_meta(&conn, 1, "project-b", Some("branch-b"), Some("B"));
    seed_prd_meta(&conn, 2, "project-a", Some("branch-a"), Some("A"));
    seed_task(&conn, "A-001", "A task");
    seed_task(&conn, "A-002", "A task 2");
    seed_task(&conn, "B-001", "B task");

    let scoped = prd::load_tasks(&conn, Some("A")).unwrap();
    assert_eq!(scoped.len(), 2);
    assert!(scoped.iter().all(|t| t.id.starts_with("A-")));
    assert!(scoped.iter().all(|t| !t.id.starts_with("B-")));

    let meta_a = prd::load_prd_metadata(&conn, prd::MetadataScope::NamedPrefix("A")).unwrap();
    assert_eq!(meta_a.project, "project-a");
    assert_eq!(meta_a.branch_name.as_deref(), Some("branch-a"));
}

#[test]
fn test_load_prd_metadata_by_prd_id_two_no_prefix() {
    // Two --no-prefix inits: LIMIT 1 / IS NULL would stamp the wrong row.
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    let file_a = temp_dir.path().join("a.json");
    let file_b = temp_dir.path().join("b.json");
    fs::write(&file_a, r#"{"project":"proj-a","userStories":[]}"#).unwrap();
    fs::write(&file_b, r#"{"project":"proj-b","userStories":[]}"#).unwrap();
    let canon_b = file_b.canonicalize().unwrap();

    // A gets lower id so LIMIT 1 would pick A.
    seed_prd_meta(&conn, 1, "proj-a", Some("branch-a"), None);
    seed_prd_meta(&conn, 2, "proj-b", Some("branch-b"), None);
    seed_task_list_file(&conn, 1, &file_a.canonicalize().unwrap());
    seed_task_list_file(&conn, 2, &canon_b);

    let hit = crate::commands::context::find_registered_by_path_identity(
        &conn,
        &canon_b,
        Some(temp_dir.path()),
        Some(temp_dir.path()),
    )
    .unwrap()
    .expect("file B must identity-match");
    assert_eq!(hit.0, 2);
    assert_eq!(hit.1, None);

    let meta_b = prd::load_prd_metadata(&conn, prd::MetadataScope::ByPrdId(hit.0)).unwrap();
    assert_eq!(meta_b.project, "proj-b");
    assert_eq!(meta_b.branch_name.as_deref(), Some("branch-b"));

    // Unscoped still LIMIT 1 → A's row.
    let unscoped = prd::load_prd_metadata(&conn, prd::MetadataScope::Unscoped).unwrap();
    assert_eq!(unscoped.project, "proj-a");
}

#[test]
fn test_load_all_helpers_two_prefix_db() {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    seed_prd_meta(&conn, 1, "project-a", Some("branch-a"), Some("A"));
    seed_prd_meta(&conn, 2, "project-b", Some("branch-b"), Some("B"));
    seed_task(&conn, "A-001", "A");
    seed_task(&conn, "B-001", "B");
    seed_task(&conn, "B-002", "B2");

    let all = prd::load_tasks(&conn, None).unwrap();
    assert_eq!(all.len(), 3, "--all helpers: task count = A+B unarchived");

    let meta = prd::load_prd_metadata(&conn, prd::MetadataScope::Unscoped).unwrap();
    assert_eq!(
        meta.project, "project-a",
        "--all metadata stays ORDER BY id LIMIT 1"
    );
}

#[test]
fn test_load_tasks_excludes_archived_scoped_and_all() {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    seed_task(&conn, "A-001", "live");
    seed_task(&conn, "A-002", "archived");
    seed_task(&conn, "B-001", "other");
    conn.execute(
        "UPDATE tasks SET archived_at = datetime('now') WHERE id = 'A-002'",
        [],
    )
    .unwrap();

    let scoped = prd::load_tasks(&conn, Some("A")).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].id, "A-001");

    let all = prd::load_tasks(&conn, None).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|t| t.id != "A-002"));
}

#[test]
fn test_load_tasks_prefix_underscore_uses_make_like_pattern() {
    // Naive format!("{prefix}-%") for P_1 matches P11 / PA1 via LIKE '_'.
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    seed_task(&conn, "P_1-001", "literal underscore");
    seed_task(&conn, "P11-001", "would match naive");
    seed_task(&conn, "PA1-001", "would also match naive");

    assert_eq!(
        crate::db::prefix::make_like_pattern("P_1"),
        "P\\_1-%",
        "must escape underscore"
    );

    let scoped = prd::load_tasks(&conn, Some("P_1")).unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].id, "P_1-001");
}

#[test]
fn test_load_tasks_empty_string_treated_as_none() {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    seed_task(&conn, "A-001", "a");
    seed_task(&conn, "B-001", "b");

    let empty = prd::load_tasks(&conn, Some("")).unwrap();
    let none = prd::load_tasks(&conn, None).unwrap();
    assert_eq!(empty.len(), 2, "empty string must not LIKE '-%'");
    assert_eq!(empty.len(), none.len());
}

#[test]
fn test_load_tasks_trailing_dash_fe_vs_feat() {
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    seed_task(&conn, "FE-001", "fe");
    seed_task(&conn, "FEAT-001", "feat");

    let fe = prd::load_tasks(&conn, Some("FE")).unwrap();
    assert_eq!(fe.len(), 1);
    assert_eq!(fe[0].id, "FE-001");
}

#[test]
fn test_exported_prd_has_no_task_prefix_field_and_serde_round_trips() {
    let prd = ExportedPrd {
        project: "p".to_string(),
        branch_name: Some("main".to_string()),
        description: None,
        priority_philosophy: None,
        global_acceptance_criteria: None,
        review_guidelines: None,
        model: None,
        default_max_retries: None,
        user_stories: vec![],
    };

    let pretty = serde_json::to_string_pretty(&prd).unwrap();
    assert!(
        !pretty.contains("taskPrefix") && !pretty.contains("task_prefix"),
        "ExportedPrd must stay lossy (no taskPrefix): {pretty}"
    );

    let value = serde_json::to_value(&prd).unwrap();
    let back: ExportedPrd = serde_json::from_value(value).unwrap();
    assert_eq!(back.project, "p");
    assert_eq!(back.branch_name.as_deref(), Some("main"));

    // Do not round-trip through PrdUserStory — Value / ExportedPrd only.
    let _again: ExportedPrd = serde_json::from_str(&pretty).unwrap();
}

#[test]
fn test_export_dir_still_four_arg_dump_all() {
    // Arity / dump-all call sites: two prefixes still dump everything via export().
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    drop(conn);

    let a = temp_dir.path().join("a.json");
    let b = temp_dir.path().join("b.json");
    fs::write(
        &a,
        r#"{"project":"proj-a","branchName":"ba","userStories":[
            {"id":"A-STORY-001","title":"A","priority":1,"passes":false}
        ]}"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"{"project":"proj-b","branchName":"bb","userStories":[
            {"id":"B-STORY-001","title":"B","priority":1,"passes":false}
        ]}"#,
    )
    .unwrap();

    init::init(
        temp_dir.path(),
        &[&a],
        false,
        false,
        false,
        false,
        PrefixMode::Explicit("A".into()),
    )
    .unwrap();
    init::init(
        temp_dir.path(),
        &[&b],
        false,
        true, // append — keep both prefixes in one DB
        false,
        false,
        PrefixMode::Explicit("B".into()),
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    let result = export(temp_dir.path(), &export_path, false, None).unwrap();
    assert_eq!(result.tasks_exported, 2);

    let exported: ExportedPrd =
        serde_json::from_str(&fs::read_to_string(&export_path).unwrap()).unwrap();
    // Unscoped LIMIT 1 → first registered metadata (A).
    assert_eq!(exported.project, "proj-a");
    let ids: Vec<_> = exported
        .user_stories
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert!(ids.iter().any(|id| id.starts_with("A-")));
    assert!(ids.iter().any(|id| id.starts_with("B-")));
}

#[test]
fn test_export_module_has_no_task_prefix_is_null() {
    // Grep invariant: export/ must not use WHERE task_prefix IS NULL.
    let export_src = concat!(
        include_str!("prd.rs"),
        include_str!("mod.rs"),
        include_str!("progress.rs"),
    );
    let lowered = export_src.to_ascii_lowercase();
    assert!(
        !lowered.contains("task_prefix is null"),
        "export/ must not use WHERE task_prefix IS NULL"
    );
}
