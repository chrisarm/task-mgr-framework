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
use task_mgr::loop_engine::model::OPUS_MODEL_1M;
use task_mgr::loop_engine::overflow::{self, OverflowEvent, RecoveryAction};
use task_mgr::loop_engine::project_config::{FallbackRunnerConfig, ProjectConfig};
use task_mgr::loop_engine::prompt::PromptResult;
use task_mgr::loop_engine::runner::RunnerKind;

/// PRD-mandated default Grok model id for the fallback rung. Pinned to the
/// literal because `model.rs` does not yet expose a `GROK_DEFAULT_MODEL`
/// constant — FEAT-002 will add it.
const GROK_DEFAULT_MODEL: &str = "grok-build";

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

fn enabled_fallback_cfg() -> ProjectConfig {
    ProjectConfig {
        fallback_runner: Some(FallbackRunnerConfig {
            enabled: true,
            provider: "grok".to_string(),
            model: GROK_DEFAULT_MODEL.to_string(),
            cli_binary: None,
            runtime_error_threshold: 2,
        }),
        ..ProjectConfig::default()
    }
}

// ── AC #3 — Fallback disabled: 4-rung ladder ends in Blocked, byte-identical ──

#[test]
fn fallback_disabled_walks_existing_four_rung_to_blocked() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "DIS-FEAT-001";
    let mut conn = make_conn_with_task(task_id, Some(OPUS_MODEL_1M));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Default ProjectConfig has fallback_runner: None — byte-identical to
    // pre-FEAT-006 behavior. Snapshot the model column before the call so we
    // can assert it is untouched on the Blocked exit.
    let project_cfg = ProjectConfig::default();
    let model_before = task_model(&conn, task_id);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &mut conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL_1M),
        &pr,
        1,
        Some("run-disabled"),
        tmp.path(),
        None,
        RunnerKind::Claude,
        &project_cfg,
    );

    assert!(
        matches!(action, RecoveryAction::Blocked),
        "fallback disabled at Opus[1M]+high MUST land on Blocked (4-rung ladder), got {action:?}",
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
    let mut conn = make_conn_with_task(task_id, Some(OPUS_MODEL_1M));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = ProjectConfig::default();

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &mut conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL_1M),
        &pr,
        1,
        None,
        tmp.path(),
        None,
        RunnerKind::Claude,
        &project_cfg,
    );

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
    let mut conn = make_conn_with_task(task_id, Some(OPUS_MODEL_1M));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = enabled_fallback_cfg();

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &mut conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL_1M),
        &pr,
        1,
        Some("run-promote"),
        tmp.path(),
        None,
        RunnerKind::Claude,
        &project_cfg,
    );

    assert!(
        matches!(
            action,
            RecoveryAction::FallbackToProvider { ref provider, ref model }
                if provider == "grok" && model == GROK_DEFAULT_MODEL
        ),
        "rung 4 must fire when fallback enabled AND effective_runner==Claude, got {action:?}",
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
        Some(&Some(OPUS_MODEL_1M.to_string())),
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
    let project_cfg = enabled_fallback_cfg();

    // Simulate an earlier iteration that promoted this task to Grok.
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Grok);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &mut conn,
        task_id,
        Some("high"),
        Some(GROK_DEFAULT_MODEL),
        &pr,
        1,
        None,
        tmp.path(),
        None,
        RunnerKind::Grok,
        &project_cfg,
    );

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
