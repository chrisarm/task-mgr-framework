# Changelog — 2026-06-02

## Harden baseline-tier runner routing + framework follow-ups

**Branch**: `feat/harden-baseline-tier-routing`
**PRD**: `tasks/prd-harden-baseline-tier-routing.md`

### What shipped

Closed a confirmed routing divergence where a task recovering from Codex→Claude
derived its baseline tier from different inputs than the original spawn site,
so a recovering task could route to the wrong provider (or fail to route).
Recovery and primary now share one `compute_baseline_model` source of truth, with
the project/user defaults threaded from the engine cache through the
failure-handler chain on both the sequential and wave paths. The cross-provider
idempotency guard was extracted into a single `promote_once` primitive (used at
all promotion sites), the on-disk config rewrite was hardened (lossless
key-preservation, idempotent, mode-preserving, `Err`-on-malformed), and
`sanitize_branch_name` now neutralizes `..` so no branch/PRD-derived name can
place a worktree outside the `-worktrees/` parent. The two largest loop-engine
files were split along clean seams into `startup.rs` and `wave_orchestration.rs`
(behavior-neutral moves).

### Why it matters

Operators running Codex with `runtimeErrorFallback` + `baselineTierRoutes` now get
recovery routing that matches the original spawn decision. The new `promote_once`
and `compute_baseline_model` contracts make the next provider/tier addition
correct by construction rather than by remembering to copy a guard, and the
god-module splits make the orchestrator and wave scheduler reviewable.

### Breaking changes

None. Documented happy-path routing precedence is unchanged; internal
failure-handler signatures gained two threaded args (compile-checked across both
callers). Config schema is unchanged — legacy keys (`byBaselineTier`,
`fallbackToClaude`, `opus`/`sonnet`/`haiku`) still migrate to canonical.

---
