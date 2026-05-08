//! Comprehensive tests for `task_mgr::loop_engine::prompt::core`.
//!
//! Covers edge cases, large inputs, unicode, budget trimming, and
//! `load_base_prompt` degradation exercised via `slot::build_prompt`.
//!
//! Integration test conventions (per learnings #896, #901):
//! - Cannot use `pub(crate)` test_utils; DB setup goes through the public API.
//! - Imports at module level, not inside individual test bodies (learning #907).

use std::path::PathBuf;

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
use task_mgr::loop_engine::prompt::slot::{SlotPromptBundle, SlotPromptParams};
use task_mgr::models::{Confidence, LearningOutcome, Task};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
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

fn insert_matching_learning(conn: &Connection, title: &str, content: &str) -> i64 {
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: title.to_string(),
        content: content.to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/loop_engine/prompt/core.rs".to_string()]),
        applies_to_task_types: Some(vec!["TEST-".to_string()]),
        applies_to_errors: None,
        tags: Some(vec!["prompt".to_string()]),
        confidence: Confidence::High,
    };
    record_learning(conn, params)
        .expect("record_learning")
        .learning_id
}

// ---------------------------------------------------------------------------
// format_task_json — edge cases
// ---------------------------------------------------------------------------

#[test]
fn format_task_json_includes_all_optional_fields_when_set() {
    let mut task = Task::new("OPT-001", "Optional fields task");
    task.description = Some("A description".into());
    task.notes = Some("Some notes".into());
    task.model = Some("claude-opus-4-5".into());
    task.difficulty = Some("high".into());
    task.escalation_note = Some("Escalation reason".into());

    let json_str = format_task_json(&task, &["src/foo.rs".to_string()]);
    let value: serde_json::Value = serde_json::from_str(&json_str).expect("must be valid JSON");

    assert_eq!(value["description"].as_str(), Some("A description"));
    assert_eq!(value["notes"].as_str(), Some("Some notes"));
    assert_eq!(value["model"].as_str(), Some("claude-opus-4-5"));
    assert_eq!(value["difficulty"].as_str(), Some("high"));
    assert_eq!(value["escalationNote"].as_str(), Some("Escalation reason"));
}

#[test]
fn format_task_json_omits_absent_optional_fields() {
    let task = Task::new("MIN-001", "Minimal task");
    let json_str = format_task_json(&task, &[]);
    let value: serde_json::Value = serde_json::from_str(&json_str).expect("must be valid JSON");

    assert!(
        value.get("description").is_none(),
        "description must be absent when None"
    );
    assert!(
        value.get("notes").is_none(),
        "notes must be absent when None"
    );
    assert!(
        value.get("model").is_none(),
        "model must be absent when None"
    );
    assert!(
        value.get("escalationNote").is_none(),
        "escalationNote must be absent when None"
    );
}

#[test]
fn format_task_json_large_description_produces_valid_json() {
    let large_desc = "A".repeat(12_000); // > 10 KB
    let mut task = Task::new("LARGE-001", "Large description task");
    task.description = Some(large_desc.clone());

    let json_str = format_task_json(&task, &[]);

    let value: serde_json::Value =
        serde_json::from_str(&json_str).expect("must produce valid JSON even for >10 KB input");

    let desc = value["description"]
        .as_str()
        .expect("description must be present");
    assert_eq!(
        desc.len(),
        12_000,
        "format_task_json must not truncate the description; got {} bytes",
        desc.len()
    );
}

#[test]
fn format_task_json_unicode_and_emoji_round_trip_correctly() {
    let unicode_title = "Task 🚀 with unicode 日本語 and accents éàü";
    let mut task = Task::new("UNI-001", unicode_title);
    task.description = Some("Content with emoji 🎉 and CJK: 中文".into());

    let json_str = format_task_json(&task, &[]);

    let value: serde_json::Value =
        serde_json::from_str(&json_str).expect("unicode must produce valid JSON");

    assert_eq!(
        value["title"].as_str(),
        Some(unicode_title),
        "unicode/emoji title must survive JSON escaping and round-trip unchanged"
    );
    assert_eq!(
        value["description"].as_str(),
        Some("Content with emoji 🎉 and CJK: 中文"),
        "unicode description must survive JSON escaping"
    );
}

#[test]
fn format_task_json_multiple_files_all_present() {
    let task = Task::new("FILES-001", "Multi-file task");
    let files = vec![
        "src/a.rs".to_string(),
        "src/b.rs".to_string(),
        "tests/c.rs".to_string(),
    ];

    let json_str = format_task_json(&task, &files);
    let value: serde_json::Value = serde_json::from_str(&json_str).expect("must be valid JSON");

    let arr = value["files"].as_array().expect("files must be an array");
    let file_strs: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();

    assert_eq!(file_strs.len(), 3, "all three files must be in output");
    assert!(file_strs.contains(&"src/a.rs"));
    assert!(file_strs.contains(&"src/b.rs"));
    assert!(file_strs.contains(&"tests/c.rs"));
}

// ---------------------------------------------------------------------------
// completion_instruction — content checks
// ---------------------------------------------------------------------------

#[test]
fn completion_instruction_references_task_id_and_title() {
    let out = completion_instruction("COMP-001", "My important task");

    assert!(
        out.contains("COMP-001"),
        "completion instruction must contain the task id; got:\n{out}"
    );
    assert!(
        out.contains("My important task"),
        "completion instruction must contain the task title; got:\n{out}"
    );
}

#[test]
fn completion_instruction_contains_completed_tag_format() {
    let out = completion_instruction("COMP-002", "Another task");

    assert!(
        out.contains("<completed>COMP-002</completed>"),
        "completion instruction must show the <completed> tag with the task id; got:\n{out}"
    );
}

#[test]
fn completion_instruction_contains_feat_commit_format() {
    let out = completion_instruction("COMP-003", "Feature task");

    assert!(
        out.contains("feat: COMP-003-completed"),
        "completion instruction must include the 'feat: <id>-completed' commit template; got:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// build_learnings_block — edge cases
// ---------------------------------------------------------------------------

#[test]
fn build_learnings_block_empty_when_no_matching_learnings_exist() {
    // Migrated DB but no learnings at all.
    let (_tmp, conn) = setup_migrated_db();
    let task = sample_task();

    let (block, shown_ids) = build_learnings_block(&conn, &task, 4_000);

    assert_eq!(
        block, "",
        "build_learnings_block must return empty string when no learnings match"
    );
    assert!(
        shown_ids.is_empty(),
        "shown_ids must be empty when no matching learnings exist; got {shown_ids:?}"
    );
}

#[test]
fn build_learnings_block_renders_learning_title_in_block() {
    let (_tmp, conn) = setup_migrated_db();
    let learning_id = insert_matching_learning(
        &conn,
        "core.rs helpers must compose without diverging",
        "Use prompt::core helpers for both sequential and slot builders to keep parity.",
    );

    let task = sample_task();
    let (block, shown_ids) = build_learnings_block(&conn, &task, 4_000);

    assert!(
        !block.is_empty(),
        "block must be non-empty when a matching learning exists"
    );
    assert!(
        block.contains("core.rs helpers must compose without diverging"),
        "block must contain the recalled learning title; got:\n{block}"
    );
    assert!(
        shown_ids.contains(&learning_id),
        "shown_ids must contain the recalled learning id {learning_id}; got {shown_ids:?}"
    );
}

#[test]
fn build_learnings_block_budget_trims_output_to_budget() {
    let (_tmp, conn) = setup_migrated_db();

    // Insert a learning with very large content so the rendered section exceeds the budget.
    insert_matching_learning(
        &conn,
        "Verbose learning for budget test",
        &"x".repeat(10_000),
    );

    let task = sample_task();
    let tiny_budget = 200_usize;
    let (block, shown_ids) = build_learnings_block(&conn, &task, tiny_budget);

    assert!(
        !shown_ids.is_empty(),
        "the large learning must still be recalled even with a tiny budget"
    );
    // truncate_to_budget appends "...\n[truncated to N bytes]" (≈28 bytes overhead)
    // when it cuts the text, so the output length is ≤ budget + that notice.
    let notice_overhead = format!("...\n[truncated to {tiny_budget} bytes]").len();
    assert!(
        block.len() <= tiny_budget + notice_overhead,
        "block must be trimmed to roughly the budget ({tiny_budget} bytes + overhead); actual len={}",
        block.len()
    );
}

#[test]
fn build_learnings_block_shown_ids_captures_multiple_learnings() {
    let (_tmp, conn) = setup_migrated_db();

    let id1 = insert_matching_learning(&conn, "Learning alpha", "Content for alpha.");
    let id2 = insert_matching_learning(&conn, "Learning beta", "Content for beta.");

    let task = sample_task();
    let (block, shown_ids) = build_learnings_block(&conn, &task, 4_000);

    assert!(!block.is_empty(), "block must be non-empty");
    // The recall backend may return both or just the highest-scored ones;
    // at least one must be captured.
    assert!(
        !shown_ids.is_empty(),
        "shown_ids must be non-empty when learnings are returned"
    );
    // All returned IDs must be from the inserted set.
    for id in &shown_ids {
        assert!(
            *id == id1 || *id == id2,
            "shown_ids contains unexpected id {id}; expected one of [{id1}, {id2}]"
        );
    }
}

// ---------------------------------------------------------------------------
// build_source_context_block — real-file scan
// ---------------------------------------------------------------------------

#[test]
fn build_source_context_block_returns_content_for_existing_source_file() {
    // The test runs inside the project tree so core.rs definitely exists.
    let project_root = std::env::current_dir().expect("current_dir");
    let touches = vec!["src/loop_engine/prompt/core.rs".to_string()];

    let block = build_source_context_block(&touches, 2_000, &project_root);

    assert!(
        !block.is_empty(),
        "should return non-empty block when source files exist; project_root={project_root:?}"
    );
}

// ---------------------------------------------------------------------------
// build_tool_awareness_block — all PermissionMode variants
// ---------------------------------------------------------------------------

#[test]
fn build_tool_awareness_block_dangerous_mode_mentions_tools() {
    let block = build_tool_awareness_block(&PermissionMode::Dangerous);

    assert!(
        !block.is_empty(),
        "Dangerous mode must produce a non-empty block"
    );
    assert!(
        block.to_ascii_lowercase().contains("tool"),
        "tool-awareness block must mention 'tool'; got:\n{block}"
    );
}

#[test]
fn build_tool_awareness_block_auto_mode_with_tools_non_empty() {
    let block = build_tool_awareness_block(&PermissionMode::Auto {
        allowed_tools: Some("Read,Write,Bash(cargo:*)".to_string()),
    });

    assert!(
        !block.is_empty(),
        "Auto mode with tools must produce a non-empty block"
    );
}

#[test]
fn build_tool_awareness_block_auto_mode_no_tools_non_empty() {
    let block = build_tool_awareness_block(&PermissionMode::Auto {
        allowed_tools: None,
    });

    assert!(
        !block.is_empty(),
        "Auto mode without tools must still produce a block"
    );
}

#[test]
fn build_tool_awareness_block_scoped_with_tools_reports_count_and_bash_prefix() {
    let tools = "Read,Write,Bash(cargo:*),Bash(git:*)";
    let block = build_tool_awareness_block(&PermissionMode::Scoped {
        allowed_tools: Some(tools.to_string()),
    });

    assert!(
        !block.is_empty(),
        "Scoped mode with allowed_tools must produce a non-empty block"
    );
    // The rendered block reports how many tools are pre-approved.
    assert!(
        block.contains('4') || block.to_ascii_lowercase().contains("tool"),
        "block must mention the tool count or 'tool'; got:\n{block}"
    );
    // Bash-prefix scoping must be surfaced.
    assert!(
        block.contains("cargo") || block.contains("git"),
        "block must list the Bash prefix scopes (cargo, git); got:\n{block}"
    );
}

#[test]
fn build_tool_awareness_block_scoped_none_returns_empty_string() {
    let block = build_tool_awareness_block(&PermissionMode::Scoped {
        allowed_tools: None,
    });

    assert_eq!(
        block, "",
        "Scoped(None) must return an empty string (text-only mode, no tools to advertise)"
    );
}

// ---------------------------------------------------------------------------
// build_key_decisions_block — normal vs. review/verify task IDs
// ---------------------------------------------------------------------------

#[test]
fn build_key_decisions_block_contains_key_decision_tag_marker() {
    let task = sample_task();
    let block = build_key_decisions_block(&task);

    assert!(!block.is_empty(), "key-decisions block must not be empty");
    assert!(
        block.contains("key-decision"),
        "block must mention the <key-decision> tag; got:\n{block}"
    );
}

#[test]
fn build_key_decisions_block_review_task_id_adds_emphasis() {
    let mut review_task = sample_task();
    review_task.id = "PRJ-CODE-REVIEW-001".to_string();

    let normal_block = build_key_decisions_block(&sample_task());
    let review_block = build_key_decisions_block(&review_task);

    assert!(
        review_block.len() > normal_block.len(),
        "REVIEW task IDs must produce a longer block (extra architectural emphasis); \
         review_len={} normal_len={}",
        review_block.len(),
        normal_block.len()
    );
}

#[test]
fn build_key_decisions_block_verify_task_id_adds_emphasis() {
    let mut verify_task = sample_task();
    verify_task.id = "PRJ-VERIFY-001".to_string();

    let normal_block = build_key_decisions_block(&sample_task());
    let verify_block = build_key_decisions_block(&verify_task);

    assert!(
        verify_block.len() > normal_block.len(),
        "VERIFY task IDs must also receive extra emphasis; \
         verify_len={} normal_len={}",
        verify_block.len(),
        normal_block.len()
    );
}

// ---------------------------------------------------------------------------
// slot::build_prompt — exercises load_base_prompt graceful degradation
// ---------------------------------------------------------------------------

#[test]
fn slot_build_prompt_missing_base_prompt_path_degrades_gracefully() {
    let (_tmp, conn) = setup_migrated_db();
    let task = sample_task();

    let params = SlotPromptParams {
        project_root: PathBuf::from("/tmp/nonexistent-project-root-xyzzy"),
        base_prompt_path: PathBuf::from("/tmp/nonexistent-base-prompt-xyzzy.md"),
        permission_mode: PermissionMode::Dangerous,
    };

    let bundle: SlotPromptBundle =
        task_mgr::loop_engine::prompt::slot::build_prompt(&conn, &task, &params);

    assert!(
        !bundle.prompt.is_empty(),
        "prompt must be non-empty even when base_prompt_path does not exist"
    );
    assert!(
        bundle.prompt.contains("## Current Task"),
        "prompt must contain the task section even without a base prompt; got:\n{}",
        &bundle.prompt[..bundle.prompt.len().min(500)]
    );
    assert_eq!(
        bundle.task_id, task.id,
        "bundle.task_id must equal the task id"
    );
}

#[test]
fn slot_build_prompt_with_real_base_prompt_file_includes_content() {
    let (tmp, conn) = setup_migrated_db();
    let task = sample_task();

    // Write a small base prompt file so load_base_prompt can read it.
    let base_prompt_path = tmp.path().join("prompt.md");
    std::fs::write(
        &base_prompt_path,
        "## Extra Instructions\n\nFollow best practices.\n",
    )
    .expect("write base prompt");

    let params = SlotPromptParams {
        project_root: PathBuf::from("/tmp/nonexistent-project-root-xyzzy"),
        base_prompt_path,
        permission_mode: PermissionMode::Dangerous,
    };

    let bundle = task_mgr::loop_engine::prompt::slot::build_prompt(&conn, &task, &params);

    assert!(
        bundle.prompt.contains("## Extra Instructions"),
        "prompt must include content from the base prompt file; got:\n{}",
        &bundle.prompt[..bundle.prompt.len().min(500)]
    );
}

#[test]
fn slot_build_prompt_large_base_prompt_is_truncated() {
    let (tmp, conn) = setup_migrated_db();
    let task = sample_task();

    // Write a base prompt larger than BASE_PROMPT_BUDGET (16_000 bytes).
    let large_content = format!("## Start\n\n{}\n## End\n", "y".repeat(20_000));
    let base_prompt_path = tmp.path().join("large_prompt.md");
    std::fs::write(&base_prompt_path, &large_content).expect("write large base prompt");

    let params = SlotPromptParams {
        project_root: PathBuf::from("/tmp/nonexistent-project-root-xyzzy"),
        base_prompt_path,
        permission_mode: PermissionMode::Dangerous,
    };

    let bundle = task_mgr::loop_engine::prompt::slot::build_prompt(&conn, &task, &params);

    // The base prompt budget is 16_000 bytes. The total prompt will include
    // other sections too, but the base prompt contribution must be <= budget.
    // We verify by checking the "## End" marker is missing (truncated before it).
    assert!(
        !bundle.prompt.contains("## End"),
        "a 20 KB base prompt must be truncated before the '## End' marker"
    );
}
