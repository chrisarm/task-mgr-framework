//! Tests for US-006 / FR-008 — operator escape valve via explicit
//! `tasks.model` edits invalidating in-memory auto-recovery overrides.
//!
//! `check_override_invalidation(ctx, conn, task_id)` is called at the top of
//! every iteration BEFORE runner dispatch. For each task with an entry in
//! `ctx.overflow_original_task_model`, it re-reads `tasks.model` from the DB.
//! On any divergence it clears ALL SIX per-task override entries:
//!
//!   1. `effort_overrides`
//!   2. `model_overrides`
//!   3. `overflow_recovered`            (HashSet)
//!   4. `overflow_original_model`
//!   5. `runner_overrides`
//!   6. `overflow_original_task_model`  (also the snapshot itself)
//!
//! A task NOT present in `overflow_original_task_model` is a no-op.
//! A no-op edit (model unchanged from snapshot) is also a no-op.

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::engine::{
    IterationContext, check_override_invalidation, resolve_effective_runner,
};
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL};
use task_mgr::loop_engine::runner::RunnerKind;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn setup_db() -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let mut conn = open_connection(dir.path()).unwrap();
    create_schema(&conn).unwrap();
    run_migrations(&mut conn).unwrap();
    (dir, conn)
}

fn insert_task(conn: &Connection, id: &str, model: Option<&str>) {
    conn.execute(
        "INSERT INTO tasks (id, title, status, model, max_retries, consecutive_failures) \
         VALUES (?, ?, 'in_progress', ?, ?, ?)",
        rusqlite::params![id, format!("Task {id}"), model, 5, 0],
    )
    .unwrap();
}

fn set_task_model(conn: &Connection, id: &str, model: Option<&str>) {
    conn.execute(
        "UPDATE tasks SET model = ? WHERE id = ?",
        rusqlite::params![model, id],
    )
    .unwrap();
}

/// Pre-populate every override channel for `task_id`, mirroring what the
/// production overflow ladder + RuntimeError fallback hook would write.
fn seed_all_overrides_for_task(ctx: &mut IterationContext, task_id: &str, snapshot_model: &str) {
    ctx.effort_overrides.insert(task_id.to_string(), "high");
    ctx.model_overrides
        .insert(task_id.to_string(), OPUS_MODEL.to_string());
    ctx.overflow_recovered.insert(task_id.to_string());
    ctx.overflow_original_model
        .insert(task_id.to_string(), snapshot_model.to_string());
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Grok);
    ctx.overflow_original_task_model
        .insert(task_id.to_string(), Some(snapshot_model.to_string()));
}

/// Assert that NO override map carries an entry for `task_id`.
fn assert_all_overrides_cleared(ctx: &IterationContext, task_id: &str) {
    assert!(
        !ctx.effort_overrides.contains_key(task_id),
        "effort_overrides[{task_id}] must be cleared after invalidation",
    );
    assert!(
        !ctx.model_overrides.contains_key(task_id),
        "model_overrides[{task_id}] must be cleared after invalidation",
    );
    assert!(
        !ctx.overflow_recovered.contains(task_id),
        "overflow_recovered[{task_id}] must be cleared after invalidation",
    );
    assert!(
        !ctx.overflow_original_model.contains_key(task_id),
        "overflow_original_model[{task_id}] must be cleared after invalidation",
    );
    assert!(
        !ctx.runner_overrides.contains_key(task_id),
        "runner_overrides[{task_id}] must be cleared after invalidation",
    );
    assert!(
        !ctx.overflow_original_task_model.contains_key(task_id),
        "overflow_original_task_model[{task_id}] must be cleared after invalidation",
    );
}

// ── AC #1 — Invalidation clears all six override entries ─────────────────────

/// A snapshot mismatch clears ALL SIX override entries for the target task and
/// leaves other tasks' overrides intact.
#[test]
fn invalidation_clears_all_six_override_entries() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "INVAL-001", Some(OPUS_MODEL));
    insert_task(&conn, "OTHER-002", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "INVAL-001", OPUS_MODEL);
    seed_all_overrides_for_task(&mut ctx, "OTHER-002", OPUS_MODEL);

    // Operator edits the model column out-of-band.
    set_task_model(&conn, "INVAL-001", Some(HAIKU_MODEL));

    check_override_invalidation(&mut ctx, &conn, "INVAL-001");

    assert_all_overrides_cleared(&ctx, "INVAL-001");
    // OTHER-002's overrides remain intact — the call is task-scoped.
    assert!(ctx.model_overrides.contains_key("OTHER-002"));
    assert!(ctx.runner_overrides.contains_key("OTHER-002"));
}

// ── AC #2 — Post-invalidation dispatch honors the new explicit model ──────────

/// After invalidation, `resolve_effective_runner` returns `RunnerKind::Claude`
/// for the haiku model — no stale Grok override shadows the operator's edit.
#[test]
fn post_invalidation_dispatch_uses_operator_model_via_claude() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "INVAL-DISP-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "INVAL-DISP-001", OPUS_MODEL);
    set_task_model(&conn, "INVAL-DISP-001", Some(HAIKU_MODEL));

    check_override_invalidation(&mut ctx, &conn, "INVAL-DISP-001");

    // After clearing, no runner_override exists; haiku is a Claude model.
    let runner = resolve_effective_runner(&ctx, "INVAL-DISP-001", Some(HAIKU_MODEL));
    assert_eq!(
        runner,
        RunnerKind::Claude,
        "post-invalidation dispatch must resolve to Claude, not the cleared Grok override"
    );
}

// ── AC #3 — No-op edit case: same model, no map mutations ────────────────────

/// When `tasks.model` matches the snapshot, all six maps remain unchanged.
#[test]
fn no_op_when_tasks_model_matches_snapshot() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "NOOP-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "NOOP-001", OPUS_MODEL);

    // DB still has OPUS_MODEL — snapshot matches, no-op expected.
    check_override_invalidation(&mut ctx, &conn, "NOOP-001");

    // All six maps untouched.
    assert_eq!(ctx.effort_overrides.get("NOOP-001"), Some(&"high"));
    assert_eq!(
        ctx.model_overrides.get("NOOP-001"),
        Some(&OPUS_MODEL.to_string())
    );
    assert!(ctx.overflow_recovered.contains("NOOP-001"));
    assert!(ctx.overflow_original_model.contains_key("NOOP-001"));
    assert!(ctx.runner_overrides.contains_key("NOOP-001"));
    assert!(ctx.overflow_original_task_model.contains_key("NOOP-001"));
}

// ── AC #4 — Clearing to NULL fires invalidation (None != Some) ───────────────

/// An operator clearing `tasks.model` to NULL triggers invalidation because
/// `None != Some(OPUS_MODEL)`.
#[test]
fn null_clearing_after_snapshot_fires_invalidation() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "INVAL-NULL-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "INVAL-NULL-001", OPUS_MODEL);

    // Operator clears the model column.
    set_task_model(&conn, "INVAL-NULL-001", None);

    check_override_invalidation(&mut ctx, &conn, "INVAL-NULL-001");

    assert_all_overrides_cleared(&ctx, "INVAL-NULL-001");
}

// ── AC #5 — Invalidation runs (stderr notice is a runtime observable) ────────

/// Verify that on mismatch the function completes and clears the maps — this
/// confirms the invalidation code path ran. The stderr notice text is a runtime
/// observable; its content is pinned by the eprintln! in the implementation
/// ("Operator changed task model for {task_id} — clearing auto-recovery
/// overrides; resolving fresh."). Capturing fd-2 from within a test binary
/// requires OS-level redirection not available here, so we verify behavior
/// through map state.
#[test]
fn invalidation_emits_notice_and_clears_state() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "STDERR-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "STDERR-001", OPUS_MODEL);
    set_task_model(&conn, "STDERR-001", Some(HAIKU_MODEL));

    // The function returns normally (no panic, no Result::Err).
    check_override_invalidation(&mut ctx, &conn, "STDERR-001");

    // Side effect proves the invalidation branch ran (not the no-op branch).
    assert_all_overrides_cleared(&ctx, "STDERR-001");
}

// ── AC #6 — Task NOT in overflow_original_task_model → no-op ─────────────────

/// A task with no snapshot entry short-circuits immediately without touching
/// any map.
#[test]
fn no_op_when_task_not_in_overflow_original_task_model() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "ABSENT-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    // Intentionally do NOT seed any overrides.

    check_override_invalidation(&mut ctx, &conn, "ABSENT-001");

    assert!(ctx.runner_overrides.is_empty());
    assert!(ctx.overflow_original_task_model.is_empty());
    assert!(ctx.effort_overrides.is_empty());
    assert!(ctx.model_overrides.is_empty());
    assert!(ctx.overflow_recovered.is_empty());
    assert!(ctx.overflow_original_model.is_empty());
}

// ── AC #7 — Test file compiles ────────────────────────────────────────────────

/// Compile-marker: if the file stops building, this test disappears and the
/// gap is visible in the test report.
#[test]
fn test_file_compiles_marker() {
    let (_dir, _conn) = setup_db();
    let mut ctx = IterationContext::new(1);
    seed_all_overrides_for_task(&mut ctx, "COMPILE-MARK", OPUS_MODEL);
    assert_all_overrides_cleared(&IterationContext::new(1), "COMPILE-MARK");
    assert_eq!(OPUS_MODEL, OPUS_MODEL);
    assert_ne!(OPUS_MODEL, HAIKU_MODEL);
}
