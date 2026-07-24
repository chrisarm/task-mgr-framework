# Claude Code Agent Instructions

You are an autonomous coding agent implementing **the Reactions Framework convergence** for **task-mgr**.

## Problem Statement

The loop engine has two execution paths — sequential (`iteration.rs::run_iteration` + `orchestrator.rs::run_loop`) and parallel/wave (`wave_scheduler.rs::run_wave_iteration` + `slot.rs`). Main-thread post-Claude *reactions* were implemented at one path's call site and silently omitted or shaped differently in the other, producing a recurring bug class. The latest: rate-limit/session-limit waiting exists only in sequential, so wave mode never waits, strands `in_progress` tasks, and false-aborts with "no eligible tasks after 3 consecutive stale iterations" — resetting in-flight work. This effort relocates all six non-path-specific reactions into a single `src/loop_engine/reactions/` module both paths route through, locked by `#[deprecated]` + `#![deny(deprecated)]` on the three engine files and exhaustive param-struct destructure on the coordinators.

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

**This effort's prime directive — the single-home contract:** every converged reaction lives in `reactions::` and is called by both paths. The wave path folds its N slot results; sequential folds its 1. Account-global reactions (rate-limit, usage gate) fire **exactly once per wave**. Never copy-paste a reaction into one path — the `#![deny(deprecated)]` lock on `iteration.rs`/`wave_scheduler.rs`/`slot.rs` will reject it at compile time, and that's intentional.

**Prohibited outcomes:**

- Copy-pasting a reaction into one path instead of calling the shared coordinator
- A rate-limit early return that zeroes `ctx.consecutive_merge_fail_waves` (wipes the cascade-halt defense)
- Resetting a task to todo that completed in the same wave (must filter on `status='in_progress'`)
- N sequential usage waits for N rate-limited slots — the wait must fire exactly once
- Tests that only assert 'no crash' or check a type without verifying content
- A coordinator param struct destructured with `..` (defeats the compile-time parity lock)
- Catch-all error handlers that swallow context; `unwrap()` on fallible paths in production

---

## Global Acceptance Criteria

These apply to **every** implementation task — task-level `acceptanceCriteria` from `task-mgr next` layer on top. If any fails, the task is not done.

- Rust: no warnings in `cargo check`
- Rust: no warnings in `cargo clippy -- -D warnings`
- Rust: scoped tests pass (`cargo test -p task-mgr <module>`); full suite green at milestones
- Rust: `cargo fmt --check` passes
- No breaking changes to public APIs unless the task explicitly relocates a leaf (then a transition `#[deprecated]` shim + `#[allow(deprecated)]` at legit sites)
- Each of the five `reactions::` coordinators destructures its param struct exhaustively (no `..`)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Everything per-task is returned by `task-mgr next`; everything PRD-wide that matters is embedded in **this prompt file** — the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

> **Heads-up on this repo:** the `tasks/` directory is also synced by Dropbox and has been observed reverting untracked files. If `tasks/reactions-framework-convergence.json` or this prompt disappears mid-run, that's the external sync — re-fetch from git or flag it, don't assume tooling corruption.

### Getting your PRD's task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/reactions-framework-convergence.json)
[ "$PREFIX" = null ] || [ -z "$PREFIX" ] && { echo "taskPrefix not set — run \`task-mgr loop init tasks/reactions-framework-convergence.json\` first"; exit 1; }
```

Use `$PREFIX` in every CLI call below. Substitute `$PREFIX` anywhere a note says `{{TASK_PREFIX}}`. (`task-mgr loop init` writes the prefix into the JSON before the loop runs; the guard above catches the case where it wasn't.)

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task    | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task                       | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings for a task            | `task-mgr recall --for-task $PREFIX-TASK-ID`                                                                                                                                      |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by MILESTONE-N`                                                                                                              |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`)                                                          |

### Files you DO touch

| File                                                | Purpose                                                                |
| --------------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/reactions-framework-convergence-prompt.md`   | This prompt file (read-only)                                           |
| `tasks/progress-{{TASK_PREFIX}}.txt`                | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — never Read the whole log; tail the most recent section:

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt   # a specific prior task
```

Skip on the first iteration (file won't exist). Create with a one-line header if missing; never crash on absent files.

---

## Your Task (every iteration)

1. **Resolve prefix and claim**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/reactions-framework-convergence.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   Output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, `notes`. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — usually just the most recent section. Grep a specific `dependsOn` task's block if you need its rationale. Skip on iteration 1.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do NOT Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly. **Never Read `CLAUDE.md` in full** — the excerpts that matter are embedded below; `grep -n -A 10 '<header>' src/loop_engine/CLAUDE.md` for anything else.

4. **Verify branch** — `git branch --show-current` matches the printed `branchName` (`refactor/reactions-framework-convergence`). Switch if wrong.

5. **Think before coding** — state assumptions; for each `edgeCases`/`invariants`/`failureModes` entry note the handling; for cross-module data access consult the Data Flow Contracts section below; only survey alternatives when `estimatedEffort: "high"` or `modifiesBehavior: true`.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (below). Fix failures before committing.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`. Do NOT edit the JSON.

10. **Append progress** — ONE tight block (format below), terminated with `---`.

11. For TEST-xxx tasks: target 80%+ coverage on new methods; `assert_eq!` on string outputs.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` picks eligible tasks (`passes:false`, deps complete, not `requiresHuman`), preferring file-overlap with the previous task, then lowest priority. You claim what it returns.

- If the task has `preflightChecks`, run them; on failure `task-mgr skip <TASK-ID> --reason "..."` and re-run `next`.
- If the previous task had a `completionCheck`, run it before starting; on failure `task-mgr fail <prev> --error "..."` and fix first.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

Several FEATs here are `modifiesBehavior: true`. ANALYSIS-001 already produced the Consumer Impact Table (in the progress log) covering callers of every relocated leaf. Before implementing a relocation:
1. Confirm ANALYSIS-001 `passes: true`; read its Consumer Impact Table block from the progress log.
2. For `BREAKS` consumers (e.g. `slot.rs:492`) → route through the coordinator. For `NEEDS_REVIEW` → verify manually. For `OK` → proceed.
3. The task JSON also carries a per-task `consumerAnalysis` — honor its `semanticDistinctions` (sequential vs wave shapes).

---

## Quality Checks

### Per-iteration scoped gate (implementation / test / fix tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test -p task-mgr <module_or_fn_name>   # scope to touchesFiles; e.g. reactions, wave_scheduler, iteration
```

Scope from `touchesFiles`. Do **NOT** run the whole workspace suite during regular iterations — that's the milestone's job.

**Test output discipline (project rule):** always pipe through `tee` to a temp file and grep results in the SAME command — never stream full output, never run twice:

```bash
cargo test -p task-mgr reactions 2>&1 | tee /tmp/t.txt | tail -3 && grep -i "FAILED\|error\[" /tmp/t.txt | head
cargo clippy -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -3 && grep "^error" /tmp/clippy.txt | head
```

### Milestone gate (MILESTONE-1 / -2 / -FINAL)

Full unscoped suite on a clean checkout, must finish green:

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test 2>&1 | tee /tmp/full.txt | tail -5 && grep -i "FAILED\|error\[" /tmp/full.txt | head -20
```

Fix EVERY failure including pre-existing ones (trunk-green is the invariant). Escape hatch only if >~12 failures all clearly unrelated: fix what's attributable, spawn one `FIX-xxx --depended-on-by <THIS-MILESTONE>` for the rest, and `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

- A relocated leaf still called directly from one of the three engine files (the deny should have caught it — if `#[allow(deprecated)]` was used to silence it, that's a wiring miss)
- A coordinator param struct destructured with `..` (lock defeated)
- `usage_params` added to `WaveIterationParams` but not wired at `orchestrator.rs:991`
- Wave reaction inserted but B2 (budget give-back) or B3 (merge-fail streak) not handled at the boundary
- New `reactions::` fn defined but not called from BOTH paths

---

## Review Tasks

| Review                  | Priority | Spawns (priority)               | Before          | Focus                                                                        |
| ----------------------- | -------- | ------------------------------- | --------------- | ---------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16) | MILESTONE-1     | The lock holds; exhaustive destructure; B1/B2/B3; transactional promotion; no unwrap(); wiring reachable on all 3 files |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)          | MILESTONE-FINAL | reactions:: DRY/complexity/coupling/clarity — full-context final pass         |

Use the **rust-python-code-reviewer** agent. Spawn fixups:

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
}' | task-mgr add --stdin --depended-on-by MILESTONE-1
```

`--depended-on-by` wires + syncs the JSON atomically. Commit `chore: <REVIEW-ID> - Add fix tasks`, then `<task-status><REVIEW-ID>:done</task-status>`. If no issues, emit done with a one-line note.

---

## Progress Report Format

APPEND to `tasks/progress-{{TASK_PREFIX}}.txt` (create with a one-line header if missing). Keep it tight (~10 lines):

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

ANALYSIS-001 additionally writes its Consumer Impact Table into its block (file:line | leaf | OK/BREAKS/NEEDS_REVIEW | migration).

---

## Learnings Guidelines

`task-mgr recall --for-task <TASK-ID>` for indexed retrieval; `task-mgr learn` to record (don't append to the learnings files directly). Write concise 1-2 line learnings; group related tasks.

---

## Stop and Blocked Conditions

**Stop** — before `<promise>COMPLETE</promise>`: all stories `passes:true`, no new tasks created in final review, all milestones pass.

**Blocked** — document in progress log, create a `CLARIFY-001` (priority 0) via `task-mgr add`, commit, then `<promise>BLOCKED</promise>`.

---

## Milestones

Milestones are **full-gate checkpoints**, not sweep sessions. Each: confirm `dependsOn` all `passes:true` → run the full quality gate (the ONE place the whole suite runs) → leave the repo green (fix pre-existing failures; spawn `FIX-xxx --depended-on-by <THIS-MILESTONE>` for non-trivial ones) → mark done only when green. `MILESTONE-1`/`-2` use opus; `MILESTONE-FINAL` uses the review model (`grok-build`).

---

## Key Learnings (from task-mgr recall)

Authoritative — do NOT Read `tasks/long-term-learnings.md`/`learnings.md`. Use `task-mgr recall --query <text>` only if a task needs one not listed here.

- **[2286]** Wave-mode parity was achieved by consolidating post-Claude work into `iteration_pipeline.rs` (shared by both paths). This effort extends that precedent with `reactions::`.
- **[2300]** The `iteration_pipeline_parity` test suite catches sequential↔wave mismatches by running identical fixtures through both configs — model `tests/reaction_parity.rs` on it.
- **[2157]** Parity tests = run the same fixture output through both code paths (sequential: skip_git=false; wave: skip_git=true) and assert equivalent side effects.
- **[2224]** The FEAT-005 unification reduced path divergence; both `run_iteration` and the slot path now share the pipeline. reactions:: is the next layer.
- **[2111]** Wave mode was missing learning extraction + bandit feedback that sequential had — fixed via the shared pipeline. Same drift shape this PRD eliminates for the six reactions.
- **[2852]** Wave-mode PromptTooLong recovery EVOLVED from bypass → shared ladder. FEAT-005 must NOT regress to a per-path bypass; reuse the existing overflow.rs tests as the equivalence oracle.
- **[2136]** A prior drift: `SlotResult` initialized `conversation: None` even when the conversation existed — wave silently dropped data the pipeline needed. Watch for analogous "field not threaded to the coordinator" misses.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

From `src/loop_engine/CLAUDE.md` — do NOT Read the full file.

- **Iteration pipeline (shared):** `process_iteration_output` is the existing shared per-task post-Claude pipeline (key-decisions, `<task-status>`, completion ladder + already-complete fallback, learnings, bandit, crash tracking). reactions:: sits AROUND it (pre-spawn, account-global, post-completion, overflow), not inside it. Do NOT fold overflow into the pipeline (#5 is its own reaction).
- **Status mutations — use TaskLifecycle:** all `tasks.status` writes go through `TaskLifecycle` verbs — NO raw `UPDATE tasks SET status…` SQL. Rate-limit reset uses `recover_in_progress_for_prefix`; it only touches `in_progress` rows (idempotent) — this is what makes B1 (completion-durability) safe.
- **Transactional promotion ctx writes are deferred:** the RuntimeError/overflow fallback hook does DB writes inside a tx and returns a `PendingPromotion`; the caller applies ctx mutations (`runner_overrides`/`model_overrides`/`overflow_original_task_model`) ONLY after `tx.commit()?` returns Ok. FEAT-002 and FEAT-005 must preserve this split.
- **Operator escape valve (`check_override_invalidation`):** runs at the top of every iteration before runner resolution; if `tasks.model` was edited out-of-band it clears all six per-task recovery channels. Folded into `resolve_task_execution` (FEAT-002) — keep the before-resolution ordering.
- **Drained-queue classification (`classify_drained_queue`):** the shared loop-end verdict. The rate-limit fix must return BEFORE the empty-group/stale path so a rate-limit never drives the stale-abort.
- **Parallel-slot merge-fail halt:** `apply_merge_fail_reset_and_halt_check` (`wave_scheduler.rs:738-739`) resets `consecutive_merge_fail_waves` to 0 when `failed_merges` is empty — the cascade-halt defense from the mw-datalake incident. B3: a rate-limit early return must NOT pass through this with empty `failed_merges`.
- **!Send constraint:** slot workers never touch `&Connection`/`&IterationContext`; reactions run on the MAIN thread (after `run_parallel_wave` joins) — that's why they can be shared. Never move a reaction into a slot worker.

---

## Data Flow Contracts

Verified access pattern for the headline rate-limit path (#6). Use exactly — do NOT guess key types.

```rust
// Building the account-reaction input from wave slot results (MAIN thread,
// in run_wave_iteration AFTER the process_slot_result loop, ~wave_scheduler.rs:1024):
let items: Vec<reactions::account::OutputReactionItem> = wave_result
    .outcomes
    .iter()
    .filter(|sr| sr.claim_succeeded)                       // skip synthetic claim-fail entries
    .map(|sr| reactions::account::OutputReactionItem {
        task_id: sr.iteration_result.task_id.as_deref(),   // Option<&str>
        outcome: &sr.iteration_result.outcome,             // &IterationOutcome
        output:  &sr.iteration_result.output,              // &str
    })
    .collect();

// Sequential builds a 1-item slice from its single IterationResult:
let items = [reactions::account::OutputReactionItem {
    task_id: Some(&task_id),
    outcome: &outcome,
    output:  &claude_output,
}];

// Coordinator (account-global; folds N → one decision; waits at most once):
match reactions::account::react_to_outputs(conn, &items, &params) {
    AccountReaction::None          => { /* proceed */ }
    AccountReaction::WaitedAndRetry => { /* wave: early return; give back loop-bound
                                            iteration (B2); DO NOT zero merge-fail streak (B3);
                                            surface tasks_completed: agg.tasks_completed */ }
    AccountReaction::Stop          => { /* terminal exit 130; output:"" parity */ }
}
```

`UsageParams` lives at `engine.rs:92`; add `pub usage_params: &'a UsageParams` to `WaveIterationParams` (`engine.rs:548-595`) and wire `usage_params: &usage_params` at `orchestrator.rs:991` (the local is already in scope from the sequential branch).

---

## Feature-Specific Checks

- **Verify the lock once per relevant task**: after relocating a leaf, confirm a direct call from `iteration.rs`/`wave_scheduler.rs`/`slot.rs` would fail `cargo build` (the `#![deny(deprecated)]` + `#[deprecated]` pair). CODE-REVIEW-1 and VERIFY-001 re-check this.
- **Exhaustive destructure**: grep each coordinator for its param-struct destructure and confirm no `..`.
- **One home, two callers**: each of the five coordinators must be defined once under `reactions::` and called from BOTH paths.

---

## Important Rules

- Work on **ONE task per iteration**
- For `estimatedEffort: "high"` tasks (CONTRACT-001, FEAT-002/005/006/010): consider `/ralph-loop` to iterate within the task until all acceptance criteria pass.
- **Commit frequently**; keep CI green; **read before writing**; **minimal changes**; check existing patterns in `src/loop_engine/CLAUDE.md` (via grep, not full Read).
