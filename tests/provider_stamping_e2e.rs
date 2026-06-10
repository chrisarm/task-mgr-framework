//! TEST-010 — end-to-end provider stamping (migration v20).
//!
//! The shared post-Claude pipeline (`iteration_pipeline::process_iteration_output`,
//! the single completion home for BOTH the sequential and wave paths) stamps,
//! on the completion arm of the claimed task:
//!   * `tasks.completed_by_provider` — the `Provider::as_str` value
//!     (`"claude"` | `"grok"` | `"codex"`), NEVER a model string.
//!   * `run_tasks.provider` / `run_tasks.model` — the effective (provider, model)
//!     pair for that `(run_id, task_id, iteration)` attempt.
//!
//! These tests drive the REAL pipeline (not the v20 migration's column-existence
//! unit tests) with a `<task-status>…:done</task-status>` completion and an
//! explicit `effective_runner` / `effective_model`, then assert the stamped
//! values via the DB. Coverage spans all three runners plus the negative
//! (non-completing iteration must NOT stamp) and the historical-NULL invariant.
//!
//! Assertions use `assert_eq!` on exact provider/model strings, never
//! `contains()`. Claude model ids come from the `model.rs` constants.

use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::migrations::run_migrations;
use task_mgr::db::{create_schema, open_connection};
use task_mgr::loop_engine::config::IterationOutcome;
use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::iteration_pipeline::{ProcessingParams, process_iteration_output};
use task_mgr::loop_engine::model::OPUS_MODEL;
use task_mgr::loop_engine::runner::RunnerKind;
use task_mgr::loop_engine::signals::SignalFlag;

/// The grok CLI's only model id. Not a Claude id, so the no_hardcoded_models
/// guard (which matches `claude-*` only) does not flag this literal.
const GROK_MODEL: &str = "grok-build";

/// Live extraction spawns a real Claude subprocess; disable it so these tests
/// stay hermetic (documented public opt-out on the ingestion module).
fn disable_llm_extraction() {
    // SAFETY: cargo test is the canonical caller; the opt-out is read once per
    // pipeline call, well after this setter returns.
    unsafe {
        std::env::set_var("TASK_MGR_NO_EXTRACT_LEARNINGS", "1");
    }
}

fn setup_migrated_db() -> (TempDir, Connection) {
    let temp = TempDir::new().expect("tempdir");
    let mut conn = open_connection(temp.path()).expect("open_connection");
    create_schema(&conn).expect("create_schema");
    run_migrations(&mut conn).expect("run_migrations");
    (temp, conn)
}

/// Seed the full (run → task → run_tasks) chain the stamp UPDATEs target:
/// a `runs` row, an `in_progress` `tasks` row, and the matching `run_tasks`
/// attempt row keyed `(run_id, task_id, iteration)`.
fn seed_run_task(conn: &Connection, run_id: &str, task_id: &str, iteration: i64) {
    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES (?1, 'active')",
        [run_id],
    )
    .expect("insert run");
    seed_task_attempt(conn, run_id, task_id, iteration);
}

/// Seed one task + its `run_tasks` attempt row under an existing run — used by
/// the heterogeneous-wave test, where several tasks share a single run.
fn seed_task_attempt(conn: &Connection, run_id: &str, task_id: &str, iteration: i64) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, priority) VALUES (?1, 'stamp fixture', 'in_progress', 50)",
        [task_id],
    )
    .expect("insert task");
    conn.execute(
        "INSERT INTO run_tasks (run_id, task_id, iteration, status) VALUES (?1, ?2, ?3, 'started')",
        rusqlite::params![run_id, task_id, iteration],
    )
    .expect("insert run_task");
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

fn completed_by_provider(conn: &Connection, task_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT completed_by_provider FROM tasks WHERE id = ?1",
        [task_id],
        |r| r.get(0),
    )
    .expect("query completed_by_provider")
}

fn run_task_provider_model(
    conn: &Connection,
    run_id: &str,
    task_id: &str,
    iteration: i64,
) -> (Option<String>, Option<String>) {
    conn.query_row(
        "SELECT provider, model FROM run_tasks WHERE run_id = ?1 AND task_id = ?2 AND iteration = ?3",
        rusqlite::params![run_id, task_id, iteration],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .expect("query run_tasks provider/model")
}

/// Drive the real pipeline for `task_id` with an explicit runner+model and an
/// output that marks the claimed task done. Returns nothing — assertions read
/// the DB afterwards.
#[allow(clippy::too_many_arguments)] // test helper mirroring ProcessingParams' shape
fn run_completion(
    conn: &mut Connection,
    fx: &mut PipelineFixture,
    run_id: &str,
    task_id: &str,
    iteration: u32,
    effective_runner: RunnerKind,
    effective_model: Option<&str>,
    slot_index: Option<usize>,
) {
    let mut outcome = IterationOutcome::Completed;
    let output = format!("<task-status>{task_id}:done</task-status>\n");
    process_iteration_output(ProcessingParams {
        conn,
        run_id,
        iteration,
        task_id: Some(task_id),
        output: &output,
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
        effective_model,
        effective_effort: None,
        effective_runner: Some(effective_runner),
        slot_index,
    });
}

// ════════════════════════════════════════════════════════════════════════════
// AC: Completed tasks carry completed_by_provider; run_tasks rows carry
// provider+model — for every runner kind.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn claude_completion_stamps_provider_and_model() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    let mut fx = PipelineFixture::new(db_temp.path());
    let (run_id, task_id, iter) = ("run-claude", "STAMP-CLAUDE-001", 1);
    seed_run_task(&conn, run_id, task_id, iter);

    run_completion(
        &mut conn,
        &mut fx,
        run_id,
        task_id,
        iter as u32,
        RunnerKind::Claude,
        Some(OPUS_MODEL),
        None,
    );

    assert_eq!(
        completed_by_provider(&conn, task_id).as_deref(),
        Some("claude"),
        "tasks.completed_by_provider must stamp the Provider::as_str value, not a model id",
    );
    assert_eq!(
        run_task_provider_model(&conn, run_id, task_id, iter),
        (Some("claude".to_string()), Some(OPUS_MODEL.to_string())),
        "run_tasks must carry the effective (provider, model) pair",
    );
}

#[test]
fn grok_completion_stamps_provider_and_model() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    let mut fx = PipelineFixture::new(db_temp.path());
    let (run_id, task_id, iter) = ("run-grok", "STAMP-GROK-001", 2);
    seed_run_task(&conn, run_id, task_id, iter);

    run_completion(
        &mut conn,
        &mut fx,
        run_id,
        task_id,
        iter as u32,
        RunnerKind::Grok,
        Some(GROK_MODEL),
        None,
    );

    assert_eq!(
        completed_by_provider(&conn, task_id).as_deref(),
        Some("grok"),
    );
    assert_eq!(
        run_task_provider_model(&conn, run_id, task_id, iter),
        (Some("grok".to_string()), Some(GROK_MODEL.to_string())),
    );
}

/// Codex routes provider-only (no `-m`): the stamp records `provider = "codex"`
/// with a NULL `run_tasks.model` (the effective model is None) — provider
/// identity is preserved even when no model string flows.
#[test]
fn codex_completion_stamps_provider_with_null_model() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    let mut fx = PipelineFixture::new(db_temp.path());
    let (run_id, task_id, iter) = ("run-codex", "STAMP-CODEX-001", 1);
    seed_run_task(&conn, run_id, task_id, iter);

    run_completion(
        &mut conn,
        &mut fx,
        run_id,
        task_id,
        iter as u32,
        RunnerKind::Codex,
        None,
        None,
    );

    assert_eq!(
        completed_by_provider(&conn, task_id).as_deref(),
        Some("codex"),
        "provider identity is stamped even when no model string flows (codex provider-only)",
    );
    assert_eq!(
        run_task_provider_model(&conn, run_id, task_id, iter),
        (Some("codex".to_string()), None),
        "run_tasks.provider is 'codex' with a NULL model for a provider-only codex route",
    );
}

// ════════════════════════════════════════════════════════════════════════════
// PRD success metric (2): a heterogeneous-provider WAVE — three slots in the
// SAME run + iteration complete via three different providers through the
// shared stamping home (the exact shape of the wave call site in
// `slot.rs::process_slot_result`, which passes `slot_index: Some(N)`).
// Each task must carry ITS OWN provider stamp — no cross-slot bleed.
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn heterogeneous_wave_stamps_each_slot_with_its_own_provider() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    let mut fx = PipelineFixture::new(db_temp.path());
    let (run_id, iter) = ("run-hetero-wave", 1_i64);
    conn.execute(
        "INSERT INTO runs (run_id, status) VALUES (?1, 'active')",
        [run_id],
    )
    .expect("insert run");

    let slots: [(&str, RunnerKind, Option<&str>); 3] = [
        ("WAVE-CLAUDE-001", RunnerKind::Claude, Some(OPUS_MODEL)),
        ("WAVE-GROK-001", RunnerKind::Grok, Some(GROK_MODEL)),
        ("WAVE-CODEX-001", RunnerKind::Codex, None),
    ];
    for (task_id, _, _) in &slots {
        seed_task_attempt(&conn, run_id, task_id, iter);
    }
    for (slot_idx, (task_id, runner, model)) in slots.iter().enumerate() {
        run_completion(
            &mut conn,
            &mut fx,
            run_id,
            task_id,
            iter as u32,
            *runner,
            *model,
            Some(slot_idx),
        );
    }

    assert_eq!(
        completed_by_provider(&conn, "WAVE-CLAUDE-001").as_deref(),
        Some("claude"),
    );
    assert_eq!(
        completed_by_provider(&conn, "WAVE-GROK-001").as_deref(),
        Some("grok"),
    );
    assert_eq!(
        completed_by_provider(&conn, "WAVE-CODEX-001").as_deref(),
        Some("codex"),
        "codex slot stamps provider identity even with no model string",
    );
    assert_eq!(
        run_task_provider_model(&conn, run_id, "WAVE-CLAUDE-001", iter),
        (Some("claude".to_string()), Some(OPUS_MODEL.to_string())),
    );
    assert_eq!(
        run_task_provider_model(&conn, run_id, "WAVE-GROK-001", iter),
        (Some("grok".to_string()), Some(GROK_MODEL.to_string())),
    );
    assert_eq!(
        run_task_provider_model(&conn, run_id, "WAVE-CODEX-001", iter),
        (Some("codex".to_string()), None),
        "run_tasks rows must not bleed providers across wave slots",
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Negatives — the stamp is gated on completion AND an effective runner.
// ════════════════════════════════════════════════════════════════════════════

/// A non-completing iteration (no done tag) must NOT stamp either surface — the
/// columns stay NULL so historical / in-flight rows are never misattributed.
#[test]
fn non_completing_iteration_does_not_stamp() {
    disable_llm_extraction();
    let (db_temp, mut conn) = setup_migrated_db();
    let mut fx = PipelineFixture::new(db_temp.path());
    let (run_id, task_id, iter) = ("run-noop", "STAMP-NOOP-001", 1);
    seed_run_task(&conn, run_id, task_id, iter);

    let mut outcome = IterationOutcome::Empty;
    process_iteration_output(ProcessingParams {
        conn: &mut conn,
        run_id,
        iteration: iter as u32,
        task_id: Some(task_id),
        output: "no completion markers in this output\n",
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
        effective_model: Some(OPUS_MODEL),
        effective_effort: None,
        effective_runner: Some(RunnerKind::Claude),
        slot_index: None,
    });

    assert_eq!(
        completed_by_provider(&conn, task_id),
        None,
        "a non-completing iteration must leave completed_by_provider NULL",
    );
    assert_eq!(
        run_task_provider_model(&conn, run_id, task_id, iter),
        (None, None),
        "a non-completing iteration must leave run_tasks provider/model NULL",
    );
}

/// Historical-row invariant (v20 down-migration semantics): the columns default
/// to NULL on a freshly migrated DB until a completion stamps them.
#[test]
fn columns_default_to_null_before_any_stamp() {
    let (_db_temp, conn) = setup_migrated_db();
    seed_run_task(&conn, "run-pre", "STAMP-PRE-001", 1);

    assert_eq!(completed_by_provider(&conn, "STAMP-PRE-001"), None);
    assert_eq!(
        run_task_provider_model(&conn, "run-pre", "STAMP-PRE-001", 1),
        (None, None),
    );
}
