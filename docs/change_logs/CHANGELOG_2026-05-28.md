# Changelog — 2026-05-28

## Reactions Framework Convergence

**Branch**: `refactor/reactions-framework-convergence`
**PRD**: `tasks/prd-reactions-framework-convergence.md`

### What shipped

Converged all non-path-specific main-thread post-Claude behaviors (rate-limit
wait, crash escalation, the pre-iteration usage gate, overflow recovery,
human-review, budget accounting, plus a new transient-backend retry reaction)
into a single `src/loop_engine/reactions/` module with five public coordinators
(`pre_spawn::resolve_task_execution`, `account::account_usage_gate`,
`account::react_to_outputs`, `post_output::handle_overflow`,
`post_completion::react_to_completions`). Both sequential and wave/parallel
execution paths now route through the same coordinators. Enforcement:
`#![deny(deprecated)]` on `iteration.rs`, `wave_scheduler.rs`, and `slot.rs`,
plus exhaustive param-struct destructure on every coordinator — copy-pasting a
reaction back into one path is a compile error. New `tests/reaction_parity.rs`
pins every named invariant (wait-once, B1 completion-durability-before-reset,
B2 budget no-consumption on `WaitedAndRetry`, B3 merge-fail-streak preservation,
Stop semantics) with counting-mock tests and a negative-control proving the
assertions are real. FR-CLEANUP-001 removed every transition shim so
`reactions::` is the only home.

### Why it matters

Closes bug #3: a `parallel_slots > 1` loop that hits a session limit overnight
now **waits and resumes** instead of false-aborting with `"no eligible tasks
after 3 consecutive stale iterations"` and resetting every in-flight task. The
strand-and-reset bug had already shipped three production incidents under
different names; the compile-time lock makes incident #4 of the same class
structurally impossible.

### Breaking changes

None for end-users. Internal to the engine:

- Direct calls from `iteration.rs` / `wave_scheduler.rs` / `slot.rs` to the old
  leaves (`overflow::handle_prompt_too_long`, `usage::check_and_wait`,
  `recovery::check_crash_escalation`, etc.) are now compile errors — callers
  route through the coordinators. Tests that drove the leaves directly were
  repointed or repaint with `#[allow(deprecated)]` at the test site.
- Wave mode now fires `react_to_completions` (human review + external-git
  reconcile) — previously a wave-mode silent omission. Human review CAN fire
  on a partial wave (other slots still in flight); this is intentional and
  documented in `src/loop_engine/CLAUDE.md` → "Post-completion reactions
  (converged)".

---
