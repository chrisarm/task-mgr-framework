//! TDD scaffolding for US-003 — overflow rung 4 (`FallbackToProvider`).
//!
//! Today the overflow ladder is four rungs:
//!   1. `downgrade_effort`     — xhigh → high.
//!   2. `escalate_below_opus`  — haiku/sonnet → opus.
//!   3. `to_1m_model`          — opus → opus[1m].
//!   4. `Blocked`              — terminal.
//!
//! FEAT-005 inserts a NEW rung BEFORE Blocked: `FallbackToProvider`.
//! Precondition: `fallbackRunner.enabled = true` AND the task's effective
//! runner is still `RunnerKind::Claude`. Effect: switch the task onto Grok by
//! inserting `runner_overrides[task] = Grok` and `model_overrides[task] =
//! cfg.model`, then UPDATE `tasks.model = cfg.model` so subsequent iterations
//! resolve the new model from the DB column (Learning #2031 + PRD §2.5
//! "tasks.model DB column interaction").
//!
//! Tests in this file are split into two cohorts:
//!
//! - **Unconditional** — exercise today's byte-identical behavior when
//!   fallback is disabled / absent. They run on every `cargo test` invocation
//!   and lock in the regression guard ("disabled path must be byte-identical
//!   to today").
//!
//! - **`#[ignore]` until FEAT-005 / FEAT-006** — drive the future signature
//!   of `handle_prompt_too_long` (additional `&FallbackRunnerConfig` /
//!   `RunnerKind effective_runner` params) and the new
//!   `RecoveryAction::FallbackToProvider` variant + `OverflowEvent.runner`
//!   field. Bodies use today's signature so the file compiles; the
//!   implementer who lands FEAT-005 must rewrite the bodies against the new
//!   signature and remove the `#[ignore]`.

use std::path::Path;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::model::OPUS_MODEL_1M;
use task_mgr::loop_engine::overflow::{self, OverflowEvent, RecoveryAction};
use task_mgr::loop_engine::prompt::PromptResult;

/// PRD-mandated default Grok model id for the fallback rung. Pinned to the
/// literal because `model.rs` does not yet expose a `GROK_DEFAULT_MODEL`
/// constant — FEAT-002 will add it. Tests reference the literal directly so
/// the file compiles on `main`.
const GROK_DEFAULT_MODEL: &str = "grok-4-fast";

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Minimal in-memory `tasks` schema with a `model` column, plus a seeded
/// in_progress row so `handle_prompt_too_long`'s status UPDATE has a row to
/// flip and FEAT-005's `tasks.model` UPDATE has a column to mutate.
fn make_conn_with_task(task_id: &str, model: Option<&str>) -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute(
        r#"CREATE TABLE tasks (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'todo',
            started_at TEXT,
            model TEXT
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

// ── AC #3 — Fallback disabled: 4-rung ladder ends in Blocked, byte-identical ──

/// Today's behavior — fallbackRunner config is `enabled = false` (the only
/// state that exists pre-FEAT-005). A task at the Opus[1M] + high ceiling
/// must land on `RecoveryAction::Blocked`, status=`blocked`, with NO
/// `runner_overrides` / `model_overrides` mutation. Locks in the regression
/// guard that the disabled path stays byte-identical to today after FEAT-005
/// lands.
#[test]
fn fallback_disabled_walks_existing_four_rung_to_blocked() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "DIS-FEAT-001";
    let conn = make_conn_with_task(task_id, Some(OPUS_MODEL_1M));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    // Snapshot the model column BEFORE the call so we can assert it is
    // untouched on the Blocked exit (FEAT-005's UPDATE only fires on the new
    // FallbackToProvider rung).
    let model_before = task_model(&conn, task_id);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL_1M),
        &pr,
        1,
        Some("run-disabled"),
        tmp.path(),
        None,
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
        "Blocked rung MUST NOT mutate tasks.model — FEAT-005's UPDATE is gated on the new rung",
    );

    // Override maps are untouched on the Blocked exit (the rungs 1-3 paths
    // are the only writers in today's code, and Blocked is the terminal rung).
    assert!(
        !ctx.model_overrides.contains_key(task_id),
        "Blocked rung MUST NOT write model_overrides",
    );
    assert!(
        !ctx.effort_overrides.contains_key(task_id),
        "Blocked rung MUST NOT write effort_overrides",
    );

    // JSONL must record exactly one event whose recovery.action == "blocked".
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

/// `fallbackRunner` absent (i.e. config block not present in
/// `.task-mgr/config.json`) MUST behave identically to `enabled = false`.
/// Today this is the only path the production code has — the test exists so
/// FEAT-005 preserves it on the `None` arm of `Option<&FallbackRunnerConfig>`.
#[test]
fn fallback_absent_matches_disabled_byte_for_byte() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "ABS-FEAT-001";
    let conn = make_conn_with_task(task_id, Some(OPUS_MODEL_1M));
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);

    let action = overflow::handle_prompt_too_long(
        &mut ctx,
        &conn,
        task_id,
        Some("high"),
        Some(OPUS_MODEL_1M),
        &pr,
        1,
        None,
        tmp.path(),
        None,
    );

    assert!(
        matches!(action, RecoveryAction::Blocked),
        "fallback absent must equal fallback disabled — both land on Blocked at the ceiling",
    );
    assert_eq!(task_status(&conn, task_id), "blocked");
    assert!(!ctx.model_overrides.contains_key(task_id));
    assert!(!ctx.effort_overrides.contains_key(task_id));
}

// ── AC #1 — Fallback enabled + Claude → FallbackToProvider + override + UPDATE

/// FEAT-005: at the Opus[1M] + high ceiling with `fallbackRunner.enabled = true`
/// AND the task's effective runner still `RunnerKind::Claude`, the helper
/// must pick the new `RecoveryAction::FallbackToProvider` rung BEFORE
/// Blocked, write `runner_overrides[task] = RunnerKind::Grok` and
/// `model_overrides[task] = cfg.model`, AND execute the
/// `UPDATE tasks SET model = ?1 WHERE id = ?2` SQL so `resolve_task_model`
/// on the next iteration picks the Grok model from the DB column (Learning
/// #2031: order is ctx → DB → stderr → dump → JSONL → rotate).
///
/// Today's signature has no `&FallbackRunnerConfig` / `RunnerKind` params,
/// so the body is a placeholder that panics until FEAT-005 / FEAT-006
/// rewrites it.
#[test]
#[ignore = "FEAT-005 / FEAT-006: requires FallbackRunnerConfig + RunnerKind threading + RecoveryAction::FallbackToProvider"]
fn fallback_enabled_claude_at_ceiling_promotes_to_grok() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "PROMO-FEAT-001";
    let conn = make_conn_with_task(task_id, Some(OPUS_MODEL_1M));
    let mut _ctx = IterationContext::new(10);
    let _pr = make_prompt_result(task_id);

    // Future shape (FEAT-005 / FEAT-006):
    //   let cfg = FallbackRunnerConfig {
    //       enabled: true,
    //       model: GROK_DEFAULT_MODEL.into(),
    //       ..Default::default()
    //   };
    //   let action = overflow::handle_prompt_too_long(
    //       &mut ctx, &conn, task_id,
    //       Some("high"), Some(OPUS_MODEL_1M),
    //       &pr, 1, Some("run-promote"), tmp.path(), None,
    //       /* effective_runner */ RunnerKind::Claude,
    //       /* fallback_cfg    */ Some(&cfg),
    //   );
    //   assert!(matches!(action, RecoveryAction::FallbackToProvider { ref provider, ref model }
    //                    if provider == "grok" && model == GROK_DEFAULT_MODEL));
    //   assert_eq!(ctx.runner_overrides.get(task_id), Some(&RunnerKind::Grok));
    //   assert_eq!(ctx.model_overrides.get(task_id).map(String::as_str), Some(GROK_DEFAULT_MODEL));
    //   assert_eq!(task_model(&conn, task_id).as_deref(), Some(GROK_DEFAULT_MODEL),
    //              "tasks.model UPDATE must run in the same operation so resolve_task_model picks Grok next iter");
    //   assert_eq!(task_status(&conn, task_id), "todo",
    //              "FallbackToProvider rung must reset status to 'todo' so the next iteration retries on Grok");
    //   let events = read_events(tmp.path());
    //   assert_eq!(events.len(), 1);
    //   assert_eq!(events[0].runner.as_deref(), Some("grok"));

    let _ = tmp.path();
    let _ = task_id;
    let _ = &conn;
    panic!(
        "FEAT-005 not yet wired — when implemented, this test must drive the new \
         signature and assert: RecoveryAction::FallbackToProvider, \
         runner_overrides[task]=Grok, model_overrides[task]={GROK_DEFAULT_MODEL}, \
         tasks.model={GROK_DEFAULT_MODEL}, status='todo'"
    );
}

// ── AC #2 — Fallback enabled + task already on Grok → Blocked, no mutation ────

/// FEAT-005: when a task has already been promoted (effective_runner == Grok)
/// AND continues to overflow, the rung 4 (FallbackToProvider) check must NOT
/// re-fire — there is no further provider to escape to. The helper returns
/// `RecoveryAction::Blocked`, leaves `runner_overrides` untouched (the
/// existing Grok override stays), and does NOT execute the tasks.model
/// UPDATE again (PRD §2.5 idempotency: pin on the single computed
/// `effective_runner` value).
///
/// Today's signature can't represent "already on Grok" because
/// `runner_overrides` doesn't exist on `IterationContext` yet. Marked
/// `#[ignore]` with the future shape in comments.
#[test]
#[ignore = "FEAT-005 / FEAT-006: requires runner_overrides field + effective_runner gate"]
fn fallback_enabled_task_already_on_grok_returns_blocked() {
    let tmp = TempDir::new().expect("tempdir");
    let task_id = "ALREADY-GROK-001";
    // Seed the DB with the task already on the Grok model (e.g. an earlier
    // iteration promoted it).
    let conn = make_conn_with_task(task_id, Some(GROK_DEFAULT_MODEL));
    let mut _ctx = IterationContext::new(10);
    let _pr = make_prompt_result(task_id);

    // Future shape (FEAT-005 / FEAT-006):
    //   ctx.runner_overrides.insert(task_id.to_string(), RunnerKind::Grok);
    //   let cfg = FallbackRunnerConfig { enabled: true, model: GROK_DEFAULT_MODEL.into(), ..Default::default() };
    //   let action = overflow::handle_prompt_too_long(
    //       &mut ctx, &conn, task_id,
    //       Some("high"), Some(GROK_DEFAULT_MODEL),
    //       &pr, 1, None, tmp.path(), None,
    //       /* effective_runner */ RunnerKind::Grok,
    //       /* fallback_cfg    */ Some(&cfg),
    //   );
    //   assert!(matches!(action, RecoveryAction::Blocked),
    //           "task already on Grok at the ceiling must land on Blocked, not re-promote");
    //   // runner_overrides preserved at Grok (do NOT clobber the prior promotion).
    //   assert_eq!(ctx.runner_overrides.get(task_id), Some(&RunnerKind::Grok));
    //   // tasks.model must remain at the Grok value — no second UPDATE.
    //   assert_eq!(task_model(&conn, task_id).as_deref(), Some(GROK_DEFAULT_MODEL));
    //   assert_eq!(task_status(&conn, task_id), "blocked");

    let _ = tmp.path();
    let _ = task_id;
    let _ = &conn;
    panic!(
        "FEAT-005 not yet wired — when implemented, this test must assert \
         RecoveryAction::Blocked + runner_overrides untouched + tasks.model untouched"
    );
}

// ── AC #5 — RecoveryAction::FallbackToProvider serializes correctly ───────────

/// FEAT-005: the new variant must serialize with `tag = "action"` set to
/// `"fallback_to_provider"` and sibling fields `provider`/`model` (NOT
/// nested inside an object). Matches the existing serde shape for other
/// rungs (see `recovery_escalate_model_serialization` in
/// `src/loop_engine/overflow.rs::tests`).
#[test]
#[ignore = "FEAT-005: RecoveryAction::FallbackToProvider variant not yet defined"]
fn fallback_to_provider_serializes_with_snake_case_tag_and_siblings() {
    // Future shape (FEAT-005):
    //   let v = serde_json::to_value(RecoveryAction::FallbackToProvider {
    //       provider: "grok".to_string(),
    //       model: GROK_DEFAULT_MODEL.to_string(),
    //   }).unwrap();
    //   assert_eq!(
    //       v,
    //       serde_json::json!({
    //           "action": "fallback_to_provider",
    //           "provider": "grok",
    //           "model": GROK_DEFAULT_MODEL,
    //       }),
    //   );
    //   // Round-trip preserves equality.
    //   let s = serde_json::to_string(&v).unwrap();
    //   let back: RecoveryAction = serde_json::from_str(&s).unwrap();
    //   assert!(matches!(back, RecoveryAction::FallbackToProvider { ref provider, ref model }
    //                    if provider == "grok" && model == GROK_DEFAULT_MODEL));
    panic!(
        "FEAT-005 not yet wired — when implemented, RecoveryAction::FallbackToProvider \
         must serialize to {{\"action\":\"fallback_to_provider\",\"provider\":\"grok\",\"model\":\"{GROK_DEFAULT_MODEL}\"}}"
    );
}

// ── AC #6 — OverflowEvent.runner: Some serializes, None is skipped ────────────

/// FEAT-005: the new `runner` field on `OverflowEvent` is `Option<String>`
/// with `#[serde(skip_serializing_if = "Option::is_none")]` (Learning #2256
/// — backward-compat). `Some("grok")` serializes as a sibling string field;
/// `None` is omitted from the JSON object entirely (NOT serialized as
/// `null` or empty string).
#[test]
#[ignore = "FEAT-005: OverflowEvent.runner field not yet defined"]
fn overflow_event_runner_some_serializes_none_is_skipped() {
    // Future shape (FEAT-005): add `runner: Option<String>` to OverflowEvent
    // with `#[serde(skip_serializing_if = "Option::is_none")]`.
    //
    //   let mut ev = sample_event();
    //   ev.runner = Some("grok".to_string());
    //   let v = serde_json::to_value(&ev).unwrap();
    //   assert_eq!(v["runner"], serde_json::Value::String("grok".into()),
    //              "Some(\"grok\") must serialize as a sibling string field");
    //
    //   let mut ev = sample_event();
    //   ev.runner = None;
    //   let v = serde_json::to_value(&ev).unwrap();
    //   let obj = v.as_object().unwrap();
    //   assert!(!obj.contains_key("runner"),
    //           "runner=None must be omitted entirely (skip_serializing_if), not serialized as null/empty");
    //
    //   // Round-trip preserves the None.
    //   let s = serde_json::to_string(&ev).unwrap();
    //   let back: OverflowEvent = serde_json::from_str(&s).unwrap();
    //   assert_eq!(back.runner, None);
    panic!(
        "FEAT-005 not yet wired — when implemented, OverflowEvent.runner must be \
         Option<String> with #[serde(skip_serializing_if = \"Option::is_none\")]"
    );
}

// ── AC #7 — user_message for FallbackToProvider mentions all five fields ──────

/// FEAT-005: the user-visible stderr line for the new rung must include the
/// `task_id`, current `effort`, current `model`, AND the new provider/model.
/// Operators need to be able to identify (a) which task pivoted, (b) what
/// state it was in, and (c) where it is now — all in one log line.
#[test]
#[ignore = "FEAT-005: RecoveryAction::FallbackToProvider variant + user_message arm not yet defined"]
fn user_message_fallback_to_provider_exact_string() {
    // Future shape (FEAT-005):
    //   let msg = RecoveryAction::FallbackToProvider {
    //       provider: "grok".to_string(),
    //       model: GROK_DEFAULT_MODEL.to_string(),
    //   }
    //   .user_message("MY-TASK-001", Some("high"), Some(OPUS_MODEL_1M));
    //   assert_eq!(
    //       msg,
    //       format!(
    //           "Prompt is too long for MY-TASK-001 at effort high, model {} — \
    //            falling back to provider grok (model {}) (Claude ladder exhausted)",
    //           OPUS_MODEL_1M, GROK_DEFAULT_MODEL,
    //       ),
    //   );
    panic!(
        "FEAT-005 not yet wired — when implemented, user_message for FallbackToProvider \
         must include task_id, effort, model, provider, AND new model in one line"
    );
}

// ── AC #9 — Test file compiles (per learning #1739 / #2139) ───────────────────

/// Compile-only marker. The file's successful build is the assertion; this
/// stub catches any future build break as a missing test rather than a
/// silent removal.
#[test]
fn test_file_compiles_marker() {
    // Touch a symbol from each public type referenced above so the linker
    // can't dead-code-eliminate the imports.
    assert_eq!(OPUS_MODEL_1M, OPUS_MODEL_1M);
    assert_eq!(GROK_DEFAULT_MODEL, "grok-4-fast");
}
