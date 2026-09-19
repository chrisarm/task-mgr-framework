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
    let result = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    let result = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: true,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    let result = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: Some(&learnings_path),
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    let result = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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

    // unique_tmp_path temps are renamed away; no leftover .*.tmp beside dest.
    let parent = path.parent().unwrap();
    let leftovers: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no leftover tmp files after atomic write: {leftovers:?}"
    );
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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();

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
fn test_export_all_two_prefixes_dump_all() {
    // all: true → today's dump even with two prefixes (no active error).
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
    let result = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &export_path,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap();
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

// ---------------------------------------------------------------------------
// FEAT-002: ExportOpts + overwrite-guard + scope selection
// ---------------------------------------------------------------------------

const ACTIVE_PREFIX_ENV: &str = "TASK_MGR_ACTIVE_PREFIX";

/// Clears leaked loop `TASK_MGR_ACTIVE_PREFIX` for resolve_context-sensitive tests.
/// Uses crate-level `ENV_PREFIX_MUTEX` so export/current/add/context tests serialize together.
struct EnvIsolation {
    _lock: std::sync::MutexGuard<'static, ()>,
    prior: Option<String>,
}

impl EnvIsolation {
    fn new() -> Self {
        let lock = crate::ENV_PREFIX_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var(ACTIVE_PREFIX_ENV).ok();
        unsafe { std::env::remove_var(ACTIVE_PREFIX_ENV) };
        Self { _lock: lock, prior }
    }
}

impl Drop for EnvIsolation {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => unsafe { std::env::set_var(ACTIVE_PREFIX_ENV, v) },
            None => unsafe { std::env::remove_var(ACTIVE_PREFIX_ENV) },
        }
    }
}

fn opts_all(to_json: &std::path::Path) -> ExportOpts<'_> {
    ExportOpts {
        to_json,
        with_progress: false,
        learnings_file: None,
        from_json: None,
        all: true,
        force: false,
    }
}

fn seed_two_prefix_db(temp_dir: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    drop(conn);

    let a = temp_dir.path().join("a.json");
    let b = temp_dir.path().join("b.json");
    fs::write(
        &a,
        r#"{"project":"proj-a","branchName":"ba","extraKeepMe":true,"userStories":[
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
        true,
        false,
        false,
        PrefixMode::Explicit("B".into()),
    )
    .unwrap();
    (a, b)
}

#[test]
fn test_export_registered_dest_without_force_refuses_bytes_identical() {
    let temp_dir = TempDir::new().unwrap();
    let (a, _b) = seed_two_prefix_db(&temp_dir);
    let before = fs::read(&a).unwrap();

    let err = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &a,
            with_progress: false,
            learnings_file: None,
            from_json: Some(&a),
            all: false,
            force: false,
        },
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("--force") && msg.to_lowercase().contains("dump"),
        "refuse must name --force and dump-not-merge: {msg}"
    );
    assert_eq!(
        fs::read(&a).unwrap(),
        before,
        "dest bytes must be identical"
    );
}

#[test]
fn test_export_registered_dest_with_force_replaces_lossy() {
    let temp_dir = TempDir::new().unwrap();
    let (a, _b) = seed_two_prefix_db(&temp_dir);
    // Seed an extra key that must disappear after --force dump.
    let seeded = r#"{"project":"proj-a","branchName":"ba","extraKeepMe":true,"taskPrefix":"A","userStories":[]}"#;
    fs::write(&a, seeded).unwrap();

    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &a,
            with_progress: false,
            learnings_file: None,
            from_json: Some(&a),
            all: false,
            force: true,
        },
    )
    .unwrap();

    let after = fs::read_to_string(&a).unwrap();
    assert!(
        !after.contains("extraKeepMe"),
        "force dump must not preserve extra keys: {after}"
    );
    assert!(
        !after.contains("taskPrefix"),
        "ExportedPrd must stay lossy (no taskPrefix): {after}"
    );
    let exported: ExportedPrd = serde_json::from_str(&after).unwrap();
    assert_eq!(exported.project, "proj-a");
    assert_eq!(exported.user_stories.len(), 1);
}

#[test]
fn test_export_all_onto_registered_still_requires_force() {
    let temp_dir = TempDir::new().unwrap();
    let (a, _b) = seed_two_prefix_db(&temp_dir);
    let before = fs::read(&a).unwrap();

    let err = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &a,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: true,
            force: false,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("--force"));
    assert_eq!(fs::read(&a).unwrap(), before);
}

#[test]
fn test_export_missing_dest_creates_without_force() {
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

    let dest = temp_dir.path().join("new-export.json");
    assert!(!dest.exists());
    export(temp_dir.path(), &opts_all(&dest)).unwrap();
    assert!(dest.is_file());
}

#[test]
fn test_export_identity_miss_existing_file_writes_without_force() {
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

    // Unregistered existing file (identity miss) — lock then write, no force.
    let dump = temp_dir.path().join("dump.json");
    fs::write(&dump, r#"{"scratch":true}"#).unwrap();
    export(temp_dir.path(), &opts_all(&dump)).unwrap();
    let after = fs::read_to_string(&dump).unwrap();
    assert!(!after.contains("scratch"));
    let exported: ExportedPrd = serde_json::from_str(&after).unwrap();
    assert_eq!(exported.user_stories.len(), 2);
}

#[test]
fn test_export_directory_dest_errors() {
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

    let dir_dest = temp_dir.path().join("outdir");
    fs::create_dir(&dir_dest).unwrap();
    let err = export(temp_dir.path(), &opts_all(&dir_dest)).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("directory"),
        "directory dest must error: {err}"
    );
}

#[test]
fn test_export_no_active_prd_errors_naming_flags() {
    let _env = EnvIsolation::new();
    let temp_dir = TempDir::new().unwrap();
    let (_a, _b) = seed_two_prefix_db(&temp_dir);
    let dest = temp_dir.path().join("out.json");
    let err = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &dest,
            with_progress: false,
            learnings_file: None,
            from_json: None,
            all: false,
            force: false,
        },
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("--from-json") && msg.contains("--all") && msg.contains("task-mgr current"),
        "no-active must name --from-json / --all / task-mgr current: {msg}"
    );
    assert!(!dest.exists(), "refuse must not create dest");
}

#[test]
fn test_export_from_json_scopes_to_pin_prefix() {
    let temp_dir = TempDir::new().unwrap();
    let (a, _b) = seed_two_prefix_db(&temp_dir);
    let dest = temp_dir.path().join("scoped.json");
    let result = export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &dest,
            with_progress: false,
            learnings_file: None,
            from_json: Some(&a),
            all: false,
            force: false,
        },
    )
    .unwrap();
    assert_eq!(result.tasks_exported, 1);
    let exported: ExportedPrd = serde_json::from_str(&fs::read_to_string(&dest).unwrap()).unwrap();
    assert_eq!(exported.project, "proj-a");
    assert!(
        exported.user_stories.iter().all(|s| s.id.starts_with("A-")),
        "from-json pin must dump only A: {:?}",
        exported.user_stories
    );
}

#[test]
fn test_export_empty_prefix_from_json_uses_by_prd_id() {
    // Do NOT use two PrefixMode::Disabled inits — insert_prd_metadata's
    // `SELECT … WHERE task_prefix IS NULL` collapses twins (prohibited proof).
    // Seed distinct NULL-prefix rows + prd_files like FEAT-001.
    let _env = EnvIsolation::new();
    let temp_dir = TempDir::new().unwrap();
    let mut conn = open_connection(temp_dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();

    let a = temp_dir.path().join("empty-a.json");
    let b = temp_dir.path().join("empty-b.json");
    fs::write(
        &a,
        r#"{"project":"empty-a","branchName":"ba","userStories":[]}"#,
    )
    .unwrap();
    fs::write(
        &b,
        r#"{"project":"empty-b","branchName":"bb","userStories":[]}"#,
    )
    .unwrap();
    let canon_a = a.canonicalize().unwrap();
    let canon_b = b.canonicalize().unwrap();

    seed_prd_meta(&conn, 1, "empty-a", Some("ba"), None);
    seed_prd_meta(&conn, 2, "empty-b", Some("bb"), None);
    seed_task_list_file(&conn, 1, &canon_a);
    seed_task_list_file(&conn, 2, &canon_b);
    seed_task(&conn, "US-001", "A");
    seed_task(&conn, "US-002", "B");
    drop(conn);

    let dest = temp_dir.path().join("pin-b.json");
    export(
        temp_dir.path(),
        &ExportOpts {
            to_json: &dest,
            with_progress: false,
            learnings_file: None,
            from_json: Some(&b),
            all: false,
            force: false,
        },
    )
    .unwrap();

    let exported: ExportedPrd = serde_json::from_str(&fs::read_to_string(&dest).unwrap()).unwrap();
    // Empty-prefix: all unarchived tasks; metadata = B via identity prd_id.
    assert_eq!(exported.project, "empty-b");
    assert_eq!(exported.branch_name.as_deref(), Some("bb"));
    assert_eq!(exported.user_stories.len(), 2);
}

#[test]
fn test_export_module_grep_invariants() {
    let export_src = concat!(
        include_str!("prd.rs"),
        include_str!("mod.rs"),
        include_str!("progress.rs"),
    );
    assert!(
        !export_src.contains("cli_write_path"),
        "export/ must not call cli_write_path"
    );
    assert!(
        !export_src.contains("preflight_from_json_path"),
        "export/ must not call preflight_from_json_path"
    );
    assert!(
        !export_src.contains("invalid_state(\n            \"add\"")
            && !export_src.contains("invalid_state(\"add\""),
        "export/ must not use invalid_state command-name \"add\""
    );
    assert!(
        !export_src.contains("with_extension(\"json.tmp\")"),
        "write_json_atomic must use unique_tmp_path, not with_extension(json.tmp)"
    );
    assert!(
        export_src.contains("unique_tmp_path"),
        "write_json_atomic must use prd_json::unique_tmp_path"
    );
    assert!(
        !export_src.contains("commands::add") && !export_src.contains("commands::update"),
        "export must not import add/update"
    );
}
