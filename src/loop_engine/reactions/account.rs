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
use std::thread;
use std::time::Duration;

use rusqlite::Connection;

use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::config::{IterationOutcome, PermissionMode};
use crate::loop_engine::recovery::probe_rate_limit_lifted;
use crate::loop_engine::usage::{self, UsageCheckResult};
use crate::loop_engine::{display, signals};

/// Inputs to [`account_usage_gate`] / [`account_usage_gate_inner`].
/// Destructured exhaustively (no `..`) by the FEAT-003 body — the single-home
/// parity lock.
///
/// `account` is `pub` so this is reachable from the integration parity harness
/// (`tests/reaction_parity.rs`).
pub struct AccountUsageGateParams<'a> {
    /// Usage-API percentage threshold above which the gate waits.
    pub threshold: u8,
    /// Loop tasks dir — `.stop`-signal polling during the wait.
    pub tasks_dir: &'a Path,
    /// Wait seconds to use when the reset timestamp can't be parsed.
    pub fallback_wait: u64,
}

/// Injected usage-gate seam (inner/outer split, mirrors
/// `react_to_outputs`/`react_to_outputs_inner` and
/// `auto_review::{maybe_fire, maybe_fire_inner}`).
///
/// Called **exactly once** per [`account_usage_gate_inner`] invocation with the
/// destructured `(threshold, tasks_dir, fallback_wait)`. Production builds this
/// from `usage::check_and_wait`; tests inject a counting closure so they are
/// hermetic (no OAuth credentials, no usage API, no real `thread::sleep`). A
/// type alias keeps `clippy::type_complexity` quiet.
pub type UsageGateFn<'f> = &'f dyn Fn(u8, &Path, u64) -> UsageCheckResult;

/// Account-global usage gate (production entry point). Builds the real
/// `usage::check_and_wait` gate closure and delegates to
/// [`account_usage_gate_inner`].
///
/// This is an *account-global* reaction: it reflects shared API-account state,
/// not per-task state, so the caller fires it **exactly once per wave** (and
/// once per sequential iteration) — never once per slot.
///
/// The relocated leaf `usage::check_and_wait` carries `#[deprecated]` and the
/// three engine files carry `#![deny(deprecated)]`, so this coordinator is its
/// single legitimate caller; the engine paths route through here instead.
pub fn account_usage_gate(params: AccountUsageGateParams<'_>) -> UsageCheckResult {
    let gate = |threshold: u8, tasks_dir: &Path, fallback_wait: u64| -> UsageCheckResult {
        #[allow(deprecated)] // single legitimate caller of the relocated leaf
        usage::check_and_wait(threshold, tasks_dir, fallback_wait)
    };
    account_usage_gate_inner(params, &gate)
}

/// Hermetic core of the account-global usage gate. Destructures the params
/// exhaustively and fires `gate` **exactly once** with
/// `(threshold, tasks_dir, fallback_wait)`, returning its [`UsageCheckResult`]
/// unchanged. Same usage state ⇒ same decision, independent of which path
/// (sequential or wave) invoked it.
///
/// The contract is pinned by the parity tests in `tests/reaction_parity.rs`.
pub fn account_usage_gate_inner(
    params: AccountUsageGateParams<'_>,
    gate: UsageGateFn<'_>,
) -> UsageCheckResult {
    // Exhaustive destructure (no `..`) — the single-home parity lock. Adding a
    // field to `AccountUsageGateParams` forces this coordinator to account for
    // it before the code compiles.
    let AccountUsageGateParams {
        threshold,
        tasks_dir,
        fallback_wait,
    } = params;

    // Fire the gate EXACTLY once and return its decision unchanged — same usage
    // state ⇒ same UsageCheckResult, independent of the sequential vs wave caller.
    gate(threshold, tasks_dir, fallback_wait)
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
            #[allow(deprecated)] // single legitimate caller of the relocated leaf
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

// ---------------------------------------------------------------------------
// Post-output transient-backend reaction (FEAT-014) — sibling of the
// rate-limit reaction above.
//
// A transient backend failure (HTTP 502/503/504, Bad Gateway, Service
// Unavailable, Anthropic overloaded_error / HTTP 529) is a "retry later"
// signal, NOT a per-account rate limit and NOT a task crash. This reaction
// keys off `IterationOutcome::TransientBackend` items in the slice (sibling to
// the `RateLimit` trigger of `react_to_outputs`) and performs a BOUNDED
// backoff-retry that REUSES the rate-limit reset+wait scaffold: reset affected
// `in_progress` task(s) to `todo`, wait EXACTLY ONCE per wave (honoring the
// backend's `Retry-After` when present, else exponential `base*2^attempt`
// capped at `max`), and report `WaitedAndRetry` so the caller retries WITHOUT
// consuming the iteration budget (B2) and WITHOUT zeroing
// `ctx.consecutive_merge_fail_waves` (B3) — identical to the rate-limit path.
//
// Unlike the rate-limit wait (which can recur indefinitely until the window
// reopens), a backend outage is bounded: after `max_attempts` consecutive
// backoffs without progress the reaction `Escalate`s, letting the caller fall
// through to the existing crash/abort path rather than looping forever.
// ---------------------------------------------------------------------------

/// Cap on consecutive transient-backend backoffs before the reaction escalates
/// to the crash/abort path (FEAT-014). Five backoff waits before a prolonged
/// outage is treated as a task failure.
pub const TRANSIENT_MAX_ATTEMPTS: u32 = 5;
/// Exponential-backoff base seconds for the transient-backend reaction
/// (`base * 2^attempt`, capped at [`TRANSIENT_BACKOFF_MAX_SECS`]).
pub const TRANSIENT_BACKOFF_BASE_SECS: u64 = 30;
/// Exponential-backoff cap seconds for the transient-backend reaction.
pub const TRANSIENT_BACKOFF_MAX_SECS: u64 = 600;
/// `.stop`-poll interval during a transient backoff wait.
const TRANSIENT_WAIT_CHECK_INTERVAL_SECS: u64 = 10;

/// Outcome of the once-per-wave account transient-backend reaction.
#[derive(Debug, PartialEq, Eq)]
pub enum TransientReaction {
    /// No `TransientBackend` item in the slice. The attempt counter was reset
    /// to 0 (the streak is broken); ZERO other DB writes, no wait.
    None,
    /// A transient backend error was detected (under the attempt cap): the
    /// affected `in_progress` task(s) were reset to `todo` and the bounded
    /// backoff wait completed. The caller retries WITHOUT consuming the
    /// iteration budget (B2) and MUST NOT zero `ctx.consecutive_merge_fail_waves`
    /// (B3) — identical to [`AccountReaction::WaitedAndRetry`].
    WaitedAndRetry,
    /// The backoff wait was interrupted by a `.stop` signal. The caller stops
    /// (sequential: `should_stop`; wave: terminal exit 130).
    Stop,
    /// The attempt cap was reached (prolonged outage). The caller falls through
    /// to the existing crash/abort path: the sequential path rewrites the
    /// outcome to `Crash(RuntimeError)`; the wave path lets the retry-tracking
    /// loop account the `TransientBackend` slot as a failure.
    Escalate,
}

/// Inputs to [`react_to_transient`] / [`react_to_transient_inner`].
/// Destructured exhaustively (no `..`) — the single-home parity lock. The
/// per-wave attempt counter is threaded separately as `&mut u32` (it is
/// account-global cross-wave state living on `IterationContext`, not a config
/// input), so it is not a field here.
pub struct TransientReactionParams<'a> {
    /// Loop tasks dir — `.stop`-signal polling during the backoff wait.
    pub tasks_dir: &'a Path,
    /// PRD prefix scoping the `in_progress` reset
    /// (`TaskLifecycle::recover_in_progress_for_prefix`). An empty string maps
    /// to `None` (reset every `in_progress` row regardless of prefix).
    pub prefix: &'a str,
    /// Active run id for `TaskLifecycle::with_run`.
    pub run_id: &'a str,
    /// Cap on consecutive backoffs before escalating
    /// ([`TRANSIENT_MAX_ATTEMPTS`] at the production call sites).
    pub max_attempts: u32,
    /// Exponential-backoff base seconds ([`TRANSIENT_BACKOFF_BASE_SECS`]).
    pub base_wait_secs: u64,
    /// Exponential-backoff cap seconds ([`TRANSIENT_BACKOFF_MAX_SECS`]).
    pub max_wait_secs: u64,
}

/// Exponential backoff: `base * 2^attempt`, saturating and capped at `max`.
/// `attempt` is 0-based, so attempt 0 waits `base`, attempt 1 waits `2*base`,
/// etc.
fn backoff_secs(base: u64, attempt: u32, max: u64) -> u64 {
    let factor = 2u64.saturating_pow(attempt);
    base.saturating_mul(factor).min(max)
}

/// Sleep `wait_secs` in short intervals, polling for a `.stop` file. Returns
/// `true` if the full wait elapsed, `false` if `.stop` interrupted it. The
/// transient-backend analogue of `usage::wait_for_usage_reset` — no usage-API
/// probe, because a backend 5xx is not a per-account rate limit.
fn transient_backoff_wait(wait_secs: u64, tasks_dir: &Path) -> bool {
    if wait_secs == 0 {
        // Nothing to wait for, but still honor a pending `.stop`.
        return !signals::check_stop_signal(tasks_dir, None);
    }
    eprintln!(
        "Transient backend error. Backing off {} before retry (checking .stop every {}s)...",
        display::format_duration(wait_secs),
        TRANSIENT_WAIT_CHECK_INTERVAL_SECS,
    );
    let mut remaining = wait_secs;
    while remaining > 0 {
        if signals::check_stop_signal(tasks_dir, None) {
            eprintln!("Stop signal detected during transient backoff. Exiting wait.");
            return false;
        }
        let chunk = remaining.min(TRANSIENT_WAIT_CHECK_INTERVAL_SECS);
        thread::sleep(Duration::from_secs(chunk));
        remaining = remaining.saturating_sub(chunk);
    }
    true
}

/// Post-output transient-backend reaction (production entry point). Builds the
/// real backoff-wait closure and delegates to [`react_to_transient_inner`].
///
/// `attempts` is the account-global consecutive-backoff counter
/// (`IterationContext::transient_backend_attempts`), threaded by reference so
/// the counter logic stays single-homed in the reaction: reset to 0 on `None`,
/// `+= 1` on `WaitedAndRetry`, unchanged on `Escalate`/`Stop`.
pub fn react_to_transient(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &TransientReactionParams<'_>,
    attempts: &mut u32,
) -> TransientReaction {
    // Exhaustive destructure (no `..`) — the single-home parity lock. Adding a
    // field to `TransientReactionParams` forces this coordinator to account for
    // it. Only `tasks_dir` feeds the wait closure here; the rest reach the
    // hermetic core via `params`.
    let &TransientReactionParams {
        tasks_dir,
        prefix: _,
        run_id: _,
        max_attempts: _,
        base_wait_secs: _,
        max_wait_secs: _,
    } = params;

    let wait = |wait_secs: u64| -> bool { transient_backoff_wait(wait_secs, tasks_dir) };

    react_to_transient_inner(conn, items, params, attempts, &wait)
}

/// Hermetic core of the post-output transient-backend reaction. Detects
/// `TransientBackend` across `items`, manages the bounded-attempt counter,
/// resets the affected `in_progress` task(s) to `todo`, and fires `wait`
/// **exactly once** under the cap.
///
/// The contract is pinned by the parity tests in `tests/reaction_parity.rs`.
pub fn react_to_transient_inner(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &TransientReactionParams<'_>,
    attempts: &mut u32,
    wait: WaitFn<'_>,
) -> TransientReaction {
    // No `TransientBackend` item ⇒ the streak is broken: reset the attempt
    // counter and report None with ZERO DB writes, no wait. (Called
    // unconditionally by both paths, so a non-transient iteration/wave is what
    // resets the counter — "N retries WITHOUT progress".)
    let Some(first_transient) = items
        .iter()
        .find(|item| matches!(item.outcome, IterationOutcome::TransientBackend { .. }))
    else {
        *attempts = 0;
        return TransientReaction::None;
    };

    // Bounded attempts: once we've already backed off `max_attempts` times
    // without progress, stop waiting and escalate — the caller falls through to
    // the existing crash/abort path rather than looping forever during a
    // prolonged backend outage. The counter is intentionally NOT reset here:
    // while the outage persists every subsequent transient wave escalates
    // immediately (feeding the crash path toward auto-block); the `None` branch
    // resets it once the backend recovers (or a different outcome breaks the
    // streak).
    if *attempts >= params.max_attempts {
        return TransientReaction::Escalate;
    }

    // A transient backend error hit the shared account mid-wave. Reset every
    // `in_progress` row under this PRD prefix back to `todo` so the next
    // wave/iteration re-runs them. The `status = 'in_progress'` guard inside
    // `recover_in_progress_for_prefix` means slots that already completed THIS
    // wave (flipped to `done`) are never clobbered (B1).
    let prefix = if params.prefix.is_empty() {
        None
    } else {
        Some(params.prefix)
    };
    if let Err(e) =
        TaskLifecycle::with_run(conn, params.run_id).recover_in_progress_for_prefix(prefix)
    {
        eprintln!(
            "Warning: failed to reset in_progress tasks after transient backend error: {}",
            e
        );
    }

    // Honor the backend's `Retry-After` (carried on the outcome) when present;
    // otherwise exponential `base * 2^attempt` capped at `max`. Computed from
    // the FIRST transient item, then fire the injected wait seam EXACTLY once
    // for the whole wave — never once per transient slot.
    let retry_after = match first_transient.outcome {
        IterationOutcome::TransientBackend { retry_after_secs } => *retry_after_secs,
        _ => None,
    };
    let wait_secs = retry_after
        .unwrap_or_else(|| backoff_secs(params.base_wait_secs, *attempts, params.max_wait_secs));

    if wait(wait_secs) {
        *attempts += 1;
        TransientReaction::WaitedAndRetry
    } else {
        TransientReaction::Stop
    }
}
