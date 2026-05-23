# Claude Code Agent Instructions

You are an autonomous coding agent implementing **TaskLifecycle Extraction (Phase 1, PRD 1 of 2)** for **task-mgr**.

## Problem Statement

`tasks.status` is currently mutated by ~20 raw `UPDATE tasks SET status …` SQL sites scattered across 13+ files (verified by grep audit during PRD drafting; supersedes the design-doc estimate of ~15). `TaskStatus::can_transition_to` at `src/models/task.rs:78` is documented as the single source of truth but is consulted by only 2 of those sites (`commands/complete.rs:199`, `commands/fail/transition.rs:123`). The "SSoT" is aspirational, not enforced.

This PRD extracts a `TaskLifecycle` service at `src/lifecycle/` that owns all status mutations plus their side effects (run_tasks bookkeeping, PRD JSON sync, decay columns, notes formatting, exact stderr warning shape). Six public verbs cover all five audit categories: `apply` (Category A user-intent + LoopStatusTag), `try_claim` (Category B race-safe pre-claim), three recovery verbs (Category C bulk recovery), `reconcile_from_prd` + `repair_stale` (Category D PRD-driven + heuristic — kept distinct per architect review).

Strict ordering: **CLARIFY-001** (runner-trait-hygiene PRD ordering gate) before anything spawns; **vertical-slice migration** (FEAT-007: skip.rs only) before **CLARIFY-002 (mini-dogfood gate)** before bulk migration. The codebase is live-dogfooded daily — corruption tolerance is zero.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases — this refactor touches live-dogfooded code) → **PHASE 2 FOUNDATION** (the lifecycle seam is the prerequisite for PRD 2 / engine carve; over-investing in correctness here is correct) → **FUNCTIONING CODE** (pragmatic, reliable, all five contract-level invariants preserved) → **CORRECTNESS** (compiles, type-checks, scoped tests pass; shadow tests assert byte-identical DB diff) → **CODE QUALITY** (clean, no warnings, no `.unwrap()` in production) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production); per-task partial-failure tolerance in `apply()` is a HARD contract — never convert to batch-level `Result<(), Err>`.

**Prohibited outcomes:**

- Converting `apply()` to return a single `Result<(), Err>` at the batch level (per-task outcomes are a contract; learning #2284)
- Hiding the conditional-WHERE expected-status set behind an unconditional `try_claim` method (must remain explicit per FR-005)
- Adding a NEW transaction wrapper around code that previously ran outside one (changes observable failure semantics)
- Adding SELECT-before-UPDATE round-trips where today's SQL writes via a conditional WHERE
- Reformatting the `PRD JSON sync failed for {task}: {err}` stderr line (operators grep for this exact prefix)
- Folding `doctor/fixes.rs` into `reconcile_from_prd` (must remain a distinct `repair_stale` verb per §6 doctor sub-decision)
- Editing `tasks/tasklifecycle-extraction.json` directly — use task-mgr CLI
- Touching the five parallel-slot cascade defenses (synthetic shared-infra slot, buildy-prefix heuristic, ephemeral overlay, consecutive-merge-fail halt, stale-ephemeral hygiene)
- Removing the `LIFECYCLE-EXCEPTION` comment from `commands/init/mod.rs:517` (lint-enforced)
- Bulk migration starting before CLARIFY-002 (mini-dogfood gate) passes
- Renaming `TaskStatus` enum variants or changing on-disk task status strings (DB migration impact; orthogonal — PRD §5 Non-Goal)
- Changing the `<task-status>` side-band tag wire format or recognized status set (tag contract is stable across both this PRD and the runner-trait-hygiene PRD — PRD §5 Non-Goal)

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output for affected crate
- Rust: No warnings in `cargo clippy -- -D warnings` for affected crate
- Rust: Scoped `cargo test -p task-mgr <module>` passes for the touched module
- Rust: `cargo fmt --check` passes
- No raw `UPDATE tasks SET status` SQL added to production code (only one exception in `commands/init/mod.rs` is permitted, marked `LIFECYCLE-EXCEPTION`)
- No new `.unwrap()` in production paths; use `?` propagation
- All status-write call sites that previously called raw SQL now call a `TaskLifecycle` verb (or are explicitly out-of-scope per `LIFECYCLE-EXCEPTION` lint)
- The five contract-level invariants (auto-claim, per-task partial-failure tolerance, DB-authoritative-PRD-best-effort, exact stderr warning shape, conditional-WHERE in API) are preserved bit-identically

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/tasklifecycle-extraction.json)
```

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task    | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by MILESTONE-N` — priority auto-computed; DB + PRD JSON updated atomically                                                   |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/tasklifecycle-extraction-prompt.md`  | This prompt file (read-only)                                        |
| `tasks/progress-<prefix>.txt`        | Progress log — **tail** for recent context, **append** after each task     |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip on the first iteration (file won't exist).

---

## Your Task (every iteration)

1. **Resolve prefix and claim**: `PREFIX=$(jq -r '.taskPrefix' tasks/tasklifecycle-extraction.json) && task-mgr next --prefix $PREFIX --claim`. If no eligible task or unmet `requires`, output `<promise>BLOCKED</promise>` and stop.

2. **Pull progress context** — most iterations want just the most recent section. If the claimed task has a relevant `dependsOn` task whose rationale matters, grep that specific block instead.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns learnings scored for THIS task. That is the only path to `tasks/long-term-learnings.md`. Never Read those files directly.

   **Never Read `CLAUDE.md` in full.** Authoritative per-task rules are already embedded in this prompt (below). If a task references a section not shown here, `grep -n -A 10 '<keyword>' CLAUDE.md`.

4. **Verify branch** — `git branch --show-current` matches `refactor/tasklifecycle-extraction`. Switch if wrong.

5. **Think before coding**: state assumptions; for each `edgeCases`/`invariants`/`failureModes` entry, note how it'll be handled; consult Data Flow Contracts (below) for cross-module access; pick an approach (one alternative if `estimatedEffort: high` OR `modifiesBehavior: true`).

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests, NOT the full suite).

8. **Commit**: `refactor: <TASK-ID>-completed - [Title]`.

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`.

10. **Append progress** — ONE post-implementation block, format below, terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

Most FEAT tasks in this PRD have `modifiesBehavior: true` (they replace existing SQL with service calls). ANALYSIS-001 is the umbrella analysis task that runs first and produces the Consumer Impact Table.

1. **ANALYSIS-001 gate**: must have `passes: true` before any `modifiesBehavior: true` FEAT spawns.
2. **Consumer Impact Table** (in `tasks/progress-<prefix>.txt` from ANALYSIS-001): reference it during implementation.
3. **Semantic distinctions** (e.g. CLI direct call vs. loop iteration vs. reconcile/repair): each context uses different `TaskLifecycle` configuration (no run_id for CLI; with_run + with_prd_sync for loop; reconcile_from_prd / repair_stale for Category D).

---

## Quality Checks

### Per-iteration scoped gate

```bash
# Rust — scope to the affected crate / module
cargo fmt --check 2>&1 | tee /tmp/fmt.txt | tail -3
cargo check 2>&1 | tee /tmp/check.txt | tail -3 && grep "^error" /tmp/check.txt | head -10
cargo clippy -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -3 && grep "^error" /tmp/clippy.txt | head -10
cargo test -p task-mgr <module_or_fn> 2>&1 | tee /tmp/test.txt | tail -5 && grep "FAILED\|error\[" /tmp/test.txt | head -10
```

Scoping heuristic: derive from `touchesFiles`. For changes in `src/lifecycle/`, run `cargo test lifecycle`. For changes in `src/commands/skip.rs`, run `cargo test commands::skip`. For `src/loop_engine/engine.rs`, run `cargo test loop_engine::engine`. **Do NOT** run the entire workspace test suite during regular iterations — that's the milestone's job.

### Milestone gate (MILESTONE-1, MILESTONE-2, MILESTONE-FINAL)

Full unscoped suite on a clean checkout:

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

If ANY test fails — including pre-existing failures predating this PRD — the milestone fixes them inline (≤12 unrelated) or spawns FIX-xxx tasks (>12 unrelated). Trunk-green is the invariant.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

- `apply_status_updates` shim signature drift: callers in `iteration_pipeline.rs:275`, sequential `~L3204`, wave `~L1166` expect `Vec<(task_id, change, applied: bool)>`. The shim MUST preserve this shape.
- Missing `#[deprecated]` on retained shims (`apply_status_updates`, `auto_block_task`, `claim_slot_task`).
- `.unwrap()` on `prepare`/`execute` results — propagate with `?`.
- New `TaskLifecycle` method has no production caller (dead code). Verify with `cargo check -- -W dead_code`.
- Plan-building (ReconcilePlan/RepairPlan) leaked into `src/lifecycle/` — must stay in `prd_reconcile.rs` / `doctor/fixes.rs`.
- Two `LIFECYCLE-EXCEPTION` tokens in production code (lint fails).
- Stderr warning string drifted from `PRD JSON sync failed for {task}: {err}\n` — TEST-INIT-003 snapshot catches this.

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REFACTOR-REVIEW-FINAL`) spawn follow-up tasks for each issue found via:

```bash
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

Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

Use the **rust-python-code-reviewer** agent.

---

## Progress Report Format

APPEND to `tasks/progress-<prefix>.txt` (create with a one-line header if missing):

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block. If your entry is longer than ~25 lines, compress it.

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:
1. Verify all stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

### Blocked Condition

If blocked (CLARIFY task waiting on human, missing dependency, unclear requirement):
1. Document blocker in the progress file
2. If new clarification needed: `task-mgr add --stdin --depended-on-by <THIS>` a CLARIFY task with `requiresHuman: true`
3. Output `<promise>BLOCKED</promise>`

---

## Milestones

Milestones (MILESTONE-1, MILESTONE-2, MILESTONE-FINAL) are **full-gate checkpoints**. Per PRD §8: MILESTONE-1 after bulk migration; MILESTONE-2 after comprehensive shadow tests + performance; MILESTONE-FINAL after VERIFY-001 dogfood gate.

### Milestone Protocol

1. Check all `dependsOn` have `passes: true`.
2. Run the full quality gate (above).
3. Leave the repo green. Pre-existing failures: fix inline (trivial) or spawn `FIX-xxx --depended-on-by <THIS-MILESTONE>` (non-trivial).
4. Mark `<task-status>MILESTONE-N:done</task-status>` only when green.

---

## Key Learnings (from task-mgr recall)

Pre-distilled learnings for this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here.

- **[#2284]** `apply_status_updates` returns `Vec<(task_id, change, applied: bool)>` per-task tuples, NOT a global boolean. Per-task partial-failure tolerance is a HARD contract — never convert to batch-level `Result<(), Err>`.
- **[#2238]** Status-tag completion gate at `iteration_pipeline.rs:275-286` checks the *claimed* task's dispatch outcome, not a global `status_updates_applied > 0` count. Service must preserve per-task outcome reporting.
- **[#2304]** `iteration_pipeline::process_iteration_output` Step 7 has subtle crash-tracking semantics for terminal claims — read this code before changing per-task outcome shape.
- **[#2796]** When a task reaches terminal status (Done, Failed, Skipped, Irrelevant), callers prune their tracking maps based on the outcome. Service preserves the per-task outcome that supports this.
- **[#2070]** Key functions used by `iteration_pipeline` exported from sibling modules: `detection::extract_*`, `feedback::*`, `output_parsing::*`. Lifecycle follows the same module-export pattern.
- **[#2065 / #2086 / #2286]** Wave-mode parity success: consolidated `iteration_pipeline` for sequential and slot paths. This is the design template for the lifecycle service — same shape but for the status-write axis.
- **[#2807]** Baseline `cargo test` count before refactoring; use `cargo test 2>&1 | grep 'test result'`. Captured in TEST-INIT-005.
- **[#2740]** Test modules within a file using `#[cfg(test)]` lose implicit access when functions are extracted. Use `pub(crate)` for shared internals or move tests with the function.
- **[#440]** Re-export pattern avoids caller import changes during extraction — `pub use src::loop_engine::engine::apply_status_updates` as a deprecated shim while migration is in progress.
- **[#2806]** Consolidate test helpers in `test_utils` to prevent duplication across the shadow test file and existing tests.
- **[#2747]** Large linear orchestrator functions are sometimes justified in single files (e.g., `run_archive` at 170 lines, +133/-109 net change). Do NOT over-split the lifecycle module just to reduce file size — clarity over count.
- **[#1581]** Adding a new parameter to a widespread function like `spawn_claude` requires updating ~9+ call sites. The lifecycle service's signature stability matters from FEAT-001 onward; do not churn it.
- **[#1448]** Stub migration pattern with `#[ignore]` tests enables TDD database changes (no schema changes in this PRD, but the pattern of stub-then-fill is the same shape as the service skeleton in FEAT-001).
- **[#1271]** Engine integration pattern: import, startup log, loop body hook. Lifecycle has no startup log; the import + hook are FEAT-010's job.
- **[#487]** Skip single-function modules — when extracting, don't split into one-file-per-function if they're tightly coupled. The lifecycle module has 7 files; verify each carries enough logic to justify.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` for the subsystems this PRD touches.

**Autonomous Loop Mode Override** (section 0 of user-global CLAUDE.md):
- Do NOT ask clarifying questions — output `<promise>BLOCKED</promise>` with a description instead
- Skip waypoints and planning phases — implement directly per this prompt file
- Do NOT invoke sub-agents for plan review — this prompt IS the plan
- Do NOT propose 2-3 approaches — pick best approach, state assumptions briefly, implement
- Use `task-mgr` CLI for task operations
- Mark status from a loop iteration via `<task-status>TASK-ID:done</task-status>` tag

**task-mgr Workflow Patterns** (project CLAUDE.md):
- NEVER edit `tasks/*.json` directly — the loop engine re-imports the file on each iteration
- Syncing JSON changes mid-effort: `task-mgr loop init <prd>.json --append --update-existing` (preserves status/started_at/completed_at)
- Spawn-fixup wiring: pass `--depended-on-by <milestone-of-this-PRD>` to route the new task to the correct PRD
- For human-in-the-loop CLARIFY tasks: embed `humanReviewOutcome` block in JSON; update DOWNSTREAM task entries in the SAME commit so the loop reads confirmed values

**Tool Usage Protocol**:
- Test output: ALWAYS pipe through `tee` to a temp file and `grep` for results in the same command — never stream full output
- Read files first; confirm paths & line numbers
- Never use `--no-verify` or skip hooks unless explicitly requested

**LLM Coding Guidelines** (project CLAUDE.md):
- Think before coding; state assumptions; identify inputs, invariants, failure modes
- Simplicity first — three similar lines beats premature abstraction
- Surgical changes — a bug fix does not need surrounding cleanup
- Goal-driven execution — every change must trace back to an acceptance criterion

**Subsystem design notes** the loop will need:
- `src/loop_engine/CLAUDE.md` — overflow recovery, auto-review, parallel slots, merge-back conflict resolution, shared iteration pipeline. The five parallel-slot cascade defenses live here and MUST be preserved.
- `src/commands/next/CLAUDE.md` — soft-dep guard for milestone scheduling.

**Permissions** (auto-mode memory):
- task-mgr loop/batch uses `permissionMode: auto` via `.task-mgr/config.json` — always use `dangerouslyDisableSandbox: true` for Bash commands.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures introduced by this PRD. Use these exactly.

### TransitionIntent → TaskLifecycle → DB UPDATE

```rust
// In commands/skip.rs (FEAT-007 example):
use crate::lifecycle::{TaskLifecycle, TransitionIntent, TransitionSource};

let intent = TransitionIntent {
    task_id: task_id.to_string(),
    target: TaskStatus::Skipped,
    notes: reason.clone(),  // Option<String>
    error: None,
    source: TransitionSource::Operator,
};

let outcomes = TaskLifecycle::new(&conn).apply(&[intent])?;
let outcome = outcomes.into_iter().next().unwrap();  // length always matches input

if !outcome.applied {
    // outcome.reason is Some(TransitionRejectReason::{InvalidTransition|UnknownTaskId|SourceMismatch})
    match outcome.reason {
        Some(TransitionRejectReason::InvalidTransition { from, to }) => {
            eprintln!("Cannot skip task in {} state", from);
            std::process::exit(1);
        }
        _ => { /* handle other rejection reasons */ }
    }
}
```

Key types at each level:
- `TransitionIntent`: owned Rust struct, typed `TaskStatus` enum target, `Option<String>` notes/error, `TransitionSource` enum source
- DB UPDATE: parameterized SQL via `conn.execute("UPDATE tasks SET status = ?, … WHERE id = ?", params![intent.target.as_str(), &intent.task_id])` — typed enum serialized to its string form
- No JSON string-key boundary — all internal data flow uses typed Rust enums and owned `String` task IDs

### Loop path: <task-status> tag → engine shim → service

```rust
// In src/loop_engine/engine.rs (FEAT-010 shim):
pub fn apply_status_updates(
    conn: &Connection,
    run_id: &str,
    iter: u32,
    extracted: &[StatusUpdate],
    prd_path: Option<&Path>,
    task_prefix: &str,
) -> Vec<(String, StatusChange, bool)> {
    let mut lifecycle = TaskLifecycle::with_run(conn, run_id, iter);
    if let Some(path) = prd_path {
        lifecycle = lifecycle.with_prd_sync(path, task_prefix);
    }

    let intents: Vec<TransitionIntent> = extracted.iter().map(|u| TransitionIntent {
        task_id: u.task_id.clone(),
        target: u.change.target_status(),
        notes: u.notes.clone(),
        error: u.error.clone(),
        source: TransitionSource::LoopStatusTag,
    }).collect();

    let outcomes = lifecycle.apply(&intents).unwrap_or_default();  // anyhow::Result -> graceful degradation on infra failure

    outcomes.into_iter().zip(extracted.iter()).map(|(outcome, update)| {
        (outcome.task_id, update.change.clone(), outcome.applied)
    }).collect()
}
```

Key invariants verified by shadow tests:
- Output `Vec` length matches input length (`outcomes.len() == intents.len()`)
- Per-task `applied` boolean is preserved through the shim
- Auto-claim path is INSIDE `lifecycle.apply()` — the shim does not handle it

### ReconcilePlan (caller-built) → reconcile_from_prd → DB

```rust
// In src/loop_engine/prd_reconcile.rs (FEAT-011):
let plan = ReconcilePlan {
    items: prd_tasks.iter().filter(|t| t.passes).map(|t| ReconcileItem {
        task_id: t.id.clone(),
        target: TaskStatus::Done,
        audit_label: Some("prd_marked_done".to_string()),
    }).collect(),
};

let report = TaskLifecycle::with_run(&conn, run_id, iter).reconcile_from_prd(plan)?;
// report.applied / report.skipped / report.rejected: Vec<TaskId>
```

**Critical**: plan-building stays in `prd_reconcile.rs`; the service consumes plans, NEVER builds them. The same shape applies to `RepairPlan` in `commands/doctor/fixes.rs`.

---

## Sequencing Reminders (per PRD §8)

1. **CLARIFY-001 must clear first** — runner-trait-hygiene ordering decision recorded. Without this, the loop may collide with `tasks/prd-runner-trait-hygiene.md` on `engine.rs` spawn sites.
2. **FEAT-007 → CLARIFY-002 → bulk migration** — the vertical-slice migration of skip.rs is gated by M=3 dogfood iterations BEFORE the remaining 21 sites migrate. Bulk migration tasks (FEAT-008+) all depend on CLARIFY-002.
3. **MILESTONE-1 vs MILESTONE-2 vs MILESTONE-FINAL** — three distinct gates: bulk migration complete, comprehensive shadow tests + performance green, full N=10 dogfood gate passed.
4. **VERIFY-001 is `requiresHuman: true`** — the maintainer drives the 10-iteration soak. Loop must emit `<promise>BLOCKED</promise>` until the humanReviewOutcome is recorded.

---

## Important Rules

- Work on **ONE story per iteration**
- For high-effort tasks (FEAT-001, FEAT-003, FEAT-008, FEAT-010): consider `/ralph-loop` to iterate within the task
- **Commit frequently** after each passing story
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first; PRD §2 negation rule #5
- **Minimal changes** — only what's required for the current task (PRD §LLM Coding Guidelines)
- **Reference the design doc** — `docs/designs/coherence-refactoring.md` for the big picture; the PRD for the contract; this prompt for the immediate task
