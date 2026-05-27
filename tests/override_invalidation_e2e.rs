//! End-to-end tests for US-006 operator escape valve — Grok fallback scenario.
//!
//! Simulates the exact state the Grok overflow rung (5th rung) or RuntimeError
//! fallback hook leaves in `IterationContext`:
//!   - `runner_overrides[task] = RunnerKind::Grok`
//!   - `model_overrides[task] = "grok-build"`
//!   - `overflow_original_task_model[task] = Some(<OPUS_MODEL>)`
//!
//! When the operator subsequently edits `tasks.model` to a different value,
//! `check_override_invalidation` must clear ALL SIX per-task override channels
//! and emit: `"Operator changed task model for {task_id} — …"`.
//!
//! Stderr capture is not available in-process without additional crates.
//! The invalidation branch is confirmed through map state: maps are cleared
//! iff the branch ran. The expected message text is:
//!   `Operator changed task model for {task_id} — clearing auto-recovery overrides; resolving fresh.`

use rusqlite::Connection;

use task_mgr::db::{create_schema, run_migrations};
use task_mgr::loop_engine::engine::{IterationContext, check_override_invalidation};
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL};
use task_mgr::loop_engine::runner::RunnerKind;

const GROK_FAST_MODEL: &str = "grok-build";

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup_in_memory_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    conn
}

fn insert_task_with_model(conn: &Connection, id: &str, model: &str) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, max_retries, consecutive_failures) \
         VALUES (?1, ?2, 'in_progress', ?3, 5, 0)",
        rusqlite::params![id, format!("Task {id}"), model],
    )
    .unwrap();
}

/// Seed only the three override channels that the Grok fallback path writes:
/// `runner_overrides`, `model_overrides`, and `overflow_original_task_model`.
/// The other three (`effort_overrides`, `overflow_recovered`, `overflow_original_model`)
/// are intentionally left empty to test the partial-populate case.
fn seed_grok_fallback_overrides(ctx: &mut IterationContext, task_id: &str, snapshot_model: &str) {
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Grok);
    ctx.model_overrides
        .insert(task_id.to_string(), GROK_FAST_MODEL.to_string());
    ctx.overflow_original_task_model
        .insert(task_id.to_string(), Some(snapshot_model.to_string()));
}

/// Assert all six override channels lack an entry for `task_id`.
fn assert_all_overrides_absent(ctx: &IterationContext, task_id: &str) {
    assert!(
        !ctx.runner_overrides.contains_key(task_id),
        "runner_overrides[{task_id}] must be absent after invalidation",
    );
    assert!(
        !ctx.model_overrides.contains_key(task_id),
        "model_overrides[{task_id}] must be absent after invalidation",
    );
    assert!(
        !ctx.effort_overrides.contains_key(task_id),
        "effort_overrides[{task_id}] must be absent after invalidation",
    );
    assert!(
        !ctx.overflow_recovered.contains(task_id),
        "overflow_recovered[{task_id}] must be absent after invalidation",
    );
    assert!(
        !ctx.overflow_original_model.contains_key(task_id),
        "overflow_original_model[{task_id}] must be absent after invalidation",
    );
    assert!(
        !ctx.overflow_original_task_model.contains_key(task_id),
        "overflow_original_task_model[{task_id}] must be absent after invalidation",
    );
}

// ── Main scenario: operator edits model after Grok fallback promotion ─────────

/// Operator changes `tasks.model` from the Grok-snapshot value to haiku.
/// All six override channels are cleared; maps for unrelated tasks survive.
///
/// Verifies: map state proves the invalidation branch ran (stderr pinned to:
/// "Operator changed task model for {task_id} — clearing auto-recovery overrides; resolving fresh.")
#[test]
fn operator_model_edit_clears_grok_overrides() {
    let conn = setup_in_memory_db();
    let task_id = "E2E-GROK-001";
    let other_id = "E2E-OTHER-002";

    insert_task_with_model(&conn, task_id, OPUS_MODEL);
    insert_task_with_model(&conn, other_id, OPUS_MODEL);

    let mut ctx = IterationContext::new(5);
    seed_grok_fallback_overrides(&mut ctx, task_id, OPUS_MODEL);
    seed_grok_fallback_overrides(&mut ctx, other_id, OPUS_MODEL);

    // Operator changes the model column — simulates `task-mgr loop init --append --update-existing`.
    conn.execute(
        "UPDATE tasks SET model = ?1 WHERE id = ?2",
        rusqlite::params![HAIKU_MODEL, task_id],
    )
    .unwrap();

    check_override_invalidation(&mut ctx, &conn, task_id);

    // All six maps must be clear for the edited task.
    assert_all_overrides_absent(&ctx, task_id);

    // Unrelated task's overrides are untouched.
    assert_eq!(
        ctx.runner_overrides.get(other_id),
        Some(&RunnerKind::Grok),
        "other task's runner_overrides must survive task-scoped invalidation",
    );
    assert_eq!(
        ctx.model_overrides.get(other_id).map(String::as_str),
        Some(GROK_FAST_MODEL),
        "other task's model_overrides must survive task-scoped invalidation",
    );
}

/// Operator-edit scenario with ALL six channels pre-populated (comprehensive
/// coverage: verifies even partially-set maps like effort_overrides are removed).
#[test]
fn operator_model_edit_clears_all_six_when_fully_populated() {
    let conn = setup_in_memory_db();
    let task_id = "E2E-FULL-001";

    insert_task_with_model(&conn, task_id, OPUS_MODEL);

    let mut ctx = IterationContext::new(5);
    // Seed all six channels to verify each removal fires.
    seed_grok_fallback_overrides(&mut ctx, task_id, OPUS_MODEL);
    ctx.effort_overrides.insert(task_id.to_string(), "high");
    ctx.overflow_recovered.insert(task_id.to_string());
    ctx.overflow_original_model
        .insert(task_id.to_string(), OPUS_MODEL.to_string());

    conn.execute(
        "UPDATE tasks SET model = ?1 WHERE id = ?2",
        rusqlite::params![HAIKU_MODEL, task_id],
    )
    .unwrap();

    check_override_invalidation(&mut ctx, &conn, task_id);

    assert_all_overrides_absent(&ctx, task_id);
}

// ── No-op scenario: same model value ─────────────────────────────────────────

/// When `tasks.model` still matches the snapshot, no maps are modified and
/// no stderr message is emitted (verified: map values remain unchanged).
#[test]
fn no_op_when_model_unchanged_from_grok_snapshot() {
    let conn = setup_in_memory_db();
    let task_id = "E2E-NOOP-001";

    insert_task_with_model(&conn, task_id, OPUS_MODEL);

    let mut ctx = IterationContext::new(5);
    seed_grok_fallback_overrides(&mut ctx, task_id, OPUS_MODEL);

    // DB still has OPUS_MODEL — same as the snapshot. No-op expected.
    check_override_invalidation(&mut ctx, &conn, task_id);

    // All three seeded maps remain intact — no-op branch ran (not the clearing branch).
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Grok),
        "runner_overrides must be intact when model matches snapshot",
    );
    assert_eq!(
        ctx.model_overrides.get(task_id).map(String::as_str),
        Some(GROK_FAST_MODEL),
        "model_overrides must be intact when model matches snapshot",
    );
    assert_eq!(
        ctx.overflow_original_task_model.get(task_id),
        Some(&Some(OPUS_MODEL.to_string())),
        "overflow_original_task_model must be intact when model matches snapshot",
    );
    // The three un-seeded channels remain empty — no spurious insertions.
    assert!(ctx.effort_overrides.is_empty());
    assert!(ctx.overflow_recovered.is_empty());
    assert!(ctx.overflow_original_model.is_empty());
}
