# Changelog — 2026-05-28

## Logging standardization (CONTRACT-LOG-001)

**Branch**: `feat/logging-standardization`
**PRD**: `tasks/logging-standardization-spike.md` (`/plan-tasks` lean flow, no standalone PRD doc)
**Prefix**: `147cf226`

### What shipped

A new four-channel logging boundary, classified per [CONTRACT-LOG-001](../../tasks/logging-standardization-spike.md):

- **Channel A / A2** — product UX + byte-locked operator contracts, via a new `ui::` surface (`src/output/ui.rs`: `emit`, `emit_err`, `emit_data`, `emit_prefixed`, `prompt`, `yellow`). Bytes + audience FD preserved; no level/timestamp ever added.
- **Channel B** — internal diagnostics, via a `tracing` subscriber (`src/observability.rs`): console layer at `WARN+` by default (raise via `TASK_MGR_LOG=...`), rolling file layer at `DEBUG+` writing to `.task-mgr/logs/task-mgr-<prefix>.<YYYY-MM-DD>.log` (prefix-suffixed exactly like `tasks/progress-<prefix>.txt`).
- **Channel C** — child-process raw stderr captured per-iteration to `.task-mgr/logs/<prefix>-<run>-<slot>-iterN-grok-stderr.log`; `GROK_TELEMETRY_TRACE_UPLOAD=0` silences xAI BatchSpanProcessor noise. Decoupled from reactions FEAT-014.

A `tests/no_raw_prints.rs` CI guard enforces the boundary going forward.

### Why it matters

Operators get a quiet console (`WARN+` default), a complete debug log per PRD effort on disk, and an unpolluted grok-stderr capture file for post-run forensics — without losing any byte-locked snapshot tests or operator-grep contracts (`lifecycle_stderr_contract.rs` passes unchanged). New tasks now route diagnostics through `tracing::warn!` and product output through `ui::*` by clear discriminator (state-change confirmation vs internal-op-failure-that-no-ops).

### Breaking changes

None on stdout for scripts/pipes. Stderr bytes are preserved on every A2 byte-locked surface (verified by the unchanged lifecycle_stderr_contract test). Console verbosity changes: routine diagnostics that previously printed unconditionally now require `TASK_MGR_LOG=debug` (or any non-default directive) to surface.

### Follow-ups

- ~36 modules remain on the `no_raw_prints` allow-list as deliberate documented exceptions (db/, learnings/, lifecycle/, plus the rest of `loop_engine/`). A "finish full-repo migration" effort would shrink the list to empty.
- The new `src/loop_engine/reactions/` module (merged in from PR #22) carries 21 raw print sites and is added to the allow-list pending its own migration pass.
- Classifier-flagged surfacing of notable grok-stderr lines from the capture files into operator/learnings flow is deferred to reactions FEAT-14.

---

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

## Parallel task execution — refactor tail (REFACTOR-077…080 + VERIFY-001)

**Branch**: `feat/parallel-task-execution`
**PRD**: `tasks/prd-parallel-task-execution.md`

### What shipped

Closing-cleanup of the parallel-execution PRD: extracted a shared `count_remaining_tasks` helper used by both sequential and wave paths (REFACTOR-077); replaced two magic-string `StaleTracker::check("stale","stale")` / `check("a","b")` call sites with named `mark_stale()` / `reset_progress()` methods (REFACTOR-078); converted `progress::log_iteration`'s 8-arg signature to a `LogIterationParams` struct and dropped `#[allow(clippy::too_many_arguments)]` (REFACTOR-079); replaced manual `serde_json::json!({...})` + 5 if-let blocks in `format_task_json_raw` with a `TaskJsonPayload` struct using `skip_serializing_if = "Option::is_none"` (REFACTOR-080). CLAUDE.md picked up the missing `--parallel` cheat sheet entry, slot worktree paths, and a pointer to the most recent migration (VERIFY-001).

### Why it matters

These are call-site preserving cleanups — no behavior change, no API breakage, full suite (3843 tests) green and clippy clean. The benefit accrues to the next engineer touching this code: named methods over magic strings, params struct over 8-arg signatures, derive-Serialize over hand-rolled JSON. The CLAUDE.md edit closes a discoverability gap on the `--parallel` flag.

### Breaking changes

None.

### Footnote

REFACTOR-080 initially broke the `prompt_sequential_v1` snapshot test because the serde struct serialized fields in declaration order while the prior `json!({...})` macro alphabetized via its internal BTreeMap. Fixed in a follow-up commit (`605d47c`) by reordering `TaskJsonPayload`'s fields alphabetically by their camelCase serialized name. The struct now carries a comment documenting the constraint, and a new learning (`task-mgr recall --tags serde,snapshot`) captures the rule for future struct-for-`json!` refactors.

---
