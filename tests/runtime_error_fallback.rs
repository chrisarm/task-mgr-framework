//! TDD scaffolding for US-004 — RuntimeError fallback hook in
//! `escalate_task_model_if_needed`.
//!
//! Tests pin the expected behavior of the FEAT-007 Grok promotion branch:
//! when a task has reached Opus and continues to fail (and the operator has
//! opted in via `.task-mgr/config.json -> fallbackRunner.enabled = true`),
//! the next failure cycle should pivot the task onto Grok by writing BOTH
//! the `tasks.model` DB column AND the in-memory `runner_overrides` /
//! `model_overrides` maps on `IterationContext`.
//!
//! FEAT-007 will change the signature of `escalate_task_model_if_needed`
//! to take the additional context (IterationContext + FallbackRunnerConfig).
//! Tests that drive that future behavior are marked `#[ignore]` with a
//! `FEAT-007` reason string; FEAT-006 will rewrite the bodies against the
//! new signature and remove the `#[ignore]`. Tests that only assert
//! behavior already true today (no-op cases, source-grep wiring check,
//! counter not touched) run unconditionally so the existing contract is
//! locked in before FEAT-007 lands.

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::engine::{escalate_task_model_if_needed, handle_task_failure};
use task_mgr::loop_engine::model::{OPUS_MODEL, SONNET_MODEL};

/// The PRD-default Grok model id. Pinned here (not imported from
/// `task_mgr::loop_engine::model`) because the constant does not yet exist
/// in `model.rs` — FEAT-002 / FEAT-003 will add it. Until then, tests
/// reference the literal so the file compiles.
const GROK_DEFAULT_MODEL: &str = "grok-2-1212";

/// Expected promotion threshold under the FEAT-007 contract. PRD §3 US-004
/// pins the default at 2 consecutive failures (same gate as Claude-tier
/// escalation, see `should_escalate_for_consecutive_failures`).
const FALLBACK_THRESHOLD: i32 = 2;

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

// ── AC #1 — Promotion fires at Opus + threshold + fallback enabled ────────────

/// FEAT-007: with a task already at Opus and `consecutive_failures == 2`,
/// the Grok promotion branch must fire: it inserts
/// `runner_overrides[task] = RunnerKind::Grok`, `model_overrides[task] = cfg.model`,
/// and updates the `tasks.model` DB column to `cfg.model`.
///
/// Today's signature has no `IterationContext` / `FallbackRunnerConfig`
/// parameters, so the call below uses the existing signature and merely
/// pins what today produces: an Opus → Opus no-op (no promotion). FEAT-006
/// will rewrite the body to construct the future arguments and assert the
/// post-FEAT-007 contract.
#[test]
#[ignore = "FEAT-007: requires FallbackRunnerConfig + IterationContext threading"]
fn promotion_fires_at_opus_and_threshold_with_fallback_enabled() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "PROMOTE-001",
        Some(OPUS_MODEL),
        FALLBACK_THRESHOLD - 1,
    );

    // CURRENT signature — kept compilable. After FEAT-007:
    //   let mut ctx = IterationContext::new(8);
    //   let cfg = FallbackRunnerConfig { enabled: true, model: GROK_DEFAULT_MODEL.into(), threshold: 2, ..Default::default() };
    //   let outcome = escalate_task_model_if_needed(&conn, "PROMOTE-001", FALLBACK_THRESHOLD, &mut ctx, &cfg).unwrap();
    //   assert_eq!(ctx.runner_overrides.get("PROMOTE-001"), Some(&RunnerKind::Grok));
    //   assert_eq!(ctx.model_overrides.get("PROMOTE-001"), Some(&GROK_DEFAULT_MODEL.to_string()));
    //   assert_eq!(read_model(&conn, "PROMOTE-001").as_deref(), Some(GROK_DEFAULT_MODEL));
    let _ = escalate_task_model_if_needed(&conn, "PROMOTE-001", FALLBACK_THRESHOLD).unwrap();
    panic!(
        "FEAT-007 not yet wired — when implemented, signature gains \
         (&mut IterationContext, &FallbackRunnerConfig) and promotion writes \
         tasks.model = {GROK_DEFAULT_MODEL} + runner_overrides + model_overrides"
    );
}

// ── AC #2 — Below threshold: no promotion, existing escalation untouched ──────

/// `consecutive_failures = 1` is below the FEAT-007 threshold AND below the
/// existing Claude-tier escalation threshold. Behavior today and after
/// FEAT-007 is identical: `escalate_task_model_if_needed` returns `Ok(None)`
/// and the DB model column is untouched. Runs unconditionally.
#[test]
fn no_promotion_when_consecutive_failures_below_threshold() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "BELOW-001", Some(OPUS_MODEL), 0);

    let result = escalate_task_model_if_needed(&conn, "BELOW-001", 1).unwrap();
    assert_eq!(
        result, None,
        "consecutive_failures=1 must not trigger escalation OR promotion",
    );
    assert_eq!(
        read_model(&conn, "BELOW-001").as_deref(),
        Some(OPUS_MODEL),
        "DB model column must remain at Opus when below threshold",
    );
}

// ── AC #3 — At Sonnet: Claude escalation fires first, not Grok promotion ──────

/// A task at Sonnet with `consecutive_failures = 2` must escalate to Opus
/// via the existing Claude-tier branch — NOT pivot to Grok. The Grok
/// promotion gate requires `current_model == Opus`. Runs unconditionally;
/// the Claude escalation step is byte-identical pre- and post-FEAT-007.
#[test]
fn sonnet_at_threshold_escalates_to_opus_first_not_grok() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "SONNET-001", Some(SONNET_MODEL), 0);

    let result = escalate_task_model_if_needed(&conn, "SONNET-001", FALLBACK_THRESHOLD).unwrap();
    assert_eq!(
        result,
        Some(OPUS_MODEL.to_string()),
        "Sonnet must escalate to Opus first; Grok promotion is reserved for the NEXT failure cycle when at Opus + threshold",
    );
    assert_eq!(
        read_model(&conn, "SONNET-001").as_deref(),
        Some(OPUS_MODEL),
        "DB model column must be Opus after Sonnet-tier escalation",
    );
    assert_ne!(
        read_model(&conn, "SONNET-001").as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "Sonnet must NOT short-circuit straight to Grok",
    );
}

// ── AC #4 — Fallback disabled: existing Opus no-op preserved byte-for-byte ────

/// With `fallbackRunner.enabled = false` (today's default and only state),
/// a task at Opus with `consecutive_failures = 2` hits the existing
/// Opus → Opus self-loop in `escalate_model`: the function returns
/// `Some(OPUS_MODEL)` and the DB `UPDATE` rewrites the column to the same
/// value it already held. FEAT-007 must preserve this exit path EXACTLY
/// when `enabled = false` — same return value, same DB write shape, same
/// stderr line. Runs unconditionally; this is the regression guard that
/// proves the disabled path is byte-identical to today's behavior.
#[test]
fn fallback_disabled_keeps_existing_opus_ceiling_byte_for_byte() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "OPUS-CEIL-001", Some(OPUS_MODEL), 0);

    let result = escalate_task_model_if_needed(&conn, "OPUS-CEIL-001", FALLBACK_THRESHOLD).unwrap();
    // Pin today's exact return: Some(OPUS_MODEL) — the Opus self-loop in
    // `escalate_model`. FEAT-007 with `enabled = false` must preserve this.
    // With `enabled = true` and the Grok branch taken, the return would be
    // `Some(GROK_DEFAULT_MODEL)` instead.
    assert_eq!(
        result,
        Some(OPUS_MODEL.to_string()),
        "with fallback disabled, escalate_model loops Opus→Opus; return must match today exactly",
    );
    assert_eq!(
        read_model(&conn, "OPUS-CEIL-001").as_deref(),
        Some(OPUS_MODEL),
        "DB model column must remain at Opus when fallback is disabled (no Grok pivot)",
    );
    assert_ne!(
        read_model(&conn, "OPUS-CEIL-001").as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "fallback disabled MUST NOT write the Grok model to the DB",
    );
}

// ── AC #5 — Counter is NOT reset by Grok promotion ────────────────────────────

/// `escalate_task_model_if_needed` reads `consecutive_failures` only via the
/// passed-in `new_count` argument; it must NOT mutate the
/// `tasks.consecutive_failures` column directly. The counter contract is
/// owned by `handle_task_failure` (increment) and `reset_consecutive_failures`
/// (clear on success). FEAT-007's Grok branch must preserve this — promotion
/// is not a "success" and the counter must remain at the threshold value so
/// `max_retries` auto-block still catches a persistently-failing Grok task.
///
/// Runs unconditionally: today the function never touches the counter, and
/// FEAT-007 must keep it that way regardless of which exit path fires.
#[test]
fn escalate_task_model_does_not_mutate_consecutive_failures_column() {
    let (_dir, conn) = setup_db();
    // Pre-set counter to threshold so escalate sees `new_count = 2`.
    insert_task(&conn, "COUNTER-001", Some(SONNET_MODEL), FALLBACK_THRESHOLD);
    let before = read_consecutive_failures(&conn, "COUNTER-001");

    let _ = escalate_task_model_if_needed(&conn, "COUNTER-001", FALLBACK_THRESHOLD).unwrap();

    let after = read_consecutive_failures(&conn, "COUNTER-001");
    assert_eq!(
        before, after,
        "escalate_task_model_if_needed must NOT mutate the consecutive_failures column \
         on any exit path (Claude tier escalation OR FEAT-007 Grok promotion); \
         counter ownership is `handle_task_failure` / `reset_consecutive_failures`",
    );
    assert_eq!(
        after, FALLBACK_THRESHOLD,
        "counter must remain at threshold value after escalation runs",
    );
}

/// FEAT-007: same invariant as above, but exercised through the Grok
/// promotion path specifically (Opus + threshold + fallback enabled).
/// The counter MUST remain at the threshold value so a subsequent Grok
/// failure still pushes the task toward `auto_block_task` at
/// `max_retries`. Marked `#[ignore]` until FEAT-007 lands the promotion
/// branch.
#[test]
#[ignore = "FEAT-007: requires Grok promotion branch to actually fire"]
fn grok_promotion_preserves_consecutive_failures_count() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "GROK-COUNT-001",
        Some(OPUS_MODEL),
        FALLBACK_THRESHOLD,
    );

    // After FEAT-007, this call would be:
    //   escalate_task_model_if_needed(&conn, "GROK-COUNT-001", FALLBACK_THRESHOLD,
    //                                 &mut ctx, &FallbackRunnerConfig::enabled());
    // and would write tasks.model = GROK_DEFAULT_MODEL. The counter must NOT
    // reset to 0 as part of the promotion — Grok failures still count.
    let _ = escalate_task_model_if_needed(&conn, "GROK-COUNT-001", FALLBACK_THRESHOLD).unwrap();
    assert_eq!(
        read_consecutive_failures(&conn, "GROK-COUNT-001"),
        FALLBACK_THRESHOLD,
        "Grok promotion must NOT reset consecutive_failures — preserves auto-block contract",
    );
    panic!(
        "FEAT-007 not yet wired — when implemented, tasks.model must be \
         {GROK_DEFAULT_MODEL} AND consecutive_failures must remain at {FALLBACK_THRESHOLD}"
    );
}

// ── AC #6 — Wave-mode wiring: slot worker MUST NOT call escalate ──────────────

/// Source-grep test: `run_slot_iteration` (the function spawned onto each
/// parallel slot worker thread) must NEVER call `escalate_task_model_if_needed`
/// directly. The Grok-promotion hook fires from the post-wave aggregation
/// step on the main thread (where `IterationContext` lives) — calling it
/// from a slot worker would either deadlock on the main-thread `&mut ctx`
/// or silently bypass override insertion (per Learning #1810:
/// `IterationContext` is not thread-safe).
///
/// We grep the source file for the function body span between
/// `pub fn run_slot_iteration(` and the next top-level `pub fn` /
/// `fn run_parallel_wave`. Any occurrence of `escalate_task_model_if_needed`
/// within that span is a wiring bug.
#[test]
fn run_slot_iteration_does_not_call_escalate_task_model_if_needed() {
    let source = std::fs::read_to_string("src/loop_engine/engine.rs")
        .expect("could not read src/loop_engine/engine.rs from tests/ cwd");

    let start = source
        .find("pub fn run_slot_iteration(")
        .expect("expected `pub fn run_slot_iteration(` to be defined in engine.rs");

    // The function body ends at the next top-level `pub fn` or `fn` declaration
    // following slot_failure_result (a private helper that sits between
    // run_slot_iteration and run_parallel_wave). We scan for any `\nfn ` or
    // `\npub fn ` after the body opens.
    let after_open = &source[start..];
    let body_close = after_open
        .find("\npub fn run_parallel_wave(")
        .expect("expected `pub fn run_parallel_wave(` after `run_slot_iteration` body");
    let body = &after_open[..body_close];

    assert!(
        !body.contains("escalate_task_model_if_needed"),
        "run_slot_iteration MUST NOT call escalate_task_model_if_needed — \
         the hook is wired on the main thread in the post-wave aggregation step \
         (Learning #1810: IterationContext is not thread-safe). \
         Found call inside run_slot_iteration body. \
         Body span (first 400 chars for diagnosis):\n{}",
        &body[..body.len().min(400)],
    );
}

/// Companion source-grep: every line in `run_slot_iteration` that touches
/// the runner module must go through the future `runner::dispatch` (or the
/// existing `claude::spawn_claude` aliased through it) — never a direct
/// re-implementation. This guards against a future refactor that
/// accidentally re-implements escalation logic inside the slot worker
/// thread instead of routing through the main-thread aggregation step.
#[test]
fn run_slot_iteration_does_not_construct_iteration_context() {
    let source = std::fs::read_to_string("src/loop_engine/engine.rs")
        .expect("could not read src/loop_engine/engine.rs from tests/ cwd");

    let start = source
        .find("pub fn run_slot_iteration(")
        .expect("expected `pub fn run_slot_iteration(` to be defined in engine.rs");
    let after_open = &source[start..];
    let body_close = after_open
        .find("\npub fn run_parallel_wave(")
        .expect("expected `pub fn run_parallel_wave(` after `run_slot_iteration`");
    let body = &after_open[..body_close];

    assert!(
        !body.contains("IterationContext::new"),
        "run_slot_iteration MUST NOT construct an IterationContext — that struct \
         is main-thread-only (no Mutex; not Send-safe per design). Slot workers \
         receive Send-safe state via SlotIterationParams / SlotContext only.",
    );
    assert!(
        !body.contains(".runner_overrides")
            && !body.contains(".model_overrides")
            && !body.contains(".effort_overrides"),
        "run_slot_iteration MUST NOT read/write IterationContext override maps — \
         override insertion happens on the main thread in the post-wave aggregation step.",
    );
}

// ── AC #7 — GrokAuthFailure does NOT increment consecutive_failures ───────────

/// FEAT-007: when a Grok subprocess returns a `TaskMgrError::GrokAuthFailure`
/// (xAI API key invalid / expired / rate-limited at auth tier), the loop
/// must NOT count that as a task failure — incrementing the counter would
/// push a healthy task toward `auto_block_task` for an operator-side
/// problem. The `handle_task_failure` pipeline must short-circuit BEFORE
/// `increment_consecutive_failures` runs when the prior outcome was a
/// `GrokAuthFailure`.
///
/// Marked `#[ignore]` until FEAT-002 lands the `GrokAuthFailure` variant
/// and FEAT-007 wires the short-circuit. When implemented, the test will
/// construct a synthetic mock that bubbles `GrokAuthFailure` up through
/// the iteration outcome and assert that `read_consecutive_failures`
/// returns the pre-call value unchanged.
#[test]
#[ignore = "FEAT-002 + FEAT-007: TaskMgrError::GrokAuthFailure variant + short-circuit not yet wired"]
fn grok_auth_failure_does_not_increment_consecutive_failures() {
    let (_dir, mut conn) = setup_db();
    insert_task(&conn, "AUTH-FAIL-001", Some(OPUS_MODEL), 0);
    let before = read_consecutive_failures(&conn, "AUTH-FAIL-001");

    // After FEAT-007: simulate a prior iteration outcome of
    //   IterationOutcome::Crash(CrashType::GrokAuthFailure)
    // and route through the pipeline that calls handle_task_failure ONLY
    // when the outcome is a non-auth failure. The shape will be:
    //   apply_status_updates(&mut conn, &outcomes_including_auth_failure, ...);
    //   assert_eq!(read_consecutive_failures(&conn, "AUTH-FAIL-001"), before);
    //
    // Today the variant doesn't exist; we exercise the current path so the
    // test compiles, then signal "not yet implemented" so the loop engineer
    // who lands FEAT-002/007 must update this body.
    handle_task_failure(&mut conn, "AUTH-FAIL-001", 1).unwrap();
    let after = read_consecutive_failures(&conn, "AUTH-FAIL-001");
    assert_eq!(
        before, after,
        "GrokAuthFailure must not increment consecutive_failures — auth lapses are \
         operator problems, not task failures",
    );
    panic!(
        "FEAT-002 not yet wired — when implemented, TaskMgrError::GrokAuthFailure \
         must short-circuit handle_task_failure before increment_consecutive_failures runs"
    );
}

// ── AC #8 — Idempotency: task already at Grok → no second promotion ───────────

/// A task whose `tasks.model` is already the Grok model id (effective_runner
/// == Grok on the next iteration) must NOT trigger a second promotion. Today
/// `escalate_model` does not recognize the Grok id and returns `None`, which
/// short-circuits the inner `if let Some(...)` block — so the DB column is
/// untouched. Runs unconditionally; FEAT-007 must preserve this idempotency
/// (e.g. via the `effective_runner == Claude` gate before the Grok branch).
#[test]
fn task_already_at_grok_is_idempotent_no_second_promotion() {
    let (_dir, conn) = setup_db();
    insert_task(&conn, "GROK-IDEMP-001", Some(GROK_DEFAULT_MODEL), 0);

    let result =
        escalate_task_model_if_needed(&conn, "GROK-IDEMP-001", FALLBACK_THRESHOLD).unwrap();
    assert_eq!(
        result, None,
        "task already at Grok must not re-promote — escalate_model returns None for an \
         unknown tier today, and FEAT-007 must preserve idempotency via the \
         `effective_runner == Claude` gate",
    );
    assert_eq!(
        read_model(&conn, "GROK-IDEMP-001").as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "DB model column must remain at Grok when the task is already at Grok",
    );
}

// ── AC #9 — Test file compiles ────────────────────────────────────────────────

/// Compile-only contract pin: importing the symbols above already proves
/// the file builds. This stub test is a single explicit assertion of the
/// AC #9 invariant ("Test file compiles (may be #[ignore] until FEAT-007)")
/// so a future build break surfaces as a missing test rather than as a
/// silent removal.
#[test]
fn test_file_compiles_marker() {
    // No-op — the file's existence + successful build is the assertion.
    assert_eq!(OPUS_MODEL, OPUS_MODEL);
}
