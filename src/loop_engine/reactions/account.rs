//! Account-global usage gate (CONTRACT-001 scaffold).
//!
//! The pre-dispatch usage/rate-limit gate is an *account-global* reaction: it
//! reflects the shared API account state, not per-task state, so it must fire
//! **exactly once per wave** (not once per slot). The sequential path already
//! calls `usage::check_and_wait` at `iteration.rs` ~L116; the wave path has no
//! call site today — that omission is the strand-bug this framework fixes.
//! FEAT (003/006) wires this coordinator into the wave preflight so a
//! rate-limited account waits once before the whole wave dispatches.

use std::path::Path;

use rusqlite::Connection;

use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::config::{IterationOutcome, PermissionMode};
use crate::loop_engine::recovery::probe_rate_limit_lifted;
use crate::loop_engine::usage::{self, UsageCheckResult};

/// Inputs to [`account_usage_gate`]. Destructured exhaustively (no `..`).
#[allow(dead_code)] // constructed by FEAT-003/FEAT-006 wiring; scaffold under CONTRACT-001
pub(crate) struct AccountUsageGateParams<'a> {
    pub threshold: u8,
    pub tasks_dir: &'a Path,
    pub fallback_wait: u64,
}

/// Account-global usage gate. Fires the shared usage check + wait once.
#[allow(dead_code)] // wired once-per-wave (and once-per-iteration sequentially) by FEAT-003/FEAT-006
pub(crate) fn account_usage_gate(params: AccountUsageGateParams<'_>) -> UsageCheckResult {
    let AccountUsageGateParams {
        threshold,
        tasks_dir,
        fallback_wait,
    } = params;

    usage::check_and_wait(threshold, tasks_dir, fallback_wait)
}

// ---------------------------------------------------------------------------
// Post-output rate-limit reaction (#6) — converged by FEAT-006.
//
// This is the account-global *post-output* rate-limit wait. Unlike
// `account_usage_gate` (which runs BEFORE dispatch), `react_to_outputs` runs
// AFTER Claude returns and keys off the captured output: if any item in the
// slice reports a rate/session limit, the affected `in_progress` task(s) are
// reset to `todo` and the usage wait fires **exactly once per wave** (never
// once per rate-limited slot).
//
// Both reactions are account-global (they reflect shared API account state,
// not per-task state), which is why this coordinator lives in `account.rs`
// alongside `account_usage_gate` — NOTE: the CONTRACT-001 `mod.rs` table
// listed a `post_output::react_to_outputs` scaffold; FEAT-006 / TEST-INIT-001
// relocate the converged reaction here. The bodies below are TDD scaffolds
// (`unimplemented!`): TEST-INIT-001 pins the contract via the ignored tests in
// `tests/reaction_parity.rs`; FEAT-006 fills in the bodies and un-ignores them.
// ---------------------------------------------------------------------------

/// Outcome of the once-per-wave account rate-limit reaction.
#[derive(Debug, PartialEq, Eq)]
pub enum AccountReaction {
    /// No `RateLimit` item in the slice. Nothing waited; ZERO DB writes.
    None,
    /// A rate-limit was detected: the affected `in_progress` task(s) were reset
    /// to `todo` and the usage wait completed. The caller retries the
    /// wave/iteration WITHOUT consuming the iteration budget (FEAT-006 B2), and
    /// MUST NOT zero `ctx.consecutive_merge_fail_waves` (FEAT-006 B3).
    WaitedAndRetry,
    /// The usage wait was interrupted by a `.stop` signal. The caller stops
    /// (sequential: `should_stop` early return; wave: terminal exit 130).
    Stop,
}

/// One per-slot (or the single sequential) output the reaction inspects.
///
/// Built from `SlotResult.iteration_result.{task_id, outcome, output}` in the
/// wave path (after filtering `claim_succeeded`), or the lone `IterationResult`
/// in the sequential path. Production-shaped — the tests construct these from
/// real [`IterationOutcome`] values and real `tasks` rows, never hand-built
/// maps.
pub struct OutputReactionItem<'a> {
    /// The claimed task id, if any (`None` mirrors a slot with no claimed task).
    pub task_id: Option<&'a str>,
    /// The classified iteration outcome for this item.
    pub outcome: &'a IterationOutcome,
    /// The captured Claude output for this item (parsed for a reset timestamp).
    pub output: &'a str,
}

/// Injected wait seam (inner/outer split, mirrors
/// `auto_review::{maybe_fire, maybe_fire_inner}`).
///
/// Called **at most once** per [`react_to_outputs_inner`] invocation, with the
/// computed wait-seconds (`parse_reset_from_output(first_rate_limited_output)
/// .unwrap_or(fallback_wait)`). Returns `true` when the wait completed (the
/// caller should retry), `false` when interrupted by a `.stop` signal.
///
/// Production builds this from `usage::check_and_wait` (pre-check) +
/// `usage::wait_for_usage_reset` (with `probe_rate_limit_lifted`); tests inject
/// a counting closure so they are hermetic (no OAuth, no usage API, no real
/// sleep). A type alias keeps `clippy::type_complexity` quiet.
pub type WaitFn<'f> = &'f dyn Fn(u64) -> bool;

/// Inputs to [`react_to_outputs`] / [`react_to_outputs_inner`]. Destructured
/// exhaustively (no `..`) by the FEAT-006 body — the single-home parity lock.
pub struct AccountReactionParams<'a> {
    /// Usage-API percentage threshold (production wait path only).
    pub threshold: u8,
    /// Whether the usage API pre-check is enabled (production wait path only).
    pub usage_enabled: bool,
    /// Loop tasks dir — `.stop`-signal polling + usage wait.
    pub tasks_dir: &'a Path,
    /// Wait seconds to use when the reset timestamp can't be parsed.
    pub fallback_wait: u64,
    /// PRD prefix scoping the `in_progress` reset
    /// (`TaskLifecycle::recover_in_progress_for_prefix`). An empty string maps
    /// to `None` (reset every `in_progress` row regardless of prefix).
    pub prefix: &'a str,
    /// Active run id for `TaskLifecycle::with_run`.
    pub run_id: &'a str,
    /// Permission mode forwarded to the early-lift probe
    /// (`probe_rate_limit_lifted`) in the production wait closure. Unused by
    /// the hermetic [`react_to_outputs_inner`] (the wait is injected there).
    pub permission_mode: &'a PermissionMode,
}

/// Post-output rate-limit reaction (production entry point). Builds the real
/// usage-wait closure and delegates to [`react_to_outputs_inner`].
///
/// The injected wait mirrors the pre-convergence sequential logic: try the
/// usage API first (when `usage_enabled`), then fall back to the
/// output-parsed reset wait with an early-lift probe.
pub fn react_to_outputs(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &AccountReactionParams<'_>,
) -> AccountReaction {
    // Exhaustive destructure (no `..`) — the single-home parity lock. Adding a
    // field to `AccountReactionParams` forces this coordinator to account for
    // it. All fields are `Copy`, so the `&Struct { .. }` pattern copies each
    // out by value while leaving `params` borrowed for the inner delegation.
    let &AccountReactionParams {
        threshold,
        usage_enabled,
        tasks_dir,
        fallback_wait,
        prefix: _,
        run_id: _,
        permission_mode,
    } = params;

    let wait = |wait_secs: u64| -> bool {
        // Try the usage API first (when enabled). It computes its own wait
        // internally, so the `wait_secs` arg is only consumed by the fallback.
        if usage_enabled {
            match usage::check_and_wait(threshold, tasks_dir, fallback_wait) {
                UsageCheckResult::StopSignaled => return false,
                UsageCheckResult::WaitedAndReset => return true,
                // Skipped / BelowThreshold / ApiError — the API did not wait;
                // fall through to the output-parsed reset wait.
                _ => {}
            }
        }
        let probe = || probe_rate_limit_lifted(permission_mode);
        #[allow(deprecated)] // single legitimate caller of the relocated leaf
        usage::wait_for_usage_reset(wait_secs, tasks_dir, fallback_wait, Some(&probe))
    };

    react_to_outputs_inner(conn, items, params, &wait)
}

/// Hermetic core of the post-output rate-limit reaction. Detects `RateLimit`
/// across `items`, resets the affected `in_progress` task(s) to `todo`, and
/// fires `wait` **exactly once**, then maps the result to an [`AccountReaction`].
///
/// The contract is pinned by the parity tests in `tests/reaction_parity.rs`.
pub fn react_to_outputs_inner(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &AccountReactionParams<'_>,
    wait: WaitFn<'_>,
) -> AccountReaction {
    // No `RateLimit` item ⇒ nothing to do. ZERO DB writes, no wait.
    let Some(first_rate_limited) = items
        .iter()
        .find(|item| *item.outcome == IterationOutcome::RateLimit)
    else {
        return AccountReaction::None;
    };

    // A rate/session limit hit the shared account mid-wave. Reset every
    // `in_progress` row under this PRD prefix back to `todo` so the next
    // wave/iteration re-runs them. The `status = 'in_progress'` guard inside
    // `recover_in_progress_for_prefix` means slots that already completed THIS
    // wave (flipped to `done` by `process_slot_result`) are never clobbered
    // (FEAT-006 B1).
    let prefix = if params.prefix.is_empty() {
        None
    } else {
        Some(params.prefix)
    };
    if let Err(e) =
        TaskLifecycle::with_run(conn, params.run_id).recover_in_progress_for_prefix(prefix)
    {
        eprintln!(
            "Warning: failed to reset in_progress tasks after rate limit: {}",
            e
        );
    }

    // Compute the wait once from the FIRST rate-limited output, then fire the
    // injected wait seam EXACTLY once for the whole wave — never once per
    // rate-limited slot.
    #[allow(deprecated)] // single legitimate caller of the relocated leaf
    let wait_secs =
        usage::parse_reset_from_output(first_rate_limited.output).unwrap_or(params.fallback_wait);

    if wait(wait_secs) {
        AccountReaction::WaitedAndRetry
    } else {
        AccountReaction::Stop
    }
}
