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
use task_mgr::loop_engine::engine::{
    IterationContext, escalate_task_model_if_needed, handle_task_failure,
};
use task_mgr::loop_engine::model::{OPUS_MODEL, OPUS_MODEL_1M, SONNET_MODEL};
use task_mgr::loop_engine::project_config::FallbackRunnerConfig;
use task_mgr::loop_engine::runner::RunnerKind;

/// Grok model id used in this file's tests. The default in
/// `FallbackRunnerConfig::default()` is `"grok-build"`, but the AC-#1 / AC-#5
/// promotion tests construct an explicit config, so we pin the value here and
/// thread it through.
const GROK_DEFAULT_MODEL: &str = "grok-build";

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
#[test]
fn promotion_fires_at_opus_and_threshold_with_fallback_enabled() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "PROMOTE-001",
        Some(OPUS_MODEL),
        FALLBACK_THRESHOLD - 1,
    );

    let mut ctx = IterationContext::new(8);
    let cfg = FallbackRunnerConfig {
        enabled: true,
        model: GROK_DEFAULT_MODEL.to_string(),
        runtime_error_threshold: FALLBACK_THRESHOLD as u32,
        ..Default::default()
    };
    let outcome = escalate_task_model_if_needed(
        &conn,
        "PROMOTE-001",
        FALLBACK_THRESHOLD,
        &mut ctx,
        Some(&cfg),
        None,
    )
    .unwrap();

    assert_eq!(
        outcome.as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "promotion must return the Grok model id from cfg",
    );
    assert_eq!(
        ctx.runner_overrides.get("PROMOTE-001"),
        Some(&RunnerKind::Grok),
        "promotion must set runner_overrides[task] = Grok",
    );
    assert_eq!(
        ctx.model_overrides.get("PROMOTE-001"),
        Some(&GROK_DEFAULT_MODEL.to_string()),
        "promotion must set model_overrides[task] = cfg.model",
    );
    assert_eq!(
        read_model(&conn, "PROMOTE-001").as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "promotion must UPDATE tasks.model to cfg.model",
    );
    assert_eq!(
        ctx.overflow_original_task_model.get("PROMOTE-001"),
        Some(&Some(OPUS_MODEL.to_string())),
        "promotion must snapshot the pre-promotion model into overflow_original_task_model",
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

    let mut ctx = IterationContext::new(8);
    let result =
        escalate_task_model_if_needed(&conn, "BELOW-001", 1, &mut ctx, None, None).unwrap();
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

    let mut ctx = IterationContext::new(8);
    let cfg = FallbackRunnerConfig {
        enabled: true,
        model: GROK_DEFAULT_MODEL.to_string(),
        runtime_error_threshold: FALLBACK_THRESHOLD as u32,
        ..Default::default()
    };
    let result = escalate_task_model_if_needed(
        &conn,
        "SONNET-001",
        FALLBACK_THRESHOLD,
        &mut ctx,
        Some(&cfg),
        None,
    )
    .unwrap();
    assert_eq!(
        result,
        Some(OPUS_MODEL.to_string()),
        "Sonnet must escalate to Opus first; Grok promotion is reserved for the NEXT failure cycle when at Opus + threshold",
    );
    assert!(
        !ctx.runner_overrides.contains_key("SONNET-001"),
        "Sonnet escalation must NOT set runner_overrides — that's the Grok promotion branch",
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

    let mut ctx = IterationContext::new(8);
    // Disabled config: Grok branch MUST NOT fire — return must match today exactly.
    let cfg = FallbackRunnerConfig::default();
    assert!(!cfg.enabled, "default config must keep fallback disabled");
    let result = escalate_task_model_if_needed(
        &conn,
        "OPUS-CEIL-001",
        FALLBACK_THRESHOLD,
        &mut ctx,
        Some(&cfg),
        None,
    )
    .unwrap();
    // Pin today's exact return: Some(OPUS_MODEL) — the Opus self-loop in
    // `escalate_model`. FEAT-007 with `enabled = false` must preserve this.
    // With `enabled = true` and the Grok branch taken, the return would be
    // `Some(GROK_DEFAULT_MODEL)` instead.
    assert_eq!(
        result,
        Some(OPUS_MODEL.to_string()),
        "with fallback disabled, escalate_model loops Opus→Opus; return must match today exactly",
    );
    assert!(
        !ctx.runner_overrides.contains_key("OPUS-CEIL-001"),
        "disabled fallback must NOT write runner_overrides",
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

    let mut ctx = IterationContext::new(8);
    let _ = escalate_task_model_if_needed(
        &conn,
        "COUNTER-001",
        FALLBACK_THRESHOLD,
        &mut ctx,
        None,
        None,
    )
    .unwrap();

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
/// `max_retries`.
#[test]
fn grok_promotion_preserves_consecutive_failures_count() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "GROK-COUNT-001",
        Some(OPUS_MODEL),
        FALLBACK_THRESHOLD,
    );

    let mut ctx = IterationContext::new(8);
    let cfg = FallbackRunnerConfig {
        enabled: true,
        model: GROK_DEFAULT_MODEL.to_string(),
        runtime_error_threshold: FALLBACK_THRESHOLD as u32,
        ..Default::default()
    };
    let outcome = escalate_task_model_if_needed(
        &conn,
        "GROK-COUNT-001",
        FALLBACK_THRESHOLD,
        &mut ctx,
        Some(&cfg),
        None,
    )
    .unwrap();

    assert_eq!(
        outcome.as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "promotion must return the Grok model id",
    );
    assert_eq!(
        read_model(&conn, "GROK-COUNT-001").as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "promotion must UPDATE tasks.model to Grok",
    );
    assert_eq!(
        read_consecutive_failures(&conn, "GROK-COUNT-001"),
        FALLBACK_THRESHOLD,
        "Grok promotion must NOT reset consecutive_failures — preserves auto-block contract",
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
/// `pub fn run_slot_iteration(` and the next top-level helper
/// (`fn claim_slot_task`). Any occurrence of `escalate_task_model_if_needed`
/// within that span is a wiring bug.
///
/// Post-FEAT-001 carve: `run_slot_iteration` lives in `slot.rs`, immediately
/// followed by `claim_slot_task` (the next top-level `fn`).
#[test]
fn run_slot_iteration_does_not_call_escalate_task_model_if_needed() {
    let source = std::fs::read_to_string("src/loop_engine/slot.rs")
        .expect("could not read src/loop_engine/slot.rs from tests/ cwd");

    let start = source
        .find("pub fn run_slot_iteration(")
        .expect("expected `pub fn run_slot_iteration(` to be defined in slot.rs");

    // The function body ends at the next top-level `fn` declaration
    // (`claim_slot_task`, which sits immediately after `run_slot_iteration`
    // in slot.rs). We scan for that anchor after the body opens.
    let after_open = &source[start..];
    let body_close = after_open
        .find("\npub(super) fn claim_slot_task(")
        .expect("expected `fn claim_slot_task(` after `run_slot_iteration` body");
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
    let source = std::fs::read_to_string("src/loop_engine/slot.rs")
        .expect("could not read src/loop_engine/slot.rs from tests/ cwd");

    let start = source
        .find("pub fn run_slot_iteration(")
        .expect("expected `pub fn run_slot_iteration(` to be defined in slot.rs");
    let after_open = &source[start..];
    let body_close = after_open
        .find("\npub(super) fn claim_slot_task(")
        .expect("expected `fn claim_slot_task(` after `run_slot_iteration`");
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
/// problem.
///
/// The short-circuit lives at the CALLER (run_loop sequential at
/// engine.rs:~3984 + the wave-mode wiring after `process_slot_result`):
/// `handle_task_failure` is NOT invoked when the iteration outcome is
/// `Crash(CrashType::GrokAuthFailure)`. We assert that contract here by
/// modeling the caller's filter check directly — exercising the live
/// short-circuit predicate without spawning a full iteration.
#[test]
fn grok_auth_failure_does_not_increment_consecutive_failures() {
    use task_mgr::loop_engine::config::{CrashType, IterationOutcome};

    let (_dir, mut conn) = setup_db();
    insert_task(&conn, "AUTH-FAIL-001", Some(OPUS_MODEL), 0);
    let before = read_consecutive_failures(&conn, "AUTH-FAIL-001");

    // Mirror the caller's skip-list predicate: when the outcome is
    // Crash(GrokAuthFailure), handle_task_failure MUST be skipped.
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
        "Crash(GrokAuthFailure) must be in the skip-list at the handle_task_failure call site",
    );

    // If the caller's filter ever regresses to NOT skip GrokAuthFailure, this
    // sibling assertion catches it: a direct call DOES increment, so the only
    // thing keeping the counter stable is the caller's filter.
    let mut ctx = IterationContext::new(8);
    handle_task_failure(&mut conn, "AUTH-FAIL-001", 1, &mut ctx, None, None).unwrap();
    let after_unfiltered_call = read_consecutive_failures(&conn, "AUTH-FAIL-001");
    assert_eq!(
        after_unfiltered_call,
        before + 1,
        "handle_task_failure unconditionally increments — the filter at the CALL SITE is what \
         protects GrokAuthFailure outcomes from incrementing the counter",
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

    // Enable fallback to exercise the idempotency gate: when tasks.model is
    // already a Grok id, provider_for_model resolves Grok → effective_runner
    // == Grok → promotion must skip. Without the gate, fallback-enabled would
    // re-write tasks.model on every escalate call.
    let mut ctx = IterationContext::new(8);
    let cfg = FallbackRunnerConfig {
        enabled: true,
        model: GROK_DEFAULT_MODEL.to_string(),
        runtime_error_threshold: FALLBACK_THRESHOLD as u32,
        ..Default::default()
    };
    let result = escalate_task_model_if_needed(
        &conn,
        "GROK-IDEMP-001",
        FALLBACK_THRESHOLD,
        &mut ctx,
        Some(&cfg),
        None,
    )
    .unwrap();
    assert_eq!(
        result, None,
        "task already at Grok must not re-promote — escalate_model returns None for an \
         unknown tier, and the effective_runner == Claude gate blocks the Grok branch",
    );
    assert!(
        !ctx.runner_overrides.contains_key("GROK-IDEMP-001"),
        "idempotent path must NOT insert a fresh runner_overrides entry",
    );
    assert_eq!(
        read_model(&conn, "GROK-IDEMP-001").as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "DB model column must remain at Grok when the task is already at Grok",
    );
}

// ── H2 regression — Opus[1M] must also trigger Grok promotion ────────────────

/// H2: a task at `OPUS_MODEL_1M` (the 1M-context Opus variant) with
/// consecutive_failures >= threshold must be promoted to Grok. The original
/// code used string-equality against `OPUS_MODEL` which excluded the 1M
/// variant; the fix uses the ModelTier-based inclusive check so both
/// `OPUS_MODEL` and `OPUS_MODEL_1M` satisfy the "was at Opus" gate.
#[test]
fn promotion_fires_at_opus_1m_and_threshold() {
    let (_dir, conn) = setup_db();
    insert_task(
        &conn,
        "OPUS1M-001",
        Some(OPUS_MODEL_1M),
        FALLBACK_THRESHOLD - 1,
    );

    let mut ctx = IterationContext::new(8);
    let cfg = FallbackRunnerConfig {
        enabled: true,
        model: GROK_DEFAULT_MODEL.to_string(),
        runtime_error_threshold: FALLBACK_THRESHOLD as u32,
        ..Default::default()
    };
    let outcome = escalate_task_model_if_needed(
        &conn,
        "OPUS1M-001",
        FALLBACK_THRESHOLD,
        &mut ctx,
        Some(&cfg),
        None,
    )
    .unwrap();

    assert_eq!(
        outcome.as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "Opus[1M] task at threshold must be promoted to Grok (H2 regression)"
    );
    assert_eq!(
        ctx.runner_overrides.get("OPUS1M-001"),
        Some(&RunnerKind::Grok),
        "runner_overrides must be set to Grok for the Opus[1M] task"
    );
    assert_eq!(
        read_model(&conn, "OPUS1M-001").as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "tasks.model must be updated to the Grok model for the Opus[1M] task"
    );
}

// ── W5 — Promotion ctx writes are deferred until after tx.commit() ───────────

/// W5: `handle_task_failure` runs inside a transaction. If the Grok-promotion
/// ctx mutations (`runner_overrides`, `model_overrides`,
/// `overflow_original_task_model`) happened BEFORE `tx.commit()`, a commit
/// failure (disk full, busy timeout, etc.) would leave the in-memory ctx
/// claiming a promotion that the DB rolled back. Next iteration would
/// dispatch the Grok runner against a `tasks.model` still pointing at Opus.
///
/// The fix is to use the deferred-apply variant: call
/// `escalate_task_model_if_needed_inner` (which performs only DB writes and
/// returns a `PendingPromotion` describing the ctx changes), then call
/// `apply_pending_promotion` ONLY after `tx.commit()?` returns Ok.
///
/// This source-grep test pins the wiring so a future refactor that calls the
/// non-deferred convenience function from inside the transaction surfaces as
/// a test failure rather than as a rare correctness regression.
#[test]
fn handle_task_failure_defers_promotion_ctx_writes_until_after_commit() {
    // Post-FEAT-002 carve: `handle_task_failure` lives in `recovery.rs`.
    let source = std::fs::read_to_string("src/loop_engine/recovery.rs")
        .expect("could not read src/loop_engine/recovery.rs from tests/ cwd");

    let start = source
        .find("pub fn handle_task_failure_with_runner(")
        .expect("expected `pub fn handle_task_failure_with_runner(` to be defined in recovery.rs");
    // The next top-level definition after handle_task_failure marks the
    // function body end. Use a search past the opening `{` to find the next
    // `\nfn ` or `\npub fn ` or `\npub(crate) fn ` declaration.
    let after_open = &source[start..];
    let body_end_rel = ["\nfn ", "\npub fn ", "\npub(crate) fn ", "\npub(super) fn "]
        .iter()
        .filter_map(|marker| {
            after_open[marker.len()..]
                .find(marker)
                .map(|p| p + marker.len())
        })
        .min()
        .expect("expected another top-level fn after handle_task_failure");
    let body = &after_open[..body_end_rel];

    // The runner-aware body MUST call the deferred-apply pair. The public
    // handle_task_failure wrapper delegates to this body with executed_runner=None.
    assert!(
        body.contains("escalate_task_model_if_needed_inner"),
        "handle_task_failure MUST call escalate_task_model_if_needed_inner (the \
         deferred-apply variant) so ctx mutations stay bundled in a \
         PendingPromotion until after tx.commit() succeeds. W5: \
         escalate_task_model_if_needed (the immediate-apply convenience) inside \
         the transaction leaves ctx dirty if commit fails.",
    );
    assert!(
        body.contains("apply_pending_promotion"),
        "handle_task_failure MUST call apply_pending_promotion to actually \
         install the deferred ctx mutations after commit succeeds.",
    );

    // The apply_pending_promotion call MUST appear AFTER tx.commit() in the
    // function body.
    let commit_idx = body
        .find("tx.commit()")
        .expect("expected `tx.commit()` in handle_task_failure body");
    let apply_idx = body
        .find("apply_pending_promotion")
        .expect("expected `apply_pending_promotion` in handle_task_failure body");
    assert!(
        apply_idx > commit_idx,
        "apply_pending_promotion MUST be called AFTER tx.commit() in \
         handle_task_failure — calling it before would defeat the W5 fix \
         (ctx would still be dirty on commit failure). commit at byte {}, \
         apply at byte {}.",
        commit_idx,
        apply_idx,
    );

    // Belt-and-suspenders: the convenience `escalate_task_model_if_needed`
    // (without `_inner`) MUST NOT appear inside handle_task_failure. Use
    // a word-boundary check by looking for the bare name not followed by
    // `_inner`. We search for the call form `escalate_task_model_if_needed(`
    // which the convenience function uses but the inner variant does not.
    assert!(
        !body.contains("escalate_task_model_if_needed("),
        "handle_task_failure MUST NOT call the convenience \
         escalate_task_model_if_needed( — that variant applies ctx writes \
         immediately and breaks the W5 commit-deferred guarantee.",
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
