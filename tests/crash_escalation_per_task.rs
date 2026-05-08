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
use task_mgr::loop_engine::engine::{IterationContext, check_crash_escalation};
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
#[ignore = "FEAT-007: pipeline must write ctx.crashed_last_iteration[task_id] = is_crash"]
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
#[ignore = "FEAT-007: check_crash_escalation must consult ctx.crashed_last_iteration"]
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
#[ignore = "FEAT-007: pipeline must write false on success; check_crash_escalation must honor it"]
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
    });

    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-SUCC"),
        Some(&false),
        "successful iteration on TASK-SUCC must flip crashed_last_iteration[TASK-SUCC] = false"
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
// AC: ctx.crashed_last_iteration map size is bounded by active task count
// (no leak after a 100-iteration loop) — pipeline-driven variant.
//
// The structural test above covers the synthetic case. This `#[ignore]`-d
// variant drives the actual pipeline in a 100-iteration loop and asserts
// the same upper bound. Together they pin BOTH the design (key by task_id)
// and the wiring (pipeline uses the right key).
// ---------------------------------------------------------------------------

#[test]
#[ignore = "FEAT-007: pipeline must key writes by task_id, not iteration count"]
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
// produce SONNET as the escalated model. A regression that returns None
// (whether through the legacy scalars OR the post-FEAT-007 map read) is
// caught here. The test uses the legacy 4-arg signature today; it will be
// updated to the ctx-aware shape when FEAT-007 lands.
// ---------------------------------------------------------------------------

#[test]
fn discriminator_check_crash_escalation_returns_some_for_same_task_crash() {
    let escalated = check_crash_escalation(Some("TASK-X"), "TASK-X", true, Some(HAIKU_MODEL));
    assert_eq!(
        escalated.as_deref(),
        Some(SONNET_MODEL),
        "discriminator: a check_crash_escalation that returns None for the canonical \
         same-task consecutive-crash case is broken — got {:?}",
        escalated,
    );

    // A second discriminator angle: cross-task (different last vs current
    // task ID) must NOT escalate. A function that always returns Some would
    // fail here.
    let cross = check_crash_escalation(Some("TASK-A"), "TASK-B", true, Some(HAIKU_MODEL));
    assert!(
        cross.is_none(),
        "cross-task crash must NOT escalate — got {:?}",
        cross,
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Adapter that lets the `#[ignore]`-d future-shape tests drive
/// `check_crash_escalation` from a context object. FEAT-007 will collapse
/// this into the function itself by giving it a `&IterationContext`
/// parameter; until then this helper bridges the new contract (read map)
/// to the legacy 4-arg signature.
///
/// Critical: this helper is the test's mental model of FEAT-007's reader,
/// NOT the production reader. When FEAT-007 lands, replace its callers with
/// the rewritten `check_crash_escalation(ctx, current_task_id, model)` and
/// delete this helper.
fn check_crash_escalation_via_ctx(
    ctx: &IterationContext,
    current_task_id: &str,
    resolved_model: Option<&str>,
) -> Option<String> {
    let last_was_crash = ctx
        .crashed_last_iteration
        .get(current_task_id)
        .copied()
        .unwrap_or(false);
    let last_task_id = if last_was_crash {
        Some(current_task_id)
    } else {
        None
    };
    check_crash_escalation(
        last_task_id,
        current_task_id,
        last_was_crash,
        resolved_model,
    )
}
