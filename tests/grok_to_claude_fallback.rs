//! FEAT-PRIMARY-004 + FEAT-PRIMARY-005 — inverse Grok→Claude fallback via the
//! overflow `PromptTooLong` rung 4.
//!
//! These tests cover the FEAT-PRIMARY-004 path: a task already running on Grok
//! (routed there via `primaryRunner`) that hits a prompt-too-long ceiling. The
//! ladder walks rungs 1–3 as usual, then rung 4 (`FallbackToProvider`) pivots
//! the task back to a Claude model configured in
//! `primaryRunner.claudeFallbackModel`.
//!
//! Mirror of `tests/overflow_fallback_rung.rs` (Claude→Grok direction).
//!
//! Coverage (4 scenarios):
//!   1. Grok task + PromptTooLong at ceiling + `claudeFallbackModel` set →
//!      `FallbackToProvider{provider:"claude", model:...}` + overrides + UPDATE
//!   2. Grok task + `claudeFallbackModel` absent → ladder ends in `Blocked`
//!   3. Idempotency: Grok task already carrying a Claude promotion override →
//!      rung 4 skipped → `Blocked`
//!   4. After inverse PromptTooLong fallback, `resolve_effective_runner` returns
//!      `RunnerKind::Claude` confirming the next iteration will use Claude

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::loop_engine::engine::{
    EffectiveRunnerInput, IterationContext, resolve_effective_runner,
};
use task_mgr::loop_engine::model::{OPUS_MODEL, OPUS_MODEL_1M, SONNET_MODEL};
use task_mgr::loop_engine::overflow::RecoveryAction;
use task_mgr::loop_engine::project_config::{ModelsConfig, ProjectConfig};
use task_mgr::loop_engine::prompt::PromptResult;
use task_mgr::loop_engine::reactions::post_output::{HandleOverflowParams, handle_overflow};
use task_mgr::loop_engine::runner::RunnerKind;

/// Grok model a task carries when it was promoted by `primaryRunner`.
const GROK_MODEL: &str = "grok-build";

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Minimal in-memory `tasks` schema sufficient for overflow rung-4 DB writes.
fn make_conn_with_task(task_id: &str, model: Option<&str>) -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute(
        r#"CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'todo',
            started_at TEXT,
            model TEXT,
            last_error TEXT,
            blocked_at_iteration INTEGER,
            updated_at TEXT
        )"#,
        [],
    )
    .expect("create tasks table");
    conn.execute(
        "INSERT INTO tasks (id, title, status, started_at, model) \
         VALUES (?1, 'fixture', 'in_progress', '2026-05-24T00:00:00Z', ?2)",
        rusqlite::params![task_id, model],
    )
    .expect("seed task row");
    conn
}

fn task_status(conn: &Connection, task_id: &str) -> String {
    conn.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
        row.get::<_, String>(0)
    })
    .expect("status query")
}

fn task_model(conn: &Connection, task_id: &str) -> Option<String> {
    conn.query_row("SELECT model FROM tasks WHERE id = ?1", [task_id], |row| {
        row.get::<_, Option<String>>(0)
    })
    .expect("model query")
}

fn make_prompt_result(task_id: &str) -> PromptResult {
    PromptResult {
        prompt: "TASK SECTION\n\nBASE PROMPT SECTION\n".to_string(),
        task_id: task_id.to_string(),
        task_files: Vec::new(),
        shown_learning_ids: Vec::new(),
        resolved_model: None,
        dropped_sections: Vec::new(),
        task_difficulty: Some("high".to_string()),
        cluster_effort: None,
        provider_hint: None,
        section_sizes: vec![("task", 12), ("base_prompt", 19)],
    }
}

/// A `ProjectConfig` wiring the inverse Grok → Claude overflow pivot via the
/// provider-first `providers.grok.fallback = "claude"` surface. The pivot is
/// tier-preserving, so a Grok-build task (Standard tier) lands on the Claude
/// Standard-rung model (opus) — the target is config-derived, not a literal
/// `claudeFallbackModel`. Grok is enabled so the source provider resolves.
fn project_cfg_with_grok_to_claude_fallback() -> ProjectConfig {
    let mut models = ModelsConfig::builtin_default();
    if let Some(p) = models.providers.get_mut("grok") {
        p.enabled = true;
        p.fallback = Some("claude".to_string());
    }
    ProjectConfig {
        models,
        ..ProjectConfig::default()
    }
}

// ── Scenario 1 — Grok task + PromptTooLong + claudeFallbackModel → Claude ─────

/// At the Opus[1M]+high ceiling (rungs 1-3 exhausted), a Grok-runner task with
/// `primaryRunner.claudeFallbackModel` set MUST fire rung 4 in the INVERSE
/// direction: `FallbackToProvider{provider:"claude", model:<claude_model>}`.
///
/// Verifies:
/// - `runner_overrides[task_id] = Claude`
/// - `model_overrides[task_id] = claude_fallback_model`
/// - `tasks.model` DB column updated to `claude_fallback_model`
/// - task status reset to `"todo"` for the next iteration
/// - `overflow_original_task_model` captures the pre-fallback Grok model
#[test]
fn grok_task_prompt_too_long_at_ceiling_falls_back_to_claude() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "8d71d1f7-REVIEW-001";
    let mut conn = make_conn_with_task(task_id, Some(GROK_MODEL));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = project_cfg_with_grok_to_claude_fallback();

    // Use the Grok model at effort="high" to bypass rungs 1-3 (effort already
    // at floor, Grok's single-rung ladder has no model escalation, no 1M Grok
    // variant). The ladder falls straight to rung 4.
    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: // effort floor reached
        Some(GROK_MODEL),
        prompt_result: &pr,
        iteration: 1,
        run_id: Some("run-inverse"),
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Grok,
        project_config: // effective runner is Grok
        &project_cfg,
    });

    assert!(
        matches!(
            action,
            RecoveryAction::FallbackToProvider { ref provider, ref model }
                if provider == "claude" && model == OPUS_MODEL
        ),
        "rung 4 inverse MUST fire for RunnerKind::Grok + providers.grok.fallback=claude \
         (tier-preserving → opus); got {action:?}",
    );
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Claude),
        "inverse fallback MUST write runner_overrides[task_id] = RunnerKind::Claude",
    );
    assert_eq!(
        ctx.model_overrides.get(task_id).map(String::as_str),
        Some(OPUS_MODEL),
        "inverse fallback MUST write model_overrides[task_id] = the tier-preserving Claude model",
    );
    assert_eq!(
        task_model(&conn, task_id).as_deref(),
        Some(OPUS_MODEL),
        "inverse fallback MUST UPDATE tasks.model to the Claude target so \
         resolve_task_model picks Claude on the next iteration",
    );
    assert_eq!(
        task_status(&conn, task_id),
        "todo",
        "inverse fallback MUST reset status to 'todo' for retry on Claude",
    );
    assert_eq!(
        ctx.overflow_original_task_model.get(task_id),
        Some(&Some(GROK_MODEL.to_string())),
        "pre-fallback Grok model MUST be snapshotted in overflow_original_task_model \
         for FR-008 override-invalidation",
    );
}

// ── Scenario 2 — Grok task + claudeFallbackModel absent → Blocked ─────────────

/// When `primaryRunner.claudeFallbackModel` is `None`, there is no inverse
/// fallback target. Rung 4 has nothing to promote to, so the task lands on
/// `Blocked` exactly as a Claude task without `fallbackRunner` does.
///
/// This prevents infinite Grok-only retries when no inverse Claude fallback is
/// configured.
#[test]
fn grok_task_prompt_too_long_without_claude_fallback_model_returns_blocked() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "8d71d1f7-REVIEW-002";
    let mut conn = make_conn_with_task(task_id, Some(GROK_MODEL));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    // Grok enabled but `providers.grok.fallback` is absent → no inverse target.
    let project_cfg = {
        let mut models = ModelsConfig::builtin_default();
        if let Some(p) = models.providers.get_mut("grok") {
            p.enabled = true;
        }
        ProjectConfig {
            models,
            ..ProjectConfig::default()
        }
    };
    let model_before = task_model(&conn, task_id);

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(GROK_MODEL),
        prompt_result: &pr,
        iteration: 1,
        run_id: None,
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Grok,
        project_config: &project_cfg,
    });

    assert!(
        matches!(action, RecoveryAction::Blocked),
        "absent claudeFallbackModel MUST result in Blocked — no inverse target configured; \
         got {action:?}",
    );
    assert_eq!(
        task_status(&conn, task_id),
        "blocked",
        "Blocked rung must set status = 'blocked'",
    );
    assert_eq!(
        task_model(&conn, task_id),
        model_before,
        "Blocked MUST NOT mutate tasks.model",
    );
    assert!(
        !ctx.runner_overrides.contains_key(task_id),
        "Blocked MUST NOT write runner_overrides",
    );
    assert!(
        !ctx.model_overrides.contains_key(task_id),
        "Blocked MUST NOT write model_overrides",
    );
}

// ── Scenario 3 — idempotency: already-promoted Grok task → Blocked ────────────

/// A Grok task that already carries a Claude promotion override (written by a
/// previous rung-4 or RuntimeError hook) MUST NOT be promoted again. The
/// `was_already_promoted` idempotency guard in `handle_prompt_too_long` fires
/// first, rung 4 is skipped, and the task lands on `Blocked`.
///
/// This prevents a task from bouncing back and forth between providers across
/// iterations within a single loop run.
#[test]
fn grok_task_already_promoted_to_claude_returns_blocked_not_promoted_again() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "8d71d1f7-REVIEW-003";
    let mut conn = make_conn_with_task(task_id, Some(GROK_MODEL));
    let project_cfg = project_cfg_with_grok_to_claude_fallback();
    let pr = make_prompt_result(task_id);

    let mut ctx = IterationContext::new(10);
    // Simulate that the inverse promotion has already fired: the task's
    // runner_overrides entry says "Claude" and the model_overrides says Sonnet.
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Claude);
    ctx.model_overrides
        .insert(task_id.to_string(), SONNET_MODEL.to_string());

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(GROK_MODEL),
        prompt_result: &pr,
        iteration: 2,
        run_id: // second overflow on the same task
        None,
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Grok,
        project_config: &project_cfg,
    });

    assert!(
        matches!(action, RecoveryAction::Blocked),
        "idempotency guard MUST fire: already-promoted task MUST land on Blocked, \
         not receive a second FallbackToProvider; got {action:?}",
    );
    // The runner_overrides entry is NOT touched (still Claude from before).
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Claude),
        "idempotency guard MUST leave runner_overrides[task_id] = Claude unchanged",
    );
    // The model_overrides entry is NOT touched.
    assert_eq!(
        ctx.model_overrides.get(task_id).map(String::as_str),
        Some(SONNET_MODEL),
        "idempotency guard MUST leave model_overrides[task_id] = Sonnet unchanged",
    );
}

// ── Scenario 4 — after inverse fallback, resolve_effective_runner → Claude ────

/// Smoke-tests the complete promotion path: after `handle_prompt_too_long` fires
/// the inverse rung-4 arm, the in-memory override map (`ctx.runner_overrides`)
/// carries `RunnerKind::Claude` for the task. The next iteration's runner
/// selection MUST return `RunnerKind::Claude` regardless of what model string
/// is passed in — the override wins.
///
/// This confirms the three-step contract:
///   overflow rung 4 → writes runner_overrides[task] = Claude
///     → next iter: resolve_effective_runner reads override → Claude
///       → spawns ClaudeRunner with claudeFallbackModel
#[test]
fn after_inverse_overflow_fallback_next_iteration_resolves_claude_runner() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "8d71d1f7-REVIEW-004";
    let mut conn = make_conn_with_task(task_id, Some(GROK_MODEL));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = project_cfg_with_grok_to_claude_fallback();

    // Fire rung 4 — writes runner_overrides + model_overrides + DB UPDATE.
    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(GROK_MODEL),
        prompt_result: &pr,
        iteration: 1,
        run_id: Some("run-next-iter"),
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Grok,
        project_config: &project_cfg,
    });
    assert!(
        matches!(action, RecoveryAction::FallbackToProvider { .. }),
        "pre-condition: rung 4 must fire; got {action:?}",
    );

    // Simulate the start of the next iteration: the DB model has been updated
    // to the tier-preserving Claude target (opus) by the rung-4 DB write.
    let db_model = task_model(&conn, task_id);
    assert_eq!(
        db_model.as_deref(),
        Some(OPUS_MODEL),
        "pre-condition: tasks.model must hold the Claude fallback model post-rung-4",
    );

    // runner_overrides takes precedence over both the db_model and any stale
    // Grok model string we might pass in (guards against a silent drift where
    // the caller re-reads the stale model before the DB write is visible).
    let runner_from_db_model = resolve_effective_runner(
        &ctx,
        task_id,
        EffectiveRunnerInput {
            model: db_model.as_deref(),
            provider_hint: None,
        },
    );
    assert_eq!(
        runner_from_db_model,
        RunnerKind::Claude,
        "resolve_effective_runner MUST return Claude when runner_overrides says Claude, \
         even when effective_model is a Claude model string",
    );

    // Stale Grok model id must not pull the task back to Grok.
    let runner_from_stale_grok = resolve_effective_runner(
        &ctx,
        task_id,
        EffectiveRunnerInput {
            model: Some(GROK_MODEL),
            provider_hint: None,
        },
    );
    assert_eq!(
        runner_from_stale_grok,
        RunnerKind::Claude,
        "runner_overrides[task] = Claude MUST win over a stale Grok model id — \
         the override is the single dispatch SSoT after promotion",
    );

    // Also verify against the 1M-context Claude model if it were somehow in play.
    let runner_from_opus_1m = resolve_effective_runner(
        &ctx,
        task_id,
        EffectiveRunnerInput {
            model: Some(OPUS_MODEL_1M),
            provider_hint: None,
        },
    );
    assert_eq!(
        runner_from_opus_1m,
        RunnerKind::Claude,
        "runner_overrides[task] = Claude MUST win over any Claude model string",
    );
}

// ── Negative: Claude→Grok fallback does NOT trigger on RunnerKind::Grok ────────

/// When the effective runner is already Grok and the `providers.claude.fallback
/// = "grok"` (Claude→Grok direction) is configured but `providers.grok.fallback`
/// (Grok→Claude) is NOT, a Grok-source overflow consults ONLY its own provider's
/// fallback (absent) → Blocked. `select_fallback_target` keys on the SOURCE
/// provider, so the claude→grok direction is irrelevant to a Grok task and a
/// task can never promote itself to its own provider.
#[test]
fn grok_runner_with_fallback_runner_configured_does_not_self_promote_to_grok() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "8d71d1f7-REVIEW-005";
    let mut conn = make_conn_with_task(task_id, Some(GROK_MODEL));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Claude→Grok wired (providers.claude.fallback="grok"); the Grok→Claude
    // inverse (providers.grok.fallback) is NOT set.
    let project_cfg = {
        let mut models = ModelsConfig::builtin_default();
        if let Some(p) = models.providers.get_mut("grok") {
            p.enabled = true;
        }
        if let Some(p) = models.providers.get_mut("claude") {
            p.fallback = Some("grok".to_string());
        }
        ProjectConfig {
            models,
            ..ProjectConfig::default()
        }
    };

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(GROK_MODEL),
        prompt_result: &pr,
        iteration: 1,
        run_id: None,
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Grok,
        project_config: // effective runner is already Grok
        &project_cfg,
    });

    assert!(
        matches!(action, RecoveryAction::Blocked),
        "RunnerKind::Grok with only the claude→grok direction wired MUST land on \
         Blocked — a Grok source consults providers.grok.fallback (absent); \
         got {action:?}",
    );
    assert!(
        !ctx.runner_overrides.contains_key(task_id),
        "no promotion override MUST be written when rung 4 is blocked",
    );
}
