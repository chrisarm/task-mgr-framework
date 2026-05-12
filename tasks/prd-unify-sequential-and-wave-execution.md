# PRD: Unify sequential and parallel-slot execution paths

**Type**: Refactor
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-05-07
**Status**: Draft

---

## 1. Overview

### Problem Statement

`run_iteration` (sequential) and `run_slot_iteration` + `process_slot_result` (parallel-wave) have drifted into two materially different pipelines. Wave mode silently skips behaviors the sequential path treats as core. The most visible consequence: the bandit/learnings system is half-blind in wave mode — slot prompts ship without learnings injected, slot output is never extracted for new learnings, and bandit feedback never updates. As parallel waves become the default for multi-task PRDs, this gap is making the loop progressively dumber the more it is used in its primary mode.

### Background

- FEAT-010 introduced parallel-wave execution with a deliberately stubbed `build_slot_prompt` and a comment promising "follow-up wiring tasks" (`src/loop_engine/engine.rs:354-356`). That follow-up never landed.
- Two structural constraints shape the design space: (1) `rusqlite::Connection` is `!Send` — slot worker threads must each open their own connection (learnings #1893, #1852, #1871). (2) Task claiming and post-wave processing already happen on the main thread (learnings #1895, #1899) — the codebase has the right division of labor; we just need to extend it.
- Related systems already work correctly and should be reused, not rewritten: shared helpers (`mark_task_done`, `apply_status_updates`, `parse_completed_tasks`, `complete_cmd::complete`, `key_decisions_db::insert_key_decision`), the LearningWriter chokepoint (graceful Ollama degradation), and the four-rung overflow recovery ladder (learning #2031).
- A user-approved plan exists at `/home/chris/.claude/plans/what-else-is-being-expressive-puzzle.md` and is the authoritative architectural reference for this PRD.

---

## 2. Goals

### Primary Goals

- [ ] Slot prompts include learnings, source context, tool awareness, key-decisions instructions, and completed-dependencies sections (parity with sequential where applicable to disjoint tasks)
- [ ] Slot output flows through `extract_learnings_from_output` so new learnings discovered in wave mode are captured
- [ ] `record_iteration_feedback` is called for every slot iteration so the UCB bandit updates regardless of execution mode
- [ ] Both paths share a single post-Claude pipeline module — adding a behavior in one place benefits both
- [ ] Sequential path output is byte-identical to today's after the prompt-builder split (Phase A regression guarantee)
- [ ] Per-slot PromptTooLong recovery uses the same four-rung ladder as sequential, isolated to the slot's own task
- [ ] Crash escalation uses **per-task crash tracking** (`HashMap<String, bool>`) so wave mode correctly escalates re-picked crashed tasks, not the brittle "last task ID == current task ID" predicate
- [ ] Wave mode gains parity with sequential's `is_task_reported_already_complete` fallback (engine.rs:3435-3457) via the shared pipeline

### Success Metrics

- **Learnings extracted in wave mode**: SQL `SELECT COUNT(*) FROM learnings WHERE created_at > <wave_start>` returns >0 after a parallel wave that previously returned 0 (today: always 0 in wave mode)
- **Bandit feedback in wave mode**: `learning_feedback` rows with `task_id` matching slot task IDs (today: zero such rows)
- **Test parity**: 100% of pre-existing tests pass; new tests assert the new parity
- **Sequential regression budget**: snapshot test for `prompt::sequential::build_prompt` is byte-identical against pre-refactor baseline for at least one fixture task (Phase A guarantee)
- **Code locality**: each shared post-Claude behavior (learnings extraction, feedback recording, status dispatch, completion detection ladder, key-decisions extraction, already-complete fallback, crash-escalation context update) has **exactly one production implementation** inside `iteration_pipeline.rs`. The pipeline is invoked from two call sites: `run_loop` (sequential, post-`run_iteration`) and `process_slot_result` (wave). Today: behaviors are split inline across `run_iteration` (engine.rs:2032-2059) and `run_loop` (engine.rs:3178-3530), with most missing entirely from `process_slot_result`.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Connection threading**: every `&Connection` use must remain on the main thread. `SlotPromptBundle` (the data shipped to slot workers) MUST be `Send`. Add a compile-time `assert_send::<SlotPromptBundle>()` test.
- **Idempotent post-processing**: calling `iteration_pipeline::process_iteration_output` exactly once per Claude invocation must produce the same DB state as today's inline sequential code does once. Calling it for N slots in a wave must be equivalent to N independent sequential post-processing calls.
- **No silent learning-injection failures**: if the learnings retrieval query errors, the slot prompt MUST still build (graceful degradation matching today's sequential behavior — learnings are a trimmable section, not critical). Same rule for source context.
- **Crash escalation correctness**: replace the brittle `last_task_id == current_task_id && last_was_crash` predicate (engine.rs:4207-4216) with a `HashMap<String, bool> crashed_last_iteration` on `IterationContext`. Both paths populate it per-task (sequential: one entry per iteration; wave: one entry per slot result). `check_crash_escalation` consults the map. Today wave mode never updates the legacy fields, AND the legacy predicate has a structural false-negative: even after this PRD's "last slot wins" patch, escalation only fires when the last-processed slot's task is also next-iteration's pick — a rare combination. Per-task tracking eliminates this entire class of false-negatives.
- **Per-slot overflow isolation**: a `PromptTooLong` outcome on slot 2 must not corrupt the recovery state of slot 0, 1, or 3. Each slot's overflow handling is keyed on its own task ID.
- **Phase A regression freedom**: the prompt-builder split (core / sequential / slot modules) must produce a byte-identical sequential prompt for at least one fixture task. No semantic change in Phase A.

### Performance Requirements

- **Main-thread serial cost**: building N slot prompts on the main thread before spawning workers adds serial latency. Budget: ≤200ms per slot × ≤8 slots = ≤1.6s of added pre-spawn latency per wave. Measure on a real wave; flag for optimization (cache learnings query across slots within a wave) if exceeded.
- **No N+1 DB queries**: when building N slot prompts in succession on the main thread, the learnings query MAY be re-issued per slot (acceptable today). Note as a future optimization if hot.
- **Pipeline overhead**: `iteration_pipeline::process_iteration_output` must not add measurable overhead vs the inline sequential code it replaces (function-call indirection only).

### Style Requirements

- Follow existing codebase patterns from `src/loop_engine/`: per-thread `Connection`, `SignalFlag` Arc-wrapped, structured error discriminators (no string-sniffing — see learning #2005), enum-based outcome types (see learning #2009).
- No `.unwrap()` in production paths unless provably safe and commented why. Best-effort observability writes (overflow dumps, JSONL events) follow existing pattern: log via `eprintln!` and never propagate failures (matches `overflow.rs` rotation pattern).
- New module `iteration_pipeline.rs` exposes `pub(crate)` functions only; the module IS the seam, individual helpers are not part of the public API.
- New `SlotPromptBundle` struct: `Send` but not `Sync` (matches `SlotContext` per learning #1864).
- Function length guideline (≤30 lines) applies to new helpers; existing functions exceeding the guideline are acceptable when single-purpose (per learning #1586).

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --------- | -------------- | ----------------- |
| Ollama down when slot prompt is built | Slot prompt builder calls into learnings retrieval which may hit Ollama for embeddings | Slot prompt builds with empty learnings section (graceful degradation, matches sequential per LearningWriter contract) |
| Slot task has empty `touchesFiles` (milestone/verification task) | Sequential includes a "non-code task" note in completion instruction; slot must too | Slot prompt includes the same non-code-note logic from `prompt::core::completion_instruction` |
| Slot 0 crashes pre-merge but slot 1 finishes cleanly | Today wave-level merge handles this; pipeline must not double-process slot 0 | `process_slot_result` for slot 0 sees `Crash` outcome and skips completion detection but still records feedback (with no-op `shown_learning_ids`) |
| Same task ID hits `PromptTooLong` twice in two consecutive waves | Sequential's `ctx.overflow_recovered` set tracks first-overflow state for banner annotation | Wave mode must use the same `IterationContext` mutex-free fields (only main thread writes); confirm `HashSet::insert` semantics work the same way |
| `<completed>X</completed>` emitted by slot assigned task Y (cross-task completion) | Today's `process_slot_result` correctly handles this (engine.rs:1216-1226 comment); pipeline must preserve it | Y is marked done; X stays in pending set; orphan reset still works |
| Slot worker thread panics after main thread builds the bundle | Bundle was built but never used; main thread must still see it as a slot failure result | `slot_failure_result` builder takes the bundle's `task_id` so accounting is correct |
| Sequential iteration immediately follows wave iteration | Per-task crash tracking (this PRD) ensures crash escalation fires exactly when a previously-crashed task is re-picked, regardless of which path crashed it | New test: `test_wave_crash_then_sequential_repick_escalates`; covers cross-mode boundary |
| Build-time `Send` regression on `SlotPromptBundle` | A future field added to the bundle introduces a non-Send type (e.g., `Rc`, `RefCell`) | `assert_send` test fails at compile time before runtime exposure |
| Phase A snapshot test fixture has nondeterministic sections | `record_shown_learnings` writes to DB; `build_synergy_section` / `build_dependency_section` order may depend on insert order; `scan_source_context` reads project root | Fixture rules: deterministic iteration number + fresh DB; fixture has zero synergy partners + zero dependencies in v1; controlled `project_root` with stable content; `cargo grep` for `HashMap<.*Section\|prompt` before merging Phase A |
| Wave slot's Claude conversation must thread to learnings extraction | Sequential reads `claude_result.conversation` inline; wave's `SlotResult.iteration_result.output` is `String` only, no conversation field | Add `conversation: Option<String>` to `IterationResult` (cross-cuts both paths' result type, single source of truth); pipeline takes `claude_conversation: Option<&str>` in `IterationProcessingParams` |
| Tasks completed via different completion paths in same iteration | Today's `process_slot_result` uses HashSet `counted` to avoid double-counting (engine.rs:1125-1131); sequential's `run_loop` increments `tasks_completed` across multiple branches without dedup | Pipeline owns the dedup HashSet; `ProcessingOutcome.tasks_completed` reflects deduped count; both call sites consume it directly |

---

## 3. User Stories

### US-001: Wave-mode learnings flow

**As a** task-mgr operator running parallel waves
**I want** the bandit / learnings system to update from wave-mode runs the same way it does for sequential runs
**So that** running with `parallel_slots > 1` doesn't silently degrade the system's learning-recall quality over time

**Acceptance Criteria:**

- [ ] After a wave completes, new `<learning>` tags emitted by any slot are persisted to the `learnings` table (with embedding scheduled per LearningWriter contract)
- [ ] After a wave completes, `learning_feedback` rows are updated with `shown_count` / `success_count` increments tied to the slot's task ID
- [ ] Slot prompts contain a "## Relevant Learnings" section when matching learnings exist (today: no such section)

### US-002: Single source of truth for post-Claude work

**As a** maintainer adding a new behavior to the post-Claude pipeline (e.g., a new tag parser or DB write)
**I want** to add it in exactly one place
**So that** I don't have to remember to mirror it across two divergent paths and risk forgetting wave mode again

**Acceptance Criteria:**

- [ ] `extract_learnings_from_output` has one production call site (`iteration_pipeline::process_iteration_output`)
- [ ] `record_iteration_feedback` has one production call site (same)
- [ ] `apply_status_updates`, `parse_completed_tasks`, `scan_output_for_completed_tasks`, `key_decisions_db::insert_key_decision`, `is_task_reported_already_complete` invocations all live inside `iteration_pipeline` (one site each)
- [ ] **Sequential pipeline call site lives in `run_loop`** (engine.rs:~3178), invoked AFTER `run_iteration` returns its raw `IterationResult`. `run_iteration` shrinks to "select task → spawn Claude → return raw `IterationResult`" — it does NOT own post-Claude work after this PRD.
- [ ] **Wave pipeline call site lives in `process_slot_result`** (engine.rs:1053), invoked once per slot result on the main thread after slot workers join.
- [ ] Sequential-only outer concerns stay where they are today: rate limit, pause signal, wrapper commit, git-detection completion fallback (sequential's git-hash detection branch), external-git reconciliation, human review triggering, overflow ladder dispatch (which itself calls into `iteration_pipeline` if we centralize overflow there too — see FR-004).
- [ ] `process_slot_result` (wave) calls the pipeline with `skip_git_completion_detection: true` (slot commits live on ephemeral branches; merge-back + post-wave external-git reconciliation handle them).

### US-003: Three-builder prompt model

**As a** maintainer touching prompt construction
**I want** shared sections to live in `prompt::core` and path-specific composition in `prompt::sequential` / `prompt::slot`
**So that** changing a shared section (e.g., task JSON shape) updates both paths automatically, while path-specific sections (e.g., synergy escalation) stay localized

**Acceptance Criteria:**

- [ ] `src/loop_engine/prompt/core.rs` exposes `format_task_json`, `completion_instruction`, `load_base_prompt`, `build_learnings_block`, `build_source_context_block`, `build_tool_awareness_block`, `build_key_decisions_block`
- [ ] `src/loop_engine/prompt/sequential.rs` contains today's `build_prompt` logic, refactored to compose via `prompt::core` helpers + sequential-only sections
- [ ] `src/loop_engine/prompt/slot.rs` exposes `build_prompt(conn, task, params) -> SlotPromptBundle`, called on the main thread by `run_parallel_wave` before spawning workers
- [ ] `prompt::build_prompt` (today's public symbol) re-exports `prompt::sequential::build_prompt` so existing call sites compile unchanged

### US-004: Per-task crash escalation

**As a** task-mgr operator running mixed wave + sequential workloads
**I want** a task that crashed in any prior iteration (wave or sequential) to be escalated when re-picked
**So that** wave-mode crashes don't permanently dodge model escalation due to a brittle "last task ID" predicate

**Acceptance Criteria:**

- [ ] `IterationContext` gains a `crashed_last_iteration: HashMap<String, bool>` field (or equivalent set of crashed task IDs since the bool is always true)
- [ ] `iteration_pipeline::process_iteration_output` populates the map for the task it processed (one entry per iteration in sequential, one per slot result in wave)
- [ ] `check_crash_escalation` (engine.rs:4207-4216) consults the map instead of `last_task_id == current_task_id && last_was_crash`
- [ ] On non-crash outcomes the entry is removed (or set to false) so the task isn't permanently flagged
- [ ] Cross-mode test: wave crashes task X → next sequential iteration re-picks X → escalation fires
- [ ] Legacy `last_task_id` and `last_was_crash` fields on `IterationContext` are removed (or marked deprecated and stop being read by `check_crash_escalation`)

### US-005: Wave mode gets the "already complete" fallback

**As a** task-mgr operator running parallel waves
**I want** Claude reporting "task already complete" without a `<completed>` tag to still close the task in wave mode
**So that** wave mode matches sequential's behavior when re-running PRDs against partially-completed work

**Acceptance Criteria:**

- [ ] The shared pipeline includes the `is_task_reported_already_complete` fallback (today only at engine.rs:3435-3457 in sequential `run_loop`)
- [ ] Wave mode test: a slot whose Claude output reports "task is already complete" (no `<completed>` tag, no commit hash) marks the task done

### US-006: Per-slot PromptTooLong recovery

**As a** task-mgr operator
**I want** a slot that hits `PromptTooLong` to recover via the four-rung ladder the same way a sequential iteration does
**So that** wave mode doesn't permanently block tasks that sequential would have rescued

**Acceptance Criteria:**

- [ ] When any slot's outcome is `Crash(PromptTooLong)`, `iteration_pipeline` invokes `overflow::handle_prompt_too_long` keyed on that slot's task ID
- [ ] Overflow JSONL events emitted from a slot include the slot index alongside the run/iteration metadata
- [ ] A `PromptTooLong` on slot 2 does not affect the recovery state of slots 0, 1, or 3 (each slot's recovery operates on its own task ID's `model_overrides` / `overflow_recovered` entries)
- [ ] Existing `learnings #2029` ("wave mode bypasses sequential PromptTooLong recovery — intentional") is invalidated/superseded as part of this PRD's completion (use `--supersedes 2029`)

---

## 4. Functional Requirements

### FR-001: Three-builder prompt module structure

The current `src/loop_engine/prompt.rs` (~600 lines) splits into a `prompt/` directory with three child modules plus a `mod.rs` re-export shim.

**Details:**
- `prompt/core.rs` — shared bedrock helpers (see US-003 acceptance criteria)
- `prompt/sequential.rs` — full sequential builder; preserves today's `BuildPromptParams`, `PromptResult`, `PromptOverflow` public types and signatures
- `prompt/slot.rs` — slim slot builder; produces `SlotPromptBundle`
- `prompt/mod.rs` — re-exports so all existing call sites continue to compile against `crate::loop_engine::prompt::build_prompt`

**Validation:**
- `cargo build` succeeds with no public API changes
- All existing prompt unit tests pass without modification (Phase A regression guarantee)

### FR-002: Slot prompt bundle and main-thread construction

A new `SlotPromptBundle` carries the assembled prompt string + side-channel data from main thread to slot worker.

**Details:**
- Struct definition (in `prompt/slot.rs`):
  ```rust
  pub struct SlotPromptBundle {
      pub prompt: String,
      pub task_id: String,
      pub task_files: Vec<String>,
      pub shown_learning_ids: Vec<i64>,
      pub resolved_model: Option<String>,
      pub effective_effort: Option<&'static str>,
      pub task_difficulty: Option<String>,
      pub dropped_sections: Vec<String>,
      pub section_sizes: Vec<(&'static str, usize)>,
  }
  ```
- Bundle is `Send` (carries no `Rc`, `RefCell`, or non-`Send` types). Compile-time `assert_send::<SlotPromptBundle>()` test guards regression.
- `prompt::slot::build_prompt(conn, task, params)` runs on the main thread and returns the bundle.
- `SlotContext` (engine.rs:271) is updated to carry `prompt_bundle: SlotPromptBundle` instead of `task: Task` + `base_prompt_path` lookup.
- `run_parallel_wave` (engine.rs:730) calls `prompt::slot::build_prompt` for each pre-claimed task BEFORE spawning the slot worker thread.
- `run_slot_iteration` (engine.rs:489) consumes `slot.prompt_bundle.prompt` directly; the old inline `build_slot_prompt` is deleted.

**Validation:**
- New test: `test_slot_prompt_bundle_is_send` (compile-time)
- New test: `test_slot_prompt_contains_learnings_section_when_learnings_exist`
- New test: `test_slot_prompt_built_on_main_thread_before_worker_spawn` (verifies ordering by checking `learning_shown_events` is populated before slot Claude spawn)

### FR-003: Shared post-Claude pipeline module

A new `src/loop_engine/iteration_pipeline.rs` module exposes `process_iteration_output` and is the single home for behaviors both paths perform. **The pipeline is invoked from `run_loop` (sequential) and `process_slot_result` (wave); `run_iteration` itself is shrunk to "select + spawn + return raw result".**

**Source-of-truth for sequential post-Claude steps being lifted:**

The sequential pipeline today is split across two functions; both contribute to the lift. Read these as the authoritative inventory:

- `run_iteration` (engine.rs:2032-2059): learnings extraction (2032-2053), `record_iteration_feedback` (2055-2059), PromptTooLong overflow recovery dispatch (2061-2094), `update_trackers` and `last_task_id`/`last_was_crash` updates (2096-2113), `last_files` update (2108-2109).
- `run_loop` (engine.rs:3178-3530): `progress::log_iteration` (~3178), key-decisions extraction + insert (3190-3207), `apply_status_updates` (3239), the full completion ladder — `<task-status>` tags → `<completed>` tags → git commit detection → output scan → `is_task_reported_already_complete` fallback (3258-3457), wrapper commit (3461-3471), external-git reconciliation (3479), human review triggering (3514-3530), tasks-completed counter increments across all completion branches (3278, 3304, 3336, 3395, 3451, 3488).

**Behaviors moving INTO the pipeline:**
1. `progress::log_iteration` (passes `slot_index`)
2. Key-decisions extraction loop → `key_decisions_db::insert_key_decision`
3. `detection::extract_status_updates` → `apply_status_updates`
4. Completion detection ladder via `detect_and_mark_completion` (new wrapper). Honors `skip_git_completion_detection` flag — sequential passes `false`, wave passes `true`. Internal order: `<task-status>:done` → `<completed>` tags → (sequential only) git-hash commit detection → output scan → `is_task_reported_already_complete` fallback. The fallback now runs in BOTH paths (US-005).
5. `learnings::ingestion::extract_learnings_from_output` — prefers `claude_conversation` over `output` (matches sequential preference at engine.rs:2034). Best-effort; errors logged via `eprintln!`.
6. `feedback::record_iteration_feedback`
7. **Per-task crash tracking**: `ctx.crashed_last_iteration.insert(task_id, was_crash)` for each task processed (US-004). On non-crash outcomes, set `false` (or remove) so the task isn't permanently flagged.

**Behaviors STAYING at the call site (not moving):**
- Wrapper commit (sequential only — uses git-hash detection that wave can't perform on ephemeral branches)
- External-git reconciliation (per-iteration in `run_loop` line 3479, per-wave in `run_wave_iteration` line 1426 — already correctly placed at the boundary, not per-slot)
- Human review triggering (deferred per existing design)
- Rate-limit handling, pause-signal handling, pre-iteration usage check (fire at the loop boundary, not per-slot)
- Tool-denial hint re-prompts (sequential-only mid-iteration retry behavior; orthogonal to post-Claude pipeline)
- Overflow ladder INVOCATION stays at the call site (`run_loop` for sequential, `process_slot_result` for wave); the ladder's body in `overflow.rs` is unchanged. See FR-004.

**Public signature:**
  ```rust
  pub(crate) fn process_iteration_output(
      conn: &mut Connection,
      ctx: &mut IterationContext,
      params: &mut IterationProcessingParams<'_>,
  ) -> ProcessingOutcome
  ```

**`IterationProcessingParams` fields:**
- `task_id: &str`
- `output: &str`
- `claude_conversation: Option<&str>` (preferred source for learnings extraction)
- `outcome: &mut IterationOutcome` — **mutable**: completion detection mutates outcome to `IterationOutcome::Completed` retroactively (sequential lines 3280, 3307, 3341, 3400, 3454). Caller observes the post-pipeline value.
- `shown_learning_ids: &[i64]`
- `run_id: &str`
- `db_dir: &Path`
- `signal_flag: &SignalFlag`
- `prd_path: &Path`
- `task_prefix: Option<&str>`
- `progress_path: &Path`
- `slot_index: Option<usize>` (None for sequential)
- `skip_git_completion_detection: bool` (true for wave)
- `iteration: u32`
- `effective_model: Option<&str>`
- `effective_effort: Option<&'static str>`
- `files_modified: &[String]`

**`ProcessingOutcome` fields (revised per architect feedback C5):**
- `tasks_completed: u32` — deduped count via internal HashSet (matches today's `process_slot_result` `counted` semantics at engine.rs:1125-1131; replaces `run_loop`'s un-deduped increments)
- `task_marked_done: bool` — whether the *currently-processed* task was marked done by any branch (renamed from `slot_marked_done` for path-agnostic naming; equivalent semantics)
- `files_aggregated: Vec<String>`
- `status_updates_applied: usize`
- `key_decisions_count: u32` — sequential's `IterationResult.key_decisions_count` was set in `run_loop` at line 3207 after extraction; pipeline now owns extraction so it must report the count
- `completed_task_ids: Vec<String>` — task IDs marked done in this call (cross-task completion: a slot processing X may emit `<completed>Y</completed>`, and callers need both IDs to update pending sets correctly per engine.rs:1216-1226)

**Validation:**
- New test: `test_process_iteration_output_extracts_learnings`
- New test: `test_process_iteration_output_records_feedback_for_shown_ids`
- New test: `test_process_iteration_output_skips_git_detection_when_flag_set`
- New test: `test_process_iteration_output_runs_already_complete_fallback_in_both_paths`
- New test: `test_process_iteration_output_dedups_tasks_completed_across_branches`
- New test: `test_process_iteration_output_mutates_outcome_on_retroactive_completion`
- Existing tests for sequential and wave behavior continue to pass after `run_loop` and `process_slot_result` are refactored to call the pipeline

### FR-004: Per-slot PromptTooLong recovery

Wave mode dispatches the four-rung overflow ladder per slot.

**Details:**
- When a slot's outcome is `Crash(PromptTooLong)`, `iteration_pipeline` (or the wave-level dispatcher) calls `overflow::handle_prompt_too_long(ctx, conn, &slot_task_id, ...)`. Today only sequential does this.
- `overflow::handle_prompt_too_long` is audited for safety under per-task concurrent invocation. The handler already keys on `task_id` for `model_overrides` / `effort_overrides` / `overflow_recovered` / `overflow_original_model` mutations — these are HashMap/HashSet entries on `IterationContext` written only on the main thread (after slots join), so no actual concurrency exists. Audit confirms: SAFE.
- `OverflowEvent` JSONL serialization gains an optional `slot_index: Option<usize>` field; existing consumers tolerate the addition (serde with `#[serde(skip_serializing_if = "Option::is_none")]`).
- Dump rotation continues to key on `sanitized_task_id`; per-slot dumps for the same task ID across waves correctly rotate together.

**Validation:**
- New test: `test_slot_prompt_too_long_isolates_to_single_slot` (slot 2 hits PromptTooLong; slots 0/1/3 unaffected)
- Existing overflow tests pass unchanged
- Audit checklist filled out in this PRD's Open Questions and confirmed before Phase D begins

### FR-005: Per-task crash tracking on `IterationContext`

Replace the brittle `last_task_id == current_task_id && last_was_crash` predicate (engine.rs:4207-4216) with a per-task map so crash escalation fires correctly across paths.

**Details:**
- Add `crashed_last_iteration: HashMap<String, bool>` (or `HashSet<String>` of crashed-IDs since the bool is always true) to `IterationContext` (engine.rs:~224). Single field; main-thread-only writes; no Arc/Mutex needed.
- The pipeline writes one entry per processed task: `true` on `IterationOutcome::Crash(_)`, `false` (or removal) on success/empty/reorder.
- `check_crash_escalation` in engine.rs:4207-4216 is rewritten to: `if !ctx.crashed_last_iteration.get(current_task_id).copied().unwrap_or(false) { return None; }`. The `last_task_id` parameter is no longer needed.
- Remove `last_task_id` and `last_was_crash` from `IterationContext` once `check_crash_escalation` no longer reads them. Audit other readers via `git grep`; remove or update.
- The map is unbounded in principle but bounded in practice by active task count per PRD. If memory becomes a concern, prune entries on terminal task statuses (`done`, `blocked`, etc.); deferred until measured.

**Validation:**
- New test: `test_per_task_crash_tracking_fires_on_repick_after_wave_crash`
- New test: `test_per_task_crash_tracking_clears_on_success`
- New test: `test_check_crash_escalation_uses_map_not_legacy_fields`
- Existing crash-escalation tests are updated to populate the map instead of `last_task_id`/`last_was_crash`

---

## 5. Non-Goals (Out of Scope)

The following are explicitly **NOT** part of this work:

- **Mid-wave `<reorder>` preemption** — Reason: parallel slots are already in flight; preempting peers is fundamentally incompatible with the wave model. Reorder hints continue to queue via `ctx.pending_reorder_hints` for next-wave consumption (selection-side wiring is a separate task per engine.rs:228-231).
- **Wrapper commit on slot tasks** — Reason: slot commits live on ephemeral branches until merge-back. The wave-level `merge_slot_branches_with_resolver` + post-wave external-git reconciliation already handle this correctly (engine.rs:1393-1441). Slot path's intentional skip of git-commit completion detection (engine.rs:1172-1174) stays.
- **Synergy cluster-wide model/effort escalation in wave mode** — Reason: wave selection deliberately picks disjoint tasks (engine.rs:516-518); cluster-wide escalation does not apply. Per-task model/effort is the correct policy and is preserved.
- **Human review triggering in wave mode** — Reason: deferred per existing design (engine.rs:1335-1337). Human reviews fire on the next sequential iteration boundary.
- **Pre-iteration usage check, pause-signal handling, rate-limit handling lifted into the pipeline** — Reason: these fire at the wave-loop boundary, not per-slot. Lifting into the per-slot pipeline would cause N redundant API/DB checks per wave.
- **Per-task crash tracking** — Reason: "last slot processed wins" semantics for `ctx.last_was_crash` are a behavior change but acceptable. Per-task tracking is a follow-up if false escalations are observed.
- **Caching learnings-retrieval queries across slots within a wave** — Reason: serial main-thread cost is within budget (≤1.6s for 8 slots). Optimization deferred until measured as hot.
- **Removing the `prompt::build_prompt` re-export** — Reason: that's a drive-by API rename. The shim stays.

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/engine.rs` — `run_iteration` (sequential entry; wraps pipeline), `run_slot_iteration` (consumes bundle, no inline prompt build), `process_slot_result` (calls pipeline), `build_slot_prompt` (DELETED), `SlotContext` (carries bundle), `SlotIterationParams` (drops `base_prompt_path`), `IterationContext` (no field changes; new write sites in wave mode for `last_task_id`/`last_was_crash`)
- `src/loop_engine/prompt.rs` — split into `prompt/{core,sequential,slot,mod}.rs`
- `src/loop_engine/prompt_sections/learnings.rs` — `build_learnings_section` factored into `prompt::core::build_learnings_block`
- `src/loop_engine/context.rs` — `scan_source_context` factored into `prompt::core::build_source_context_block`
- `src/loop_engine/overflow.rs` — `handle_prompt_too_long` audited; `OverflowEvent` gains optional `slot_index`
- `src/learnings/ingestion/mod.rs` — `extract_learnings_from_output` consumed by both paths via the new pipeline (no signature change, no rewrite)
- `src/loop_engine/feedback.rs` — `record_iteration_feedback` consumed by both paths (no signature change)
- New: `src/loop_engine/iteration_pipeline.rs`

### Dependencies

- **Internal**: existing helpers all stay in place — `mark_task_done`, `update_prd_task_passes`, `complete_cmd::complete`, `apply_status_updates`, `parse_completed_tasks`, `scan_output_for_completed_tasks`, `key_decisions_db::insert_key_decision`, `LearningWriter`, `progress::log_iteration`. The refactor moves call sites, not implementations.
- **External**: none. No new crates. Ollama optional (graceful degradation already exists).

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| -------- | ---- | ---- | -------------- |
| **Three-builder split + main-thread bundle (chosen, per user direction)** | Solves rusqlite `!Send` cleanly: all DB reads on main thread, slot worker just spawns Claude. Path-specific intent is explicit (sequential vs slot needs differ). Re-uses 100% of existing section helpers. Composes well with the existing pattern of "claim on main, work on worker, post-process on main" (learnings #1895, #1899). | Three modules to maintain instead of one. Builds N prompts serially on main thread before spawning N workers (~1.6s budget for 8 slots — within target). | **Preferred** |
| Single `build_prompt` with `PromptMode { Sequential, Slot }` flag | One module; toggling sections via flag is concise. | Forces every section function to be aware of mode, leaking path concerns into shared code. Sequential's cluster-aware logic and slot's disjoint-task assumptions don't naturally collapse to a flag. Leaks the wave's design assumption ("disjoint tasks") into all section helpers. | Rejected |
| Keep `build_slot_prompt` separate; lift section helpers only | Smallest-diff option. | Doesn't address the structural problem — two builders forever; future divergence is the default outcome. We'd be back here in 6 months. | Rejected |

**Selected Approach**: Three-builder split (`core` + `sequential` + `slot`) with main-thread `SlotPromptBundle` construction. Combined with shared `iteration_pipeline.rs` for post-Claude work. The bundle approach lets us reuse `prompt::core` helpers from both paths while keeping the path-specific composition (which sections to include, in what order, with what budget) explicit and isolated.

**Phase 2 Foundation Check**: Investing one focused refactor cycle (~1-2 days of pipeline-level work) prevents the recurring class of "X works in sequential mode but is silently broken in wave mode" bugs. Wave mode is already the default for multi-task PRDs and parallel slot count is climbing — every behavior we add to the loop will silently miss wave mode without this unification, accumulating ~1-3 days of drift per quarter (re-discovering and re-fixing each gap one at a time). 1:10 ratio comfortably met.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| ---- | ------ | ---------- | ---------- |
| **Phase A introduces silent regression in sequential prompt** | High (sequential is the heavily-used path; a regression here breaks all loops) | Medium | Phase A includes a snapshot test asserting byte-identical sequential prompt output for a fixture task. Fixture rules (per architect review C4): deterministic iteration number, fresh DB, zero synergy partners + zero dependencies in v1, controlled `project_root`. Pre-merge `cargo grep "HashMap<.*Section\\|prompt"` to surface section-ordering nondeterminism. Phase A is its own commit before Phase B begins. If snapshot diffs, do not proceed to Phase B. |
| **`SlotPromptBundle` becomes non-Send via a future field addition** | Medium (compile error blocks development; hard to debug if `Send` is lost transitively) | Medium | Add compile-time `static_assertions::assert_impl_all!(SlotPromptBundle: Send)` test. CI catches the regression at compile time, not runtime. |
| **Per-task crash tracking map grows unbounded** | Low (memory waste only; no correctness impact) | Low | Map is bounded in practice by active task count per PRD (typically <100). If observed as hot, prune entries on terminal task statuses. Not a release blocker. |
| **Per-slot overflow ladder has hidden shared mutable state** | High (incorrect recovery could leave slots in inconsistent states) | Low | **AUDIT COMPLETE** (architect review C3): all four mutation sites in `overflow::handle_prompt_too_long` (overflow.rs:329, 335, 341, 353, 354) take `&mut IterationContext` keyed on `task_id`. DB UPDATE is `... AND status='in_progress'` guarded. JSONL append uses `O_APPEND` with line-size <4KB (atomic). Dump rotation is per-`sanitized_task_id`. Same-wave-same-task collision in dump filenames is theoretical (wave selection picks disjoint tasks). **Confirmed safe under serial main-thread invocation**; no Phase D blocker. |
| **`IterationOutcome` mutation by pipeline surprises caller observers** | Medium (caller logic that branches on outcome before vs after pipeline call could misbehave) | Low | `params.outcome: &mut IterationOutcome` is documented as in-out parameter; pipeline mutates only via the completion-detection branch (matching today's `run_loop` behavior at engine.rs:3280, 3307, 3341, 3400, 3454). Existing callers already see post-mutation values today. Document the contract; cover with `test_process_iteration_output_mutates_outcome_on_retroactive_completion`. |

#### Top 3 Risks (ranked by Impact × Likelihood)

1. **Phase A silent regression in sequential prompt** (High × Medium) — biggest blast radius, primary user-facing path. **Mitigated by**: snapshot test with fixture rules (deterministic iteration, fresh DB, no synergy/deps in v1) required before Phase A merges.
2. **`IterationOutcome` mutation contract mishandled by callers** (Medium × Low) — emerged in architect review; explicit in-out parameter contract avoids surprises. **Mitigated by**: documented contract + targeted test.
3. **`SlotPromptBundle` Send regression via field addition** (Medium × Medium) — easy to introduce, hard to debug. **Mitigated by**: compile-time `assert_impl_all!` test.

### Security Considerations

- No new attack surface. Slot worker threads still receive only the prompt string + their own per-thread `Connection`; no shared mutable state crosses threads.
- Overflow JSONL writes remain best-effort and never propagate failures (matches existing pattern; failures cannot disrupt the loop).
- LearningWriter chokepoint pattern is preserved (Ollama embedding scheduled best-effort, graceful when down).

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --------------- | --------- | ----------------- | --------------- | ------------ |
| `prompt::slot::build_prompt` | `fn build_prompt(conn: &Connection, task: &Task, params: &SlotPromptParams<'_>) -> SlotPromptBundle` | `SlotPromptBundle { prompt, task_id, task_files, shown_learning_ids, resolved_model, effective_effort, task_difficulty, dropped_sections, section_sizes }` | Infallible (graceful degradation on missing optional sections) | Records `learning_shown_events` rows for ranked learnings included in the prompt |
| `prompt::core::build_learnings_block` | `fn build_learnings_block(conn: &Connection, task: &Task, budget: usize) -> (String, Vec<i64>)` | `(rendered_section, shown_learning_ids)` | Returns `("", vec![])` on retrieval errors | None (caller chooses whether to record `shown_learning_ids`) |
| `prompt::core::build_source_context_block` | `fn build_source_context_block(touches_files: &[String], project_root: &Path, budget: usize) -> String` | Rendered section string (may be empty) | Returns `""` on FS errors | None |
| `prompt::core::build_tool_awareness_block` | `fn build_tool_awareness_block(permission_mode: &PermissionMode) -> String` | Rendered section string | Infallible | None |
| `prompt::core::build_key_decisions_block` | `fn build_key_decisions_block(task: &Task) -> String` | Rendered section string | Infallible | None |
| `prompt::core::format_task_json` | `fn format_task_json(task: &Task, status_override: Option<&str>, include_files: bool, include_escalation_note: bool, escalation_note: Option<&str>) -> String` | JSON string | Infallible (uses fallback on serde failure, matching existing pattern) | None |
| `iteration_pipeline::process_iteration_output` | `pub(crate) fn process_iteration_output(conn: &mut Connection, ctx: &mut IterationContext, params: &mut IterationProcessingParams<'_>) -> ProcessingOutcome` | `ProcessingOutcome { tasks_completed, task_marked_done, files_aggregated, status_updates_applied, key_decisions_count, completed_task_ids }` | Infallible at the boundary (internal errors logged via `eprintln!`); `params.outcome` may be mutated to `IterationOutcome::Completed` retroactively | Many: status updates, completion detection (incl. `is_task_reported_already_complete` fallback), learnings extraction, feedback recording, per-task crash-tracking map updates, key-decision inserts, progress logging |
| `SlotPromptBundle` (struct) | See FR-002 | N/A | N/A | Bundle is `Send`; consumers must not introduce non-`Send` fields |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --------------- | ----------------- | ------------------ | --------- | --------- |
| `prompt::build_prompt` | `fn build_prompt(params: &BuildPromptParams) -> Result<Option<PromptResult>, ...>` | Unchanged (re-exported from `prompt::sequential::build_prompt`) | No | N/A — re-export shim preserves call sites |
| `SlotContext` (struct) | `{ slot_index, working_root, task: Task, last_activity_epoch }` | `{ slot_index, working_root, prompt_bundle: SlotPromptBundle, last_activity_epoch }` | Yes (internal `pub(crate)`) | All construction sites move into `run_parallel_wave` (one site); update `build_slot_contexts` to call `prompt::slot::build_prompt` |
| `SlotIterationParams` | Has `base_prompt_path: &Path` field | Drops `base_prompt_path` (baked into bundle) | Yes (internal `pub(crate)`) | Remove field initialization in `build_shared_slot_params` |
| `OverflowEvent` (serde) | No `slot_index` | Adds `slot_index: Option<usize>` with `#[serde(skip_serializing_if = "Option::is_none")]` | No (additive serde change; existing consumers tolerate missing field) | None |
| `IterationResult` (struct) | No `conversation` field; sequential reads `claude_result.conversation` inline | Adds `conversation: Option<String>` so wave's `SlotResult.iteration_result` carries the structured conversation through to the pipeline's learnings-extraction step | Yes (internal `pub(crate)`) | Update ~19 `IterationResult` construction sites in engine.rs (per architect grep). Most are early-exit paths that pass `None`; only the post-Claude success path passes `Some(...)`. |
| `IterationContext` (struct) | Has `last_task_id: Option<String>`, `last_was_crash: bool` | Removes the two legacy fields; adds `crashed_last_iteration: HashMap<String, bool>` (or `HashSet<String>`) | Yes (internal `pub(crate)`) | Update `check_crash_escalation` (engine.rs:4207-4216), `IterationContext::new` initializer, and any test that touched the legacy fields. Audit via `git grep last_task_id\\|last_was_crash` before removal. |
| `check_crash_escalation` | Predicate over `last_task_id`, `last_was_crash`, `current_task_id` | Predicate over `ctx.crashed_last_iteration[current_task_id]` | Yes (internal) | Single function rewrite; callers pass `ctx` instead of three separate fields |

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --------- | ----------------------- | ----------------------------- |
| Main thread builds bundle → ships to worker → worker emits output → main thread post-processes | `SlotPromptBundle` (Rust struct, typed fields, all `Send`) → `SlotContext.prompt_bundle` (typed field) → worker reads `slot.prompt_bundle.prompt: String` → worker returns `SlotResult { iteration_result, claim_succeeded, slot_index }` → main thread reads `slot_result.iteration_result.output: String` and `slot_result.shown_learning_ids: Vec<i64>` (added field) | ```rust // Main thread, before spawn:\nlet bundle = prompt::slot::build_prompt(&conn, &task, &params);\nlet ctx = SlotContext { slot_index, working_root, prompt_bundle: bundle, last_activity_epoch };\n// Worker thread:\nlet prompt_str = &slot.prompt_bundle.prompt;\nlet shown_ids = slot.prompt_bundle.shown_learning_ids.clone(); // move into result\n// Main thread, post-join:\nlet shown_ids = &slot_result.shown_learning_ids; // pipeline reads via params``` |
| `IterationContext` field reads/writes across paths | `IterationContext.last_task_id: Option<String>`, `last_was_crash: bool`, `model_overrides: HashMap<String, String>`, `overflow_recovered: HashSet<String>`, `pending_slot_tasks: Vec<String>`, `pending_reorder_hints: Vec<String>` — all `pub(crate)` typed fields, written only on the main thread | ```rust // Sequential post-iteration:\nctx.last_task_id = Some(task_id.clone());\nctx.last_was_crash = matches!(outcome, IterationOutcome::Crash(_));\n// Wave (after all slots join, sequentially):\nfor slot_result in &wave_result.outcomes {\n    process_slot_result(slot_result, &mut params, ctx, &mut agg);\n    // pipeline updates ctx.last_task_id / last_was_crash inside\n}``` |
| `extract_learnings_from_output` invocation | `claude_conversation: Option<&str>` (preferred, structured) → fallback to `claude_output: &str` → `extract_learnings_from_output(conn, source, Some(&task_id), Some(run_id), Some(db_dir), Some(signal_flag))` | ```rust let learning_source = claude_conversation.as_deref().unwrap_or(claude_output);\nif !learnings::ingestion::is_extraction_disabled() && !learning_source.is_empty() {\n    match learnings::ingestion::extract_learnings_from_output(\n        conn, learning_source, Some(task_id), Some(run_id),\n        Some(db_dir), Some(signal_flag),\n    ) {\n        Ok(r) if r.learnings_extracted > 0 => eprintln!(...),\n        Ok(_) => {},\n        Err(e) => eprintln!("Warning: learning extraction failed: {}", e),\n    }\n}``` |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --------- | ----- | ------ | ---------- |
| `src/loop_engine/engine.rs:357` (`build_slot_prompt`) | Inline slot prompt builder | DELETED — replaced by `prompt::slot::build_prompt` | All construction redirected to new builder via `run_parallel_wave` |
| `src/loop_engine/engine.rs:489` (`run_slot_iteration`) | Builds prompt inline; constructs Claude spawn args | OK — reads `slot.prompt_bundle.prompt` instead | Refactor in Phase B; covered by existing test suite |
| `src/loop_engine/engine.rs:1053` (`process_slot_result`) | Inline post-Claude work (key decisions, status, completion, file aggregation) | OK — body shrinks to a `process_iteration_output` call + slot-specific glue | Refactor in Phase C; new tests for learnings/feedback parity |
| `src/loop_engine/engine.rs:1436+` (`run_iteration`) | Inline post-Claude work (everything in steps 7.7-9) | OK — body shrinks to a `process_iteration_output` call wrapped by sequential-only outer concerns | Refactor in Phase C; snapshot tests guard regression |
| `src/loop_engine/engine.rs:730` (`run_parallel_wave`) | Spawns slot worker threads from `SlotContext` | OK — adds main-thread `prompt::slot::build_prompt` call before spawn | Refactor in Phase B |
| `src/main.rs:1095` (`task-mgr extract-learnings` CLI) | Direct call to `extract_learnings_from_output` | NO CHANGE — pipeline doesn't affect CLI | None |
| Tests in `tests/` and `engine.rs` test module asserting `build_slot_prompt` content | Direct assertions on slot prompt sections | BREAKS — slot prompt now contains learnings/source-context/tool-awareness sections | Update assertions in Phase B; new tests assert the new sections present |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --------- | ------- | ---------------- | --------------------- |
| `process_slot_result` completion detection | Wave mode, ephemeral branch, slot's commit not yet merged | Skips git-commit detection (engine.rs:1172-1174 comment); uses `<completed>` tag → output scan fallback only | Same behavior, but driven by `skip_git_completion_detection: true` flag passed into `iteration_pipeline::process_iteration_output` (no semantic change, just plumbing) |
| `run_iteration` completion detection | Sequential, on the loop branch directly | Full ladder: `<task-status>` tags → `<completed>` tags → git-commit detection → output scan fallback → wrapper-commit if no hash | Same behavior, but `iteration_pipeline` handles tag/completion dispatch; `run_iteration` keeps the wrapper-commit + git-hash detection as outer concerns (NOT moved into pipeline) |
| `extract_learnings_from_output` source preference | Sequential | Prefers `claude_conversation` (structured) over `claude_output` (raw) | Same — both paths now use this preference via shared pipeline |
| `IterationContext.overflow_recovered` set membership | Sequential | Tracks first-overflow tasks for banner annotation | Same — wave mode now also writes to this set when a slot hits PromptTooLong |

### Inversion Checklist

- [x] All callers identified and checked? Yes — `run_iteration`, `run_slot_iteration`, `process_slot_result`, `run_parallel_wave`, plus tests; documented in Consumers table
- [x] Routing/branching decisions that depend on output reviewed? Yes — `IterationOutcome` matching (Crash variants, Empty, Reorder), `slot_marked_done` detection, `WaveAggregator` accumulation
- [x] Tests that validate current behavior identified? Yes — `test_run_slot_iteration_*`, `test_run_wave_iteration_*`, `test_build_slot_prompt_*`, `test_run_parallel_wave_*` (~30+ tests); Phase A snapshot test added
- [x] Different semantic contexts for same code discovered and documented? Yes — git-completion detection differs by ephemeral-branch context (Semantic Distinctions table)

### Documentation

| Doc | Action | Description |
| --- | ------ | ----------- |
| `CLAUDE.md` (project) | Update | Add a "Iteration pipeline (shared)" section pointing to `src/loop_engine/iteration_pipeline.rs`. Update the "Slot merge-back conflict resolution" section to cross-reference. Note the "last slot wins" semantics for crash escalation. |
| `src/loop_engine/prompt/mod.rs` (rustdoc) | Create | Module-level doc explaining the three-builder split: when to use `core` vs `sequential` vs `slot`, and the rule "DB reads happen on the main thread; slot workers consume bundles." |
| `src/loop_engine/iteration_pipeline.rs` (rustdoc) | Create | Module-level doc listing the steps in `process_iteration_output` with engine.rs line refs for the original sequential and wave call sites (so future readers can audit equivalence). |
| Learning #2029 | Supersede | The "wave mode bypasses sequential PromptTooLong recovery (intentional)" learning is no longer accurate after Phase D. Use `task-mgr edit-learning 2029 --supersedes` or `task-mgr learn --supersedes 2029` with the new corrected learning. |

---

## 7. Open Questions

- [x] **Overflow handler audit for per-task isolation** — RESOLVED in architect review C3 (see Risks table). Pipeline-side overflow dispatch is safe; the PRD's Phase D blocker is removed.
- [x] **Crash escalation: per-task tracking now or defer?** — RESOLVED: fold per-task `HashMap<String, bool>` into this PRD (US-004 + FR-005). Eliminates false-negative class.
- [x] **Wave mode "already complete" fallback** — RESOLVED: unified into the pipeline (US-005, FR-003 step 4).
- [x] **Pipeline call site (sequential)** — RESOLVED: `run_loop` (architect's pick), not `run_iteration`. `run_iteration` shrinks to "select + spawn + return raw IterationResult".
- [ ] **Snapshot fixture choice for Phase A regression test**: Build a v1 fixture with a single task, no synergy partners, no dependencies, empty `touchesFiles`, empty `steering_path`, empty `session_guidance`, and one controlled `learnings` row to exercise the trimmable-section path. Add a v2 fixture with dependencies + source context once v1 passes.
- [ ] **Performance budget validation**: Once Phase B lands, run a wave with `parallel_slots=8` on a real PRD and measure pre-spawn main-thread latency. If it exceeds 1.6s, add a follow-up task to cache the learnings retrieval per wave.
- [ ] **Test placement**: Pipeline unit tests go in `iteration_pipeline.rs`'s `#[cfg(test)] mod tests` block. End-to-end wave-and-sequential parity tests go in `tests/iteration_pipeline_parity.rs` (new file). Phase A snapshot test goes in `tests/prompt_sequential_snapshot.rs`.
- [ ] **`IterationResult.conversation` field addition fallout**: ~19 construction sites in engine.rs need updating per architect grep. Most are early-exit paths that pass `None`; only the post-Claude success path passes `Some(...)`. Mechanical; verify no out-of-tree consumer (e.g., bin targets) constructs `IterationResult` literals.

---

## Appendix

### Related Documents

- `/home/chris/.claude/plans/what-else-is-being-expressive-puzzle.md` — approved plan; authoritative architectural reference
- `tasks/prd-parallel-task-execution.md` — original FEAT-010 PRD that introduced the wave model
- `tasks/prd-overflow-recovery-and-diagnostics.md` — PromptTooLong four-rung ladder (sequential)
- `CLAUDE.md` § "Slot merge-back conflict resolution" — companion infrastructure
- `CLAUDE.md` § "Learning Creation Chokepoint" — `LearningWriter` contract preserved

### Related Learnings (institutional memory)

- **#1893 / #1852 / #1871** — `rusqlite::Connection` is `!Send`; per-thread connections are mandatory. Drives the main-thread-bundle architectural choice.
- **#1895** — Task claiming happens on the main thread before spawning slot workers. This refactor extends the same pattern to prompt construction.
- **#1899** — Wave post-processing already happens on the main thread after worker join. Confirms `iteration_pipeline` as a natural fit on the existing seam.
- **#1864** — `SlotContext` is `Send` but not `Sync`. `SlotPromptBundle` follows the same pattern.
- **#2009** — Enum-based slot failure discrimination refactor was successful and tests-clean. Provides a template for any new outcome enums introduced here.
- **#2005** — Replace string-sniffing with structured discriminators. Apply when introducing the `skip_git_completion_detection` flag and the `IterationProcessingParams` struct.
- **#2029** — "Wave mode bypasses sequential PromptTooLong recovery (intentional)" — to be SUPERSEDED by this PRD's Phase D.
- **#2031** — Four-rung overflow recovery contract — preserve invariants in Phase D.
- **#1609** — Survey all `spawn_claude` call sites before flipping a new last arg. Apply when any spawn signature changes (none planned, but watch for it).
- **#1875** — Parallel execution review checklist for AC. Apply at code-review time.

### Glossary

- **Slot**: A worker thread executing one task within a parallel wave. Slot 0 lives in the main worktree; slots 1+ get ephemeral worktrees.
- **Wave**: One iteration of the parallel-execution loop. A wave selects N disjoint tasks, spawns N slot workers, joins them, merges their ephemeral branches, and post-processes results.
- **Bundle**: `SlotPromptBundle` — `Send`-safe carrier of the assembled prompt + side-channel data, produced on the main thread and consumed by a slot worker.
- **Pipeline**: `iteration_pipeline::process_iteration_output` — the shared post-Claude processing routine called by both sequential and wave paths.
- **Bedrock helpers**: section builders in `prompt::core` that both `sequential` and `slot` builders compose from.
- **Disjoint tasks**: tasks scored as non-overlapping (different `touchesFiles`, no shared synergy partners) — wave selection's invariant.
