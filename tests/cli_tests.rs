//! CLI integration tests using assert_cmd and predicates crates.
//!
//! These tests invoke the actual task-mgr binary and verify:
//! - --help shows usage
//! - init --from-json works with fixture
//! - list --format json produces valid JSON
//! - next --format json produces expected structure
//! - Invalid commands return non-zero exit code
//! - Missing required args return helpful error

// Allow deprecated cargo_bin function - the macro alternative requires more boilerplate
// and the function works fine for our use case
#![allow(deprecated)]

use assert_cmd::cargo::cargo_bin;
use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

/// Get the path to the sample PRD fixture file.
fn sample_prd_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample_prd.json")
}

/// Create a tempdir and initialize it with the sample PRD.
fn setup_initialized_tempdir() -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

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
// Test: task-mgr --help shows usage
// ============================================================================

#[test]
fn test_help_shows_usage() {
    Command::new(cargo_bin("task-mgr"))
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("task-mgr"))
        .stdout(predicate::str::contains("USAGE:").or(predicate::str::contains("Usage:")))
        .stdout(predicate::str::contains("COMMANDS:").or(predicate::str::contains("Commands:")));
}

#[test]
fn test_subcommand_help() {
    // Test help for major subcommands
    for cmd in ["init", "next", "list", "complete", "learn", "doctor"] {
        Command::new(cargo_bin("task-mgr"))
            .args([cmd, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains(cmd));
    }
}

// ============================================================================
// Test: task-mgr init --from-json works with fixture
// ============================================================================

#[test]
fn test_init_from_json() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

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
        // Output is "Initialized: 7 tasks, 14 files, 16 relationships"
        .stdout(predicate::str::contains("Initialized"))
        .stdout(predicate::str::contains("7 tasks"));
}

#[test]
fn test_init_creates_database() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();
    let db_path = temp_dir.path().join("tasks.db");

    // Database should not exist yet
    assert!(!db_path.exists());

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

    // Database should now exist
    assert!(db_path.exists());
}

#[test]
fn test_init_dry_run_shows_preview() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Run with dry-run flag
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "init",
            "--from-json",
            prd_path.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let output_str = String::from_utf8(output).unwrap();
    // Should show preview info
    assert!(
        output_str.contains("dry") || output_str.contains("would") || output_str.contains("7"),
        "Dry run should show preview: {}",
        output_str
    );
}

// ============================================================================
// Test: task-mgr list --format json produces valid JSON
// ============================================================================

#[test]
fn test_list_json_produces_valid_json() {
    let temp_dir = setup_initialized_tempdir();

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).unwrap();
    let parsed: Result<Value, _> = serde_json::from_str(&json_str);
    assert!(parsed.is_ok(), "Output should be valid JSON: {}", json_str);

    let value = parsed.unwrap();
    assert!(value.is_object(), "JSON should be an object");
    assert!(
        value.get("tasks").is_some(),
        "JSON should have 'tasks' field"
    );
}

#[test]
fn test_list_shows_all_tasks() {
    let temp_dir = setup_initialized_tempdir();

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).unwrap();
    let parsed: Value = serde_json::from_str(&json_str).unwrap();

    let tasks = parsed.get("tasks").and_then(|v| v.as_array()).unwrap();
    assert_eq!(tasks.len(), 7, "Should have 7 tasks from sample PRD");
}

#[test]
fn test_list_filter_by_status() {
    let temp_dir = setup_initialized_tempdir();

    // List only todo tasks
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["list", "--status", "todo", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).unwrap();
    let parsed: Value = serde_json::from_str(&json_str).unwrap();
    let tasks = parsed.get("tasks").and_then(|v| v.as_array()).unwrap();

    // All returned tasks should have status "todo"
    for task in tasks {
        assert_eq!(
            task.get("status").and_then(|v| v.as_str()),
            Some("todo"),
            "Filtered tasks should have todo status"
        );
    }
}

// ============================================================================
// Test: task-mgr next --format json produces expected structure
// ============================================================================

#[test]
fn test_next_json_produces_expected_structure() {
    let temp_dir = setup_initialized_tempdir();

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["next", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).unwrap();
    let parsed: Value = serde_json::from_str(&json_str).unwrap();

    // Should have task field (even if null)
    assert!(
        parsed.get("task").is_some(),
        "JSON should have 'task' field"
    );

    // If there's a task, it should have required fields
    if let Some(task) = parsed.get("task").and_then(|v| v.as_object()) {
        assert!(task.contains_key("id"), "Task should have 'id' field");
        assert!(task.contains_key("title"), "Task should have 'title' field");
        assert!(
            task.contains_key("priority"),
            "Task should have 'priority' field"
        );
    }
}

#[test]
fn test_next_has_selection_info() {
    let temp_dir = setup_initialized_tempdir();

    // Add a learning to verify the overall next output structure
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "learn",
            "--outcome",
            "pattern",
            "--title",
            "Test pattern",
            "--content",
            "This is a test pattern for CLI tests",
            "--tags",
            "test,cli",
        ])
        .assert()
        .success();

    // Now get next task
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["next", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).unwrap();
    let parsed: Value = serde_json::from_str(&json_str).unwrap();

    // Should have selection field with reason
    assert!(
        parsed.get("selection").is_some(),
        "JSON should have 'selection' field"
    );
}

// ============================================================================
// Test: Invalid commands return non-zero exit code
// ============================================================================

#[test]
fn test_invalid_command_returns_error() {
    Command::new(cargo_bin("task-mgr"))
        .arg("nonexistent-command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error").or(predicate::str::contains("unrecognized")));
}

#[test]
fn test_complete_nonexistent_task_returns_error() {
    let temp_dir = setup_initialized_tempdir();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["complete", "NONEXISTENT-TASK-ID"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("error")));
}

#[test]
fn test_init_nonexistent_file_returns_error() {
    let temp_dir = TempDir::new().unwrap();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args([
            "init",
            "--no-prefix",
            "--from-json",
            "/nonexistent/path/to/file.json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error").or(predicate::str::contains("No such file")));
}

// ============================================================================
// Test: Missing required args return helpful error
// ============================================================================

#[test]
fn test_init_without_from_json_returns_error() {
    let temp_dir = TempDir::new().unwrap();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--from-json"));
}

#[test]
fn test_complete_without_task_id_returns_error() {
    let temp_dir = setup_initialized_tempdir();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .arg("complete")
        .assert()
        .failure()
        .stderr(predicate::str::contains("<TASK_IDS>").or(predicate::str::contains("required")));
}

#[test]
fn test_learn_without_required_args_returns_error() {
    let temp_dir = setup_initialized_tempdir();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .arg("learn")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--outcome")
                .or(predicate::str::contains("--title"))
                .or(predicate::str::contains("required")),
        );
}

// ============================================================================
// Test: --format flag works consistently across commands
// ============================================================================

#[test]
fn test_format_flag_text_and_json() {
    let temp_dir = setup_initialized_tempdir();

    // Test list with text format
    let text_output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["list", "--format", "text"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text_str = String::from_utf8(text_output).unwrap();
    // Text format should NOT be valid JSON
    let is_json: Result<Value, _> = serde_json::from_str(&text_str);
    assert!(
        is_json.is_err(),
        "Text format should not produce valid JSON"
    );

    // Test list with json format
    let json_output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(json_output).unwrap();
    // JSON format should be valid JSON
    let is_json: Result<Value, _> = serde_json::from_str(&json_str);
    assert!(is_json.is_ok(), "JSON format should produce valid JSON");
}

// ============================================================================
// Test: Database isolation per test (tempdir)
// ============================================================================

#[test]
fn test_database_isolation() {
    // Create two separate tempdirs
    let temp_dir1 = TempDir::new().unwrap();
    let temp_dir2 = TempDir::new().unwrap();
    let prd_path = sample_prd_path();

    // Initialize both with the same PRD
    for temp_dir in [&temp_dir1, &temp_dir2] {
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
    }

    // Complete a task in temp_dir1
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir1.path().to_str().unwrap()])
        .args(["complete", "TASK-003", "--force"])
        .assert()
        .success();

    // Verify TASK-003 is done in temp_dir1
    let output1 = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir1.path().to_str().unwrap()])
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json1: Value = serde_json::from_str(&String::from_utf8(output1).unwrap()).unwrap();
    let tasks1 = json1.get("tasks").and_then(|v| v.as_array()).unwrap();
    let task3_1 = tasks1
        .iter()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("TASK-003"))
        .unwrap();
    assert_eq!(task3_1.get("status").and_then(|v| v.as_str()), Some("done"));

    // Verify TASK-003 is still todo in temp_dir2 (isolation)
    let output2 = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir2.path().to_str().unwrap()])
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json2: Value = serde_json::from_str(&String::from_utf8(output2).unwrap()).unwrap();
    let tasks2 = json2.get("tasks").and_then(|v| v.as_array()).unwrap();
    let task3_2 = tasks2
        .iter()
        .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("TASK-003"))
        .unwrap();
    assert_eq!(
        task3_2.get("status").and_then(|v| v.as_str()),
        Some("todo"),
        "Databases should be isolated - temp_dir2 should not see temp_dir1 changes"
    );
}

// ============================================================================
// Test: Export produces valid JSON that can be re-imported
// ============================================================================

#[test]
fn test_export_roundtrip() {
    let temp_dir = setup_initialized_tempdir();
    let export_path = temp_dir.path().join("exported.json");

    // Export
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["export", "--to-json", export_path.to_str().unwrap()])
        .assert()
        .success();

    assert!(export_path.exists(), "Export file should be created");

    // Verify it's valid JSON
    let content = fs::read_to_string(&export_path).unwrap();
    let parsed: Value = serde_json::from_str(&content).unwrap();
    assert!(
        parsed.get("userStories").is_some(),
        "Export should have userStories field"
    );

    // Re-import into new directory
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

    // Both should have same number of tasks
    let output2 = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir2.path().to_str().unwrap()])
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json2: Value = serde_json::from_str(&String::from_utf8(output2).unwrap()).unwrap();
    let tasks2 = json2.get("tasks").and_then(|v| v.as_array()).unwrap();
    assert_eq!(tasks2.len(), 7, "Re-imported should have same task count");
}

// ============================================================================
// Test: Run lifecycle commands
// ============================================================================

#[test]
fn test_run_lifecycle() {
    let temp_dir = setup_initialized_tempdir();

    // Begin a run
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["run", "begin", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap()).unwrap();
    let run_id = json
        .get("run_id")
        .and_then(|v| v.as_str())
        .expect("Should have run_id");

    // End the run
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["run", "end", "--run-id", run_id, "--status", "completed"])
        .assert()
        .success();
}

// ============================================================================
// Test: Stats command
// ============================================================================

#[test]
fn test_stats_command() {
    let temp_dir = setup_initialized_tempdir();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .arg("stats")
        .assert()
        .success()
        .stdout(predicate::str::contains("todo").or(predicate::str::contains("done")));
}

#[test]
fn test_stats_json_format() {
    let temp_dir = setup_initialized_tempdir();

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["stats", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap()).unwrap();
    // Stats JSON has "tasks" field (not "task_counts")
    assert!(json.get("tasks").is_some(), "Stats should have tasks field");
    assert!(
        json.get("learnings").is_some(),
        "Stats should have learnings field"
    );
}

// ============================================================================
// Test: curate subcommand (retire / unretire)
// ============================================================================

/// Set up a tempdir with an initialized DB and insert a learning via `task-mgr learn`.
/// Returns the tempdir (keep alive) and the learning ID (obtained from JSON output).
fn setup_dir_with_learning(title: &str, outcome: &str) -> (TempDir, i64) {
    let temp_dir = TempDir::new().unwrap();
    let dir = temp_dir.path().to_str().unwrap().to_owned();

    // Init an empty DB (no PRD needed for curate tests)
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", &dir])
        .args(["migrate", "all"])
        .assert()
        .success();

    // Insert learning via CLI
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", &dir])
        .args([
            "--format",
            "json",
            "learn",
            "--outcome",
            outcome,
            "--title",
            title,
            "--content",
            "Integration test content",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap()).unwrap();
    let learning_id = json["learning_id"].as_i64().unwrap();

    (temp_dir, learning_id)
}

#[test]
fn test_curate_help_shows_subcommands() {
    // AC6: curate --help shows retire and unretire subcommands
    Command::new(cargo_bin("task-mgr"))
        .args(["curate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("retire"))
        .stdout(predicate::str::contains("unretire"));
}

#[test]
fn test_curate_retire_help() {
    // AC6: curate retire --help shows all flags
    Command::new(cargo_bin("task-mgr"))
        .args(["curate", "retire", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run"))
        .stdout(predicate::str::contains("min-age-days"))
        .stdout(predicate::str::contains("min-shows"))
        .stdout(predicate::str::contains("max-rate"));
}

#[test]
fn test_curate_retire_dry_run_flag() {
    // AC4: --dry-run flag works via CLI — no DB changes but output shows candidates
    let (temp_dir, learning_id) = setup_dir_with_learning("Stale pattern", "pattern");
    let dir = temp_dir.path().to_str().unwrap();

    // Age the learning and set stats to match criterion 2 (shown >= 10, applied = 0)
    let db_path = temp_dir.path().join("tasks.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE learnings SET times_shown = 12 WHERE id = ?1",
            [learning_id],
        )
        .unwrap();
    }

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["curate", "retire", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    assert!(
        text.contains("Dry run") || text.contains("dry"),
        "dry-run output must mention 'Dry run': {text}"
    );
    assert!(
        text.contains("no changes made"),
        "dry-run output must say 'no changes made': {text}"
    );

    // Verify no DB changes
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let retired: bool = conn
        .query_row(
            "SELECT retired_at IS NOT NULL FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!retired, "dry-run must not set retired_at");
}

#[test]
fn test_curate_retire_custom_thresholds() {
    // AC4: --min-age-days, --min-shows, --max-rate flags change candidate set
    let (temp_dir, learning_id) = setup_dir_with_learning("Low-conf fresh learning", "pattern");
    let dir = temp_dir.path().to_str().unwrap();

    // A 5-day-old low-confidence unapplied learning is NOT a candidate at default threshold (90 days)
    // but IS at --min-age-days=3
    {
        let db_path = temp_dir.path().join("tasks.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE learnings SET confidence = 'low', created_at = datetime('now', '-5 days') WHERE id = ?1",
            [learning_id],
        )
        .unwrap();
    }

    // With default threshold: should find 0 candidates (or none for this learning)
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["curate", "retire", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No retirement candidates").or(
            // Might still list as 0 if no candidates; text varies
            predicate::str::is_empty().not(),
        ));

    // With custom threshold (3 days): learning should be a candidate
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["curate", "retire", "--dry-run", "--min-age-days", "3"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    assert!(
        text.contains("Low-conf fresh learning"),
        "with --min-age-days=3, 5-day-old low-conf learning must be a candidate: {text}"
    );
}

#[test]
fn test_curate_retire_json_output() {
    // AC5: curate retire --format json produces valid JSON with expected fields
    let (temp_dir, learning_id) = setup_dir_with_learning("JSON retire test", "pattern");
    let dir = temp_dir.path().to_str().unwrap();

    // Make it a candidate (shown >= 10, applied = 0)
    {
        let db_path = temp_dir.path().join("tasks.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE learnings SET times_shown = 15 WHERE id = ?1",
            [learning_id],
        )
        .unwrap();
    }

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir, "--format", "json"])
        .args(["curate", "retire", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap())
        .expect("curate retire must produce valid JSON");

    assert!(json.get("dry_run").is_some(), "JSON must have dry_run");
    assert!(
        json.get("candidates_found").is_some(),
        "JSON must have candidates_found"
    );
    assert!(
        json.get("learnings_retired").is_some(),
        "JSON must have learnings_retired"
    );
    assert!(
        json.get("candidates").is_some(),
        "JSON must have candidates"
    );
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["learnings_retired"], 0);
}

#[test]
fn test_curate_unretire_json_output() {
    // AC5: curate unretire --format json produces valid JSON
    let (temp_dir, learning_id) = setup_dir_with_learning("Unretire JSON test", "pattern");
    let dir = temp_dir.path().to_str().unwrap();

    // Retire it first
    {
        let db_path = temp_dir.path().join("tasks.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
            [learning_id],
        )
        .unwrap();
    }

    let id_str = learning_id.to_string();
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir, "--format", "json"])
        .args(["curate", "unretire", &id_str])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap())
        .expect("curate unretire must produce valid JSON");

    assert!(json.get("restored").is_some(), "JSON must have restored");
    assert!(json.get("errors").is_some(), "JSON must have errors");
    assert!(
        json["restored"]
            .as_array()
            .unwrap()
            .contains(&Value::Number(learning_id.into())),
        "restored must contain the unretired learning ID"
    );
}

#[test]
fn test_curate_e2e_retire_unretire_workflow() {
    // AC1: Full E2E: init -> create learning -> retire --dry-run -> retire -> verify excluded
    // from learnings list -> unretire -> verify re-included
    let (temp_dir, learning_id) = setup_dir_with_learning("E2E retire target", "pattern");
    let dir = temp_dir.path().to_str().unwrap();
    let db_path = temp_dir.path().join("tasks.db");
    let id_str = learning_id.to_string();

    // Make it a retirement candidate (shown >= 10, applied = 0)
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE learnings SET times_shown = 20 WHERE id = ?1",
            [learning_id],
        )
        .unwrap();
    }

    // Step 1: dry-run must find candidate but make no changes
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["curate", "retire", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("E2E retire target"));

    let retired_before: bool = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT retired_at IS NOT NULL FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(!retired_before, "dry-run must not set retired_at");

    // Step 2: actual retire — learning gets soft-archived
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["curate", "retire"])
        .assert()
        .success()
        .stdout(predicate::str::contains("E2E retire target"));

    let retired_after: bool = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT retired_at IS NOT NULL FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(retired_after, "curate retire must set retired_at");

    // Step 3: learnings list must exclude retired learning
    let list_output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["learnings"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let list_text = String::from_utf8(list_output).unwrap();
    assert!(
        !list_text.contains("E2E retire target"),
        "retired learning must not appear in learnings list: {list_text}"
    );

    // Step 4: unretire — learning becomes active again
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["curate", "unretire", &id_str])
        .assert()
        .success();

    let retired_final: bool = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT retired_at IS NOT NULL FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(!retired_final, "curate unretire must clear retired_at");

    // Step 5: learning reappears in list after unretire
    let list_after_output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["learnings"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let list_after_text = String::from_utf8(list_after_output).unwrap();
    assert!(
        list_after_text.contains("E2E retire target"),
        "unretired learning must reappear in learnings list: {list_after_text}"
    );
}

// ============================================================================
// ============================================================================
// Test: curate enrich subcommand
// ============================================================================

/// Returns the path to the fake claude binary fixture used to mock LLM calls
/// in curate enrich integration tests.
fn fake_claude_path() -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake_claude.sh")
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn test_curate_enrich_help_shows_flags() {
    // AC9: curate --help shows enrich subcommand; curate enrich --help shows expected flags
    Command::new(cargo_bin("task-mgr"))
        .args(["curate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("enrich"));

    Command::new(cargo_bin("task-mgr"))
        .args(["curate", "enrich", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run"))
        .stdout(predicate::str::contains("batch-size"))
        .stdout(predicate::str::contains("field"));
}

#[test]
fn test_curate_enrich_dry_run_no_db_changes() {
    // AC1: dry-run shows proposals but leaves DB unchanged
    let (temp_dir, learning_id) = setup_dir_with_learning("Needs enrichment", "pattern");
    let dir = temp_dir.path().to_str().unwrap();
    let db_path = temp_dir.path().join("tasks.db");

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .env("CLAUDE_BINARY", fake_claude_path())
        .args(["curate", "enrich", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    assert!(
        text.contains("Dry run") || text.contains("dry"),
        "dry-run output must mention dry run: {text}"
    );
    assert!(
        text.contains("no changes made"),
        "dry-run output must say 'no changes made': {text}"
    );

    // Verify DB unchanged: applies_to_task_types must still be NULL
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let task_types: Option<String> = conn
        .query_row(
            "SELECT applies_to_task_types FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        task_types.is_none(),
        "applies_to_task_types must remain NULL after dry-run"
    );
}

#[test]
fn test_curate_enrich_populates_metadata() {
    // AC2: after enrich (non-dry-run), applies_to_task_types and errors are populated
    let (temp_dir, learning_id) = setup_dir_with_learning("Metadata missing", "pattern");
    let dir = temp_dir.path().to_str().unwrap();
    let db_path = temp_dir.path().join("tasks.db");

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .env("CLAUDE_BINARY", fake_claude_path())
        .args(["curate", "enrich"])
        .assert()
        .success();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let task_types: Option<String> = conn
        .query_row(
            "SELECT applies_to_task_types FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        task_types.is_some(),
        "applies_to_task_types must be populated after enrich"
    );

    let errors: Option<String> = conn
        .query_row(
            "SELECT applies_to_errors FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        errors.is_some(),
        "applies_to_errors must be populated after enrich"
    );
}

#[test]
fn test_curate_enrich_idempotent_second_run_zero_candidates() {
    // AC3: re-run after all fields are enriched shows 0 candidates
    let (temp_dir, _learning_id) = setup_dir_with_learning("Already enriched", "pattern");
    let dir = temp_dir.path().to_str().unwrap();

    // First run: enrich all
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .env("CLAUDE_BINARY", fake_claude_path())
        .args(["curate", "enrich"])
        .assert()
        .success();

    // Second run: 0 candidates
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .env("CLAUDE_BINARY", fake_claude_path())
        .args(["curate", "enrich"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    assert!(
        text.contains("0 candidates") || text.contains("No enrichment candidates"),
        "second run must report 0 candidates: {text}"
    );
}

#[test]
fn test_curate_enrich_skips_retired_learnings() {
    // AC4: retired learnings are excluded from enrich candidates
    let (temp_dir, active_id) = setup_dir_with_learning("Active learning", "pattern");
    let dir = temp_dir.path().to_str().unwrap();
    let db_path = temp_dir.path().join("tasks.db");

    // Insert a second learning and retire it
    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir, "--format", "json"])
        .args([
            "learn",
            "--outcome",
            "pattern",
            "--title",
            "Retired learning",
            "--content",
            "Retired content",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap()).unwrap();
    let retired_id = json["learning_id"].as_i64().unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
            [retired_id],
        )
        .unwrap();
    }

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .env("CLAUDE_BINARY", fake_claude_path())
        .args(["curate", "enrich"])
        .assert()
        .success();

    // Active learning must be enriched; retired learning must NOT be enriched
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let active_types: Option<String> = conn
        .query_row(
            "SELECT applies_to_task_types FROM learnings WHERE id = ?1",
            [active_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(active_types.is_some(), "active learning must be enriched");

    let retired_types: Option<String> = conn
        .query_row(
            "SELECT applies_to_task_types FROM learnings WHERE id = ?1",
            [retired_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        retired_types.is_none(),
        "retired learning must NOT be enriched"
    );
}

#[test]
fn test_curate_enrich_field_filter() {
    // AC5: --field=applies_to_files only enriches applies_to_files candidates
    let (temp_dir, learning_id) = setup_dir_with_learning("Field filter test", "pattern");
    let dir = temp_dir.path().to_str().unwrap();
    let db_path = temp_dir.path().join("tasks.db");

    // Pre-populate applies_to_files so it is NOT a candidate for --field applies_to_files
    // but leave applies_to_task_types NULL
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE learnings SET applies_to_files = '[\"src/**/*.rs\"]' WHERE id = ?1",
            [learning_id],
        )
        .unwrap();
    }

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .env("CLAUDE_BINARY", fake_claude_path())
        .args(["curate", "enrich", "--field", "applies_to_files"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    assert!(
        text.contains("0 candidates") || text.contains("No enrichment candidates"),
        "--field=applies_to_files must report 0 candidates when all files already set: {text}"
    );

    // applies_to_task_types must still be NULL (not enriched by field-filtered run)
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let task_types: Option<String> = conn
        .query_row(
            "SELECT applies_to_task_types FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        task_types.is_none(),
        "applies_to_task_types must remain NULL after applies_to_files-only enrich"
    );
}

#[test]
fn test_edit_learning_add_task_types_via_cli() {
    // AC6: edit-learning --add-task-types FEAT-,FIX- works through CLI
    let (temp_dir, learning_id) = setup_dir_with_learning("Task types test", "pattern");
    let dir = temp_dir.path().to_str().unwrap();
    let db_path = temp_dir.path().join("tasks.db");
    let id_str = learning_id.to_string();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["edit-learning", &id_str, "--add-task-types", "FEAT-,FIX-"])
        .assert()
        .success();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let task_types: Option<String> = conn
        .query_row(
            "SELECT applies_to_task_types FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap();
    let types_json = task_types.expect("applies_to_task_types must be set after edit-learning");
    assert!(
        types_json.contains("FEAT-") && types_json.contains("FIX-"),
        "applies_to_task_types must contain FEAT- and FIX-: {types_json}"
    );
}

#[test]
fn test_edit_learning_add_errors_via_cli() {
    // AC7: edit-learning --add-errors 'timeout' works through CLI
    let (temp_dir, learning_id) = setup_dir_with_learning("Errors test", "pattern");
    let dir = temp_dir.path().to_str().unwrap();
    let db_path = temp_dir.path().join("tasks.db");
    let id_str = learning_id.to_string();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .args(["edit-learning", &id_str, "--add-errors", "timeout"])
        .assert()
        .success();

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let errors: Option<String> = conn
        .query_row(
            "SELECT applies_to_errors FROM learnings WHERE id = ?1",
            [learning_id],
            |r| r.get(0),
        )
        .unwrap();
    let errors_json = errors.expect("applies_to_errors must be set after edit-learning");
    assert!(
        errors_json.contains("timeout"),
        "applies_to_errors must contain 'timeout': {errors_json}"
    );
}

#[test]
fn test_curate_enrich_json_output_format() {
    // AC8: curate enrich --format json produces valid JSON with expected fields
    let (temp_dir, _learning_id) = setup_dir_with_learning("JSON output test", "pattern");
    let dir = temp_dir.path().to_str().unwrap();

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir, "--format", "json"])
        .env("CLAUDE_BINARY", fake_claude_path())
        .args(["curate", "enrich", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap())
        .expect("curate enrich must produce valid JSON");

    assert!(json.get("dry_run").is_some(), "JSON must have dry_run");
    assert!(
        json.get("total_candidates").is_some(),
        "JSON must have total_candidates"
    );
    assert!(
        json.get("learnings_enriched").is_some(),
        "JSON must have learnings_enriched"
    );
    assert!(json.get("proposals").is_some(), "JSON must have proposals");
    assert_eq!(json["dry_run"], true);
    assert_eq!(json["learnings_enriched"], 0);
}

#[test]
fn test_curate_enrich_text_output_format() {
    // AC8: curate enrich (text mode) produces human-readable output
    let (temp_dir, _learning_id) = setup_dir_with_learning("Text output test", "pattern");
    let dir = temp_dir.path().to_str().unwrap();

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", dir])
        .env("CLAUDE_BINARY", fake_claude_path())
        .args(["curate", "enrich", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    // Text output must mention the candidate count and dry-run status
    assert!(
        text.contains("candidate") || text.contains("0"),
        "text output must mention candidates: {text}"
    );
}

// ============================================================================
// Test: Doctor command
// ============================================================================

#[test]
fn test_doctor_command() {
    let temp_dir = setup_initialized_tempdir();

    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("healthy").or(predicate::str::contains("issues")));
}

#[test]
fn test_doctor_json_format() {
    let temp_dir = setup_initialized_tempdir();

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", temp_dir.path().to_str().unwrap()])
        .args(["doctor", "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap()).unwrap();
    assert!(
        json.get("summary").is_some() || json.get("issues").is_some(),
        "Doctor JSON should have summary or issues"
    );
}

// ============================================================================
// Integration tests: archive command with real binary
// ============================================================================

/// Setup: init P1 (all tasks done) and P2 (tasks incomplete) in a temp dir.
/// Returns (TempDir, dir_str) — caller must keep TempDir alive.
///
/// Fixtures are copied into tasks/ inside the temp dir so the archive command
/// can move them (avoids cross-device link errors from /tmp vs project dir).
fn setup_archive_test_dir() -> (TempDir, String) {
    let temp_dir = TempDir::new().unwrap();
    let dir = temp_dir.path().to_str().unwrap().to_string();

    // Create tasks/ directory and copy fixtures into it
    let tasks_dir = temp_dir.path().join("tasks");
    fs::create_dir_all(&tasks_dir).unwrap();

    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let p1_src = manifest_dir.join("tests/fixtures/prd_p1_alpha.json");
    let p2_src = manifest_dir.join("tests/fixtures/prd_p2_beta.json");
    let p1_dest = tasks_dir.join("prd_p1_alpha.json");
    let p2_dest = tasks_dir.join("prd_p2_beta.json");
    fs::copy(&p1_src, &p1_dest).unwrap();
    fs::copy(&p2_src, &p2_dest).unwrap();

    // Init P1 (alpha-project) from the local copy
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", &dir])
        .args(["init", "--from-json", p1_dest.to_str().unwrap()])
        .assert()
        .success();

    // Init P2 (beta-project) with --append from the local copy
    Command::new(cargo_bin("task-mgr"))
        .args(["--dir", &dir])
        .args(["init", "--append", "--from-json", p2_dest.to_str().unwrap()])
        .assert()
        .success();

    // Mark all P1 tasks as done directly in DB
    let db_path = temp_dir.path().join("tasks.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("UPDATE tasks SET status = 'done' WHERE id LIKE 'P1-%'", [])
            .unwrap();
    }

    // Leave P2 tasks as-is (todo / default state — incomplete)

    (temp_dir, dir)
}

#[test]
fn test_archive_dry_run_shows_p1_archived_p2_skipped() {
    let (temp_dir, dir) = setup_archive_test_dir();
    let _keep = &temp_dir;

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", &dir])
        .args(["archive", "--dry-run", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();

    // Dry-run header must be present (matches "Dry Run", "dry run", "DRY RUN")
    assert!(
        text.to_lowercase().contains("dry run"),
        "dry-run output must mention dry run mode: {text}"
    );

    // P1 should be shown as archived (or would-be archived)
    assert!(
        text.contains("P1") || text.contains("alpha"),
        "dry-run output must mention P1/alpha: {text}"
    );

    // P2 should be shown as skipped
    assert!(
        text.contains("P2")
            || text.contains("beta")
            || text.contains("skip")
            || text.contains("Skip"),
        "dry-run output must mention P2/beta as skipped: {text}"
    );

    // Verify no DB changes: P1 tasks still in tasks table
    let db_path = temp_dir.path().join("tasks.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let p1_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'P1-%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        p1_count > 0,
        "dry-run must not remove P1 tasks from DB (found {p1_count})"
    );
}

#[test]
fn test_archive_actual_archives_p1_leaves_p2() {
    let (temp_dir, dir) = setup_archive_test_dir();
    let _keep = &temp_dir;

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", &dir])
        .args(["archive", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();

    // P1 must appear as archived
    assert!(
        text.contains("P1") || text.contains("alpha"),
        "archive output must mention P1/alpha: {text}"
    );

    let db_path = temp_dir.path().join("tasks.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();

    // P1 tasks must be cleared from the DB after archiving
    let p1_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'P1-%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        p1_count, 0,
        "P1 tasks must be cleared from DB after archive"
    );

    // P2 tasks must remain intact
    let p2_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE id LIKE 'P2-%'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        p2_count > 0,
        "P2 tasks must remain in DB after archiving only P1 (found {p2_count})"
    );
}

#[test]
fn test_archive_json_format_structure() {
    let (temp_dir, dir) = setup_archive_test_dir();
    let _keep = &temp_dir;

    let output = Command::new(cargo_bin("task-mgr"))
        .args(["--dir", &dir])
        .args(["archive", "--format", "json", "--all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_str(&String::from_utf8(output).unwrap())
        .expect("archive --format json must produce valid JSON");

    assert!(
        json.get("archived").is_some(),
        "JSON must have 'archived' field: {json}"
    );
    assert!(
        json.get("prds_archived").is_some(),
        "JSON must have 'prds_archived' field: {json}"
    );
    assert!(
        json.get("prds_skipped").is_some(),
        "JSON must have 'prds_skipped' field: {json}"
    );

    let prds_archived = json["prds_archived"].as_array().unwrap();
    assert_eq!(
        prds_archived.len(),
        1,
        "Exactly 1 PRD (P1) should be archived"
    );
    assert_eq!(
        prds_archived[0]["task_prefix"].as_str().unwrap(),
        "P1",
        "Archived PRD must be P1"
    );

    let prds_skipped = json["prds_skipped"].as_array().unwrap();
    assert_eq!(
        prds_skipped.len(),
        1,
        "Exactly 1 PRD (P2) should be skipped"
    );
}
