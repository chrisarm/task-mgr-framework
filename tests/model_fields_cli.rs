//! CLI integration tests for model/difficulty/escalationNote fields.
//!
//! These tests exercise the full CLI flow through the actual task-mgr binary:
//! init --from-json → next --format json → export --to-json → re-import.
//!
//! Verifies that model selection fields survive the complete pipeline
//! when driven from the command line (not just the Rust API).

// Allow deprecated cargo_bin function - the macro alternative requires more boilerplate
#![allow(deprecated)]

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};

mod common;
use common::render_fixture_tmpl;

/// Initialize a tempdir from a named fixture (rendered from `<name>.tmpl`),
/// returning the tempdir.
fn init_from_fixture(fixture_name: &str) -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = render_fixture_tmpl(fixture_name, temp_dir.path());

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "init",
            "--no-prefix",
            "--from-json",
            prd_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    temp_dir
}

// ============================================================================
// Test: init --from-json imports PRD with model fields successfully
// ============================================================================

#[test]
fn test_init_with_model_fields_succeeds() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = render_fixture_tmpl("prd_with_all_model_fields.json", temp_dir.path());

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "init",
            "--no-prefix",
            "--from-json",
            prd_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicates::prelude::predicate::str::contains("Initialized"))
        .stdout(predicates::prelude::predicate::str::contains("2 tasks"));
}

#[test]
fn test_init_with_partial_model_fields_succeeds() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = render_fixture_tmpl("prd_with_partial_model_fields.json", temp_dir.path());

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "init",
            "--no-prefix",
            "--from-json",
            prd_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicates::prelude::predicate::str::contains("3 tasks"));
}

// ============================================================================
// Test: next --format json includes model/difficulty/escalation_note
// ============================================================================

#[test]
fn test_next_json_includes_model_fields() {
    let temp_dir = init_from_fixture("prd_with_all_model_fields.json");

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["--format", "json", "next"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).unwrap();
    let parsed: Value = serde_json::from_str(&json_str).unwrap();

    // MT-001 is the only eligible task (passes: false, no unmet deps)
    let task = parsed
        .get("task")
        .expect("next output should have 'task' field");

    assert_eq!(
        task.get("id").and_then(|v| v.as_str()),
        Some("MT-001"),
        "Should select MT-001 as next task"
    );

    assert_eq!(
        task.get("model").and_then(|v| v.as_str()),
        Some(OPUS_MODEL),
        "next --json should include task model"
    );

    assert_eq!(
        task.get("difficulty").and_then(|v| v.as_str()),
        Some("high"),
        "next --json should include task difficulty"
    );

    assert_eq!(
        task.get("escalation_note").and_then(|v| v.as_str()),
        Some("Needs opus for complex refactor"),
        "next --json should include task escalation_note"
    );
}

#[test]
fn test_next_json_omits_null_model_fields() {
    let temp_dir = init_from_fixture("prd_with_partial_model_fields.json");

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["--format", "json", "next"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).unwrap();
    let parsed: Value = serde_json::from_str(&json_str).unwrap();

    // PM-001 has highest priority and model fields set
    let task = parsed.get("task").expect("should have task");
    assert_eq!(
        task.get("id").and_then(|v| v.as_str()),
        Some("PM-001"),
        "Should select PM-001 (highest priority)"
    );

    // PM-001 has model and difficulty but no escalationNote
    assert_eq!(task.get("model").and_then(|v| v.as_str()), Some(OPUS_MODEL),);
    assert_eq!(
        task.get("difficulty").and_then(|v| v.as_str()),
        Some("high"),
    );
    // escalation_note should be absent (not null) since skip_serializing_if = "Option::is_none"
    assert!(
        task.get("escalation_note").is_none(),
        "escalation_note should be omitted when NULL, got: {:?}",
        task.get("escalation_note")
    );
}

// ============================================================================
// Test: export --to-json preserves model/difficulty/escalationNote
// ============================================================================

#[test]
fn test_export_preserves_model_fields() {
    let temp_dir = init_from_fixture("prd_with_all_model_fields.json");
    let export_path = temp_dir.path().join("exported.json");

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["export", "--to-json", export_path.to_str().unwrap()])
        .assert()
        .success();

    let content = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&content).unwrap();

    // Top-level model field
    assert_eq!(
        exported.get("model").and_then(|v| v.as_str()),
        Some(SONNET_MODEL),
        "Export should preserve top-level model"
    );

    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("Export should have userStories");

    // MT-001
    let mt001 = stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some("MT-001"))
        .expect("MT-001 should be in export");

    assert_eq!(
        mt001.get("model").and_then(|v| v.as_str()),
        Some(OPUS_MODEL),
        "Export should preserve per-task model"
    );
    assert_eq!(
        mt001.get("difficulty").and_then(|v| v.as_str()),
        Some("high"),
        "Export should preserve per-task difficulty"
    );
    assert_eq!(
        mt001.get("escalationNote").and_then(|v| v.as_str()),
        Some("Needs opus for complex refactor"),
        "Export should preserve escalationNote in camelCase"
    );

    // MT-002
    let mt002 = stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some("MT-002"))
        .expect("MT-002 should be in export");

    assert_eq!(
        mt002.get("model").and_then(|v| v.as_str()),
        Some(HAIKU_MODEL),
    );
    assert_eq!(
        mt002.get("difficulty").and_then(|v| v.as_str()),
        Some("low"),
    );
    assert_eq!(
        mt002.get("escalationNote").and_then(|v| v.as_str()),
        Some("Simple task, haiku is fine"),
    );
}

#[test]
fn test_export_omits_null_model_fields() {
    let temp_dir = init_from_fixture("prd_with_partial_model_fields.json");
    let export_path = temp_dir.path().join("exported.json");

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["export", "--to-json", export_path.to_str().unwrap()])
        .assert()
        .success();

    let content = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&content).unwrap();

    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("Export should have userStories");

    // PM-002 has no model fields at all
    let pm002 = stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some("PM-002"))
        .expect("PM-002 should be in export");

    let pm002_json = serde_json::to_string(pm002).unwrap();
    assert!(
        !pm002_json.contains("\"model\""),
        "PM-002 should not have model key in export"
    );
    assert!(
        !pm002_json.contains("\"difficulty\""),
        "PM-002 should not have difficulty key in export"
    );
    assert!(
        !pm002_json.contains("\"escalationNote\""),
        "PM-002 should not have escalationNote key in export"
    );
}

// ============================================================================
// Test: re-import preserves model fields via --append --update-existing
// ============================================================================

#[test]
fn test_reimport_preserves_model_fields() {
    let temp_dir = init_from_fixture("prd_with_all_model_fields.json");
    let prd_path = render_fixture_tmpl("prd_with_all_model_fields.json", temp_dir.path());

    // Re-import with --append --update-existing
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "init",
            "--no-prefix",
            "--from-json",
            prd_path.to_str().unwrap(),
            "--append",
            "--update-existing",
        ])
        .assert()
        .success();

    // Export and verify fields survived
    let export_path = temp_dir.path().join("after_reimport.json");
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["export", "--to-json", export_path.to_str().unwrap()])
        .assert()
        .success();

    let content = fs::read_to_string(&export_path).unwrap();
    let exported: Value = serde_json::from_str(&content).unwrap();

    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("Export should have userStories");

    let mt001 = stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some("MT-001"))
        .expect("MT-001 should survive re-import");

    assert_eq!(
        mt001.get("model").and_then(|v| v.as_str()),
        Some(OPUS_MODEL),
        "model should survive re-import"
    );
    assert_eq!(
        mt001.get("difficulty").and_then(|v| v.as_str()),
        Some("high"),
        "difficulty should survive re-import"
    );
    assert_eq!(
        mt001.get("escalationNote").and_then(|v| v.as_str()),
        Some("Needs opus for complex refactor"),
        "escalationNote should survive re-import"
    );
}

// ============================================================================
// Test: full round-trip via CLI: init → export → re-init from export
// ============================================================================

#[test]
fn test_full_cli_round_trip() {
    // Phase 1: Import original PRD
    let temp_dir1 = init_from_fixture("prd_with_all_model_fields.json");
    let export_path = temp_dir1.path().join("exported.json");

    // Phase 2: Export from first DB
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir1.path().to_str().unwrap()])
        .args(["export", "--to-json", export_path.to_str().unwrap()])
        .assert()
        .success();

    // Phase 3: Import the exported JSON into a fresh DB
    let temp_dir2 = TempDir::new().unwrap();
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir2.path().to_str().unwrap()])
        .args([
            "init",
            "--no-prefix",
            "--from-json",
            export_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // Phase 4: Export from second DB and compare
    let export_path2 = temp_dir2.path().join("re-exported.json");
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir2.path().to_str().unwrap()])
        .args(["export", "--to-json", export_path2.to_str().unwrap()])
        .assert()
        .success();

    let json1 = fs::read_to_string(&export_path).unwrap();
    let json2 = fs::read_to_string(&export_path2).unwrap();

    let val1: Value = serde_json::from_str(&json1).unwrap();
    let val2: Value = serde_json::from_str(&json2).unwrap();

    let stories1 = val1.get("userStories").and_then(|v| v.as_array()).unwrap();
    let stories2 = val2.get("userStories").and_then(|v| v.as_array()).unwrap();

    assert_eq!(
        stories1.len(),
        stories2.len(),
        "Story count should match after round-trip"
    );

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

    // Top-level model should also match
    assert_eq!(
        val1.get("model"),
        val2.get("model"),
        "Top-level model should survive full round-trip"
    );
}

// ============================================================================
// Test: next --format json for task with no model fields
// ============================================================================

#[test]
fn test_next_json_no_model_fields_backward_compat() {
    let temp_dir = init_from_fixture("prd_no_model_fields.json");

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["--format", "json", "next"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).unwrap();
    let parsed: Value = serde_json::from_str(&json_str).unwrap();

    let task = parsed.get("task").expect("should have task");
    assert_eq!(
        task.get("id").and_then(|v| v.as_str()),
        Some("NM-001"),
        "Should select NM-001 from no-model PRD"
    );

    // Model fields should be absent (not null)
    assert!(
        task.get("model").is_none(),
        "model should be omitted for legacy PRD task"
    );
    assert!(
        task.get("difficulty").is_none(),
        "difficulty should be omitted for legacy PRD task"
    );
    assert!(
        task.get("escalation_note").is_none(),
        "escalation_note should be omitted for legacy PRD task"
    );
}
