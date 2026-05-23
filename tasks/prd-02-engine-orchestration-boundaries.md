# PRD: Engine Orchestration Boundaries — Carving `engine.rs`

**Type**: Refactor
**Priority**: P2 (Medium) — high foundational ROI; not user-visible until next feature lands on the new boundaries
**Author**: Claude Code
**Created**: 2026-05-19
**Status**: Draft

> **Design context.** This is the **second PRD of Coherence Phase 1** (Cluster A) per `docs/designs/coherence-refactoring.md`. The first PRD — `prd-tasklifecycle-extraction.md` (prefix `035925a9`) — must merge first; this PRD assumes `TaskLifecycle` exists and is the single status-write surface. Phases 2 (prompt assembler, recall abstraction) and 3 (event journal research) are explicitly out of scope. Boundary contract with the parallel `runner-trait-hygiene` Phase 2 work is in §6.

---

## 1. Overview

### Problem Statement

`src/loop_engine/engine.rs` is **9,644 lines** and serves as the integration hub for almost everything the loop subsystem does. After TaskLifecycle Extraction merges, the file's `~15` direct `UPDATE tasks SET status` sites have already routed through one named surface — but the surrounding 9k lines of orchestration (outer `run_loop`, sequential `run_iteration`, wave `run_wave_iteration` + `run_slot_iteration` + `process_slot_result`, per-task recovery helpers, signal handling, config loading) still live in one file with no module boundary between concerns.

The pain is now mechanical. Adding a new monitoring hook, recovery branch, or per-iteration side effect requires:
1. Searching the 9k-line file for the call site (often 3-4 sites: sequential, wave-slot, wave-result, fallback)
2. Touching each in lockstep
3. Hoping the wave-vs-sequential parity invariant (already documented in `iteration_pipeline.rs`) still holds

The `iteration_pipeline.rs` extraction shipped earlier (the shared post-Claude pipeline) proves the value: once a concern lives behind a single function, parity divergence becomes a compile-time concern. That win covered only the post-Claude processing block. The other ~9k lines have not had the same treatment.

### Background

Per `docs/designs/coherence-refactoring.md` §2 "Break Up `engine.rs`", the file already has natural seams:

- **Outer orchestration** (`run_loop` at `engine.rs:3048`, ~1300 lines): batch lifecycle, signal handling, run begin/end, config loading, worktree setup/teardown, auto-review trigger, decisions prompt.
- **Sequential iteration** (`run_iteration` at `engine.rs:2230`, ~750 lines): single-task per-iteration body.
- **Wave scheduling** (`run_wave_iteration` at `engine.rs:1835`, `run_parallel_wave` at `engine.rs:871`, plus preflight / deadlock / inter-wave helpers): the parallel-slot scheduler.
- **Slot lifecycle** (`run_slot_iteration` at `engine.rs:557`, `slot_early_exit`, `claim_slot_task`, `slot_failure_result`, `process_slot_result` at `engine.rs:1409`): per-slot work + post-merge processing.
- **Per-task recovery** (`check_crash_escalation`, `check_override_invalidation`, `should_auto_block`, `apply_pending_promotion`, `escalate_task_model_if_needed*`, `increment_consecutive_failures`, `auto_block_task`, `handle_task_failure`): the model-escalation + override-invalidation machinery.
- **Already extracted (keep)**: `iteration_pipeline.rs` (the shared post-Claude pipeline), `runner.rs` (LlmRunner trait + dispatch), `overflow.rs` (5-rung ladder), `worktree.rs` (slot worktrees + merge-back), `merge_resolver.rs`, `prd_reconcile.rs`, `git_reconcile.rs`, `auto_review.rs`, `prompt/*`, `prompt_sections/*`.

This refactor carves the orchestration along the seams above without changing observable behavior. The "five layers of parallel-slot cascade defenses" documented in `src/loop_engine/CLAUDE.md` are invariant — every defensive mechanism either moves verbatim or is replaced by a demonstrably equivalent shape with a passing regression test.

Relevant prior learnings consulted (`task-mgr recall`):
- **[unify-sequential-and-wave-execution PRD]** — established `iteration_pipeline.rs` as the parity-divergence prevention pattern. This PRD generalizes the pattern to the rest of the orchestration.
- **TaskLifecycle Extraction PRD (035925a9) outcomes** — see retrospective; the engine's status writes are now routed through one surface, so the carve no longer has to also move that mess.
- Five-layer parallel-slot defenses (in `src/loop_engine/CLAUDE.md`): slot path threading, consecutive-merge-fail halt, implicit-overlap baseline + buildy heuristic, cross-wave file affinity, stale ephemeral hygiene. All must survive the carve.
- The shared-pipeline invariant (`iteration_pipeline.rs::process_iteration_output`): sequential and wave paths share one post-Claude pipeline. This PRD must not split it.

### Intended Outcome

After this PRD lands:

- `engine.rs` is reduced to a thin module root that re-exports the public surface other code consumes (`run_loop`, `IterationContext`, `IterationResult`, the recovery primitives still in tree). Target: **`engine.rs` < 1500 lines.**
- Four new sibling modules under `src/loop_engine/`: `orchestrator.rs`, `iteration.rs`, `wave_scheduler.rs`, `slot.rs`. (`recovery.rs` is added if and only if the per-task-recovery cluster cleanly extracts; otherwise it remains in `engine.rs`. See §6 Approaches.)
- The shared `iteration_pipeline.rs` contract is **strengthened**, not weakened: both `iteration.rs` and `slot.rs` call `process_iteration_output`; the wiring assertion (`Send` bound on `SlotPromptBundle`) and the wave/sequential parity comment in `prompt/mod.rs` still apply and are updated to point at the new module names.
- A dogfood gate of **N=10 iterations across two distinct PRDs** runs successfully on the refactored code before this PRD merges (per §"Dogfood concurrency (N-iteration exit gate)" risk in the coherence design).

---

## 2. Goals

### Primary Goals

- [ ] Reduce `engine.rs` to under 1500 lines (from 9644) by extracting `orchestrator.rs` / `iteration.rs` / `wave_scheduler.rs` / `slot.rs`.
- [ ] Preserve byte-identical observable behavior — every loop run that worked before runs the same after (same stderr, same DB writes, same exit codes, same `tasks/progress-*.txt` outputs).
- [ ] Preserve all five layers of parallel-slot cascade defenses (slot-path threading, consecutive-merge-fail halt, implicit-overlap baseline + buildy heuristic, cross-wave file affinity, stale ephemeral hygiene) with proof-of-preservation regression tests.
- [ ] Make the wave/sequential parity invariant a module-boundary concern: every new orchestration concern has one obvious add point in each module, not three scattered call sites.

### Success Metrics

- `engine.rs` line count: from 9644 → **< 1500**.
- New module line counts (target ranges, not contracts): `orchestrator.rs` ~1500, `iteration.rs` ~1500, `wave_scheduler.rs` ~2500, `slot.rs` ~2500. Wave is largest because it covers run_wave_iteration + run_parallel_wave + preflight + slot context building + result processing.
- Dogfood gate: **10 successful iterations across 2 distinct PRDs** on the refactored code with the same stderr/DB outputs as a baseline capture on the pre-refactor `engine.rs`.
- Test runtime: no more than **+5%** wall-clock for `cargo test -p task-mgr` (extraction shouldn't add work; we measure to catch accidental duplication).
- Zero new `unsafe` blocks; zero new `pub` items outside the explicit module re-export list.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Byte-identical observable behavior.** The five-layer defenses, the overflow ladder, the auto-review trigger, the decisions prompt, the `<task-status>` dispatch, the merge-resolver wiring, and the iteration-pipeline parity all behave identically after the carve. "Identical" is measured by the dogfood gate: same stderr lines, same DB final state, same exit code on the same input PRD + git state.
- **No status-write site escapes `TaskLifecycle`.** This refactor MUST NOT introduce a raw `UPDATE tasks SET status` site, even temporarily. If a code path needs a status mutation, it routes through the service. The LIFECYCLE-EXCEPTION grep lint introduced by the TaskLifecycle PRD remains green throughout.
- **`Send + Sync` invariants on `SlotPromptBundle` and `LlmRunner` are preserved.** The compile-time assertion in `prompt/mod.rs` continues to pass. The static-dispatch `RunnerKind` match remains the hot path; no `Box<dyn LlmRunner>` is introduced.
- **Wave/sequential parity** of post-Claude processing: both `iteration.rs::run_iteration` and `slot.rs::process_slot_result` call `iteration_pipeline::process_iteration_output`. A new compile-time / test-time assertion enforces that any new step added to `ProcessingParams` is consumed by both call sites.

### Performance Requirements

- **No additional allocations on the hot path.** Slot-context creation, prompt building, runner dispatch, and result processing perform the same allocations they do today (verified by `cargo bench` if present, or by spot-checking with a counting allocator in a test).
- **Module re-exports do not introduce indirection.** Where today's `engine.rs` calls a private helper, the post-carve call goes through one `pub(crate)` boundary at most. We do NOT introduce `pub use` chains that pierce three modules.

### Style Requirements

- **No `.unwrap()` or `.expect()` in production paths** unless already present pre-refactor and clearly safe. If an existing `.unwrap()` is moving as part of an extraction, it's preserved verbatim (this is a refactor, not a hardening pass — that's a separate concern).
- **Module-private helpers stay module-private.** Visibility widens only when an extraction forces a cross-module call. The visibility ladder is `pub(super)` → `pub(crate)` → `pub`, in that order — never widen beyond what's needed.
- **Imports follow the existing convention**: `use crate::loop_engine::<module>::{symbol}` form, no glob `*` imports outside `#[cfg(test)]` modules.
- **Comments explain WHY, not WHAT.** Carve-site comments call out the boundary contract ("this module owns the wave scheduler; sequential path lives in `iteration.rs`") rather than narrating the extraction itself.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Loop runs against a worktree where slot 0 IS the project root | The slot-path-threading defense (defense layer #1) hinges on threading the actual worktree path, not recomputing via `compute_slot_worktree_path` | `wave_scheduler::merge_slot_branches_with_resolver` (or wherever the call lives post-carve) still receives `&[PathBuf]` from `ensure_slot_worktrees` — no caller re-derives slot 0's path |
| Wave run hits the `SYNTHETIC_DEADLOCK_SLOT` sentinel (`usize::MAX`) | The synthesis pattern in `handle_ephemeral_deadlock` MUST emit at least one record even when every blocking ephemeral branch had a malformed suffix | Move the sentinel + handler into `wave_scheduler.rs` verbatim; downstream `is_empty` checks (in `apply_merge_fail_reset_and_halt_check`) still see the synthetic record |
| Stale ephemeral worktree at startup with un-merged commits AND `halt_threshold > 0` | The startup hygiene defense aborts to force operator reconciliation | `orchestrator.rs::run_loop` calls `worktree::reconcile_stale_ephemeral_slots` BEFORE `ensure_slot_worktrees` (Step 9.5 order is contractual — see `src/loop_engine/CLAUDE.md`) |
| Operator edits `tasks.model` mid-loop while an overflow recovery is active | The `check_override_invalidation` escape valve clears all six per-task channels in one shot | Per-task recovery cluster preserves the `check_override_invalidation` call AT THE TOP of every iteration (before `resolve_effective_runner`); if `recovery.rs` is extracted, the call site moves; if not, it stays in `iteration.rs` / `wave_scheduler.rs` at the same boundary |
| Wave iteration where every candidate's only overlap was ephemeral (deadlock guard) | The `ephemeral_block_diagnostics`-populated branch must trip the FEAT-002 reset/halt contract, not spin forever | `wave_scheduler.rs::handle_ephemeral_deadlock` emits the synthetic-slot record AND populates diagnostics; `apply_merge_fail_reset_and_halt_check` (moved or kept) sees a non-empty `failed_merges` and trips |
| Loop receives SIGINT mid-wave with active slot subprocesses | `setup_signal_handler` arms a flag the watchdog polls every 200ms; subprocesses get SIGTERM → 3s grace → SIGKILL | `orchestrator.rs` owns signal-handler setup; the flag is threaded through `WaveIterationParams` and `SlotIterationParams` exactly as it is today — no new ownership transfer |
| Auto-review fires after a clean exit while in batch mode | ONE review fires at end-of-batch for the LAST successful PRD that met the threshold — never per-PRD | `orchestrator.rs::run_loop` (or `batch.rs` if the call lives there) keeps the existing `auto_review::maybe_fire` invocation at the same point in the run lifecycle |
| Loop run with no eligible tasks at start | `handle_no_eligible_tasks` returns a clean exit, not an error | Move into `wave_scheduler.rs` (sequential path has its own no-eligible-tasks branch in `iteration.rs`) — both paths emit the same stderr line and exit code |

---

## 3. User Stories

### US-001: Maintainer adds a new per-iteration monitoring hook

**As a** task-mgr maintainer
**I want** to add a "log iteration duration" hook in one obvious place
**So that** I don't have to find and update three scattered call sites and risk wave/sequential divergence

**Acceptance Criteria:**
- [ ] The new hook is added to a single function and the wiring is picked up by both sequential (`iteration.rs::run_iteration`) and wave (`slot.rs::process_slot_result`) paths.
- [ ] If the hook is post-Claude processing, it lives in `iteration_pipeline.rs::process_iteration_output` and both call sites pick it up automatically (current behavior preserved).
- [ ] If the hook is pre-spawn, it lives in a new shared helper in `iteration.rs` or `wave_scheduler.rs` — never duplicated.

### US-002: Maintainer needs to understand the wave scheduler in isolation

**As a** new contributor
**I want** to read the wave scheduler without scrolling past 5000 lines of unrelated code
**So that** I can reason about parallel-slot scheduling, ephemeral overlay, and merge-back independently of sequential iteration

**Acceptance Criteria:**
- [ ] `wave_scheduler.rs` contains `run_wave_iteration`, `run_parallel_wave`, `wave_preflight_check`, `handle_no_eligible_tasks`, `handle_ephemeral_deadlock`, `build_slot_contexts`, `wait_inter_wave_delay`, `apply_merge_fail_reset_and_halt_check`, and `count_remaining_active_tasks`.
- [ ] Module-level rustdoc explains the wave lifecycle (preflight → group selection → slot build → spawn → merge-back → reconcile) with pointers into `slot.rs` and `worktree.rs`.
- [ ] `src/loop_engine/CLAUDE.md` is updated so the "Touchpoints" table points at the new module locations.

### US-003: Reviewer audits status-write paths during the carve

**As a** code reviewer
**I want** to confirm no extraction reintroduced a raw `UPDATE tasks SET status` site
**So that** the TaskLifecycle invariant the prior PRD established is not silently broken

**Acceptance Criteria:**
- [ ] The LIFECYCLE-EXCEPTION grep-lint test (introduced by the TaskLifecycle PRD) is green at every commit boundary in this PRD's branch.
- [ ] The dogfood gate's DB-final-state assertion matches between pre- and post-carve runs on the same input.

---

## 4. Functional Requirements

### FR-001: Extract `orchestrator.rs` (outer loop + lifecycle)

Move from `engine.rs` into a new `src/loop_engine/orchestrator.rs`:
- `pub async fn run_loop(mut run_config: LoopRunConfig) -> LoopResult` (currently `engine.rs:3048`).
- `fn setup_signal_handler(signal_flag: SignalFlag)` (currently `engine.rs:4562`).
- `pub fn on_run_completed(conn: &Connection, task_prefix: Option<&str>)` (currently `engine.rs:4609`).
- `fn record_session_guidance(...)` (currently `engine.rs:4631`).
- `fn check_global_skills(source_root: &Path)` (currently `engine.rs:2985`).
- `fn trigger_human_reviews(conn: &Connection, params: HumanReviewParams<'_>)` and `query_human_review_tasks` (currently `engine.rs:4388`, `:4426`).
- `fn prompt_pending_key_decisions(conn: &Connection, run_id: &str, yes_mode: bool)` (currently `engine.rs:4467`).

**Details:**
- The `run_loop` body dispatches to `iteration::run_iteration` (sequential) or `wave_scheduler::run_wave_iteration` (wave) at the iteration boundary. The dispatch decision lives in `orchestrator.rs`.
- `LoopRunConfig`, `LoopResult`, and the run-scoped state types either move with `run_loop` or stay in `engine.rs` as re-exported types. Default: stay in `engine.rs` to keep the public surface stable; `orchestrator.rs` imports them via `crate::loop_engine::{LoopRunConfig, LoopResult, IterationContext, ...}`.

**Validation:**
- `cargo test -p task-mgr` passes.
- The dogfood gate's stderr capture for `run_loop`'s start-of-run banner, signal-handler arming line, and end-of-run banner is unchanged.

### FR-002: Extract `iteration.rs` (sequential per-task body)

Move from `engine.rs` into `src/loop_engine/iteration.rs`:
- `pub fn run_iteration(...)` (currently `engine.rs:2230`).
- Private helpers exclusively used by the sequential path.
- The `run_iteration → iteration_pipeline::process_iteration_output` call site.

**Details:**
- `run_iteration` keeps its current signature. Internal helpers it calls today (overflow handling, escalation, override-invalidation check, pending-promotion application) either move with it (if exclusively sequential), to `recovery.rs` (if shared with wave), or stay in `engine.rs` (if shared but unclear).
- The decision rule: a helper that is called from `run_iteration` AND from any wave-side function (`run_slot_iteration`, `process_slot_result`) is a "shared" helper and goes to `recovery.rs` (or stays in `engine.rs`, see §6 Approaches).
- The decision rule for `IterationContext` and `IterationResult`: stay in `engine.rs` (these are the public hand-off types other modules consume) and are re-exported via `pub use`.

**Validation:**
- A sequential-loop dogfood run (`task-mgr loop run <prd> --yes`) on a representative PRD with 5-10 tasks completes with byte-identical DB state and stderr capture as a pre-refactor baseline run.

### FR-003: Extract `wave_scheduler.rs` (parallel wave + merge-back)

Move from `engine.rs` into `src/loop_engine/wave_scheduler.rs`:
- `pub fn run_wave_iteration(...)` (currently `engine.rs:1835`).
- `pub fn run_parallel_wave(...)` (currently `engine.rs:871`).
- `fn wave_preflight_check(...)` (currently `engine.rs:1093`).
- `fn handle_no_eligible_tasks(...)` (currently `engine.rs:1158`).
- `fn handle_ephemeral_deadlock(...)` (currently `engine.rs:1220`).
- `fn build_slot_contexts(...)` (currently `engine.rs:1316`).
- `fn build_shared_slot_params(...)` (currently `engine.rs:1358`).
- `fn build_slot_prompt_params(...)` (currently `engine.rs:1377`).
- `fn count_remaining_active_tasks(...)` (currently `engine.rs:1597`).
- `fn wait_inter_wave_delay(...)` (currently `engine.rs:1615`).
- `fn reset_task_to_todo(...)` (currently `engine.rs:1643`) — note: this is a recovery helper specific to merge-back failure resets; it stays with the wave scheduler since wave is its only caller.
- `fn apply_merge_fail_reset_and_halt_check(...)` (currently `engine.rs:1691`).
- `fn read_prd_implicit_overlap_files(...)` (currently `engine.rs:1762`).
- `fn apply_post_merge_reconcile(...)` (currently `engine.rs:1790`).
- The `SYNTHETIC_DEADLOCK_SLOT = usize::MAX` constant and its handler.

**Details:**
- `wave_scheduler.rs` owns the wave lifecycle. It delegates per-slot work to `slot.rs::run_slot_iteration` and per-slot post-processing to `slot.rs::process_slot_result`.
- The five-layer defense layer-1 (slot-path threading) is enforced at this boundary: `wave_scheduler` MUST pass `slot_paths: &[PathBuf]` from `ensure_slot_worktrees` through to `merge_slot_branches_with_resolver`. No recomputation. A test asserts this contract at the type level (e.g., a unit test in `wave_scheduler.rs` that constructs a `WaveIterationParams` and confirms `slot_worktree_paths` is the literal vector that `ensure_slot_worktrees` returned).
- The synthetic-deadlock sentinel and its handler MUST emit at least one `FailedMerge` record even when every blocking ephemeral had a malformed suffix.

**Validation:**
- A 2-slot wave-mode dogfood run on a representative parallel PRD completes with byte-identical DB state, ephemeral-branch lifecycle (created → merged → deleted), and stderr capture vs. pre-refactor baseline.
- The five existing tests covering parallel-slot defenses (in `src/loop_engine/worktree.rs` and `src/commands/next/selection.rs`) continue to pass without modification.

### FR-004: Extract `slot.rs` (slot lifecycle + result processing)

Move from `engine.rs` into `src/loop_engine/slot.rs`:
- `pub fn run_slot_iteration(...)` (currently `engine.rs:557`).
- `fn slot_early_exit(...)` (currently `engine.rs:512`).
- `fn claim_slot_task(conn: &Connection, task_id: &str) -> bool` (currently `engine.rs:787`).
- `fn slot_failure_result(...)` (currently `engine.rs:818`).
- `fn process_slot_result(...)` (currently `engine.rs:1409`).
- The `SlotContext`, `SlotResult`, `SlotEarlyExit`, `SlotFailureKind` types if they are only consumed by slot.rs and wave_scheduler.rs; if `IterationContext` consumes them too, they stay in `engine.rs` and are re-exported.

**Details:**
- `slot.rs::run_slot_iteration` is called from inside `wave_scheduler.rs::run_parallel_wave` on a thread spawned per slot. The `Send + Sync` requirement on slot inputs is unchanged; `SlotPromptBundle::Send` is already enforced by a compile-time assertion in `prompt/mod.rs`.
- `slot.rs::process_slot_result` calls `iteration_pipeline::process_iteration_output` — same contract as `iteration.rs::run_iteration`.
- `claim_slot_task` uses `WHERE id = ? AND status IN ('todo', 'in_progress')` for race-safe slot resumption. **After TaskLifecycle Extraction**, this routes through `TaskLifecycle::try_claim(task_id, &[TaskStatus::Todo, TaskStatus::InProgress])`. This PRD does NOT re-implement the claim — it moves the existing call to the new module.

**Validation:**
- Wave-mode tests in `tests/parallel_*` continue to pass.
- A 4-slot wave-mode dogfood run on a representative parallel PRD completes successfully and demonstrates the slot-0-safety-guard (defense layer #5) is intact (verified by injecting a stale `<branch>-slot-0` ref in a test fixture and asserting `classify_ephemeral_branch` returns `Err`).

### FR-005: Per-task recovery cluster placement

The per-task recovery cluster — `check_crash_escalation`, `check_override_invalidation`, `should_auto_block`, `should_escalate_for_consecutive_failures`, `apply_pending_promotion`, `escalate_task_model_if_needed_inner`, `escalate_task_model_if_needed`, `increment_consecutive_failures`, `reset_consecutive_failures`, `auto_block_task`, `handle_task_failure`, `prompt_overflow_result`, `probe_rate_limit_lifted`, `update_trackers`, `normalize_baseline` — is called from both sequential and wave paths.

**Two approaches are evaluated in §6. Selected approach is committed during PRD review (before any extraction begins).**

**Validation:**
- The `5ba153a7-FEAT-007` Grok runtime-error fallback hook behavior is byte-identical (test `tests/grok_runtime_error_fallback.rs` continues to pass).
- The `5ba153a7-FEAT-008` override-invalidation escape valve continues to fire at the top of every iteration regardless of which module owns the call.

### FR-006: `iteration_pipeline.rs` parity assertion is strengthened

Today, the parity contract is maintained by a comment in `prompt/mod.rs` ("new sections MUST also be wired through `slot`"). This PRD adds a mechanical enforcement:

- A test (in `tests/iteration_pipeline_parity.rs` or as a compile-time assertion in `iteration_pipeline.rs`) confirms that **both** `iteration::run_iteration` and `slot::process_slot_result` call `process_iteration_output` with a `ProcessingParams` constructed from the same set of fields.
- The test uses a small reflection trick: each call site has a `#[cfg(test)] fn call_sites_use_full_params(params: ProcessingParams<'_>) -> ()` that exhaustively destructures `params`. A new field added to `ProcessingParams` without updating both destructures fails to compile.

**Validation:**
- The test exists and passes; deliberately adding a field to `ProcessingParams` without updating both call sites causes a compile error (verified manually during PRD implementation).

### FR-007: Module-level `CLAUDE.md` update

Update `src/loop_engine/CLAUDE.md`'s "Touchpoints" table so every row points at the new module. The narrative sections ("Overflow recovery and diagnostics", "Iteration pipeline (shared)", "Parallel-slot scheduling") are updated where they reference `engine.rs` symbols by name. No content is deleted; only locations are updated.

**Validation:**
- Manual diff review of `CLAUDE.md` confirms every `engine.rs::<symbol>` reference is either updated to the new location or stays valid because the symbol stayed in `engine.rs`.

### FR-008: Public surface stability

The `pub` and `pub(crate)` items that other modules import are stable across this PRD. Specifically:
- `crate::loop_engine::run_loop` remains importable from the same path. If it moves to `orchestrator.rs`, `mod.rs` re-exports it via `pub use orchestrator::run_loop`.
- `crate::loop_engine::IterationContext`, `IterationResult`, `IterationOutcome`, `LoopRunConfig`, `LoopResult` remain importable from the same path.
- `crate::loop_engine::apply_status_updates` (if it still exists post-TaskLifecycle) and the recovery primitives consumed by external tests (e.g., `escalate_task_model_if_needed`, `should_auto_block`) remain importable from the same path.

**Validation:**
- `cargo check --all-targets --all-features` passes without import changes outside `src/loop_engine/`.

---

## 5. Non-Goals (Out of Scope)

- **Reducing the wave/sequential semantics gap.** Sequential and wave will continue to call the same shared `iteration_pipeline` for post-Claude work and otherwise have their own structure. Closing more parity gaps is its own PRD.
- **Introducing trait abstractions over the orchestration.** No `IterationStrategy` trait, no `dyn Scheduler` indirection. This refactor moves code; it does not abstract it.
- **Touching `runner.rs`, `overflow.rs`, `worktree.rs`, `merge_resolver.rs`, `prd_reconcile.rs`, `git_reconcile.rs`, `auto_review.rs`, `prompt/*`, `prompt_sections/*`.** These are already extracted and stable. This PRD only touches `engine.rs` (the source) and creates the four new module files.
- **Improving the per-task recovery surface.** The recovery cluster moves (per FR-005) but its internal logic is not refactored. That's a follow-up PRD if warranted.
- **Performance optimization.** This is a structural refactor; allocation patterns and the hot path should not change.
- **Prompt assembler unification (Coherence design item 3) or recall abstraction (item 4).** Phase 2 work, separate PRDs.
- **Status-write site changes.** All status writes already route through `TaskLifecycle` (Phase 1 first PRD). This refactor does not touch the service surface.
- **Boundary contract enforcement with runner-trait-hygiene Phase 2.** That PRD coordinates separately; see §6 below.
- **Renaming public items.** `run_loop`, `run_iteration`, `run_wave_iteration`, `process_slot_result`, etc. keep their names. Internal helpers may shed redundant prefixes (e.g., `wave_*` → `*` inside `wave_scheduler.rs`) but exported symbols are stable.

---

## 6. Technical Considerations

### Affected Components

| File | Change |
| --- | --- |
| `src/loop_engine/engine.rs` | Shrinks from 9644 → < 1500 lines; keeps public types, re-exports new module items, may keep the recovery cluster if Approach B wins |
| `src/loop_engine/orchestrator.rs` (NEW) | Owns `run_loop`, signal handler, run begin/end, human-review trigger, decisions prompt |
| `src/loop_engine/iteration.rs` (NEW) | Owns `run_iteration` (sequential) + sequential-only helpers |
| `src/loop_engine/wave_scheduler.rs` (NEW) | Owns `run_wave_iteration`, `run_parallel_wave`, preflight, deadlock, slot-context build, merge-back orchestration |
| `src/loop_engine/slot.rs` (NEW) | Owns `run_slot_iteration`, slot early exit, `claim_slot_task`, slot failure result, `process_slot_result` |
| `src/loop_engine/recovery.rs` (NEW, conditional on Approach A) | Owns per-task recovery cluster called by both sequential and wave paths |
| `src/loop_engine/mod.rs` | Adds the new module declarations and `pub use` re-exports |
| `src/loop_engine/CLAUDE.md` | Touchpoints table updated; narrative sections updated for new locations |
| `src/loop_engine/iteration_pipeline.rs` | Unchanged content; new parity assertion test added in `tests/iteration_pipeline_parity.rs` |
| `tests/` | New: `tests/iteration_pipeline_parity.rs` (FR-006); modified: any test that imports moved symbols (should be zero if re-exports hold) |

### Dependencies

- **TaskLifecycle Extraction PRD (`035925a9`) MUST merge first.** This PRD assumes the LIFECYCLE-EXCEPTION grep lint is green, `claim_slot_task` routes through `TaskLifecycle::try_claim`, and the ~12 Category C recovery sites in `engine.rs` already route through the service. Carving on top of an in-flight lifecycle migration would create rebase hell.
- **No new external crates.** This is a pure refactor.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| **A. Five new modules** (orchestrator + iteration + wave_scheduler + slot + recovery) | Each cluster has a named home; `engine.rs` becomes a thin re-export shell (~500 lines); future recovery refactors are scoped to one file | One additional module to introduce; `recovery.rs` becomes a cross-cutting dependency for both `iteration.rs` and `wave_scheduler.rs` (already true in `engine.rs` today) | **Preferred** |
| **B. Four new modules** (orchestrator + iteration + wave_scheduler + slot); recovery cluster stays in `engine.rs` | Smaller diff; `engine.rs` retains the cluster of cross-cutting helpers that both other modules import | `engine.rs` stays ~2500 lines (the recovery cluster is ~1000 lines plus IterationContext/etc.); future recovery improvements still happen in the monolith | Alternative |
| **C. Six new modules** (split `iteration.rs` and `wave_scheduler.rs` further) | Even smaller files | Each module becomes too narrow; cross-module coordination overhead increases; harder to onboard | Rejected |

**Selected Approach**: **A. Five new modules.** Rationale:
1. The Phase 2 foundation principle applies: extracting `recovery.rs` now (one day of work) saves a future refactor when the recovery cluster grows (rate-limit detection, new failure modes, retry strategies). The cluster is already a clear concern with ~15 functions; leaving it in `engine.rs` is the path of least resistance, but the next time we add a recovery primitive we'll be back here.
2. The cross-module dependency that `recovery.rs` introduces (both `iteration.rs` and `wave_scheduler.rs` import it) is exactly the same dependency shape that exists today inside `engine.rs` — it's just made explicit.
3. The dogfood gate cost is the same for Approach A or B; the marginal cost of one extra extraction is bounded by ~1 day.

**Phase 2 Foundation Check**: Approach A costs ~1 day more than Approach B but avoids a future "we should have extracted recovery.rs" refactor when the next recovery primitive lands. 1:10 ratio: ~1 day now vs. ~2 weeks of next-refactor churn (extracting recovery while also touching the new feature) = clearly worth it. Pre-launch, foundations compound.

**Extraction order** (decided 2026-05-20 per architect R8): **slot.rs first, then recovery.rs**, then wave_scheduler.rs, then iteration.rs, then orchestrator.rs. Rationale: extracting slot.rs first creates the first cross-module consumer of recovery functions; when recovery.rs extracts in the second step, the cross-module call shape is exercised immediately with a real consumer (slot.rs imports update atomically inside the recovery.rs extraction commit). This catches any visibility / re-export issues in the recovery extraction itself rather than discovering them later when wave_scheduler.rs or iteration.rs land.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Carve accidentally breaks one of the five parallel-slot defenses (slot-path threading, halt threshold, implicit-overlap baseline, ephemeral overlay, stale ephemeral hygiene) | High — silent regression to the cascade-failure bug class | Medium — these defenses are subtle and the diff is large | (a) Move each defense as a single-commit extraction with the existing tests in scope. (b) Dogfood gate: 4-slot wave on a representative parallel PRD post-extraction. (c) Each defense has a regression test in `worktree.rs` / `selection.rs` already; CI catches direct breakage. |
| `iteration_pipeline.rs` parity invariant silently breaks when a future field is added to `ProcessingParams` | Medium — wave/sequential divergence is the bug class this whole refactor is preventing | Low — we're adding a mechanical assertion (FR-006) | Compile-time / test-time assertion that BOTH call sites exhaustively destructure `ProcessingParams`. Deliberate field-add-without-destructure verified during PRD implementation as a compile failure. |
| The carve creates merge conflicts with TaskLifecycle Extraction (PRD `035925a9`) if both are open at once | High — large rebase + risk of dropping a defense during conflict resolution | Low — TaskLifecycle Extraction MUST merge first per §2.5 of the coherence design and the Cluster A serialization rule | Hard dependency: this PRD does not start until `035925a9-MILESTONE-FINAL` is `done` and the branch is merged to `main`. Stated as a CLARIFY-001-style gate at the top of the task list. |
| Per-task recovery extraction (Approach A) breaks the `apply_pending_promotion`-after-`tx.commit()` invariant | High — could reintroduce the in-memory-state-mutation-vs-rolled-back-DB-write bug class | Low — the existing pattern (`inner` helper returns `PendingPromotion`, convenience wrapper applies post-commit) survives extraction verbatim | Move the pair as one commit; existing tests in `engine.rs` for the transactional pattern come with it; dogfood gate's overflow-recovery scenario exercises the path. |
| Dogfood gate finds a subtle regression after 10 iterations | Medium — late discovery is more expensive than early | Medium — refactors of this size historically surface one or two surprises | Run the dogfood gate on TWO distinct PRDs (per the coherence design's `N=10 iterations across two distinct PRDs` rule). Capture stderr + DB-final-state snapshots BEFORE the carve begins as the reference; any divergence post-carve is a hard fail. |

### Security Considerations

- No new attack surface. This refactor moves code; it does not add inputs, change permissions, or alter spawn boundaries.
- The `PermissionMode` plumbing (Auto / Scoped / Dangerous) is preserved verbatim across extractions. The Claude/Grok permission-mode flag mappings (in `runner.rs`) are unchanged.
- Signal-handler ownership moves to `orchestrator.rs` but the SIGTERM-grace-SIGKILL escalation contract is identical.

### Public Contracts

#### New Interfaces

| Module/Symbol | Signature | Returns | Side Effects |
| --- | --- | --- | --- |
| `orchestrator::run_loop` | `async fn run_loop(LoopRunConfig) -> LoopResult` | `LoopResult` | (same as today: DB writes, stderr emissions, spawning runner subprocesses, signal handler arming) |
| `iteration::run_iteration` | `fn run_iteration(...) -> IterationResult` | `IterationResult` | (same as today) |
| `wave_scheduler::run_wave_iteration` | `fn run_wave_iteration(...) -> WaveOutcome` | `WaveOutcome` | (same as today) |
| `slot::run_slot_iteration` | `fn run_slot_iteration(...) -> SlotResult` | `SlotResult` | (same as today) |
| `slot::process_slot_result` | `fn process_slot_result(...) -> ()` | `()` | (same as today) |
| `recovery::*` (Approach A) | Various per-task recovery primitives | (existing return shapes) | (same as today) |

Note: every "new" interface is actually a relocated existing function. Signatures are byte-identical pre/post-refactor. No new behavior is introduced.

#### Modified Interfaces

| Module/Symbol | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `crate::loop_engine::run_loop` | `pub async fn run_loop(LoopRunConfig) -> LoopResult` | (unchanged via `pub use orchestrator::run_loop` in `mod.rs`) | No | None |
| `crate::loop_engine::run_iteration` | `pub fn run_iteration(...) -> IterationResult` | (unchanged via `pub use iteration::run_iteration` in `mod.rs`) | No | None |

(All other re-exported symbols follow the same pattern: the import path stays stable; the implementation moves.)

### Data Flow Contracts

**N/A** — this refactor does not introduce new cross-module data structures. Existing types (`IterationContext`, `SlotContext`, `WaveIterationParams`, `SlotIterationParams`, `ProcessingParams`, `RunnerOpts`, `RunnerResult`) keep their current layouts and ownership rules. The only data-flow shift is the literal location of the function that constructs / consumes them, which is what the public-contract table above tracks.

### Consumers of Changed Behavior

**The carve changes no observable behavior.** No consumer table is required because no consumer perceives a change. The dogfood gate (10 iterations × 2 PRDs, byte-identical DB + stderr) is the proof.

If a consumer DOES perceive a change (any stderr-line difference, any DB-state difference), that is by definition a regression and the PRD does not ship until it's fixed.

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| `claim_slot_task` (slot-0 race-safe claim) | Called from `slot.rs::run_slot_iteration` post-extraction | `WHERE id = ? AND status IN ('todo', 'in_progress')` via `TaskLifecycle::try_claim` (post-TaskLifecycle PRD) | Same — the call moves to `slot.rs`, the predicate is unchanged |
| `check_override_invalidation` (operator escape valve) | Called at top of every iteration, before `resolve_effective_runner` | Clears all six per-task auto-recovery channels in one shot if `tasks.model` diverges from the snapshot | Same — the call site moves to `iteration.rs` (sequential) and `wave_scheduler.rs` or `slot.rs` (wave); the order-before-resolve constraint is preserved |
| `apply_pending_promotion` (transactional promotion) | Called AFTER `tx.commit()?` returns Ok, with the `PendingPromotion` produced by `escalate_task_model_if_needed_inner` | Mutates ctx (`runner_overrides`, `model_overrides`, `overflow_original_task_model`) only after the DB commit succeeds | Same — `inner` and convenience wrapper move as a pair to `recovery.rs` (Approach A) or stay in `engine.rs` (Approach B); the contract is preserved |
| `SYNTHETIC_DEADLOCK_SLOT = usize::MAX` | Used by `handle_ephemeral_deadlock` to ensure `failed_merges` is non-empty when every blocking ephemeral had a malformed suffix | Synthesis pattern emits at least one record so `apply_merge_fail_reset_and_halt_check` sees `!is_empty()` and trips | Same — sentinel and handler move together to `wave_scheduler.rs` |

### Inversion Checklist

- [ ] Every defense layer (1–5 in `src/loop_engine/CLAUDE.md`) has its existing regression test still pass post-extraction?
- [ ] Every `pub` or `pub(crate)` symbol that's imported from outside `src/loop_engine/` continues to resolve from the same path?
- [ ] Every `iteration_pipeline.rs` field added/removed in the future is forced to update BOTH call sites at compile time?
- [ ] The recovery cluster's transactional pattern (`inner` + `apply_pending_promotion` after commit) is moved as one commit, not split?
- [ ] The `SYNTHETIC_DEADLOCK_SLOT` sentinel and its handler move together?
- [ ] `iteration_pipeline.rs` itself is not touched (its parity assertion is added in `tests/`, not the production file)?
- [ ] Dogfood gate runs against TWO distinct PRDs, not one?

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/loop_engine/CLAUDE.md` | Update | Touchpoints table updated; narrative sections re-pointed where they name `engine.rs::<symbol>` |
| `docs/designs/coherence-refactoring.md` | Update | Add a brief "Retrospective: Phase 1 second PRD" appendix once this PRD lands |
| `CLAUDE.md` (project root) | No change | No public-CLI changes; no operator-visible workflow changes |
| `.claude/commands/*` | No change | Slash commands are unaffected |
| Rustdoc on new module files | Create | Each new module gets a module-level rustdoc explaining its scope and pointing at sibling modules |

---

## 7. Open Questions

- [x] Approach A vs B (five-module vs four-module carve)? **Resolved: Approach A** (see §6 Selected Approach).
- [ ] Should `IterationContext` / `IterationResult` move to `orchestrator.rs` or stay in `engine.rs`? **Default: stay in `engine.rs`** because they're consumed by external tests and other modules; moving them would require updating many import paths. Confirm during implementation if the import surface is small enough to make moving practical.
- [ ] Does the dogfood gate run against `tasks/parallel-task-execution.json` (a representative wave PRD) and `tasks/curate-session-cleanup.json` (a representative sequential PRD)? **Default: yes**; pick alternatives at implementation start if those PRDs are already complete or unavailable.
- [ ] Should the per-task recovery cluster extracted into `recovery.rs` also gain a unit-test module (`recovery_tests.rs`)? **Default: no** — the existing tests in `engine.rs` move with the cluster; adding a new test module is a separate hygiene PRD.

---

## Appendix

### Related Documents

- `docs/designs/coherence-refactoring.md` — parent design document
- `docs/designs/runner-trait-hygiene.md` — parallel effort; see "Boundary Contract with Runner Trait Hygiene Effort"
- `tasks/prd-tasklifecycle-extraction.md` — Phase 1 first PRD; MUST merge first
- `src/loop_engine/CLAUDE.md` — subsystem design notes; touchpoints table is updated by this PRD
- `tasks/prd-unify-sequential-and-wave-execution.md` — established `iteration_pipeline.rs`; the parity-divergence prevention pattern this PRD generalizes

### Boundary Contract with Runner-Trait-Hygiene Phase 2

This PRD and the parallel runner-trait-hygiene Phase 2 PRD (`prd-01-runner-capability-enforcement.md`) both touch `engine.rs`. The boundary rules:

- **runner-trait-hygiene Phase 2** edits `runner.rs` (adding `RunnerCapability`, `supports`, dispatch enforcement) and the few `engine.rs` call sites that today hard-code provider-specific branches (e.g., `RunnerKind::Claude` vs `RunnerKind::Grok` at `engine.rs:5044`).
- **This PRD** moves the call sites. If runner-trait-hygiene Phase 2 changes a call site, this PRD's wave_scheduler / slot extraction picks up the changed version.
- **First-to-merge wins.** Whichever PRD merges first leaves clear seams for the second. If runner-trait-hygiene Phase 2 merges first, the affected call sites are slightly different but the extraction targets the same logical block.
- **Listed as a "review for overlap" stakeholder** on each PRD's code review.

### Glossary

- **Carve**: extracting a coherent cluster of functions from a monolithic file into a new sibling module.
- **Defense layer (1–5)**: one of the five layered parallel-slot defenses documented in `src/loop_engine/CLAUDE.md`.
- **Dogfood gate**: running `task-mgr loop` on a real PRD against the refactored code, asserting byte-identical observable behavior vs. a baseline capture.
- **Parity invariant**: the rule that sequential and wave paths share `iteration_pipeline::process_iteration_output` — any post-Claude step added must run for both paths.
- **Synthetic-deadlock sentinel**: `SYNTHETIC_DEADLOCK_SLOT = usize::MAX`, used to ensure the FEAT-002 halt-threshold trips even when every blocking ephemeral had a malformed suffix.
