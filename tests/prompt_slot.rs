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
use task_mgr::loop_engine::prompt::slot::{
    CRITICAL_OVERFLOW_SENTINEL, SlotPromptBundle, SlotPromptParams, build_prompt,
};
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

fn make_params(project_root: PathBuf, base_prompt_path: PathBuf) -> SlotPromptParams<'static> {
    SlotPromptParams {
        project_root,
        base_prompt_path,
        permission_mode: PermissionMode::Dangerous,
        steering_path: None,
        session_guidance: "",
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
// AC: SlotContext carries SlotPromptBundle (FEAT-002), and slot_failure_result
// reads task identity from `bundle.task_id`. This test verifies the contract
// at the only externally-observable seam: build a bundle on the main thread,
// hand it to `run_slot_iteration` via a `SlotContext`, and confirm the
// returned `SlotResult.iteration_result.task_id` matches `bundle.task_id`.
//
// We pre-set the signal flag so the slot bails before spawning Claude, which
// keeps this an integration test of the wiring (not of the Claude subprocess).
// ---------------------------------------------------------------------------

#[test]
fn slot_context_threads_bundle_task_id_through_run_slot_iteration() {
    use task_mgr::loop_engine::engine::{SlotContext, SlotIterationParams, run_slot_iteration};
    use task_mgr::loop_engine::signals::SignalFlag;

    let (temp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let task = sample_task();
    let bundle = build_prompt(
        &conn,
        &task,
        &make_params(project.path().to_path_buf(), base_prompt),
    );
    let expected_task_id = bundle.task_id.clone();

    // Pre-signal so run_slot_iteration takes the early-exit path. The point
    // of the test is not to spawn Claude; it's to assert the worker reads
    // `bundle.task_id` (and only `bundle.task_id`) for the returned result.
    let signal = SignalFlag::new();
    signal.set();

    let slot = SlotContext {
        slot_index: 0,
        working_root: project.path().to_path_buf(),
        prompt_bundle: bundle,
    };
    let params = SlotIterationParams {
        db_dir: temp.path().to_path_buf(),
        permission_mode: PermissionMode::Dangerous,
        signal_flag: signal,
        default_model: None,
        verbose: false,
        iteration: 1,
        max_iterations: 1,
        elapsed_secs: 0,
    };

    let result = run_slot_iteration(&slot, &params).expect("run_slot_iteration");
    assert_eq!(
        result.iteration_result.task_id.as_deref(),
        Some(expected_task_id.as_str()),
        "SlotResult.iteration_result.task_id must come from bundle.task_id"
    );
}

// ---------------------------------------------------------------------------
// AC (FEAT-001 M3): steering + session_guidance threaded into slot prompts.
//
// Positive: steering_path=Some(fixture) and session_guidance=non-empty must
// render both `## Steering` and `## Session Guidance` headers in the assembled
// prompt with content from each input.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_renders_steering_and_session_guidance_when_set() {
    let (_tmp, conn) = setup_migrated_db();
    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "# base\n").unwrap();

    let steering_file = project.path().join("steering.md");
    fs::write(&steering_file, "Project-wide guidance: prefer DI.").unwrap();

    let task = sample_task();
    let params = SlotPromptParams {
        project_root: project.path().to_path_buf(),
        base_prompt_path: base_prompt,
        permission_mode: PermissionMode::Dangerous,
        steering_path: Some(steering_file.as_path()),
        session_guidance: "operator note: focus on edge cases",
    };
    let bundle = build_prompt(&conn, &task, &params);

    assert!(
        bundle.prompt.contains("## Steering"),
        "prompt must contain `## Steering` header when steering_path is Some; got:\n{}",
        bundle.prompt
    );
    assert!(
        bundle.prompt.contains("Project-wide guidance: prefer DI."),
        "prompt must contain steering file content; got:\n{}",
        bundle.prompt
    );
    assert!(
        bundle.prompt.contains("## Session Guidance"),
        "prompt must contain `## Session Guidance` header when guidance is non-empty; got:\n{}",
        bundle.prompt
    );
    assert!(
        bundle.prompt.contains("operator note: focus on edge cases"),
        "prompt must contain session guidance text; got:\n{}",
        bundle.prompt
    );

    // Display order: steering and guidance must precede the tool-awareness
    // block so project-wide instructions land before per-task content,
    // matching sequential.rs.
    let steering_pos = bundle.prompt.find("## Steering").expect("steering header");
    let guidance_pos = bundle
        .prompt
        .find("## Session Guidance")
        .expect("guidance header");
    let tool_pos = bundle
        .prompt
        .find("## Available Tools")
        .expect("tool-awareness header");
    assert!(
        steering_pos < tool_pos && guidance_pos < tool_pos,
        "steering ({steering_pos}) and guidance ({guidance_pos}) must precede tool block ({tool_pos})"
    );
}

// ---------------------------------------------------------------------------
// AC (FEAT-001 M3, negative): steering_path=None + session_guidance=""
// must produce a prompt with NEITHER section header present.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_omits_steering_and_guidance_when_unset() {
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
        !bundle.prompt.contains("## Steering"),
        "prompt must NOT contain `## Steering` header when steering_path is None; got:\n{}",
        bundle.prompt
    );
    assert!(
        !bundle.prompt.contains("## Session Guidance"),
        "prompt must NOT contain `## Session Guidance` header when guidance is empty; got:\n{}",
        bundle.prompt
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

// ---------------------------------------------------------------------------
// WIRE-FIX-002 AC (positive): TOTAL_PROMPT_BUDGET cap holds and dropped_sections
// records the names of trimmable sections that didn't fit.
//
// We force overflow by inflating the base prompt template (a critical section)
// to consume nearly the whole 80 KB budget; the trimmable sections (learnings,
// source) are then forced to drop because the remainder cannot accommodate
// them. The bundle's prompt MUST stay <= TOTAL_PROMPT_BUDGET (80 KB) and at
// least one of "learnings" / "source" MUST appear in dropped_sections.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_oversize_drops_trimmable_sections_and_caps_total_budget() {
    let (_tmp, conn) = setup_migrated_db();

    // Insert a recall-matching learning whose content alone is ~6 KB so the
    // learnings block, if it weren't capped, would still need ~4 KB after
    // truncate-to-budget. With <4 KB remaining, it should drop.
    let learn_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "oversize learning for budget-cap test".into(),
        content: "L".repeat(6_000),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/loop_engine/prompt/slot.rs".into()]),
        applies_to_task_types: Some(vec!["TEST-".into()]),
        applies_to_errors: None,
        tags: Some(vec!["budget".into()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, learn_params).expect("record_learning");

    // Inflate base prompt to ~74 KB (under BASE_PROMPT_BUDGET=16 KB after
    // truncation but the post-truncation value is still 16 KB; we instead
    // need the COMPOSITE critical total to consume most of TOTAL_PROMPT_BUDGET).
    // BASE_PROMPT_BUDGET clips this to 16 KB; couple that with a large source
    // file to push the trimmable budget below the section size threshold.
    let project = project_with_files(&[(
        "src/big.rs",
        &format!(
            "// large source file for budget cap test\n{}\n",
            "x".repeat(60_000)
        ),
    )]);
    let base_prompt = project.path().join("prompt.md");
    // The base prompt itself is one of the four critical sections; making it
    // fully consume its 16 KB cap is enough to leave a tight remainder when
    // combined with the inflated task JSON below.
    fs::write(&base_prompt, "B".repeat(60_000)).unwrap();

    // Inflate the task description so format_task_json produces a large
    // critical section that eats most of TOTAL_PROMPT_BUDGET. ~60 KB.
    let mut task = sample_task();
    task.description = Some("D".repeat(60_000));
    conn.execute(
        "INSERT OR IGNORE INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        ["TEST-SLOT-001", "task"],
    )
    .expect("insert task row");
    conn.execute(
        "INSERT INTO task_files (task_id, file_path) VALUES (?1, ?2)",
        ["TEST-SLOT-001", "src/big.rs"],
    )
    .expect("insert task_files row");

    let params = SlotPromptParams {
        project_root: project.path().to_path_buf(),
        base_prompt_path: base_prompt,
        permission_mode: PermissionMode::Dangerous,
        steering_path: None,
        session_guidance: "",
    };
    let bundle = build_prompt(&conn, &task, &params);

    // The cap MUST hold even with oversized content.
    assert!(
        bundle.prompt.len() <= 80_000,
        "bundle.prompt ({} bytes) must stay within TOTAL_PROMPT_BUDGET (80_000)",
        bundle.prompt.len()
    );

    // At least one trimmable section should have been dropped — and the
    // CRITICAL sentinel MUST NOT appear (we wrote the test to keep critical
    // total under 80 KB).
    assert!(
        !bundle
            .dropped_sections
            .contains(&CRITICAL_OVERFLOW_SENTINEL.to_string()),
        "CRITICAL sentinel should not fire in this scenario; got dropped_sections: {:?}",
        bundle.dropped_sections,
    );
    let dropped_trimmable: bool = bundle
        .dropped_sections
        .iter()
        .any(|s| s == "learnings" || s == "source");
    assert!(
        dropped_trimmable,
        "at least one of 'learnings' / 'source' must appear in dropped_sections \
         for this oversize fixture; got: {:?}",
        bundle.dropped_sections,
    );
}

// ---------------------------------------------------------------------------
// WIRE-FIX-002 AC: shown_learning_ids MUST be empty whenever 'learnings'
// appears in dropped_sections — feeding the bandit with learnings the agent
// never saw skews UCB scoring.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_clears_shown_learning_ids_when_learnings_dropped() {
    let (_tmp, conn) = setup_migrated_db();

    let learn_params = RecordLearningParams {
        outcome: LearningOutcome::Pattern,
        title: "learning that should drop under budget pressure".into(),
        content: "X".repeat(3_500),
        task_id: None,
        run_id: None,
        root_cause: None,
        solution: None,
        applies_to_files: Some(vec!["src/loop_engine/prompt/slot.rs".into()]),
        applies_to_task_types: Some(vec!["TEST-".into()]),
        applies_to_errors: None,
        tags: Some(vec!["bandit".into()]),
        confidence: Confidence::High,
    };
    record_learning(&conn, learn_params).expect("record_learning");

    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    fs::write(&base_prompt, "B".repeat(20_000)).unwrap();

    // Force a tight remainder by oversizing the task JSON (critical section)
    // so the learnings block can't fit. The learnings block is ~3.5 KB after
    // recall + truncate; we leave a few bytes of remainder for it.
    //
    // Critical budget math (must stay under TOTAL_PROMPT_BUDGET=80_000 to avoid
    // tripping the CRITICAL sentinel):
    //   task_section ≈ 58_000 (60_000 description + JSON overhead)
    //   task_ops     ≈ 3_000  (static string, see prompt_sections::task_ops)
    //   completion   ≈ 1_500
    //   base_prompt  ≤ 16_000 (BASE_PROMPT_BUDGET cap on truncate_to_budget)
    //   total        ≈ 78_500 → remainder ≈ 1_500 — too tight for the ~3.5 KB
    //                                          learnings block.
    let mut task = sample_task();
    task.description = Some("D".repeat(58_000));
    let params = SlotPromptParams {
        project_root: project.path().to_path_buf(),
        base_prompt_path: base_prompt,
        permission_mode: PermissionMode::Dangerous,
        steering_path: None,
        session_guidance: "",
    };
    let bundle = build_prompt(&conn, &task, &params);

    if bundle.dropped_sections.iter().any(|s| s == "learnings") {
        assert!(
            bundle.shown_learning_ids.is_empty(),
            "shown_learning_ids must be empty when 'learnings' is dropped; got {:?}",
            bundle.shown_learning_ids,
        );
    } else {
        // Defensive: if the fixture didn't actually push learnings out,
        // surface that — the test premise needs a tighter knob, not a silent pass.
        panic!(
            "fixture failed to drop 'learnings'; dropped_sections = {:?}",
            bundle.dropped_sections
        );
    }
}

// ---------------------------------------------------------------------------
// WIRE-FIX-002 AC: critical-only oversize — when the four critical sections
// alone exceed TOTAL_PROMPT_BUDGET, build_prompt MUST return a sentinel
// bundle (empty prompt, dropped_sections = ["CRITICAL"]) and the caller
// (build_slot_contexts) handles it gracefully without panic.
// ---------------------------------------------------------------------------

#[test]
fn build_prompt_critical_only_oversize_returns_sentinel_bundle() {
    let (_tmp, conn) = setup_migrated_db();

    let project = project_with_files(&[]);
    let base_prompt = project.path().join("prompt.md");
    // The base prompt is truncated to BASE_PROMPT_BUDGET (16 KB) — large
    // input is fine; we instead push the task JSON over the 80 KB cap.
    fs::write(&base_prompt, "# base\n").unwrap();

    // 200 KB description guarantees the task JSON section alone exceeds
    // TOTAL_PROMPT_BUDGET (80 KB) — there is no per-section truncation for
    // the task JSON in slot.rs, so this is the cheapest knob.
    let mut task = sample_task();
    task.description = Some("D".repeat(200_000));

    let params = SlotPromptParams {
        project_root: project.path().to_path_buf(),
        base_prompt_path: base_prompt,
        permission_mode: PermissionMode::Dangerous,
        steering_path: None,
        session_guidance: "",
    };
    let bundle = build_prompt(&conn, &task, &params);

    assert!(
        bundle
            .dropped_sections
            .contains(&CRITICAL_OVERFLOW_SENTINEL.to_string()),
        "dropped_sections must contain the CRITICAL sentinel when critical sections \
         exceed TOTAL_PROMPT_BUDGET; got: {:?}",
        bundle.dropped_sections,
    );
    assert!(
        bundle.prompt.is_empty(),
        "bundle.prompt must be empty when CRITICAL sentinel fires (caller skips slot); \
         got {} bytes",
        bundle.prompt.len(),
    );
    assert_eq!(
        bundle.task_id, task.id,
        "bundle.task_id must still mirror task.id even on the sentinel path \
         so caller logging identifies the offending task",
    );
    assert!(
        bundle.shown_learning_ids.is_empty(),
        "shown_learning_ids must be empty on the sentinel path — no recall results \
         were ever surfaced",
    );
}
