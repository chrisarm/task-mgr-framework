# Changelog — 2026-05-07

## Unify sequential and parallel-slot execution paths

**Branch**: `feat/unify-execution-paths`
**PRD**: `tasks/prd-unify-sequential-and-wave-execution.md`

### What shipped

The autonomous loop's two execution paths (sequential `run_iteration` and parallel-wave `process_slot_result`) now share a single post-Claude pipeline (`src/loop_engine/iteration_pipeline.rs::process_iteration_output`) and a three-builder prompt module (`src/loop_engine/prompt/{core,sequential,slot,mod}.rs`). Wave mode gains parity for: learnings extraction, bandit feedback, key-decision capture, the "task already complete" fallback, and per-slot `PromptTooLong` recovery via the same four-rung ladder sequential uses. Crash escalation moves from a brittle `last_task_id == current_task_id && last_was_crash` predicate to a per-task `crashed_last_iteration: HashMap<String, bool>`, fixing a class of false-negatives where a wave-mode crash on task X failed to escalate when X was re-picked.

### Why it matters

Wave mode is the default for multi-task PRDs. Before this change, every behavior added to the sequential post-Claude path silently missed wave mode — the loop got progressively dumber the more parallelism it used. Concrete fix: bandit feedback rows for slot tasks (previously zero) now update; `<learning>` tags emitted by slots persist to the `learnings` table with embeddings scheduled; `<completed>` tags retroactively flip `Empty` → `Completed` in both modes; and a `PromptTooLong` on slot 2 isolates to slot 2's task ID without corrupting peers' recovery state.

### Breaking changes

None observable. Internal `pub(crate)` shape changes:
- `IterationContext` removes `last_task_id` / `last_was_crash`, adds `crashed_last_iteration: HashMap<String, bool>`.
- `IterationResult` adds `conversation: Option<String>` (preferred source for learnings extraction over raw stdout).
- `SlotContext` carries `prompt_bundle: SlotPromptBundle` instead of `task: Task`.
- `OverflowEvent` gains optional `slot_index` (`#[serde(skip_serializing_if = "Option::is_none")]` keeps sequential JSONL byte-identical).
- `prompt::build_prompt` re-exported from `prompt/mod.rs` so external callers compile unchanged.

### Follow-ups

Three reviewer-flagged mediums become CODE-FIX/WIRE-FIX work in the next loop pass: (M1) skip the pipeline when `slot.claim_succeeded == false`; (M2) replace the `status_updates_applied > 0` heuristic with explicit per-(task_id,status) success tracking; (M3) decide whether to thread `steering.md` and `session_guidance` into slot prompts or document the intentional drop.

---
