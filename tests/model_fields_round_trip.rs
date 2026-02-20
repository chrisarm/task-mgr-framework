//! Integration tests for model/difficulty/escalationNote round-trip fidelity.
//!
//! Tests the full pipeline: parse JSON -> import to DB -> export -> verify.

use serde_json::Value;
use std::fs;
use tempfile::TempDir;

use task_mgr::commands::{export, init};
use task_mgr::db::open_connection;

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn import_and_export(fixture_name: &str) -> (TempDir, Value) {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path(fixture_name);

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    let export_path = temp_dir.path().join("exported.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    let exported_json = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&exported_json).unwrap();

    (temp_dir, exported)
}

fn get_story<'a>(stories: &'a [Value], id: &str) -> &'a Value {
    stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))
        .unwrap_or_else(|| panic!("Story {} not found in exported JSON", id))
}

// ========== Round-trip: all model fields populated ==========

#[test]
fn test_round_trip_all_model_fields() {
    let (_temp_dir, exported) = import_and_export("prd_with_all_model_fields.json");

    // Verify top-level model field round-trips
    assert_eq!(
        exported.get("model").and_then(|v| v.as_str()),
        Some("claude-sonnet-4-6"),
        "Top-level model should round-trip"
    );

    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("userStories should be present");

    // MT-001: explicit opus model + high difficulty + escalation note
    let mt001 = get_story(stories, "MT-001");
    assert_eq!(
        mt001.get("model").and_then(|v| v.as_str()),
        Some("claude-opus-4-6"),
        "MT-001 model should round-trip"
    );
    assert_eq!(
        mt001.get("difficulty").and_then(|v| v.as_str()),
        Some("high"),
        "MT-001 difficulty should round-trip"
    );
    assert_eq!(
        mt001.get("escalationNote").and_then(|v| v.as_str()),
        Some("Needs opus for complex refactor"),
        "MT-001 escalationNote should round-trip"
    );

    // MT-002: haiku model + low difficulty + escalation note
    let mt002 = get_story(stories, "MT-002");
    assert_eq!(
        mt002.get("model").and_then(|v| v.as_str()),
        Some("claude-haiku-4-5-20251001"),
        "MT-002 model should round-trip"
    );
    assert_eq!(
        mt002.get("difficulty").and_then(|v| v.as_str()),
        Some("low"),
        "MT-002 difficulty should round-trip"
    );
    assert_eq!(
        mt002.get("escalationNote").and_then(|v| v.as_str()),
        Some("Simple task, haiku is fine"),
        "MT-002 escalationNote should round-trip"
    );
}

// ========== Round-trip: partial model overrides ==========

#[test]
fn test_round_trip_partial_model_overrides() {
    let (_temp_dir, exported) = import_and_export("prd_with_partial_model_fields.json");

    // Top-level model present
    assert_eq!(
        exported.get("model").and_then(|v| v.as_str()),
        Some("claude-sonnet-4-6"),
        "Top-level model should be present"
    );

    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("userStories should be present");

    // PM-001: has model and difficulty
    let pm001 = get_story(stories, "PM-001");
    assert_eq!(
        pm001.get("model").and_then(|v| v.as_str()),
        Some("claude-opus-4-6"),
        "PM-001 should have model"
    );
    assert_eq!(
        pm001.get("difficulty").and_then(|v| v.as_str()),
        Some("high"),
        "PM-001 should have difficulty"
    );
    // PM-001 has no escalationNote -> should be absent from JSON
    assert!(
        pm001.get("escalationNote").is_none(),
        "PM-001 escalationNote should be omitted when NULL"
    );

    // PM-002: no model fields at all -> all should be absent
    let pm002 = get_story(stories, "PM-002");
    assert!(
        pm002.get("model").is_none(),
        "PM-002 model should be omitted when NULL"
    );
    assert!(
        pm002.get("difficulty").is_none(),
        "PM-002 difficulty should be omitted when NULL"
    );
    assert!(
        pm002.get("escalationNote").is_none(),
        "PM-002 escalationNote should be omitted when NULL"
    );

    // PM-003: only escalationNote, no model or difficulty
    let pm003 = get_story(stories, "PM-003");
    assert!(
        pm003.get("model").is_none(),
        "PM-003 model should be omitted when NULL"
    );
    assert!(
        pm003.get("difficulty").is_none(),
        "PM-003 difficulty should be omitted when NULL"
    );
    assert_eq!(
        pm003.get("escalationNote").and_then(|v| v.as_str()),
        Some("Escalated due to prior failure"),
        "PM-003 escalationNote should round-trip"
    );
}

// ========== Round-trip: no model fields (backward compat) ==========

#[test]
fn test_round_trip_no_model_fields_backward_compat() {
    let (_temp_dir, exported) = import_and_export("prd_no_model_fields.json");

    // Top-level model should be absent
    assert!(
        exported.get("model").is_none(),
        "Top-level model should be omitted when not in input"
    );

    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("userStories should be present");

    // Both tasks should have no model fields
    for story in stories {
        let id = story.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        assert!(
            story.get("model").is_none(),
            "Task {} should not have model field",
            id
        );
        assert!(
            story.get("difficulty").is_none(),
            "Task {} should not have difficulty field",
            id
        );
        assert!(
            story.get("escalationNote").is_none(),
            "Task {} should not have escalationNote field",
            id
        );
    }

    // Core fields should still round-trip
    assert_eq!(
        exported.get("project").and_then(|v| v.as_str()),
        Some("no-model-project")
    );
    let nm001 = get_story(stories, "NM-001");
    assert_eq!(
        nm001.get("title").and_then(|v| v.as_str()),
        Some("Legacy task one")
    );
    assert_eq!(
        nm001.get("passes"),
        Some(&Value::Bool(false)),
        "NM-001 passes should be false"
    );
    let nm002 = get_story(stories, "NM-002");
    assert_eq!(
        nm002.get("passes"),
        Some(&Value::Bool(true)),
        "NM-002 passes should be true"
    );
}

// ========== update_task preserves model fields on re-import ==========

#[test]
fn test_update_task_preserves_model_fields_on_reimport() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path("prd_with_all_model_fields.json");

    // First import
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Verify DB has model fields via direct SQL
    let conn = open_connection(temp_dir.path()).unwrap();
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'MT-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(difficulty.as_deref(), Some("high"));
    assert_eq!(
        escalation_note.as_deref(),
        Some("Needs opus for complex refactor")
    );
    drop(conn);

    // Re-import with append=true, update_existing=true (existing tasks get updated)
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        true,
        true,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Verify model fields are preserved after update
    let conn = open_connection(temp_dir.path()).unwrap();
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'MT-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        model.as_deref(),
        Some("claude-opus-4-6"),
        "model should be preserved after re-import"
    );
    assert_eq!(
        difficulty.as_deref(),
        Some("high"),
        "difficulty should be preserved after re-import"
    );
    assert_eq!(
        escalation_note.as_deref(),
        Some("Needs opus for complex refactor"),
        "escalation_note should be preserved after re-import"
    );
}

// ========== Exported JSON uses camelCase ==========

#[test]
fn test_exported_json_uses_camel_case_escalation_note() {
    let (_temp_dir, exported) = import_and_export("prd_with_all_model_fields.json");

    let raw_json = serde_json::to_string_pretty(&exported).unwrap();

    // camelCase key should be present
    assert!(
        raw_json.contains("\"escalationNote\""),
        "Exported JSON should use camelCase 'escalationNote'"
    );

    // snake_case key should NOT be present
    assert!(
        !raw_json.contains("\"escalation_note\""),
        "Exported JSON should NOT use snake_case 'escalation_note'"
    );
}

// ========== NULL fields are omitted from exported JSON ==========

#[test]
fn test_null_fields_omitted_from_exported_json() {
    let (_temp_dir, exported) = import_and_export("prd_with_partial_model_fields.json");

    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("userStories should be present");

    // PM-002 has no model fields - verify they're absent (not null)
    let pm002 = get_story(stories, "PM-002");

    // Re-serialize just this story to check the raw JSON output
    let pm002_json = serde_json::to_string(pm002).unwrap();

    assert!(
        !pm002_json.contains("\"model\""),
        "PM-002 should not contain 'model' key at all, got: {}",
        pm002_json
    );
    assert!(
        !pm002_json.contains("\"difficulty\""),
        "PM-002 should not contain 'difficulty' key at all, got: {}",
        pm002_json
    );
    assert!(
        !pm002_json.contains("\"escalationNote\""),
        "PM-002 should not contain 'escalationNote' key at all, got: {}",
        pm002_json
    );

    // Also verify that actual null values are not serialized
    assert!(
        !pm002_json.contains(": null"),
        "No null values should appear in exported JSON, got: {}",
        pm002_json
    );
}

// ========== Verify DB state directly after import ==========

#[test]
fn test_db_state_after_import_with_all_model_fields() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path("prd_with_all_model_fields.json");

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();

    // Verify prd_metadata.default_model
    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        default_model.as_deref(),
        Some("claude-sonnet-4-6"),
        "prd_metadata.default_model should store the top-level model"
    );

    // Verify MT-001 task columns
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'MT-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(difficulty.as_deref(), Some("high"));
    assert_eq!(
        escalation_note.as_deref(),
        Some("Needs opus for complex refactor")
    );

    // Verify MT-002 task columns
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'MT-002'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(model.as_deref(), Some("claude-haiku-4-5-20251001"));
    assert_eq!(difficulty.as_deref(), Some("low"));
    assert_eq!(
        escalation_note.as_deref(),
        Some("Simple task, haiku is fine")
    );
}

#[test]
fn test_db_state_after_import_with_no_model_fields() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path("prd_no_model_fields.json");

    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    let conn = open_connection(temp_dir.path()).unwrap();

    // Verify prd_metadata.default_model is NULL
    let default_model: Option<String> = conn
        .query_row(
            "SELECT default_model FROM prd_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        default_model, None,
        "prd_metadata.default_model should be NULL for legacy PRD"
    );

    // Verify task columns are NULL
    let (model, difficulty, escalation_note): (Option<String>, Option<String>, Option<String>) =
        conn.query_row(
            "SELECT model, difficulty, escalation_note FROM tasks WHERE id = 'NM-001'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(model, None, "model should be NULL for legacy task");
    assert_eq!(
        difficulty, None,
        "difficulty should be NULL for legacy task"
    );
    assert_eq!(
        escalation_note, None,
        "escalation_note should be NULL for legacy task"
    );
}

// ========== Full pipeline: parse -> import -> export -> re-parse ==========

#[test]
fn test_full_pipeline_reimport_exported_json() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path("prd_with_all_model_fields.json");

    // Import original
    init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Export
    let export_path = temp_dir.path().join("exported.json");
    export::export(temp_dir.path(), &export_path, false, None).unwrap();

    // Re-import the exported file into a fresh DB
    let temp_dir2 = TempDir::new().unwrap();
    init::init(
        temp_dir2.path(),
        &[&export_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Export again
    let export_path2 = temp_dir2.path().join("re-exported.json");
    export::export(temp_dir2.path(), &export_path2, false, None).unwrap();

    // Compare both exports - they should be identical
    let json1 = fs::read_to_string(&export_path).unwrap();
    let json2 = fs::read_to_string(&export_path2).unwrap();

    let val1: Value = serde_json::from_str(&json1).unwrap();
    let val2: Value = serde_json::from_str(&json2).unwrap();

    // Compare user stories
    let stories1 = val1.get("userStories").and_then(|v| v.as_array()).unwrap();
    let stories2 = val2.get("userStories").and_then(|v| v.as_array()).unwrap();

    assert_eq!(stories1.len(), stories2.len(), "Story count should match");

    for (s1, s2) in stories1.iter().zip(stories2.iter()) {
        let id = s1.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        assert_eq!(
            s1.get("model"),
            s2.get("model"),
            "model mismatch for {}",
            id
        );
        assert_eq!(
            s1.get("difficulty"),
            s2.get("difficulty"),
            "difficulty mismatch for {}",
            id
        );
        assert_eq!(
            s1.get("escalationNote"),
            s2.get("escalationNote"),
            "escalationNote mismatch for {}",
            id
        );
    }

    // Compare top-level model
    assert_eq!(
        val1.get("model"),
        val2.get("model"),
        "Top-level model should match after re-import"
    );
}
