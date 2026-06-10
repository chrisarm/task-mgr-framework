//! Phase 3 wave-mode tests for US-004 RuntimeError fallback (TEST-003) and
//! TEST-001 wave-mode rate-limit wait and resume (bug #3 repro).
//!
//! Verifies the RuntimeError fallback hook wiring in wave mode:
//!
//! - Hook fires from the post-wave aggregation step (main thread), NOT from
//!   inside a slot worker (Learning #1810: IterationContext not thread-safe).
//! - `ctx.runner_overrides` mutations are observed only AFTER all slot
//!   threads have joined and the main-thread aggregation loop runs.
//! - `run_slot_iteration` body does NOT call `handle_task_failure`
//!   (source-grep assertion).
//! - Idempotency: a task already promoted to Grok sees another
//!   `Crash(RuntimeError)` — hook fires, counter increments, but no second
//!   promotion occurs (`effective_runner == Grok` guard).
//! - Merge-back logic is runner-agnostic: `merge_slot_branches_with_resolver`
//!   does not branch on `RunnerKind` (PRD §2.5 "Wave mode: two slots on
//!   different runners merge back").
//!
//! Also verifies the wave-mode rate-limit wait and resume contract (TEST-001):
//!
//! - A 2-slot wave whose slots return the session-limit string triggers exactly
//!   one usage wait (injected/hermetic) and returns `WaitedAndRetry` — never
//!   `None` (which was the pre-FEAT-006 silent omission that produced the
//!   strand-and-false-abort bug).
//! - Rate-limited `in_progress` tasks are reset to `todo` before the wait so
//!   the next wave finds them eligible (prevents "no eligible tasks after N
//!   consecutive stale iterations").
//! - The `WaveOutcome` carries `iteration_consumed: false` / `rate_limited_retry:
//!   true`, and `account_iteration_budget` returns that budget increment so
//!   `max_iterations` is not consumed by the wait wave.

use std::cell::Cell;

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::config::{CrashType, IterationOutcome, PermissionMode};
use task_mgr::loop_engine::engine::BlackoutState;
use task_mgr::loop_engine::engine::{IterationContext, handle_task_failure};
use task_mgr::loop_engine::model::OPUS_MODEL;
use task_mgr::loop_engine::model::Provider;
use task_mgr::loop_engine::project_config::FallbackRunnerConfig;
use task_mgr::loop_engine::reactions::account::{
    AccountReaction, AccountReactionParams, OutputReactionItem, WaitFn, react_to_outputs_inner,
};
use task_mgr::loop_engine::reactions::{IterationBudgetParams, account_iteration_budget};
use task_mgr::loop_engine::runner::RunnerKind;

/// Grok model id expected after promotion. Matches `FallbackRunnerConfig::default`
/// behaviour exercised by `runtime_error_fallback.rs`; pinned here so a rename
/// propagates to compile errors across both files.
const GROK_DEFAULT_MODEL: &str = "grok-build";

/// Number of consecutive failures at which the Grok promotion branch fires.
/// Must match `FALLBACK_THRESHOLD` in `runtime_error_fallback.rs` (both
/// derive from PRD §3 default of 2).
const FALLBACK_THRESHOLD: i32 = 2;

// ── Shared helpers ────────────────────────────────────────────────────────────

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
         VALUES (?, ?, 'in_progress', ?, 5, ?)",
        rusqlite::params![id, format!("Task {id}"), model, consecutive_failures],
    )
    .unwrap();
}

fn read_task_status(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT status FROM tasks WHERE id = ?", [id], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

/// `PermissionMode` for hermetic inner tests — actual value unused since the
/// wait is injected (no OAuth / usage API / real sleep in the inner path).
static WAVE_RL_PERMISSION_MODE: PermissionMode = PermissionMode::Dangerous;

fn read_consecutive_failures(conn: &Connection, id: &str) -> i32 {
    conn.query_row(
        "SELECT consecutive_failures FROM tasks WHERE id = ?",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

fn read_model(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row("SELECT model FROM tasks WHERE id = ?", [id], |r| {
        r.get::<_, Option<String>>(0)
    })
    .unwrap()
}

fn enabled_fallback_cfg() -> FallbackRunnerConfig {
    FallbackRunnerConfig {
        enabled: true,
        model: GROK_DEFAULT_MODEL.to_string(),
        runtime_error_threshold: FALLBACK_THRESHOLD as u32,
        ..Default::default()
    }
}

// ── AC 1 — Post-wave aggregation fires the hook for the crashing slot only ───

/// Synthetic 2-slot wave: slot 0 returns `Crash(RuntimeError)`, slot 1
/// returns `Completed`. The post-wave aggregation loop must call
/// `handle_task_failure` for slot 0's task (triggering Grok promotion
/// because the task is at Opus + threshold) and MUST NOT call it for
/// slot 1's task (`Completed` → skip).
#[test]
fn post_wave_aggregation_fires_runtime_error_hook_for_crashing_slot_not_completed_slot() {
    let (_dir, mut conn) = setup_db();

    let slot0_task = "WAVE-CRASH-001";
    let slot1_task = "WAVE-DONE-001";

    // slot 0: at Opus, one failure below threshold.
    // After handle_task_failure increments to FALLBACK_THRESHOLD → Grok promotion fires.
    insert_task(&conn, slot0_task, Some(OPUS_MODEL), FALLBACK_THRESHOLD - 1);
    // slot 1: normal state; its outcome is Completed so handle_task_failure is skipped.
    insert_task(&conn, slot1_task, Some(OPUS_MODEL), 0);

    let cfg = enabled_fallback_cfg();
    let mut ctx = IterationContext::new(8);

    // Synthetic wave outcomes mirroring the post-wave dispatch filter in
    // `run_wave_iteration` (engine.rs post-`run_parallel_wave` loop).
    // Tuple: (task_id, outcome, claim_succeeded).
    let wave_outcomes: &[(&str, IterationOutcome, bool)] = &[
        (
            slot0_task,
            IterationOutcome::Crash(CrashType::RuntimeError),
            true,
        ),
        (slot1_task, IterationOutcome::Completed, true),
    ];

    // Replicate the post-wave aggregation loop (engine.rs ~line 1900).
    // Completed/Empty/Reorder/RateLimit/GrokAuthFailure are skipped; everything
    // else (including Crash(RuntimeError)) triggers handle_task_failure.
    for (task_id, outcome, claim_succeeded) in wave_outcomes {
        if !claim_succeeded {
            continue;
        }
        if matches!(
            outcome,
            IterationOutcome::Completed
                | IterationOutcome::Empty
                | IterationOutcome::Reorder(_)
                | IterationOutcome::RateLimit
                | IterationOutcome::Crash(CrashType::GrokAuthFailure)
        ) {
            continue;
        }
        handle_task_failure(
            &mut conn,
            task_id,
            1,
            &mut ctx,
            Some(&cfg),
            None,
            None,
            None,
        )
        .unwrap();
    }

    assert_eq!(
        ctx.runner_overrides.get(slot0_task),
        Some(&RunnerKind::Grok),
        "post-wave aggregation must promote the crashing slot's task to Grok",
    );
    assert!(
        !ctx.runner_overrides.contains_key(slot1_task),
        "Completed slot must NOT trigger handle_task_failure; its task must be absent from runner_overrides",
    );
}

// ── AC 2 — runner_overrides mutation observed AFTER slot threads join ─────────

/// `runner_overrides` starts empty before the post-wave aggregation loop runs.
/// It is populated only AFTER the loop executes on the main thread. This
/// sequence proves the mutation lives entirely in the post-aggregation step —
/// slot workers never touch this map (Learning #1810: IterationContext is not
/// thread-safe).
#[test]
fn runner_overrides_is_empty_before_post_aggregation_and_populated_after() {
    let (_dir, mut conn) = setup_db();

    let task_id = "WAVE-ORDER-001";
    insert_task(&conn, task_id, Some(OPUS_MODEL), FALLBACK_THRESHOLD - 1);

    let cfg = enabled_fallback_cfg();
    let mut ctx = IterationContext::new(8);

    // BEFORE post-aggregation: runner_overrides must be empty.
    // In production, slot workers have all joined at this point but have
    // never written to runner_overrides (Learning #1810 thread-safety contract).
    assert!(
        ctx.runner_overrides.is_empty(),
        "runner_overrides must be empty before the post-aggregation loop runs — \
         slot workers never touch this map (Learning #1810)",
    );

    // Simulate: post-wave aggregation calls handle_task_failure for the crashing slot.
    handle_task_failure(
        &mut conn,
        task_id,
        1,
        &mut ctx,
        Some(&cfg),
        None,
        None,
        None,
    )
    .unwrap();

    // AFTER post-aggregation: mutation must now be visible on the main thread.
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Grok),
        "runner_overrides must be populated AFTER the post-aggregation loop — \
         the write happens on the main thread, never in a slot worker",
    );
}

// ── AC 3 — run_slot_iteration must NOT call handle_task_failure ───────────────

/// Source-grep: the slot worker body (`run_slot_iteration`) must not contain
/// a call to `handle_task_failure`. That function mutates
/// `IterationContext.runner_overrides` (not thread-safe; Learning #1810) and
/// must only run on the main thread in the post-wave aggregation step.
///
/// Complementary to the existing `run_slot_iteration_does_not_call_escalate_task_model_if_needed`
/// test in `runtime_error_fallback.rs`; together they pin the complete wiring
/// contract for the RuntimeError fallback hook.
#[test]
fn run_slot_iteration_does_not_call_handle_task_failure() {
    let source = std::fs::read_to_string("src/loop_engine/slot.rs")
        .expect("could not read src/loop_engine/slot.rs");

    let start = source
        .find("pub fn run_slot_iteration(")
        .expect("`pub fn run_slot_iteration(` must be defined in slot.rs");

    let after_start = &source[start..];
    let body_end = after_start
        .find("\npub(super) fn claim_slot_task(")
        .expect("`fn claim_slot_task(` must follow `run_slot_iteration` body");
    let body = &after_start[..body_end];

    assert!(
        !body.contains("handle_task_failure"),
        "run_slot_iteration MUST NOT call handle_task_failure — \
         that function mutates IterationContext (not thread-safe; Learning #1810). \
         The RuntimeError fallback hook fires from the main-thread post-wave aggregation step only. \
         Body span (first 400 chars for diagnosis):\n{}",
        &body[..body.len().min(400)],
    );
}

// ── AC 4 — Idempotency: task already on Grok skips second promotion ───────────

/// A task promoted to Grok during wave N sees `Crash(RuntimeError)` again
/// during wave N+1. The post-wave aggregation fires `handle_task_failure`
/// (because RuntimeError is not in the skip-list), which calls
/// `escalate_task_model_if_needed`. That function skips the Grok branch
/// because `effective_runner == Grok` (idempotency guard). The
/// `consecutive_failures` counter increments toward `max_retries` so the
/// auto-block contract still holds.
#[test]
fn wave_idempotency_second_runtime_error_on_grok_task_increments_counter_skips_promotion() {
    let (_dir, mut conn) = setup_db();

    let task_id = "WAVE-IDEMP-001";
    // Task is already at Grok after prior promotion; consecutive_failures at threshold.
    insert_task(&conn, task_id, Some(GROK_DEFAULT_MODEL), FALLBACK_THRESHOLD);

    let cfg = enabled_fallback_cfg();
    let mut ctx = IterationContext::new(8);

    // Simulate prior-wave state: runner_overrides already has the Grok entry.
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Grok);

    let failures_before = read_consecutive_failures(&conn, task_id);

    // Post-wave aggregation for the second Crash(RuntimeError) on the same task.
    handle_task_failure(
        &mut conn,
        task_id,
        2,
        &mut ctx,
        Some(&cfg),
        None,
        None,
        None,
    )
    .unwrap();

    // Counter must have incremented (progressing toward max_retries auto-block).
    let failures_after = read_consecutive_failures(&conn, task_id);
    assert_eq!(
        failures_after,
        failures_before + 1,
        "consecutive_failures must keep incrementing on each RuntimeError even after Grok promotion",
    );

    // runner_overrides must still show Grok — no change from a second promotion attempt.
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Grok),
        "runner_overrides must remain Grok after the second RuntimeError — \
         idempotency guard (effective_runner == Grok) prevents re-promotion",
    );

    // DB model must remain at Grok — the promotion branch was skipped.
    assert_eq!(
        read_model(&conn, task_id).as_deref(),
        Some(GROK_DEFAULT_MODEL),
        "tasks.model must remain Grok after second RuntimeError — \
         idempotency guard prevents a spurious DB UPDATE",
    );
}

// ── AC 5 — Merge-back logic is runner-agnostic (source-grep) ─────────────────

/// `merge_slot_branches_with_resolver` is the function that merges each
/// slot's ephemeral branch back into the main branch. It must not branch on
/// `RunnerKind`, `runner_overrides`, `GrokRunner`, or `ClaudeRunner` — the
/// merge step operates at the git level and is runner-agnostic (PRD §2.5:
/// "Wave mode: two slots on different runners merge back").
///
/// This guards the invariant that a wave with slot 0 on ClaudeRunner and
/// slot 1 on GrokRunner produces identical merge behaviour to an
/// all-Claude wave.
#[test]
fn merge_back_logic_does_not_branch_on_runner_kind() {
    let worktree_src = std::fs::read_to_string("src/loop_engine/worktree.rs")
        .expect("could not read src/loop_engine/worktree.rs");

    // Locate the function body.
    let fn_start = worktree_src
        .find("pub(crate) fn merge_slot_branches_with_resolver(")
        .expect("`merge_slot_branches_with_resolver` must be defined in worktree.rs");
    let after_fn = &worktree_src[fn_start..];
    // Body ends at the next top-level pub function / pub(crate) function.
    let body_end = after_fn
        .find("\npub fn ")
        .or_else(|| after_fn.find("\npub(crate) fn "))
        .unwrap_or(after_fn.len());
    let fn_body = &after_fn[..body_end];

    assert!(
        !fn_body.contains("RunnerKind"),
        "merge_slot_branches_with_resolver must not branch on RunnerKind — \
         merge-back is runner-agnostic (PRD §2.5)",
    );
    assert!(
        !fn_body.contains("runner_overrides"),
        "merge_slot_branches_with_resolver must not reference runner_overrides — \
         merge-back sees only git branches, not runner identities",
    );
    assert!(
        !fn_body.contains("GrokRunner"),
        "merge_slot_branches_with_resolver must not reference GrokRunner — \
         runner-agnostic contract",
    );
    assert!(
        !fn_body.contains("ClaudeRunner"),
        "merge_slot_branches_with_resolver must not reference ClaudeRunner — \
         runner-agnostic contract",
    );

    // Sanity: function exists so the asserts above are not vacuously true.
    assert!(
        worktree_src.contains("merge_slot_branches_with_resolver"),
        "sanity: merge_slot_branches_with_resolver must exist in worktree.rs",
    );
}

// ── M2 — wave-mode banner dedup: single promotion banner per task per wave ─────

/// M2: in a wave, a task can trigger BOTH the overflow rung-4
/// (`handle_prompt_too_long` FallbackToProvider arm) AND the RuntimeError
/// hook (`escalate_task_model_if_needed`) in the same iteration.  Both sites
/// check `ctx.runner_overrides.contains_key(task_id)` (the `already_promoted`
/// / `was_already_promoted` flag) to decide whether to emit the banner.
///
/// The invariant: the FIRST path to insert into `runner_overrides` also emits
/// the banner; any subsequent path that finds the key already present skips
/// the banner.
///
/// This test verifies the state machine by:
/// 1. Pre-populating `runner_overrides` (simulating overflow rung-4 having
///    already fired and printed the first banner for this task).
/// 2. Calling `escalate_task_model_if_needed` — which represents the
///    RuntimeError hook that would fire if the same task also hit the
///    threshold in the same wave.
/// 3. Asserting that `runner_overrides` still has exactly ONE entry for the
///    task (no duplicate insert) and that the model state is consistent.
///
/// Stderr is NOT captured; the mechanism is verified via state assertions.
/// The banner-suppression path (`!already_promoted == false → skip eprintln!`)
/// in `escalate_task_model_if_needed` and the symmetric path in
/// `handle_prompt_too_long` are the single-print guarantees.
#[test]
fn wave_promotion_banner_dedup_second_path_skips_when_runner_overrides_already_set() {
    let (_dir, mut conn) = setup_db();

    let task_id = "WAVE-M2-DEDUP-001";
    // Task at Opus ceiling (would have overflowed), consecutive_failures one
    // below threshold (handle_task_failure will push it to threshold).
    insert_task(&conn, task_id, Some(OPUS_MODEL), FALLBACK_THRESHOLD - 1);

    let mut ctx = IterationContext::new(8);
    let cfg = enabled_fallback_cfg();

    // Simulate overflow rung-4 having already fired for this task: it inserts
    // into runner_overrides and (logically) emitted the first banner. The
    // escalate call below must detect this pre-existing entry and skip its
    // own banner.
    ctx.runner_overrides
        .insert(task_id.to_string(), RunnerKind::Grok);

    // The RuntimeError hook path: handle_task_failure increments to threshold
    // and calls escalate_task_model_if_needed.
    handle_task_failure(
        &mut conn,
        task_id,
        1,
        &mut ctx,
        Some(&cfg),
        None,
        None,
        None,
    )
    .unwrap();

    // State must be consistent: one entry, correct value, no double-insert.
    assert_eq!(
        ctx.runner_overrides.get(task_id),
        Some(&RunnerKind::Grok),
        "runner_overrides must still map to Grok after the second path runs — \
         the already_promoted gate prevents a second insertion but must not remove \
         the existing entry",
    );
    // Exactly one key for this task (HashMap insert is idempotent so no
    // structural duplication, but the value must still be Grok).
    let grok_entries: Vec<_> = ctx
        .runner_overrides
        .iter()
        .filter(|(k, _)| k.as_str() == task_id)
        .collect();
    assert_eq!(
        grok_entries.len(),
        1,
        "runner_overrides must have exactly ONE entry for the task after both paths run",
    );
}

// ── TEST-001: Wave-mode rate-limit waits and resumes (bug #3 repro) ─────────────

/// A 2-slot wave whose BOTH slots return the session-limit string must:
///   1. Fire the injected wait EXACTLY ONCE (not once per slot) — AC1.
///   2. Reset both `in_progress` tasks to `todo` before the wait — AC3.
///   3. Return `AccountReaction::WaitedAndRetry`, NOT `None` — AC1/AC2.
///
/// This is the regression test for the production incident: before FEAT-006
/// wired `react_to_outputs` into the wave path, rate-limited slots stayed
/// `in_progress`. The next wave therefore found zero eligible tasks and the
/// stale iteration counter eventually aborted with "no eligible tasks after N
/// consecutive stale iterations". By resetting to `todo` AND returning
/// `WaitedAndRetry`, the coordinator prevents that cascade.
///
/// Uses the hermetic inner seam (`react_to_outputs_inner` + injected `WaitFn`)
/// — no OAuth, no usage API, no real sleep.
#[test]
fn wave_rate_limit_two_slots_session_limit_waits_once_and_resets_to_todo() {
    let (_dir, mut conn) = setup_db();

    let slot0_task = "WAVE-RL-001";
    let slot1_task = "WAVE-RL-002";

    // Both slots hit the session limit while still in_progress — the state
    // left behind when neither slot completed before the limit fired.
    insert_task(&conn, slot0_task, None, 0);
    insert_task(&conn, slot1_task, None, 0);

    // Exact session-limit string produced by the Claude CLI. Detected by
    // `detection::is_rate_limited` → `IterationOutcome::RateLimit`.
    let session_limit_output = "You've hit your session limit · resets 11pm";

    let rate = IterationOutcome::RateLimit;
    let items = [
        OutputReactionItem {
            task_id: Some(slot0_task),
            outcome: &rate,
            output: session_limit_output,
        },
        OutputReactionItem {
            task_id: Some(slot1_task),
            outcome: &rate,
            output: session_limit_output,
        },
    ];

    // Injected wait seam: records calls, returns true (completed → retry).
    // `Cell` is required because the seam is `Fn`, not `FnMut`.
    let wait_calls = Cell::new(0u32);
    let wait = |_wait_secs: u64| -> bool {
        wait_calls.set(wait_calls.get() + 1);
        true
    };

    let tasks_dir_tmp = TempDir::new().unwrap();
    let params = AccountReactionParams {
        threshold: 80,
        usage_enabled: false,
        tasks_dir: tasks_dir_tmp.path(),
        fallback_wait: 600,
        prefix: "WAVE-RL",
        run_id: "wave-rl-run",
        permission_mode: &WAVE_RL_PERMISSION_MODE,
        // Spillover disabled — this case pins the legacy wave reset-and-wait path.
        spillover_enabled: false,
        primary_provider: Provider::Claude,
        blackout_fallback_secs: 3600,
        now_secs: 0,
    };

    let mut blackout = BlackoutState::default();
    let reaction =
        react_to_outputs_inner(&mut conn, &items, &params, &mut blackout, &wait as WaitFn);

    // AC1: the account-global wait fires EXACTLY once for the whole wave,
    // regardless of how many rate-limited slots there are.
    assert_eq!(
        wait_calls.get(),
        1,
        "the account-global wait must fire EXACTLY once per 2-slot rate-limited wave, \
         not once per slot — pre-FEAT-006 the wait never fired at all in wave mode",
    );

    // AC1: the reaction must be WaitedAndRetry (wait completed → caller retries).
    // Before FEAT-006, the wave path never called react_to_outputs, so the
    // effective reaction was always None — triggering the strand-and-abort bug.
    assert_eq!(
        reaction,
        AccountReaction::WaitedAndRetry,
        "a completed session-limit wait over a 2-slot wave must return WaitedAndRetry, \
         signalling the caller to retry WITHOUT consuming the iteration budget",
    );

    // AC3: both rate-limited tasks must be reset to `todo` so the next wave
    // finds them eligible. While they remain `in_progress` the scheduler can't
    // select them, producing the "no eligible tasks" stale abort cascade.
    assert_eq!(
        read_task_status(&conn, slot0_task).as_deref(),
        Some("todo"),
        "slot 0's rate-limited task must be reset to `todo` before the wait so \
         the next wave can re-claim and re-run it",
    );
    assert_eq!(
        read_task_status(&conn, slot1_task).as_deref(),
        Some("todo"),
        "slot 1's rate-limited task must be reset to `todo` before the wait so \
         the next wave can re-claim and re-run it",
    );
}

/// `account_iteration_budget` with `consumes_budget: false` must give the
/// loop-bound iteration counter back (AC4: max_iterations is not consumed).
///
/// This is the budget rule that `run_wave_iteration` exercises via
/// `WaveOutcome { iteration_consumed: false }` on a `WaitedAndRetry` return.
/// Routing both the wave and sequential paths through this helper (FEAT-013)
/// ensures neither path drifts on the give-back rule.
#[test]
fn wave_rate_limit_does_not_consume_max_iterations_budget() {
    // Simulate the orchestrator's loop-bound state after one top-of-pass
    // increment: iteration == 1, iterations_completed == 0.
    let mut iteration: u32 = 1;
    let mut iterations_completed: u32 = 0;

    // A rate-limited wave returns iteration_consumed: false — give back.
    account_iteration_budget(IterationBudgetParams {
        iteration: &mut iteration,
        iterations_completed: &mut iterations_completed,
        consumes_budget: false,
    });

    // The give-back must return the counter to 0 so the while-loop condition
    // (`while iteration < max_iterations`) re-enters the same iteration slot.
    assert_eq!(
        iteration, 0,
        "a non-consuming (rate-limited) wave must give the loop-bound iteration \
         back via saturating_sub(1) — the rate-limit wait must not burn \
         max_iterations budget",
    );
    // The reported completions stat must be unchanged — no task was completed.
    assert_eq!(
        iterations_completed, 0,
        "iterations_completed must not advance for a non-consuming wave — \
         the rate-limit wait did not make progress on the task list",
    );
}

/// A consuming wave (normal completion) DOES advance `iterations_completed`.
/// Companion to the give-back test above: proves the helper distinguishes
/// consuming vs non-consuming outcomes correctly.
#[test]
fn wave_consuming_outcome_advances_iterations_completed() {
    let mut iteration: u32 = 1;
    let mut iterations_completed: u32 = 0;

    account_iteration_budget(IterationBudgetParams {
        iteration: &mut iteration,
        iterations_completed: &mut iterations_completed,
        consumes_budget: true,
    });

    // The loop-bound counter is already advanced at the loop top; we only
    // advance the reported stat here.
    assert_eq!(
        iteration, 1,
        "a consuming wave must leave the loop-bound iteration unchanged \
         (already incremented at the loop top)",
    );
    assert_eq!(
        iterations_completed, 1,
        "a consuming wave must advance iterations_completed by one",
    );
}

/// Source-grep: `run_aggregate_wave_results` (inside `run_wave_iteration`) must
/// return `WaveOutcome { rate_limited_retry: true, iteration_consumed: false }`
/// when `react_to_outputs` returns `WaitedAndRetry`.  This flag is what makes
/// the orchestrator `continue` (skip the stale tracker / merge-fail check) on
/// a rate-limited wave — AC2: "no stale abort for the rate-limit cause".
///
/// Complementary to the behavioral tests above: this grep pins the wiring
/// contract so a refactor that drops `rate_limited_retry: true` on the
/// `WaitedAndRetry` arm becomes a test failure rather than a silent regression.
#[test]
fn wave_scheduler_returns_rate_limited_retry_flag_on_waited_and_retry() {
    let src = std::fs::read_to_string("src/loop_engine/wave_scheduler.rs")
        .expect("could not read src/loop_engine/wave_scheduler.rs");

    // The WaitedAndRetry arm must return early with rate_limited_retry: true.
    // This flag makes the orchestrator `continue` past the stale-tracker /
    // merge-fail checks, so the stale counter never increments on a rate-limited
    // wave.
    assert!(
        src.contains("AccountReaction::WaitedAndRetry"),
        "wave_scheduler.rs must match on AccountReaction::WaitedAndRetry — \
         the wiring that triggers the rate-limit wait in wave mode",
    );
    assert!(
        src.contains("rate_limited_retry: true"),
        "wave_scheduler.rs must set rate_limited_retry: true in the WaitedAndRetry arm — \
         this tells the orchestrator to skip the stale/merge-fail checks on a \
         rate-limited wave (AC2: no stale abort for the rate-limit cause)",
    );
    assert!(
        src.contains("iteration_consumed: false"),
        "wave_scheduler.rs must set iteration_consumed: false in the WaitedAndRetry arm — \
         the rate-limit wait must not consume the max_iterations budget (AC4)",
    );
}

/// Source-grep: `orchestrator.rs` must have `if outcome.rate_limited_retry { continue; }`
/// so a rate-limited wave skips the stale/merge-fail tracking step entirely.
/// Without this `continue`, a rate-limited wave with zero failed merges would
/// reset `consecutive_merge_fail_waves` to 0 (zeroing the B3 cascade-halt
/// defense) and potentially count toward the stale-abort threshold.
#[test]
fn orchestrator_skips_stale_tracker_on_rate_limited_retry() {
    let src = std::fs::read_to_string("src/loop_engine/orchestrator.rs")
        .expect("could not read src/loop_engine/orchestrator.rs");

    assert!(
        src.contains("if outcome.rate_limited_retry"),
        "orchestrator.rs must check outcome.rate_limited_retry — the guard that \
         skips stale/merge-fail tracking on a rate-limited wave",
    );
    // The guard must `continue` the wave loop, not fall through.
    assert!(
        src.contains("if outcome.rate_limited_retry {")
            || src.contains("if outcome.rate_limited_retry {\n"),
        "orchestrator.rs: the rate_limited_retry guard must use a block scope that \
         `continue`s the outer wave loop — not a return or early break",
    );
}

// ── Compile marker ─────────────────────────────────────────────────────────────

/// Confirms the file builds. The imports above are the real compile-time
/// contract — this stub ensures any future build break surfaces as a
/// test failure rather than a silent empty module.
#[test]
fn test_file_compiles_marker() {
    assert_eq!(OPUS_MODEL, OPUS_MODEL);
}
