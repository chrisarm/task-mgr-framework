# Changelog — 2026-05-08

## Unify Execution Followups

**Branch**: `feat/unify-execution-followups`
**PRD**: `tasks/unify-execution-followups.json`

### What shipped

Nine review-loop followups against `feat/unify-execution-paths` plus 2 spawned refactor fixes and one cosmetic cleanup:
- **M1**: `process_slot_result` early-returns when `slot.claim_succeeded == false`, preventing pollution of `ctx.crashed_last_iteration` with tasks that never executed.
- **M2**: `apply_status_updates` returns `Vec<(String, TaskStatusChange, bool)>` per-update; the pipeline's status-tag completion gate now requires the **claimed task's specific** `(id, Done, true)` tuple instead of the global "any update succeeded" flag.
- **M3**: slot prompts now thread `steering_path` and `session_guidance` for sequential parity (`SlotPromptParams<'a>` gained both fields; `core::build_session_guidance_block` is the shared helper).
- **L6**: `crashed_last_iteration` map now pruned on terminal-status DB writes inside `apply_status_updates` and `mark_task_done` — restores the documented "bounded by active task count" invariant.
- **L7**: slot prompt builder gained `TOTAL_PROMPT_BUDGET = 80_000` (matches sequential), `try_fit_section`, and `dropped_sections` accounting; `shown_learning_ids` cleared when learnings drop (no false bandit feedback).
- **L1+L8**: `crash_tracker.record_success()` hoisted out of `record_completion` (called once per pass); dead `_completion_epoch_start` read deleted.
- **L3+L4**: per-slot overflow branch hardened with `debug_assert!(prompt_for_overflow.is_some())` + `bundle.difficulty` threaded through `SlotResult` into `synthetic_prompt.task_difficulty`.
- **REFACTOR-FIX-001**: `prompt::sequential` adopted the new `core::build_session_guidance_block` for parity with slot.
- **REFACTOR-FIX-002**: replaced 80KB clone with `.take()` on `prompt_for_overflow` in the per-slot overflow branch.
- **CLEANUP-001**: removed two misfiled WIRE-FIX-001 / CODE-FIX-002 placeholder entries from `loop-reliability.json` (Learning #2236 fallout).

### Why it matters

The unify-execution-paths PRD shipped with three known mediums and six known easy-win lows; this branch closes all of them and restores invariants documented in CODE-FIX-003. Wave-mode operators now get steering and session-guidance content in slot prompts (parity with sequential), the bandit no longer credits learnings the agent never saw (when sections drop), and the per-task crash map stays bounded by active-task count again.

### Breaking changes

- `apply_status_updates` return type changed from `u32` to `Vec<(String, TaskStatusChange, bool)>` and gained a `mut ctx: Option<&mut IterationContext>` parameter for the prune call. One production caller (`iteration_pipeline.rs:254`); ~10 test callers updated.
- `SlotPromptParams` gained lifetime parameter `'a` (was `'static`-only) plus `steering_path: Option<&'a Path>` and `session_guidance: &'a str` fields.
- `OverflowEvent` gained `task_difficulty: Option<String>` with `#[serde(skip_serializing_if = "Option::is_none")]` (additive; existing JSONL consumers tolerate).

### Follow-ups

- **M-1** (review finding, MEDIUM, operationally benign): `iteration_pipeline.rs::process_iteration_output` Step 7 re-inserts pruned `crashed_last_iteration` entries for non-Done terminal claims (`<task-status>X:failed</task-status>`). Restore the invariant by unioning `completed_task_ids` with status_results entries whose status is in `{Done, Failed, Skipped, Irrelevant}`. Captured as Learning #2304.
- **L-1** (review finding, LOW): wave with all-failed-claim slots registers `all_crashed=true` and triggers backoff. Pre-existing; consider treating `claim_succeeded=false` as `all_crashed=false`.
- L-3 / L-8 (test gap nits) and L-2 / L-4–L-7 (stylistic) — minor.

---
