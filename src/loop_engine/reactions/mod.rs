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
//! CONTRACT-001 established the boundary, the enforcement mechanism, and the
//! typed coordinator signatures; the owning FEATs (FEAT-002/003/005/006/010/013)
//! then filled the bodies and wired every coordinator into BOTH execution paths.
//! All six converged reactions (the five above plus
//! [`account_iteration_budget`]) are now fully wired — none carries
//! `#[allow(dead_code)]`, and the per-coordinator rustdoc names the sequential
//! and wave call sites. `tests/reaction_parity.rs` pins that the two path shapes
//! compute identical results for identical inputs.

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
// The loop bound is `orchestrator.rs:916` `while iteration < max_iterations`
// with a top-of-pass increment (`:917`). A `RateLimit` / `Reorder` /
// `TransientBackend` (WaitedAndRetry) outcome must give that increment back so
// a persistently rate-limited / unavailable run does not burn its
// `max_iterations` budget on waits — bounded termination then relies on the
// `.stop`/signal check, NOT the iteration ceiling. The sequential path used to
// do `iteration -= 1` (orchestrator.rs RateLimit arm) and the wave path
// `iteration = iteration.saturating_sub(1)` (the `iteration_consumed == false`
// branch); FEAT-013 routes BOTH through this one helper so the two paths
// cannot drift on the budget rule.
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
pub fn account_iteration_budget(params: IterationBudgetParams<'_>) {
    // Exhaustive destructure (no `..`) — the CONTRACT-001 parity lock: a new
    // budget field forces every accounting decision back through this body.
    let IterationBudgetParams {
        iteration,
        iterations_completed,
        consumes_budget,
    } = params;
    if consumes_budget {
        // Consuming outcome: the loop-bound `iteration` was already advanced at
        // the loop top; only the reported stat moves.
        *iterations_completed += 1;
    } else {
        // Give-back outcome (RateLimit / Reorder / WaitedAndRetry): return the
        // top-of-pass increment so a persistently unavailable backend doesn't
        // burn its `max_iterations` budget on waits. `saturating_sub` is a
        // floor guard — the loop top guarantees `*iteration >= 1` here.
        *iteration = iteration.saturating_sub(1);
    }
}
