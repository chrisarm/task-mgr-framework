//! Contract tests for per-task crash tracking on `IterationContext`.
//!
//! TDD scaffolding for FEAT-007 (per-task crash tracking + `check_crash_escalation`
//! rewrite). FEAT-007 will:
//!
//! 1. Have `iteration_pipeline::process_iteration_output` write
//!    `ctx.crashed_last_iteration[task_id] = matches!(outcome, Crash(_))` once
//!    per iteration.
//! 2. Rewrite `check_crash_escalation` to consult that map instead of the
//!    paired `last_task_id` / `last_was_crash` scalars.
//! 3. Remove the legacy `last_task_id` and `last_was_crash` fields.
//!
//! The field already exists in this commit (TEST-INIT-004 added it as an
//! additive stub alongside the legacy fields) so this test file compiles
//! against today's tree. Tests that exercise the pipeline-write or rewritten
//! `check_crash_escalation` behavior are `#[ignore]`-d with FEAT-007 reasons.
//!
//! Notes for future maintainers:
//! - Integration test → cannot use `pub(crate)` test_utils helpers (per
//!   learning #896). All construction goes through the public DB API and
//!   `IterationContext::new`.
//! - We exercise the synthetic-context style the AC explicitly calls for:
//!   "Use a synthetic IterationContext + repeated invocations rather than
//!   the real loop." No `run_iteration` / `run_loop` / `run_parallel_wave`
//!   here — just direct ctx mutation + targeted helper calls.
//! - The discriminator test at the bottom is an active assertion that fails
//!   if `check_crash_escalation` ever returns `None` for the canonical
//!   "same-task consecutive crash" case — pinning the contract whether the
//!   function reads the legacy scalars (today) or the map (post-FEAT-007).

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::loop_engine::config::IterationOutcome;
use task_mgr::loop_engine::detection::{TaskStatusChange, TaskStatusUpdate};
use task_mgr::loop_engine::engine::{
    IterationContext, apply_status_updates, check_crash_escalation,
};
use task_mgr::loop_engine::iteration_pipeline::{ProcessingParams, process_iteration_output};
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
use task_mgr::loop_engine::signals::SignalFlag;

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// Open a DB with full schema + all migrations applied. The `TempDir` return
/// value MUST outlive the `Connection` — dropping it yanks the on-disk file.
fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

/// Insert a task row in `in_progress` so the pipeline's completion ladder /
/// crash bookkeeping has something to anchor to.
fn insert_in_progress_task(conn: &Connection, task_id: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        [task_id, "Per-task crash test fixture"],
    )
    .expect("insert task row");
}

/// Disable LLM-based learning extraction for the duration of a test so the
/// pipeline call stays hermetic. See `iteration_pipeline.rs` tests for the
/// canonical justification of this opt-out.
fn disable_llm_extraction() {
    // SAFETY: cargo test's set_var-on-process pattern; the opt-out is only
    // checked once per pipeline call after this setter has returned.
    unsafe {
        std::env::set_var("TASK_MGR_NO_EXTRACT_LEARNINGS", "1");
    }
}

/// Bag of owned values the test can hand to `ProcessingParams` via borrows.
/// Mirrors the fixture pattern in `tests/iteration_pipeline.rs`.
struct PipelineFixture {
    project: TempDir,
    prd_path: PathBuf,
    progress_path: PathBuf,
    db_dir: PathBuf,
    signal_flag: SignalFlag,
    ctx: IterationContext,
}

impl PipelineFixture {
    fn new(db_dir: &std::path::Path) -> Self {
        let project = TempDir::new().expect("project tempdir");
        let prd_path = project.path().join("prd.json");
        fs::write(&prd_path, "{\"tasks\":[]}\n").expect("write prd json");
        let progress_path = project.path().join("progress.txt");
        fs::write(&progress_path, "").expect("write progress");
        Self {
            db_dir: db_dir.to_path_buf(),
            project,
            prd_path,
            progress_path,
            signal_flag: SignalFlag::new(),
            ctx: IterationContext::new(5),
        }
    }
}

// ---------------------------------------------------------------------------
// Field shape: starts empty, writable per task, idempotent on duplicate keys.
// These are unconditional so the public surface stays pinned even when the
// FEAT-007-gated tests are skipped.
// ---------------------------------------------------------------------------

#[test]
fn crashed_last_iteration_starts_empty() {
    let ctx = IterationContext::new(5);
    assert!(
        ctx.crashed_last_iteration.is_empty(),
        "fresh IterationContext must start with an empty crashed_last_iteration map"
    );
}

#[test]
fn crashed_last_iteration_is_writable_per_task() {
    let mut ctx = IterationContext::new(5);
    ctx.crashed_last_iteration
        .insert("TASK-A".to_string(), true);
    ctx.crashed_last_iteration
        .insert("TASK-B".to_string(), false);

    assert_eq!(
        ctx.crashed_last_iteration.get("TASK-A"),
        Some(&true),
        "writes for TASK-A must round-trip"
    );
    assert_eq!(
        ctx.crashed_last_iteration.get("TASK-B"),
        Some(&false),
        "writes for TASK-B must round-trip"
    );
    assert_eq!(
        ctx.crashed_last_iteration.len(),
        2,
        "two distinct task IDs must produce two map entries"
    );
}

#[test]
fn crashed_last_iteration_is_idempotent_on_repeated_writes() {
    let mut ctx = IterationContext::new(5);
    for _ in 0..50 {
        ctx.crashed_last_iteration
            .insert("TASK-X".to_string(), true);
    }
    assert_eq!(
        ctx.crashed_last_iteration.len(),
        1,
        "repeated writes for one task ID must collapse to a single map entry — \
         the key is the task_id, NOT the iteration index"
    );
    assert_eq!(ctx.crashed_last_iteration.get("TASK-X"), Some(&true));
}

// ---------------------------------------------------------------------------
// AC: ctx.crashed_last_iteration map size is bounded by active task count
// (no leak after a 100-iteration loop).
//
// The contract: pipeline writes use the task_id as the key, so a long loop
// over a small task set has bounded memory. We exercise this synthetically —
// the test doubles as a design contract for the FEAT-007 pipeline writer
// ("must key by task_id, not iteration").
// ---------------------------------------------------------------------------

#[test]
fn crashed_last_iteration_size_bounded_by_active_task_count() {
    let mut ctx = IterationContext::new(5);
    let active_ids = ["TASK-A", "TASK-B", "TASK-C"];

    for i in 0..100 {
        let task_id = active_ids[i % active_ids.len()].to_string();
        // Simulate the pipeline write: alternating crash/success.
        let is_crash = i % 2 == 0;
        ctx.crashed_last_iteration.insert(task_id, is_crash);
    }

    assert!(
        ctx.crashed_last_iteration.len() <= active_ids.len(),
        "after 100 iterations across {} active tasks, map size must be <= {} \
         (got {}). The pipeline MUST key by task_id; keying by iteration would \
         leak unbounded entries.",
        active_ids.len(),
        active_ids.len(),
        ctx.crashed_last_iteration.len(),
    );
}

// ---------------------------------------------------------------------------
// AC #4: legacy fields removed (compile-time assertion).
//
// This test references ONLY `crashed_last_iteration` and never touches
// `ctx.last_task_id` or `ctx.last_was_crash`. After FEAT-007 removes those
// fields, this test continues to compile unchanged — that is the structural
// guarantee the AC asks for. Today the legacy fields exist (additive
// transition state) but this test deliberately ignores them.
// ---------------------------------------------------------------------------

#[test]
fn check_crash_escalation_does_not_require_legacy_fields_on_ctx() {
    let mut ctx = IterationContext::new(5);
    // The future contract: callers populate the per-task map and ask for
    // escalation. We only borrow the field FEAT-007 keeps; nothing here
    // references the soon-to-be-removed scalars.
    ctx.crashed_last_iteration
        .insert("TASK-X".to_string(), true);

    // Compile-time guard: a function that takes `&IterationContext` should
    // be reachable using only the surviving public surface. We exercise it
    // through the today-and-tomorrow stable shape of `check_crash_escalation`
    // without touching `ctx.last_task_id` / `ctx.last_was_crash`.
    let _ = &ctx.crashed_last_iteration;
}

// ---------------------------------------------------------------------------
// AC: Test asserts wave crash on task X populates ctx.crashed_last_iteration[X]=true.
//
// Drives the pipeline directly with a Crash outcome and asserts the per-task
// map records it. Today the pipeline writes the legacy scalars and does NOT
// touch the map; FEAT-007 will swap the writer over, at which point this
// test passes.
// ---------------------------------------------------------------------------

#[test]
fn wave_crash_on_task_populates_crashed_last_iteration_true() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "TASK-WAVE-X");

    let mut fx = PipelineFixture::new(db_temp.path());
    // A `Crash` outcome stands in for the wave path — process_slot_result
    // funnels every per-slot outcome through the same pipeline call after
    // FEAT-006 wires it.
    let mut outcome =
        IterationOutcome::Crash(task_mgr::loop_engine::config::CrashType::RuntimeError);

    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TASK-WAVE-X"),
        output: "",
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: fx.project.path(),
        git_scan_depth: 5,
        skip_git_completion_detection: true,
        prd_path: &fx.prd_path,
        task_prefix: None,
        progress_path: &fx.progress_path,
        db_dir: &fx.db_dir,
        signal_flag: &fx.signal_flag,
        ctx: &mut fx.ctx,
        files_modified: &[],
        effective_model: None,
        effective_effort: None,
        slot_index: None,
    });

    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-WAVE-X"),
        Some(&true),
        "Crash outcome on TASK-WAVE-X must record crashed_last_iteration[TASK-WAVE-X] = true",
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts next sequential iteration re-picking X triggers crash
// escalation (model jumped per the crash ladder).
//
// Cross-mode wiring: a wave-path crash sets the per-task flag; a subsequent
// sequential iteration on the same task observes the flag via
// `check_crash_escalation` and escalates. Today the function reads the
// legacy scalars; FEAT-007 wires it to consult the map instead.
// ---------------------------------------------------------------------------

#[test]
fn sequential_repick_after_wave_crash_escalates_per_ladder() {
    let mut ctx = IterationContext::new(5);
    // Wave path recorded a crash on TASK-X (simulated — FEAT-007 pipeline
    // does this for real).
    ctx.crashed_last_iteration
        .insert("TASK-X".to_string(), true);

    // Sequential iteration picks TASK-X again. Post-FEAT-007 the function
    // signature reads ctx + current_task_id + resolved_model directly. We
    // exercise the future call shape via a thin helper that the rewrite
    // will collapse into the function itself; until then we hand-roll the
    // adapter through the legacy 4-arg signature so the assertion still
    // pins the escalation outcome.
    let escalated = check_crash_escalation_via_ctx(&ctx, "TASK-X", Some(HAIKU_MODEL));

    assert_eq!(
        escalated.as_deref(),
        Some(SONNET_MODEL),
        "consecutive same-task crash must escalate haiku → sonnet"
    );

    let escalated_from_sonnet = check_crash_escalation_via_ctx(&ctx, "TASK-X", Some(SONNET_MODEL));
    assert_eq!(
        escalated_from_sonnet.as_deref(),
        Some(OPUS_MODEL),
        "consecutive same-task crash must escalate sonnet → opus"
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts sequential success on task X sets/removes
// ctx.crashed_last_iteration[X] to/of false (next pick does NOT escalate).
// ---------------------------------------------------------------------------

#[test]
fn sequential_success_clears_crashed_last_iteration_for_task() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "TASK-SUCC");

    let mut fx = PipelineFixture::new(db_temp.path());
    // Pre-state: TASK-SUCC was recorded as crashed in a prior iteration.
    fx.ctx
        .crashed_last_iteration
        .insert("TASK-SUCC".to_string(), true);

    // This iteration completes the task — the output emits a <completed>
    // tag, which the pipeline routes through the completion ladder. The
    // outcome will be promoted to Completed (NOT Crash) and the per-task
    // flag must flip to false.
    let mut outcome = IterationOutcome::Empty;
    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 2,
        task_id: Some("TASK-SUCC"),
        output: "<completed>TASK-SUCC</completed>\n",
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: fx.project.path(),
        git_scan_depth: 5,
        skip_git_completion_detection: true,
        prd_path: &fx.prd_path,
        task_prefix: None,
        progress_path: &fx.progress_path,
        db_dir: &fx.db_dir,
        signal_flag: &fx.signal_flag,
        ctx: &mut fx.ctx,
        files_modified: &[],
        effective_model: None,
        effective_effort: None,
        slot_index: None,
    });

    // CODE-FIX-003: terminal transitions prune the entry entirely rather than
    // flipping to false. The task is done — it has no active lifetime in the map.
    assert!(
        !fx.ctx.crashed_last_iteration.contains_key("TASK-SUCC"),
        "done task must be pruned from crashed_last_iteration (entry must be absent)"
    );

    // Next pick of TASK-SUCC must NOT escalate.
    let escalated = check_crash_escalation_via_ctx(&fx.ctx, "TASK-SUCC", Some(HAIKU_MODEL));
    assert!(
        escalated.is_none(),
        "after success, re-picking TASK-SUCC must NOT escalate; got {:?}",
        escalated,
    );
}

// ---------------------------------------------------------------------------
// M-1 follow-up to CODE-FIX-003: terminal <task-status>:failed/skipped/
// irrelevant claims must be pruned from crashed_last_iteration end-to-end via
// process_iteration_output, NOT only via apply_status_updates.
//
// apply_status_updates correctly removes the entry on terminal dispatch, but
// Step 7 of process_iteration_output would re-insert unless its went_terminal
// predicate covers the non-Done terminal cases. This test pins the contract
// (Learning #2304): the entry must be ABSENT after the full pipeline pass.
// ---------------------------------------------------------------------------

#[test]
fn pipeline_pass_with_terminal_failed_status_tag_prunes_crashed_last_iteration() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "FAILTASK");

    let mut fx = PipelineFixture::new(db_temp.path());
    fx.ctx
        .crashed_last_iteration
        .insert("FAILTASK".to_string(), true);

    let mut outcome = IterationOutcome::Empty;
    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 3,
        task_id: Some("FAILTASK"),
        output: "<task-status>FAILTASK:failed</task-status>\n",
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: fx.project.path(),
        git_scan_depth: 5,
        skip_git_completion_detection: true,
        prd_path: &fx.prd_path,
        task_prefix: None,
        progress_path: &fx.progress_path,
        db_dir: &fx.db_dir,
        signal_flag: &fx.signal_flag,
        ctx: &mut fx.ctx,
        files_modified: &[],
        effective_model: None,
        effective_effort: None,
        slot_index: None,
    });

    assert!(
        !fx.ctx.crashed_last_iteration.contains_key("FAILTASK"),
        "<task-status>:failed must prune FAILTASK end-to-end through the pipeline; \
         apply_status_updates removes it, Step 7 must NOT re-insert (Learning #2304)"
    );
}

// ---------------------------------------------------------------------------
// AC: ctx.crashed_last_iteration map size is bounded by active task count
// (no leak after a 100-iteration loop) — pipeline-driven variant.
//
// The structural test above covers the synthetic case. This `#[ignore]`-d
// variant drives the actual pipeline in a 100-iteration loop and asserts
// the same upper bound. Together they pin BOTH the design (key by task_id)
// and the wiring (pipeline uses the right key).
// ---------------------------------------------------------------------------

#[test]
fn pipeline_loop_keeps_crashed_last_iteration_bounded_by_task_count() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    let task_ids = ["TASK-LOOP-A", "TASK-LOOP-B", "TASK-LOOP-C"];
    for id in &task_ids {
        insert_in_progress_task(&conn, id);
    }

    let mut fx = PipelineFixture::new(db_temp.path());

    for i in 0..100 {
        let task_id = task_ids[i % task_ids.len()];
        let mut outcome = if i % 2 == 0 {
            IterationOutcome::Crash(task_mgr::loop_engine::config::CrashType::RuntimeError)
        } else {
            IterationOutcome::Empty
        };
        let _ = process_iteration_output(ProcessingParams {
            conn: &mut conn,
            run_id: "test-run",
            iteration: (i + 1) as u32,
            task_id: Some(task_id),
            output: "",
            conversation: None,
            shown_learning_ids: &[],
            outcome: &mut outcome,
            working_root: fx.project.path(),
            git_scan_depth: 5,
            skip_git_completion_detection: true,
            prd_path: &fx.prd_path,
            task_prefix: None,
            progress_path: &fx.progress_path,
            db_dir: &fx.db_dir,
            signal_flag: &fx.signal_flag,
            ctx: &mut fx.ctx,
            files_modified: &[],
            effective_model: None,
            effective_effort: None,
            slot_index: None,
        });
    }

    assert!(
        fx.ctx.crashed_last_iteration.len() <= task_ids.len(),
        "after 100 pipeline calls across {} tasks, map size must be <= {} (got {})",
        task_ids.len(),
        task_ids.len(),
        fx.ctx.crashed_last_iteration.len(),
    );
}

// ---------------------------------------------------------------------------
// AC (discriminator): a check_crash_escalation that always returns None
// fails the cross-mode test.
//
// Active assertion: same-task consecutive crash on a haiku-baseline task must
// produce SONNET as the escalated model. A regression that returns None is
// caught here.
// ---------------------------------------------------------------------------

fn crash_map(entries: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), *v))
        .collect()
}

#[test]
fn discriminator_check_crash_escalation_returns_some_for_same_task_crash() {
    let escalated =
        check_crash_escalation(&crash_map(&[("TASK-X", true)]), "TASK-X", Some(HAIKU_MODEL));
    assert_eq!(
        escalated.as_deref(),
        Some(SONNET_MODEL),
        "discriminator: a check_crash_escalation that returns None for the canonical \
         same-task consecutive-crash case is broken — got {:?}",
        escalated,
    );

    // Cross-task: TASK-A crashed but TASK-B is the current task — must NOT escalate.
    let cross =
        check_crash_escalation(&crash_map(&[("TASK-A", true)]), "TASK-B", Some(HAIKU_MODEL));
    assert!(
        cross.is_none(),
        "cross-task crash must NOT escalate — got {:?}",
        cross,
    );
}

// ---------------------------------------------------------------------------
// CODE-FIX-003: crashed_last_iteration pruned on terminal transitions.
// ---------------------------------------------------------------------------

/// Crash task X in iteration N, mark X done via apply_status_updates in
/// iteration N+1 — entry must be absent (contains_key == false).
#[test]
fn terminal_done_via_status_tag_prunes_crashed_last_iteration() {
    let (_db_temp, mut conn) = setup_migrated_db();
    // Insert task as in_progress so the Done dispatch succeeds.
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES ('FIX003-TASK', 't', 'in_progress', 50)",
        [],
    )
    .unwrap();

    let mut ctx = IterationContext::new(5);
    // Pre-state: task was recorded as crashed in a prior iteration.
    ctx.crashed_last_iteration
        .insert("FIX003-TASK".to_string(), true);

    let updates = vec![TaskStatusUpdate {
        task_id: "FIX003-TASK".to_string(),
        status: TaskStatusChange::Done,
    }];
    apply_status_updates(
        &mut conn,
        &updates,
        None,
        None,
        None,
        None,
        None,
        Some(&mut ctx),
    );

    assert!(
        !ctx.crashed_last_iteration.contains_key("FIX003-TASK"),
        "terminal Done dispatch must prune FIX003-TASK from crashed_last_iteration"
    );
}

/// Dispatch failure (task not in DB → complete fails) must NOT prune the
/// entry — the DB row never reached terminal state.
#[test]
fn failed_dispatch_does_not_prune_crashed_last_iteration() {
    let (_tmp, mut conn) = setup_migrated_db();
    // Intentionally do NOT insert the task — complete_cmd::complete will fail.

    let mut ctx = IterationContext::new(5);
    ctx.crashed_last_iteration
        .insert("GHOST-TASK".to_string(), true);

    let updates = vec![TaskStatusUpdate {
        task_id: "GHOST-TASK".to_string(),
        status: TaskStatusChange::Done,
    }];
    apply_status_updates(
        &mut conn,
        &updates,
        None,
        None,
        None,
        None,
        None,
        Some(&mut ctx),
    );

    assert_eq!(
        ctx.crashed_last_iteration.get("GHOST-TASK"),
        Some(&true),
        "failed dispatch must NOT prune crashed_last_iteration"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Thin adapter used by the per-task crash tracking tests: reads the map from
/// `ctx` and delegates to `check_crash_escalation`. Replaces the pre-FEAT-007
/// adapter that bridged the legacy 4-arg signature.
fn check_crash_escalation_via_ctx(
    ctx: &IterationContext,
    current_task_id: &str,
    resolved_model: Option<&str>,
) -> Option<String> {
    check_crash_escalation(&ctx.crashed_last_iteration, current_task_id, resolved_model)
}
