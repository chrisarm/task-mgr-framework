//! Shared main-thread *reaction* coordinators (PRD: reactions-framework-convergence).
//!
//! The loop engine has two execution paths — sequential
//! (`iteration.rs::run_iteration`) and parallel-wave
//! (`wave_scheduler.rs::run_wave_iteration` + `slot.rs`). Main-thread
//! post-Claude *reactions* (usage gate, rate-limit wait, overflow ladder,
//! per-task recovery resolution, completion handling) were historically
//! implemented at one path's call site and silently omitted or shaped
//! differently in the other, producing a recurring parity-divergence bug
//! class. This module is the single home every converged reaction lives in;
//! BOTH paths route through these coordinators. The wave path folds its N
//! slot results into one reaction; the sequential path folds its 1.
//!
//! ## The single-home contract (enforced at compile time)
//!
//! Two mechanisms keep a reaction from being copy-pasted back into one path:
//!
//! 1. **`#[deprecated]` on the relocated leaf + `#![deny(deprecated)]` on the
//!    three engine files** (`iteration.rs`, `wave_scheduler.rs`, `slot.rs`).
//!    A direct call to a relocated leaf from any engine file fails `cargo
//!    build`; the only legitimate caller is the coordinator here (which marks
//!    its single call site `#[allow(deprecated)]` during the transition
//!    window, until the leaf body is physically relocated into this module by
//!    the owning FEAT).
//! 2. **Exhaustive param-struct destructure (no `..`)** in every coordinator.
//!    Adding a field to a coordinator's param struct is a compile error until
//!    every coordinator body accounts for it — the parity-divergence the
//!    framework exists to prevent becomes a compile-time concern.
//!
//! ## The five coordinators
//!
//! | Coordinator | Module | Relocated leaf(s) |
//! |---|---|---|
//! | [`pre_spawn::resolve_task_execution`] | `pre_spawn` | `recovery::check_override_invalidation`, `recovery::check_crash_escalation` |
//! | [`account::account_usage_gate`] | `account` | `usage::check_and_wait` (pre-dispatch, once per wave) |
//! | [`account::react_to_outputs`] | `account` | post-output rate-limit wait (`usage::{parse_reset_from_output, wait_for_usage_reset}`) |
//! | [`account::react_to_transient`] | `account` | post-output transient-backend (HTTP 5xx / overloaded) bounded backoff-retry (FEAT-014) |
//! | [`post_output::handle_overflow`] | `post_output` | `overflow::handle_prompt_too_long` |
//! | [`post_completion::react_to_completions`] | `post_completion` | `orchestrator::trigger_human_reviews` |
//!
//! CONTRACT-001 establishes the boundary, the enforcement mechanism, and the
//! typed coordinator signatures. `handle_overflow` is fully wired (both engine
//! paths route through it) to prove the lock end-to-end; the remaining four
//! coordinators are typed scaffolds whose bodies are filled in / wired by the
//! owning FEATs (FEAT-002/003/005/006/010/013). They carry `#[allow(dead_code)]`
//! only until those FEATs call them from both paths.

// `account`, `pre_spawn`, `post_output`, and `post_completion` are `pub` (not
// `pub(crate)`) so their converged coordinators are reachable from the
// integration parity harness (`tests/reaction_parity.rs`), mirroring
// `pub mod iteration_pipeline`: `account` for the post-output rate-limit
// reaction + usage gate (TEST-INIT-001/002), `pre_spawn` for
// `resolve_task_execution` (TEST-INIT-002), `post_output` for `handle_overflow`
// (TEST-INIT-003), `post_completion` for `react_to_completions` (TEST-INIT-004).
pub mod account;
pub mod post_completion;
pub mod post_output;
pub mod pre_spawn;

// ---------------------------------------------------------------------------
// Shared iteration-budget accounting (#13) — converged by FEAT-013.
//
// The loop bound is `orchestrator.rs:918` `while iteration < max_iterations`
// with a top-of-pass increment (`:920`). A `RateLimit` / `Reorder` /
// `TransientBackend` (WaitedAndRetry) outcome must give that increment back so
// a persistently rate-limited / unavailable run does not burn its
// `max_iterations` budget on waits — bounded termination then relies on the
// `.stop`/signal check, NOT the iteration ceiling. The sequential path does
// `iteration -= 1` (orchestrator.rs RateLimit arm) and the wave path does
// `iteration = iteration.saturating_sub(1)` (the `iteration_consumed == false`
// branch); FEAT-013 routes BOTH through this one helper so the two paths
// cannot drift on the budget rule. The body below is a TDD scaffold
// (`unimplemented!`): TEST-INIT-004 pins the contract via the ignored tests in
// `tests/reaction_parity.rs`; FEAT-013 fills it in and un-ignores them.
// ---------------------------------------------------------------------------

/// Inputs to [`account_iteration_budget`]. Destructured exhaustively (no `..`).
pub struct IterationBudgetParams<'a> {
    /// The loop-bound iteration counter (`orchestrator.rs:918`
    /// `while iteration < max_iterations`, incremented at the top of each pass).
    pub iteration: &'a mut u32,
    /// The `iterations_completed` stat reported at loop end.
    pub iterations_completed: &'a mut u32,
    /// `false` for a give-back outcome — `RateLimit` / `Reorder` /
    /// `TransientBackend` (WaitedAndRetry) — `true` for every consuming outcome.
    pub consumes_budget: bool,
}

/// Apply the iteration-budget rule for one completed iteration/wave, the single
/// home for the sequential `iteration -= 1` and the wave give-back.
///
/// Contract (pinned by TEST-INIT-004; implemented by FEAT-013):
/// - `consumes_budget == false` ⇒ give the loop-bound iteration back
///   (`*iteration = iteration.saturating_sub(1)`); leave `iterations_completed`
///   unchanged.
/// - `consumes_budget == true` ⇒ advance `iterations_completed`; leave the
///   loop-bound `iteration` (already incremented at the loop top) unchanged.
#[allow(dead_code)] // wired into both paths by FEAT-013
pub fn account_iteration_budget(params: IterationBudgetParams<'_>) {
    let _ = params;
    unimplemented!(
        "FEAT-013: destructure IterationBudgetParams exhaustively; when \
         consumes_budget is false give the loop-bound iteration back \
         (*iteration = iteration.saturating_sub(1)) and leave \
         iterations_completed unchanged; when true increment \
         iterations_completed and leave iteration unchanged. One home for the \
         sequential `iteration -= 1` and the wave give-back so the two paths \
         cannot drift on the budget rule."
    )
}
