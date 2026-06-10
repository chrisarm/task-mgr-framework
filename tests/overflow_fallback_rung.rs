//! Integration tests for the overflow rung 4 — `RecoveryAction::FallbackToProvider`.
//!
//! Coverage:
//!   * Disabled / absent fallback config → byte-identical to today's 4-rung
//!     ladder ending in `Blocked` at the Opus[1M]+high ceiling.
//!   * Enabled + effective_runner Claude → promotes to Grok: writes
//!     `runner_overrides[task] = Grok`, `model_overrides[task] = cfg.model`,
//!     UPDATEs `tasks.model = cfg.model`, status reset to `'todo'`, and a
//!     `FallbackToProvider` JSONL event lands.
//!   * Enabled + effective_runner already Grok → idempotency guard skips
//!     rung 4 and lands on `Blocked` (no re-promote, no second tasks.model
//!     UPDATE).
//!   * `RecoveryAction::FallbackToProvider` serde shape:
//!     `{"action":"fallback_to_provider","provider":"grok","model":"grok-build"}`.

use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::model::{FABLE_MODEL, ONE_M_SUFFIX, OPUS_MODEL, OPUS_MODEL_1M};
use task_mgr::loop_engine::overflow::{OverflowEvent, RecoveryAction};
use task_mgr::loop_engine::project_config::{ModelsConfig, ProjectConfig};
use task_mgr::loop_engine::prompt::PromptResult;
use task_mgr::loop_engine::reactions::post_output::{HandleOverflowParams, handle_overflow};
use task_mgr::loop_engine::runner::RunnerKind;

/// PRD-mandated default Grok model id for the fallback rung — the model the
/// builtin Grok provider ladder maps its single (Standard) rung to.
const GROK_DEFAULT_MODEL: &str = "grok-build";

/// The new Claude overflow ceiling: the 1M-context variant of the frontier
/// (fable) model. The legacy ceiling was `OPUS_MODEL_1M`; the provider-first
/// ladder now climbs haiku → sonnet → opus → fable → fable[1m] before rung 4.
fn fable_1m() -> String {
    format!("{FABLE_MODEL}{ONE_M_SUFFIX}")
}

/// Build a `ProjectConfig` whose provider-first `models` block wires the
/// cross-provider fallbacks the overflow rung-4 pivot reads
/// (`providers.<source>.fallback`). Grok is enabled so a `claude → grok` pivot
/// target resolves; Claude is enabled by the builtin default. A `None` fallback
/// means that provider has no rung-4 pivot.
fn models_with_fallbacks(
    claude_fallback: Option<&str>,
    grok_fallback: Option<&str>,
) -> ProjectConfig {
    let mut models = ModelsConfig::builtin_default();
    if let Some(p) = models.providers.get_mut("grok") {
        p.enabled = true;
        p.fallback = grok_fallback.map(str::to_string);
    }
    if let Some(p) = models.providers.get_mut("claude") {
        p.fallback = claude_fallback.map(str::to_string);
    }
    ProjectConfig {
        models,
        ..ProjectConfig::default()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Minimal in-memory `tasks` schema with a `model` column, plus a seeded
/// in_progress row so `handle_prompt_too_long`'s status UPDATE has a row to
/// flip and the rung-4 `tasks.model` UPDATE has a column to mutate.
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
         VALUES (?1, 'fixture', 'in_progress', '2026-05-04T00:00:00Z', ?2)",
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
        prompt: "TASK SECTION\n\nLEARNINGS SECTION\n\nBASE PROMPT SECTION\n".to_string(),
        task_id: task_id.to_string(),
        task_files: Vec::new(),
        shown_learning_ids: Vec::new(),
        resolved_model: None,
        dropped_sections: Vec::new(),
        task_difficulty: Some("high".to_string()),
        cluster_effort: None,
        provider_hint: None,
        section_sizes: vec![("task", 12), ("learnings", 17), ("base_prompt", 19)],
    }
}

fn read_events(base_dir: &Path) -> Vec<OverflowEvent> {
    let path = base_dir.join("overflow-events.jsonl");
    let raw = std::fs::read_to_string(&path).expect("jsonl exists");
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<OverflowEvent>(l).expect("parse jsonl line"))
        .collect()
}

/// Claude → Grok fallback configured (`providers.claude.fallback = "grok"`),
/// no inverse. Replaces the legacy `fallbackRunner` surface.
fn enabled_fallback_cfg() -> ProjectConfig {
    models_with_fallbacks(Some("grok"), None)
}

// ── AC #3 — Fallback disabled: 4-rung ladder ends in Blocked, byte-identical ──

#[test]
fn fallback_disabled_walks_existing_four_rung_to_blocked() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "DIS-FEAT-001";
    let ceiling = fable_1m();
    let mut conn = make_conn_with_task(task_id, Some(&ceiling));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Default ProjectConfig has no `providers.claude.fallback` — byte-identical
    // to the no-cross-provider-pivot behavior. Snapshot the model column before
    // the call so we can assert it is untouched on the Blocked exit.
    let project_cfg = ProjectConfig::default();
    let model_before = task_model(&conn, task_id);

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(&ceiling),
        prompt_result: &pr,
        iteration: 1,
        run_id: Some("run-disabled"),
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Claude,
        project_config: &project_cfg,
    });

    assert!(
        matches!(action, RecoveryAction::Blocked),
        "fallback absent at fable[1M]+high MUST land on Blocked (ladder exhausted), got {action:?}",
    );
    assert_eq!(
        task_status(&conn, task_id),
        "blocked",
        "Blocked rung must mark the row blocked",
    );
    assert_eq!(
        task_model(&conn, task_id),
        model_before,
        "Blocked rung MUST NOT mutate tasks.model",
    );

    assert!(
        !ctx.model_overrides.contains_key(task_id),
        "Blocked rung MUST NOT write model_overrides",
    );
    assert!(
        !ctx.effort_overrides.contains_key(task_id),
        "Blocked rung MUST NOT write effort_overrides",
    );
    assert!(
        !ctx.runner_overrides.contains_key(task_id),
        "Blocked rung MUST NOT write runner_overrides",
    );

    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].recovery, RecoveryAction::Blocked));
    let raw = std::fs::read_to_string(tmp.path().join("overflow-events.jsonl")).unwrap();
    let v: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(
        v["recovery"]["action"],
        serde_json::Value::String("blocked".into()),
        "raw JSONL action must be 'blocked' on the disabled exit",
    );
}

#[test]
fn fallback_absent_matches_disabled_byte_for_byte() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "ABS-FEAT-001";
    let ceiling = fable_1m();
    let mut conn = make_conn_with_task(task_id, Some(&ceiling));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = ProjectConfig::default();

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(&ceiling),
        prompt_result: &pr,
        iteration: 1,
        run_id: None,
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Claude,
        project_config: &project_cfg,
    });

    assert!(
        matches!(action, RecoveryAction::Blocked),
        "fallback absent must equal fallback disabled — both land on Blocked at the ceiling",
    );
    assert_eq!(task_status(&conn, task_id), "blocked");
    assert!(!ctx.model_overrides.contains_key(task_id));
    assert!(!ctx.effort_overrides.contains_key(task_id));
    assert!(!ctx.runner_overrides.contains_key(task_id));
}

// ── AC #1 — Fallback enabled + Claude → FallbackToProvider + override + UPDATE

#[test]
fn fallback_enabled_claude_at_ceiling_promotes_to_grok() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "PROMO-FEAT-001";
    let ceiling = fable_1m();
    let mut conn = make_conn_with_task(task_id, Some(&ceiling));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = enabled_fallback_cfg();

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(&ceiling),
        prompt_result: &pr,
        iteration: 1,
        run_id: Some("run-promote"),
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Claude,
        project_config: &project_cfg,
    });

    assert!(
        matches!(
            action,
            RecoveryAction::FallbackToProvider { ref provider, ref model }
                if provider == "grok" && model == GROK_DEFAULT_MODEL
        ),
        "rung 4 must fire when providers.claude.fallback=grok AND runner==Claude, got {action:?}",
    );
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Grok),
        "runner_overrides MUST gain a Grok entry for this task",
    );
    assert_eq!(
        ctx.model_overrides.get(task_id).map(String::as_str),
        Some(GROK_DEFAULT_MODEL),
        "model_overrides MUST be set to cfg.model",
    );
    assert_eq!(
        task_model(&conn, task_id).as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "tasks.model UPDATE must run so resolve_task_model picks Grok next iter",
    );
    assert_eq!(
        task_status(&conn, task_id),
        "todo",
        "FallbackToProvider must reset status to 'todo' so the next iteration retries on Grok",
    );

    // overflow_original_task_model captures the pre-fallback model column.
    assert_eq!(
        ctx.overflow_original_task_model.get(task_id),
        Some(&Some(ceiling.clone())),
        "Step 2 capture must snapshot the pre-UPDATE tasks.model value for FR-008 invalidation",
    );

    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].recovery,
        RecoveryAction::FallbackToProvider { ref provider, ref model }
            if provider == "grok" && model == GROK_DEFAULT_MODEL
    ));
}

// ── AC #2 — Fallback enabled + task already on Grok → Blocked, no mutation ────

#[test]
fn fallback_enabled_task_already_on_grok_returns_blocked() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "ALREADY-GROK-001";
    let mut conn = make_conn_with_task(task_id, Some(GROK_DEFAULT_MODEL));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    // Both directions wired so rung 4 WOULD fire for a Grok source — the only
    // thing stopping a re-promote is the `promote_once` idempotency guard.
    let project_cfg = models_with_fallbacks(Some("grok"), Some("claude"));

    // Simulate an earlier iteration that promoted this task to Grok.
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Grok);

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(GROK_DEFAULT_MODEL),
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
        "task already on Grok at the ceiling must land on Blocked, not re-promote, got {action:?}",
    );
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Grok),
        "prior Grok promotion must be preserved (Blocked rung does not clobber runner_overrides)",
    );
    assert_eq!(
        task_model(&conn, task_id).as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "tasks.model must remain at the Grok value — no second UPDATE on the Blocked rung",
    );
    assert_eq!(task_status(&conn, task_id), "blocked");
}

// ── Inverse Grok → Claude rung (config-derived, tier-preserving) ──────────────

/// A `ProjectConfig` wiring the inverse Grok → Claude pivot via
/// `providers.grok.fallback = "claude"`, optionally pairing it with the
/// `providers.claude.fallback = "grok"` direction (to prove the two don't
/// interfere). `grok_to_claude = false` exercises the "no inverse target →
/// Blocked" path.
fn grok_inverse_cfg(grok_to_claude: bool, with_claude_to_grok: bool) -> ProjectConfig {
    models_with_fallbacks(
        with_claude_to_grok.then_some("grok"),
        grok_to_claude.then_some("claude"),
    )
}

/// AC #6 — A Grok task that overflows at the ceiling, with
/// `providers.grok.fallback = "claude"` set, fires rung 4 in the inverse
/// direction. The pivot is TIER-PRESERVING: grok-build sits at the Standard
/// tier, so the Claude target is the Standard-rung model (opus), and the runner
/// flips to Claude.
#[test]
fn grok_primary_overflow_with_claude_fallback_promotes_to_claude() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "GROKP-FEAT-001";
    // Native Grok task: tasks.model is a Grok model, NO prior override.
    let mut conn = make_conn_with_task(task_id, Some(GROK_DEFAULT_MODEL));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = grok_inverse_cfg(true, false);

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(GROK_DEFAULT_MODEL),
        prompt_result: &pr,
        iteration: 1,
        run_id: Some("run-inverse"),
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Grok,
        project_config: &project_cfg,
    });

    assert!(
        matches!(
            action,
            RecoveryAction::FallbackToProvider { ref provider, ref model }
                if provider == "claude" && model == OPUS_MODEL
        ),
        "Grok overflow with grok.fallback=claude MUST pivot to Claude (tier-preserving → opus), got {action:?}",
    );
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Claude),
        "runner_overrides MUST flip to Claude for the inverse promotion",
    );
    assert_eq!(
        ctx.model_overrides.get(task_id).map(String::as_str),
        Some(OPUS_MODEL),
        "model_overrides MUST be set to the tier-preserving Claude model (opus)",
    );
    assert_eq!(
        task_model(&conn, task_id).as_deref(),
        Some(OPUS_MODEL),
        "tasks.model UPDATE must run so resolve_task_model picks Claude next iter",
    );
    assert_eq!(
        task_status(&conn, task_id),
        "todo",
        "inverse FallbackToProvider must reset status to 'todo' for the Claude retry",
    );
    assert_eq!(
        ctx.overflow_original_task_model.get(task_id),
        Some(&Some(GROK_DEFAULT_MODEL.to_string())),
        "Step 2 capture must snapshot the pre-UPDATE Grok tasks.model value",
    );

    let events = read_events(tmp.path());
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].recovery,
        RecoveryAction::FallbackToProvider { ref provider, ref model }
            if provider == "claude" && model == OPUS_MODEL
    ));
    // The runner field reports the runner active when the overflow fired (Grok).
    assert_eq!(events[0].runner.as_deref(), Some("grok"));
}

/// AC #7 — A Grok task that overflows with `providers.grok.fallback` ABSENT
/// skips rung 4 and lands on Blocked — no inverse target, no mutation.
#[test]
fn grok_primary_overflow_without_claude_fallback_blocks() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "GROKP-FEAT-002";
    let mut conn = make_conn_with_task(task_id, Some(GROK_DEFAULT_MODEL));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = grok_inverse_cfg(false, false);

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(GROK_DEFAULT_MODEL),
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
        "Grok-primary overflow without claudeFallbackModel MUST land on Blocked, got {action:?}",
    );
    assert_eq!(task_status(&conn, task_id), "blocked");
    assert!(!ctx.runner_overrides.contains_key(task_id));
    assert!(!ctx.model_overrides.contains_key(task_id));
    assert_eq!(
        task_model(&conn, task_id).as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "Blocked rung MUST NOT mutate tasks.model",
    );
}

/// AC #8 (regression) — With NO inverse (`providers.grok.fallback` unset), the
/// `providers.claude.fallback = "grok"` direction promotes a Claude task to Grok
/// exactly as before; the inverse branch is unreachable.
#[test]
fn claude_to_grok_byte_identical_when_primary_runner_none() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "REGRESS-FEAT-001";
    let ceiling = fable_1m();
    let mut conn = make_conn_with_task(task_id, Some(&ceiling));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    // claude→grok wired, no inverse grok→claude.
    let project_cfg = enabled_fallback_cfg();
    assert!(project_cfg.models.providers["grok"].fallback.is_none());

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(&ceiling),
        prompt_result: &pr,
        iteration: 1,
        run_id: Some("run-regress"),
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Claude,
        project_config: &project_cfg,
    });

    assert!(
        matches!(
            action,
            RecoveryAction::FallbackToProvider { ref provider, ref model }
                if provider == "grok" && model == GROK_DEFAULT_MODEL
        ),
        "Claude→Grok promotion must be unchanged when no inverse is wired, got {action:?}",
    );
    assert_eq!(ctx.runner_overrides.get(task_id), Some(&RunnerKind::Grok));
    assert_eq!(
        task_model(&conn, task_id).as_deref(),
        Some(GROK_DEFAULT_MODEL),
    );
    assert_eq!(task_status(&conn, task_id), "todo");
}

/// AC #9 (idempotency) — A task already promoted Grok→Claude that overflows
/// AGAIN goes to Blocked. The standing Claude override means
/// `was_already_promoted` is true, so rung 4 is skipped and the task does NOT
/// bounce back to Grok even though an enabled Grok fallback is present.
#[test]
fn grok_to_claude_promoted_task_overflows_again_blocks() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "BOUNCE-FEAT-001";
    // Post-promotion state: the task climbed the Claude ladder to the
    // fable[1M]+high ceiling, so rungs 1-3 are exhausted and rung 4 is reached.
    let ceiling = fable_1m();
    let mut conn = make_conn_with_task(task_id, Some(&ceiling));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    // Both directions configured to prove neither re-fires.
    let project_cfg = grok_inverse_cfg(true, true);

    // Simulate the prior Grok→Claude promotion (now at the fable[1M] ceiling).
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Claude);
    ctx.model_overrides
        .insert(task_id.to_string(), ceiling.clone());

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(&ceiling),
        prompt_result: &pr,
        iteration: 1,
        run_id: None,
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: // Post-promotion the task runs on Claude.
        RunnerKind::Claude,
        project_config: &project_cfg,
    });

    assert!(
        matches!(action, RecoveryAction::Blocked),
        "an already-promoted (Grok→Claude) task must Block, not bounce back to Grok, got {action:?}",
    );
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Claude),
        "the standing Claude override must be preserved on the Blocked rung",
    );
    assert_eq!(task_status(&conn, task_id), "blocked");
}

/// AC #9 (idempotency, mirror) — A task already promoted Claude→Grok that
/// overflows AGAIN goes to Blocked even when `primary_runner` is configured —
/// it must not bounce back to Claude via the inverse branch.
#[test]
fn claude_to_grok_promoted_task_with_primary_runner_blocks() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "BOUNCE-FEAT-002";
    let mut conn = make_conn_with_task(task_id, Some(GROK_DEFAULT_MODEL));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = grok_inverse_cfg(true, true);

    // Simulate the prior Claude→Grok promotion.
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Grok);
    ctx.model_overrides
        .insert(task_id.to_string(), GROK_DEFAULT_MODEL.to_string());

    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(GROK_DEFAULT_MODEL),
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
        "an already-promoted (Claude→Grok) task must Block, not bounce back to Claude, got {action:?}",
    );
    assert_eq!(ctx.runner_overrides.get(task_id), Some(&RunnerKind::Grok));
    assert_eq!(task_status(&conn, task_id), "blocked");
}

// ── AC #5 — RecoveryAction::FallbackToProvider serializes correctly ───────────

#[test]
fn fallback_to_provider_serializes_with_snake_case_tag_and_siblings() {
    let v = serde_json::to_value(RecoveryAction::FallbackToProvider {
        provider: "grok".to_string(),
        model: GROK_DEFAULT_MODEL.to_string(),
    })
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "action": "fallback_to_provider",
            "provider": "grok",
            "model": GROK_DEFAULT_MODEL,
        }),
    );
    // Round-trip preserves equality.
    let s = serde_json::to_string(&v).unwrap();
    let back: RecoveryAction = serde_json::from_str(&s).unwrap();
    assert!(matches!(
        back,
        RecoveryAction::FallbackToProvider { ref provider, ref model }
            if provider == "grok" && model == GROK_DEFAULT_MODEL
    ));
}

// ── AC #7 — user_message for FallbackToProvider includes all relevant fields ──

#[test]
fn user_message_fallback_to_provider_exact_string() {
    let msg = RecoveryAction::FallbackToProvider {
        provider: "grok".to_string(),
        model: GROK_DEFAULT_MODEL.to_string(),
    }
    .user_message("MY-TASK-001", Some("high"), Some(OPUS_MODEL_1M));
    assert_eq!(
        msg,
        format!(
            "Prompt is too long for MY-TASK-001 at effort high, model {} — \
             falling back to {} (Claude ladder exhausted)",
            OPUS_MODEL_1M, GROK_DEFAULT_MODEL,
        ),
    );
}

// ── AC #9 — Test file compiles (per learning #1739 / #2139) ───────────────────

#[test]
fn test_file_compiles_marker() {
    assert_eq!(OPUS_MODEL_1M, OPUS_MODEL_1M);
    assert_eq!(GROK_DEFAULT_MODEL, "grok-build");
}
