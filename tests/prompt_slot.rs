//! Contract tests for `task_mgr::loop_engine::prompt::slot`.
//!
//! TDD scaffolding for FEAT-001 (the slot-mode prompt builder). These tests
//! pin the public surface, the Send guarantee, and the section-content
//! invariants of `SlotPromptBundle` BEFORE the implementation lands. They
//! MUST fail against the current empty-bundle stub and pass once
//! `build_prompt` composes the `prompt::core` helpers.
//!
//! Notes for future maintainers:
//! - This file is an integration test, so it cannot use the `pub(crate)`
//!   `loop_engine::test_utils` helpers (per learning #896). Setup goes
//!   through the public DB API.
//! - Migration-aware DB setup uses `open_connection` + `create_schema` +
//!   `run_migrations` so FTS5 / supersession-aware retrieval is wired up.
//! - The compile-time `assert_impl_all!(SlotPromptBundle: Send)` assertion
//!   is the canonical guard against accidentally adding an `Rc` / `RefCell`
//!   field to the bundle. It runs on every build, no `#[ignore]` permitted.

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use static_assertions::assert_impl_all;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::learnings::crud::{RecordLearningParams, record_learning};
use task_mgr::loop_engine::config::PermissionMode;
use task_mgr::loop_engine::prompt::slot::{SlotPromptBundle, SlotPromptParams, build_prompt};
use task_mgr::models::{Confidence, LearningOutcome, Task};

// Compile-time invariant: SlotPromptBundle must cross thread boundaries.
// Adding any non-Send field (Rc, RefCell, MutexGuard) breaks the build here.
// AC: "Compile-time `static_assertions::assert_impl_all!(SlotPromptBundle: Send)` test exists and compiles".
assert_impl_all!(SlotPromptBundle: Send);

/// Open a DB with full schema + all migrations applied. The TempDir return
/// value must outlive the Connection.
fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

fn sample_task() -> Task {
    let mut task = Task::new("TEST-SLOT-001", "Validate prompt::slot::build_prompt");
    task.description = Some("Ensure SlotPromptBundle composes shared helpers.".into());
    task.acceptance_criteria = vec![
        "Bundle is Send".into(),
        "Bundle prompt contains all four standard sections".into(),
    ];
    task.notes = Some("TDD scaffolding".into());
    task.difficulty = Some("medium".into());
    task
}

/// Build a project-root tempdir that contains the touched files so the
/// source-context section has something to render.
fn project_with_files(touches: &[(&str, &str)]) -> TempDir {
    let temp = TempDir::new().expect("project tempdir");
    for (rel_path, contents) in touches {
        let abs = temp.path().join(rel_path);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("create dir for touched file");
        }
        fs::write(&abs, contents).expect("write touched file");
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

// ---------------------------------------------------------------------------
// AC: Test asserts SlotPromptBundle.task_id matches the input task's id.
//
// This invariant must hold even for the stub — orphan-reset accounting reads
// bundle.task_id, so misalignment is a contract break regardless of whether
// the body has been filled in. No #[ignore].
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_sets_task_id_to_input_task_id() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base prompt template\n").unwrap();

    let task = sample_task();
    let params = make_params(project.path().to_path_buf(), base_prompt);
    let bundle = build_prompt(&conn, &task, &params);

    assert_eq!(
        bundle.task_id, task.id,
        "bundle.task_id must mirror the input Task::id so slot accounting stays correct \
         after the worker has been spawned"
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts SlotPromptBundle.prompt contains the '## Relevant Learnings'
// header when matching learnings exist in the DB.
// AC: Test asserts SlotPromptBundle.shown_learning_ids is non-empty when
// learnings were included.
// AC (discriminator): stub returning empty `prompt` fails this assertion.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_includes_learnings_section_and_ids_when_db_has_matches() {
    let (_tmp, conn) = setup_migrated_db();

    // Record a learning that targets TEST-* and FEAT-* tasks so the recall
    // backend has something to surface for sample_task() (id "TEST-SLOT-001").
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "slot prompts must surface learnings to wave workers".into(),
        content: "build_prompt must compose build_learnings_block so wave-mode \
                  workers see the same recall results as the sequential path."
            .into(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/loop_engine/prompt/slot.rs".into()]),
        applies_to_task_types: Some(vec!["TEST-".into(), "FEAT-".into()]),
        applies_to_errors: None,
        tags: Some(vec!["prompt".into(), "slot".into()]),
        confidence: Confidence::High,
    };
    let inserted = record_learning(&conn, params)
        .expect("record_learning")
        .learning_id;

    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();
    let task = sample_task();
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        !bundle.prompt.is_empty(),
        "bundle.prompt must not be empty when matching learnings exist (stub discriminator)"
    );
    assert!(
        bundle.prompt.contains("## Relevant Learnings"),
        "bundle.prompt must contain the '## Relevant Learnings' header; got:\n{}",
        bundle.prompt
    );
    assert!(
        !bundle.shown_learning_ids.is_empty(),
        "shown_learning_ids must not be empty when the learnings block was rendered"
    );
    assert!(
        bundle.shown_learning_ids.contains(&inserted),
        "shown_learning_ids must include the recalled learning id ({inserted}); got {:?}",
        bundle.shown_learning_ids,
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts SlotPromptBundle.prompt contains the source-context section
// when touchesFiles is non-empty and files exist.
// AC (discriminator): stub returning empty `prompt` fails this assertion.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_includes_source_context_when_touches_files_resolve() {
    let (_tmp, conn) = setup_migrated_db();

    let project = project_with_files(&[(
        "src/foo.rs",
        "// the canary content the source-context block should surface\n\
         pub fn answer() -> u32 { 42 }\n",
    )]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    // Note: build_prompt reads `task_files` from the DB, not from a Vec on
    // the Task struct. The implementation in FEAT-001 will populate this
    // table from the JSON when the task is inserted; here we mirror that by
    // inserting the row directly so the helper has data to scan. The test
    // tolerates either retrieval path — we only assert on the final prompt.
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        ["TEST-SLOT-001", "Validate prompt::slot::build_prompt"],
    )
    .expect("insert task row");
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES (?1, ?2)",
        ["TEST-SLOT-001", "src/foo.rs"],
    )
    .expect("insert task_files row");

    let task = sample_task();
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        !bundle.prompt.is_empty(),
        "bundle.prompt must not be empty when touches_files resolves to real files \
         (stub discriminator)"
    );
    let lower = bundle.prompt.to_ascii_lowercase();
    assert!(
        lower.contains("source") || lower.contains("foo.rs"),
        "bundle.prompt must reference the source-context block (header containing 'source' \
         or the touched file path); got:\n{}",
        bundle.prompt
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts SlotPromptBundle.prompt contains the tool-awareness section.
// AC (discriminator): empty stub fails this assertion.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_includes_tool_awareness_block() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task();
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        !bundle.prompt.is_empty(),
        "bundle.prompt must not be empty (stub discriminator)"
    );
    let lower = bundle.prompt.to_ascii_lowercase();
    assert!(
        lower.contains("tool"),
        "bundle.prompt must contain the tool-awareness block (something mentioning 'tool'); \
         got:\n{}",
        bundle.prompt
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts SlotPromptBundle.prompt contains the key-decisions section.
// AC (discriminator): empty stub fails this assertion.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_includes_key_decisions_block() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task();
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        !bundle.prompt.is_empty(),
        "bundle.prompt must not be empty (stub discriminator)"
    );
    assert!(
        bundle.prompt.contains("key-decision"),
        "bundle.prompt must contain the key-decisions block (mentions <key-decision> tag); \
         got:\n{}",
        bundle.prompt
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts a slot worker panic AFTER bundle construction surfaces as
// a slot failure whose recorded task_id matches bundle.task_id (orphan-reset
// accounting stays correct).
//
// This pins the contract that `slot_failure_result` reads task identity from
// the bundle (post-FEAT-002 wiring), not from a stale `Task` reference. The
// test is gated until FEAT-002 swaps `SlotContext.task` for
// `SlotContext.prompt_bundle`; today's `SlotContext` still holds a `Task`,
// so the assertion can only run once that field is in place.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "FEAT-002 swaps SlotContext.task → SlotContext.prompt_bundle; assertion is exercised \
            via run_parallel_wave's panic path once that wiring lands"]
fn slot_worker_panic_after_bundle_construction_records_bundle_task_id() {
    // The full assertion is integration-tested through run_parallel_wave once
    // SlotContext carries SlotPromptBundle. Encoding the contract here as a
    // standalone test ensures the wiring task can flip #[ignore] off without
    // having to invent a new test scaffold. The shape of the eventual test:
    //
    //   1. Build a SlotPromptBundle for a claimed task on the main thread.
    //   2. Spawn a slot worker that panics AFTER reading the bundle.
    //   3. Assert the resulting SlotResult.iteration_result.task_id ==
    //      bundle.task_id (NOT derived from the now-moved Task reference).
    //
    // We intentionally keep this body empty rather than asserting against
    // the current stub so a regression that wires the panic path to the
    // wrong field doesn't slip past a "no-op" passing test.
    panic!(
        "FEAT-002 must implement: SlotContext.prompt_bundle.task_id is the canonical task id \
         used by slot_failure_result for the panic path. See engine.rs:786-803 + the new \
         SlotContext field per FEAT-002 AC #1."
    );
}

// ---------------------------------------------------------------------------
// AC (discriminator): the empty-prompt stub fails learnings + source +
// tool-awareness assertions. This dedicated test pins the discriminator
// behavior explicitly so a future regression that returns `String::new()`
// for build_prompt is caught by a single, named assertion.
// ---------------------------------------------------------------------------

#[test]
fn all_four_standard_sections_present_in_assembled_prompt() {
    let (_tmp, conn) = setup_migrated_db();

    // Insert a learning so the learnings block is non-empty and the
    // '## Relevant Learnings' header appears in the assembled prompt.
    let params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "all four sections guard test".into(),
        content: "contract: every build_prompt call must include all four standard sections."
            .into(),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/loop_engine/prompt/slot.rs".into()]),
        applies_to_task_types: Some(vec!["TEST-".into(), "FEAT-".into()]),
        applies_to_errors: None,
        tags: Some(vec!["prompt".into(), "slot".into()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, params).expect("record_learning");

    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task();
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );

    assert!(
        !bundle.prompt.is_empty()
            && bundle.prompt.contains("## Relevant Learnings")
            && bundle.prompt.to_ascii_lowercase().contains("tool")
            && bundle.prompt.contains("key-decision"),
        "all four standard sections must appear in bundle.prompt; got:\n{}",
        bundle.prompt
    );
}
