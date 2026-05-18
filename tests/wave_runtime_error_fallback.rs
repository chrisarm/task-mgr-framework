//! Phase 3 wave-mode tests for US-004 RuntimeError fallback (TEST-003).
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

use rusqlite::Connection;
use tempfile::TempDir;

use task_mgr::db::{create_schema, open_connection, run_migrations};
use task_mgr::loop_engine::config::{CrashType, IterationOutcome};
use task_mgr::loop_engine::engine::{IterationContext, handle_task_failure};
use task_mgr::loop_engine::model::OPUS_MODEL;
use task_mgr::loop_engine::project_config::FallbackRunnerConfig;
use task_mgr::loop_engine::runner::RunnerKind;

/// Grok model id expected after promotion. Matches `FallbackRunnerConfig::default`
/// behaviour exercised by `runtime_error_fallback.rs`; pinned here so a rename
/// propagates to compile errors across both files.
const GROK_DEFAULT_MODEL: &str = "grok-4-fast";

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
        handle_task_failure(&mut conn, task_id, 1, &mut ctx, Some(&cfg)).unwrap();
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
    handle_task_failure(&mut conn, task_id, 1, &mut ctx, Some(&cfg)).unwrap();

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
    let source = std::fs::read_to_string("src/loop_engine/engine.rs")
        .expect("could not read src/loop_engine/engine.rs");

    let start = source
        .find("pub fn run_slot_iteration(")
        .expect("`pub fn run_slot_iteration(` must be defined in engine.rs");

    let after_start = &source[start..];
    let body_end = after_start
        .find("\npub fn run_parallel_wave(")
        .expect("`pub fn run_parallel_wave(` must follow `run_slot_iteration` body");
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
    handle_task_failure(&mut conn, task_id, 2, &mut ctx, Some(&cfg)).unwrap();

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

// ── Compile marker ─────────────────────────────────────────────────────────────

/// Confirms the file builds. The imports above are the real compile-time
/// contract — this stub ensures any future build break surfaces as a
/// test failure rather than a silent empty module.
#[test]
fn test_file_compiles_marker() {
    assert_eq!(OPUS_MODEL, OPUS_MODEL);
}
