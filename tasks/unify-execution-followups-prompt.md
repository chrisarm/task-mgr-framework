# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Unify Execution Followups** for **task-mgr**.

## Problem Statement

The unify-execution-paths PRD (prefix `5d1118de`, branch `feat/unify-execution-paths`) shipped with three medium and six low review findings. This PRD addresses all of them plus a cosmetic cleanup of two misfiled spawn-fixup entries from that PRD's CODE-REVIEW-1.

The fixes target the same loop_engine subsystem and are mechanically small but semantically important — especially M1 (claim-failed slots polluting `crashed_last_iteration`), M2 (status-tag completion gate using a global "any update succeeded" flag instead of per-(task_id, status) success), and M3 (slot prompts silently dropping `steering.md` and `session_guidance` content vs the sequential builder).

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

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context
- Skipping the apply_status_updates per-update result (M2) — global counter is the bug being fixed

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: All scoped tests pass with `cargo test -p task-mgr <module>`
- Rust: `cargo fmt --check` passes
- No `.unwrap()` in production code paths (test code excepted)
- No literal Claude model strings outside `src/loop_engine/model.rs` (regression-tested by `tests/no_hardcoded_models.rs` — import `OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL` constants)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/unify-execution-followups.json)
```

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task                        | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID` (also `--query`, `--tag`)                                                                                                            |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/unify-execution-followups.json` — **--from-json is REQUIRED** (see Learning #2236 below)     |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

### Files you DO touch

| File                                                | Purpose                                                                |
| --------------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/unify-execution-followups-prompt.md`         | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`                        | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines. Two patterns cover every case:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read on the first iteration. Create with a one-line header if missing.

---

## Your Task (every iteration)

1. **Resolve prefix and claim**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/unify-execution-followups.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   Output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, `notes`. If no eligible task or unmet `requires`, output `<promise>BLOCKED</promise>` with the reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section. Skip on the first iteration.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. The `## Key Learnings` section below is the curated subset; consult recall only if a task references something not pre-distilled here.

   **Never Read `CLAUDE.md` in full.** The `## CLAUDE.md Excerpts` section below has every bullet that applies to this PRD. If a task description references a section not here:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```

4. **Verify branch** — `git branch --show-current` matches `feat/unify-execution-followups`. Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult **Data Flow Contracts** below or grep 2-3 existing call sites.
   - Pick an approach. Survey alternatives only when `estimatedEffort: "high"` OR `modifiesBehavior: true`.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). Fix failures before committing.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:` / `fix:` / `test:` / `cleanup:` as appropriate).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`. Do NOT edit the JSON.

10. **Append progress** — ONE post-implementation block, terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

When a task declares `modifiesBehavior: true`:

1. Read the specific callers/consumers named in the task description.
2. Decide per-caller: `OK` / `BREAKS` (split via `task-mgr add --stdin` then skip the original) / `NEEDS_REVIEW`.
3. If multiple call sites need different handling, split rather than shoehorn.

**Tasks in this PRD with `modifiesBehavior: true`:**
- **CODE-FIX-002** (apply_status_updates return type change) — survey callers before signature flip; the only production caller should be `iteration_pipeline.rs:253`.
- **FEAT-001** (slot prompt threading) — adds new SlotPromptParams fields; engine.rs build_slot_contexts is the one production wiring site.
- **WIRE-FIX-002** (slot prompt budget cap) — changes prompt assembly behavior; verify no test asserts a specific prompt size that the cap would now constrain.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **REVIEW-001** runs the full gate.

### Per-iteration scoped gate (CODE-FIX / WIRE-FIX / FEAT / REFACTOR-N tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test -p task-mgr <module_or_fn_name>   # scope to touched crate/module
```

For `touchesFiles` containing `src/loop_engine/iteration_pipeline.rs` → `cargo test --test iteration_pipeline` and `cargo test --test iteration_pipeline_parity`.
For `src/loop_engine/prompt/slot.rs` → `cargo test --test prompt_slot` and `cargo test --test prompt_slot_comprehensive`.
For `src/loop_engine/engine.rs` (broad) → run a representative subset: `cargo test -p task-mgr loop_engine`.

Pre-commit hooks may run additional checks. Fix every failure before committing.

**Do NOT** run the entire workspace test suite (`cargo test --workspace`) during regular iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test --workspace
```

If ANY test fails — including pre-existing failures — REVIEW-001 fixes them. Default: attempt every failure. Trunk-green is the invariant.

Pragmatic escape hatch: if there are >12 unrelated failures, fix the diff-attributable ones inline and spawn a single `FIX-xxx` task with `--from-json tasks/unify-execution-followups.json` listing the rest.

---

## Common Wiring Failures (REVIEW-001 reference)

- New code unreachable from production entry points → grep for caller
- Test mocks bypass real wiring → verify production path separately
- Config/struct field added but not threaded through → wire through
- Wrong key type on map access (`String` vs `&str` vs newtype) → check Data Flow Contracts
- New CLI argument / DB column / JSON field defined but not threaded into the dispatcher

---

## Review Tasks

| Review         | Priority | Spawns (priority)                                          | Focus                                                                                            |
| -------------- | -------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| REFACTOR-001   | 98       | `REFACTOR-FIX-xxx` (50-97) — **MUST** use `--from-json`    | DRY, complexity, coupling, prune-helper consolidation, function length                           |
| REVIEW-001     | 99       | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) — **MUST** use `--from-json` | Wiring, security, error handling, no `unwrap()`, qualityDimensions met, full-suite green |

### Spawning follow-up tasks (CRITICAL — read Learning #2236)

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "acceptanceCriteria": ["..."],
  "priority": 60,
  "touchesFiles": ["..."]
}' | task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/unify-execution-followups.json
```

**`--from-json tasks/unify-execution-followups.json` is REQUIRED.** Without it, the spawn entry leaks into whatever PRD JSON the CLI defaults to. CLEANUP-001 in this very PRD exists because that exact mistake happened on the parent PRD's CODE-REVIEW-1. Don't repeat it.

`--depended-on-by` wires the new task into REVIEW-001's `dependsOn` AND syncs the PRD JSON atomically — don't edit the JSON yourself.

---

## Progress Report Format

APPEND to `tasks/progress-$PREFIX.txt`:

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence]
Files: [comma-separated paths]
Learnings: [1-3 bullets]
---
```

Target: ~10 lines per block. If longer than ~25 lines, compress.

---

## Learnings Guidelines

Don't Read `tasks/long-term-learnings.md` or `tasks/learnings.md` directly. Use:

- `task-mgr recall --for-task <TASK-ID>`
- `task-mgr recall --query "<keywords>"`

Record your own with `task-mgr learn`.

**Concise format:**
- GOOD: "`temps::chrono::Timezone` accessed via full path, not temps_core"
- BAD: long descriptive paragraph

---

## Stop and Blocked Conditions

### Stop

Before `<promise>COMPLETE</promise>`:
1. All tasks `passes: true`
2. No new tasks created in final review
3. REVIEW-001 passed full suite

```
<promise>COMPLETE</promise>
```

### Blocked

If blocked:
1. Document blocker in progress file
2. Create clarification task: `echo '{...}' | task-mgr add --stdin --depended-on-by <blocked-task> --from-json tasks/unify-execution-followups.json`
3. Output:

```
<promise>BLOCKED</promise>
```

---

## Key Learnings (from task-mgr recall)

These are pre-distilled. Treat as authoritative.

- **#2236** — Spawned fixup tasks (`CODE-FIX-`, `WIRE-FIX-`, `IMPL-FIX-`, `REFACTOR-N-`) leak into the wrong PRD JSON when `task-mgr add --stdin` is invoked without `--from-json`. CLEANUP-001 in this PRD exists because of this exact bug on the parent PRD. **ALWAYS pass `--from-json tasks/unify-execution-followups.json` when spawning.**
- **#2237** — `process_slot_result` calls `iteration_pipeline::process_iteration_output` unconditionally, including for slots with `claim_succeeded == false`. Pollutes `ctx.crashed_last_iteration`. CODE-FIX-001 fixes this with a top-of-function early-return.
- **#2238** — Status-tag completion gate (`iteration_pipeline.rs:275-286`) uses global "any update succeeded" flag, falsely marking the claimed task done when its specific dispatch failed but a peer's succeeded. CODE-FIX-002 surfaces per-(task_id, status) success from `apply_status_updates`.
- **#2239** — `prompt::slot::build_prompt` silently drops `steering.md` and `session_guidance` vs sequential. The disjoint-tasks rationale that justifies dropping synergy/reorder/sibling-PRD does NOT apply to project-wide steering or operator pause feedback. FEAT-001 threads them through.
- **#2079, #2073** — TDD-bootstrap pattern: pre-land struct fields with `None`/empty defaults at every literal site, plus `#[ignore]`'d body tests, so the implementation task only flips the post-Claude success site. TEST-INIT-001 uses this pattern for CODE-FIX-002.
- **#2043** — `static_assertions::assert_impl_all!(T: Send)` enforces thread-safety bounds at compile time. FEAT-001 must NOT regress the `SlotPromptBundle: Send` assertion.
- **#2191, #2198, #2224** — Both sequential and wave paths share `iteration_pipeline::process_iteration_output`. Adding behavior in the pipeline benefits both modes. Resist re-introducing per-mode special cases.
- **#2031** — Four-rung overflow recovery contract: ctx update → DB UPDATE → stderr → dump → JSONL → rotate. WIRE-FIX-001's debug_assert addition must NOT change this ordering.
- **#1864** — `SlotContext` is `Send` but not `Sync` (intentional). Same applies to `SlotPromptBundle`.
- **#2068** — `skip_git_completion_detection` is mode-agnostic for the already-complete fallback. Both wave and sequential paths run it. Don't accidentally re-couple the fallback to mode.
- **#2066** — Task completion deduplication across signal sources (status-tag, completed-tag, git, scan, fallback) is owned by the pipeline's HashSet. Don't re-implement at call sites.

---

## CLAUDE.md Excerpts (only what applies to this change)

These bullets were extracted for the subsystems this PRD touches. Do NOT Read CLAUDE.md in full.

### Loop CLI Cheat Sheet (CLAUDE.md:67)

- Add a task: `echo '{...}' | task-mgr add --stdin`
- Link into milestone: append `--depended-on-by MILESTONE-ID`
- Mark status: emit `<task-status>TASK-ID:done</task-status>` (also: `failed`, `skipped`, `irrelevant`, `blocked`)
- Permission guard: loop iterations deny Edit/Write on `tasks/*.json` via `--disallowedTools`
- Never edit `.task-mgr/tasks/*.json` directly
- **Spawn-fixup PRD targeting**: when CODE-REVIEW or MILESTONE iterations spawn ad-hoc fixup tasks, the `task-mgr add --stdin` invocation MUST disambiguate the destination PRD with `--from-json tasks/<correct-prd>.json` OR `--depended-on-by <milestone-of-correct-prd>`. Otherwise the entry leaks into whatever PRD JSON the CLI defaults to.

### Overflow recovery and diagnostics (CLAUDE.md:75)

Order of operations is contractual (do not reorder): **ctx update → DB UPDATE → stderr → dump → JSONL → rotate**. Recovery state must be durable before any best-effort observability writes. WIRE-FIX-001's debug_assert addition is at the top of the per-slot overflow branch and must NOT change this ordering.

`OverflowEvent` JSONL serialization gained `slot_index: Option<usize>` with `#[serde(skip_serializing_if = "Option::is_none")]` — sequential JSONL stays byte-identical.

### Iteration pipeline (shared) — landing on merge-back of the parent PRD

`iteration_pipeline::process_iteration_output` is the canonical home for post-Claude behaviors. Both `run_loop` (sequential, ~engine.rs:3204) and `process_slot_result` (wave, ~engine.rs:1166) invoke it. Adding behavior in the pipeline benefits both modes; do NOT re-introduce per-mode branches at call sites.

The pipeline owns:
1. `progress::log_iteration`
2. `<key-decision>` extraction → `key_decisions_db::insert_key_decision`
3. `<task-status>` dispatch via `apply_status_updates` (CODE-FIX-002 changes its return type)
4. Completion ladder: `<task-status>:done` → `<completed>` → git (skip_git=false only) → output scan → already-complete fallback
5. `learnings::ingestion::extract_learnings_from_output`
6. `feedback::record_iteration_feedback`
7. Per-task crash-tracking writes onto `ctx.crashed_last_iteration` (CODE-FIX-001 must short-circuit BEFORE this for claim_succeeded=false; CODE-FIX-003 must prune AFTER terminal-status DB writes)

### Learning Creation Chokepoint (CLAUDE.md:129)

All production code paths that create learnings must go through `LearningWriter` in `src/learnings/crud/writer.rs`. CODE-FIX-002 / CODE-FIX-003 / WIRE-FIX-001 / WIRE-FIX-002 do NOT create learnings — but if any task spawns a sub-fix that does, route through LearningWriter.

### Soft-dep guard for milestone scheduling (CLAUDE.md:342)

`SPAWNED_FIXUP_PREFIXES = ["REFACTOR-N", "CODE-FIX", "WIRE-FIX", "IMPL-FIX"]` — this PRD uses `CODE-FIX-`, `WIRE-FIX-`, `REFACTOR-N-`, `CLEANUP-`, `FEAT-`, `TEST-INIT-`, `REFACTOR-`, `REVIEW-`. The soft-dep guard token-matches on these prefixes; AC text that mentions a fixup prefix as a standalone token will defer milestone candidates while same-prefix siblings are active. **AC writing convention**: if an AC mentions `CODE-FIX-xxx` etc. in text, write it as a standalone token (not embedded in a parenthetical with a different prefix) so the guard fires correctly.

---

## Data Flow Contracts

### apply_status_updates → iteration_pipeline gate (CODE-FIX-002)

```rust
// Source: src/loop_engine/engine.rs::apply_status_updates
// CURRENT signature (before CODE-FIX-002):
pub fn apply_status_updates(
    conn: &mut Connection,
    updates: &[detection::TaskStatusUpdate],   // <-- input
    run_id: Option<&str>,
    prd_path: Option<&Path>,
    task_prefix: Option<&str>,
    progress_path: Option<&Path>,
    db_dir: Option<&Path>,
) -> u32                                          // <-- TODAY: global count
// PROPOSED (CODE-FIX-002):
) -> Vec<(String, detection::TaskStatusChange, bool)>   // <-- per-(id, status, applied)

// Consumer: src/loop_engine/iteration_pipeline.rs:253-286
// CURRENT consumption:
let status_updates_applied: u32 = apply_status_updates(...);
result.status_updates_applied = status_updates_applied;
if status_updates_applied > 0
    && status_updates.iter().any(|u| {
        matches!(u.status, detection::TaskStatusChange::Done) && u.task_id == claimed_id
    })
{
    record_completion(claimed_id, ...);    // <-- BUG: fires even if claimed dispatch failed
}

// PROPOSED consumption (CODE-FIX-002):
let apply_results = apply_status_updates(...);
result.status_updates_applied = apply_results.iter().filter(|(_, _, ok)| *ok).count() as u32;
if apply_results.iter().any(|(id, st, ok)|
    *ok && id == claimed_id && matches!(st, detection::TaskStatusChange::Done)
) {
    record_completion(claimed_id, ...);
}
```

`detection::TaskStatusChange` enum lives at `src/loop_engine/detection.rs:252` (variants: `Done`, `Failed`, `Skipped`, `Irrelevant`, `Unblock`, `Reset`).

### SlotPromptBundle → SlotResult → synthetic_prompt (WIRE-FIX-001)

```rust
// Source: src/loop_engine/prompt/slot.rs::SlotPromptBundle
pub struct SlotPromptBundle {
    pub prompt: String,
    pub task_id: String,
    pub task_files: Vec<String>,
    pub shown_learning_ids: Vec<i64>,
    pub resolved_model: Option<String>,
    pub difficulty: Option<String>,            // <-- already exists
    pub section_sizes: Vec<(&'static str, usize)>,
}

// Intermediate: src/loop_engine/engine.rs::SlotResult (~ line 749 + companions)
pub struct SlotResult {
    pub iteration_result: IterationResult,
    pub claim_succeeded: bool,
    pub slot_index: usize,
    pub shown_learning_ids: Vec<i64>,
    pub prompt_for_overflow: Option<String>,
    pub section_sizes: Vec<(&'static str, usize)>,
    // PROPOSED (WIRE-FIX-001): pub task_difficulty: Option<String>,
}

// Sink: src/loop_engine/engine.rs:1128-1140 (per-slot overflow branch)
let synthetic_prompt = crate::loop_engine::prompt::PromptResult {
    prompt: slot_result.prompt_for_overflow.clone().unwrap_or_default(),  // <-- L3: add debug_assert above
    task_id: tid.clone(),
    task_files: slot_result.iteration_result.files_modified.clone(),
    shown_learning_ids: Vec::new(),
    resolved_model: slot_result.iteration_result.effective_model.clone(),
    dropped_sections: Vec::new(),
    task_difficulty: None,                                                 // <-- L4: read slot_result.task_difficulty
    cluster_effort: slot_result.iteration_result.effective_effort,
    section_sizes: slot_result.section_sizes.clone(),
};
```

### Slot prompt threading (FEAT-001)

```rust
// Source: src/loop_engine/prompt/sequential.rs::BuildPromptParams (lines ~108-130)
pub struct BuildPromptParams<'a> {
    // ... many fields ...
    pub session_guidance: &'a str,
    pub steering_path: Option<&'a Path>,
    // ...
}

// Engine reading them (sequential):
//   src/loop_engine/engine.rs:1635 — let session_guidance_text = ctx.session_guidance.format_for_prompt();
//   src/loop_engine/engine.rs:1644-1646 — passed to BuildPromptParams
//   src/loop_engine/engine.rs:2937-2940 — steering_path resolved from paths.tasks_dir.join("steering.md")

// Target: src/loop_engine/prompt/slot.rs::SlotPromptParams
//   ADD: pub steering_path: Option<&Path>,
//   ADD: pub session_guidance: &str,

// Build site (engine.rs build_slot_contexts):
//   ctx.session_guidance is the source-of-truth; format_for_prompt() returns &str
//   steering_path: Option<&Path> — reuse the same `steering` resolved at engine.rs:2937 above
```

### crashed_last_iteration prune (CODE-FIX-003)

```rust
// Owner: src/loop_engine/engine.rs::IterationContext (line ~228)
pub crashed_last_iteration: HashMap<String, bool>,

// Production write site: src/loop_engine/iteration_pipeline.rs:452-457 (insert true on Crash, false otherwise)

// PROPOSED prune sites:
//   src/loop_engine/engine.rs::apply_status_updates Done/Failed/Skipped/Irrelevant arms
//     — AFTER successful complete_cmd::complete / fail / skip / irrelevant call
//     — REQUIRES: thread &mut IterationContext into apply_status_updates (currently has &mut Connection only)
//   src/loop_engine/prd_reconcile.rs::mark_task_done
//     — AFTER successful DB write
//     — REQUIRES: thread &mut IterationContext into mark_task_done (currently doesn't have it)
//
// Survey via `git grep mark_task_done` — multiple call sites need the new param threaded.
```

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- Work on the correct branch: **feat/unify-execution-followups**
- **Always pass `--from-json tasks/unify-execution-followups.json`** when spawning fixup tasks (Learning #2236)
