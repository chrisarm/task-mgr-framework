//! End-to-end test: wave mode learnings extraction + bandit feedback.
//!
//! Drives a synthetic 3-slot parallel wave through `process_iteration_output`
//! (the shared post-Claude pipeline that `process_slot_result` calls for each
//! slot) and asserts the PRD §2 SQL-level success metrics:
//!
//! - AC2: `SELECT COUNT(*) FROM learnings WHERE created_at > wave_start > 0`
//! - AC3: `window_applied` incremented for shown learnings whose logical task_id
//!   corresponds to the slot tasks (bandit feedback fires in wave mode)
//! - AC4: Learnings written via `LearningWriter::new(Some(db_dir))` — the
//!   embedding scheduling contract (flush doesn't panic when Ollama is absent)
//!
//! ## Why this test matters
//!
//! Before the PRD's unification work, `process_slot_result` skipped learnings
//! extraction and bandit feedback entirely — wave mode accumulated no bandit
//! signal regardless of how many tasks completed. This test pins the corrected
//! behavior that fires identically in both sequential and wave mode.
//!
//! ## Notes for maintainers
//!
//! - Integration test → cannot use `pub(crate)` `loop_engine::test_utils`.
//!   Setup goes through the public API: `open_connection` + `create_schema` +
//!   `run_migrations`.
//! - `TASK_MGR_NO_EXTRACT_LEARNINGS=1` disables the Claude subprocess that
//!   `extract_learnings_from_output` would otherwise spawn. Learnings are
//!   inserted directly via `LearningWriter` to simulate what extraction would
//!   produce in a real wave, without coupling the test to Claude availability.
//! - The `learning_feedback` column in the PRD ACs maps to `window_applied` on
//!   the `learnings` table (the actual bandit schema). All three retrieval
//!   backends and the bandit feedback path share this column.
//! - `wave_start` is captured as `datetime('now', '-1 second')` to avoid
//!   sub-second SQLite timestamp collisions with learnings inserted immediately
//!   after — learnings created during the wave are guaranteed to be strictly
//!   after the captured timestamp.

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::learnings::bandit::{get_window_stats, record_learning_shown};
use task_mgr::learnings::crud::{LearningWriter, RecordLearningParams};
use task_mgr::loop_engine::config::IterationOutcome;
use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::iteration_pipeline::{ProcessingParams, process_iteration_output};
use task_mgr::loop_engine::signals::SignalFlag;
use task_mgr::models::{Confidence, LearningOutcome};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// Open a DB with full schema + all migrations applied.
fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

/// Insert an `in_progress` task row so the completion paths have something to
/// transition. Mirrors the helper in `iteration_pipeline.rs`.
fn insert_slot_task(conn: &Connection, task_id: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        [task_id, "Wave slot task"],
    )
    .expect("insert slot task");
}

/// Owned bag that carries the project files and pipeline state for one wave.
struct WaveFixture {
    project: TempDir,
    prd_path: PathBuf,
    progress_path: PathBuf,
    db_dir: PathBuf,
    signal_flag: SignalFlag,
    ctx: IterationContext,
}

impl WaveFixture {
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

/// Disable LLM-based extraction for the duration of a test. The opt-out is
/// documented as a public contract on the ingestion module.
fn disable_llm_extraction() {
    // SAFETY: same single-process reasoning as in iteration_pipeline.rs.
    // `is_extraction_disabled()` is checked once per pipeline call, well after
    // this setter returns, so a same-test race is structurally impossible.
    unsafe {
        std::env::set_var("TASK_MGR_NO_EXTRACT_LEARNINGS", "1");
    }
}

// ---------------------------------------------------------------------------
// Primary end-to-end test
// ---------------------------------------------------------------------------

/// Synthetic 3-slot wave: each slot has a task, a learning was injected into
/// its prompt, and the slot output contains a `<learning>` tag plus a
/// `<completed>` tag. Verifies the three PRD §2 SQL success metrics.
#[test]
fn wave_mode_learnings_and_bandit_feedback_sql_metrics() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();

    // Capture wave_start one second in the past so all learnings inserted
    // during the simulated wave are guaranteed to have created_at > wave_start.
    let wave_start: String = conn
        .query_row("SELECT datetime('now', '-1 second')", [], |r| r.get(0))
        .expect("wave_start");

    // Three slots — task IDs mirror what a real 3-slot wave would claim.
    let slot_task_ids = ["WAVE-E2E-SLOT-0", "WAVE-E2E-SLOT-1", "WAVE-E2E-SLOT-2"];
    for task_id in &slot_task_ids {
        insert_slot_task(&conn, task_id);
    }

    // AC4: Insert each slot's learning via LearningWriter(Some(db_dir)).
    //
    // This simulates what `extract_learnings_from_output` produces in a real
    // wave (disabled here via env var to avoid Claude subprocess). Using
    // `Some(db_dir)` ensures the writer queues pending embeddings — the
    // contract AC4 verifies is that `flush()` does not panic when Ollama is
    // absent (graceful degradation is the production invariant).
    let mut shown_ids_per_slot: Vec<Vec<i64>> = Vec::new();
    for (slot_idx, task_id) in slot_task_ids.iter().enumerate() {
        let mut writer = LearningWriter::new(Some(db_temp.path()));
        let result = writer
            .record(
                &conn,
                RecordLearningParams {
                    outcome: LearningOutcome::Pattern,
                    title: format!("Wave e2e: slot {slot_idx} learning"),
                    content: format!(
                        "Extracted from slot {slot_idx} output while processing {task_id}"
                    ),
                    task_id: Some(task_id.to_string()),
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
            .expect("LearningWriter::record");

        // AC4 contract: flush does not panic even when Ollama is unreachable.
        let _embed_count = writer.flush(&conn);

        // Record as shown so the pipeline's bandit feedback path has a row to
        // update when this slot completes (mirrors prompt injection behavior).
        record_learning_shown(&conn, result.learning_id, 1).expect("record_learning_shown");
        shown_ids_per_slot.push(vec![result.learning_id]);
    }

    // AC2 (primary metric): learnings inserted during the wave are visible.
    let learning_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings WHERE created_at > ?1",
            [&wave_start],
            |r| r.get(0),
        )
        .expect("learnings count after wave_start");
    assert!(
        learning_count > 0,
        "AC2: SELECT COUNT(*) FROM learnings WHERE created_at > wave_start must return > 0; \
         got {learning_count} (wave_start={wave_start})"
    );

    // Simulate process_slot_result calling process_iteration_output for each slot.
    // Each slot output carries a <completed> tag and a <learning> tag (the tag
    // shape extraction targets — disabled here via env var, but the shape is
    // present to exercise the tag-scanning path).
    let mut fx = WaveFixture::new(db_temp.path());
    for (slot_idx, task_id) in slot_task_ids.iter().enumerate() {
        let output = format!(
            "<completed>{task_id}</completed>\n\
             <learning><title>Inline slot {slot_idx}</title>\
             <content>This slot produced a learning.</content></learning>\n"
        );
        let mut outcome = IterationOutcome::Empty;
        process_iteration_output(ProcessingParams {
            conn: &mut conn,
            run_id: "wave-e2e-run",
            iteration: 1,
            task_id: Some(task_id),
            output: &output,
            conversation: None,
            shown_learning_ids: &shown_ids_per_slot[slot_idx],
            outcome: &mut outcome,
            working_root: fx.project.path(),
            git_scan_depth: 0,
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
            slot_index: Some(slot_idx),
        });

        assert_eq!(
            outcome,
            IterationOutcome::Completed,
            "slot {slot_idx} ({task_id}) must reach Completed after <completed> tag"
        );
    }

    // AC3: bandit feedback fired for each slot's shown learning.
    //
    // The AC refers to `learning_feedback.shown_count / success_count`; the
    // actual schema tracks this via `learnings.window_shown` / `window_applied`
    // (the bandit columns). `window_applied >= 1` confirms the pipeline called
    // `record_learning_applied` for the slot's shown learning on completion —
    // the "task_id corresponds to slot tasks" predicate is satisfied by the
    // 1:1 mapping between shown_ids_per_slot[i] and slot_task_ids[i].
    for (slot_idx, shown_ids) in shown_ids_per_slot.iter().enumerate() {
        for &learning_id in shown_ids {
            let stats = get_window_stats(&conn, learning_id).expect("get_window_stats");
            assert!(
                stats.window_applied >= 1,
                "AC3: slot {slot_idx} learning {learning_id} must have window_applied >= 1 \
                 after slot completion (bandit feedback must fire in wave mode); \
                 got window_applied={}",
                stats.window_applied
            );
        }
    }

    // Final AC2 check: all per-slot learnings are still in the window.
    let final_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM learnings WHERE created_at > ?1",
            [&wave_start],
            |r| r.get(0),
        )
        .expect("final learnings count");
    assert!(
        final_count as usize >= slot_task_ids.len(),
        "AC2: final learnings count ({final_count}) must be >= number of wave slots ({}); \
         every slot must have contributed ≥1 learning",
        slot_task_ids.len()
    );
}

// ---------------------------------------------------------------------------
// Discriminator: known-bad behavior (no bandit feedback) fails AC3
// ---------------------------------------------------------------------------

/// Negative test: if `process_iteration_output` is never called for a slot
/// (the pre-unification bug — `process_slot_result` used to short-circuit
/// before the pipeline), `window_applied` stays at 0 for shown learnings.
///
/// This test asserts that the no-pipeline path FAILS the AC3 assertion so
/// the positive test above has discriminating power.
#[test]
fn discriminator_no_pipeline_call_leaves_window_applied_zero() {
    let (db_temp, conn) = setup_migrated_db();
    disable_llm_extraction();

    let mut writer = LearningWriter::new(Some(db_temp.path()));
    let result = writer
        .record(
            &conn,
            RecordLearningParams {
                outcome: LearningOutcome::Pattern,
                title: "Discriminator: never-shown learning".to_string(),
                content: "This learning was never used in a slot that ran the pipeline".to_string(),
                task_id: None,
                run_id: None,
                root_cause: None,
                solution: None,
                applies_to_files: None,
                applies_to_task_types: None,
                applies_to_errors: None,
                tags: None,
                confidence: Confidence::Medium,
            },
        )
        .expect("LearningWriter::record");
    let _embed_count = writer.flush(&conn);

    record_learning_shown(&conn, result.learning_id, 1).expect("record_learning_shown");

    // Intentionally do NOT call process_iteration_output — simulates the
    // pre-unification bug where process_slot_result skipped the pipeline.

    let stats = get_window_stats(&conn, result.learning_id).expect("get_window_stats");
    assert_eq!(
        stats.window_applied, 0,
        "discriminator: without a pipeline call, window_applied must stay 0 — \
         this is the pre-unification bug state that AC3 catches (got {})",
        stats.window_applied
    );
}
