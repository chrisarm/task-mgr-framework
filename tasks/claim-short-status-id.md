# Claim-scoped short `<task-status>` id resolve

**Type**: plan-tasks lean brief  
**Branch**: `feat/claim-short-status-id`  
**Task list**: `tasks/claim-short-status-id.json`  
**Prompt**: `tasks/claim-short-status-id-prompt.md`

## Problem

Agents commonly emit bare story ids in status tags:

```text
<task-status>REFACTOR-001:done</task-status>
```

while the loop claimed the PRD-prefixed DB id (`97be64d7-REFACTOR-001`). Lifecycle dispatch is exact-id only → **Task not found** → claim stays `in_progress` → stale recovery resets to `todo` → the same task is re-selected until max iterations. Work may be correct; the DB never completes.

## In scope

- Pure match/resolve helpers: tag id refers to **this iteration’s claimed task** (full id or bare form via `strip_task_prefix` + 8-hex fallback).
- Rewrite in `process_iteration_output` **before** `apply_status_updates` (all status keywords) and before Step 4b `<completed>` `mark_task_done` for claim-matching bare ids.
- Operator note on rewrite; unit + integration tests including **negative** cases (`001` must not match).
- Seq + wave both fixed via the single shared pipeline.

## Out of scope

- Global short-id lookup against the tasks table.
- Wave-wide peer claim list for peer bare tags.
- Hard-fail iteration when claim’s tag misses entirely (stricter than rewrite).
- Changing `TaskLifecycle` exact-id SSoT.
- Prompt/banner copy overhaul (nice follow-up, not this list).

## Success bar

- Short claim-matching `:done` completes the claimed task and flips pipeline outcome to Completed.
- Non-matching bare ids still fail dispatch (unchanged).
- No loose suffix matching; full suite green at REVIEW-001.

## Key files / subsystems

- `src/loop_engine/output_parsing.rs` — `strip_task_prefix`; new pure resolve helpers
- `src/loop_engine/iteration_pipeline.rs` — Step 3 status dispatch + Step 4 completion ladder
- `src/loop_engine/engine.rs` — `apply_status_updates` shim (do not widen with claim id)
- `tests/iteration_pipeline.rs` — pipeline contract tests

## Notes for review

- **Claim-scoped only** — ambiguity bound is “this pass’s claimed task,” not the PRD’s full task set.
- Preserve Learning **2238**: completion gate is per-entry `(claimed_id, Done, applied)`.
- Preserve lifecycle stderr contract for true dispatch misses.
- 8-hex fallback mirrors `model::strip_prd_prefix` rules without coupling to private model APIs.
