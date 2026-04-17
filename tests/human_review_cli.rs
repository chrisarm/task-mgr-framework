//! CLI integration tests for requiresHuman and humanReviewTimeout fields.
//!
//! Exercises the full CLI pipeline:
//!   init --from-json → show → next --format json → export --to-json → re-import.
//!
//! Verifies that requiresHuman/humanReviewTimeout survive the complete pipeline
//! when driven from the command line (not just the Rust API).

// Allow deprecated cargo_bin function - the macro alternative requires more boilerplate
#![allow(deprecated)]

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

/// Get the path to a fixture file by name.
fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Initialize a tempdir from a named fixture, returning the tempdir.
fn init_from_fixture(fixture_name: &str) -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path(fixture_name);

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
// Test: init --from-json imports PRD with requiresHuman successfully
// ============================================================================

#[test]
fn test_init_with_requires_human_succeeds() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path("prd_with_requires_human.json");

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

// ============================================================================
// Test: show displays "Requires Human Review: Yes" for requiresHuman tasks
// ============================================================================

#[test]
fn test_show_displays_requires_human_yes() {
    let temp_dir = init_from_fixture("prd_with_requires_human.json");

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["show", "HR-001"])
        .assert()
        .success()
        .stdout(predicates::prelude::predicate::str::contains(
            "Requires Human Review: Yes",
        ));
}

#[test]
fn test_show_displays_human_review_timeout() {
    let temp_dir = init_from_fixture("prd_with_requires_human.json");

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["show", "HR-001"])
        .assert()
        .success()
        .stdout(predicates::prelude::predicate::str::contains(
            "Human Review Timeout: 120s",
        ));
}

#[test]
fn test_show_omits_requires_human_for_regular_task() {
    let temp_dir = init_from_fixture("prd_with_requires_human.json");

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["show", "HR-002"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    assert!(
        !text.contains("Requires Human Review"),
        "show should not display 'Requires Human Review' for regular tasks, got:\n{text}"
    );
}

// ============================================================================
// Test: next --format json includes requires_human field
// ============================================================================

#[test]
fn test_next_json_includes_requires_human_true() {
    let temp_dir = init_from_fixture("prd_with_requires_human.json");

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

    // HR-001 has higher priority (1) and should be selected first
    let task = parsed
        .get("task")
        .expect("next output should have 'task' field");

    assert_eq!(
        task.get("id").and_then(|v| v.as_str()),
        Some("HR-001"),
        "Should select HR-001 as next task (highest priority)"
    );

    assert_eq!(
        task.get("requires_human").and_then(|v| v.as_bool()),
        Some(true),
        "next --json should include requires_human: true for HR-001"
    );
}

#[test]
fn test_next_json_requires_human_false_for_regular_task() {
    let temp_dir = init_from_fixture("prd_with_requires_human.json");

    // Mark HR-001 as done (--force bypasses claim requirement) so HR-002 becomes next
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["done", "--force", "HR-001"])
        .assert()
        .success();

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

    let task = parsed
        .get("task")
        .expect("next output should have 'task' field");

    assert_eq!(
        task.get("id").and_then(|v| v.as_str()),
        Some("HR-002"),
        "Should select HR-002 after HR-001 is done"
    );

    assert_eq!(
        task.get("requires_human").and_then(|v| v.as_bool()),
        Some(false),
        "next --json should include requires_human: false for regular task"
    );
}

// ============================================================================
// Test: export --to-json round-trips requiresHuman and humanReviewTimeout
// ============================================================================

#[test]
fn test_export_preserves_requires_human_and_timeout() {
    let temp_dir = init_from_fixture("prd_with_requires_human.json");
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

    let hr001 = stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some("HR-001"))
        .expect("HR-001 should be in export");

    assert_eq!(
        hr001.get("requiresHuman").and_then(|v| v.as_bool()),
        Some(true),
        "Export should preserve requiresHuman: true for HR-001"
    );

    assert_eq!(
        hr001.get("humanReviewTimeout").and_then(|v| v.as_u64()),
        Some(120),
        "Export should preserve humanReviewTimeout: 120 for HR-001"
    );
}

#[test]
fn test_export_omits_requires_human_for_regular_task() {
    let temp_dir = init_from_fixture("prd_with_requires_human.json");
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

    let hr002 = stories
        .iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some("HR-002"))
        .expect("HR-002 should be in export");

    let hr002_json = serde_json::to_string(hr002).unwrap();
    assert!(
        !hr002_json.contains("\"requiresHuman\""),
        "HR-002 export should not contain requiresHuman key, got: {hr002_json}"
    );
    assert!(
        !hr002_json.contains("\"humanReviewTimeout\""),
        "HR-002 export should not contain humanReviewTimeout key, got: {hr002_json}"
    );
}

// ============================================================================
// Test: full round-trip — init → export → re-init → verify fields preserved
// ============================================================================

#[test]
fn test_full_round_trip_requires_human() {
    // Phase 1: Import original PRD
    let temp_dir1 = init_from_fixture("prd_with_requires_human.json");
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

    let json1: Value = serde_json::from_str(&fs::read_to_string(&export_path).unwrap()).unwrap();
    let json2: Value = serde_json::from_str(&fs::read_to_string(&export_path2).unwrap()).unwrap();

    let stories1 = json1.get("userStories").and_then(|v| v.as_array()).unwrap();
    let stories2 = json2.get("userStories").and_then(|v| v.as_array()).unwrap();

    assert_eq!(stories1.len(), stories2.len(), "Story count must match");

    for (s1, s2) in stories1.iter().zip(stories2.iter()) {
        let id = s1.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        assert_eq!(
            s1.get("requiresHuman"),
            s2.get("requiresHuman"),
            "requiresHuman mismatch for {id}"
        );
        assert_eq!(
            s1.get("humanReviewTimeout"),
            s2.get("humanReviewTimeout"),
            "humanReviewTimeout mismatch for {id}"
        );
    }
}
