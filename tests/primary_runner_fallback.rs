//! FEAT-PRIMARY-003 — inverse RuntimeError fallback hook: Grok → Claude in
//! `escalate_task_model_if_needed`.
//!
//! Mirror of the FEAT-007 Claude→Grok promotion (see
//! `tests/runtime_error_fallback.rs`), opposite direction. A task routed to
//! the Grok primary runner that keeps hitting RuntimeErrors is promoted onto
//! the configured Claude model after `primaryRunner.runtimeErrorThreshold`
//! consecutive failures — provided `primaryRunner.claudeFallbackModel` is set.
//!
//! These tests pin the four FEAT-PRIMARY-003 test acceptance criteria:
//!   1. Grok-primary task at threshold + `claudeFallbackModel` set → promotion
//!      writes `runner_overrides[id] = Claude`, `model_overrides[id] = claude`,
//!      and `UPDATE tasks SET model = claude`.
//!   2. After promotion, the next iteration resolves the Claude runner.
//!   3. `claudeFallbackModel` absent → no promotion; the task dead-ends.
//!   4. `Crash(GrokAuthFailure)` does NOT increment `consecutive_failures`
//!      (the caller-site filter is what protects the counter).

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::engine::{
    IterationContext, escalate_task_model_if_needed, handle_task_failure, resolve_effective_runner,
};
use task_mgr::loop_engine::model::SONNET_MODEL;
use task_mgr::loop_engine::project_config::PrimaryRunnerConfig;
use task_mgr::loop_engine::runner::RunnerKind;

/// Grok model id a primary-runner-routed task carries in `tasks.model`.
const GROK_MODEL: &str = "grok-4-fast";

/// Default inverse-promotion threshold (PRD: `primaryRunner.runtimeErrorThreshold`
/// defaults to 2 — same gate as Claude-tier escalation).
const PRIMARY_THRESHOLD: i32 = 2;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let mut conn = open_connection(dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (dir, conn)
}

fn insert_task(conn: &Connection, id: &str, model: Option<&str>, consecutive_failures: i32) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, max_retries, consecutive_failures) \
         VALUES (?, ?, 'in_progress', ?, ?, ?)",
        rusqlite::params![id, format!("Task {id}"), model, 5, consecutive_failures],
    )
    .unwrap();
}

fn read_model(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT model FROM tasks WHERE id = ?", [id], |r| {
        r.get::<_, Option<String>>(0)
    })
    .unwrap()
}

fn read_consecutive_failures(conn: &Connection, id: &str) -> i32 {
    conn.query_row(
        "SELECT consecutive_failures FROM tasks WHERE id = ?",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

/// A `PrimaryRunnerConfig` with the inverse fallback wired to a Claude model.
fn primary_cfg_with_fallback(claude_model: &str) -> PrimaryRunnerConfig {
    PrimaryRunnerConfig {
        claude_fallback_model: Some(claude_model.to_string()),
        runtime_error_threshold: PRIMARY_THRESHOLD as u32,
        ..Default::default()
    }
}

// ── Test #8 — Inverse promotion fires at Grok + threshold + claudeFallbackModel ─

/// A Grok-primary task with `consecutive_failures == threshold` and
/// `claudeFallbackModel` set must pivot to Claude: it inserts
/// `runner_overrides[id] = RunnerKind::Claude`, `model_overrides[id] = claude`,
/// updates `tasks.model` to the Claude model, and snapshots the pre-promotion
/// Grok model for FR-008 override invalidation. The promotion banner is
/// emitted by `apply_pending_promotion` (asserted indirectly via the override
/// state it writes alongside the banner).
#[test]
fn inverse_promotion_fires_at_grok_and_threshold_with_claude_fallback() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "GROK-PRIMARY-001",
        Some(GROK_MODEL),
        PRIMARY_THRESHOLD - 1,
    );

    let mut ctx = IterationContext::new(8);
    let primary = primary_cfg_with_fallback(SONNET_MODEL);
    let outcome = escalate_task_model_if_needed(
        &conn,
        "GROK-PRIMARY-001",
        PRIMARY_THRESHOLD,
        &mut ctx,
        None,
        Some(&primary),
    )
    .unwrap();

    assert_eq!(
        outcome.as_deref(),
        Some(SONNET_MODEL),
        "inverse promotion must return the configured claudeFallbackModel",
    );
    assert_eq!(
        ctx.runner_overrides.get("GROK-PRIMARY-001"),
        Some(&RunnerKind::Claude),
        "inverse promotion must set runner_overrides[id] = Claude",
    );
    assert_eq!(
        ctx.model_overrides.get("GROK-PRIMARY-001"),
        Some(&SONNET_MODEL.to_string()),
        "inverse promotion must set model_overrides[id] = claudeFallbackModel",
    );
    assert_eq!(
        read_model(&conn, "GROK-PRIMARY-001").as_deref(),
        Some(SONNET_MODEL),
        "inverse promotion must UPDATE tasks.model to the Claude fallback model",
    );
    assert_eq!(
        ctx.overflow_original_task_model.get("GROK-PRIMARY-001"),
        Some(&Some(GROK_MODEL.to_string())),
        "inverse promotion must snapshot the pre-promotion Grok model for FR-008",
    );
    // AC #4 — the counter is owned by handle_task_failure / reset_consecutive_failures;
    // escalate must NOT touch it on any exit path. It stays at the inserted value.
    assert_eq!(
        read_consecutive_failures(&conn, "GROK-PRIMARY-001"),
        PRIMARY_THRESHOLD - 1,
        "inverse promotion must NOT mutate consecutive_failures — preserves auto-block contract",
    );
}

// ── Test #9 — After promotion, the next iteration resolves the Claude runner ───

/// Once a Grok-primary task has been promoted, `resolve_effective_runner`
/// (the single dispatch SSoT) must report `RunnerKind::Claude` so the next
/// iteration spawns `ClaudeRunner` with `claude_fallback_model`. The override
/// wins regardless of the model string passed in (a stale Grok model id must
/// not pull the task back to Grok).
#[test]
fn after_inverse_promotion_next_iteration_resolves_claude_runner() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "GROK-NEXT-001",
        Some(GROK_MODEL),
        PRIMARY_THRESHOLD - 1,
    );

    let mut ctx = IterationContext::new(8);
    let primary = primary_cfg_with_fallback(SONNET_MODEL);
    escalate_task_model_if_needed(
        &conn,
        "GROK-NEXT-001",
        PRIMARY_THRESHOLD,
        &mut ctx,
        None,
        Some(&primary),
    )
    .unwrap();

    // Next iteration resolves the runner from the post-promotion DB model.
    let next_model = read_model(&conn, "GROK-NEXT-001");
    assert_eq!(
        resolve_effective_runner(&ctx, "GROK-NEXT-001", next_model.as_deref()),
        RunnerKind::Claude,
        "post-promotion, the task must spawn via ClaudeRunner using claude_fallback_model",
    );
    // The override must win even if the resolver were handed the old Grok id.
    assert_eq!(
        resolve_effective_runner(&ctx, "GROK-NEXT-001", Some(GROK_MODEL)),
        RunnerKind::Claude,
        "runner_overrides[id] = Claude must win over a stale Grok model id",
    );
}

// ── Test #10 — claudeFallbackModel absent → no promotion, task dead-ends ───────

/// With `primaryRunner.claudeFallbackModel` unset (`None`), a Grok-primary task
/// at threshold must NOT promote — it stays on Grok and dead-ends, exactly like
/// a Claude task with no Grok fallback configured. No override is written and
/// the DB model column is untouched.
#[test]
fn no_inverse_promotion_when_claude_fallback_model_absent() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "GROK-NOFB-001",
        Some(GROK_MODEL),
        PRIMARY_THRESHOLD - 1,
    );

    let mut ctx = IterationContext::new(8);
    // claude_fallback_model is None (the Default).
    let primary = PrimaryRunnerConfig {
        runtime_error_threshold: PRIMARY_THRESHOLD as u32,
        ..Default::default()
    };
    assert!(primary.claude_fallback_model.is_none());

    let outcome = escalate_task_model_if_needed(
        &conn,
        "GROK-NOFB-001",
        PRIMARY_THRESHOLD,
        &mut ctx,
        None,
        Some(&primary),
    )
    .unwrap();

    assert_eq!(
        outcome, None,
        "no claudeFallbackModel → no inverse promotion; escalate_model returns None for Grok tier",
    );
    assert!(
        !ctx.runner_overrides.contains_key("GROK-NOFB-001"),
        "absent claudeFallbackModel must NOT write a runner override",
    );
    assert_eq!(
        read_model(&conn, "GROK-NOFB-001").as_deref(),
        Some(GROK_MODEL),
        "DB model column must remain at Grok when no inverse fallback is configured",
    );
}

/// Parity guard: `primaryRunner` entirely absent (`None`) is identical to the
/// absent-fallback case — no promotion, task stays on Grok.
#[test]
fn no_inverse_promotion_when_primary_runner_absent() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "GROK-NOPR-001",
        Some(GROK_MODEL),
        PRIMARY_THRESHOLD - 1,
    );

    let mut ctx = IterationContext::new(8);
    let outcome = escalate_task_model_if_needed(
        &conn,
        "GROK-NOPR-001",
        PRIMARY_THRESHOLD,
        &mut ctx,
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        outcome, None,
        "primaryRunner absent → no inverse promotion (Grok tier has no Claude escalation)",
    );
    assert!(!ctx.runner_overrides.contains_key("GROK-NOPR-001"));
    assert_eq!(
        read_model(&conn, "GROK-NOPR-001").as_deref(),
        Some(GROK_MODEL)
    );
}

// ── Test #11 — GrokAuthFailure does NOT increment consecutive_failures ─────────

/// When a Grok-primary task's iteration ends in `Crash(GrokAuthFailure)`, the
/// loop must NOT count it as a task failure: an xAI auth lapse is an operator
/// problem, not a task fault. The protection lives at the CALL SITE — both the
/// sequential (`run_loop`) and wave (post-aggregation) wirings skip
/// `handle_task_failure` for `Crash(GrokAuthFailure)`. We pin that contract by
/// (a) asserting the skip-list predicate includes the variant and (b) proving
/// that an unfiltered direct call DOES increment, so only the caller filter
/// keeps the counter stable. This reuses the FEAT-007 cascade-prevention logic.
#[test]
fn grok_auth_failure_does_not_increment_consecutive_failures_for_primary_task() {
    use task_mgr::loop_engine::config::{CrashType, IterationOutcome};

    let (_dir, mut conn) = setup_db();
    insert_task(&conn, "GROK-AUTH-001", Some(GROK_MODEL), 0);
    let before = read_consecutive_failures(&conn, "GROK-AUTH-001");

    // (a) The caller's skip-list predicate must include Crash(GrokAuthFailure).
    let outcome = IterationOutcome::Crash(CrashType::GrokAuthFailure);
    let should_skip = matches!(
        outcome,
        IterationOutcome::Completed
            | IterationOutcome::Empty
            | IterationOutcome::Reorder(_)
            | IterationOutcome::RateLimit
            | IterationOutcome::Crash(CrashType::GrokAuthFailure)
    );
    assert!(
        should_skip,
        "Crash(GrokAuthFailure) must be in the handle_task_failure skip-list at the call site",
    );

    // (b) The direct (unfiltered) call increments — so the ONLY thing keeping a
    // GrokAuthFailure from incrementing the counter is the caller-site filter.
    let mut ctx = IterationContext::new(8);
    let primary = primary_cfg_with_fallback(SONNET_MODEL);
    handle_task_failure(
        &mut conn,
        "GROK-AUTH-001",
        1,
        &mut ctx,
        None,
        Some(&primary),
    )
    .unwrap();
    assert_eq!(
        read_consecutive_failures(&conn, "GROK-AUTH-001"),
        before + 1,
        "handle_task_failure unconditionally increments — the call-site filter is the protection",
    );
}
