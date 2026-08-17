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
use crate::loop_engine::usage::{UsageCheckResult, load_usage_info, usage_suggests_lifted};
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
/// Called **at most once** per [`react_to_outputs_inner`] on the legacy wait
/// path, with the **already-resolved** wait seconds from
/// [`decide_account_rate_limit`] (`0` = ready now → return true immediately).
/// Returns `true` when the wait completed (or was already ready), `false` when
/// interrupted by a `.stop` signal.
///
/// Tests inject a counting closure (hermetic — no OAuth, no sleep).
pub type WaitFn<'f> = &'f dyn Fn(u64) -> bool;

/// Injected reset-wait seam for the production post-output wrapper. Mirrors
/// [`wait_for_usage_reset`], including the optional early-lift probe.
pub type ResetWaitFn<'f> = &'f dyn Fn(u64, &Path, u64, Option<&dyn Fn() -> bool>) -> bool;

/// Injected early-lift probe seam for the production post-output wrapper.
pub type RateLimitProbeFn<'f> = &'f dyn Fn(&PermissionMode) -> bool;

/// Pure post-rate-limit decision (no I/O). Order is intentional:
/// 1. pure spend-stop (no API/output reset) → never blackout, never wait
/// 2. spillover → blackout with resolved secs
/// 3. legacy → wait with resolved secs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitAction {
    /// Credits/spend limit with no time-based reset — stop the loop.
    StopSpend,
    /// Sleep `secs` then retry (`0` = already ready).
    Wait { secs: u64 },
    /// Record a provider blackout for `secs` and reroute (spillover path).
    Blackout { secs: u64 },
}

/// Resolve wait/blackout seconds: API wins, then CLI output, then fallback.
///
/// `Some(0)` means **ready now** (past reset) and is preserved — it must NOT
/// collapse to `fallback`. Only when both sources are `None` do we use
/// `fallback`.
pub(crate) fn resolve_wait_secs(api: Option<u64>, output: Option<u64>, fallback: u64) -> u64 {
    match (api, output) {
        (Some(s), _) => s,
        (None, Some(s)) => s,
        (None, None) => fallback,
    }
}

/// Pure rate-limit action after a `RateLimit` hit (no I/O).
///
/// `api_secs` / `output_secs`: `None` = unknown; `Some(0)` = ready; `Some(n>0)` = wait n.
pub(crate) fn decide_account_rate_limit(
    api_secs: Option<u64>,
    output_secs: Option<u64>,
    output: &str,
    spillover_enabled: bool,
    fallback_wait: u64,
    blackout_fallback_secs: u64,
) -> RateLimitAction {
    // Pure spend/credits with no time-based reset: stop before blackout/wait.
    if api_secs.is_none() && output_secs.is_none() && is_spend_limit_message(output) {
        return RateLimitAction::StopSpend;
    }

    if spillover_enabled {
        let secs = resolve_wait_secs(api_secs, output_secs, blackout_fallback_secs);
        return RateLimitAction::Blackout { secs };
    }

    let secs = resolve_wait_secs(api_secs, output_secs, fallback_wait);
    RateLimitAction::Wait { secs }
}

/// Narrow spend/credits phrasing — not plain "monthly usage limit".
pub(crate) fn is_spend_limit_message(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("spend limit")
        || lower.contains("usage-credits")
        || lower.contains("admin-settings/usage")
        || (lower.contains("usage credits") && lower.contains("limit"))
}

/// Inputs to [`react_to_outputs`] / [`react_to_outputs_inner`]. Destructured
/// exhaustively (no `..`) by the FEAT-006 body — the single-home parity lock.
pub struct AccountReactionParams<'a> {
    /// Usage-API percentage threshold (production wait path only).
    pub threshold: u8,
    /// Whether the usage API pre-check is enabled (production wait path only).
    pub usage_enabled: bool,
    /// Whether post-output RateLimit recovery may touch Anthropic account I/O
    /// and spawn the Claude CLI early-lift probe. Keyed only from resolved
    /// Claude provider enablement, not the pre-iteration usage env flag.
    pub anthropic_account_io_allowed: bool,
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

/// Post-output rate-limit reaction (production entry point).
///
/// Dual-gate (do not collapse): load usage / try usage-gate only when
/// `anthropic_account_io_allowed` (Claude enablement). Env `usage_enabled` still
/// gates the pre-iteration-style usage_gate path when both allow. Early-lift
/// probe is Claude-only. Delegates to hermetic core with optional API reset.
pub fn react_to_outputs(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &AccountReactionParams<'_>,
    blackout: &mut BlackoutState,
) -> AccountReaction {
    let usage_gate = |threshold: u8, tasks_dir: &Path, fallback_wait: u64| -> UsageCheckResult {
        check_and_wait(threshold, tasks_dir, fallback_wait)
    };
    let reset_wait = |wait_secs: u64,
                      tasks_dir: &Path,
                      _fallback_wait: u64,
                      probe: Option<&dyn Fn() -> bool>| {
        // Local wait_for_usage_reset resolves unknown duration upstream
        // via decide_account_rate_limit; fallback_wait is unused here.
        wait_for_usage_reset(wait_secs, tasks_dir, probe)
    };
    let probe =
        |permission_mode: &PermissionMode| -> bool { probe_rate_limit_lifted(permission_mode) };
    react_to_outputs_with_io_seams(
        conn,
        items,
        params,
        blackout,
        &usage_gate,
        &reset_wait,
        &probe,
    )
}

/// Post-output rate-limit reaction with production I/O seams injected. Tests use
/// this to prove Anthropic/Claude side effects are skipped when Claude is
/// disabled without live credentials or a real Claude binary.
pub fn react_to_outputs_with_io_seams(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &AccountReactionParams<'_>,
    blackout: &mut BlackoutState,
    usage_gate: UsageGateFn<'_>,
    reset_wait: ResetWaitFn<'_>,
    probe_rate_limit: RateLimitProbeFn<'_>,
) -> AccountReaction {
    // Exhaustive destructure (no `..`) — the single-home parity lock.
    let &AccountReactionParams {
        threshold,
        usage_enabled,
        anthropic_account_io_allowed,
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

    // Usage load only when Claude account I/O is allowed (skip Anthropic when
    // Claude is disabled). Feeds decide_account_rate_limit's api_secs.
    let api_secs = if anthropic_account_io_allowed {
        let usage = load_usage_info();
        let secs = usage
            .as_ref()
            .and_then(|u| u.reset_at.as_deref())
            .and_then(estimate_reset_seconds);
        if let Some(s) = secs {
            if s > 0 {
                eprintln!(
                    "Usage API: window resets in {}.",
                    display::format_duration(s)
                );
            } else {
                eprintln!("Usage API: reset window already open (ready).");
            }
        }
        secs
    } else {
        None
    };

    let wait = |wait_secs: u64| -> bool {
        // Optional usage-gate first when both Claude allow-flag and env enablement.
        if anthropic_account_io_allowed && usage_enabled {
            match usage_gate(threshold, tasks_dir, fallback_wait) {
                UsageCheckResult::StopSignaled => return false,
                UsageCheckResult::WaitedAndReset => return true,
                _ => {}
            }
        }
        // Reset wait always runs; early-lift probe is Claude-only (spawns CLI).
        // Do not fold a live usage re-fetch into this probe — FEAT-002 spies
        // assert `probe_rate_limit` is invoked when wired.
        let probe = || probe_rate_limit(permission_mode);
        let probe_arg: Option<&dyn Fn() -> bool> =
            anthropic_account_io_allowed.then_some(&probe as &dyn Fn() -> bool);
        reset_wait(wait_secs, tasks_dir, fallback_wait, probe_arg)
    };

    react_to_outputs_inner(conn, items, params, blackout, api_secs, &wait)
}

/// Hermetic core of the post-output rate-limit reaction.
///
/// `api_reset_secs`: inject `None` in tests (or a known value) so the pure
/// [`decide_account_rate_limit`] path is hermetic. Production
/// [`react_to_outputs`] loads this via [`load_usage_info`].
///
/// Order (same for spillover and legacy after the spend check):
/// 1. reset `in_progress` → `todo`
/// 2. pure spend-stop → message → [`AccountReaction::Stop`] (no blackout)
/// 3. spillover → blackout with resolved secs → Reroute / Proceed
/// 4. legacy → `wait(secs)` → WaitedAndRetry / Stop
///
/// The contract is pinned by the parity tests in `tests/reaction_parity.rs`.
pub fn react_to_outputs_inner(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &AccountReactionParams<'_>,
    blackout: &mut BlackoutState,
    api_reset_secs: Option<u64>,
    wait: WaitFn<'_>,
) -> AccountReaction {
    // No `RateLimit` item ⇒ nothing to do. ZERO DB writes, no wait.
    let Some(first_rate_limited) = items
        .iter()
        .find(|item| *item.outcome == IterationOutcome::RateLimit)
    else {
        return AccountReaction::None;
    };

    // Always reset in_progress first so work isn't stuck if we StopSpend.
    reset_in_progress_tasks(conn, params.run_id, params.prefix, "rate limit");

    let output_secs = parse_reset_from_output(first_rate_limited.output);
    let action = decide_account_rate_limit(
        api_reset_secs,
        output_secs,
        first_rate_limited.output,
        params.spillover_enabled,
        params.fallback_wait,
        params.blackout_fallback_secs,
    );

    match action {
        RateLimitAction::StopSpend => {
            eprintln!(
                "Usage/spend limit with no time-based reset from the API or CLI output.\n\
                 Raise credits: Claude Code /usage-credits (or admin usage settings).\n\
                 Stopping the loop (tasks left as todo)."
            );
            AccountReaction::Stop
        }
        RateLimitAction::Blackout { secs } => {
            // FEAT-008: ephemeral blackout; never touches runner_overrides.
            let already_active = blackout
                .active(params.now_secs)
                .contains(&params.primary_provider);
            blackout.record(params.primary_provider, params.now_secs, secs);
            if already_active {
                AccountReaction::ProceedWithSpillover
            } else {
                AccountReaction::RerouteAndRetry
            }
        }
        RateLimitAction::Wait { secs } => {
            // Fire wait EXACTLY once for the whole wave.
            if wait(secs) {
                AccountReaction::WaitedAndRetry
            } else {
                AccountReaction::Stop
            }
        }
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
    _fallback_wait: u64,
) -> QuotaDeferral {
    let wait = |wait_secs: u64| -> bool {
        // No early-lift probe: a quota blackout reopens on its own schedule.
        // `.stop` polling inside `wait_for_usage_reset` still applies.
        // `wait_secs` is remaining blackout duration (`0` = ready immediately).
        // `_fallback_wait` is unused: blackout max_remaining is authoritative
        // (callers still pass LOOP fallback for signature stability).
        wait_for_usage_reset(wait_secs, tasks_dir, None)
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

/// Production wait-loop intervals.
const PROD_TIMING: WaitTiming = WaitTiming {
    stop_check_secs: 10,
    probe_secs: 30,
    status_secs: 12 * 60,
};

/// Intervals for [`wait_for_usage_reset_inner`]. Production uses [`PROD_TIMING`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct WaitTiming {
    /// Silent `.stop` poll interval.
    pub stop_check_secs: u64,
    /// Early-lift probe interval.
    pub probe_secs: u64,
    /// Sparse status-line interval.
    pub status_secs: u64,
}

/// Production wait wrapper: fixed timing + real sleep.
///
/// **`wait_secs` semantics (must not regress):**
/// - `0` = **ready now** → return `true` immediately (never becomes 300s fallback)
/// - `n > 0` = sleep up to `min(n, MAX_WAIT_SECS)`; if `n > MAX`, log the true
///   duration and the cap
///
/// Unknown duration is resolved **before** this function (`resolve_wait_secs` /
/// `decide_account_rate_limit`); this function never invents a fallback.
pub(crate) fn wait_for_usage_reset(
    wait_secs: u64,
    tasks_dir: &Path,
    probe_fn: Option<&dyn Fn() -> bool>,
) -> bool {
    wait_for_usage_reset_inner(wait_secs, tasks_dir, probe_fn, PROD_TIMING, |d| {
        thread::sleep(d)
    })
}

/// Injectable wait body (hermetic tests pass tiny timing + no-op / virtual sleep).
pub(crate) fn wait_for_usage_reset_inner(
    wait_secs: u64,
    tasks_dir: &Path,
    probe_fn: Option<&dyn Fn() -> bool>,
    timing: WaitTiming,
    sleep: impl Fn(Duration),
) -> bool {
    // Ready now — do not treat 0 as "unknown" / fallback.
    if wait_secs == 0 {
        eprintln!("Usage window ready. Resuming...");
        return true;
    }

    let capped = wait_secs > MAX_WAIT_SECS;
    let effective_wait = wait_secs.min(MAX_WAIT_SECS);
    if capped {
        eprintln!(
            "Usage API reset in {}; waiting max {} then retrying.",
            display::format_duration(wait_secs),
            display::format_duration(MAX_WAIT_SECS),
        );
    } else {
        eprintln!(
            "Waiting {} for usage reset{} (status every {})...",
            display::format_duration(effective_wait),
            if probe_fn.is_some() {
                format!("; probing every {}s", timing.probe_secs)
            } else {
                String::new()
            },
            display::format_duration(timing.status_secs),
        );
    }

    let mut remaining = effective_wait;
    // First probe after one full interval (account still limited right after hit).
    let mut since_last_probe: u64 = 0;
    // First status after status_secs (banner already has initial duration).
    let mut since_last_status: u64 = 0;

    while remaining > 0 {
        if signals::check_stop_signal(tasks_dir, None) {
            eprintln!("Stop signal detected during usage wait. Exiting wait.");
            return false;
        }

        if let Some(ref probe) = probe_fn
            && since_last_probe >= timing.probe_secs
        {
            since_last_probe = 0;
            if probe() {
                eprintln!("  Rate limit lifted early (usage API). Resuming...");
                return true;
            }
            // Still limited — quiet.
        }

        if since_last_status >= timing.status_secs {
            since_last_status = 0;
            eprintln!(
                "  Still waiting — {} remaining.",
                display::format_duration(remaining)
            );
        }

        let sleep_time = remaining.min(timing.stop_check_secs).max(1);
        sleep(Duration::from_secs(sleep_time));
        remaining = remaining.saturating_sub(sleep_time);
        since_last_probe = since_last_probe.saturating_add(sleep_time);
        since_last_status = since_last_status.saturating_add(sleep_time);
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
/// - Unparseable → `None` (unknown)
/// - Past or ≤0  → `Some(0)` (**ready now** — not unknown)
/// - Future      → `Some(secs)`
pub(crate) fn estimate_reset_seconds(reset_at: &str) -> Option<u64> {
    // Format: "2024-01-15T12:00:00Z" or "2024-01-15T12:00:00+00:00" / fractional
    let parsed = chrono::DateTime::parse_from_rfc3339(reset_at)
        .ok()
        .map(|dt| dt.timestamp())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(reset_at, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|dt| dt.and_utc().timestamp())
        });

    let reset_epoch = parsed?;
    let now = chrono::Utc::now().timestamp();

    if reset_epoch > now {
        Some((reset_epoch - now) as u64)
    } else {
        Some(0) // ready — do not return None (that would thrash into fallback)
    }
}

/// Check usage and wait if above threshold. Main entry point for pre-iteration usage check.
///
/// Orchestrates:
/// 1. `load_usage_info` (creds + refresh + OAuth/org usage API)
/// 2. If above threshold, wait for reset with API early-lift probe
///
/// Returns the result of the check-and-wait cycle.
pub(crate) fn check_and_wait(
    threshold: u8,
    tasks_dir: &Path,
    fallback_wait: u64,
) -> UsageCheckResult {
    let usage = match load_usage_info() {
        Some(u) => u,
        None => {
            // Distinguish "no creds" from "API failed" is best-effort: load
            // already degraded; surface as skipped when nothing usable.
            return UsageCheckResult::Skipped;
        }
    };

    eprintln!(
        "Usage: {:.1}% (threshold: {}%)",
        usage.percentage, threshold
    );

    if usage.percentage < f64::from(threshold) {
        return UsageCheckResult::BelowThreshold;
    }

    // Above threshold: wait. Some(0) = ready now; None = unknown → fallback.
    let wait_secs = usage
        .reset_at
        .as_deref()
        .and_then(estimate_reset_seconds)
        .unwrap_or(fallback_wait);

    let probe = || {
        if let Some(info) = load_usage_info() {
            if usage_suggests_lifted(&info, threshold, false) {
                return true;
            }
            if let Some(r) = info.reset_at.as_deref() {
                return estimate_reset_seconds(r) == Some(0);
            }
        }
        false
    };

    let completed = wait_for_usage_reset(wait_secs, tasks_dir, Some(&probe));

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
    fn test_estimate_reset_seconds_past_returns_ready_zero() {
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let ts = past.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert_eq!(
            result,
            Some(0),
            "Past timestamp must be Some(0) ready, not None (avoids fallback thrash)"
        );
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
    fn test_estimate_reset_seconds_exactly_now_is_ready() {
        let now = chrono::Utc::now();
        let ts = now.to_rfc3339();
        let result = estimate_reset_seconds(&ts);
        assert_eq!(
            result,
            Some(0),
            "Timestamp at exact now is ready (Some(0)), not unknown"
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

    // --- resolve / decide pure tests ---

    #[test]
    fn test_resolve_wait_secs_api_wins_including_zero() {
        assert_eq!(resolve_wait_secs(Some(0), Some(500), 300), 0);
        assert_eq!(resolve_wait_secs(Some(120), Some(500), 300), 120);
        assert_eq!(resolve_wait_secs(None, Some(500), 300), 500);
        assert_eq!(resolve_wait_secs(None, None, 300), 300);
    }

    #[test]
    fn test_decide_spend_stop_no_resets() {
        let action = decide_account_rate_limit(
            None,
            None,
            "You've hit your individual spend limit · run /usage-credits",
            false,
            300,
            3600,
        );
        assert_eq!(action, RateLimitAction::StopSpend);
    }

    #[test]
    fn test_decide_spend_with_api_reset_waits() {
        let action = decide_account_rate_limit(
            Some(3600),
            None,
            "You've hit your individual spend limit · run /usage-credits",
            false,
            300,
            3600,
        );
        assert_eq!(action, RateLimitAction::Wait { secs: 3600 });
    }

    #[test]
    fn test_decide_spillover_blackout_uses_api_secs() {
        let action = decide_account_rate_limit(Some(7200), None, "rate limited", true, 300, 3600);
        assert_eq!(action, RateLimitAction::Blackout { secs: 7200 });
    }

    #[test]
    fn test_decide_spillover_pure_spend_stops_no_blackout() {
        let action =
            decide_account_rate_limit(None, None, "spend limit · usage-credits", true, 300, 3600);
        assert_eq!(
            action,
            RateLimitAction::StopSpend,
            "pure spend must not record a short blackout"
        );
    }

    #[test]
    fn test_is_spend_limit_message_narrow() {
        assert!(is_spend_limit_message(
            "You've hit your individual spend limit · run /usage-credits"
        ));
        assert!(!is_spend_limit_message(
            "You've hit your org's monthly usage limit"
        ));
        assert!(!is_spend_limit_message(
            "You've hit your limit · resets 4pm"
        ));
    }

    // --- wait_for_usage_reset tests ---

    #[test]
    fn test_wait_zero_is_ready_not_fallback() {
        let temp_dir = TempDir::new().unwrap();
        // Must complete immediately — never sleep fallback 300s.
        let completed = wait_for_usage_reset(0, temp_dir.path(), None);
        assert!(completed, "Some(0) ready must return true immediately");
    }

    #[test]
    fn test_wait_for_usage_reset_stop_signal_interrupts() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        let completed = wait_for_usage_reset(60, temp_dir.path(), None);
        assert!(!completed, "Should be interrupted by stop signal");
    }

    #[test]
    fn test_wait_for_usage_reset_short_wait_completes() {
        let temp_dir = TempDir::new().unwrap();
        let completed = wait_for_usage_reset(1, temp_dir.path(), None);
        assert!(completed);
    }

    #[test]
    fn test_wait_for_usage_reset_capped_stop_interrupts() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        let completed = wait_for_usage_reset(u64::MAX, temp_dir.path(), None);
        assert!(!completed, "Should be interrupted by stop signal");
    }

    #[test]
    fn test_wait_for_usage_reset_stop_file_created_during_wait() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(STOP_FILE), "").unwrap();
        let completed = wait_for_usage_reset(100, temp_dir.path(), None);
        assert!(!completed, "Stop file should interrupt wait");
    }

    #[test]
    fn test_wait_probe_exits_early_with_tiny_timing() {
        let temp_dir = TempDir::new().unwrap();
        let probe = || true;
        let timing = WaitTiming {
            stop_check_secs: 1,
            probe_secs: 1,
            status_secs: 100,
        };
        // Fake sleep: no wall clock; probe fires after first "interval".
        let completed = wait_for_usage_reset_inner(
            10,
            temp_dir.path(),
            Some(&probe),
            timing,
            |_| {}, // no-op sleep — loop advances remaining via sleep_time
        );
        // With no-op sleep remaining still decreases... wait, if sleep is no-op
        // remaining still decreases each iteration. Probe fires when
        // since_last_probe >= 1 after first sleep chunk. Good.
        assert!(completed, "Probe returning true should exit wait early");
    }

    #[test]
    fn test_wait_probe_false_completes_with_tiny_timing() {
        let temp_dir = TempDir::new().unwrap();
        let probe = || false;
        let timing = WaitTiming {
            stop_check_secs: 1,
            probe_secs: 1,
            status_secs: 100,
        };
        let completed =
            wait_for_usage_reset_inner(2, temp_dir.path(), Some(&probe), timing, |_| {});
        assert!(completed, "false probe must not block completion");
    }

    // --- Constants ---

    #[test]
    fn test_max_wait_is_5_hours() {
        assert_eq!(MAX_WAIT_SECS, 5 * 3600);
    }

    #[test]
    fn test_prod_timing_intervals() {
        assert_eq!(PROD_TIMING.stop_check_secs, 10);
        assert_eq!(PROD_TIMING.probe_secs, 30);
        assert_eq!(PROD_TIMING.status_secs, 12 * 60);
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
