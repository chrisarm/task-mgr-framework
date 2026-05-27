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
