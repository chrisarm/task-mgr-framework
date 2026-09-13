# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Keep honest terminal closes from being reset to `todo`** for **task-mgr**.

## Problem Statement

A slot agent honestly classified a VERIFY gate:

```
<task-status>805856aa-VERIFY-PACER-OBS:blocked</task-status>
<promise>COMPLETE</promise>
```

The shared pipeline applied that tag (`:blocked` → `Failed` → `FailStatus::Blocked`), so the DB row was `blocked`. Loop-exit cleanup then printed `Reset uncompleted slot task … to todo` (orchestrator 17.6). The honest block was undone; the next wave re-claimed the same gate as `todo`.

Root cause: `reset_task_to_todo` calls unguarded `TaskLifecycle::resurrect_for_iteration`. Trackers (`last_claimed_task`, `pending_slot_tasks`) clear only on `:done`, so honest terminals, overflow-rung-5 blocks, and `auto_block_after_failures` are force-written back to `todo`.

Selected approach (architect-reviewed): two Category C Recovery verbs — `recover_in_progress` (`in_progress → todo`) for 17.5/17.6 and overflow rungs 1–3; `reopen_after_merge_fail` (`in_progress|done → todo`) for FEAT-002. Leave `resurrect_for_iteration` unguarded. Skip `handle_task_failure` **inside** the function when `read_status` is terminal.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Adding `WHERE status = 'in_progress'` to `resurrect_for_iteration` (learning #4358; `recovery_tests` pin `blocked → todo`)
- `SELECT status` + `TaskStatus::from_str` + maybe-write in `wave_scheduler` or `orchestrator` (wrong module, TOCTOU)
- One shared `in_progress`-only guard on `reset_task_to_todo` used by both 17.5/17.6 and merge-fail (strands premature `:done` on the ephemeral)
- `ProcessingOutcome.closed_task_ids` or widening `completed_task_ids` to all terminals
- Treating `<promise>BLOCKED</promise>` without a status tag as a DB close
- Counting a terminal close as `tasks_completed` / wrapper-commit / `IterationOutcome::Completed`
- Dual-site `handle_task_failure` skip at orchestrator ~597 and wave_scheduler ~1216
- Skipping `handle_task_failure` when status is `todo` (overflow rungs 1–3 still need the counter)
- Adding `TaskStatusChange::Blocked` or flipping pipeline outcome to `IterationOutcome::Blocked` on `:blocked`
- Fixing the adjacent wave `TransientBackend` exclusion-list hole in this PRD
- Changing decay / doctor / CLI `reset` / `unblock` / `unskip` / reconcile `force=true`
- Reordering `reconcile_ambiguous_exit` / `prd_complete` to after 17.5 (learning #5151)
- Unifying `last_claimed_task` + `pending_slot_tasks`, or drain-on-terminal as the load-bearing fix
- New raw `UPDATE tasks SET status` outside `src/lifecycle/`
- Tests that only assert no crash without checking DB status
- Manual edits to `tasks/*.json` for status (use task-mgr CLI / task-status tags)
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy --all-targets -- -D warnings`
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() in production code paths
- Every `tasks.status` write goes through a `TaskLifecycle` verb
- Sequential and wave stay parity-locked for the new skip and the two reset predicates

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation is already embedded in **this prompt file**. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/honest-terminal-closes.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need | Command |
| ---- | ------- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` using the task ID from `## Current Task` |
| List remaining tasks (debug only) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings relevant to a task | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up task (review spawns) | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| ---- | ------- |
| `tasks/honest-terminal-closes-prompt.md` | This prompt file (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — never Read the whole log:

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist).

---

## Your Task (every iteration)

1. **Work the task in `## Current Task`** — the loop engine already selected and claimed it. If none eligible or unmet `requires`, output `<promise>BLOCKED</promise>` and stop.
2. **Pull only the progress context you need** — most recent section, or grep a `dependsOn` task. For CONTRACT dependents, grep `## CONTRACT-001`.
3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Never Read `tasks/long-term-learnings.md` or full `CLAUDE.md`; grep a section if needed.
4. **Verify branch** — `git branch --show-current` is `feat/honest-terminal-closes`.
5. **Think before coding** — state assumptions; handle every `edgeCases` / `invariants` / `failureModes` entry; consult **Data Flow Contracts** below for status writes.
6. **Implement** — single task, code and tests in one coherent change.
7. **Run the scoped quality gate** (below). Fix failures before committing.
8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `fix:` / `test:` / `docs:`).
9. **Emit** `<task-status><TASK-ID>:done</task-status>`.
10. **Append progress** — one block, terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

This PRD already contains the consumer table. Do **not** spawn an ANALYSIS task.

1. Read the `consumerAnalysis` on the current task (injected in `## Current Task`).
2. `BREAKS` → the named mitigation is the implementation (FIX-001 retargets 17.5/17.6; do not also retarget merge-fail).
3. `NEEDS_REVIEW` → verify the semantic distinction before writing (especially merge-fail × `:done` vs orphan × `:done`).
4. `OK` → proceed.

Semantic split that must not be collapsed: **orphan reclaim** (`in_progress` only) vs **merge-fail reopen** (`in_progress` and `done`).

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **REVIEW-001** runs the full gate and must leave the repo green (including pre-existing failures).

### Per-iteration scoped gate

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
# Scope from touchesFiles:
cargo test --lib lifecycle::tests::recovery_tests
cargo test --lib loop_engine::wave_scheduler
cargo test --lib loop_engine::recovery
cargo test --test retry_tracking
cargo test --test overflow_recovery
```

**Do NOT** run the entire unscoped workspace suite during regular iterations — that is REVIEW-001's job.

### Full gate at REVIEW-001

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

If ANY test fails — including pre-existing — REVIEW-001 fixes them. Below ~12 failures, just fix them. Above that and clearly unrelated: spawn one `FIX-xxx` via `task-mgr add --stdin --depended-on-by REVIEW-001` and `<promise>BLOCKED</promise>`.

---

## Contract Tasks

`CONTRACT-001` **lands the two verb implementations + `recovery_tests`** in this story. No loop-engine call-site swap — FIX-001/002/003 do that. FIX-003 depends only on CONTRACT-001 and will not compile if the methods are missing.

- Implement `recover_in_progress` and `reopen_after_merge_fail` (atomic WHERE, clear `started_at`, set `updated_at`).
- Record the full contract under `## CONTRACT-001` in the progress log.
- Downstream FIX tasks swap call sites against that text.
- Do not change `resurrect_for_iteration` SQL.

---

## Review Tasks

| Review | Priority | Spawns | Focus |
| ------ | -------- | ------ | ----- |
| CODE-REVIEW-1 | 13 | `CODE-FIX` / `WIRE-FIX` | Verb wiring, two predicates, skip-inside-function, parity auditor |
| REVIEW-001 | 99 | `FIX-xxx` | Full unscoped suite; all ACs; `resurrect` still flips `blocked → todo` |

Spawn:

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-1: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 14,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

After CODE-REVIEW-1, run the `loop-engine-parity-auditor` on `orchestrator.rs`, `wave_scheduler.rs`, `slot.rs`, `recovery.rs`.

---

## Progress Report Format

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines. Progress files are gitignored — do not stage them.

---

## Stop and Blocked Conditions

Before `<promise>COMPLETE</promise>`: all stories `passes: true`, no new unfixed review tasks, REVIEW-001 green.

If blocked: document in progress, spawn `CLARIFY-001` via `task-mgr add` if needed, emit `<promise>BLOCKED</promise>`.

---

## Key Learnings (from task-mgr recall)

- **[4358]** `resurrect_for_iteration` intentionally omits the `in_progress` guard — do not "fix" it to paper over this bug.
- **[4810]** Recovery resets gate via `WHERE status = …`, not SELECT-then-write.
- **[5151]** `reconcile_ambiguous_exit()` MUST run before step 17.5 `reset_task_to_todo`.
- **[4357]** Lifecycle Recovery verbs stay separate from plan-driven (reconcile/repair/decay) verbs.
- **[3093]** Reset to `todo` clears `started_at` in the same UPDATE.
- **[2304]** Crash-map prune treats Failed/Skipped/Irrelevant as terminal — that is not reset authority.
- **[3727]** Do not increment `consecutive_failures` for non-task-logic outcomes; an already-terminal honest close is that class.
- **[3101]** Wave retry tracking is main-thread and must stay in parity with sequential — skip inside `handle_task_failure_with_runner`.
- **[2126]/[2988]** Overflow handler runs BEFORE the shared pipeline on both paths.
- **[3846]** Category C Recovery verbs live in `src/lifecycle/recovery.rs` with unit tests in `recovery_tests.rs`.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

### `src/lifecycle/CLAUDE.md`

- This module is the **single source of truth for all `tasks.status` mutations**. Every write goes through a `TaskLifecycle` verb (exception: `commands/init/mod.rs`, `LIFECYCLE-EXCEPTION`).
- Recovery verbs (`recover_in_progress_for_prefix`, `auto_block_after_failures`, `resurrect_for_iteration`) are **not** routed through the plan/matrix path. They carry `TransitionSource::Recovery`.
- `recover_in_progress_for_prefix` and `auto_block_after_failures` keep `WHERE status = 'in_progress'`.
- `resurrect_for_iteration` **deliberately omits** that guard so callers can force any listed ID to `todo`. Do not assume all Recovery verbs behave the same.

### `src/loop_engine/CLAUDE.md`

- All `tasks.status` writes inside `loop_engine/` go through `TaskLifecycle` verbs. Do **not** add raw `UPDATE tasks SET status` here.
- Overflow: both paths route through `handle_overflow` **before** `process_iteration_output`. Ladder: rungs 1–3 retry-in-place, rung 4 `resurrect_with_model_override`, rung 5 block.
- Shared pipeline: sequential and wave share `process_iteration_output`. `completed_task_ids` is **done-only**.
- Sequential and wave dual-path changes must stay in parity (run `loop-engine-parity-auditor` after editing `orchestrator.rs` / `wave_scheduler.rs` / `slot.rs` / `recovery.rs`).

---

## Data Flow Contracts

These are **verified access patterns**. Use them exactly.

### Orphan / merge-fail write (no pre-read)

```rust
// 17.5 / 17.6 / overflow rungs 1–3
TaskLifecycle::new(conn).recover_in_progress(task_id)?
// Ok(true) iff one row: status='todo', started_at=NULL, WHERE id=? AND status='in_progress'

// merge-fail only
TaskLifecycle::new(conn).reopen_after_merge_fail(task_id)?
// Ok(true) iff one row: same SET, WHERE id=? AND status IN ('in_progress','done')
```

Log `Reset {kind} {id} to todo` **only** on `Ok(true)`. Do **not** `SELECT status` then call unguarded `resurrect_for_iteration`.

### Failure-skip read (handle_task_failure only)

```rust
if crate::lifecycle::read_status(conn, task_id).is_some_and(|s| s.is_terminal()) {
    return Ok(());
}
```

`TaskStatus::is_terminal()` = `done | blocked | skipped | irrelevant`. Do **not** skip `todo` or `in_progress`.

### Trackers are not reset authority

- `IterationContext.pending_slot_tasks: Vec<String>` — all-run claimed ids; drained on `:done` / merge-fail retain.
- `last_claimed_task: Option<String>` — last sequential claim; cleared on `:done`.
- `ProcessingOutcome.completed_task_ids: Vec<String>` — **done-only**. Do not stuff terminals in.

### Status CHECK (there is no `'failed'` row)

```text
tasks.status IN ('todo','in_progress','done','blocked','skipped','irrelevant')
:failed / :fail / :blocked  →  tasks.status = 'blocked'
```

### Consecutive failures

```text
tasks.consecutive_failures: i32
increment_consecutive_failures has no status predicate
After FIX-004: increment is skipped when read_status is terminal; still runs on todo
```

---

## Feature-Specific Checks

- `src/lifecycle/tests/recovery_tests.rs::resurrect_for_iteration_flips_listed_ids_to_todo` must still assert `FEAT-2` (`blocked`) → `todo`.
- Overflow-rung-5-then-helper and auto-block-then-helper tests are the ones that fail if anyone "simplifies" to tag-history.
- Merge-fail × `done` → `todo` is the pin that fails if merge-fail is pointed at `recover_in_progress`.
- After FIX-004 / CODE-REVIEW-1: `loop-engine-parity-auditor` on `orchestrator.rs`, `wave_scheduler.rs`, `slot.rs`, `recovery.rs`.

---

## Key Context / Reference

| Path | Role |
| ---- | ---- |
| `src/lifecycle/recovery.rs` | Category C verbs; add `recover_in_progress` + `reopen_after_merge_fail` |
| `src/lifecycle/tests/recovery_tests.rs` | Verb unit tables; do not weaken `resurrect_for_iteration` tests |
| `src/lifecycle/mod.rs` | `read_status`; module rustdoc verb list |
| `src/loop_engine/wave_scheduler.rs` | `reset_task_to_todo`, `apply_merge_fail_reset_and_halt_check` |
| `src/loop_engine/orchestrator.rs` | Steps 17.5 / 17.6; sequential `handle_task_failure` call site |
| `src/loop_engine/recovery.rs` | `handle_task_failure_with_runner` |
| `src/loop_engine/reactions/post_output.rs` | Overflow ladder rungs 1–3 |
| `src/loop_engine/slot.rs` | `pending_slot_tasks` push / `slot_marked_done` drain |
| `src/models/task.rs` | `TaskStatus::is_terminal()` |
| PRD | `tasks/prd-honest-terminal-closes.md` |

### Predicate table (SSoT)

| Site | `in_progress` | `done` | `blocked` / `skipped` / `irrelevant` |
| --- | --- | --- | --- |
| 17.5 / 17.6 (orphan) | reset | keep | keep |
| Overflow rungs 1–3 | reset | keep | keep |
| Merge-fail | reset | **reset** | keep |
| `resurrect_for_iteration` | reset | reset | reset (frozen; do not call from new sites) |

### Out of scope (do not implement)

- `TaskStatusChange::Blocked`
- Changing `resurrect_for_iteration` SQL
- Decay / doctor / CLI `reset` / `unblock` / `unskip`
- Reconcile `force=true`
- `<promise>BLOCKED</promise>` as a DB close
- `prd_complete` / drain-classifier / exit-code redesign
- `closed_task_ids` / tracker unification
- Wave `TransientBackend` exclusion-list parity
- Re-blocking `805856aa-VERIFY-PACER-OBS` in restaurant_agent_ex

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- **Check existing patterns** — Recovery siblings already use conditional WHERE
