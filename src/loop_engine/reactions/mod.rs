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
//! | [`post_output::react_to_outputs`] | `post_output` | post-output rate-limit wait (`usage::{parse_reset_from_output, wait_for_usage_reset}`) |
//! | [`post_output::handle_overflow`] | `post_output` | `overflow::handle_prompt_too_long` |
//! | [`post_completion::react_to_completions`] | `post_completion` | `orchestrator::trigger_human_reviews` |
//!
//! CONTRACT-001 establishes the boundary, the enforcement mechanism, and the
//! typed coordinator signatures. `handle_overflow` is fully wired (both engine
//! paths route through it) to prove the lock end-to-end; the remaining four
//! coordinators are typed scaffolds whose bodies are filled in / wired by the
//! owning FEATs (FEAT-002/003/005/006/010/013). They carry `#[allow(dead_code)]`
//! only until those FEATs call them from both paths.

// `account` and `pre_spawn` are `pub` (not `pub(crate)`) so their converged
// coordinators are reachable from the integration parity harness
// (`tests/reaction_parity.rs`), mirroring `pub mod iteration_pipeline`:
// `account` for the post-output rate-limit reaction + usage gate (TEST-INIT-001/002),
// `pre_spawn` for `resolve_task_execution` (TEST-INIT-002). The remaining
// coordinator submodules stay crate-private until their owning FEAT needs
// integration-test reachability.
pub mod account;
pub(crate) mod post_completion;
pub(crate) mod post_output;
pub mod pre_spawn;
