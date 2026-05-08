//! Contract tests for `task_mgr::loop_engine::prompt::core`.
//!
//! TDD scaffolding for FEAT-001 (the prompt-builder split). These tests
//! pin the public surface and degradation behavior of the bedrock prompt
//! helpers BEFORE the implementations land. They MUST fail against the
//! current empty-string stubs and pass once the helpers are wired up.
//!
//! Notes for future maintainers:
//! - This file is an integration test, so it cannot use the
//!   `pub(crate)` `loop_engine::test_utils` helpers (per learning #896).
//!   Setup goes through the public API.
//! - The "discriminator" assertions explicitly distinguish a stub that
//!   returns `String::new()` for everything from a real implementation
//!   (per the task acceptance criteria).
//! - Migration-aware DB setup uses `open_connection` + `create_schema` +
//!   `run_migrations` (per CLAUDE.md note about supersession-aware tests).

use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::learnings::crud::{RecordLearningParams, record_learning};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::prompt::core::{
    build_key_decisions_block, build_learnings_block, build_source_context_block,
    build_tool_awareness_block, completion_instruction, format_task_json,
};
use task_mgr::models::{Confidence, LearningOutcome, Task};

const LEARNINGS_BUDGET: usize = 4_000;
const SOURCE_BUDGET: usize = 2_000;

/// Open a DB with full schema + all migrations applied.
///
/// The TempDir return value must outlive the Connection — dropping it
/// removes the on-disk database file mid-test.
fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

/// Open a DB with schema only — NO migrations. Used to provoke retrieval
/// errors in the learnings backend (the `learnings_fts` virtual table is
/// created by migration v8, not the base schema).
fn setup_unmigrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    (temp, conn)
}

fn sample_task() -> Task {
    let mut task = Task::new("TEST-CORE-001", "Validate prompt::core helpers");
    task.description = Some("Ensure shared section helpers compose correctly.".into());
    task.acceptance_criteria = vec!["Round-trip JSON works".into(), "Edge cases degrade".into()];
    task.notes = Some("TDD scaffolding".into());
    task.difficulty = Some("medium".into());
    task
}

// ---------------------------------------------------------------------------
// AC: Happy-path test for format_task_json with task_id/title/files populated
// AC: Invariant test: format_task_json output parses back via serde_json::from_str
// ---------------------------------------------------------------------------

#[test]
#[ignore = "FEAT-001 implements format_task_json (stub returns empty)"]
fn format_task_json_includes_id_title_and_files() {
    let task = sample_task();
    let files = vec!["src/foo.rs".to_string(), "src/bar.rs".to_string()];

    let json_str = format_task_json(&task, &files);

    assert!(
        !json_str.is_empty(),
        "format_task_json must not return empty (stub discriminator)"
    );

    // Round-trip: must be parseable JSON.
    let value: serde_json::Value = serde_json::from_str(&json_str)
        .expect("format_task_json output must be valid JSON that round-trips via serde_json");

    assert_eq!(
        value.get("id").and_then(|v| v.as_str()),
        Some("TEST-CORE-001"),
        "id field must be present and equal to the Task id"
    );
    assert_eq!(
        value.get("title").and_then(|v| v.as_str()),
        Some("Validate prompt::core helpers"),
        "title field must be present and equal to the Task title"
    );

    let files_val = value
        .get("files")
        .expect("files field must be present in JSON");
    let files_arr = files_val
        .as_array()
        .expect("files must serialize as a JSON array");
    let file_strs: Vec<&str> = files_arr.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        file_strs.contains(&"src/foo.rs") && file_strs.contains(&"src/bar.rs"),
        "files JSON must include every entry from the input slice; got {file_strs:?}"
    );
}

#[test]
#[ignore = "FEAT-001 implements format_task_json (stub returns empty)"]
fn format_task_json_round_trips_with_empty_files() {
    let task = sample_task();
    let json_str = format_task_json(&task, &[]);

    assert!(!json_str.is_empty(), "stub discriminator");
    let value: serde_json::Value =
        serde_json::from_str(&json_str).expect("must round-trip even with empty files");
    assert_eq!(
        value.get("id").and_then(|v| v.as_str()),
        Some(task.id.as_str())
    );
}

// ---------------------------------------------------------------------------
// AC: Happy-path test for completion_instruction includes task_id and title
// ---------------------------------------------------------------------------

#[test]
#[ignore = "FEAT-001 implements completion_instruction (stub returns empty)"]
fn completion_instruction_mentions_id_and_title() {
    let out = completion_instruction("TEST-CORE-001", "Validate prompt::core helpers");

    assert!(
        !out.is_empty(),
        "completion_instruction must not return empty (stub discriminator)"
    );
    assert!(
        out.contains("TEST-CORE-001"),
        "completion instruction must reference the task id; got: {out}"
    );
    assert!(
        out.contains("Validate prompt::core helpers"),
        "completion instruction must reference the task title; got: {out}"
    );
}

// ---------------------------------------------------------------------------
// AC: Happy-path / known-bad-discriminator test for build_learnings_block
// ---------------------------------------------------------------------------

#[test]
#[ignore = "FEAT-001 implements build_learnings_block (stub returns empty)"]
fn build_learnings_block_renders_recalled_learning() {
    let (_tmp, conn) = setup_migrated_db();

    // Insert a learning that should match a TEST-* task — use a permissive
    // pattern so we don't depend on internal recall scoring details.
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "prompt-core helpers must round-trip JSON".to_string(),
        content: "format_task_json output is consumed by Claude CLI; \
                  must remain valid JSON for downstream tooling."
            .to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/loop_engine/prompt/core.rs".to_string()]),
        applies_to_task_types: Some(vec!["TEST-".to_string(), "FEAT-".to_string()]),
        applies_to_errors: None,
        tags: Some(vec!["prompt".to_string(), "core".to_string()]),
        confidence: Confidence::High,
    };
    let inserted = record_learning(&conn, params)
        .expect("record_learning")
        .learning_id;

    let task = sample_task();
    let (block, shown_ids) = build_learnings_block(&conn, &task, LEARNINGS_BUDGET);

    // This is the stub discriminator: a real impl renders a non-empty block
    // when at least one matching learning exists.
    assert!(
        !block.is_empty(),
        "build_learnings_block must render a non-empty section when matching learnings exist; \
         a stub returning String::new() must NOT pass this test"
    );

    // The block must reference the recalled learning meaningfully — title is
    // the most stable proxy across rendering format changes.
    assert!(
        block.contains("prompt-core helpers must round-trip JSON"),
        "rendered learnings block should contain the recalled learning's title; got:\n{block}"
    );

    // shown_ids must reflect what was actually shown; if the block is
    // populated the inserted ID should be present (or at minimum the vec
    // should not be empty — the second assertion is the looser invariant).
    assert!(
        !shown_ids.is_empty(),
        "shown_ids must not be empty when build_learnings_block renders content"
    );
    assert!(
        shown_ids.contains(&inserted),
        "shown_ids must contain the recalled learning id ({inserted}); got {shown_ids:?}"
    );
}

#[test]
fn build_learnings_block_returns_empty_on_retrieval_error() {
    // Schema present but migrations skipped → `learnings_fts` does NOT exist.
    // Recall paths that touch FTS5 must fail gracefully (empty result, not panic).
    let (_tmp, conn) = setup_unmigrated_db();
    let task = sample_task();

    let (block, shown_ids) = build_learnings_block(&conn, &task, LEARNINGS_BUDGET);

    assert_eq!(
        block, "",
        "build_learnings_block must return an empty section on retrieval error \
         (e.g. missing FTS5 table on a fresh DB); got: {block}"
    );
    assert!(
        shown_ids.is_empty(),
        "shown_ids must be empty when retrieval fails; got {shown_ids:?}"
    );
}

// ---------------------------------------------------------------------------
// AC: Edge-case test: build_source_context_block returns '' when project_root
// does not exist
// ---------------------------------------------------------------------------

#[test]
fn build_source_context_block_empty_when_project_root_missing() {
    // Pick a path under /tmp that we are confident does not exist.
    let nonexistent = Path::new("/tmp/task-mgr-test-prompt-core-nonexistent-root-xyzzy");
    assert!(
        !nonexistent.exists(),
        "test setup error: chosen 'nonexistent' path actually exists"
    );

    let touches = vec!["src/foo.rs".to_string(), "src/bar.rs".to_string()];
    let block = build_source_context_block(&touches, SOURCE_BUDGET, nonexistent);

    assert_eq!(
        block, "",
        "build_source_context_block must return '' when project_root does not exist; got: {block}"
    );
}

#[test]
fn build_source_context_block_empty_for_empty_touches() {
    // A trivially-true degradation: no files → no block. Real implementations
    // should short-circuit, and an empty stub passes coincidentally — that's
    // intentional, this test just pins the behavior.
    let temp = TempDir::new().expect("tempdir");
    let block = build_source_context_block(&[], SOURCE_BUDGET, temp.path());
    assert_eq!(block, "", "empty touches must produce empty block");
}

// ---------------------------------------------------------------------------
// AC: Happy-path tests for build_tool_awareness_block and build_key_decisions_block
// (round out the helper coverage; both also serve as stub discriminators).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "FEAT-001 implements build_tool_awareness_block (stub returns empty)"]
fn build_tool_awareness_block_renders_for_dangerous_mode() {
    let block = build_tool_awareness_block(&PermissionMode::Dangerous);

    assert!(
        !block.is_empty(),
        "build_tool_awareness_block must render content for Dangerous mode (stub discriminator)"
    );
    assert!(
        block.to_ascii_lowercase().contains("tool"),
        "tool-awareness block must mention tools; got: {block}"
    );
}

#[test]
#[ignore = "FEAT-001 implements build_tool_awareness_block (stub returns empty)"]
fn build_tool_awareness_block_renders_for_auto_mode() {
    let block = build_tool_awareness_block(&PermissionMode::Auto {
        allowed_tools: None,
    });
    assert!(
        !block.is_empty(),
        "build_tool_awareness_block must render content for Auto mode"
    );
}

#[test]
#[ignore = "FEAT-001 implements build_key_decisions_block (stub returns empty)"]
fn build_key_decisions_block_includes_key_decision_marker() {
    let task = sample_task();
    let block = build_key_decisions_block(&task.id);

    assert!(
        !block.is_empty(),
        "build_key_decisions_block must render content (stub discriminator)"
    );
    assert!(
        block.contains("key-decision"),
        "key-decisions block must mention the <key-decision> tag format; got:\n{block}"
    );
}

#[test]
#[ignore = "FEAT-001 implements build_key_decisions_block (stub returns empty)"]
fn build_key_decisions_block_emphasizes_review_tasks() {
    let mut task = sample_task();
    task.id = "TEST-REVIEW-001".to_string();

    let review_block = build_key_decisions_block(&task.id);
    let normal_block = build_key_decisions_block(&sample_task().id);

    assert!(!review_block.is_empty(), "review block must not be empty");
    assert!(
        review_block.len() > normal_block.len(),
        "review/verify task IDs should produce additional emphasis content; \
         review_len={} normal_len={}",
        review_block.len(),
        normal_block.len()
    );
}
