//! Account-global usage gate (converged by FEAT-003/006).
//!
//! The pre-dispatch usage/rate-limit gate is an *account-global* reaction: it
//! reflects the shared API account state, not per-task state, so it fires
//! **exactly once per wave** (not once per slot). Both the sequential path
//! (`iteration.rs` ~L116) and the wave preflight route through this coordinator
//! — fixing the strand-bug where the wave path had no call site and a
//! rate-limited account never waited before the wave dispatched.

use std::path::Path;
use std::thread;
use std::time::Duration;

use chrono::TimeZone;
use rusqlite::Connection;

use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::config::{IterationOutcome, PermissionMode};
use crate::loop_engine::engine::BlackoutState;
use crate::loop_engine::model::Provider;
use crate::loop_engine::recovery::probe_rate_limit_lifted;
use crate::loop_engine::usage::{UsageCheckResult, check_usage_api};
use crate::loop_engine::{display, oauth, signals};

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
        check_and_wait(threshold, tasks_dir, fallback_wait)
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
// alongside `account_usage_gate`. FEAT-006 relocated the converged reaction
// here (the CONTRACT-001 `mod.rs` table originally sketched it under
// `post_output`) and both engine paths now route through it: sequential at
// `iteration.rs:703`, wave at `wave_scheduler.rs:1170`. The contract is pinned
// by the parity tests in `tests/reaction_parity.rs`.
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
    /// FEAT-008 quota-aware failover: a Claude rate-limit hit while
    /// difficulty-spillover is enabled. A provider blackout was **freshly**
    /// recorded on `ctx.provider_blackouts` from the reset timestamp (or
    /// `blackoutFallbackSecs` when unparseable), the affected `in_progress`
    /// task(s) were reset to `todo`, and the wait was **skipped** — the next
    /// selection pass reroutes spillover-eligible work to another provider and
    /// the no-eligible deferral branch waits only if EVERYTHING is
    /// quota-deferred. Caller treats it exactly like [`WaitedAndRetry`] for the
    /// budget give-back (B2) and the merge-fail-streak preservation (B3) — it
    /// simply did not block. NEVER touches `runner_overrides`.
    RerouteAndRetry,
    /// FEAT-008: a Claude rate-limit hit while spillover is enabled AND the
    /// provider was **already** under an active blackout (a prior wave recorded
    /// it). The window is extended; no fresh reset is implied. Treated
    /// identically to [`RerouteAndRetry`] by both callers — distinguished only
    /// so the reaction does not misreport a re-entrant rate-limit as a brand-new
    /// blackout. NEVER touches `runner_overrides`.
    ProceedWithSpillover,
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
    /// FEAT-008: whether difficulty-spillover is configured
    /// (`routing.spillover.maxDifficulty` is set). `false` → the reaction takes
    /// the legacy reset-and-wait path, byte-identical to pre-FEAT-008.
    pub spillover_enabled: bool,
    /// FEAT-008: the provider a Claude rate-limit blacks out (the resolved
    /// `models.primary_provider`; Claude in v1). Used only on the spillover
    /// path.
    pub primary_provider: Provider,
    /// FEAT-008: blackout window (seconds) recorded when the rate-limit reset
    /// timestamp is unparseable (`routing.spillover.blackoutFallbackSecs`).
    pub blackout_fallback_secs: u64,
    /// FEAT-008: the "now" (Unix-epoch seconds) the blackout expiry is keyed on.
    /// Threaded as an input so the spillover path is deterministic in tests.
    pub now_secs: u64,
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
    blackout: &mut BlackoutState,
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
        spillover_enabled: _,
        primary_provider: _,
        blackout_fallback_secs: _,
        now_secs: _,
    } = params;

    let wait = |wait_secs: u64| -> bool {
        // Try the usage API first (when enabled). It computes its own wait
        // internally, so the `wait_secs` arg is only consumed by the fallback.
        if usage_enabled {
            match check_and_wait(threshold, tasks_dir, fallback_wait) {
                UsageCheckResult::StopSignaled => return false,
                UsageCheckResult::WaitedAndReset => return true,
                // Skipped / BelowThreshold / ApiError — the API did not wait;
                // fall through to the output-parsed reset wait.
                _ => {}
            }
        }
        let probe = || probe_rate_limit_lifted(permission_mode);
        wait_for_usage_reset(wait_secs, tasks_dir, fallback_wait, Some(&probe))
    };

    react_to_outputs_inner(conn, items, params, blackout, &wait)
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
    blackout: &mut BlackoutState,
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
    // wave/iteration re-runs them. Slots that already completed THIS wave
    // (flipped to `done` by `process_slot_result`) are never clobbered
    // (FEAT-006 B1) — the `status = 'in_progress'` guard is inside the helper.
    // Runs in BOTH the spillover and the legacy paths.
    reset_in_progress_tasks(conn, params.run_id, params.prefix, "rate limit");

    // The reset timestamp is parsed once from the FIRST rate-limited output and
    // shared by both paths (the spillover blackout window and the legacy wait).
    let reset_secs = parse_reset_from_output(first_rate_limited.output);

    // FEAT-008 quota-aware failover. When difficulty-spillover is enabled, the
    // shared account's primary provider is blacked out (ephemerally, on
    // `ctx.provider_blackouts`) and the wait is SKIPPED: the next selection pass
    // reroutes spillover-eligible work to another provider, and the no-eligible
    // deferral branch waits only if EVERY remaining task is quota-deferred. This
    // channel is EPHEMERAL — it never reads or writes `runner_overrides` (the
    // permanent cross-provider promotion channel owned by `promote_once`). With
    // spillover DISABLED (the default), this branch is skipped entirely and the
    // reaction is byte-identical to the pre-FEAT-008 reset-and-wait path.
    if params.spillover_enabled {
        let blackout_secs = reset_secs.unwrap_or(params.blackout_fallback_secs);
        let already_active = blackout
            .active(params.now_secs)
            .contains(&params.primary_provider);
        blackout.record(params.primary_provider, params.now_secs, blackout_secs);
        return if already_active {
            AccountReaction::ProceedWithSpillover
        } else {
            AccountReaction::RerouteAndRetry
        };
    }

    // Legacy reset-and-wait path. Compute the wait once and fire the injected
    // wait seam EXACTLY once for the whole wave — never once per rate-limited
    // slot.
    let wait_secs = reset_secs.unwrap_or(params.fallback_wait);
    if wait(wait_secs) {
        AccountReaction::WaitedAndRetry
    } else {
        AccountReaction::Stop
    }
}

/// FEAT-008 deferral-first outcome — the verdict BOTH no-eligible paths (wave
/// `handle_no_eligible_tasks` and the sequential `NoEligibleTasks` branch) get
/// from [`handle_quota_deferral`] BEFORE any stale / auto-recovery / drained
/// classification.
#[derive(Debug, PartialEq, Eq)]
pub enum QuotaDeferral {
    /// No provider blackout is active (or it expired, or no todo work remains).
    /// The caller proceeds to its normal auto-recovery / stale logic. Any
    /// expired-but-lingering blackout was cleared as a side effect.
    Inactive,
    /// A provider blackout is active AND todo work remains: the empty selection
    /// is quota-DEFERRAL, not a stale or drained queue. The reset wait has
    /// completed (or `.stop` interrupted it) and the blackout was cleared. The
    /// caller retries WITHOUT marking the stale tracker. `stopped == true` →
    /// `.stop` fired during the wait; the caller stops instead of retrying.
    Deferred { stopped: bool },
}

/// Count `todo` rows for `task_prefix` (`None` = every prefix). Read-only — used
/// by the deferral check to decide whether an active blackout still has work to
/// wait for. `archived_at IS NULL` mirrors the drain-classification queries so
/// an archived row never keeps a blackout alive.
fn count_todo_tasks(conn: &Connection, task_prefix: Option<&str>) -> i64 {
    // `id LIKE '' || '%'` collapses to `id LIKE '%'` (every non-null id) when no
    // prefix is given, so one parameterized query covers both cases.
    let like_prefix = task_prefix.unwrap_or("");
    conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE status = 'todo' AND id LIKE ?1 || '%' \
         AND archived_at IS NULL",
        rusqlite::params![like_prefix],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// FEAT-008 deferral-first check (production entry point). When a provider
/// blackout is active and todo work remains, wait for the reset reusing the
/// EXISTING [`wait_for_usage_reset`] machinery (no busy-spin), clear the
/// blackout, and report [`QuotaDeferral::Deferred`]. Builds the real wait
/// closure and delegates to [`handle_quota_deferral_inner`].
///
/// Called FIRST — before stale / auto-recovery / drained classification — by
/// BOTH no-eligible paths, so an all-quota-deferred wave/iteration never trips
/// the stale-abort tracker (learning 3927).
pub fn handle_quota_deferral(
    conn: &Connection,
    task_prefix: Option<&str>,
    blackout: &mut BlackoutState,
    now_secs: u64,
    tasks_dir: &Path,
    fallback_wait: u64,
) -> QuotaDeferral {
    let wait = |wait_secs: u64| -> bool {
        // No early-lift probe: a quota blackout reopens on its own schedule, and
        // the probe is an OAuth/usage-API call we deliberately avoid on this
        // deferral path. `.stop` polling inside `wait_for_usage_reset` still
        // applies, so the wait stays interruptible.
        wait_for_usage_reset(wait_secs, tasks_dir, fallback_wait, None)
    };
    handle_quota_deferral_inner(conn, task_prefix, blackout, now_secs, &wait)
}

/// Hermetic core of the deferral-first check. Takes the wait as an injected seam
/// so the parity/edge-case tests can drive it without a real sleep, OAuth, or
/// usage API (`tests/model_selection_engine_edges.rs`). NEVER touches
/// `ctx.stale_tracker` or `runner_overrides` — it returns a verdict; the caller
/// owns the control flow.
pub fn handle_quota_deferral_inner(
    conn: &Connection,
    task_prefix: Option<&str>,
    blackout: &mut BlackoutState,
    now_secs: u64,
    wait: &dyn Fn(u64) -> bool,
) -> QuotaDeferral {
    if !blackout.any_active(now_secs) {
        return QuotaDeferral::Inactive;
    }
    // A blackout is active but nothing is left to defer → not a deferral; clear
    // the stale channel and let the caller run its normal drain classification.
    if count_todo_tasks(conn, task_prefix) == 0 {
        blackout.clear();
        return QuotaDeferral::Inactive;
    }
    // Wait until the LAST blacked-out provider reopens, then clear so the next
    // selection pass re-evaluates eligibility against a fresh channel.
    let wait_secs = blackout.max_remaining_secs(now_secs);
    let completed = wait(wait_secs);
    blackout.clear();
    QuotaDeferral::Deferred {
        stopped: !completed,
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

/// Resets every `in_progress` row under `prefix` back to `todo`, logging a
/// warning on error. The `status = 'in_progress'` guard inside
/// `recover_in_progress_for_prefix` means slots that already completed this
/// wave (flipped to `done`) are never clobbered (B1). `context` is appended
/// to the warning message to distinguish rate-limit from transient callers.
fn reset_in_progress_tasks(conn: &mut Connection, run_id: &str, prefix: &str, context: &str) {
    let prefix_opt = if prefix.is_empty() {
        None
    } else {
        Some(prefix)
    };
    if let Err(e) = TaskLifecycle::with_run(conn, run_id).recover_in_progress_for_prefix(prefix_opt)
    {
        eprintln!(
            "Warning: failed to reset in_progress tasks after {}: {}",
            context, e
        );
    }
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
    // wave/iteration re-runs them. Slots that already completed THIS wave
    // (flipped to `done`) are never clobbered (B1) — the `status =
    // 'in_progress'` guard is inside the helper.
    reset_in_progress_tasks(
        conn,
        params.run_id,
        params.prefix,
        "transient backend error",
    );

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

// ---------------------------------------------------------------------------
// Usage-wait helpers (CLEANUP-001: moved here from usage.rs).
//
// These helpers were originally in `usage.rs` with `#[deprecated]` notes
// pointing at `account.rs` as their converged home. CLEANUP-001 completes
// the move: the functions live here, the deprecation annotations are gone,
// and the call sites in this coordinator call them directly.
// ---------------------------------------------------------------------------

/// Maximum wait time for usage reset: 5 hours in seconds.
const MAX_WAIT_SECS: u64 = 5 * 3600;

/// Interval between .stop signal checks during wait: 10 seconds.
const WAIT_CHECK_INTERVAL_SECS: u64 = 10;

/// Interval between rate-limit probe checks: 60 seconds.
const PROBE_INTERVAL_SECS: u64 = 60;

/// Wait for usage to reset, displaying a countdown to stderr.
///
/// Checks the `.stop` signal file every `WAIT_CHECK_INTERVAL_SECS` seconds.
/// When `probe_fn` is `Some`, calls it every ~60 seconds to check if the
/// rate limit has been lifted early. The probe returns `true` if the limit
/// is lifted (resume immediately).
///
/// Returns `true` if the wait completed (or probe succeeded),
/// `false` if interrupted by `.stop`.
///
/// The `wait_secs` parameter specifies how long to wait. It is capped at
/// `MAX_WAIT_SECS` (5 hours).
pub(crate) fn wait_for_usage_reset(
    wait_secs: u64,
    tasks_dir: &Path,
    fallback_wait: u64,
    probe_fn: Option<&dyn Fn() -> bool>,
) -> bool {
    let effective_wait = if wait_secs == 0 {
        fallback_wait
    } else {
        wait_secs.min(MAX_WAIT_SECS)
    };

    eprintln!(
        "Usage limit reached. Waiting {} for reset{}...",
        display::format_duration(effective_wait),
        if probe_fn.is_some() {
            format!(" (probing every {}s)", PROBE_INTERVAL_SECS)
        } else {
            String::new()
        }
    );

    let mut remaining = effective_wait;
    // Start at the probe interval so the first probe fires immediately
    let mut since_last_probe: u64 = PROBE_INTERVAL_SECS;

    while remaining > 0 {
        // Check for stop signal
        if signals::check_stop_signal(tasks_dir, None) {
            eprintln!("Stop signal detected during usage wait. Exiting wait.");
            return false;
        }

        // Periodic probe: check if rate limit has been lifted early
        if let Some(ref probe) = probe_fn
            && since_last_probe >= PROBE_INTERVAL_SECS
        {
            since_last_probe = 0;
            eprintln!("  Probing whether rate limit has been lifted...");
            if probe() {
                eprintln!("  Rate limit lifted early! Resuming...");
                return true;
            }
            eprintln!("  Still rate-limited. Continuing wait...");
        }

        // Display countdown every interval
        let sleep_time = remaining.min(WAIT_CHECK_INTERVAL_SECS);

        eprintln!(
            "  Usage reset in {} (checking .stop every {}s)...",
            display::format_duration(remaining),
            WAIT_CHECK_INTERVAL_SECS
        );

        thread::sleep(Duration::from_secs(sleep_time));
        remaining = remaining.saturating_sub(sleep_time);
        since_last_probe += sleep_time;
    }

    eprintln!("Usage wait complete. Resuming...");
    true
}

/// Parse a reset time from Claude CLI output like "resets 4pm (America/Los_Angeles)".
///
/// Extracts the time token after "resets " and computes seconds until that local time.
/// Returns `None` if the pattern is not found, unparseable, or the time has already passed.
pub(crate) fn parse_reset_from_output(output: &str) -> Option<u64> {
    let lower = output.to_lowercase();
    let idx = lower.find("resets ")?;
    let after = &lower[idx + "resets ".len()..];

    // Extract time token: everything up to the next space or '('
    let end = after
        .find(|c: char| c == '(' || (c.is_whitespace() && c != ' '))
        .unwrap_or(after.len());
    let token_region = after[..end].trim();

    // The token might be like "4pm", "12:30am", "4:00pm", "16:00"
    // Take the first whitespace-delimited word as the time token
    let token = token_region
        .split_whitespace()
        .next()
        .unwrap_or(token_region);

    let (hour, minute) = parse_time_token(token)?;

    let now = chrono::Local::now();
    let today = now.date_naive();

    // Build target datetime in local timezone — try today first, then tomorrow
    let target_naive = today.and_hms_opt(hour, minute, 0)?;
    let target_local = now.timezone().from_local_datetime(&target_naive).single()?;

    let diff = target_local.signed_duration_since(now);
    if diff.num_seconds() > 0 {
        return Some(diff.num_seconds() as u64);
    }

    // Time already passed today — assume it means tomorrow
    let tomorrow = today.succ_opt()?;
    let target_naive = tomorrow.and_hms_opt(hour, minute, 0)?;
    let target_local = now.timezone().from_local_datetime(&target_naive).single()?;
    let diff = target_local.signed_duration_since(now);
    if diff.num_seconds() > 0 {
        return Some(diff.num_seconds() as u64);
    }

    None
}

/// Parse a time token like "4pm", "12:30am", "4:00pm", "16:00" into (hour, minute).
fn parse_time_token(token: &str) -> Option<(u32, u32)> {
    let token = token.trim().trim_end_matches([',', '.']);

    let (time_part, am_pm) = if let Some(stripped) = token.strip_suffix("am") {
        (stripped, Some("am"))
    } else if let Some(stripped) = token.strip_suffix("pm") {
        (stripped, Some("pm"))
    } else {
        (token, None)
    };

    let (hour, minute) = if let Some(colon_pos) = time_part.find(':') {
        let h: u32 = time_part[..colon_pos].parse().ok()?;
        let m: u32 = time_part[colon_pos + 1..].parse().ok()?;
        (h, m)
    } else {
        let h: u32 = time_part.parse().ok()?;
        (h, 0)
    };

    let hour = match am_pm {
        Some("am") => {
            if hour == 12 {
                0
            } else if hour > 12 {
                return None;
            } else {
                hour
            }
        }
        Some("pm") => {
            if hour == 12 {
                12
            } else if hour > 12 {
                return None;
            } else {
                hour + 12
            }
        }
        _ => hour, // 24-hour format
    };

    if hour >= 24 || minute >= 60 {
        return None;
    }

    Some((hour, minute))
}

/// Estimate seconds until reset from an ISO 8601 timestamp string.
///
/// Returns `None` if the timestamp can't be parsed or is in the past.
fn estimate_reset_seconds(reset_at: &str) -> Option<u64> {
    // Try parsing common ISO 8601 formats
    // Format: "2024-01-15T12:00:00Z" or "2024-01-15T12:00:00+00:00"
    let parsed = chrono::DateTime::parse_from_rfc3339(reset_at)
        .ok()
        .map(|dt| dt.timestamp())
        .or_else(|| {
            // Try without timezone
            chrono::NaiveDateTime::parse_from_str(reset_at, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|dt| dt.and_utc().timestamp())
        });

    let reset_epoch = parsed?;
    let now = chrono::Utc::now().timestamp();

    if reset_epoch > now {
        Some((reset_epoch - now) as u64)
    } else {
        None // Reset time is in the past
    }
}

/// Check usage and wait if above threshold. Main entry point for pre-iteration usage check.
///
/// Orchestrates:
/// 1. Ensure OAuth token is valid
/// 2. Check usage API
/// 3. If above threshold, wait for reset
///
/// Returns the result of the check-and-wait cycle.
pub(crate) fn check_and_wait(
    threshold: u8,
    tasks_dir: &Path,
    fallback_wait: u64,
) -> UsageCheckResult {
    // Step 1: Ensure token is valid
    let path = oauth::credentials_path();
    let creds = match oauth::read_credentials(&path) {
        Some(c) => c,
        None => return UsageCheckResult::Skipped,
    };

    // Refresh if needed
    if oauth::is_token_expiring(&creds, 5) {
        match oauth::refresh_token(&path, &creds) {
            Ok(_) => eprintln!("OAuth token refreshed for usage check"),
            Err(e) => {
                eprintln!("Warning: could not refresh token for usage check: {}", e);
                // Try with existing token anyway
            }
        }
    }

    // Re-read credentials (may have been refreshed)
    let creds = match oauth::read_credentials(&path) {
        Some(c) => c,
        None => return UsageCheckResult::Skipped,
    };

    // Step 2: Check usage API
    let usage = match check_usage_api(&creds.access_token) {
        Some(u) => u,
        None => return UsageCheckResult::ApiError("Failed to check usage API".to_string()),
    };

    eprintln!(
        "Usage: {:.1}% (threshold: {}%)",
        usage.percentage, threshold
    );

    if usage.percentage < f64::from(threshold) {
        return UsageCheckResult::BelowThreshold;
    }

    // Step 3: Usage is above threshold, wait for reset
    let wait_secs = usage
        .reset_at
        .as_deref()
        .and_then(estimate_reset_seconds)
        .unwrap_or(0);

    let completed = wait_for_usage_reset(wait_secs, tasks_dir, fallback_wait, None);

    if completed {
        UsageCheckResult::WaitedAndReset
    } else {
        UsageCheckResult::StopSignaled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loop_engine::STOP_FILE; // pub(crate) in loop_engine/mod.rs
    use tempfile::TempDir;

    // --- estimate_reset_seconds tests ---

    #[test]
    fn test_estimate_reset_seconds_future_rfc3339() {
        let future = chrono::Utc::now() + chrono::Duration::hours(2);
        let ts = future.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        // Should be approximately 7200 seconds (within 5 seconds tolerance)
        assert!(secs > 7190, "Expected >7190 but got {}", secs);
        assert!(secs < 7210, "Expected <7210 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_past_returns_none() {
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let ts = past.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_none(), "Past timestamp should return None");
    }

    #[test]
    fn test_estimate_reset_seconds_invalid_format_returns_none() {
        let result = estimate_reset_seconds("not-a-timestamp");
        assert!(result.is_none());
    }

    #[test]
    fn test_estimate_reset_seconds_naive_format() {
        let future = chrono::Utc::now() + chrono::Duration::minutes(30);
        let ts = future.format("%Y-%m-%dT%H:%M:%S").to_string();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        assert!(secs > 1790, "Expected >1790 but got {}", secs);
        assert!(secs < 1810, "Expected <1810 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_one_second_in_future() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(2);
        let ts = future.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        assert!(secs <= 3, "Expected <=3 but got {}", secs);
        assert!(secs >= 1, "Expected >=1 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_exactly_now() {
        let now = chrono::Utc::now();
        let ts = now.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(
            result.is_none(),
            "Timestamp at exact now should return None (not in future)"
        );
    }

    #[test]
    fn test_estimate_reset_seconds_far_future() {
        let future = chrono::Utc::now() + chrono::Duration::days(30);
        let ts = future.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        assert!(secs > 2_591_000, "Expected >2591000 but got {}", secs);
        assert!(secs < 2_593_000, "Expected <2593000 but got {}", secs);
    }

    #[test]
    fn test_estimate_reset_seconds_empty_string() {
        assert!(estimate_reset_seconds("").is_none());
    }

    #[test]
    fn test_estimate_reset_seconds_random_garbage() {
        assert!(estimate_reset_seconds("not-a-date-at-all").is_none());
        assert!(estimate_reset_seconds("12345").is_none());
        assert!(estimate_reset_seconds("2024-13-45T99:99:99Z").is_none());
    }

    #[test]
    fn test_estimate_reset_seconds_with_z_suffix() {
        let future = chrono::Utc::now() + chrono::Duration::minutes(10);
        let ts = format!("{}Z", future.format("%Y-%m-%dT%H:%M:%S"));
        let result = estimate_reset_seconds(&ts);
        assert!(result.is_some());
        let secs = result.unwrap();
        assert!(secs > 590, "Expected >590 but got {}", secs);
        assert!(secs < 610, "Expected <610 but got {}", secs);
    }

    // --- wait_for_usage_reset tests ---

    #[test]
    fn test_wait_for_usage_reset_zero_wait_uses_fallback() {
        let temp_dir = TempDir::new().unwrap();
        let completed = wait_for_usage_reset(0, temp_dir.path(), 1, None);
        assert!(completed, "Should complete with very short fallback");
    }

    #[test]
    fn test_wait_for_usage_reset_stop_signal_interrupts() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        let completed = wait_for_usage_reset(60, temp_dir.path(), 300, None);
        assert!(!completed, "Should be interrupted by stop signal");
    }

    #[test]
    fn test_wait_for_usage_reset_caps_at_max() {
        assert_eq!(MAX_WAIT_SECS, 18000);
    }

    #[test]
    fn test_wait_for_usage_reset_short_wait_completes() {
        let temp_dir = TempDir::new().unwrap();
        let completed = wait_for_usage_reset(1, temp_dir.path(), 1, None);
        assert!(completed);
    }

    #[test]
    fn test_wait_for_usage_reset_very_short_wait() {
        let temp_dir = TempDir::new().unwrap();
        let completed = wait_for_usage_reset(0, temp_dir.path(), 0, None);
        assert!(completed, "Zero effective wait should complete immediately");
    }

    #[test]
    fn test_wait_for_usage_reset_capped_at_max() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        let completed = wait_for_usage_reset(u64::MAX, temp_dir.path(), 300, None);
        assert!(!completed, "Should be interrupted by stop signal");
    }

    #[test]
    fn test_wait_for_usage_reset_fallback_not_used_when_wait_nonzero() {
        let temp_dir = TempDir::new().unwrap();
        let completed = wait_for_usage_reset(1, temp_dir.path(), 3600, None);
        assert!(completed, "Should complete quickly with 1 second wait");
    }

    #[test]
    fn test_wait_for_usage_reset_stop_file_created_during_wait() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        let completed = wait_for_usage_reset(100, temp_dir.path(), 300, None);
        assert!(!completed, "Stop file should interrupt wait");
    }

    #[test]
    fn test_wait_for_usage_reset_probe_exits_early() {
        let temp_dir = TempDir::new().unwrap();
        let probe = || true;
        let completed = wait_for_usage_reset(3600, temp_dir.path(), 300, Some(&probe));
        assert!(completed, "Probe returning true should exit wait early");
    }

    #[test]
    fn test_wait_for_usage_reset_probe_false_continues() {
        let temp_dir = TempDir::new().unwrap();
        let probe = || false;
        let completed = wait_for_usage_reset(1, temp_dir.path(), 1, Some(&probe));
        assert!(
            completed,
            "Probe returning false should not prevent completion"
        );
    }

    // --- Constants ---

    #[test]
    fn test_max_wait_is_5_hours() {
        assert_eq!(MAX_WAIT_SECS, 5 * 3600);
    }

    #[test]
    fn test_wait_check_interval_is_10_seconds() {
        assert_eq!(WAIT_CHECK_INTERVAL_SECS, 10);
    }

    // --- parse_reset_from_output tests ---

    #[test]
    fn test_parse_reset_from_output_4pm() {
        let now = chrono::Local::now();
        let future = now + chrono::Duration::hours(2);
        let hour_str = future.format("%-I%P").to_string();
        let output = format!(
            "You've hit your limit · resets {} (America/Los_Angeles)",
            hour_str
        );
        let result = parse_reset_from_output(&output);
        assert!(result.is_some(), "Should parse '{}' from output", hour_str);
        let secs = result.unwrap();
        assert!(secs >= 3600, "Expected >=3600 but got {}", secs);
        assert!(secs <= 7200, "Expected <=7200 but got {}", secs);
    }

    #[test]
    fn test_parse_reset_from_output_with_minutes() {
        let now = chrono::Local::now();
        let future = now + chrono::Duration::hours(1) + chrono::Duration::minutes(30);
        let time_str = future.format("%-I:%M%P").to_string();
        let output = format!("resets {} (America/Los_Angeles)", time_str);
        let result = parse_reset_from_output(&output);
        assert!(result.is_some(), "Should parse '{}' from output", time_str);
        let secs = result.unwrap();
        assert!(
            secs >= 5340,
            "Expected >=5340 (90 min - truncation) but got {}",
            secs
        );
        assert!(
            secs <= 5400,
            "Expected <=5400 (90 min, target truncated to :00) but got {}",
            secs
        );
    }

    #[test]
    fn test_parse_reset_from_output_no_match() {
        let output = "Some random output without reset info";
        assert!(parse_reset_from_output(output).is_none());
    }

    #[test]
    fn test_parse_reset_from_output_empty() {
        assert!(parse_reset_from_output("").is_none());
    }

    #[test]
    fn test_parse_reset_from_output_past_time_wraps_to_tomorrow() {
        let now = chrono::Local::now();
        let past = now - chrono::Duration::hours(2);
        let time_str = past.format("%-I%P").to_string();
        let output = format!("resets {}", time_str);
        let result = parse_reset_from_output(&output);
        assert!(
            result.is_some(),
            "Past time '{}' should wrap to tomorrow",
            time_str
        );
        let secs = result.unwrap();
        assert!(secs > 75000, "Expected >75000 (~21h) but got {}", secs);
        assert!(secs < 86400, "Expected <86400 (24h) but got {}", secs);
    }

    #[test]
    fn test_parse_reset_from_output_case_insensitive() {
        let now = chrono::Local::now();
        let future = now + chrono::Duration::hours(3);
        let time_str = future.format("%-I%P").to_string().to_uppercase();
        let output = format!("RESETS {} (America/Los_Angeles)", time_str);
        let result = parse_reset_from_output(&output);
        assert!(
            result.is_some(),
            "Should handle uppercase 'RESETS {}' ",
            time_str
        );
    }

    #[test]
    fn test_parse_reset_from_output_24h_format() {
        let now = chrono::Local::now();
        let future = now + chrono::Duration::hours(1);
        let time_str = future.format("%H:%M").to_string();
        let output = format!("resets {}", time_str);
        let result = parse_reset_from_output(&output);
        assert!(
            result.is_some(),
            "Should parse 24h format '{}' from output",
            time_str
        );
    }

    // --- parse_time_token unit tests ---

    #[test]
    fn test_parse_time_token_simple_pm() {
        assert_eq!(parse_time_token("4pm"), Some((16, 0)));
    }

    #[test]
    fn test_parse_time_token_simple_am() {
        assert_eq!(parse_time_token("9am"), Some((9, 0)));
    }

    #[test]
    fn test_parse_time_token_12am() {
        assert_eq!(parse_time_token("12am"), Some((0, 0)));
    }

    #[test]
    fn test_parse_time_token_12pm() {
        assert_eq!(parse_time_token("12pm"), Some((12, 0)));
    }

    #[test]
    fn test_parse_time_token_with_minutes() {
        assert_eq!(parse_time_token("4:30pm"), Some((16, 30)));
    }

    #[test]
    fn test_parse_time_token_midnight_minutes() {
        assert_eq!(parse_time_token("12:15am"), Some((0, 15)));
    }

    #[test]
    fn test_parse_time_token_24h() {
        assert_eq!(parse_time_token("16:00"), Some((16, 0)));
        assert_eq!(parse_time_token("0:00"), Some((0, 0)));
        assert_eq!(parse_time_token("23:59"), Some((23, 59)));
    }

    #[test]
    fn test_parse_time_token_invalid() {
        assert_eq!(parse_time_token(""), None);
        assert_eq!(parse_time_token("abc"), None);
        assert_eq!(parse_time_token("25:00"), None);
        assert_eq!(parse_time_token("12:60pm"), None);
        assert_eq!(parse_time_token("13pm"), None); // 13pm is invalid
    }
}
