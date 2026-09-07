//! Account-global usage gate (converged by FEAT-003/006).
//!
//! The pre-dispatch usage/rate-limit gate is an *account-global* reaction: it
//! reflects the shared API account state, not per-task state, so it fires
//! **exactly once per wave** (not once per slot). Both the sequential path
//! (`iteration.rs` ~L116) and the wave preflight route through this coordinator
//! — fixing the strand-bug where the wave path had no call site and a
//! rate-limited account never waited before the wave dispatched.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::thread;
use std::time::Duration;

use chrono::TimeZone;
use rusqlite::Connection;

use crate::lifecycle::TaskLifecycle;
use crate::loop_engine::config::{IterationOutcome, PermissionMode};
use crate::loop_engine::engine::BlackoutState;
use crate::loop_engine::model::{CapabilityTier, Provider};
use crate::loop_engine::project_config::TierFallback;
use crate::loop_engine::quota::{
    AccountLowInput, BucketEval, OnLowAction, QuotaBucket, QuotaEval, UsagePolicy, evaluate_quota,
};
use crate::loop_engine::recovery::probe_rate_limit_lifted;
use crate::loop_engine::runner::RunnerKind;
use crate::loop_engine::usage::{
    UsageCheckResult, UsageInfo, buckets_for_run_models, load_usage_info_with_threshold,
    usage_suggests_lifted,
};
use crate::loop_engine::{display, signals};

/// Inputs to [`account_usage_gate`] / [`account_usage_gate_inner`].
/// Destructured exhaustively (no `..`) by the FEAT-003 body — the single-home
/// parity lock.
///
/// `account` is `pub` so this is reachable from the integration parity harness
/// (`tests/reaction_parity.rs`).
pub struct AccountUsageGateParams<'a> {
    /// Remaining-percent floor (0–100). Wait when account remaining ≤ this.
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

/// Injected usage-API load seam for [`react_to_outputs_with_io_seams`].
/// Production wires [`load_usage_info`]; tests inject a hermetic closure so
/// rung-scoped RateLimit never hits live OAuth/usage (and so ordinary
/// RateLimit can assert the load without credentials).
pub type LoadUsageFn<'f> = &'f dyn Fn() -> Option<UsageInfo>;

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
    // Spend/credits: stop before blackout/wait when the Usage API did not
    // supply a reset. Live CLI banners often embed `resets 3:40am` (session
    // window) alongside spend/credits copy — that output_secs must NOT defeat
    // StopSpend, or the loop parks until morning while the account is still
    // spend-blocked (raising credits is the only recovery). api_secs still
    // wins when present (account-binding reset from the usage API).
    if api_secs.is_none() && is_spend_limit_message(output) {
        return RateLimitAction::StopSpend;
    }

    // PR-1 / FR-002: rung-scoped CLI (Fable/Opus/…) → fixed Wait via
    // `blackout_fallback_secs` (default 3600). Ignores api_secs / output_secs,
    // never Blackout (spillover is not a working rung for this phrasing), never
    // falls through to usage_fallback_wait (300s).
    if is_rung_scoped_rate_limit_message(output) {
        return RateLimitAction::Wait {
            secs: blackout_fallback_secs,
        };
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

/// PR-1 / FR-002: narrow Fable/rung-scoped CLI phrasing that takes the fixed
/// `blackout_fallback_secs` Wait (default 3600) and skips usage_gate + early-lift
/// probe.
///
/// True when:
/// - a capability-rung model token (`fable|opus|sonnet|haiku`) is a real word
///   (start/end/whitespace — **not** hyphen/underscore) and is followed by
///   `limit` within a short window, **or**
/// - the live phrase `reached your (fable|opus|sonnet|haiku) limit`, **or**
/// - `switch models` appears on the **same line** as `reached` or `limit`
///   (not an unanchored whole-capture contains — docs/commentary on a later
///   line must not force Wait 3600).
///
/// `/model` alone is **not** sufficient. Plain `You've reached your session
/// limit` / account `hit your limit · resets 4pm` do **not** match — those
/// keep api_secs / may Blackout. Hyphenated configured model ids
/// (`OPUS_MODEL` / `FABLE_MODEL`) must **not** count as a token match.
pub(crate) fn is_rung_scoped_rate_limit_message(output: &str) -> bool {
    let lower = output.to_lowercase();
    if switch_models_on_rate_limit_line(&lower) {
        return true;
    }
    if reached_your_model_limit_phrase(&lower) {
        return true;
    }
    model_token_followed_by_limit(&lower)
}

/// `switch models` only counts toward the 3600 override when it shares a line
/// with `reached` or `limit` (live Fable: one line, two sentences).
fn switch_models_on_rate_limit_line(lower: &str) -> bool {
    for line in lower.lines() {
        if line.contains("switch models") && (line.contains("reached") || line.contains("limit")) {
            return true;
        }
    }
    false
}

const RUNG_MODEL_TOKENS: &[&str] = &["fable", "opus", "sonnet", "haiku"];

/// Live CLI copy: `You've reached your Fable limit` (and Opus/Sonnet/Haiku).
fn reached_your_model_limit_phrase(lower: &str) -> bool {
    for token in RUNG_MODEL_TOKENS {
        // Avoid allocation: scan for "reached your <token> limit".
        let mut from = 0;
        while let Some(rel) = lower[from..].find("reached your ") {
            let phrase_start = from + rel;
            let after_prefix = phrase_start + "reached your ".len();
            if lower[after_prefix..].starts_with(token) {
                let after_token = after_prefix + token.len();
                if lower[after_token..].starts_with(" limit") {
                    return true;
                }
            }
            from = phrase_start + 1;
        }
    }
    false
}

/// True at string start/end or when the adjacent char is whitespace.
/// Hyphen and underscore are **not** word boundaries — otherwise
/// hyphenated / underscored configured model ids falsely match `opus`/`fable`.
fn is_real_word_boundary(lower: &str, index: usize, before: bool) -> bool {
    if before {
        if index == 0 {
            return true;
        }
        // Tokens are ASCII so `index` is a char boundary; take the prior char.
        lower[..index]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace())
    } else if index >= lower.len() {
        true
    } else {
        lower[index..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace())
    }
}

/// Word-bounded model token followed by `limit` within 64 bytes of the token.
fn model_token_followed_by_limit(lower: &str) -> bool {
    for token in RUNG_MODEL_TOKENS {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(token) {
            let start = from + rel;
            let end = start + token.len();
            if is_real_word_boundary(lower, start, true) && is_real_word_boundary(lower, end, false)
            {
                // end+64 can land mid-codepoint in mixed Unicode agent output.
                let window_end = lower.floor_char_boundary((end + 64).min(lower.len()));
                if lower[end..window_end].contains("limit") {
                    return true;
                }
            }
            from = start + 1;
        }
    }
    false
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
    let threshold = params.threshold;
    let load_usage = || load_usage_info_with_threshold(threshold);
    react_to_outputs_with_io_seams(
        conn,
        items,
        params,
        blackout,
        &usage_gate,
        &reset_wait,
        &probe,
        &load_usage,
    )
}

/// Post-output rate-limit reaction with production I/O seams injected. Tests use
/// this to prove Anthropic/Claude side effects are skipped when Claude is
/// disabled (or when RateLimit is rung-scoped) without live credentials or a
/// real Claude binary.
#[allow(clippy::too_many_arguments)] // four distinct I/O seams; packing relocates noise
pub fn react_to_outputs_with_io_seams(
    conn: &mut Connection,
    items: &[OutputReactionItem<'_>],
    params: &AccountReactionParams<'_>,
    blackout: &mut BlackoutState,
    usage_gate: UsageGateFn<'_>,
    reset_wait: ResetWaitFn<'_>,
    probe_rate_limit: RateLimitProbeFn<'_>,
    load_usage: LoadUsageFn<'_>,
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

    // Narrow rung-scoped phrasing (Fable/Opus/…): skip load_usage_info,
    // usage_gate, and early-lift probe. The skip cannot live only in the
    // hermetic inner — loading api_secs would still hit OAuth/usage and print
    // a multi-hour reset banner that decide then ignores; wiring the probe
    // would undo the 3600s Wait in ~30s (or used 55% < 92 → BelowThreshold).
    //
    // Also skip when the slice has no RateLimit at all — the inner early-returns
    // AccountReaction::None, but a premature load_usage here would still hit
    // live Anthropic on every wave/sequential completion when Claude is enabled
    // and ~/.claude credentials exist (hung unit tests / false account I/O).
    let has_rate_limit = items
        .iter()
        .any(|item| *item.outcome == IterationOutcome::RateLimit);
    let rung_scoped = items.iter().any(|item| {
        *item.outcome == IterationOutcome::RateLimit
            && is_rung_scoped_rate_limit_message(item.output)
    });

    // Usage load only when there is a RateLimit, Claude account I/O is allowed,
    // AND it is not rung-scoped. Ordinary RateLimit still feeds decide's
    // api_secs; rung-scoped Wait ignores api_secs (forced blackout_fallback_secs).
    let api_secs = if has_rate_limit && !rung_scoped && anthropic_account_io_allowed {
        let usage = load_usage();
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
        // Rung-scoped phrasing skips this leg (and the early-lift probe below).
        if !rung_scoped && anthropic_account_io_allowed && usage_enabled {
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
            (!rung_scoped && anthropic_account_io_allowed).then_some(&probe as &dyn Fn() -> bool);
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

    // Prefer a rung-scoped RateLimit for decide when any match — aligns with the
    // `any()` skip in `react_to_outputs_with_io_seams`. Mixed wave
    // [account hit-your-limit, Fable switch-models] must Wait(blackout_fallback_secs)
    // and never Blackout, even when the non-Fable item is first.
    let decide_item = items
        .iter()
        .find(|item| {
            *item.outcome == IterationOutcome::RateLimit
                && is_rung_scoped_rate_limit_message(item.output)
        })
        .unwrap_or(first_rate_limited);

    // Always reset in_progress first so work isn't stuck if we StopSpend.
    reset_in_progress_tasks(conn, params.run_id, params.prefix, "rate limit");

    let output_secs = parse_reset_from_output(decide_item.output);
    let action = decide_account_rate_limit(
        api_reset_secs,
        output_secs,
        decide_item.output,
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
///
/// Reused by the PR-2 horizon middle band: reset in (waitIfResetWithinMinutes,
/// stopIfResetBeyondHours] waits capped at this value (3h waits; 5h–12h is
/// cap-and-repark).
pub(crate) const MAX_WAIT_SECS: u64 = 5 * 3600;

// ---------------------------------------------------------------------------
// PR-2 / FEAT-005 — evaluate → apply (horizon + tierFallback)
//
// `evaluate_quota` is pure per-bucket facts (never ask). This layer resolves
// ask / wait / stop / unavailable from remaining work + `routing.tierFallback`.
// Seq and wave share this function (parity); callers must destructure results
// exhaustively.
// ---------------------------------------------------------------------------

/// Snapshot of remaining runnable work for apply (not passed to evaluate).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemainingWorkSnapshot {
    /// At least one remaining task can run on a rung/provider outside the
    /// evaluate-unavailable set. Spillover is never a working rung.
    ///
    /// Used only for **scoped** unavailable horizon decisions (Proceed vs
    /// Stop/Ask). Account-binding lows (`session` / `weekly_all`) ignore this
    /// flag beyond the stop horizon — those buckets are shared across Claude
    /// rungs, so same-provider siblings are not an alternative.
    pub other_rungs_runnable: bool,
    /// Remaining work includes review-class tasks.
    pub has_review: bool,
    /// Highest difficulty among remaining tasks: `"low"` / `"medium"` / `"high"`.
    pub max_difficulty: Option<&'static str>,
    /// Remaining work includes explicit `tasks.model` forced routes.
    pub has_forced: bool,
}

/// Account-side action after apply (may coexist with unavailable rungs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaAccountAction {
    /// No account wait/stop/ask — proceed (possibly with rung exclusion).
    Proceed,
    /// Sleep `secs` (already capped at [`MAX_WAIT_SECS`] when in the middle band).
    Wait { secs: u64 },
    /// Stop this PRD (`in_progress` → `todo`). Next-PRD inherit of unavailable is PR-3.
    Stop,
    /// Operator forbade downgrade; ask (TTL > 0 would sleep — PR-3).
    Ask,
    /// Ask with TTL 0: no sleep, no continue (soft-stop for operator).
    Defer,
}

/// Combined apply result. Account wait/stop can coexist with rung unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaApplyResult {
    /// Rungs to place on the proto-channel (factory / allowing tierFallback).
    pub unavailable: Vec<(Provider, CapabilityTier)>,
    pub account: QuotaAccountAction,
}

/// Whether factory/allowing `tierFallback` permits auto-unavailable for `work`.
///
/// `None` (explicit JSON null) → forbade. Narrower `maxDifficulty`, or
/// `includeReview: false` with review work → forbade.
///
/// `includeForced: false` is **not** a global forbid: factory defaults keep
/// auto-unavailable for eligible rungs even when some todos carry
/// `tasks.model`. Forced-model tasks stay selectable until the PR-3
/// family-match walker defers them per-task.
pub fn tier_fallback_allows(fb: Option<&TierFallback>, work: &RemainingWorkSnapshot) -> bool {
    let Some(fb) = fb else {
        return false;
    };
    if work.has_review && !fb.include_review {
        return false;
    }
    // `include_forced` / `has_forced` are PR-3 per-task family-match, not a
    // whole-PRD opt-out — factory defaults still auto-unavailable here.
    let fb_rank = difficulty_rank_str(&fb.max_difficulty).unwrap_or(0);
    let work_rank = work
        .max_difficulty
        .and_then(difficulty_rank_str)
        .unwrap_or(0);
    work_rank <= fb_rank
}

fn difficulty_rank_str(d: &str) -> Option<usize> {
    match d.trim().to_ascii_lowercase().as_str() {
        "low" => Some(0),
        "medium" => Some(1),
        "high" => Some(2),
        _ => None,
    }
}

/// Pure apply: resolve ask/wait/stop/unavailable from evaluate outputs.
///
/// Does **not** perform I/O, sleep, or proto-channel mutation — callers replace
/// `IterationContext.unavailable_rungs` on successful evaluate and execute
/// [`QuotaAccountAction`].
pub fn apply_quota(
    eval: &QuotaEval,
    buckets: &[QuotaBucket],
    policy: &UsagePolicy,
    tier_fallback: Option<&TierFallback>,
    work: &RemainingWorkSnapshot,
) -> QuotaApplyResult {
    let allows = tier_fallback_allows(tier_fallback, work);

    let mut unavailable: Vec<(Provider, CapabilityTier)> = Vec::new();
    let mut pending_ask = false;
    let mut scoped_wait_resets: Vec<u64> = Vec::new();

    if !eval.unavailable.is_empty() {
        if allows {
            for rung in &eval.unavailable {
                if !unavailable.contains(rung) {
                    unavailable.push(*rung);
                }
            }
            // Only-frontier-left: still may need wait/stop from scoped resets.
            if !work.other_rungs_runnable {
                scoped_wait_resets.extend(reset_secs_for_unavailable(eval, buckets));
            }
        } else if work.other_rungs_runnable {
            pending_ask = true;
        } else {
            // Forbade + nothing else runnable → horizon wait/stop, not ask.
            scoped_wait_resets.extend(reset_secs_for_unavailable(eval, buckets));
        }
    }

    let mut explicit_stop = false;
    let mut explicit_wait: Vec<u64> = Vec::new();
    let mut explicit_ask = false;
    let mut spend_stop = false;
    let mut account_wait_resets: Vec<u64> = Vec::new();

    for low in &eval.account_low {
        if let Some(action) = explicit_on_low_for_bucket(policy, &low.bucket_id, buckets) {
            match action {
                OnLowAction::Stop => explicit_stop = true,
                OnLowAction::Wait => {
                    if let Some(secs) = low.reset_secs {
                        explicit_wait.push(secs);
                    }
                }
                OnLowAction::Ask => explicit_ask = true,
                OnLowAction::Ignore | OnLowAction::Unavailable => {}
            }
            continue;
        }

        // Spend / amount buckets: stop only when remainingAmount ≤ 0.
        if is_spend_kind(&low.kind) || account_low_is_amount_only(low, buckets) {
            if low.remaining <= 0.0 {
                spend_stop = true;
            }
            continue;
        }

        if let Some(secs) = low.reset_secs {
            account_wait_resets.push(secs);
        } else {
            // Unknown reset → treat as beyond horizon when low.
            account_wait_resets.push(policy.stop_if_reset_beyond_hours.saturating_mul(3600) + 1);
        }
    }

    // Merge wait candidates; among multiple low wait buckets use the LATEST reset.
    let has_account_binding_wait = !account_wait_resets.is_empty();
    let mut wait_resets = account_wait_resets;
    wait_resets.extend(scoped_wait_resets);
    wait_resets.extend(explicit_wait);
    let latest = wait_resets.iter().copied().max();

    let wait_within_secs = policy.wait_if_reset_within_minutes.saturating_mul(60);
    let stop_beyond_secs = policy.stop_if_reset_beyond_hours.saturating_mul(3600);

    let mut account = QuotaAccountAction::Proceed;

    if spend_stop || explicit_stop {
        account = QuotaAccountAction::Stop;
    } else if let Some(secs) = latest {
        if secs <= wait_within_secs {
            account = QuotaAccountAction::Wait { secs };
        } else if secs <= stop_beyond_secs {
            account = QuotaAccountAction::Wait {
                secs: secs.min(MAX_WAIT_SECS),
            };
        } else if has_account_binding_wait {
            // Account-binding (session / weekly_all) beyond horizon: every
            // Claude rung shares that bucket, so other_rungs_runnable is not a
            // working alternative — Stop (do not keep burning quota).
            account = QuotaAccountAction::Stop;
        } else if !work.other_rungs_runnable {
            // Scoped-only beyond horizon and nothing else can run → Stop.
            // other_rungs_runnable ignores spillover (caller's responsibility).
            account = QuotaAccountAction::Stop;
        } else if pending_ask || explicit_ask {
            account = ask_or_defer(policy.ask_ttl_minutes);
        } else {
            // Scoped unavailable beyond horizon; other work remains → Proceed
            // with exclusions only (factory / allowing tierFallback).
            account = QuotaAccountAction::Proceed;
        }
    } else if pending_ask || explicit_ask {
        account = ask_or_defer(policy.ask_ttl_minutes);
    }

    // stop beats ask
    if matches!(account, QuotaAccountAction::Ask | QuotaAccountAction::Defer)
        && (spend_stop || explicit_stop)
    {
        account = QuotaAccountAction::Stop;
    }
    if matches!(account, QuotaAccountAction::Ask | QuotaAccountAction::Defer)
        && matches!(latest, Some(secs) if secs > stop_beyond_secs)
        && !work.other_rungs_runnable
    {
        account = QuotaAccountAction::Stop;
    }

    QuotaApplyResult {
        unavailable,
        account,
    }
}

fn ask_or_defer(ask_ttl_minutes: u64) -> QuotaAccountAction {
    if ask_ttl_minutes == 0 {
        QuotaAccountAction::Defer
    } else {
        QuotaAccountAction::Ask
    }
}

fn reset_secs_for_unavailable(eval: &QuotaEval, buckets: &[QuotaBucket]) -> Vec<u64> {
    let mut out = Vec::new();
    for (id, beval) in &eval.per_bucket {
        if !matches!(beval, BucketEval::Unavailable { .. }) {
            continue;
        }
        if let Some(bucket) = buckets.iter().find(|b| b.id == *id)
            && let Some(secs) = parse_bucket_reset_secs(bucket)
        {
            out.push(secs);
        }
    }
    out
}

fn parse_bucket_reset_secs(bucket: &QuotaBucket) -> Option<u64> {
    let raw = bucket.resets_at.as_deref()?;
    let ts = chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|dt| dt.and_utc())
        })?;
    let delta = ts.signed_duration_since(chrono::Utc::now()).num_seconds();
    Some(if delta <= 0 { 0 } else { delta as u64 })
}

fn explicit_on_low_for_bucket(
    policy: &UsagePolicy,
    bucket_id: &str,
    buckets: &[QuotaBucket],
) -> Option<OnLowAction> {
    let bucket = buckets.iter().find(|b| b.id == bucket_id)?;
    policy.rules.iter().find_map(|rule| {
        let kind_ok = rule.kind.as_ref().is_none_or(|k| k == &bucket.kind);
        let id_ok = rule.id.as_ref().is_none_or(|i| i == &bucket.id);
        if kind_ok && id_ok {
            // when predicates: only severity opt-in (same as evaluate).
            if let Some(when) = rule.when.as_ref() {
                if let Some(want) = when.get("severity").and_then(|v| v.as_str()) {
                    if bucket.severity.as_deref() != Some(want) {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            Some(rule.on_low)
        } else {
            None
        }
    })
}

fn is_spend_kind(kind: &str) -> bool {
    matches!(
        kind,
        "spend" | "dollars" | "credits" | "tokens" | "extra_usage"
    )
}

fn account_low_is_amount_only(low: &AccountLowInput, buckets: &[QuotaBucket]) -> bool {
    let Some(bucket) = buckets.iter().find(|b| b.id == low.bucket_id) else {
        return false;
    };
    let has_percent = bucket
        .measurements
        .iter()
        .any(|m| matches!(m.unit, crate::loop_engine::quota::MeasurementUnit::Percent));
    !has_percent
        && bucket.measurements.iter().any(|m| {
            matches!(
                m.unit,
                crate::loop_engine::quota::MeasurementUnit::Dollars
                    | crate::loop_engine::quota::MeasurementUnit::Credits
                    | crate::loop_engine::quota::MeasurementUnit::Tokens
            )
        })
}

/// Evaluate buckets and apply horizon / tierFallback. Pure besides the
/// `now`-dependent reset math inside evaluate/apply.
pub fn evaluate_and_apply_quota(
    buckets: &[QuotaBucket],
    policy: &UsagePolicy,
    remaining_min: u8,
    tier_fallback: Option<&TierFallback>,
    work: &RemainingWorkSnapshot,
) -> (QuotaEval, QuotaApplyResult) {
    let eval = evaluate_quota(buckets, policy, remaining_min);
    let applied = apply_quota(&eval, buckets, policy, tier_fallback, work);
    (eval, applied)
}

/// Replace `unavailable_rungs` from an apply result (successful evaluate path).
///
/// Call only after a successful usage/evaluate cycle. On API failure the caller
/// must **not** invoke this — keep the previous snapshot.
pub fn replace_unavailable_rungs(
    unavailable_rungs: &mut HashSet<(Provider, CapabilityTier)>,
    applied: &QuotaApplyResult,
) {
    unavailable_rungs.clear();
    unavailable_rungs.extend(applied.unavailable.iter().copied());
}

/// Inputs to [`account_quota_preflight`] / [`account_quota_preflight_inner`].
/// Destructured exhaustively (no `..`) — seq/wave parity lock.
pub struct QuotaPreflightParams<'a> {
    /// Remaining-percent floor (0–100).
    pub threshold: u8,
    pub tasks_dir: &'a Path,
    pub fallback_wait: u64,
    pub policy: &'a UsagePolicy,
    pub tier_fallback: Option<&'a TierFallback>,
    /// When false (`LOOP_USAGE_CHECK_ENABLED=false`): skip evaluate/replace/
    /// wait/stop and keep the proto-channel snapshot.
    pub execute_account_action: bool,
    pub unavailable_rungs: &'a mut HashSet<(Provider, CapabilityTier)>,
    /// Preview of remaining work used by apply (caller computes from DB +
    /// evaluate.unavailable, or passes a hermetic fixture).
    pub work: &'a RemainingWorkSnapshot,
    /// Injected buckets when `Some`. `None` means API/credentials failed —
    /// keep the proto-channel snapshot.
    pub buckets: Option<&'a [QuotaBucket]>,
    /// Account-binding remaining percent from UsageInfo (org fallback path).
    pub account_remaining: Option<f64>,
    pub account_reset_at: Option<&'a str>,
}

/// Inputs to [`run_account_quota_gate`]. Exhaustive destructure at the call
/// site is not required (this is the production loader wrapper); the inner
/// [`account_quota_preflight_inner`] carries the parity lock.
pub struct RunAccountQuotaGateParams<'a> {
    pub conn: &'a mut Connection,
    pub task_prefix: Option<&'a str>,
    pub run_id: &'a str,
    pub unavailable_rungs: &'a mut HashSet<(Provider, CapabilityTier)>,
    /// Permanent promote_once pins — same map consulted by
    /// [`super::pre_spawn::compute_quota_excluded_ids`]. Snapshot must honor
    /// these so pinned Grok/Codex work counts as other-rung runnable.
    pub runner_overrides: &'a HashMap<String, RunnerKind>,
    pub models: &'a crate::loop_engine::model::ResolvedModelsConfig,
    pub policy: &'a UsagePolicy,
    pub tier_fallback: Option<&'a TierFallback>,
    pub threshold: u8,
    pub tasks_dir: &'a Path,
    pub fallback_wait: u64,
    /// When false (`LOOP_USAGE_CHECK_ENABLED=false`): skip OAuth/usage load,
    /// keep the proto-channel snapshot, and do not sleep/stop/defer.
    pub execute_account_action: bool,
}

/// Load usage, evaluate+apply, refresh proto-channel, optionally wait/stop.
///
/// Shared by sequential (`iteration.rs`) and wave (`wave_orchestration.rs`)
/// so both paths produce the same decision for the same buckets+policy.
/// When `execute_account_action` is false (`LOOP_USAGE_CHECK_ENABLED=false`),
/// returns [`UsageCheckResult::Skipped`] without calling the usage loader —
/// proto-channel snapshot is kept. Dual predicate: pre-gate I/O requires
/// env ∧ Claude enabled (`UsageParams.enabled`).
///
/// On [`QuotaAccountAction::Stop`], resets `in_progress` → `todo` under
/// `task_prefix` before returning [`UsageCheckResult::StopSignaled`].
pub fn run_account_quota_gate(params: RunAccountQuotaGateParams<'_>) -> UsageCheckResult {
    let threshold = params.threshold;
    let load = || load_usage_info_with_threshold(threshold);
    run_account_quota_gate_inner(params, &load)
}

/// Hermetic core of [`run_account_quota_gate`]. Production wires
/// [`load_usage_info_with_threshold`]; tests inject a spy so
/// `LOOP_USAGE_CHECK_ENABLED=false` can assert zero OAuth/usage I/O.
pub fn run_account_quota_gate_inner(
    params: RunAccountQuotaGateParams<'_>,
    load_usage: LoadUsageFn<'_>,
) -> UsageCheckResult {
    let RunAccountQuotaGateParams {
        conn,
        task_prefix,
        run_id,
        unavailable_rungs,
        runner_overrides,
        models,
        policy,
        tier_fallback,
        threshold,
        tasks_dir,
        fallback_wait,
        execute_account_action,
    } = params;

    // Dual predicate (pre-iteration): env off ⇒ no load_usage_info / OAuth GET.
    // Keep proto-channel snapshot; do not document a replace-on-disabled exception.
    if !execute_account_action {
        return UsageCheckResult::Skipped;
    }

    let usage = load_usage();
    // Re-ingest OAuth HUD with the run's ResolvedModelsConfig so extra-mark
    // sees pins (e.g. frontier→opus). Builtin snapshot on info.buckets alone
    // would leave frontier selectable after an Opus HUD low (WIRE-FIX-001).
    let run_buckets = usage
        .as_ref()
        .map(|info| buckets_for_run_models(info, models));
    let (buckets, account_remaining, account_reset_at) = match (&usage, &run_buckets) {
        (Some(info), Some(owned)) => {
            if let Some(banner) = info.remaining_banner.as_deref() {
                eprintln!("{banner}");
            } else {
                eprintln!(
                    "{}% left (floor {}%)",
                    format_remaining_pct(info.percentage),
                    threshold
                );
            }
            (
                if owned.is_empty() {
                    None
                } else {
                    Some(owned.as_slice())
                },
                Some(info.percentage),
                info.reset_at.as_deref(),
            )
        }
        _ => (None, None, None),
    };

    let (work, horizon_stop) = if let Some(buckets) = buckets {
        let eval = evaluate_quota(buckets, policy, threshold);
        let work = compute_remaining_work_snapshot(
            conn,
            task_prefix,
            models,
            &eval.unavailable,
            runner_overrides,
        );
        let applied = apply_quota(&eval, buckets, policy, tier_fallback, &work);
        let horizon_stop = matches!(applied.account, QuotaAccountAction::Stop);
        (work, horizon_stop)
    } else {
        // No buckets (API fail or org-only): assume other work can run so we
        // do not spuriously Stop; keep proto-channel snapshot.
        (
            RemainingWorkSnapshot {
                other_rungs_runnable: true,
                ..RemainingWorkSnapshot::default()
            },
            false,
        )
    };

    let result = account_quota_preflight(QuotaPreflightParams {
        threshold,
        tasks_dir,
        fallback_wait,
        policy,
        tier_fallback,
        execute_account_action,
        unavailable_rungs,
        work: &work,
        buckets,
        account_remaining,
        account_reset_at,
    });

    if horizon_stop && matches!(result, UsageCheckResult::StopSignaled) {
        // Horizon Stop: park in_progress back to todo for this PRD.
        let prefix = task_prefix.unwrap_or("");
        reset_in_progress_tasks(conn, run_id, prefix, "quota horizon stop");
    }

    result
}

/// Production entry: build wait closure and run [`account_quota_preflight_inner`].
pub fn account_quota_preflight(params: QuotaPreflightParams<'_>) -> UsageCheckResult {
    let threshold = params.threshold;
    let tasks_dir = params.tasks_dir;
    let wait = |secs: u64| -> bool {
        let probe = || {
            if let Some(info) = load_usage_info_with_threshold(threshold) {
                if usage_suggests_lifted(&info, threshold, false) {
                    return true;
                }
                if let Some(r) = info.reset_at.as_deref() {
                    return estimate_reset_seconds(r) == Some(0);
                }
            }
            false
        };
        wait_for_usage_reset(secs, tasks_dir, Some(&probe))
    };
    account_quota_preflight_inner(params, &wait)
}

/// Hermetic core of the PR-2 quota preflight (evaluate → apply → proto-channel
/// → optional account wait/stop/defer). Same buckets+policy ⇒ same decision
/// for sequential and wave callers (exhaustive destructure, no `..`).
pub fn account_quota_preflight_inner(
    params: QuotaPreflightParams<'_>,
    wait: WaitFn<'_>,
) -> UsageCheckResult {
    let QuotaPreflightParams {
        threshold,
        tasks_dir: _,
        fallback_wait,
        policy,
        tier_fallback,
        execute_account_action,
        unavailable_rungs,
        work,
        buckets,
        account_remaining,
        account_reset_at,
    } = params;

    // Env-disabled pre-gate: keep proto-channel snapshot; no evaluate/replace.
    if !execute_account_action {
        return UsageCheckResult::Skipped;
    }

    let applied = match buckets {
        Some(buckets) => {
            let (_eval, applied) =
                evaluate_and_apply_quota(buckets, policy, threshold, tier_fallback, work);
            replace_unavailable_rungs(unavailable_rungs, &applied);
            Some(applied)
        }
        None => {
            // API fail — keep snapshot; do not clear.
            None
        }
    };

    if let Some(applied) = applied {
        return execute_quota_account_action(&applied.account, wait);
    }

    // Org / no-buckets fallback: legacy remaining-percent gate.
    let Some(remaining) = account_remaining else {
        return UsageCheckResult::Skipped;
    };
    if remaining > f64::from(threshold) {
        return UsageCheckResult::BelowThreshold;
    }
    // None = unknown → fallback_wait; Some(0) = ready now (pass through).
    let wait_secs = account_reset_at
        .and_then(estimate_reset_seconds)
        .unwrap_or(fallback_wait);
    if wait_secs == 0 {
        return UsageCheckResult::BelowThreshold;
    }
    if wait(wait_secs) {
        UsageCheckResult::WaitedAndReset
    } else {
        UsageCheckResult::StopSignaled
    }
}

fn execute_quota_account_action(action: &QuotaAccountAction, wait: WaitFn<'_>) -> UsageCheckResult {
    match action {
        QuotaAccountAction::Proceed => UsageCheckResult::BelowThreshold,
        QuotaAccountAction::Wait { secs } => {
            // secs==0 is ready-now (past/now reset). Do NOT treat as unknown
            // and substitute fallback_wait (300s) — wait_for_usage_reset
            // already treats 0 as immediate resume.
            if *secs == 0 {
                return UsageCheckResult::BelowThreshold;
            }
            if wait(*secs) {
                UsageCheckResult::WaitedAndReset
            } else {
                UsageCheckResult::StopSignaled
            }
        }
        QuotaAccountAction::Stop => UsageCheckResult::StopSignaled,
        QuotaAccountAction::Ask => {
            // PR-3 would TTL-sleep; PR-2 factory default TTL is 0 → Defer.
            UsageCheckResult::Deferred
        }
        QuotaAccountAction::Defer => UsageCheckResult::Deferred,
    }
}

/// Build a [`RemainingWorkSnapshot`] from todo rows under `task_prefix`.
///
/// `unavailable_preview` is typically `evaluate_quota(...).unavailable`.
/// Spillover is not treated as a working rung: resolution uses an empty
/// blackout set so spillover reroute cannot invent runnable capacity.
///
/// Effective provider/tier matches [`super::pre_spawn::compute_quota_excluded_ids`]:
/// a `runner_overrides` pin (promote_once) owns the provider; tier still comes
/// from resolve with empty blackouts. Without this, pinned Grok/Codex work is
/// miscounted as Claude and can spuriously horizon-Stop.
pub fn compute_remaining_work_snapshot(
    conn: &Connection,
    task_prefix: Option<&str>,
    models: &crate::loop_engine::model::ResolvedModelsConfig,
    unavailable_preview: &[(Provider, CapabilityTier)],
    runner_overrides: &HashMap<String, RunnerKind>,
) -> RemainingWorkSnapshot {
    let like_prefix = task_prefix.unwrap_or("");
    let mut stmt = match conn.prepare(
        "SELECT id, model, difficulty FROM tasks \
         WHERE status IN ('todo', 'in_progress') AND id LIKE ?1 || '%' \
           AND archived_at IS NULL",
    ) {
        Ok(s) => s,
        Err(_) => return RemainingWorkSnapshot::default(),
    };
    let rows = match stmt.query_map(rusqlite::params![like_prefix], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return RemainingWorkSnapshot::default(),
    };

    let empty_blackouts = HashSet::new();
    let unavailable: HashSet<(Provider, CapabilityTier)> =
        unavailable_preview.iter().copied().collect();

    let mut other_rungs_runnable = false;
    let mut has_review = false;
    let mut has_forced = false;
    let mut max_rank: Option<usize> = None;

    for (id, model_col, difficulty) in rows.flatten() {
        if model_col.as_ref().is_some_and(|m| !m.is_empty()) {
            has_forced = true;
        }
        // Review-class heuristic (no task_type column): id tokens.
        if id.contains("REVIEW") || id.contains("CODE-REVIEW") {
            has_review = true;
        }
        if let Some(r) = difficulty.as_deref().and_then(difficulty_rank_str) {
            max_rank = Some(max_rank.map_or(r, |m| m.max(r)));
        }
        let plan = crate::loop_engine::model::resolve_execution_plan(
            &crate::loop_engine::model::PlanContext {
                task_id: &id,
                task_model: model_col.as_deref(),
                difficulty: difficulty.as_deref(),
                models,
                provider_blackouts: &empty_blackouts,
            },
        );
        // Same effective-provider rule as compute_quota_excluded_ids: pin wins
        // for provider; tier from resolve (empty blackouts — no spillover).
        let (effective_provider, effective_tier) = match runner_overrides.get(&id) {
            Some(kind) => (provider_of_runner(*kind), plan.tier),
            None => (plan.provider, plan.tier),
        };
        if !unavailable.contains(&(effective_provider, effective_tier)) {
            other_rungs_runnable = true;
        }
    }

    let max_difficulty = match max_rank {
        Some(0) => Some("low"),
        Some(1) => Some("medium"),
        Some(2) => Some("high"),
        _ => None,
    };

    RemainingWorkSnapshot {
        other_rungs_runnable,
        has_review,
        max_difficulty,
        has_forced,
    }
}

/// `RunnerKind → Provider` identity (same mapping as pre_spawn / post_output).
fn provider_of_runner(kind: RunnerKind) -> Provider {
    match kind {
        RunnerKind::Claude => Provider::Claude,
        RunnerKind::Grok => Provider::Grok,
        RunnerKind::Codex => Provider::Codex,
    }
}

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

fn format_remaining_pct(remaining: f64) -> String {
    if (remaining - remaining.round()).abs() < f64::EPSILON {
        format!("{}", remaining.round() as i64)
    } else {
        format!("{remaining:.1}")
    }
}

/// Check usage and wait if remaining is at or below the floor.
///
/// Orchestrates:
/// 1. `load_usage_info` (creds + refresh + OAuth/org usage API)
/// 2. If remaining ≤ floor, wait for reset with API early-lift probe
///
/// Returns the result of the check-and-wait cycle.
pub(crate) fn check_and_wait(
    threshold: u8,
    tasks_dir: &Path,
    fallback_wait: u64,
) -> UsageCheckResult {
    // Pass live remaining-min into parse so reset_at uses the same floor as
    // the remaining compare below (not a hardcoded 8).
    let usage = match load_usage_info_with_threshold(threshold) {
        Some(u) => u,
        None => {
            // Distinguish "no creds" from "API failed" is best-effort: load
            // already degraded; surface as skipped when nothing usable.
            return UsageCheckResult::Skipped;
        }
    };

    if let Some(banner) = usage.remaining_banner.as_deref() {
        eprintln!("{banner}");
    } else {
        eprintln!(
            "{}% left (floor {}%)",
            format_remaining_pct(usage.percentage),
            threshold
        );
    }

    // Proceed when remaining > floor (old used≥92 ≡ remaining≤8).
    if usage.percentage > f64::from(threshold) {
        return UsageCheckResult::BelowThreshold;
    }

    // Above threshold: wait. Some(0) = ready now; None = unknown → fallback.
    let wait_secs = usage
        .reset_at
        .as_deref()
        .and_then(estimate_reset_seconds)
        .unwrap_or(fallback_wait);

    let probe = || {
        if let Some(info) = load_usage_info_with_threshold(threshold) {
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
    use crate::loop_engine::model::{FABLE_MODEL, OPUS_MODEL};
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
    fn test_decide_spend_stop_ignores_embedded_session_reset_in_banner() {
        // Live CLI: spend/credits banner also carries `resets 3:40am`. Parsing
        // that into output_secs must not demote StopSpend into a multi-hour Wait
        // (REVIEW-001 suite hang when a wave accidentally hit real Claude).
        let output = "You've hit your individual spend limit · run /usage-credits \
            to raise it, or visit claude.ai/admin-settings/usage · your session \
            limit resets 3:40am (America/Los_Angeles)";
        let output_secs = parse_reset_from_output(output);
        assert!(
            output_secs.is_some(),
            "precondition: banner still yields a parsed session reset"
        );
        let action = decide_account_rate_limit(None, output_secs, output, false, 300, 3600);
        assert_eq!(action, RateLimitAction::StopSpend);
        let spillover = decide_account_rate_limit(None, output_secs, output, true, 300, 3600);
        assert_eq!(
            spillover,
            RateLimitAction::StopSpend,
            "spend+embedded reset must not Blackout under spillover either"
        );
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

    // --- PR-1 / FR-002 rung-scoped RateLimit Wait override ---

    const FABLE_CLI: &str =
        "You've reached your Fable limit. To continue, switch models with /model.";
    const SIX_DAYS_SECS: u64 = 6 * 24 * 3600;

    #[test]
    fn test_rung_scoped_predicate_narrow() {
        assert!(is_rung_scoped_rate_limit_message(FABLE_CLI));
        assert!(is_rung_scoped_rate_limit_message(
            "You've reached your Opus limit"
        ));
        assert!(is_rung_scoped_rate_limit_message(
            "You've reached your Sonnet limit"
        ));
        // Unanchored "switch models" (no reached/limit on the same line) is
        // not the 3600 override — docs/help copy must not trip it.
        assert!(!is_rung_scoped_rate_limit_message(
            "Please switch models to continue"
        ));
        // Plain session / account copy — ordinary RateLimit, no 3600 override.
        // (e)(f)
        assert!(!is_rung_scoped_rate_limit_message(
            "You've reached your session limit"
        ));
        assert!(!is_rung_scoped_rate_limit_message(
            "You've hit your limit · resets 4pm"
        ));
        // `/model` alone is not sufficient (no model token, no anchored
        // "switch models"). (h)
        assert!(!is_rung_scoped_rate_limit_message(
            "Try /model to pick another model"
        ));
        // Account banner + later commentary mentioning "switch models" must
        // not take the rung-scoped override (line-scoped, not whole-capture).
        let mixed = "You've hit your limit · resets 4pm\n\
             Docs: when Fable is exhausted, switch models with /model.";
        assert!(!is_rung_scoped_rate_limit_message(mixed));
        // Hyphen/underscore are NOT word boundaries — model ids in ordinary
        // session RateLimit stdout must not force Wait 3600.
        let opus_banner = format!("model: {OPUS_MODEL}\nYou've hit your limit · resets 4pm");
        assert!(
            !is_rung_scoped_rate_limit_message(&opus_banner),
            "OPUS_MODEL must not match token opus"
        );
        let fable_banner = format!("model: {FABLE_MODEL}\nYou've hit your limit · resets 4pm");
        assert!(
            !is_rung_scoped_rate_limit_message(&fable_banner),
            "FABLE_MODEL must not match token fable"
        );
        let opus_later = format!("use {OPUS_MODEL} for this task; you hit a rate limit later");
        assert!(
            !is_rung_scoped_rate_limit_message(&opus_later),
            "hyphenated model id + later 'limit' must not match"
        );
        assert!(
            !is_rung_scoped_rate_limit_message(
                "model: claude_opus_5\nYou've hit your limit · resets 4pm"
            ),
            "underscore must not count as a word boundary either"
        );
    }

    #[test]
    fn test_decide_switch_models_on_later_line_keeps_output_secs() {
        let mixed = "You've hit your limit · resets 4pm\n\
             Commentary: switch models with /model per the docs.";
        let action = decide_account_rate_limit(None, Some(500), mixed, false, 300, 3600);
        assert_eq!(
            action,
            RateLimitAction::Wait { secs: 500 },
            "switch models on a later line must not force Wait 3600"
        );
    }

    #[test]
    fn test_model_token_window_floors_utf8_char_boundary() {
        // "opus" (ASCII) + 63 ASCII bytes + € (U+20AC, 3 UTF-8 bytes).
        // Without floor_char_boundary, end+64 lands on the second byte of € and
        // `lower[end..window_end]` panics. With the floor, the slice stops before €.
        let mid_codepoint = format!("opus{}{}", "a".repeat(63), "€");
        assert!(!is_rung_scoped_rate_limit_message(&mid_codepoint));

        // Positive: "limit" still found inside the floored window before the
        // multi-byte char that would otherwise split the raw end+64 index.
        let with_limit = format!("opus limit{}{}", "a".repeat(50), "€");
        assert!(is_rung_scoped_rate_limit_message(&with_limit));
    }

    #[test]
    fn test_decide_fable_ignores_api_secs_waits_blackout_fallback() {
        // (b) api_secs must be populated — None is not sufficient to prove ignore.
        let action =
            decide_account_rate_limit(Some(SIX_DAYS_SECS), Some(500), FABLE_CLI, false, 300, 3600);
        assert_eq!(
            action,
            RateLimitAction::Wait { secs: 3600 },
            "Fable phrasing must Wait blackout_fallback_secs, ignoring api/output secs \
             and never falling through to usage_fallback_wait 300"
        );
    }

    #[test]
    fn test_decide_fable_spillover_still_waits_never_blackout() {
        // (c) spillover_enabled must not Blackout on rung-scoped phrasing.
        let action =
            decide_account_rate_limit(Some(SIX_DAYS_SECS), None, FABLE_CLI, true, 300, 3600);
        assert_eq!(action, RateLimitAction::Wait { secs: 3600 });
    }

    #[test]
    fn test_decide_opus_limit_also_takes_3600_override() {
        // Override is not contains("fable") only.
        let action = decide_account_rate_limit(
            Some(SIX_DAYS_SECS),
            None,
            "You've reached your Opus limit",
            false,
            300,
            3600,
        );
        assert_eq!(action, RateLimitAction::Wait { secs: 3600 });
    }

    #[test]
    fn test_decide_plain_session_limit_keeps_api_secs() {
        // (f) plain reached-your-limit without model token / switch models.
        let action = decide_account_rate_limit(
            Some(7200),
            None,
            "You've reached your session limit",
            false,
            300,
            3600,
        );
        assert_eq!(action, RateLimitAction::Wait { secs: 7200 });
    }

    #[test]
    fn test_decide_plain_session_limit_spillover_may_blackout() {
        let action = decide_account_rate_limit(
            Some(7200),
            None,
            "You've reached your session limit",
            true,
            300,
            3600,
        );
        assert_eq!(action, RateLimitAction::Blackout { secs: 7200 });
    }

    #[test]
    fn test_decide_hit_your_limit_resets_4pm_no_3600_override() {
        // (e) account copy must not take the rung-scoped override.
        let action = decide_account_rate_limit(
            None,
            Some(500),
            "You've hit your limit · resets 4pm",
            false,
            300,
            3600,
        );
        assert_eq!(action, RateLimitAction::Wait { secs: 500 });
    }

    #[test]
    fn test_decide_model_id_plus_hit_your_limit_still_blackouts_under_spillover() {
        // Full CLI stdout often embeds OPUS_MODEL / FABLE_MODEL alongside
        // ordinary account RateLimit copy. Hyphen must not create a false
        // rung-scoped Wait 3600; spillover still Blackouts.
        let with_opus = format!("Running {OPUS_MODEL}\nYou've hit your limit · resets 4pm");
        let action = decide_account_rate_limit(None, Some(500), &with_opus, true, 300, 3600);
        assert_eq!(
            action,
            RateLimitAction::Blackout { secs: 500 },
            "model id + account hit-your-limit must use output_secs Blackout, not Wait 3600"
        );

        let with_fable = format!("Running {FABLE_MODEL}\nYou've hit your limit · resets 4pm");
        let action = decide_account_rate_limit(Some(7200), Some(500), &with_fable, true, 300, 3600);
        assert_eq!(
            action,
            RateLimitAction::Blackout { secs: 7200 },
            "FABLE_MODEL + account copy must prefer api_secs Blackout, not Wait 3600"
        );
    }

    #[test]
    fn test_decide_slash_model_alone_no_3600_override() {
        // (h) `/model` alone keeps api_secs / may Blackout.
        let action = decide_account_rate_limit(
            Some(7200),
            None,
            "Try /model to pick another model",
            true,
            300,
            3600,
        );
        assert_eq!(action, RateLimitAction::Blackout { secs: 7200 });
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
    fn test_parse_reset_from_output_sep_month_token_stays_none() {
        // PR-1: do NOT add dated month-name parsing. Fable 3600 comes from the
        // phrasing override, not from parsing a weekly reset out of CLI text.
        assert!(
            parse_reset_from_output("You've reached your Fable limit · resets Sep 12, 12:59am")
                .is_none()
        );
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

    // --- PR-2 apply_quota / horizon / tierFallback ---

    fn pct_bucket(
        id: &str,
        kind: &str,
        remaining: f64,
        reset_in_secs: i64,
        rungs: Option<Vec<(Provider, CapabilityTier)>>,
    ) -> QuotaBucket {
        let resets_at =
            (chrono::Utc::now() + chrono::Duration::seconds(reset_in_secs)).to_rfc3339();
        QuotaBucket {
            id: id.into(),
            kind: kind.into(),
            label: String::new(),
            measurements: vec![crate::loop_engine::quota::Measurement {
                remaining,
                unit: crate::loop_engine::quota::MeasurementUnit::Percent,
            }],
            resets_at: Some(resets_at),
            severity: None,
            is_active: None,
            rungs,
        }
    }

    fn factory_fb() -> TierFallback {
        TierFallback {
            max_difficulty: "high".into(),
            include_review: true,
            include_forced: false,
        }
    }

    #[test]
    fn apply_factory_rung_low_other_runnable_is_unavailable_not_ask() {
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            6 * 24 * 3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        let policy = UsagePolicy::default();
        let eval = evaluate_quota(std::slice::from_ref(&frontier), &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: true,
            max_difficulty: Some("high"),
            ..RemainingWorkSnapshot::default()
        };
        let applied = apply_quota(&eval, &[frontier], &policy, Some(&factory_fb()), &work);
        assert_eq!(
            applied.unavailable,
            vec![(Provider::Claude, CapabilityTier::Frontier)]
        );
        assert_eq!(applied.account, QuotaAccountAction::Proceed);
    }

    #[test]
    fn apply_factory_with_forced_model_still_unavailable_proceed() {
        // Overflow / explicit tasks.model sets has_forced. Factory
        // includeForced:false must NOT globally forbid unavailable — leave
        // forced tasks selectable (PR-3 walker) and proceed on standard.
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            6 * 24 * 3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        let policy = UsagePolicy::default();
        let eval = evaluate_quota(std::slice::from_ref(&frontier), &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: true,
            has_forced: true,
            max_difficulty: Some("high"),
            ..RemainingWorkSnapshot::default()
        };
        let fb = factory_fb();
        assert!(
            !fb.include_forced,
            "factory includeForced must stay false (PR-3 per-task)"
        );
        assert!(
            tier_fallback_allows(Some(&fb), &work),
            "factory defaults must still allow unavailable when has_forced"
        );
        let applied = apply_quota(&eval, &[frontier], &policy, Some(&fb), &work);
        assert_eq!(
            applied.unavailable,
            vec![(Provider::Claude, CapabilityTier::Frontier)]
        );
        assert_eq!(
            applied.account,
            QuotaAccountAction::Proceed,
            "factory + has_forced must not Defer (would claim tierFallback forbade)"
        );
    }

    #[test]
    fn apply_include_review_false_defers_when_review_is_remaining() {
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            6 * 24 * 3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        let policy = UsagePolicy::default(); // ask_ttl_minutes = 0 → Defer
        let eval = evaluate_quota(std::slice::from_ref(&frontier), &policy, 8);
        let fb = TierFallback {
            max_difficulty: "high".into(),
            include_review: false,
            include_forced: false,
        };
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: true,
            has_review: true,
            max_difficulty: Some("high"),
            ..RemainingWorkSnapshot::default()
        };
        assert!(!tier_fallback_allows(Some(&fb), &work));
        let applied = apply_quota(&eval, &[frontier], &policy, Some(&fb), &work);
        assert!(applied.unavailable.is_empty());
        assert_eq!(applied.account, QuotaAccountAction::Defer);
    }

    #[test]
    fn apply_forbade_null_tier_fallback_defers_when_other_runnable() {
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            6 * 24 * 3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        let policy = UsagePolicy::default(); // ask_ttl_minutes = 0
        let eval = evaluate_quota(std::slice::from_ref(&frontier), &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: true,
            max_difficulty: Some("high"),
            ..RemainingWorkSnapshot::default()
        };
        let applied = apply_quota(&eval, &[frontier], &policy, None, &work);
        assert!(applied.unavailable.is_empty());
        assert_eq!(applied.account, QuotaAccountAction::Defer);
    }

    #[test]
    fn apply_account_low_3h_waits_capped() {
        let session = pct_bucket("five_hour", "session", 5.0, 3 * 3600, None);
        let policy = UsagePolicy::default();
        let eval = evaluate_quota(std::slice::from_ref(&session), &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: false,
            ..RemainingWorkSnapshot::default()
        };
        let applied = apply_quota(&eval, &[session], &policy, Some(&factory_fb()), &work);
        match applied.account {
            QuotaAccountAction::Wait { secs } => {
                assert!(secs <= MAX_WAIT_SECS);
                assert!(secs > 2 * 3600);
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn apply_account_low_3h_waits_capped_even_when_other_rungs_runnable() {
        // Session-only AccountLow in the middle band still Wait-caps; runnable
        // siblings must not flip the account action to Proceed/Stop.
        let session = pct_bucket("five_hour", "session", 5.0, 3 * 3600, None);
        let policy = UsagePolicy::default();
        let eval = evaluate_quota(std::slice::from_ref(&session), &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: true,
            ..RemainingWorkSnapshot::default()
        };
        let applied = apply_quota(&eval, &[session], &policy, Some(&factory_fb()), &work);
        match applied.account {
            QuotaAccountAction::Wait { secs } => {
                assert!(secs <= MAX_WAIT_SECS);
                assert!(secs > 2 * 3600);
            }
            other => panic!("expected capped Wait, got {other:?}"),
        }
    }

    #[test]
    fn apply_account_weekly_all_6d_stops_even_when_other_rungs_runnable() {
        // Account-binding weekly_all shares quota across Claude rungs.
        // other_rungs_runnable (standard still "looks" selectable after scoped
        // subtract) must NOT Proceed and keep burning the weekly bucket.
        for remaining in [5.0_f64, 0.0] {
            let week = pct_bucket("seven_day", "weekly_all", remaining, 6 * 24 * 3600, None);
            let policy = UsagePolicy::default();
            let eval = evaluate_quota(std::slice::from_ref(&week), &policy, 8);
            let work = RemainingWorkSnapshot {
                other_rungs_runnable: true,
                max_difficulty: Some("high"),
                ..RemainingWorkSnapshot::default()
            };
            let applied = apply_quota(&eval, &[week], &policy, Some(&factory_fb()), &work);
            assert_eq!(
                applied.account,
                QuotaAccountAction::Stop,
                "weekly_all remaining {remaining} @ 6d must Stop, not Proceed"
            );
            assert!(
                applied.unavailable.is_empty(),
                "account-binding must not emit scoped unavailable"
            );
        }
    }

    #[test]
    fn apply_only_frontier_6d_no_fallback_stops() {
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            6 * 24 * 3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        let policy = UsagePolicy::default();
        let eval = evaluate_quota(std::slice::from_ref(&frontier), &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: false,
            ..RemainingWorkSnapshot::default()
        };
        let applied = apply_quota(&eval, &[frontier], &policy, Some(&factory_fb()), &work);
        assert_eq!(applied.account, QuotaAccountAction::Stop);
    }

    #[test]
    fn apply_only_frontier_6h_cap_and_repark_not_stop() {
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            6 * 3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        let policy = UsagePolicy::default();
        let eval = evaluate_quota(std::slice::from_ref(&frontier), &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: false,
            ..RemainingWorkSnapshot::default()
        };
        let applied = apply_quota(&eval, &[frontier], &policy, Some(&factory_fb()), &work);
        match applied.account {
            QuotaAccountAction::Wait { secs } => assert_eq!(secs, MAX_WAIT_SECS),
            other => panic!("expected capped Wait, got {other:?}"),
        }
    }

    #[test]
    fn apply_latest_reset_among_multiple_wait_buckets() {
        let session = pct_bucket("five_hour", "session", 5.0, 2 * 3600, None);
        let week = pct_bucket("seven_day", "weekly_all", 5.0, 6 * 24 * 3600, None);
        let policy = UsagePolicy::default();
        let buckets = [session, week];
        let eval = evaluate_quota(&buckets, &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: false,
            ..RemainingWorkSnapshot::default()
        };
        let applied = apply_quota(&eval, &buckets, &policy, Some(&factory_fb()), &work);
        // Latest is weekly 6d → beyond 12h + nothing runnable → Stop (not 2h wait).
        assert_eq!(applied.account, QuotaAccountAction::Stop);
    }

    #[test]
    fn apply_spend_stops_only_at_zero_not_percent_floor() {
        let spend = QuotaBucket {
            id: "credits".into(),
            kind: "credits".into(),
            label: String::new(),
            measurements: vec![crate::loop_engine::quota::Measurement {
                remaining: 8.0, // 8 credits left — must NOT halt
                unit: crate::loop_engine::quota::MeasurementUnit::Credits,
            }],
            resets_at: None,
            severity: None,
            is_active: None,
            rungs: None,
        };
        let policy = UsagePolicy::default();
        let eval = evaluate_quota(std::slice::from_ref(&spend), &policy, 8);
        // Amount > 0 is not amount_exhausted in evaluate → Ignore.
        assert!(eval.account_low.is_empty());
        let spent = QuotaBucket {
            measurements: vec![crate::loop_engine::quota::Measurement {
                remaining: 0.0,
                unit: crate::loop_engine::quota::MeasurementUnit::Credits,
            }],
            ..spend
        };
        let eval = evaluate_quota(std::slice::from_ref(&spent), &policy, 8);
        let work = RemainingWorkSnapshot::default();
        let applied = apply_quota(&eval, &[spent], &policy, Some(&factory_fb()), &work);
        assert_eq!(applied.account, QuotaAccountAction::Stop);
    }

    #[test]
    fn apply_stop_beats_ask() {
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            6 * 24 * 3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        let session = pct_bucket("five_hour", "session", 5.0, 6 * 24 * 3600, None);
        let policy = UsagePolicy::default();
        let buckets = [frontier, session];
        let eval = evaluate_quota(&buckets, &policy, 8);
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: false,
            ..RemainingWorkSnapshot::default()
        };
        // Forbade would ask, but nothing runnable + beyond horizon → Stop wins.
        let applied = apply_quota(&eval, &buckets, &policy, None, &work);
        assert_eq!(applied.account, QuotaAccountAction::Stop);
    }

    #[test]
    fn execute_wait_zero_is_ready_now_not_fallback_300() {
        // Evaluate emits reset_secs=0 for past/now; apply maps to Wait { 0 };
        // execute must NOT substitute fallback_wait (300).
        use std::cell::RefCell;
        let session = pct_bucket("five_hour", "session", 5.0, -5, None);
        let policy = UsagePolicy::default();
        let fb = factory_fb();
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: false,
            ..RemainingWorkSnapshot::default()
        };
        let eval = evaluate_quota(std::slice::from_ref(&session), &policy, 8);
        let applied = apply_quota(&eval, &[session.clone()], &policy, Some(&fb), &work);
        assert_eq!(
            applied.account,
            QuotaAccountAction::Wait { secs: 0 },
            "past/now reset must apply as Wait {{ 0 }}"
        );

        let waited = RefCell::new(Vec::<u64>::new());
        let wait = |secs: u64| {
            waited.borrow_mut().push(secs);
            true
        };
        let mut set = HashSet::new();
        let result = account_quota_preflight_inner(
            QuotaPreflightParams {
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                policy: &policy,
                tier_fallback: Some(&fb),
                execute_account_action: true,
                unavailable_rungs: &mut set,
                work: &work,
                buckets: Some(std::slice::from_ref(&session)),
                account_remaining: Some(5.0),
                account_reset_at: None,
            },
            &wait,
        );
        assert_eq!(
            result,
            UsageCheckResult::BelowThreshold,
            "Wait {{ 0 }} must be ready-now / BelowThreshold, not a 300s sleep"
        );
        assert!(
            waited.borrow().is_empty(),
            "ready-now must not invoke wait (got {:?})",
            waited.borrow()
        );
    }

    #[test]
    fn execute_wait_positive_secs_still_sleeps() {
        use std::cell::RefCell;
        let session = pct_bucket("five_hour", "session", 5.0, 120, None);
        let policy = UsagePolicy::default();
        let fb = factory_fb();
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: false,
            ..RemainingWorkSnapshot::default()
        };
        let waited = RefCell::new(Vec::<u64>::new());
        let wait = |secs: u64| {
            waited.borrow_mut().push(secs);
            true
        };
        let mut set = HashSet::new();
        let result = account_quota_preflight_inner(
            QuotaPreflightParams {
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                policy: &policy,
                tier_fallback: Some(&fb),
                execute_account_action: true,
                unavailable_rungs: &mut set,
                work: &work,
                buckets: Some(std::slice::from_ref(&session)),
                account_remaining: Some(5.0),
                account_reset_at: None,
            },
            &wait,
        );
        assert_eq!(result, UsageCheckResult::WaitedAndReset);
        let calls = waited.borrow();
        assert_eq!(calls.len(), 1, "positive Wait must invoke wait once");
        assert!(
            (100..=140).contains(&calls[0]),
            "expected ~120s wait, got {}",
            calls[0]
        );
    }

    #[test]
    fn org_fallback_reset_zero_is_ready_now_not_fallback_300() {
        // buckets=None falls through to legacy remaining gate; past reset → 0.
        use std::cell::RefCell;
        let policy = UsagePolicy::default();
        let fb = factory_fb();
        let work = RemainingWorkSnapshot::default();
        let waited = RefCell::new(Vec::<u64>::new());
        let wait = |secs: u64| {
            waited.borrow_mut().push(secs);
            true
        };
        let mut set = HashSet::new();
        let past = (chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339();
        let result = account_quota_preflight_inner(
            QuotaPreflightParams {
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                policy: &policy,
                tier_fallback: Some(&fb),
                execute_account_action: true,
                unavailable_rungs: &mut set,
                work: &work,
                buckets: None,
                account_remaining: Some(5.0),
                account_reset_at: Some(past.as_str()),
            },
            &wait,
        );
        assert_eq!(
            result,
            UsageCheckResult::BelowThreshold,
            "past reset must be ready-now, not fallback_wait 300"
        );
        assert!(
            waited.borrow().is_empty(),
            "ready-now must not invoke wait (got {:?})",
            waited.borrow()
        );
    }

    #[test]
    fn preflight_inner_parity_same_buckets_same_decision() {
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            6 * 24 * 3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        let policy = UsagePolicy::default();
        let fb = factory_fb();
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: true,
            max_difficulty: Some("high"),
            ..RemainingWorkSnapshot::default()
        };
        let mut set_a = HashSet::new();
        let mut set_b = HashSet::new();
        let wait = |_secs: u64| true;
        let a = account_quota_preflight_inner(
            QuotaPreflightParams {
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                policy: &policy,
                tier_fallback: Some(&fb),
                execute_account_action: true,
                unavailable_rungs: &mut set_a,
                work: &work,
                buckets: Some(std::slice::from_ref(&frontier)),
                account_remaining: Some(76.0),
                account_reset_at: None,
            },
            &wait,
        );
        let b = account_quota_preflight_inner(
            QuotaPreflightParams {
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                policy: &policy,
                tier_fallback: Some(&fb),
                execute_account_action: true,
                unavailable_rungs: &mut set_b,
                work: &work,
                buckets: Some(std::slice::from_ref(&frontier)),
                account_remaining: Some(76.0),
                account_reset_at: None,
            },
            &wait,
        );
        assert_eq!(a, b);
        assert_eq!(set_a, set_b);
        assert!(set_a.contains(&(Provider::Claude, CapabilityTier::Frontier)));
        assert_eq!(a, UsageCheckResult::BelowThreshold);
    }

    #[test]
    fn preflight_keeps_snapshot_on_api_fail() {
        let mut set = HashSet::from([(Provider::Claude, CapabilityTier::Frontier)]);
        let policy = UsagePolicy::default();
        let fb = factory_fb();
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: true,
            ..RemainingWorkSnapshot::default()
        };
        let wait = |_secs: u64| true;
        let result = account_quota_preflight_inner(
            QuotaPreflightParams {
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                policy: &policy,
                tier_fallback: Some(&fb),
                execute_account_action: true,
                unavailable_rungs: &mut set,
                work: &work,
                buckets: None, // API fail
                account_remaining: None,
                account_reset_at: None,
            },
            &wait,
        );
        assert_eq!(result, UsageCheckResult::Skipped);
        assert!(set.contains(&(Provider::Claude, CapabilityTier::Frontier)));
    }

    #[test]
    fn preflight_disabled_keeps_proto_channel_snapshot() {
        let frontier = pct_bucket(
            "weekly_scoped",
            "weekly_scoped",
            5.0,
            3600,
            Some(vec![(Provider::Claude, CapabilityTier::Frontier)]),
        );
        // Pre-existing snapshot must survive LOOP_USAGE_CHECK_ENABLED=false.
        let mut set = HashSet::from([(Provider::Claude, CapabilityTier::Standard)]);
        let policy = UsagePolicy::default();
        let fb = factory_fb();
        let work = RemainingWorkSnapshot {
            other_rungs_runnable: true,
            max_difficulty: Some("high"),
            ..RemainingWorkSnapshot::default()
        };
        let wait = |_secs: u64| true;
        let result = account_quota_preflight_inner(
            QuotaPreflightParams {
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                policy: &policy,
                tier_fallback: Some(&fb),
                execute_account_action: false, // LOOP_USAGE_CHECK_ENABLED=false
                unavailable_rungs: &mut set,
                work: &work,
                buckets: Some(std::slice::from_ref(&frontier)),
                account_remaining: Some(76.0),
                account_reset_at: None,
            },
            &wait,
        );
        assert_eq!(result, UsageCheckResult::Skipped);
        assert_eq!(
            set,
            HashSet::from([(Provider::Claude, CapabilityTier::Standard)]),
            "disabled preflight must keep snapshot (no replace from buckets)"
        );
    }

    #[test]
    fn gate_disabled_skips_usage_load_and_keeps_snapshot() {
        // AC: LOOP_USAGE_CHECK_ENABLED=false + Claude enabled → no load_usage_info.
        use std::cell::Cell;
        let load_calls = Cell::new(0u32);
        let load = || {
            load_calls.set(load_calls.get() + 1);
            None
        };
        let mut set = HashSet::from([(Provider::Claude, CapabilityTier::Frontier)]);
        let mut conn = Connection::open_in_memory().expect("in-memory");
        let models = crate::loop_engine::model::builtin_resolved_models();
        let policy = UsagePolicy::default();
        let fb = factory_fb();
        let overrides = HashMap::new();
        let result = run_account_quota_gate_inner(
            RunAccountQuotaGateParams {
                conn: &mut conn,
                task_prefix: None,
                run_id: "run",
                unavailable_rungs: &mut set,
                runner_overrides: &overrides,
                models,
                policy: &policy,
                tier_fallback: Some(&fb),
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                execute_account_action: false,
            },
            &load,
        );
        assert_eq!(result, UsageCheckResult::Skipped);
        assert_eq!(
            load_calls.get(),
            0,
            "disabled gate must not call load_usage_info"
        );
        assert!(
            set.contains(&(Provider::Claude, CapabilityTier::Frontier)),
            "disabled gate must keep proto-channel snapshot"
        );
    }

    #[test]
    fn gate_enabled_invokes_usage_load() {
        use std::cell::Cell;
        let load_calls = Cell::new(0u32);
        let load = || {
            load_calls.set(load_calls.get() + 1);
            None
        };
        let mut set = HashSet::new();
        let mut conn = Connection::open_in_memory().expect("in-memory");
        let models = crate::loop_engine::model::builtin_resolved_models();
        let policy = UsagePolicy::default();
        let fb = factory_fb();
        let overrides = HashMap::new();
        let result = run_account_quota_gate_inner(
            RunAccountQuotaGateParams {
                conn: &mut conn,
                task_prefix: None,
                run_id: "run",
                unavailable_rungs: &mut set,
                runner_overrides: &overrides,
                models,
                policy: &policy,
                tier_fallback: Some(&fb),
                threshold: 8,
                tasks_dir: Path::new("/tmp"),
                fallback_wait: 300,
                execute_account_action: true,
            },
            &load,
        );
        assert_eq!(result, UsageCheckResult::Skipped); // no buckets → skipped
        assert_eq!(
            load_calls.get(),
            1,
            "enabled gate must call load_usage_info once"
        );
    }

    // --- compute_remaining_work_snapshot honors runner_overrides pins ---

    fn snapshot_seed_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory");
        conn.execute_batch(
            r#"
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'todo',
                model TEXT,
                difficulty TEXT,
                archived_at TEXT
            );
            INSERT INTO tasks (id, status, difficulty) VALUES
                ('pinned-frontier', 'todo', 'high');
            "#,
        )
        .expect("seed");
        conn
    }

    #[test]
    fn snapshot_without_pin_treats_claude_frontier_as_unavailable() {
        // Discriminator: sole high-difficulty task resolves to Claude frontier
        // under empty blackouts; with frontier unavailable and no pin,
        // other_rungs_runnable must be false (would horizon-Stop).
        let conn = snapshot_seed_conn();
        let models = crate::loop_engine::model::builtin_resolved_models();
        let unavailable = [(Provider::Claude, CapabilityTier::Frontier)];
        let empty_overrides = HashMap::new();
        let work =
            compute_remaining_work_snapshot(&conn, None, models, &unavailable, &empty_overrides);
        assert!(
            !work.other_rungs_runnable,
            "unpinned Claude frontier task must not count as other-rung runnable; got {work:?}"
        );
    }

    #[test]
    fn snapshot_with_grok_pin_counts_as_other_rungs_runnable() {
        // Same sole frontier task, but promote_once pin to Grok: effective
        // provider is Grok (tier still frontier). Claude frontier unavailable
        // must NOT hide this runnable work (avoids spurious horizon Stop).
        let conn = snapshot_seed_conn();
        let models = crate::loop_engine::model::builtin_resolved_models();
        let unavailable = [(Provider::Claude, CapabilityTier::Frontier)];
        let mut overrides = HashMap::new();
        overrides.insert("pinned-frontier".to_string(), RunnerKind::Grok);
        let work = compute_remaining_work_snapshot(&conn, None, models, &unavailable, &overrides);
        assert!(
            work.other_rungs_runnable,
            "Grok-pinned task must count as other-rung runnable when only Claude \
             frontier is unavailable; got {work:?}"
        );
    }

    #[test]
    fn snapshot_with_codex_pin_counts_as_other_rungs_runnable() {
        let conn = snapshot_seed_conn();
        let models = crate::loop_engine::model::builtin_resolved_models();
        let unavailable = [(Provider::Claude, CapabilityTier::Frontier)];
        let mut overrides = HashMap::new();
        overrides.insert("pinned-frontier".to_string(), RunnerKind::Codex);
        let work = compute_remaining_work_snapshot(&conn, None, models, &unavailable, &overrides);
        assert!(
            work.other_rungs_runnable,
            "Codex-pinned task must count as other-rung runnable when only Claude \
             frontier is unavailable; got {work:?}"
        );
    }
}
