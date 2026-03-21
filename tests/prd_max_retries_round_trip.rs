//! Integration tests for maxRetries / defaultMaxRetries round-trip fidelity.
//!
//! Tests the full pipeline: parse JSON → import to DB → export → verify.
//!
//! Covers all acceptance criteria for INT-002:
//!   1. JSON with all new fields (including unknown ones) imports without error
//!   2. Resolved max_retries values are correct in DB
//!   3. max_retries survives the import → export round-trip
//!   4. defaultMaxRetries at the PRD level survives the round-trip
//!   5. Old-format JSON (no new fields) imports with default max_retries = 3
//!   6. Per-task maxRetries overrides PRD-level defaultMaxRetries

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

/// Import a fixture and export it; return (TempDir, exported JSON as Value).
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

// ── AC-1, AC-2: DB state after import with all new fields ─────────────────────

/// Verifies correct resolved max_retries values in DB after import.
///
/// Resolution precedence: per-task maxRetries > PRD defaultMaxRetries > 3.
///   MR-001: explicit 7  → DB has 7
///   MR-002: no value    → DB has 5 (from PRD defaultMaxRetries)
///   MR-003: explicit 2  → DB has 2
#[test]
fn test_db_state_after_import_max_retries_resolved_correctly() {
    let temp_dir = TempDir::new().unwrap();
    let prd_path = fixture_path("prd_with_max_retries.json");

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

    // MR-001: explicit maxRetries: 7 overrides PRD default (5)
    let mr001_retries: i32 = conn
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = 'MR-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        mr001_retries, 7,
        "MR-001: explicit maxRetries: 7 must be stored in DB"
    );

    // MR-002: no maxRetries → resolved from PRD defaultMaxRetries: 5
    let mr002_retries: i32 = conn
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = 'MR-002'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        mr002_retries, 5,
        "MR-002: missing maxRetries must resolve to PRD defaultMaxRetries: 5"
    );

    // MR-003: explicit maxRetries: 2 overrides PRD default (5)
    let mr003_retries: i32 = conn
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = 'MR-003'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        mr003_retries, 2,
        "MR-003: explicit maxRetries: 2 must be stored in DB"
    );

    // PRD-level defaultMaxRetries stored in prd_metadata
    let prd_default: Option<i32> = conn
        .query_row(
            "SELECT default_max_retries FROM prd_metadata LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        prd_default,
        Some(5),
        "prd_metadata.default_max_retries must be 5"
    );
}

// ── AC-3, AC-4: per-task maxRetries round-trips through export ────────────────

/// Verifies that maxRetries values survive import → export intact.
///
///   MR-001: 7 in JSON → DB → exported JSON shows maxRetries: 7
///   MR-002: resolved 5 in DB → exported JSON shows maxRetries: 5
///   MR-003: 2 in JSON → DB → exported JSON shows maxRetries: 2
#[test]
fn test_round_trip_max_retries_per_task() {
    let (_temp_dir, exported) = import_and_export("prd_with_max_retries.json");

    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("userStories must be present in exported JSON");

    // MR-001: explicit 7
    let mr001 = get_story(stories, "MR-001");
    assert_eq!(
        mr001.get("maxRetries").and_then(|v| v.as_i64()),
        Some(7),
        "MR-001: maxRetries: 7 must survive the round-trip"
    );

    // MR-002: resolved from PRD default (5)
    let mr002 = get_story(stories, "MR-002");
    assert_eq!(
        mr002.get("maxRetries").and_then(|v| v.as_i64()),
        Some(5),
        "MR-002: resolved maxRetries: 5 must be present in export"
    );

    // MR-003: explicit 2
    let mr003 = get_story(stories, "MR-003");
    assert_eq!(
        mr003.get("maxRetries").and_then(|v| v.as_i64()),
        Some(2),
        "MR-003: maxRetries: 2 must survive the round-trip"
    );
}

// ── AC-4: PRD-level defaultMaxRetries round-trips ────────────────────────────

/// Verifies defaultMaxRetries at the PRD level survives the round-trip.
#[test]
fn test_round_trip_default_max_retries_prd_level() {
    let (_temp_dir, exported) = import_and_export("prd_with_max_retries.json");

    assert_eq!(
        exported.get("defaultMaxRetries").and_then(|v| v.as_i64()),
        Some(5),
        "PRD-level defaultMaxRetries: 5 must survive the round-trip"
    );
}

// ── AC-5: old-format JSON (no new fields) imports with defaults ───────────────

/// Verifies that an old-format JSON missing maxRetries imports successfully
/// with max_retries defaulting to 3 (the hardcoded global default).
#[test]
fn test_old_format_json_imports_with_default_max_retries() {
    let temp_dir = TempDir::new().unwrap();

    // Minimal old-format PRD: no maxRetries, no defaultMaxRetries
    let old_prd = serde_json::json!({
        "project": "old-format-test",
        "branchName": "test/old-format",
        "userStories": [
            {
                "id": "OLD-001",
                "title": "Legacy task without retry fields",
                "priority": 1,
                "passes": false
            },
            {
                "id": "OLD-002",
                "title": "Another legacy task",
                "priority": 2,
                "passes": false
            }
        ]
    });

    let prd_path = temp_dir.path().join("old-format.json");
    fs::write(&prd_path, old_prd.to_string()).unwrap();

    // Import must succeed without any errors
    let init_result = init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();
    assert_eq!(
        init_result.tasks_imported, 2,
        "old-format PRD must import 2 tasks without error"
    );

    // Both tasks must default to max_retries = 3
    let conn = open_connection(temp_dir.path()).unwrap();

    for task_id in &["OLD-001", "OLD-002"] {
        let retries: i32 = conn
            .query_row(
                "SELECT max_retries FROM tasks WHERE id = ?",
                [task_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            retries, 3,
            "task {} without maxRetries must default to max_retries = 3",
            task_id
        );
    }

    // PRD-level default_max_retries must be NULL (not set)
    let prd_default: Option<i32> = conn
        .query_row(
            "SELECT default_max_retries FROM prd_metadata LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        prd_default, None,
        "old-format PRD must have NULL default_max_retries in prd_metadata"
    );
}

// ── defaultMaxRetries absent from JSON when NULL ──────────────────────────────

/// Verifies that defaultMaxRetries is absent from exported JSON when not set
/// (backward compat: old exported JSON should not gain a new field).
#[test]
fn test_default_max_retries_omitted_when_not_set() {
    let temp_dir = TempDir::new().unwrap();

    let prd = serde_json::json!({
        "project": "no-default-retries",
        "userStories": [
            {"id": "X-001", "title": "Task", "priority": 1, "passes": false}
        ]
    });

    let prd_path = temp_dir.path().join("no-default.json");
    fs::write(&prd_path, prd.to_string()).unwrap();

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
    assert!(
        !exported_json.contains("\"defaultMaxRetries\""),
        "defaultMaxRetries must be absent from JSON when not set in original PRD"
    );
}

// ── AC-1: unknown fields (taskType, verifyCommand) silently ignored ───────────

/// Verifies that unknown fields in the JSON (taskType, verifyCommand) do not
/// cause import failures — they are silently ignored by serde.
#[test]
fn test_unknown_fields_in_json_silently_ignored() {
    let temp_dir = TempDir::new().unwrap();

    let prd = serde_json::json!({
        "project": "unknown-fields-test",
        "environmentChecks": ["cargo build", "cargo test"],
        "userStories": [
            {
                "id": "UF-001",
                "title": "Task with unknown fields",
                "priority": 1,
                "passes": false,
                "maxRetries": 4,
                "taskType": "implementation",
                "verifyCommand": "cargo test --test integration",
                "someUnknownField": "should be ignored"
            }
        ]
    });

    let prd_path = temp_dir.path().join("unknown-fields.json");
    fs::write(&prd_path, prd.to_string()).unwrap();

    // Import must not fail despite unknown fields
    let init_result = init::init(
        temp_dir.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();
    assert_eq!(
        init_result.tasks_imported, 1,
        "import must succeed even with unknown fields present"
    );

    // Known field (maxRetries) must be correctly stored
    let conn = open_connection(temp_dir.path()).unwrap();
    let retries: i32 = conn
        .query_row(
            "SELECT max_retries FROM tasks WHERE id = 'UF-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        retries, 4,
        "maxRetries: 4 must be stored even when unknown fields are present"
    );
}

// ── AC-6: full round-trip: insert → read → verify ────────────────────────────

/// Full pipeline: import → export → re-import → re-export → values identical.
///
/// This is the most comprehensive round-trip test. After two import/export
/// cycles, all maxRetries values must match.
#[test]
fn test_full_pipeline_reimport_preserves_max_retries() {
    let temp_dir1 = TempDir::new().unwrap();
    let prd_path = fixture_path("prd_with_max_retries.json");

    // First import
    init::init(
        temp_dir1.path(),
        &[&prd_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // First export
    let export1_path = temp_dir1.path().join("export1.json");
    export::export(temp_dir1.path(), &export1_path, false, None).unwrap();

    // Second import from first export (simulates crash-recovery)
    let temp_dir2 = TempDir::new().unwrap();
    init::init(
        temp_dir2.path(),
        &[&export1_path],
        false,
        false,
        false,
        false,
        init::PrefixMode::Disabled,
    )
    .unwrap();

    // Second export
    let export2_path = temp_dir2.path().join("export2.json");
    export::export(temp_dir2.path(), &export2_path, false, None).unwrap();

    // Compare both exports for max_retries fields
    let json1: Value = serde_json::from_str(&fs::read_to_string(&export1_path).unwrap()).unwrap();
    let json2: Value = serde_json::from_str(&fs::read_to_string(&export2_path).unwrap()).unwrap();

    // PRD-level defaultMaxRetries must match
    assert_eq!(
        json1.get("defaultMaxRetries"),
        json2.get("defaultMaxRetries"),
        "defaultMaxRetries must be identical after two export cycles"
    );

    let stories1 = json1.get("userStories").and_then(|v| v.as_array()).unwrap();
    let stories2 = json2.get("userStories").and_then(|v| v.as_array()).unwrap();

    assert_eq!(stories1.len(), stories2.len(), "story count must match");

    for (s1, s2) in stories1.iter().zip(stories2.iter()) {
        let id = s1.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        assert_eq!(
            s1.get("maxRetries"),
            s2.get("maxRetries"),
            "maxRetries mismatch for task {} after two export cycles",
            id
        );
    }
}

// ── Partial fields: some tasks have maxRetries, some don't ───────────────────

/// Mixed PRD: some tasks have explicit maxRetries, others rely on PRD default.
/// Verifies the correct resolution ordering for each task.
#[test]
fn test_partial_max_retries_resolution() {
    let temp_dir = TempDir::new().unwrap();

    // PRD with defaultMaxRetries: 4; one task has explicit override, one doesn't
    let prd = serde_json::json!({
        "project": "partial-retries",
        "defaultMaxRetries": 4,
        "userStories": [
            {
                "id": "P-001",
                "title": "Task with explicit maxRetries: 1",
                "priority": 1,
                "passes": false,
                "maxRetries": 1
            },
            {
                "id": "P-002",
                "title": "Task without explicit maxRetries",
                "priority": 2,
                "passes": false
            }
        ]
    });

    let prd_path = temp_dir.path().join("partial-retries.json");
    fs::write(&prd_path, prd.to_string()).unwrap();

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

    let exported: Value = serde_json::from_str(&fs::read_to_string(&export_path).unwrap()).unwrap();
    let stories = exported
        .get("userStories")
        .and_then(|v| v.as_array())
        .expect("userStories must be present");

    // P-001: explicit 1
    let p001 = get_story(stories, "P-001");
    assert_eq!(
        p001.get("maxRetries").and_then(|v| v.as_i64()),
        Some(1),
        "P-001: explicit maxRetries: 1 must survive round-trip"
    );

    // P-002: inherits PRD default (4)
    let p002 = get_story(stories, "P-002");
    assert_eq!(
        p002.get("maxRetries").and_then(|v| v.as_i64()),
        Some(4),
        "P-002: resolved maxRetries: 4 (from PRD default) must be in export"
    );
}

// ── Exported JSON uses camelCase for maxRetries ───────────────────────────────

/// Verifies the exported JSON key is "maxRetries" (camelCase), not "max_retries".
#[test]
fn test_exported_json_uses_camel_case_max_retries() {
    let (_temp_dir, exported) = import_and_export("prd_with_max_retries.json");

    let raw_json = serde_json::to_string_pretty(&exported).unwrap();

    assert!(
        raw_json.contains("\"maxRetries\""),
        "exported JSON must use camelCase 'maxRetries', not snake_case"
    );
    assert!(
        !raw_json.contains("\"max_retries\""),
        "exported JSON must NOT use snake_case 'max_retries'"
    );
    assert!(
        raw_json.contains("\"defaultMaxRetries\""),
        "exported JSON must use camelCase 'defaultMaxRetries'"
    );
    assert!(
        !raw_json.contains("\"default_max_retries\""),
        "exported JSON must NOT use snake_case 'default_max_retries'"
    );
}
