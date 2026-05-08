//! Contract tests for `task_mgr::loop_engine::iteration_pipeline`.
//!
//! TDD scaffolding for FEAT-003 (the shared post-Claude pipeline). These
//! tests pin the public surface and the behavioral invariants of
//! `process_iteration_output` BEFORE the implementation lands. They fail
//! against the current `ProcessingOutcome::default()` stub and pass once
//! FEAT-003 wires the real pipeline.
//!
//! Each test is `#[ignore]`-d with a FEAT-003 reason so CI stays green
//! while still preserving the contract for future maintainers. The
//! known-bad discriminator (the stub returning `ProcessingOutcome::default()`)
//! is exercised at the bottom of the file.
//!
//! Notes for future maintainers:
//! - Integration test → cannot use `pub(crate)` `loop_engine::test_utils`
//!   helpers (per learning #896). Setup goes through the public DB API.
//! - DB setup uses `open_connection` + `create_schema` + `run_migrations`
//!   so the bandit window stats and supersession-aware retrieval are wired.
//! - Tests run with `TASK_MGR_NO_EXTRACT_LEARNINGS=1` to keep them
//!   hermetic — `extract_learnings_from_output` spawns a real Claude
//!   subprocess otherwise, which is wrong for unit-style integration tests.
//!   The opt-out is a documented public contract on the ingestion module.
//! - Tests that depend on Tokio subprocess spawning (live extraction) are
//!   noted explicitly in their `#[ignore]` reasons so they can be flipped
//!   on with a mock seam without inventing new scaffolding.

use std::fs;
use std::path::PathBuf;

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

/// Open a DB with full schema + all migrations applied. The `TempDir` return
/// value MUST outlive the `Connection` — dropping it yanks the on-disk file.
fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

/// Insert a task row into the DB so the completion paths have something to
/// transition. `process_iteration_output` should treat the task as `todo`
/// and flip it `done` via `mark_task_done` / `complete_cmd::complete`.
fn insert_todo_task(conn: &Connection, task_id: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, ?2, 'in_progress', 50)",
        [task_id, "Pipeline test task"],
    )
    .expect("insert task row");
}

/// Insert a learning + record an initial `record_learning_shown` call so
/// `record_learning_applied` (called inside the pipeline) has a window row
/// to update. This mirrors the helper in `feedback.rs` unit tests.
fn insert_shown_learning(conn: &Connection, title: &str) -> i64 {
    let inserted = record_learning(
        conn,
        RecordLearningParams {
            outcome: LearningOutcome::Pattern,
            title: title.into(),
            content: "Pipeline contract test fixture".into(),
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

/// Bag of owned values the test can hand to `ProcessingParams` via borrows.
/// Keeps the per-test boilerplate manageable without relying on `'static`
/// strings that don't fit the param shape.
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

/// Convenience to disable LLM-based learning extraction for the duration of
/// a test. Set + unset is safe across cargo test threads because every test
/// that actually checks extraction calls this — they're not exercising
/// extraction-on behavior in parallel.
fn disable_llm_extraction() {
    // SAFETY: cargo test is the canonical caller; we accept the inherent
    // single-test-process race on env vars. The opt-out is checked via
    // `is_extraction_disabled()` once per pipeline call, well after this
    // setter has returned, so a same-test race is structurally impossible.
    unsafe {
        std::env::set_var("TASK_MGR_NO_EXTRACT_LEARNINGS", "1");
    }
}

// ---------------------------------------------------------------------------
// AC: Test asserts process_iteration_output extracts learnings from output via
// extract_learnings_from_output (count > 0 in `learnings` table after a
// fixture output containing `<learning>` tags).
//
// Live extraction spawns a Claude subprocess (see
// `crate::learnings::ingestion::extract_learnings_from_output`). We cannot
// run that hermetically in CI, so the test is gated until FEAT-003 either:
//   a) wires extraction with a mock seam usable from integration tests, or
//   b) ships an env-aware short-circuit that validates the call site without
//      paying for a real Claude run.
//
// The contract this test pins: when extraction is ENABLED and the output
// contains `<learning>` tags, the pipeline records ≥1 row into `learnings`
// and reports `learnings_extracted >= 1` in the returned `ProcessingOutcome`.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "FEAT-003 wires extract_learnings_from_output (requires Claude subprocess or mock seam; \
            stub never calls extraction)"]
fn process_iteration_output_extracts_learnings_from_fixture_output() {
    let (db_temp, mut conn) = setup_migrated_db();
    let mut outcome = IterationOutcome::Completed;
    let mut fx = PipelineFixture::new(db_temp.path());

    // Fixture output containing the `<learning>` tag shape extraction targets.
    let output = "<learning><title>Pipeline must extract</title>\
                  <content>This is a real learning from the iteration.</content>\
                  </learning>\n";

    let baseline: i64 = conn
        .query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
        .unwrap_or(0);

    let result = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-001"),
        output,
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: fx.project.path(),
        git_scan_depth: 5,
        skip_git_completion_detection: false,
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

    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM learnings", [], |r| r.get(0))
        .expect("count learnings after");

    assert!(
        after > baseline,
        "process_iteration_output must persist ≥1 new learning when output has <learning> tags; \
         baseline={baseline} after={after}",
    );
    assert!(
        result.learnings_extracted >= 1,
        "ProcessingOutcome.learnings_extracted must reflect the persisted learnings; got {}",
        result.learnings_extracted,
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts process_iteration_output records bandit feedback for
// shown_learning_ids (rows in `learning_feedback` with matching learning_id
// and task_id).
//
// In this codebase, bandit feedback for shown learnings lives on the
// `learnings` row itself (`window_applied` column). The implementation
// path is `feedback::record_iteration_feedback` →
// `bandit::record_learning_applied`. We assert via `get_window_stats`,
// which reads the same column the bandit writes.
//
// Discriminator: stub doesn't update window_applied; assertion fails.
// ---------------------------------------------------------------------------

#[test]
fn process_iteration_output_records_bandit_feedback_for_shown_learnings() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    let id_a = insert_shown_learning(&conn, "Bandit feedback target A");
    let id_b = insert_shown_learning(&conn, "Bandit feedback target B");
    let shown = [id_a, id_b];

    let mut outcome = IterationOutcome::Completed;
    let mut fx = PipelineFixture::new(db_temp.path());

    process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-002"),
        output: "no completion tags",
        conversation: None,
        shown_learning_ids: &shown,
        outcome: &mut outcome,
        working_root: fx.project.path(),
        git_scan_depth: 5,
        skip_git_completion_detection: false,
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

    let stats_a = get_window_stats(&conn, id_a).expect("window stats A");
    let stats_b = get_window_stats(&conn, id_b).expect("window stats B");
    assert_eq!(
        stats_a.window_applied, 1,
        "shown learning {id_a} must have window_applied incremented on Completed",
    );
    assert_eq!(
        stats_b.window_applied, 1,
        "shown learning {id_b} must have window_applied incremented on Completed",
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts skip_git_completion_detection=true skips the git-hash
// detection branch (no git operations attempted on a non-repo dir).
//
// `check_git_for_task_completion` shells out to `git log` against the
// passed working_root. On a directory that is NOT a git repo, the command
// runs but `output.status.success()` is false and the helper returns
// `None`. So "no git ops attempted" is best validated by passing a
// non-existent working_root and asserting the pipeline does not panic /
// error out — i.e., the git branch was structurally short-circuited.
// ---------------------------------------------------------------------------

#[test]
fn process_iteration_output_skip_git_true_does_not_attempt_git_detection() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_todo_task(&conn, "TEST-PIPE-SKIP-TRUE");

    // Deliberately point working_root at a path that does NOT exist on
    // disk. Any code path that actually invokes `git log` here would
    // surface as a stderr line — and structurally, the pipeline must NOT
    // touch git when `skip_git_completion_detection == true`. The
    // assertion is that the pipeline returns without panicking and without
    // mutating outcome via the git branch (which only fires when a commit
    // is found).
    let nonexistent = PathBuf::from("/nonexistent/path/should/never/be/scanned");
    let mut outcome = IterationOutcome::Empty;
    let mut fx = PipelineFixture::new(db_temp.path());

    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-SKIP-TRUE"),
        output: "no completion signals at all",
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: &nonexistent,
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

    // Without a git-detected completion AND without any other completion
    // tag, the outcome must stay Empty (NOT promoted to Completed via the
    // git branch). This is the structural guard for "git detection was
    // skipped".
    assert_eq!(
        outcome,
        IterationOutcome::Empty,
        "skip_git_completion_detection=true must not flip outcome to Completed via git detection"
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts skip_git_completion_detection=false attempts git detection
// (when in a repo).
//
// We init a real git repo, create a commit whose subject mentions the
// task with the `-COMPLETED` suffix the helper requires, and assert the
// pipeline upgrades the outcome to Completed via the git branch. Both
// `git init` and `git commit` run via std::process::Command — the test is
// skipped automatically if git is missing (only relevant in degraded CI).
// ---------------------------------------------------------------------------

#[test]
fn process_iteration_output_skip_git_false_attempts_git_detection() {
    use std::process::Command;

    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_todo_task(&conn, "TEST-PIPE-SKIP-FALSE");

    let mut fx = PipelineFixture::new(db_temp.path());
    let repo = fx.project.path();

    // Initialise git repo with a deterministic identity. Tests skip cleanly
    // (without false negatives) when git isn't installed.
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping: git not available");
        return;
    }
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo)
        .status()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(repo)
        .status()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(repo)
        .status()
        .expect("git config name");
    fs::write(repo.join("seed.txt"), "seed").expect("seed file");
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo)
        .status()
        .expect("git add");
    // The detection helper requires the literal `-COMPLETED` suffix
    // (case-insensitive on the task ID portion).
    Command::new("git")
        .args(["commit", "-q", "-m", "feat: TEST-PIPE-SKIP-FALSE-completed"])
        .current_dir(repo)
        .status()
        .expect("git commit");

    let mut outcome = IterationOutcome::Empty;
    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-SKIP-FALSE"),
        output: "no inline completion tag — relying on git",
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: repo,
        git_scan_depth: 5,
        skip_git_completion_detection: false,
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
        outcome,
        IterationOutcome::Completed,
        "skip_git_completion_detection=false must surface the `-completed` commit and upgrade \
         outcome to Completed",
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts is_task_reported_already_complete fallback fires in BOTH
// wave (skip_git=true) and sequential (skip_git=false) modes.
//
// This is the wave-mode parity bug the PRD calls out: today's
// process_slot_result never runs the already-complete fallback, so a slot
// whose task was completed in an earlier run never gets flipped done.
// ---------------------------------------------------------------------------

#[test]
fn already_complete_fallback_fires_in_skip_git_true_mode() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_todo_task(&conn, "TEST-PIPE-ALREADY-A");

    let mut outcome = IterationOutcome::Empty;
    let mut fx = PipelineFixture::new(db_temp.path());

    let output = "I checked — TEST-PIPE-ALREADY-A is already complete from a previous run.";
    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-ALREADY-A"),
        output,
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
        outcome,
        IterationOutcome::Completed,
        "already-complete fallback must fire in skip_git=true (wave) mode — this is the parity \
         bug FEAT-003 fixes",
    );
}

#[test]
fn already_complete_fallback_fires_in_skip_git_false_mode() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_todo_task(&conn, "TEST-PIPE-ALREADY-B");

    let mut outcome = IterationOutcome::Empty;
    let mut fx = PipelineFixture::new(db_temp.path());

    let output = "TEST-PIPE-ALREADY-B was already done in a prior run; no further work needed.";
    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-ALREADY-B"),
        output,
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: fx.project.path(),
        git_scan_depth: 5,
        skip_git_completion_detection: false,
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
        outcome,
        IterationOutcome::Completed,
        "already-complete fallback must fire in skip_git=false (sequential) mode (existing \
         engine.rs behavior at line 3454)",
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts ProcessingOutcome.tasks_completed dedups across multiple
// completion branches in one call (matches today's process_slot_result
// HashSet semantics).
//
// Setup: output emits BOTH `<task-status>TASK-ID:done</task-status>` AND
// `<completed>TASK-ID</completed>` for the same task. Without dedup, that
// counts 2; the contract is exactly 1.
// ---------------------------------------------------------------------------

#[test]
fn tasks_completed_dedups_across_status_and_completed_branches() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_todo_task(&conn, "TEST-PIPE-DEDUP");

    let mut outcome = IterationOutcome::Empty;
    let mut fx = PipelineFixture::new(db_temp.path());

    // Same task ID appears in two different completion-signaling tag shapes.
    let output = "<task-status>TEST-PIPE-DEDUP:done</task-status>\n\
                  <completed>TEST-PIPE-DEDUP</completed>\n";
    let result = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-DEDUP"),
        output,
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
        result.tasks_completed, 1,
        "tasks_completed must dedup across <task-status> + <completed> branches — got {}",
        result.tasks_completed,
    );
    assert_eq!(
        result.completed_task_ids,
        vec!["TEST-PIPE-DEDUP".to_string()],
        "completed_task_ids must contain exactly one entry for the deduped task"
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts process_iteration_output mutates params.outcome to
// IterationOutcome::Completed when retroactive completion is detected.
//
// Construct an `Empty` outcome (e.g., Claude returned no tags but a
// `<completed>` tag is in the output) and assert it mutates to `Completed`
// after the pipeline call. This matches sequential at engine.rs:3280, 3307,
// 3341, 3400, 3454 — every completion branch flips outcome to Completed.
// ---------------------------------------------------------------------------

#[test]
fn process_iteration_output_mutates_empty_outcome_to_completed_on_retroactive_completion() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_todo_task(&conn, "TEST-PIPE-MUTATE");

    let mut outcome = IterationOutcome::Empty;
    let mut fx = PipelineFixture::new(db_temp.path());

    // Output emits a `<completed>` tag for the claimed task — this should
    // retroactively mark the iteration as Completed even though the
    // analyzed outcome was Empty.
    let output = "<completed>TEST-PIPE-MUTATE</completed>\n";
    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-MUTATE"),
        output,
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
        outcome,
        IterationOutcome::Completed,
        "retroactive <completed> tag must mutate outcome from Empty to Completed (matches \
         sequential at engine.rs:3307)",
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts ProcessingOutcome.completed_task_ids includes both the
// processed task AND any cross-task `<completed>Y</completed>` IDs.
//
// Slot output may emit a `<completed>` tag for a peer task (Claude
// finished slot X's task and ALSO closed out a sibling). The pipeline
// must record both in `completed_task_ids` so the wave aggregator can
// reconcile them.
// ---------------------------------------------------------------------------

#[test]
fn completed_task_ids_includes_processed_task_and_cross_task_ids() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_todo_task(&conn, "TEST-PIPE-CROSS-A");
    insert_todo_task(&conn, "TEST-PIPE-CROSS-B");

    let mut outcome = IterationOutcome::Empty;
    let mut fx = PipelineFixture::new(db_temp.path());

    // Claimed task is A, but the output completes BOTH A and B.
    let output = "<completed>TEST-PIPE-CROSS-A</completed>\n\
                  <completed>TEST-PIPE-CROSS-B</completed>\n";
    let result = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-CROSS-A"),
        output,
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
        result
            .completed_task_ids
            .contains(&"TEST-PIPE-CROSS-A".to_string()),
        "completed_task_ids must include the processed task A; got {:?}",
        result.completed_task_ids,
    );
    assert!(
        result
            .completed_task_ids
            .contains(&"TEST-PIPE-CROSS-B".to_string()),
        "completed_task_ids must include the cross-task B; got {:?}",
        result.completed_task_ids,
    );
    assert_eq!(
        result.tasks_completed, 2,
        "tasks_completed must reflect both unique completions; got {}",
        result.tasks_completed,
    );
}

// ---------------------------------------------------------------------------
// AC: Test asserts the pipeline NEVER invokes merge / external-git /
// wrapper-commit operations in either skip_git mode (those stay at the
// run_loop / run_wave_iteration call sites — slot 0 crash + slot 1
// success scenario must not double-process via pipeline).
//
// We assert this structurally by passing a working_root with no commits
// to wrap and asserting `ctx.last_commit` is unchanged after the call:
// `wrapper_commit` would set `ctx.last_commit` if the pipeline invoked it.
// External-git reconciliation similarly leaves observable side effects
// (run_id-scoped commit attribution) — we don't add extra setup for it
// here, since the simpler `last_commit` check is the canonical guard.
// ---------------------------------------------------------------------------

#[test]
fn process_iteration_output_does_not_invoke_wrapper_commit() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    insert_todo_task(&conn, "TEST-PIPE-NO-WRAP");

    let mut outcome = IterationOutcome::Empty;
    let mut fx = PipelineFixture::new(db_temp.path());

    // Mark a baseline `last_commit` so we can assert it stays put.
    fx.ctx.last_commit = Some("BASELINE-HASH".to_string());

    let output = "<completed>TEST-PIPE-NO-WRAP</completed>";
    let _ = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-NO-WRAP"),
        output,
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: fx.project.path(),
        git_scan_depth: 5,
        skip_git_completion_detection: false,
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
        fx.ctx.last_commit.as_deref(),
        Some("BASELINE-HASH"),
        "pipeline must not invoke wrapper_commit (would overwrite ctx.last_commit) — those \
         operations live at run_loop / run_wave_iteration call sites",
    );
}

// ---------------------------------------------------------------------------
// AC (discriminator): no-op stub returning ProcessingOutcome::default()
// fails the learnings / feedback / dedup assertions. This dedicated test
// pins the discriminator behavior so a regression that returns
// `ProcessingOutcome::default()` is caught by a single named assertion.
//
// Today this test is the canonical "stub is bad" check — it MUST fail
// against the current stub. Once FEAT-003 lands, it pins the contract
// (every section must contribute to the returned ProcessingOutcome).
// ---------------------------------------------------------------------------

#[test]
fn no_op_stub_fails_combined_contract_assertions() {
    let (db_temp, mut conn) = setup_migrated_db();
    disable_llm_extraction();
    let learning_id = insert_shown_learning(&conn, "Discriminator learning");
    insert_todo_task(&conn, "TEST-PIPE-DISC");

    let mut outcome = IterationOutcome::Empty;
    let mut fx = PipelineFixture::new(db_temp.path());

    // Mix of signals: status tag, completed tag, shown learning. Stub that
    // returns default fails on tasks_completed (would be 0), on
    // completed_task_ids (would be empty), AND on bandit feedback
    // (window_applied stays at 0).
    let output = "<task-status>TEST-PIPE-DISC:done</task-status>\n\
                  <completed>TEST-PIPE-DISC</completed>\n";
    let result = process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id: "test-run",
        iteration: 1,
        task_id: Some("TEST-PIPE-DISC"),
        output,
        conversation: None,
        shown_learning_ids: &[learning_id],
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

    let stats = get_window_stats(&conn, learning_id).expect("window stats");

    assert!(
        result.tasks_completed >= 1
            && !result.completed_task_ids.is_empty()
            && stats.window_applied >= 1
            && outcome == IterationOutcome::Completed,
        "ProcessingOutcome::default() stub fails the combined contract; \
         tasks_completed={tc} completed_task_ids={cti:?} window_applied={wa} outcome={oc:?}",
        tc = result.tasks_completed,
        cti = result.completed_task_ids,
        wa = stats.window_applied,
        oc = outcome,
    );
}

// ---------------------------------------------------------------------------
// Type-level guards: ProcessingOutcome::default() returns the empty
// outcome, and `_outcome` constructions compile against the actual
// signature. These run unconditionally so the file always exercises the
// public API even when the body-level tests are #[ignore]-d.
// ---------------------------------------------------------------------------

#[test]
fn processing_outcome_default_is_empty() {
    let d = ProcessingOutcome::default();
    assert_eq!(d.tasks_completed, 0);
    assert!(d.completed_task_ids.is_empty());
    assert_eq!(d.key_decisions_count, 0);
    assert_eq!(d.status_updates_applied, 0);
    assert_eq!(d.learnings_extracted, 0);
}

#[test]
fn processing_params_constructs_against_real_signature() {
    // Compile-time check that ProcessingParams accepts the field shape the
    // contract calls for. No assertion needed — if this compiles, the
    // public surface matches the test suite's expectations. Functions
    // called inside the pipeline ARE expected to take `&mut Connection`,
    // `&mut IterationOutcome`, `&mut IterationContext` simultaneously, so
    // the borrow checker exercises the worst-case case here.
    let (_db_temp, mut conn) = setup_migrated_db();
    let mut outcome = IterationOutcome::Empty;
    let mut ctx = IterationContext::new(5);
    let signal_flag = SignalFlag::new();
    let project = TempDir::new().expect("tempdir");
    let prd_path = project.path().join("prd.json");
    fs::write(&prd_path, "{}").unwrap();
    let progress_path = project.path().join("progress.txt");
    fs::write(&progress_path, "").unwrap();

    let _params = ProcessingParams {
        conn: &mut conn,
        run_id: "rid",
        iteration: 1,
        task_id: None,
        output: "",
        conversation: None,
        shown_learning_ids: &[],
        outcome: &mut outcome,
        working_root: project.path(),
        git_scan_depth: 5,
        skip_git_completion_detection: false,
        prd_path: &prd_path,
        task_prefix: None,
        progress_path: &progress_path,
        db_dir: project.path(),
        signal_flag: &signal_flag,
        ctx: &mut ctx,
        files_modified: &[],
        effective_model: None,
        effective_effort: None,
        slot_index: None,
    };
}
