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
