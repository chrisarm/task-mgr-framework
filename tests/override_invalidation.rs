//! TDD scaffolding for US-006 / FR-008 — operator escape valve via explicit
//! `tasks.model` edits invalidating in-memory auto-recovery overrides.
//!
//! Contract under test (post-FEAT-008):
//!
//! `check_override_invalidation(&Connection, &mut IterationContext, task_id)`
//! is called from the main thread at the top of every iteration, BEFORE
//! runner dispatch. For each task that has an entry in
//! `ctx.overflow_original_task_model`, it re-reads `tasks.model` from the DB
//! and compares with the snapshotted value. On any divergence (Some→Some,
//! Some→None, None→Some) it clears ALL SIX per-task override entries for
//! that task:
//!
//!   1. `effort_overrides`
//!   2. `model_overrides`
//!   3. `overflow_recovered`            (HashSet — `.remove(task)`)
//!   4. `overflow_original_model`
//!   5. `runner_overrides`              (FEAT-006 field)
//!   6. `overflow_original_task_model`  (FEAT-006 field — also the snapshot itself)
//!
//! and emits a single-line stderr notice. A task NOT present in
//! `overflow_original_task_model` is a no-op (no DB read, no map changes).
//! A no-op edit (`tasks.model` unchanged from snapshot) is also a no-op
//! (no map writes, no stderr).
//!
//! ── Compile/run model ────────────────────────────────────────────────────────
//!
//! The fields `runner_overrides` and `overflow_original_task_model`, and the
//! function `check_override_invalidation`, do not yet exist (FEAT-006 adds
//! the fields, FEAT-008 adds the function). Every test that drives the new
//! contract is therefore `#[ignore = "FEAT-008: ..."]`; the body uses today's
//! signatures so the file compiles. A compile-marker test runs unconditionally
//! so the AC #7 invariant (test file compiles) surfaces as a missing test if
//! the file ever stops building.

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::engine::IterationContext;
use task_mgr::loop_engine::model::{HAIKU_MODEL, OPUS_MODEL};

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

/// Pre-populate every override channel for `task_id` so a successful
/// invalidation call clears all six entries. Mirrors what the production
/// overflow ladder + RuntimeError fallback hook would have written when the
/// task was originally promoted to Grok. Keeping the seeding inside one
/// helper means a future field rename only updates one place.
///
/// Today this compiles only against the four EXISTING fields; the two
/// FEAT-006 fields (`runner_overrides`, `overflow_original_task_model`)
/// are commented out and will be uncommented by FEAT-008 when it lands the
/// new fields. The helper is kept here (rather than inlined) so the
/// FEAT-008 author has a single edit point.
#[allow(dead_code)]
fn seed_all_overrides_for_task(ctx: &mut IterationContext, task_id: &str, snapshot_model: &str) {
    ctx.effort_overrides.insert(task_id.to_string(), "high");
    ctx.model_overrides
        .insert(task_id.to_string(), OPUS_MODEL.to_string());
    ctx.overflow_recovered.insert(task_id.to_string());
    ctx.overflow_original_model
        .insert(task_id.to_string(), snapshot_model.to_string());
    // FEAT-006 fields — uncomment when the struct gains them, FEAT-008 will
    // wire the check that reads/clears these too:
    //   ctx.runner_overrides
    //       .insert(task_id.to_string(), RunnerKind::Grok);
    //   ctx.overflow_original_task_model
    //       .insert(task_id.to_string(), Some(snapshot_model.to_string()));
    let _ = snapshot_model;
}

/// Assert that NO override map carries an entry for `task_id`. Used by the
/// "successful invalidation clears all six" tests. Today this asserts on the
/// four existing maps; FEAT-008 will extend it to the two FEAT-006 maps.
#[allow(dead_code)]
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
    // FEAT-006 fields — uncomment with FEAT-008:
    //   assert!(!ctx.runner_overrides.contains_key(task_id), ...);
    //   assert!(!ctx.overflow_original_task_model.contains_key(task_id), ...);
}

// ── AC #1 — Invalidation clears all six override entries ──────────────────────

/// FEAT-008: a snapshot mismatch (`tasks.model` differs from
/// `overflow_original_task_model[task]`) clears ALL SIX override entries for
/// that task on a single call to `check_override_invalidation`.
///
/// Setup: snapshot = `OPUS_MODEL`; `tasks.model` updated to `HAIKU_MODEL`.
/// Expected: every per-task entry in the six override channels is gone after
/// one call; the maps' overall structure (entries for OTHER tasks) is left
/// alone.
#[test]
#[ignore = "FEAT-008: requires check_override_invalidation + runner_overrides/overflow_original_task_model fields"]
fn invalidation_clears_all_six_override_entries() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "INVAL-001", Some(OPUS_MODEL));
    insert_task(&conn, "OTHER-002", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "INVAL-001", OPUS_MODEL);
    // Seed an unrelated task's overrides so we can prove the call is
    // task-scoped (the loop iterates per-task; clearing INVAL-001 must not
    // touch OTHER-002).
    seed_all_overrides_for_task(&mut ctx, "OTHER-002", OPUS_MODEL);

    // Operator edits the model column out-of-band.
    set_task_model(&conn, "INVAL-001", Some(HAIKU_MODEL));

    // After FEAT-008:
    //   check_override_invalidation(&conn, &mut ctx, "INVAL-001").unwrap();
    //   assert_all_overrides_cleared(&ctx, "INVAL-001");
    //   // OTHER-002's overrides remain intact:
    //   assert!(ctx.model_overrides.contains_key("OTHER-002"));

    panic!(
        "FEAT-008 not yet wired — when implemented, check_override_invalidation(&conn, \
         &mut ctx, \"INVAL-001\") must clear all six per-task entries (effort_overrides, \
         model_overrides, overflow_recovered, overflow_original_model, runner_overrides, \
         overflow_original_task_model) and leave OTHER-002's overrides intact"
    );
}

// ── AC #2 — Post-invalidation dispatch honors the new explicit model ──────────

/// FEAT-008: after invalidation, the very next runner-dispatch call for the
/// task must spawn `RunnerKind::Claude` with the operator's new model
/// (`HAIKU_MODEL`), confirming that no stale override silently shadows the
/// explicit edit. The override-clearing step is what enables this — without
/// it, `runner_overrides[INVAL-001] = Grok` and `model_overrides[INVAL-001]
/// = grok-2-...` would still win the precedence race against the DB column.
#[test]
#[ignore = "FEAT-008: requires runner_overrides field + effective-runner pipeline that reads it"]
fn post_invalidation_dispatch_uses_operator_model_via_claude() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "INVAL-DISP-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "INVAL-DISP-001", OPUS_MODEL);
    set_task_model(&conn, "INVAL-DISP-001", Some(HAIKU_MODEL));

    // After FEAT-008:
    //   check_override_invalidation(&conn, &mut ctx, "INVAL-DISP-001").unwrap();
    //   let resolved = resolve_effective_runner(&conn, &ctx, "INVAL-DISP-001").unwrap();
    //   assert_eq!(resolved.kind, RunnerKind::Claude);
    //   assert_eq!(resolved.model, HAIKU_MODEL);

    panic!(
        "FEAT-008 not yet wired — when implemented, post-invalidation dispatch resolves to \
         RunnerKind::Claude + model={HAIKU_MODEL}, NOT RunnerKind::Grok from the cleared override"
    );
}

// ── AC #3 — No-op edit case: same model, no map mutations, no stderr ──────────

/// FEAT-008: when `tasks.model` matches the snapshotted value
/// (`overflow_original_task_model[task] == read_model(conn, task)`), the
/// check is a no-op: no maps are modified, no stderr notice is emitted.
/// This is the common per-iteration case (operator made no edit) and must
/// be cheap (one DB read, no mutations).
#[test]
#[ignore = "FEAT-008: requires check_override_invalidation"]
fn no_op_when_tasks_model_matches_snapshot() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "NOOP-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "NOOP-001", OPUS_MODEL);

    // DB still has OPUS_MODEL — snapshot matches.
    // After FEAT-008:
    //   let stderr = capture_stderr(|| {
    //       check_override_invalidation(&conn, &mut ctx, "NOOP-001").unwrap();
    //   });
    //   // Maps unchanged:
    //   assert_eq!(ctx.effort_overrides.get("NOOP-001"), Some(&"high"));
    //   assert_eq!(ctx.model_overrides.get("NOOP-001"), Some(&OPUS_MODEL.to_string()));
    //   assert!(ctx.overflow_recovered.contains("NOOP-001"));
    //   assert!(ctx.overflow_original_model.contains_key("NOOP-001"));
    //   assert!(ctx.runner_overrides.contains_key("NOOP-001"));
    //   assert!(ctx.overflow_original_task_model.contains_key("NOOP-001"));
    //   assert!(stderr.is_empty(), "no stderr on no-op, got {stderr:?}");

    panic!(
        "FEAT-008 not yet wired — when implemented, the no-op case must leave all six maps \
         unchanged AND emit zero stderr bytes (compare against today's snapshot)"
    );
}

// ── AC #4 — Clearing to NULL fires invalidation (None != Some) ────────────────

/// FEAT-008: an operator clearing `tasks.model` to NULL — e.g.
/// `task-mgr unset-model INVAL-NULL-001` falling back to the project
/// default — must trigger invalidation, since `None` differs from the
/// snapshotted `Some(OPUS_MODEL)`. The comparison is full Option equality,
/// not a fallback-to-snapshot quirk.
#[test]
#[ignore = "FEAT-008: requires check_override_invalidation"]
fn null_clearing_after_snapshot_fires_invalidation() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "INVAL-NULL-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "INVAL-NULL-001", OPUS_MODEL);

    // Operator clears the model column (falls back to project default).
    set_task_model(&conn, "INVAL-NULL-001", None);

    // After FEAT-008:
    //   check_override_invalidation(&conn, &mut ctx, "INVAL-NULL-001").unwrap();
    //   assert_all_overrides_cleared(&ctx, "INVAL-NULL-001");

    panic!(
        "FEAT-008 not yet wired — when implemented, NULL != Some(OPUS_MODEL) must trigger \
         invalidation (the comparison is full Option equality, not Some-only)"
    );
}

// ── AC #5 — Stderr notice mentions task id + canonical phrase ─────────────────

/// FEAT-008: on invalidation, exactly one stderr line is emitted that
/// includes (a) the task ID and (b) the canonical phrase
/// `Operator changed task model for ... clearing auto-recovery overrides`.
/// This is the operator-visible signal that an out-of-band edit was honored.
/// A future log refactor that drops either the task id or the phrase is a
/// regression — operators grep for both.
#[test]
#[ignore = "FEAT-008: requires check_override_invalidation + stderr capture"]
fn invalidation_emits_stderr_notice_with_task_id_and_canonical_phrase() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "STDERR-001", Some(OPUS_MODEL));

    let mut ctx = IterationContext::new(5);
    seed_all_overrides_for_task(&mut ctx, "STDERR-001", OPUS_MODEL);
    set_task_model(&conn, "STDERR-001", Some(HAIKU_MODEL));

    // After FEAT-008:
    //   let stderr = capture_stderr(|| {
    //       check_override_invalidation(&conn, &mut ctx, "STDERR-001").unwrap();
    //   });
    //   assert!(stderr.contains("STDERR-001"), "task id missing from stderr: {stderr:?}");
    //   assert!(
    //       stderr.contains("Operator changed task model for")
    //           && stderr.contains("clearing auto-recovery overrides"),
    //       "canonical phrase missing from stderr: {stderr:?}",
    //   );

    panic!(
        "FEAT-008 not yet wired — when implemented, the stderr notice must include both \
         the task ID (STDERR-001) and the canonical phrase \
         'Operator changed task model for ... clearing auto-recovery overrides'"
    );
}

// ── AC #6 — Task NOT in overflow_original_task_model → no-op (no DB read) ─────

/// FEAT-008: a task that never went through the overflow ladder or
/// RuntimeError fallback hook has NO entry in `overflow_original_task_model`.
/// `check_override_invalidation` must short-circuit on that lookup — no DB
/// `SELECT tasks.model` query, no map mutations, no stderr. This is the
/// dominant case (most tasks never need a Grok pivot) and must be free.
///
/// The "no DB read" assertion is exercised in FEAT-008 by passing a
/// connection wrapper that counts queries; here we pin the no-mutation
/// half of the contract and document the wider intent.
#[test]
#[ignore = "FEAT-008: requires check_override_invalidation + overflow_original_task_model field"]
fn no_op_when_task_not_in_overflow_original_task_model() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "ABSENT-001", Some(OPUS_MODEL));

    let _ctx = IterationContext::new(5);
    // Intentionally do NOT seed any overrides — this is a fresh task that
    // never tripped the overflow ladder. The check must short-circuit.
    // FEAT-008 will rebind as `let mut ctx = ...` and pass `&mut ctx` to
    // `check_override_invalidation`; for now ctx is unused, prefixed `_`.

    // After FEAT-008:
    //   check_override_invalidation(&conn, &mut ctx, "ABSENT-001").unwrap();
    //   assert!(ctx.runner_overrides.is_empty());
    //   assert!(ctx.overflow_original_task_model.is_empty());
    //   assert!(ctx.effort_overrides.is_empty());
    //   assert!(ctx.model_overrides.is_empty());
    //   assert!(ctx.overflow_recovered.is_empty());
    //   assert!(ctx.overflow_original_model.is_empty());

    panic!(
        "FEAT-008 not yet wired — when implemented, a task absent from \
         overflow_original_task_model must short-circuit before any DB read or map mutation"
    );
}

// ── AC #7 — Test file compiles ────────────────────────────────────────────────

/// Compile-only contract pin: importing the symbols above already proves
/// the file builds. This stub test is a single explicit assertion of the
/// AC #7 invariant ("Test file compiles (may be #[ignore] until FEAT-008)")
/// so a future build break surfaces as a missing test rather than as a
/// silent removal.
#[test]
fn test_file_compiles_marker() {
    // No-op — the file's existence + successful build is the assertion.
    // Touch both helpers so dead-code analysis can't strip them before
    // FEAT-008 lands the bodies that will exercise them.
    let (_dir, _conn) = setup_db();
    let mut ctx = IterationContext::new(1);
    seed_all_overrides_for_task(&mut ctx, "COMPILE-MARK", OPUS_MODEL);
    // A brand-new ctx has nothing to clear — `assert_all_overrides_cleared`
    // succeeds trivially and exercises the helper's import path.
    assert_all_overrides_cleared(&IterationContext::new(1), "COMPILE-MARK");
    assert_eq!(OPUS_MODEL, OPUS_MODEL);
    assert_ne!(OPUS_MODEL, HAIKU_MODEL);
}
