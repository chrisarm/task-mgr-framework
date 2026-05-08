//! Comprehensive stress tests and edge cases for per-task crash tracking.
//!
//! These tests expand on `tests/crash_escalation_per_task.rs` (TEST-INIT-004)
//! by covering:
//!
//! - Multi-round wave→sequential→wave→sequential alternations on the same task.
//! - Cross-task isolation: task Y success must not clear task X crash.
//! - Bounded-map guarantees under a 100-iteration synthetic loop with 5 tasks.
//! - Terminal-status clear semantics:
//!     - "done" path: pipeline sets `crashed_last_iteration[id] = false`.
//!     - "blocked" path: `auto_block_task` does NOT touch the map; crash entry
//!       is retained from the last crash outcome.
//! - Stale map entries for archived/removed tasks do not affect new tasks.
//! - Repeated crash → done → crash reset: the ladder restarts from the base
//!   model after a completion (no memory of prior escalation).
//!
//! Notes:
//! - Integration test → cannot use `pub(crate)` test_utils helpers. All
//!   construction goes through the public API.
//! - All pipeline calls set `TASK_MGR_NO_EXTRACT_LEARNINGS=1` so tests are
//!   hermetic (no Claude subprocess spawned).
//! - `check_crash_escalation` is pure: same inputs → same output, no side
//!   effects. Pipeline tests exercise the full write path.

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::loop_engine::config::{CrashType, IterationOutcome};
use task_mgr::loop_engine::engine::{IterationContext, auto_block_task, check_crash_escalation};
use task_mgr::loop_engine::iteration_pipeline::{ProcessingParams, process_iteration_output};
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL, SONNET_MODEL};
use task_mgr::loop_engine::signals::SignalFlag;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

fn insert_in_progress_task(conn: &Connection, task_id: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        [task_id, "crash-tracking comprehensive fixture"],
    )
    .expect("insert task row");
}

fn disable_llm_extraction() {
    unsafe {
        std::env::set_var("TASK_MGR_NO_EXTRACT_LEARNINGS", "1");
    }
}

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
        fs::write(&prd_path, "{\"tasks\":[]}\n").expect("write prd");
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

/// Run `process_iteration_output` with a `Crash` outcome and return the
/// mutated outcome. Models the wave crash path (skip_git = true).
fn run_pipeline_crash(
    conn: &mut Connection,
    fx: &mut PipelineFixture,
    task_id: &str,
    iteration: u32,
) -> IterationOutcome {
    let mut outcome = IterationOutcome::Crash(CrashType::RuntimeError);
    process_iteration_output(ProcessingParams {
        conn,
        run_id: "test-run",
        iteration,
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
    outcome
}

/// Run `process_iteration_output` with an `Empty` (non-crash) outcome.
fn run_pipeline_noop(
    conn: &mut Connection,
    fx: &mut PipelineFixture,
    task_id: &str,
    iteration: u32,
) -> IterationOutcome {
    let mut outcome = IterationOutcome::Empty;
    process_iteration_output(ProcessingParams {
        conn,
        run_id: "test-run",
        iteration,
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
    outcome
}

/// Run `process_iteration_output` with a `<completed>` tag — models the
/// completion path (sets outcome to Completed, writes false into crash map).
fn run_pipeline_completion(
    conn: &mut Connection,
    fx: &mut PipelineFixture,
    task_id: &str,
    iteration: u32,
) -> IterationOutcome {
    let tag = format!("<completed>{task_id}</completed>\n");
    let mut outcome = IterationOutcome::Empty;
    process_iteration_output(ProcessingParams {
        conn,
        run_id: "test-run",
        iteration,
        task_id: Some(task_id),
        output: &tag,
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
    outcome
}

fn crash_map(entries: &[(&str, bool)]) -> std::collections::HashMap<String, bool> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_string(), *v))
        .collect()
}

// ---------------------------------------------------------------------------
// AC 1: wave→sequential→wave→sequential alternations for the same task ID
// ---------------------------------------------------------------------------

/// Four-round alternation: wave crash → sequential re-pick escalates → wave
/// success → sequential re-pick doesn't escalate.
///
/// This is the primary cross-mode wiring contract: the crash flag set during a
/// wave iteration must be visible to the subsequent sequential iteration, and
/// a success in sequential mode must clear it before the next wave round.
#[test]
fn wave_seq_wave_seq_alternation_escalation_propagates() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "TASK-ALT");

    let mut fx = PipelineFixture::new(db_temp.path());

    // Round 1 (wave path): crash on TASK-ALT.
    run_pipeline_crash(&mut conn, &mut fx, "TASK-ALT", 1);
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-ALT"),
        Some(&true),
        "round 1 wave crash must set flag = true"
    );

    // Round 2 (sequential path): re-pick escalates haiku → sonnet.
    let escalated = check_crash_escalation(
        &fx.ctx.crashed_last_iteration,
        "TASK-ALT",
        Some(HAIKU_MODEL),
    );
    assert_eq!(
        escalated.as_deref(),
        Some(SONNET_MODEL),
        "round 2 sequential re-pick must escalate haiku → sonnet"
    );

    // Sequential success: run noop so pipeline writes false.
    run_pipeline_noop(&mut conn, &mut fx, "TASK-ALT", 2);
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-ALT"),
        Some(&false),
        "sequential success must flip flag to false"
    );

    // Round 3 (wave path again): crash again.
    run_pipeline_crash(&mut conn, &mut fx, "TASK-ALT", 3);
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-ALT"),
        Some(&true),
        "round 3 wave crash must re-set flag = true"
    );

    // Round 4 (sequential path again): escalation fires from the base model.
    let escalated2 = check_crash_escalation(
        &fx.ctx.crashed_last_iteration,
        "TASK-ALT",
        Some(HAIKU_MODEL),
    );
    assert_eq!(
        escalated2.as_deref(),
        Some(SONNET_MODEL),
        "round 4 sequential re-pick must escalate from base model again"
    );
}

/// Multiple complete wave→sequential alternation cycles on the same task.
/// Each cycle: wave crash → sequential escalates → sequential success clears.
/// Asserts the ladder always restarts from the base model after a success.
#[test]
fn three_complete_alternation_cycles_ladder_restarts_each_time() {
    for cycle in 1u32..=3 {
        let map_crashed = crash_map(&[("TASK-X", true)]);
        let escalated = check_crash_escalation(&map_crashed, "TASK-X", Some(HAIKU_MODEL));
        assert_eq!(
            escalated.as_deref(),
            Some(SONNET_MODEL),
            "cycle {cycle}: haiku crash must always escalate to sonnet"
        );

        // Simulate success (pipeline writes false).
        let map_success = crash_map(&[("TASK-X", false)]);
        let after_success = check_crash_escalation(&map_success, "TASK-X", Some(HAIKU_MODEL));
        assert_eq!(
            after_success, None,
            "cycle {cycle}: after success, no escalation for haiku"
        );
    }
}

// ---------------------------------------------------------------------------
// AC 2: task X crash, task Y success, task X re-pick → escalates
// ---------------------------------------------------------------------------

/// Task Y success via pipeline must not clobber task X's crash flag.
/// After Y succeeds, X still has `true` in the map and escalates on re-pick.
#[test]
fn task_y_success_does_not_clear_task_x_crash() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "TASK-X");
    insert_in_progress_task(&conn, "TASK-Y");

    let mut fx = PipelineFixture::new(db_temp.path());

    // TASK-X crashes.
    run_pipeline_crash(&mut conn, &mut fx, "TASK-X", 1);
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-X"),
        Some(&true),
        "TASK-X must be flagged as crashed"
    );

    // TASK-Y succeeds (noop outcome).
    run_pipeline_noop(&mut conn, &mut fx, "TASK-Y", 2);
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-Y"),
        Some(&false),
        "TASK-Y must be flagged as not-crashed"
    );

    // TASK-X flag must still be true — Y's success is scoped to Y's key.
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-X"),
        Some(&true),
        "TASK-Y success must not clear TASK-X's crash flag"
    );

    // Re-picking TASK-X must escalate.
    let escalated =
        check_crash_escalation(&fx.ctx.crashed_last_iteration, "TASK-X", Some(HAIKU_MODEL));
    assert_eq!(
        escalated.as_deref(),
        Some(SONNET_MODEL),
        "TASK-X re-pick must escalate despite TASK-Y success"
    );

    // Re-picking TASK-Y must NOT escalate.
    let no_escalation =
        check_crash_escalation(&fx.ctx.crashed_last_iteration, "TASK-Y", Some(HAIKU_MODEL));
    assert_eq!(
        no_escalation, None,
        "TASK-Y re-pick must not escalate (last iteration was a success)"
    );
}

/// Multiple tasks crash and succeed in alternating order. Each task's
/// escalation is independently gated by its own map entry.
#[test]
fn multiple_tasks_independent_escalation_state() {
    // TASK-A crashed, TASK-B succeeded, TASK-C crashed.
    let map = crash_map(&[("TASK-A", true), ("TASK-B", false), ("TASK-C", true)]);

    // A escalates.
    let a = check_crash_escalation(&map, "TASK-A", Some(SONNET_MODEL));
    assert_eq!(a.as_deref(), Some(OPUS_MODEL), "TASK-A must escalate");

    // B does not escalate.
    let b = check_crash_escalation(&map, "TASK-B", Some(SONNET_MODEL));
    assert_eq!(b, None, "TASK-B (success) must not escalate");

    // C escalates.
    let c = check_crash_escalation(&map, "TASK-C", Some(HAIKU_MODEL));
    assert_eq!(c.as_deref(), Some(SONNET_MODEL), "TASK-C must escalate");

    // D (absent from map) does not escalate.
    let d = check_crash_escalation(&map, "TASK-D", Some(HAIKU_MODEL));
    assert_eq!(d, None, "TASK-D (absent) must not escalate");
}

// ---------------------------------------------------------------------------
// AC 3: map size remains bounded across a 100-iteration synthetic loop
// ---------------------------------------------------------------------------

/// 100 iterations across 5 tasks with wave-then-sequential alternation.
/// Each even iteration is a crash (wave), each odd is a noop (sequential).
/// Map size must never exceed the number of distinct active task IDs.
#[test]
fn map_bounded_100_iterations_5_tasks_mixed_modes() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    let task_ids = ["STRESS-A", "STRESS-B", "STRESS-C", "STRESS-D", "STRESS-E"];
    for id in &task_ids {
        insert_in_progress_task(&conn, id);
    }

    let mut fx = PipelineFixture::new(db_temp.path());

    for i in 0..100u32 {
        let task_id = task_ids[(i as usize) % task_ids.len()];
        if i % 2 == 0 {
            run_pipeline_crash(&mut conn, &mut fx, task_id, i + 1);
        } else {
            run_pipeline_noop(&mut conn, &mut fx, task_id, i + 1);
        }
    }

    assert!(
        fx.ctx.crashed_last_iteration.len() <= task_ids.len(),
        "after 100 iterations across {} tasks, map size must be <= {} (got {}). \
         The pipeline must key by task_id, NOT by iteration.",
        task_ids.len(),
        task_ids.len(),
        fx.ctx.crashed_last_iteration.len(),
    );

    // Final state: each task's last entry is at iteration 99 (even → crash)
    // or 98 (odd). Tasks at even final index → crashed; odd final index → noop.
    // Assert map contains exactly the 5 task IDs.
    for id in &task_ids {
        assert!(
            fx.ctx.crashed_last_iteration.contains_key(*id),
            "all 5 active tasks must have entries in the crash map; missing {id}"
        );
    }
}

/// Synthetic stress test: same shape as above but using direct map writes
/// (no pipeline) to pin the bounded-key-by-task_id design contract.
#[test]
fn synthetic_bounded_map_100_iterations_5_tasks() {
    let mut ctx = IterationContext::new(5);
    let tasks = ["S1", "S2", "S3", "S4", "S5"];

    for i in 0..100 {
        let tid = tasks[i % tasks.len()].to_string();
        ctx.crashed_last_iteration.insert(tid, i % 3 == 0);
    }

    assert_eq!(
        ctx.crashed_last_iteration.len(),
        tasks.len(),
        "synthetic loop: map size must equal unique task count, not iteration count"
    );
}

// ---------------------------------------------------------------------------
// AC 4: removal/clear semantics on terminal task statuses (done, blocked)
// ---------------------------------------------------------------------------

/// Terminal status "done": completing a task via the pipeline sets its crash
/// flag to false, so a subsequent re-pick does NOT trigger escalation.
///
/// This verifies the "done clear" path end-to-end through the pipeline.
#[test]
fn done_task_via_pipeline_sets_crash_flag_false() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "TASK-DONE");

    let mut fx = PipelineFixture::new(db_temp.path());

    // Prior crash.
    run_pipeline_crash(&mut conn, &mut fx, "TASK-DONE", 1);
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-DONE"),
        Some(&true),
        "crash must set flag = true"
    );

    // Completion: outcome is mutated to Completed → pipeline step 7 prunes the
    // entry (CODE-FIX-003: terminal transitions remove the entry rather than
    // flipping it to false; done tasks have no active lifetime in the map).
    // We re-insert as in_progress so mark_task_done can transition it.
    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = 'TASK-DONE'",
        [],
    )
    .expect("reset to in_progress");
    run_pipeline_completion(&mut conn, &mut fx, "TASK-DONE", 2);

    assert!(
        !fx.ctx.crashed_last_iteration.contains_key("TASK-DONE"),
        "completion must prune entry from crashed_last_iteration (CODE-FIX-003)"
    );

    // Re-picking TASK-DONE (e.g. hypothetically) must not escalate.
    let escalated = check_crash_escalation(
        &fx.ctx.crashed_last_iteration,
        "TASK-DONE",
        Some(HAIKU_MODEL),
    );
    assert_eq!(
        escalated, None,
        "after done, re-pick must not escalate (entry absent)"
    );
}

/// Terminal status "blocked": `auto_block_task` writes the `blocked` status
/// to the DB but does NOT touch `crashed_last_iteration`. The map retains the
/// last crash value. This means if a blocked task were somehow re-queued
/// (bug or manual reset), it would still trigger escalation.
///
/// The key contract: blocking is DB-only; crash history lives in the map.
#[test]
fn blocked_task_retains_crash_flag_auto_block_does_not_clear_map() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "TASK-BLK");

    let mut fx = PipelineFixture::new(db_temp.path());

    // Crash (pipeline writes true).
    run_pipeline_crash(&mut conn, &mut fx, "TASK-BLK", 1);
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-BLK"),
        Some(&true),
        "pre-block crash must set flag = true"
    );

    // Auto-block: writes `blocked` to the DB — does NOT touch the map.
    auto_block_task(&conn, "TASK-BLK", 3, 1).expect("auto_block_task");

    // Verify DB state is blocked.
    let status: String = conn
        .query_row("SELECT status FROM tasks WHERE id = 'TASK-BLK'", [], |r| {
            r.get(0)
        })
        .expect("query status");
    assert_eq!(status, "blocked", "task must be blocked in DB");

    // Map is unchanged — auto_block_task is a DB-only operation.
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-BLK"),
        Some(&true),
        "auto_block_task must not clear the crash map entry; it stays true"
    );

    // If the task were hypothetically re-tried, escalation would fire.
    let escalated = check_crash_escalation(
        &fx.ctx.crashed_last_iteration,
        "TASK-BLK",
        Some(SONNET_MODEL),
    );
    assert_eq!(
        escalated.as_deref(),
        Some(OPUS_MODEL),
        "blocked task with crash map entry still escalates if re-picked"
    );
}

/// Terminal status "blocked" variant: task reaches blocked status via a
/// non-crash outcome. The crash flag should reflect the actual last outcome
/// (false), not the fact that the task was blocked.
#[test]
fn blocked_task_non_crash_outcome_map_reflects_false() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "TASK-BLK2");

    let mut fx = PipelineFixture::new(db_temp.path());

    // Noop (empty) outcome — pipeline writes false.
    run_pipeline_noop(&mut conn, &mut fx, "TASK-BLK2", 1);
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-BLK2"),
        Some(&false),
        "noop outcome must set flag = false"
    );

    // Admin blocks the task (e.g. manual action or overflow rung 4).
    auto_block_task(&conn, "TASK-BLK2", 3, 1).expect("auto_block_task");

    // Map still reflects false — the task's last iteration was not a crash.
    assert_eq!(
        fx.ctx.crashed_last_iteration.get("TASK-BLK2"),
        Some(&false),
        "after non-crash + blocked, flag stays false"
    );

    // No escalation on hypothetical re-pick.
    let escalated = check_crash_escalation(
        &fx.ctx.crashed_last_iteration,
        "TASK-BLK2",
        Some(HAIKU_MODEL),
    );
    assert_eq!(
        escalated, None,
        "non-crash blocked task must not trigger escalation"
    );
}

// ---------------------------------------------------------------------------
// Edge cases: stale entries and repeated crashes
// ---------------------------------------------------------------------------

/// Task removed mid-loop: stale map entry for an archived/removed task does
/// not affect a different task's escalation decision.
///
/// This models the scenario where TASK-OLD crashed and was archived (or
/// manually removed from the active task list), but the crash map still
/// retains the entry. A new TASK-NEW should not be affected.
#[test]
fn stale_map_entry_for_removed_task_does_not_affect_new_task() {
    // Simulate: TASK-OLD crashed and was removed from active task set.
    let map = crash_map(&[("TASK-OLD", true)]);

    // TASK-NEW is a fresh task — absent from the map.
    let result = check_crash_escalation(&map, "TASK-NEW", Some(HAIKU_MODEL));
    assert_eq!(
        result, None,
        "stale crash entry for TASK-OLD must not escalate TASK-NEW"
    );

    // TASK-OLD entry is still stale-but-present; it would escalate if old ID were reused.
    let stale = check_crash_escalation(&map, "TASK-OLD", Some(HAIKU_MODEL));
    assert_eq!(
        stale.as_deref(),
        Some(SONNET_MODEL),
        "stale map entry for TASK-OLD still produces escalation if looked up by same ID"
    );
}

/// Repeated crashes on the same task follow the full model ladder.
/// Between crashes the pipeline writes `true` (crash) each time, but
/// the calling code feeds the escalated model back into the next check.
#[test]
fn repeated_crashes_traverse_full_ladder_haiku_sonnet_opus() {
    let crashed = crash_map(&[("TASK-REPEAT", true)]);

    let step1 = check_crash_escalation(&crashed, "TASK-REPEAT", Some(HAIKU_MODEL));
    assert_eq!(
        step1.as_deref(),
        Some(SONNET_MODEL),
        "step 1: haiku → sonnet"
    );

    let step2 = check_crash_escalation(&crashed, "TASK-REPEAT", step1.as_deref());
    assert_eq!(step2.as_deref(), Some(OPUS_MODEL), "step 2: sonnet → opus");

    let step3 = check_crash_escalation(&crashed, "TASK-REPEAT", step2.as_deref());
    assert_eq!(
        step3.as_deref(),
        Some(OPUS_MODEL),
        "step 3: opus → opus (ceiling)"
    );
}

/// Crash then done then crash again: the second crash sequence starts from
/// the base model. Prior escalation history is not retained across completions.
#[test]
fn crash_done_crash_ladder_restarts_from_base() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_in_progress_task(&conn, "TASK-CDR");

    let mut fx = PipelineFixture::new(db_temp.path());

    // First crash: escalates to sonnet.
    run_pipeline_crash(&mut conn, &mut fx, "TASK-CDR", 1);
    let escalated = check_crash_escalation(
        &fx.ctx.crashed_last_iteration,
        "TASK-CDR",
        Some(HAIKU_MODEL),
    );
    assert_eq!(escalated.as_deref(), Some(SONNET_MODEL));

    // Completion: entry is pruned (CODE-FIX-003).
    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = 'TASK-CDR'",
        [],
    )
    .ok();
    run_pipeline_completion(&mut conn, &mut fx, "TASK-CDR", 2);
    assert!(
        !fx.ctx.crashed_last_iteration.contains_key("TASK-CDR"),
        "completion must prune crash entry (CODE-FIX-003)"
    );

    // Second crash sequence: escalation must restart from haiku, not from the
    // previously escalated sonnet. The ladder has no memory of prior cycles.
    conn.execute(
        "INSERT OR IGNORE INTO tasks (id, title, status, priority) \
         VALUES ('TASK-CDR', 'crash-done-crash fixture', 'in_progress', 50)",
        [],
    )
    .ok();
    conn.execute(
        "UPDATE tasks SET status = 'in_progress' WHERE id = 'TASK-CDR'",
        [],
    )
    .ok();
    run_pipeline_crash(&mut conn, &mut fx, "TASK-CDR", 3);

    let escalated2 = check_crash_escalation(
        &fx.ctx.crashed_last_iteration,
        "TASK-CDR",
        Some(HAIKU_MODEL),
    );
    assert_eq!(
        escalated2.as_deref(),
        Some(SONNET_MODEL),
        "after done+re-crash, ladder restarts: haiku → sonnet (not sonnet → opus)"
    );
}

/// None-model crash after done+re-crash still escalates to opus (baseline rule).
#[test]
fn none_model_crash_after_done_escalates_to_opus() {
    let crashed = crash_map(&[("TASK-NONE", true)]);
    let result = check_crash_escalation(&crashed, "TASK-NONE", None);
    assert_eq!(
        result.as_deref(),
        Some(OPUS_MODEL),
        "None model crash must always escalate to opus"
    );
}
