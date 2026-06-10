//! TEST-010 — full-path operator-escape-valve lifecycle on a NULL-model
//! (anchor-resolved) task.
//!
//! The existing escape-valve unit tests (`override_invalidation.rs`,
//! `model_selection_engine_edges::edge_case_0_*`) SEED the recovery channels by
//! hand to mirror what the ladder writes, then call `invalidate_stale_overrides`
//! in isolation. This test drives the **real** overflow ladder
//! (`reactions::post_output::handle_overflow`) to PRODUCE those channels — the
//! genuine escalation — and then exercises the escape valve across the full
//! lifecycle:
//!
//!   1. anchor-resolved task: `tasks.model` is NULL.
//!   2. escalation: `handle_overflow` at the ceiling pivots to the configured
//!      cross-provider fallback (rung 4, FallbackToProvider → Grok). It writes
//!      `runner_overrides` / `model_overrides` / `tasks.model` AND snapshots the
//!      pre-pivot `tasks.model` (NULL) into `overflow_original_task_model`.
//!   3. the ladder's OWN `tasks.model` write is absorbed: a follow-up
//!      `invalidate_stale_overrides` is a no-op (the recovery it just set up
//!      survives) — NULL-original semantics.
//!   4. operator edits `tasks.model` out-of-band to a DIFFERENT model → the
//!      six-channel clear fires.
//!   5. downstream effect: `resolve_effective_runner` now follows the operator's
//!      model (Claude), with the stale Grok runner override gone.
//!
//! Stderr capture is unavailable in-process; "fires exactly once" is verified
//! through observable map state (the same approach the existing suites take).

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::loop_engine::engine::{
    EffectiveRunnerInput, IterationContext, resolve_effective_runner,
};
use task_mgr::loop_engine::model::{FABLE_MODEL, ONE_M_SUFFIX, SONNET_MODEL};
use task_mgr::loop_engine::overflow::RecoveryAction;
use task_mgr::loop_engine::project_config::{ModelsConfig, ProjectConfig};
use task_mgr::loop_engine::prompt::PromptResult;
use task_mgr::loop_engine::reactions::post_output::{HandleOverflowParams, handle_overflow};
use task_mgr::loop_engine::reactions::pre_spawn::invalidate_stale_overrides;
use task_mgr::loop_engine::runner::RunnerKind;

/// The grok CLI's only model id (the builtin Grok ladder's single rung). Not a
/// Claude id, so no_hardcoded_models (matches `claude-*` only) does not flag it.
const GROK_MODEL: &str = "grok-build";

/// The Claude overflow ceiling: the 1M-context variant of the frontier (fable)
/// model. At `(fable[1m], effort=high)` rungs 1–3 are exhausted, so the ladder
/// reaches rung 4 (cross-provider pivot) when a `providers.claude.fallback` is set.
fn fable_1m() -> String {
    format!("{FABLE_MODEL}{ONE_M_SUFFIX}")
}

/// Minimal in-memory `tasks` schema with a nullable `model` column plus one
/// seeded in_progress row, mirroring `overflow_fallback_rung.rs`. `model = None`
/// is the anchor-resolved case the escape-valve NULL-original semantics target.
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
         VALUES (?1, 'fixture', 'in_progress', '2026-06-09T00:00:00Z', ?2)",
        rusqlite::params![task_id, model],
    )
    .expect("seed task row");
    conn
}

fn set_task_model(conn: &Connection, task_id: &str, model: Option<&str>) {
    conn.execute(
        "UPDATE tasks SET model = ?1 WHERE id = ?2",
        rusqlite::params![model, task_id],
    )
    .expect("update tasks.model");
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

/// `ProjectConfig` whose provider-first `models` block wires `claude.fallback =
/// "grok"` (the rung-4 pivot target) with Grok enabled.
fn models_claude_fallback_grok() -> ProjectConfig {
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
}

fn assert_recovery_channels_present(ctx: &IterationContext, id: &str) {
    assert!(
        ctx.runner_overrides.contains_key(id),
        "runner_overrides[{id}]"
    );
    assert!(
        ctx.model_overrides.contains_key(id),
        "model_overrides[{id}]"
    );
    assert!(
        ctx.overflow_recovered.contains(id),
        "overflow_recovered[{id}]"
    );
    assert!(
        ctx.overflow_original_model.contains_key(id),
        "overflow_original_model[{id}]"
    );
    assert!(
        ctx.overflow_original_task_model.contains_key(id),
        "overflow_original_task_model[{id}]"
    );
}

fn assert_recovery_channels_cleared(ctx: &IterationContext, id: &str) {
    assert!(
        !ctx.runner_overrides.contains_key(id),
        "runner_overrides[{id}]"
    );
    assert!(
        !ctx.model_overrides.contains_key(id),
        "model_overrides[{id}]"
    );
    assert!(
        !ctx.effort_overrides.contains_key(id),
        "effort_overrides[{id}]"
    );
    assert!(
        !ctx.overflow_recovered.contains(id),
        "overflow_recovered[{id}]"
    );
    assert!(
        !ctx.overflow_original_model.contains_key(id),
        "overflow_original_model[{id}]"
    );
    assert!(
        !ctx.overflow_original_task_model.contains_key(id),
        "overflow_original_task_model[{id}]"
    );
}

#[test]
fn null_model_escalation_then_operator_edit_clears_six_channels_full_path() {
    let task_id = "ESCAPE-NULL-FEAT-001";
    let tmp = TempDir::new().expect("tempdir");
    // (1) anchor-resolved task: NULL tasks.model.
    let mut conn = make_conn_with_task(task_id, None);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = models_claude_fallback_grok();
    let ceiling = fable_1m();

    // (2) ESCALATION through the real ladder: at (fable[1m], effort=high) the
    // Claude ladder is exhausted, so rung 4 pivots to the configured Grok
    // fallback. This is the genuine write of the recovery channels — not a
    // hand-seeded mirror.
    let action = handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(&ceiling),
        prompt_result: &pr,
        iteration: 1,
        run_id: Some("run-escape"),
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Claude,
        project_config: &project_cfg,
    });
    assert!(
        matches!(action, RecoveryAction::FallbackToProvider { .. }),
        "an anchor-resolved task at the ceiling with claude.fallback=grok must pivot to Grok \
         (rung 4), seeding the recovery channels, got {action:?}",
    );

    // The ladder wrote tasks.model AND snapshotted the pre-pivot NULL.
    assert_eq!(
        task_model(&conn, task_id).as_deref(),
        Some(GROK_MODEL),
        "rung 4 writes the tier-preserving fallback model into tasks.model",
    );
    assert_eq!(
        ctx.overflow_original_task_model.get(task_id),
        Some(&None),
        "the snapshot captured the pre-pivot NULL tasks.model (anchor-resolved)",
    );
    assert_eq!(
        ctx.runner_overrides.get(task_id).copied(),
        Some(RunnerKind::Grok),
        "the pivot promoted the task onto the Grok runner",
    );
    assert_recovery_channels_present(&ctx, task_id);

    // (3) The ladder's OWN tasks.model write is absorbed: invalidate is a no-op
    // (NULL-original semantics — current == model_overrides). Recovery survives.
    invalidate_stale_overrides(&mut ctx, &conn, task_id);
    assert_recovery_channels_present(&ctx, task_id);
    assert_eq!(
        ctx.runner_overrides.get(task_id).copied(),
        Some(RunnerKind::Grok),
        "the escape valve must NOT self-trip on the ladder's own tasks.model write",
    );

    // (4) Operator edits tasks.model out-of-band to a DIFFERENT model → the ONE
    // legitimate fire: the six-channel clear.
    set_task_model(&conn, task_id, Some(SONNET_MODEL));
    invalidate_stale_overrides(&mut ctx, &conn, task_id);
    assert_recovery_channels_cleared(&ctx, task_id);

    // (5) Downstream effect: runner resolution now follows the operator's model
    // (Claude), with the stale Grok override gone.
    let runner = resolve_effective_runner(
        &ctx,
        task_id,
        EffectiveRunnerInput {
            model: Some(SONNET_MODEL),
            provider_hint: None,
        },
    );
    assert_eq!(
        runner,
        RunnerKind::Claude,
        "after the clear, the operator's Claude model drives the runner — the stale Grok runner \
         override has been invalidated",
    );
}

/// Control: with NO operator edit after the escalation, the escape valve never
/// fires across repeated pre-spawn passes — the recovery the ladder set up is
/// stable (no spurious self-trip on subsequent iterations).
#[test]
fn null_model_escalation_without_operator_edit_never_clears() {
    let task_id = "ESCAPE-STABLE-FEAT-001";
    let tmp = TempDir::new().expect("tempdir");
    let mut conn = make_conn_with_task(task_id, None);
    let mut ctx = IterationContext::new(10);
    let pr = make_prompt_result(task_id);
    let project_cfg = models_claude_fallback_grok();
    let ceiling = fable_1m();

    handle_overflow(HandleOverflowParams {
        ctx: &mut ctx,
        conn: &mut conn,
        task_id,
        effort: Some("high"),
        effective_model: Some(&ceiling),
        prompt_result: &pr,
        iteration: 1,
        run_id: Some("run-stable"),
        base_dir: tmp.path(),
        slot_index: None,
        effective_runner: RunnerKind::Claude,
        project_config: &project_cfg,
    });
    assert_recovery_channels_present(&ctx, task_id);

    // Three pre-spawn passes with no operator edit — recovery must persist.
    for _ in 0..3 {
        invalidate_stale_overrides(&mut ctx, &conn, task_id);
        assert_recovery_channels_present(&ctx, task_id);
    }
}
