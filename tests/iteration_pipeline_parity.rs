//! End-to-end parity tests for `iteration_pipeline::process_iteration_output`.
//!
//! TEST-003 expands TEST-INIT-003 (`tests/iteration_pipeline.rs`) with tests
//! that drive the **same fixture output** through both the sequential
//! (`skip_git_completion_detection = false`) and wave
//! (`skip_git_completion_detection = true`) configurations and assert that the
//! observable side effects match — modulo the two intentional delta knobs:
//!
//! - `skip_git_completion_detection`: wave mode never opens `git log` from
//!   per-slot worktrees; sequential mode does. Cases that exercise the git
//!   branch are excluded from parity.
//! - `slot_index`: wave mode passes a slot number into the progress log
//!   header; sequential mode passes `None`. The progress file format differs
//!   only in that header byte sequence; the observable DB state, learning
//!   feedback, and `ProcessingOutcome` content are byte-identical.
//!
//! Notes for future maintainers:
//!
//! - Integration test → cannot use the `pub(crate)` `loop_engine::test_utils`
//!   module (learning #896). Setup goes through the public DB API.
//! - DB setup uses `open_connection` + `create_schema` + `run_migrations` so
//!   the bandit window-stats columns and supersession-aware retrieval are
//!   wired (learning #896).
//! - All tests run with `TASK_MGR_NO_EXTRACT_LEARNINGS=1` to keep them
//!   hermetic — `extract_learnings_from_output` would otherwise spawn a real
//!   Claude subprocess. The opt-out is a documented public contract.
//! - Each parity test creates **two** independent DBs and runs the pipeline
//!   once per DB so neither configuration's side effects can leak into the
//!   other. The same fixture inputs (output text, shown_learning_ids, task
//!   metadata) are used on both sides; the only differences are the deltas
//!   listed above.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::learnings::bandit::{get_window_stats, record_learning_shown};
use task_mgr::learnings::crud::{RecordLearningParams, record_learning};
use task_mgr::loop_engine::config::IterationOutcome;
use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::iteration_pipeline::{
    ProcessingOutcome, ProcessingParams, process_iteration_output,
};
use task_mgr::loop_engine::signals::SignalFlag;
use task_mgr::models::{Confidence, LearningOutcome};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// Open a DB with full schema + all migrations applied. The `TempDir` MUST
/// outlive the `Connection` — dropping it deletes the on-disk file.
fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

/// Insert an `in_progress` task row so the completion paths have a row to
/// transition. Mirrors the helper in `tests/iteration_pipeline.rs`.
fn insert_in_progress_task(conn: &Connection, task_id: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        [task_id, "Parity test task"],
    )
    .expect("insert task row");
}

/// Insert a `runs` row so paths writing FK-referencing rows (e.g. the
/// `key_decisions` table — see migration v12, FK `run_id REFERENCES runs(run_id)`)
/// don't trip the foreign-key constraint. The pipeline always uses
/// `run_id = "parity-run"` in this file's `RunConfig`.
fn insert_run(conn: &Connection, run_id: &str) {
    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES (?1, 'active')",
        [run_id],
    )
    .expect("insert run row");
}

/// Insert a learning + record an initial `record_learning_shown` so
/// `record_learning_applied` (called inside the pipeline via
/// `feedback::record_iteration_feedback`) has a window row to update.
fn insert_shown_learning(conn: &Connection, title: &str) -> i64 {
    let inserted = record_learning(
        conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: title.into(),
            content: "Parity-test learning fixture".into(),
            task_id: None,
            run_id: None,
            root_cause: None,
            solution: None,
            applies_to_files: None,
            applies_to_task_types: None,
            applies_to_errors: None,
            tags: None,
            confidence: Confidence::High,
        },
    )
    .expect("record_learning");
    record_learning_shown(conn, inserted.learning_id, 1).expect("record_learning_shown");
    inserted.learning_id
}

/// Disable LLM-based learning extraction for the duration of the test. See
/// the module docs for why this is required.
fn disable_llm_extraction() {
    // SAFETY: cargo test is the canonical caller; we accept the inherent
    // single-process race on env vars. The opt-out is checked via
    // `is_extraction_disabled()` once per pipeline call — well after this
    // setter has returned — so a same-test-thread race is structurally
    // impossible.
    unsafe {
        std::env::set_var("TASK_MGR_NO_EXTRACT_LEARNINGS", "1");
    }
}

/// Owned state the test hands to `ProcessingParams` via borrows.
struct PipelineFixture {
    project: TempDir,
    prd_path: PathBuf,
    progress_path: PathBuf,
    db_dir: PathBuf,
    signal_flag: SignalFlag,
    ctx: IterationContext,
}

impl PipelineFixture {
    fn new(db_dir: &Path) -> Self {
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

/// A snapshot of the post-pipeline DB state we care about for parity. Only
/// fields that should match across sequential ↔ wave runs are captured;
/// transient timestamps (`completed_at`, `updated_at`) are NOT included
/// because their value is `datetime('now')` at write time and inevitably
/// differs across two runs.
#[derive(Debug, PartialEq, Eq)]
struct DbSnapshot {
    /// `(task_id, status)` pairs for every row in `tasks`.
    tasks: Vec<(String, String)>,
    /// `(learning_id, window_shown, window_applied)` for the supplied IDs in
    /// stable id-ascending order.
    bandit_window: Vec<(i64, i32, i32)>,
}

impl DbSnapshot {
    fn capture(conn: &Connection, learning_ids: &[i64]) -> Self {
        let mut tasks = Vec::new();
        let mut stmt = conn
            .prepare("SELECT id, status FROM tasks ORDER BY id")
            .expect("prepare tasks select");
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query tasks");
        for row in rows {
            tasks.push(row.expect("row"));
        }

        let mut bandit_window = Vec::with_capacity(learning_ids.len());
        let mut sorted = learning_ids.to_vec();
        sorted.sort_unstable();
        for id in sorted {
            let stats = get_window_stats(conn, id).expect("window_stats");
            bandit_window.push((id, stats.window_shown, stats.window_applied));
        }

        Self {
            tasks,
            bandit_window,
        }
    }
}

/// One configuration of the pipeline run. The test driver fills this twice —
/// once with `skip_git_completion_detection = false`, once with `true`.
struct RunConfig<'a> {
    skip_git_completion_detection: bool,
    /// `slot_index` is the second deliberate delta. Wave mode passes
    /// `Some(N)`; sequential passes `None`. It only flows into the progress
    /// log header — the DB state and `ProcessingOutcome` content do not
    /// depend on it.
    slot_index: Option<usize>,
    /// Initial `outcome` to mutate in place. The fixture controls this so
    /// the test can pin retroactive completion behavior.
    initial_outcome: IterationOutcome,
    output: &'a str,
    task_id: Option<&'a str>,
    shown_learning_ids: &'a [i64],
}

/// Run the pipeline once with the supplied config and return everything the
/// caller needs to compare against a sibling run (snapshot, ProcessingOutcome,
/// the post-call outcome, and the `crashed_last_iteration` view of `ctx`).
fn run_once(
    conn: &mut Connection,
    fx: &mut PipelineFixture,
    cfg: RunConfig<'_>,
) -> (
    DbSnapshot,
    ProcessingOutcome,
    IterationOutcome,
    Option<bool>,
) {
    let mut outcome = cfg.initial_outcome;
    let working_root = fx.project.path().to_path_buf();
    let result = process_iteration_output(ProcessingParams {
        conn,
        run_id: "parity-run",
        iteration: 1,
        task_id: cfg.task_id,
        output: cfg.output,
        conversation: None,
        shown_learning_ids: cfg.shown_learning_ids,
        outcome: &mut outcome,
        working_root: &working_root,
        git_scan_depth: 5,
        skip_git_completion_detection: cfg.skip_git_completion_detection,
        prd_path: &fx.prd_path,
        task_prefix: None,
        progress_path: &fx.progress_path,
        db_dir: &fx.db_dir,
        signal_flag: &fx.signal_flag,
        ctx: &mut fx.ctx,
        files_modified: &[],
        effective_model: None,
        effective_effort: None,
        slot_index: cfg.slot_index,
    });

    let crash_flag = cfg
        .task_id
        .and_then(|tid| fx.ctx.crashed_last_iteration.get(tid).copied());
    let snapshot = DbSnapshot::capture(conn, cfg.shown_learning_ids);
    (snapshot, result, outcome, crash_flag)
}

/// Compare two `ProcessingOutcome`s for parity. `completed_task_ids` ordering
/// can differ across the two paths if the implementation iterates a HashSet,
/// so we compare as sets while still pinning length.
fn assert_outcomes_equivalent(seq: &ProcessingOutcome, wave: &ProcessingOutcome) {
    assert_eq!(
        seq.tasks_completed, wave.tasks_completed,
        "tasks_completed must match across sequential and wave",
    );
    assert_eq!(
        seq.key_decisions_count, wave.key_decisions_count,
        "key_decisions_count must match across sequential and wave",
    );
    assert_eq!(
        seq.status_updates_applied, wave.status_updates_applied,
        "status_updates_applied must match across sequential and wave",
    );
    assert_eq!(
        seq.learnings_extracted, wave.learnings_extracted,
        "learnings_extracted must match across sequential and wave",
    );
    let seq_set: HashSet<&String> = seq.completed_task_ids.iter().collect();
    let wave_set: HashSet<&String> = wave.completed_task_ids.iter().collect();
    assert_eq!(
        seq_set, wave_set,
        "completed_task_ids set must match across sequential and wave",
    );
    assert_eq!(
        seq.completed_task_ids.len(),
        wave.completed_task_ids.len(),
        "completed_task_ids length must match (no extra entries in either path)",
    );
}

// ---------------------------------------------------------------------------
// AC: Sequential post-Claude DB state == wave-mode DB state for the same
// fixture output (modulo expected differences: skip_git, slot_index).
// ---------------------------------------------------------------------------

#[test]
fn parity_db_state_after_completed_tag() {
    disable_llm_extraction();

    // Sequential side.
    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    insert_in_progress_task(&conn_seq, "TEST-PARITY-DB-A");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let (snap_seq, out_seq, outcome_seq, crash_seq) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output: "<completed>TEST-PARITY-DB-A</completed>\n",
            task_id: Some("TEST-PARITY-DB-A"),
            shown_learning_ids: &[],
        },
    );

    // Wave side — same fixture, only knobs flipped.
    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    insert_in_progress_task(&conn_wave, "TEST-PARITY-DB-A");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (snap_wave, out_wave, outcome_wave, crash_wave) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Empty,
            output: "<completed>TEST-PARITY-DB-A</completed>\n",
            task_id: Some("TEST-PARITY-DB-A"),
            shown_learning_ids: &[],
        },
    );

    assert_eq!(
        snap_seq, snap_wave,
        "DB state must match across sequential ↔ wave for the same fixture output",
    );
    assert_outcomes_equivalent(&out_seq, &out_wave);
    assert_eq!(
        outcome_seq, outcome_wave,
        "post-call outcome must match across sequential ↔ wave",
    );
    assert_eq!(
        outcome_seq,
        IterationOutcome::Completed,
        "<completed> tag must promote outcome to Completed in both modes",
    );
    assert_eq!(
        crash_seq, crash_wave,
        "crashed_last_iteration[task_id] must match across sequential ↔ wave",
    );
    assert_eq!(
        crash_seq,
        Some(false),
        "Completed outcome must record crashed=false in both modes",
    );
}

#[test]
fn parity_db_state_after_status_done_tag() {
    disable_llm_extraction();

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    insert_in_progress_task(&conn_seq, "TEST-PARITY-STATUS");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let output = "<task-status>TEST-PARITY-STATUS:done</task-status>\n";
    let (snap_seq, out_seq, outcome_seq, _crash_seq) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-STATUS"),
            shown_learning_ids: &[],
        },
    );

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    insert_in_progress_task(&conn_wave, "TEST-PARITY-STATUS");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (snap_wave, out_wave, outcome_wave, _crash_wave) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(2),
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-STATUS"),
            shown_learning_ids: &[],
        },
    );

    assert_eq!(snap_seq, snap_wave, "DB state must match across modes");
    assert_outcomes_equivalent(&out_seq, &out_wave);
    assert_eq!(
        outcome_seq, outcome_wave,
        "post-call outcome must match across modes",
    );
    assert_eq!(out_seq.status_updates_applied, 1);
    assert_eq!(
        out_seq.tasks_completed, 1,
        "single :done status tag must mark the task done exactly once",
    );
}

// ---------------------------------------------------------------------------
// AC: Learnings extracted in wave mode match learnings extracted in
// sequential mode for the same Claude output.
//
// With `TASK_MGR_NO_EXTRACT_LEARNINGS=1` set, the extraction subprocess is
// short-circuited in BOTH paths: `learnings_extracted` is 0 and no rows are
// added to the `learnings` table. The contract this test pins is that the
// opt-out fires identically in both modes — so the extraction-related state
// remains parity. (The "live extraction" parity belongs in a mocked test;
// see the noted ignore in tests/iteration_pipeline.rs.)
// ---------------------------------------------------------------------------

#[test]
fn parity_learnings_extracted_count_matches() {
    disable_llm_extraction();

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    insert_in_progress_task(&conn_seq, "TEST-PARITY-LEARN");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let baseline_seq: i64 = conn_seq
        .query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
        .unwrap_or(0);
    let output = "<learning><title>Parity test claim</title>\
                  <content>This would be a real learning.</content>\
                  </learning>\n\
                  <completed>TEST-PARITY-LEARN</completed>\n";
    let (_, out_seq, _, _) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Completed,
            output,
            task_id: Some("TEST-PARITY-LEARN"),
            shown_learning_ids: &[],
        },
    );
    let after_seq: i64 = conn_seq
        .query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
        .expect("count learnings seq");

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    insert_in_progress_task(&conn_wave, "TEST-PARITY-LEARN");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let baseline_wave: i64 = conn_wave
        .query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
        .unwrap_or(0);
    let (_, out_wave, _, _) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Completed,
            output,
            task_id: Some("TEST-PARITY-LEARN"),
            shown_learning_ids: &[],
        },
    );
    let after_wave: i64 = conn_wave
        .query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
        .expect("count learnings wave");

    assert_eq!(
        out_seq.learnings_extracted, out_wave.learnings_extracted,
        "ProcessingOutcome.learnings_extracted must match across modes",
    );
    assert_eq!(
        after_seq - baseline_seq,
        after_wave - baseline_wave,
        "delta in `learnings` row count must match across modes",
    );
}

// ---------------------------------------------------------------------------
// AC: Feedback rows in wave mode match sequential mode for the same
// shown_learning_ids.
//
// `feedback::record_iteration_feedback` only fires bandit application on
// `Completed` outcomes. We exercise both: a Completed run (where each shown
// learning's window_applied advances by 1) and a non-Completed run (where it
// stays put). Both modes MUST produce the same window stats.
// ---------------------------------------------------------------------------

#[test]
fn parity_bandit_feedback_on_completed_run() {
    disable_llm_extraction();

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    let id_a_seq = insert_shown_learning(&conn_seq, "Parity learning A");
    let id_b_seq = insert_shown_learning(&conn_seq, "Parity learning B");
    insert_in_progress_task(&conn_seq, "TEST-PARITY-FEEDBACK");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let shown_seq = [id_a_seq, id_b_seq];
    let output = "<completed>TEST-PARITY-FEEDBACK</completed>\n";
    let (snap_seq, _out_seq, _, _) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-FEEDBACK"),
            shown_learning_ids: &shown_seq,
        },
    );

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    let id_a_wave = insert_shown_learning(&conn_wave, "Parity learning A");
    let id_b_wave = insert_shown_learning(&conn_wave, "Parity learning B");
    insert_in_progress_task(&conn_wave, "TEST-PARITY-FEEDBACK");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let shown_wave = [id_a_wave, id_b_wave];
    let (snap_wave, _out_wave, _, _) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(1),
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-FEEDBACK"),
            shown_learning_ids: &shown_wave,
        },
    );

    // The two DBs assigned the same primary keys (1, 2) because each starts
    // empty, so we can compare the bandit_window vectors directly.
    assert_eq!(
        snap_seq.bandit_window, snap_wave.bandit_window,
        "bandit window stats must match across sequential ↔ wave for same shown ids",
    );
    for (id, shown, applied) in &snap_seq.bandit_window {
        assert!(
            *shown >= 1 && *applied == 1,
            "Completed run must increment window_applied for learning {id}; got shown={shown} applied={applied}",
        );
    }
}

#[test]
fn parity_bandit_feedback_on_non_completed_run() {
    disable_llm_extraction();

    // Run both paths with an `Empty` initial outcome and an output that
    // produces NO completion signals. In both modes, `record_iteration_feedback`
    // must NOT advance window_applied.
    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    let id_seq = insert_shown_learning(&conn_seq, "Empty-run learning");
    insert_in_progress_task(&conn_seq, "TEST-PARITY-NOOP");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let (snap_seq, _, outcome_seq, _) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output: "no completion tags here",
            task_id: Some("TEST-PARITY-NOOP"),
            shown_learning_ids: &[id_seq],
        },
    );

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    let id_wave = insert_shown_learning(&conn_wave, "Empty-run learning");
    insert_in_progress_task(&conn_wave, "TEST-PARITY-NOOP");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (snap_wave, _, outcome_wave, _) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(3),
            initial_outcome: IterationOutcome::Empty,
            output: "no completion tags here",
            task_id: Some("TEST-PARITY-NOOP"),
            shown_learning_ids: &[id_wave],
        },
    );

    assert_eq!(outcome_seq, IterationOutcome::Empty);
    assert_eq!(outcome_wave, IterationOutcome::Empty);
    assert_eq!(
        snap_seq.bandit_window, snap_wave.bandit_window,
        "bandit window stats must match across modes when no completion fires",
    );
    for (id, _shown, applied) in &snap_seq.bandit_window {
        assert_eq!(
            *applied, 0,
            "non-Completed run must NOT increment window_applied for learning {id}",
        );
    }
}

// ---------------------------------------------------------------------------
// AC: is_task_reported_already_complete fallback fires equivalently in both
// paths.
//
// This is the wave-mode parity bug the PRD calls out: the legacy
// process_slot_result never invoked the already-complete fallback, so
// re-claimed tasks completed in a previous run were never flipped done.
// ---------------------------------------------------------------------------

#[test]
fn parity_already_complete_fallback_fires_in_both_modes() {
    disable_llm_extraction();

    let output = "I checked and TEST-PARITY-ALREADY is already complete from a previous run.";

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    insert_in_progress_task(&conn_seq, "TEST-PARITY-ALREADY");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let (snap_seq, out_seq, outcome_seq, _) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-ALREADY"),
            shown_learning_ids: &[],
        },
    );

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    insert_in_progress_task(&conn_wave, "TEST-PARITY-ALREADY");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (snap_wave, out_wave, outcome_wave, _) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-ALREADY"),
            shown_learning_ids: &[],
        },
    );

    // Both paths must end in `Completed` and the task row must be `done`.
    assert_eq!(
        outcome_seq,
        IterationOutcome::Completed,
        "sequential: already-complete fallback must promote outcome to Completed",
    );
    assert_eq!(
        outcome_wave,
        IterationOutcome::Completed,
        "wave: already-complete fallback must promote outcome to Completed (PRD parity fix)",
    );
    assert_eq!(
        snap_seq, snap_wave,
        "DB state after already-complete fallback must match across sequential ↔ wave",
    );
    assert_outcomes_equivalent(&out_seq, &out_wave);
    assert_eq!(
        out_seq.tasks_completed, 1,
        "fallback must record exactly one completion in both paths",
    );
}

#[test]
fn parity_already_complete_fallback_does_not_fire_when_phrase_absent() {
    disable_llm_extraction();

    // Same shape as the prior test, but the output contains NO already-complete
    // phrasing. Both paths must leave the task unflipped and outcome Empty.
    let output = "Working on it; will continue next iteration.";

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    insert_in_progress_task(&conn_seq, "TEST-PARITY-NOFALLBACK");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let (snap_seq, out_seq, outcome_seq, _) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-NOFALLBACK"),
            shown_learning_ids: &[],
        },
    );

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    insert_in_progress_task(&conn_wave, "TEST-PARITY-NOFALLBACK");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (snap_wave, out_wave, outcome_wave, _) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-NOFALLBACK"),
            shown_learning_ids: &[],
        },
    );

    assert_eq!(outcome_seq, IterationOutcome::Empty);
    assert_eq!(outcome_wave, IterationOutcome::Empty);
    assert_eq!(
        snap_seq, snap_wave,
        "no-op runs must produce identical DB state in both modes",
    );
    assert_outcomes_equivalent(&out_seq, &out_wave);
    assert_eq!(out_seq.tasks_completed, 0);
    assert!(out_seq.completed_task_ids.is_empty());
}

// ---------------------------------------------------------------------------
// AC: Dedup HashSet correctly handles overlapping completion paths.
//
// The pipeline routes through up to four completion branches in one pass:
//   4a) <task-status>:done</task-status>
//   4b) <completed>...</completed>
//   4c) git-commit detection (sequential only)
//   4d) output-scan fallback (used when 4a/4b emitted nothing)
//   4e) is_task_reported_already_complete fallback
//
// Multiple branches can fire for the same task ID. The dedup HashSet
// guarantees `tasks_completed`, `completed_task_ids` count, and the
// underlying DB row count each see the task EXACTLY ONCE. The tests below
// hammer the boundaries: same-id across (4a)+(4b), cross-task with one
// shared id, dedup-with-fallback off-path, and the parity stipulation that
// both modes apply the same dedup semantics.
// ---------------------------------------------------------------------------

#[test]
fn dedup_hashset_collapses_status_and_completed_for_same_id() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    insert_in_progress_task(&conn, "TEST-PARITY-DEDUP-1");
    let mut fx = PipelineFixture::new(db_temp.path());
    let output = "<task-status>TEST-PARITY-DEDUP-1:done</task-status>\n\
                  <completed>TEST-PARITY-DEDUP-1</completed>\n";
    let (_, result, outcome, _) = run_once(
        &mut conn,
        &mut fx,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-DEDUP-1"),
            shown_learning_ids: &[],
        },
    );

    assert_eq!(result.tasks_completed, 1, "dedup must collapse to 1");
    assert_eq!(
        result.completed_task_ids,
        vec!["TEST-PARITY-DEDUP-1".to_string()],
        "completed_task_ids must hold the deduped task exactly once",
    );
    assert_eq!(outcome, IterationOutcome::Completed);
    // Verify that the underlying DB row was only flipped once: status='done'
    // and not stuck at any inconsistent value.
    let status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE id = ?1",
            ["TEST-PARITY-DEDUP-1"],
            |r| r.get(0),
        )
        .expect("task row");
    assert_eq!(status, "done");
}

#[test]
fn dedup_hashset_handles_cross_task_completion_with_shared_id() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    // Three tasks: claimed (A), and two cross-task completions (B, C). The
    // claimed task A surfaces via BOTH <task-status> AND <completed> — that's
    // the "shared id" duplication. B appears only via <completed>, C appears
    // via <completed> twice (defensive against the parser emitting duplicates).
    insert_in_progress_task(&conn, "TEST-PARITY-DEDUP-A");
    insert_in_progress_task(&conn, "TEST-PARITY-DEDUP-B");
    insert_in_progress_task(&conn, "TEST-PARITY-DEDUP-C");
    let mut fx = PipelineFixture::new(db_temp.path());
    let output = "<task-status>TEST-PARITY-DEDUP-A:done</task-status>\n\
                  <completed>TEST-PARITY-DEDUP-A</completed>\n\
                  <completed>TEST-PARITY-DEDUP-B</completed>\n\
                  <completed>TEST-PARITY-DEDUP-C</completed>\n\
                  <completed>TEST-PARITY-DEDUP-C</completed>\n";
    let (snap, result, outcome, _) = run_once(
        &mut conn,
        &mut fx,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-DEDUP-A"),
            shown_learning_ids: &[],
        },
    );

    assert_eq!(
        result.tasks_completed, 3,
        "three unique IDs must each count once after dedup",
    );
    assert_eq!(
        result.completed_task_ids.len(),
        3,
        "completed_task_ids must hold one entry per unique completion",
    );
    let ids: HashSet<String> = result.completed_task_ids.iter().cloned().collect();
    assert!(ids.contains("TEST-PARITY-DEDUP-A"));
    assert!(ids.contains("TEST-PARITY-DEDUP-B"));
    assert!(ids.contains("TEST-PARITY-DEDUP-C"));
    assert_eq!(outcome, IterationOutcome::Completed);

    // All three rows ended at `done`.
    for (_, status) in &snap.tasks {
        assert_eq!(
            status, "done",
            "every task must be marked done after the multi-completion pass",
        );
    }
}

#[test]
fn dedup_hashset_parity_across_sequential_and_wave_for_overlapping_paths() {
    disable_llm_extraction();
    let output = "<task-status>TEST-PARITY-DEDUP-OVL:done</task-status>\n\
                  <completed>TEST-PARITY-DEDUP-OVL</completed>\n\
                  <completed>TEST-PARITY-DEDUP-OVL</completed>\n";

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    insert_in_progress_task(&conn_seq, "TEST-PARITY-DEDUP-OVL");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let (snap_seq, out_seq, outcome_seq, _) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-DEDUP-OVL"),
            shown_learning_ids: &[],
        },
    );

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    insert_in_progress_task(&conn_wave, "TEST-PARITY-DEDUP-OVL");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (snap_wave, out_wave, outcome_wave, _) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Empty,
            output,
            task_id: Some("TEST-PARITY-DEDUP-OVL"),
            shown_learning_ids: &[],
        },
    );

    assert_eq!(snap_seq, snap_wave, "DB state must match across modes");
    assert_outcomes_equivalent(&out_seq, &out_wave);
    assert_eq!(outcome_seq, outcome_wave);
    assert_eq!(
        out_seq.tasks_completed, 1,
        "dedup must collapse status + 2x completed to exactly 1 in sequential",
    );
    assert_eq!(
        out_wave.tasks_completed, 1,
        "dedup must collapse status + 2x completed to exactly 1 in wave",
    );
    // Status update applied count is NOT subject to dedup — each tag is a
    // separate apply call. A single `:done` tag must register as 1 status
    // update applied in both modes.
    assert_eq!(out_seq.status_updates_applied, 1);
    assert_eq!(out_wave.status_updates_applied, 1);
}

// ---------------------------------------------------------------------------
// Crash tracking parity. `crashed_last_iteration[task_id]` MUST mirror
// `matches!(outcome, IterationOutcome::Crash(_))` after the call, regardless
// of which mode runs the pipeline. This pins the FEAT-007 contract.
// ---------------------------------------------------------------------------

#[test]
fn parity_crash_tracking_writes_for_crash_outcome() {
    use task_mgr::loop_engine::config::CrashType;
    disable_llm_extraction();

    let crash = IterationOutcome::Crash(CrashType::RuntimeError);

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    insert_in_progress_task(&conn_seq, "TEST-PARITY-CRASH");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let (_, _, outcome_seq, crash_seq) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: crash.clone(),
            output: "claude crashed mid-iteration",
            task_id: Some("TEST-PARITY-CRASH"),
            shown_learning_ids: &[],
        },
    );

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    insert_in_progress_task(&conn_wave, "TEST-PARITY-CRASH");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (_, _, outcome_wave, crash_wave) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: crash,
            output: "claude crashed mid-iteration",
            task_id: Some("TEST-PARITY-CRASH"),
            shown_learning_ids: &[],
        },
    );

    assert_eq!(outcome_seq, outcome_wave);
    assert!(matches!(outcome_seq, IterationOutcome::Crash(_)));
    assert_eq!(
        crash_seq, crash_wave,
        "crashed_last_iteration must match across sequential ↔ wave for a Crash outcome",
    );
    assert_eq!(
        crash_seq,
        Some(true),
        "Crash outcome must record crashed=true via crashed_last_iteration",
    );
}

// ---------------------------------------------------------------------------
// AC (coverage): exercise key_decisions storage in both modes — adds a
// `<key-decision>` tag to the fixture and asserts the row count matches.
// This rounds out coverage on the pipeline's Step 2 across both paths.
// ---------------------------------------------------------------------------

#[test]
fn parity_key_decisions_stored_in_both_modes() {
    disable_llm_extraction();
    let key_decision_block = "<key-decision>\n\
        <title>Pick storage backend</title>\n\
        <description>Trade speed against complexity</description>\n\
        <option label=\"SQLite\">simple, embedded</option>\n\
        <option label=\"Postgres\">scalable, more ops</option>\n\
        </key-decision>\n";

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    insert_run(&conn_seq, "parity-run");
    insert_in_progress_task(&conn_seq, "TEST-PARITY-KD");
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let (_, out_seq, _, _) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output: key_decision_block,
            task_id: Some("TEST-PARITY-KD"),
            shown_learning_ids: &[],
        },
    );
    let kd_count_seq: i64 = conn_seq
        .query_row("SELECT COUNT(*) FROM key_decisions", [], |r| r.get(0))
        .expect("count key_decisions seq");

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    insert_run(&conn_wave, "parity-run");
    insert_in_progress_task(&conn_wave, "TEST-PARITY-KD");
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (_, out_wave, _, _) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Empty,
            output: key_decision_block,
            task_id: Some("TEST-PARITY-KD"),
            shown_learning_ids: &[],
        },
    );
    let kd_count_wave: i64 = conn_wave
        .query_row("SELECT COUNT(*) FROM key_decisions", [], |r| r.get(0))
        .expect("count key_decisions wave");

    assert_eq!(
        out_seq.key_decisions_count, out_wave.key_decisions_count,
        "ProcessingOutcome.key_decisions_count must match across modes",
    );
    assert_eq!(
        kd_count_seq, kd_count_wave,
        "key_decisions row count must match across modes",
    );
    assert_eq!(
        out_seq.key_decisions_count, 1,
        "single <key-decision> tag must be stored exactly once",
    );
}

// ---------------------------------------------------------------------------
// AC (coverage): when `task_id == None`, the pipeline must NOT touch the
// per-task crash map and MUST NOT panic. This exercises the early-return
// guard at the top of completion + crash bookkeeping. Both modes must behave
// identically.
// ---------------------------------------------------------------------------

#[test]
fn parity_no_claimed_task_skips_completion_and_crash_bookkeeping() {
    disable_llm_extraction();

    let (db_temp_seq, mut conn_seq) = setup_migrated_db();
    let mut fx_seq = PipelineFixture::new(db_temp_seq.path());
    let (_, out_seq, outcome_seq, _) = run_once(
        &mut conn_seq,
        &mut fx_seq,
        RunConfig {
            skip_git_completion_detection: false,
            slot_index: None,
            initial_outcome: IterationOutcome::Empty,
            output: "<completed>SOME-OTHER-TASK</completed>",
            task_id: None,
            shown_learning_ids: &[],
        },
    );

    let (db_temp_wave, mut conn_wave) = setup_migrated_db();
    let mut fx_wave = PipelineFixture::new(db_temp_wave.path());
    let (_, out_wave, outcome_wave, _) = run_once(
        &mut conn_wave,
        &mut fx_wave,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: IterationOutcome::Empty,
            output: "<completed>SOME-OTHER-TASK</completed>",
            task_id: None,
            shown_learning_ids: &[],
        },
    );

    assert_eq!(outcome_seq, IterationOutcome::Empty);
    assert_eq!(outcome_wave, IterationOutcome::Empty);
    assert!(
        fx_seq.ctx.crashed_last_iteration.is_empty(),
        "no claimed task must leave crashed_last_iteration untouched (sequential)",
    );
    assert!(
        fx_wave.ctx.crashed_last_iteration.is_empty(),
        "no claimed task must leave crashed_last_iteration untouched (wave)",
    );
    assert_outcomes_equivalent(&out_seq, &out_wave);
    assert_eq!(out_seq.tasks_completed, 0);
}

// ---------------------------------------------------------------------------
// M1 guard: claim_succeeded=false slots must not pollute crashed_last_iteration.
//
// The guard in process_slot_result returns early when claim_succeeded=false,
// preventing both the overflow handler and process_iteration_output from
// running for tasks that were never moved to in_progress.
//
// This test simulates a 3-slot wave:
//   slot 0 (claim_succeeded=true,  crash) → pipeline runs → crash recorded
//   slot 1 (claim_succeeded=false, crash) → guard fires   → pipeline skipped
//   slot 2 (claim_succeeded=true,  crash) → pipeline runs → crash recorded
//
// Observable invariant: ctx.crashed_last_iteration.len() == 2 (slots 0+2 only).
// Slot 1's task_id must NOT appear — the pipeline never ran for it.
// ---------------------------------------------------------------------------

#[test]
fn claim_failed_slot_does_not_pollute_crash_map() {
    use task_mgr::loop_engine::config::CrashType;
    disable_llm_extraction();

    let crash = IterationOutcome::Crash(CrashType::RuntimeError);

    // One DB + one shared ctx — all three slots share IterationContext in a real wave.
    let (db_temp, mut conn) = setup_migrated_db();
    insert_run(&conn, "parity-run");
    insert_in_progress_task(&conn, "WAVE-SLOT-0-TASK");
    // WAVE-SLOT-1-TASK intentionally absent: claim_succeeded=false means the row
    // was never moved to in_progress, so the pipeline must not run for it.
    insert_in_progress_task(&conn, "WAVE-SLOT-2-TASK");
    let mut fx = PipelineFixture::new(db_temp.path());

    // Slot 0: claim succeeded → pipeline runs → crash recorded in crash map.
    run_once(
        &mut conn,
        &mut fx,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(0),
            initial_outcome: crash.clone(),
            output: "slot 0 crashed",
            task_id: Some("WAVE-SLOT-0-TASK"),
            shown_learning_ids: &[],
        },
    );

    // Slot 1: claim_succeeded=false → process_slot_result returns early.
    // The guard in engine.rs prevents process_iteration_output from being called
    // here. We simulate that by not calling run_once for this slot.

    // Slot 2: claim succeeded → pipeline runs → crash recorded in crash map.
    run_once(
        &mut conn,
        &mut fx,
        RunConfig {
            skip_git_completion_detection: true,
            slot_index: Some(2),
            initial_outcome: crash,
            output: "slot 2 crashed",
            task_id: Some("WAVE-SLOT-2-TASK"),
            shown_learning_ids: &[],
        },
    );

    assert_eq!(
        fx.ctx.crashed_last_iteration.len(),
        2,
        "crash map must contain exactly 2 entries: slot 0 + slot 2 \
         (slot 1 claim_succeeded=false must be absent)",
    );
    assert!(
        fx.ctx
            .crashed_last_iteration
            .contains_key("WAVE-SLOT-0-TASK"),
        "slot 0 crash must be recorded",
    );
    assert!(
        fx.ctx
            .crashed_last_iteration
            .contains_key("WAVE-SLOT-2-TASK"),
        "slot 2 crash must be recorded",
    );
    assert!(
        !fx.ctx
            .crashed_last_iteration
            .contains_key("WAVE-SLOT-1-TASK"),
        "slot 1 (claim_succeeded=false) must NOT appear in crashed_last_iteration",
    );
}
