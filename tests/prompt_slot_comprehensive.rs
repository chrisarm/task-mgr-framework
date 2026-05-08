//! Comprehensive tests for `task_mgr::loop_engine::prompt::slot::build_prompt`.
//!
//! Covers task-shape variations (empty / single / multiple touchesFiles), learning
//! budget enforcement, source-context presence/absence, and the compile-time Send
//! regression guard.
//!
//! Integration test conventions (per learnings #896, #901):
//! - Cannot use `pub(crate)` test_utils; DB setup goes through the public API.
//! - Imports at module level, not inside individual test bodies (learning #907).

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use static_assertions::assert_impl_all;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::learnings::crud::{RecordLearningParams, record_learning};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::model::SONNET_MODEL;
use task_mgr::loop_engine::prompt::slot::{SlotPromptBundle, SlotPromptParams, build_prompt};
use task_mgr::models::{Confidence, LearningOutcome, Task};

// ---------------------------------------------------------------------------
// Compile-time regression guard
// ---------------------------------------------------------------------------

// SlotPromptBundle must remain Send so it can cross the worker-thread boundary.
// Adding any Rc, RefCell, or MutexGuard field breaks this assertion at compile
// time rather than at runtime.
assert_impl_all!(SlotPromptBundle: Send);

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

fn sample_task(id: &str) -> Task {
    let mut task = Task::new(id, "Comprehensive slot prompt test task");
    task.description = Some("Testing SlotPromptBundle composition.".into());
    task.acceptance_criteria = vec!["Bundle is correct".into()];
    task.difficulty = Some("medium".into());
    task
}

/// Write real files in a tempdir and return the dir handle.
fn project_with_files(touches: &[(&str, &str)]) -> TempDir {
    let temp = TempDir::new().expect("project tempdir");
    for (rel_path, contents) in touches {
        let abs = temp.path().join(rel_path);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(&abs, contents).expect("write file");
    }
    temp
}

fn make_params(project_root: PathBuf, base_prompt_path: PathBuf) -> SlotPromptParams {
    SlotPromptParams {
        project_root,
        base_prompt_path,
        permission_mode: PermissionMode::Dangerous,
    }
}

/// Insert a task row and associated task_files rows into the DB so
/// `load_task_files` has data to return.
fn insert_task_with_files(conn: &Connection, task_id: &str, files: &[&str]) {
    conn.execute(
        "INSERT OR IGNORE INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        [task_id, "test task"],
    )
    .expect("insert task");
    for file in files {
        conn.execute(
            "INSERT INTO task_files (task_id, file_path) VALUES (?1, ?2)",
            [task_id, file],
        )
        .expect("insert task_files row");
    }
}

/// Insert a learning that matches TEST-* task types so the recall backend
/// surfaces it for sample tasks whose IDs start with "TEST-".
fn insert_matching_learning(conn: &Connection, title: &str, content: &str) -> i64 {
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: title.to_string(),
        content: content.to_string(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/loop_engine/prompt/slot.rs".into()]),
        applies_to_task_types: Some(vec!["TEST-".into()]),
        applies_to_errors: None,
        tags: Some(vec!["slot".into()]),
        confidence: Confidence::High,
    };
    record_learning(conn, params)
        .expect("record_learning")
        .learning_id
}

// ---------------------------------------------------------------------------
// AC 1a: empty touchesFiles → task_files empty, source section absent
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_empty_touches_files_yields_empty_task_files_and_no_source_section() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    // No task_files rows inserted — touchesFiles is empty.
    let task = sample_task("TEST-SLOT-EMPTY-001");
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        bundle.task_files.is_empty(),
        "task_files must be empty when the task has no associated task_files rows; \
         got {:?}",
        bundle.task_files
    );
    assert!(
        !bundle.prompt.contains("## Current Source Context"),
        "prompt must NOT contain the source-context section when task_files is empty; \
         source section was dropped (got {} bytes prompt)",
        bundle.prompt.len()
    );
}

// ---------------------------------------------------------------------------
// AC 1b: single touchesFile → task_files has one entry, source section present
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_single_touches_file_yields_one_task_file_and_source_section() {
    let (_tmp, conn) = setup_migrated_db();
    let project =
        project_with_files(&[("src/lib.rs", "pub struct MyStruct;\npub fn my_fn() {}\n")]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task("TEST-SLOT-SINGLE-001");
    insert_task_with_files(&conn, "TEST-SLOT-SINGLE-001", &["src/lib.rs"]);

    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert_eq!(
        bundle.task_files.len(),
        1,
        "task_files must have exactly one entry; got {:?}",
        bundle.task_files
    );
    assert_eq!(bundle.task_files[0], "src/lib.rs");
    assert!(
        bundle.prompt.contains("## Current Source Context"),
        "prompt must contain the source-context section when a real file is touched; \
         got:\n{}",
        &bundle.prompt[..bundle.prompt.len().min(600)]
    );
}

// ---------------------------------------------------------------------------
// AC 1c: multiple touchesFiles → task_files has all entries
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_multiple_touches_files_yields_all_task_files() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[
        ("src/alpha.rs", "pub fn alpha() {}\n"),
        ("src/beta.rs", "pub fn beta() {}\n"),
        ("tests/gamma.rs", "fn test_gamma() {}\n"),
    ]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task("TEST-SLOT-MULTI-001");
    insert_task_with_files(
        &conn,
        "TEST-SLOT-MULTI-001",
        &["src/alpha.rs", "src/beta.rs", "tests/gamma.rs"],
    );

    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert_eq!(
        bundle.task_files.len(),
        3,
        "task_files must contain all three file paths; got {:?}",
        bundle.task_files
    );
    for expected in &["src/alpha.rs", "src/beta.rs", "tests/gamma.rs"] {
        assert!(
            bundle.task_files.iter().any(|f| f == expected),
            "task_files must contain '{expected}'; got {:?}",
            bundle.task_files
        );
    }
}

// ---------------------------------------------------------------------------
// AC 2: no matching learnings → shown_learning_ids empty, header absent
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_no_learnings_yields_empty_shown_ids_and_no_learnings_header() {
    let (_tmp, conn) = setup_migrated_db();
    // Fresh DB — no learnings inserted.
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task("TEST-SLOT-NOLEARN-001");
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        bundle.shown_learning_ids.is_empty(),
        "shown_learning_ids must be empty when no learnings match; got {:?}",
        bundle.shown_learning_ids
    );
    assert!(
        !bundle.prompt.contains("## Relevant Learnings"),
        "prompt must NOT contain the '## Relevant Learnings' header when no learnings match; \
         got {} bytes prompt",
        bundle.prompt.len()
    );
}

// ---------------------------------------------------------------------------
// AC 3: many large learnings → learnings block trimmed to LEARNINGS_BUDGET
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_many_large_learnings_shown_ids_non_empty_content_trimmed() {
    let (_tmp, conn) = setup_migrated_db();

    // Insert two learnings with large content so the combined block exceeds
    // LEARNINGS_BUDGET (4 000 bytes). Each content alone is ~6 000 bytes.
    let id1 = insert_matching_learning(
        &conn,
        "Large learning alpha for budget trim test",
        &"a".repeat(6_000),
    );
    let id2 = insert_matching_learning(
        &conn,
        "Large learning beta for budget trim test",
        &"b".repeat(6_000),
    );

    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task("TEST-SLOT-TRIM-001");
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    // At least one learning must have been recalled and shown.
    assert!(
        !bundle.shown_learning_ids.is_empty(),
        "shown_learning_ids must be non-empty when matching learnings exist, even after trimming"
    );
    for id in &bundle.shown_learning_ids {
        assert!(
            *id == id1 || *id == id2,
            "shown_learning_ids contains unexpected id {id}; expected one of [{id1}, {id2}]"
        );
    }

    // The prompt must contain the learnings header.
    assert!(
        bundle.prompt.contains("## Relevant Learnings"),
        "prompt must contain the '## Relevant Learnings' header even when trimming occurs"
    );

    // The combined raw learning content is ~12 000 bytes; LEARNINGS_BUDGET is
    // 4 000 bytes. The learnings block in the prompt must be substantially
    // shorter than the raw input, which we verify by asserting neither full
    // content blob is present verbatim.
    assert!(
        !bundle.prompt.contains(&"a".repeat(6_000)),
        "full 6 000-byte 'a' content must NOT appear verbatim — learnings block is trimmed"
    );
    assert!(
        !bundle.prompt.contains(&"b".repeat(6_000)),
        "full 6 000-byte 'b' content must NOT appear verbatim — learnings block is trimmed"
    );
}

// ---------------------------------------------------------------------------
// AC 4: "dropped_sections" — sections absent when their preconditions fail
//
// SlotPromptBundle has no `dropped_sections` field; we verify the equivalent
// behavior by asserting that sections are absent from the prompt when their
// preconditions are not met (i.e., the builder silently drops them).
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_source_section_absent_when_no_task_files_and_present_when_files_exist() {
    let (tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[("src/canary.rs", "pub fn canary() {}\n")]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    // Without task_files rows — source section must be dropped.
    let task_no_files = sample_task("TEST-SLOT-DROP-001");
    let bundle_no_files = build_prompt(
        &conn,
        &task_no_files,
        &make_params(project.path().to_path_buf(), base_prompt.clone()),
    );
    assert!(
        !bundle_no_files.prompt.contains("## Current Source Context"),
        "source-context section must be absent (dropped) when task has no task_files"
    );

    // With task_files rows — source section must appear.
    let task_with_files = sample_task("TEST-SLOT-DROP-002");
    insert_task_with_files(&conn, "TEST-SLOT-DROP-002", &["src/canary.rs"]);
    let bundle_with_files = build_prompt(
        &conn,
        &task_with_files,
        &make_params(project.path().to_path_buf(), base_prompt.clone()),
    );
    assert!(
        bundle_with_files
            .prompt
            .contains("## Current Source Context"),
        "source-context section must be present when task_files contains a real file"
    );

    // Prompt with source section must be strictly larger.
    assert!(
        bundle_with_files.prompt.len() > bundle_no_files.prompt.len(),
        "prompt with source context ({} bytes) must be larger than without ({} bytes)",
        bundle_with_files.prompt.len(),
        bundle_no_files.prompt.len()
    );

    drop(tmp);
}

#[test]
fn build_prompt_learnings_section_absent_when_no_learnings_and_present_when_learnings_exist() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    // Without learnings — section must be dropped.
    let task_no_learn = sample_task("TEST-SLOT-DLEARN-001");
    let bundle_no_learn = build_prompt(
        &conn,
        &task_no_learn,
        &make_params(project.path().to_path_buf(), base_prompt.clone()),
    );
    assert!(
        !bundle_no_learn.prompt.contains("## Relevant Learnings"),
        "learnings section must be absent (dropped) when no learnings match"
    );

    // Insert a learning then rebuild — section must appear.
    insert_matching_learning(
        &conn,
        "A matching learning",
        "Content that matches TEST-SLOT.",
    );
    let task_with_learn = sample_task("TEST-SLOT-DLEARN-001"); // same task type prefix
    let bundle_with_learn = build_prompt(
        &conn,
        &task_with_learn,
        &make_params(project.path().to_path_buf(), base_prompt.clone()),
    );
    assert!(
        bundle_with_learn.prompt.contains("## Relevant Learnings"),
        "learnings section must be present when a matching learning exists"
    );

    // Prompt with learnings must be larger.
    assert!(
        bundle_with_learn.prompt.len() > bundle_no_learn.prompt.len(),
        "prompt with learnings ({} bytes) must be larger than without ({} bytes)",
        bundle_with_learn.prompt.len(),
        bundle_no_learn.prompt.len()
    );
}

// ---------------------------------------------------------------------------
// AC 5: "section_sizes" — each assembled section contributes measurable bytes
//
// SlotPromptBundle has no `section_sizes` field; we verify the equivalent
// by measuring the byte contribution of known sections directly on the prompt.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_assembled_sections_have_non_zero_byte_sizes() {
    let (_tmp, conn) = setup_migrated_db();

    // Insert a learning so the learnings section is non-empty.
    insert_matching_learning(
        &conn,
        "Section-size test learning",
        "Each section must contribute a measurable number of bytes to the prompt.",
    );

    let project = project_with_files(&[("src/measured.rs", "pub fn measured() {}\n")]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(
        &base_prompt,
        "## Base Instructions\n\nFollow best practices.\n",
    )
    .unwrap();

    let task = sample_task("TEST-SLOT-SIZE-001");
    insert_task_with_files(&conn, "TEST-SLOT-SIZE-001", &["src/measured.rs"]);

    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    // Verify each known section header is present and contributes bytes.
    let sections = [
        ("## Current Task", "task JSON section"),
        ("## Current Source Context", "source-context section"),
        ("## Relevant Learnings", "learnings section"),
        ("## Completing This Task", "completion-instruction section"),
    ];
    for (header, label) in &sections {
        let start = bundle
            .prompt
            .find(header)
            .unwrap_or_else(|| panic!("{label} header '{header}' not found in prompt"));
        // Ensure there's at least one non-whitespace byte following the header.
        let tail = bundle.prompt[start + header.len()..].trim_start_matches('\n');
        assert!(
            !tail.is_empty(),
            "{label} (header '{header}') must have non-zero content following the header"
        );
    }

    // The total prompt size must be substantial (all sections combined).
    assert!(
        bundle.prompt.len() > 500,
        "assembled prompt must exceed 500 bytes when all sections are present; got {}",
        bundle.prompt.len()
    );
}

// ---------------------------------------------------------------------------
// AC 6: Send regression guard (compile-time; duplicate for this test file)
// ---------------------------------------------------------------------------
// The `assert_impl_all!(SlotPromptBundle: Send)` at the top of this file is
// the canonical guard. No additional runtime test is needed — the compile
// error IS the test failure.

// ---------------------------------------------------------------------------
// Additional coverage: resolved_model and difficulty propagation
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_propagates_model_and_difficulty_from_task() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let mut task = sample_task("TEST-SLOT-MODEL-001");
    task.model = Some(SONNET_MODEL.into());
    task.difficulty = Some("high".into());

    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert_eq!(
        bundle.resolved_model.as_deref(),
        Some(SONNET_MODEL),
        "resolved_model must mirror the task's model field"
    );
    assert_eq!(
        bundle.difficulty.as_deref(),
        Some("high"),
        "difficulty must mirror the task's difficulty field"
    );
}

#[test]
fn build_prompt_empty_model_string_normalised_to_none() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let mut task = sample_task("TEST-SLOT-MODEL-002");
    task.model = Some(String::new()); // empty string must be treated as None

    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        bundle.resolved_model.is_none(),
        "resolved_model must be None when the task model field is an empty string; \
         got {:?}",
        bundle.resolved_model
    );
}

#[test]
fn build_prompt_no_model_set_yields_none_resolved_model() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task("TEST-SLOT-MODEL-003"); // no model set
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        bundle.resolved_model.is_none(),
        "resolved_model must be None when the task has no model; got {:?}",
        bundle.resolved_model
    );
}

// ---------------------------------------------------------------------------
// Additional coverage: prompt always contains mandatory fixed sections
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_always_contains_task_section_and_completion_instruction() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    // Minimal task with no optional fields set.
    let task = Task::new("TEST-SLOT-MANDATORY-001", "Minimal slot task");
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        bundle.prompt.contains("## Current Task"),
        "prompt must always contain '## Current Task' section"
    );
    assert!(
        bundle.prompt.contains("## Completing This Task"),
        "prompt must always contain '## Completing This Task' section"
    );
    assert!(
        bundle.prompt.contains("TEST-SLOT-MANDATORY-001"),
        "task id must appear in the assembled prompt"
    );
    assert_eq!(
        bundle.task_id, "TEST-SLOT-MANDATORY-001",
        "bundle.task_id must match the task id"
    );
}
