# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Claim-scoped short status-id resolve** for **task-mgr**.

## Problem Statement

Agents often emit bare story ids in `<task-status>` / `<completed>` tags (`REFACTOR-001:done`) while the loop claimed the PRD-prefixed DB id (`97be64d7-REFACTOR-001`). Exact-id dispatch fails → claim stays `in_progress` → stale recovery → infinite re-pick until max iterations. Fix by rewriting **only** tags that refer to **this iteration’s claimed task** to the full claimed id before dispatch, in the shared `process_iteration_output` pipeline (seq + wave).

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Global short-id resolution against the whole tasks table (ambiguous across PRDs)
- Unrestricted ends_with / substring match that lets `001` match `…-REFACTOR-001`
- Changing TaskLifecycle exact-id lookup as the fix (lifecycle stays exact-id SSoT)
- Rewriting only inside `extract_status_updates` (no claim context; keep parse pure)
- Rewriting peer bare ids to the claimed task when they are not the claim's bare form
- Altering the dispatch-failed warning byte format for true misses (`lifecycle_stderr_contract`)
- Using a single global `status_updates_applied>0` gate instead of per-claimed `(id, Done, true)` — Learning 2238
- Tests that only assert no crash without verifying DB status and outcome flip
- Manual edits to `tasks/*.json` for status (use task-mgr CLI / task-status tags)
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings`
- Rust: Scoped tests for touched modules pass
- Rust: `cargo fmt --check` passes
- No unwrap() in production code paths
- No breaking changes to existing APIs unless explicitly required
- Sequential and wave both benefit via `process_iteration_output` only (no path-specific fork)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything global is already embedded in **this prompt file**.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/claim-short-status-id.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need | Command |
| ---- | ------- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` using the task ID from `## Current Task` |
| List remaining tasks (debug only) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up task | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` — **always the full id from the iteration banner** (this feature makes bare claim-matching ids work, but full id remains the contract) |

### Files you DO touch

| File | Purpose |
| ---- | ------- |
| `tasks/claim-short-status-id-prompt.md` | This prompt file (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — **tail** for recent context, **append** after each task |

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

---

## Your Task (every iteration)

1. **Work the task in `## Current Task`** — already selected and claimed. If no eligible task, `<promise>BLOCKED</promise>`.
2. **Pull only the progress context you need** — recent section or specific dependsOn block.
3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Never Read `learnings.md` / `long-term-learnings.md` whole. Never Read full `CLAUDE.md` — grep if needed.
4. **Verify branch** — matches `feat/claim-short-status-id`.
5. **Think before coding** — assumptions, edgeCases/failureModes, Data Flow Contracts below.
6. **Implement** — code and tests in one coherent change.
7. **Run the scoped quality gate** — fix before commit.
8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).
9. **Emit status**: `<task-status><FULL-TASK-ID>:done</task-status>` using the exact id from the banner / `## Current Task`.
10. **Append progress** — tight block terminated with `---`.

---

## Task Selection (reference)

The loop engine owns selection and claim. Emit `<reorder>TASK-ID</reorder>` only to request a different pick next iteration. Never combine reorder with `next --claim`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

When a task declares `modifiesBehavior: true` (FEAT-002):

1. Read callers named in the task (process_iteration_output call sites: sequential + wave).
2. Both must keep working via the shared pipeline rewrite — do not special-case paths.
3. Document that bare claim-matching tags now complete; non-matching bare tags still TaskNotFound.

---

## Quality Checks

### Per-iteration scoped gate (FEAT / FIX / REFACTOR-FIX)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
# FEAT-001
cargo test -p task_mgr tag_id_refers -- --nocapture
cargo test -p task_mgr resolve_tag -- --nocapture
# or module-scoped:
cargo test -p task_mgr output_parsing -- --nocapture
# FEAT-002
cargo test --test iteration_pipeline -- --nocapture
```

Do **not** run the full unscoped workspace suite on normal FEATs.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

If ANY test fails — including pre-existing — REVIEW-001 fixes them (or spawns FIX-xxx when >~12 clearly unrelated).

---

## Common Wiring Failures (REVIEW-001 reference)

- Helper written but never called from Step 3 → rewrite never runs
- Rewrite after `apply_status_updates` → DB still misses
- Only rewrites `:done` → failed/blocked short tags still stick
- Step 4b `<completed>` left bare → second failure class remains
- Peer bare id rewritten to claim → false complete (Learning 2188 / 2238)

---

## Review Tasks

| Review | Priority | Spawns | Focus |
| ------ | -------- | ------ | ----- |
| REFACTOR-001 | 98 | `REFACTOR-FIX-xxx` (50-97) | DRY, complexity, pure vs pipeline split |
| REVIEW-001 | 99 | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) | Wiring, 001-negative test, full suite, no lifecycle drift |

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 60,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

---

## Progress Report Format

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

---

## Learnings Guidelines

Use `task-mgr recall --for-task <TASK-ID>` / `--query` / `--tag`. Record with `task-mgr learn`. Do not append learnings files by hand.

---

## Stop and Blocked Conditions

### Stop Condition

Before `<promise>COMPLETE</promise>`: all tasks `passes: true`, no new review-spawned work pending, REVIEW-001 full suite green.

### Blocked Condition

Document blocker, spawn clarify via `task-mgr add --stdin`, output `<promise>BLOCKED</promise>`.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this task list. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless recall is sparse (then use `task-mgr recall --query`, not a full Read).

- **[2238]** Status-tag completion gate must check the *claimed* task's per-entry `(id, Done, applied)` — never global `status_updates_applied > 0`.
- **[2284]** / **[2635]** / **[3334]** `apply_status_updates` returns per-(task_id, status) success tuples; partial failure tolerance is a hard contract.
- **[2065]** / **[2086]** Shared `process_iteration_output` is the single post-Claude pipeline for sequential and wave — wire once there.
- **[2188]** Peer `<completed>`/`<task-status>` tags complete peers independently; do not map peer bare ids onto the claimed task.
- **[1571]** / **[1578]** / **[4054]** Side-band `<task-status>` is extract → apply through lifecycle; agents must emit tags for DB updates.
- **[3461]** LoopStatusTag `:done` from `todo` auto-claims then completes — rewrite to full id preserves this path.
- **[2270]** `complete()` is idempotent on already-done — dual short+full tags after rewrite are OK.
- **[4959]** Safe prefix matching needs dash/token boundaries — never bare substring.
- **[4965]** Production task ids use deterministic 8-hex PRD prefixes.
- **[193]** / **[4495]** Substring false positives (e.g. `-p` in `--print`) — require negative-control tests; no loose `ends_with`.
- **[2210]** / **[3142]** REFACTOR: surgical only; “no refactor needed” + empty commit is valid.

---

## CLAUDE.md Excerpts (only what applies to this change)

- Parallel-slot / wave and sequential both use shared iteration pipeline behaviors; keep parity by not forking paths.
- Status mutations for the loop go through lifecycle / command handlers; side-band tags are applied as metadata alongside iteration outcome detection.
- PRD task JSON is source of truth for the loop; agents mark status via `<task-status>` tags, never hand-edit JSON.
- Model IDs live in `src/loop_engine/model.rs` — do not hardcode models in this task list.
- `ui::*` for product UX / operator notes; `tracing` for internal diagnostics only.

---

## Data Flow Contracts

**Claimed task id (full DB id)**

```text
ProcessingParams.task_id: Option<&str>
  e.g. Some("97be64d7-REFACTOR-001")   // always full when present
ProcessingParams.task_prefix: Option<&str>
  e.g. Some("97be64d7")
```

**Status tag path (after this feature)**

```text
output: &str
  → detection::extract_status_updates(output)
       TaskStatusUpdate { task_id: "REFACTOR-001", status: Done }
  → resolve_tag_id_to_claimed("REFACTOR-001", claimed, task_prefix)
       → "97be64d7-REFACTOR-001"   // rewrite in-place on TaskStatusUpdate.task_id
  → apply_status_updates(... updates with FULL ids ...)
       → Vec<(String /*full id*/, TaskStatusChange, bool applied)>
  → Step 4a: id == claimed_id && Done && applied → record_completion
```

**Prefix strip (existing)**

```text
strip_task_prefix("97be64d7-REFACTOR-001", Some("97be64d7")) → "REFACTOR-001"
// 8-hex fallback when task_prefix is None: strip [0-9a-f]{8}- only (same rule as model::strip_prd_prefix)
```

**Do NOT**

```text
// Wrong: global DB lookup of bare id
// Wrong: claimed.ends_with(tag_id) alone
// Wrong: TaskLifecycle::apply learns about short ids
```

---

## Feature-Specific Checks

```bash
# Must exist and fail under loose ends_with-only implementations:
# tag "001" must NOT resolve to claim "97be64d7-REFACTOR-001"

# Happy path integration shape:
# insert in_progress "aaaaaaaa-REFACTOR-001"
# process_iteration_output task_id=Some(full), task_prefix=Some("aaaaaaaa"),
#   output="<task-status>REFACTOR-001:done</task-status>"
# assert DB status done + outcome Completed + status_updates_applied >= 1
```

---

## Reference Code

Existing helper to build on:

```rust
// src/loop_engine/output_parsing.rs
pub(crate) fn strip_task_prefix<'a>(task_id: &'a str, prefix: Option<&str>) -> &'a str

// Pipeline Step 3 today (insert rewrite between these two):
let status_updates = detection::extract_status_updates(output);
// NEW: rewrite short claim-matching ids here when task_id is Some
apply_status_updates(conn, &status_updates, /* … */, task_prefix, /* … */);
```
