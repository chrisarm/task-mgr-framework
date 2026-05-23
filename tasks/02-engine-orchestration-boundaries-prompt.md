# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Engine Orchestration Boundaries — Carving `engine.rs`** for **task-mgr**.

## Problem Statement

`src/loop_engine/engine.rs` is **9,644 lines** and is the integration hub for almost everything the loop subsystem does. After the TaskLifecycle Extraction PRD merged, the file's ~15 raw `UPDATE tasks SET status` sites already route through `TaskLifecycle` — but the surrounding 9k lines of orchestration (outer `run_loop`, sequential `run_iteration`, wave `run_wave_iteration` + `run_slot_iteration` + `process_slot_result`, per-task recovery helpers, signal handling, config loading) still live in one file with no module boundary between concerns.

Adding any new monitoring hook, recovery branch, or per-iteration side effect today requires searching the 9k-line file for 3-4 call sites and touching them in lockstep, hoping the wave-vs-sequential parity invariant still holds. The `iteration_pipeline.rs` extraction (the shared post-Claude pipeline) proved the win pattern: once a concern lives behind a single function, parity divergence becomes a compile-time concern. This refactor generalizes that pattern to the rest of the orchestration.

This PRD carves `engine.rs` into five sibling modules: **orchestrator.rs** (outer loop), **iteration.rs** (sequential), **wave_scheduler.rs** (parallel wave + merge-back), **slot.rs** (per-slot lifecycle + result), **recovery.rs** (per-task recovery cluster). The five layers of parallel-slot cascade defenses must survive verbatim or with provably equivalent replacements. Observable behavior must be byte-identical.

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

Non-negotiables for THIS PRD: this is a refactor, not a hardening pass. Existing `.unwrap()`s, error-handling shapes, and stderr emissions move verbatim. Carve along existing seams; do not invent new ones. Each extraction is one commit. Defense layers move with their regression tests in scope. The dogfood gate (10 iterations × 2 PRDs, byte-identical stderr + DB) is the final arbiter.

**Prohibited outcomes:**

- Adding a raw `UPDATE tasks SET status` site, even temporarily, anywhere in the new modules — LIFECYCLE-EXCEPTION grep lint MUST stay green at every commit
- Introducing dynamic dispatch (`Box<dyn LlmRunner>`, `Box<dyn Scheduler>`, etc.) on the hot path; static-dispatch `RunnerKind` enum stays the path
- Recomputing slot 0's worktree path via `compute_slot_worktree_path(_, branch, 0)` — defense layer #1 hinges on threading the path from `ensure_slot_worktrees`
- Splitting the `apply_pending_promotion` / `escalate_task_model_if_needed_inner` pair across commits — the after-`tx.commit()` invariant must move atomically
- Removing or refactoring the `SYNTHETIC_DEADLOCK_SLOT` sentinel — defense layer #4's deadlock-guard contract depends on it
- Widening visibility beyond what an extraction strictly requires (`pub(super)` → `pub(crate)` → `pub` ladder)
- Adding `pub use` chains that pierce three or more modules to preserve a single import path
- Renaming exported symbols (`run_loop`, `run_iteration`, `run_wave_iteration`, `process_slot_result`, etc.) — public surface stability per FR-008
- Editing `iteration_pipeline.rs`'s production code (the FR-006 assertion lives in `tests/` or as a `#[cfg(test)]` block)
- Skipping the dogfood gate. 10 iterations × 2 distinct PRDs with byte-identical stderr/DB capture is REVIEW-001's gate
- Tidying or refactoring extracted code during the move. Carve first, polish (if needed) in a separate REFACTOR-xxx after REFACTOR-REVIEW-FINAL

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: `cargo fmt --check` passes
- Rust: `cargo check --all-targets --all-features` passes with no new warnings
- Rust: `cargo clippy -- -D warnings` passes
- Rust: Scoped tests for touched files pass with `cargo test`
- No new `.unwrap()` / `.expect()` in production paths (existing ones move verbatim; `#[cfg(test)]` unchanged)
- No new raw `UPDATE tasks SET status` SQL anywhere — LIFECYCLE-EXCEPTION grep lint stays green
- Static-dispatch `RunnerKind` path preserved on every spawn boundary (no `Box<dyn LlmRunner>`)
- `Send + Sync` invariants on `SlotPromptBundle` + `LlmRunner` preserved (compile-time assertion in `prompt/mod.rs` still passes)
- Public surface stable: `cargo check --all-targets` passes without import changes outside `src/loop_engine/`
- Comments explain WHY (boundary contracts, defense-layer invariants), never narrate WHAT or the extraction itself

---

## Cross-PRD Dependencies (check before every task)

This PRD blocks on work in other PRD files. Before claiming any task, verify each entry below shows `passes: true` in its referenced PRD JSON (use `jq '.userStories[] | select(.id=="<id>") | .passes' tasks/<other-prd>.json`). If any is still `false`, output `<promise>BLOCKED</promise>` with the reason and stop.

- **tasklifecycle-extraction.json :: MILESTONE-FINAL** — TaskLifecycle service must own every status write (LIFECYCLE-EXCEPTION grep lint green) before the carve begins. Otherwise the carve would also have to move the ~15 raw UPDATE sites in engine.rs, inflating blast radius and creating merge hell against an in-flight migration. CLARIFY-001 verifies this gate explicitly.

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/02-engine-orchestration-boundaries.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task    | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` — priority auto-computed; DB + PRD JSON updated atomically                                                     |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task (rare — e.g., cross-PRD `requires[]`), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/02-engine-orchestration-boundaries.json
jq '.globalAcceptanceCriteria' tasks/02-engine-orchestration-boundaries.json
```

### Files you DO touch

| File                                              | Purpose                                                                |
| ------------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/engine-orchestration-boundaries-prompt.md` | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`                      | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log. Two patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing; never crash on absent files.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/02-engine-orchestration-boundaries.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes` — everything you need.

2. **Pull only the progress context you need** — most iterations want just the most recent section. If `dependsOn` references a task whose rationale you need, grep that specific task's block.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns learnings scored highest for this specific task. **Do not** Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, grep for the relevant term:
   ```bash
   grep -n -A 10 '<keyword or header>' src/loop_engine/CLAUDE.md
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed. Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - Pick an approach. Only for `estimatedEffort: "high"` tasks: one rejected alternative + one-line reason.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). Fix failures before committing; never commit broken code.

8. **Commit**: `refactor: <TASK-ID>-completed - [Title]` (or `feat:`/`test:` as appropriate for the task type — this PRD is almost entirely `refactor:`).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON.

10. **Append progress** — ONE post-implementation block, terminated with `---` so the next iteration's tail works.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` already picks: eligible tasks (`passes: false`, deps complete, not `requiresHuman`), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority.

---

## Quality Checks

### Per-iteration scoped gate

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
# Most FEAT tasks here touch src/loop_engine/ — scope to the loop_engine module
cargo fmt --check
cargo check                                                  # fast type check
cargo clippy -- -D warnings
cargo test -p task-mgr loop_engine                           # scoped to loop_engine
cargo test -p task-mgr <specific_test_fn>                    # narrower if needed
```

Scoping heuristic: extractions touch `src/loop_engine/`, so `cargo test -p task-mgr` filtered to relevant modules is usually right. Defense-layer regression tests live in `src/loop_engine/worktree.rs`, `src/loop_engine/engine.rs`, and `src/commands/next/selection.rs` — confirm they still run after each extraction.

**Do NOT** run the entire workspace test suite during regular iterations — that's REVIEW-001's job.

### Final gate at REVIEW-001 (the milestone)

`REVIEW-001` runs the **full, unscoped** suite on a clean checkout AND the dogfood gate (10 iterations × 2 PRDs with byte-identical stderr + DB diff vs. ANALYSIS-001 baselines). The full suite must finish green, including pre-existing failures (trunk-green is the invariant).

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
# Plus the dogfood gate — see REVIEW-001 acceptance criteria
```

If more than ~12 pre-existing failures are clearly unrelated to this PRD, spawn one `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001` and BLOCKED until resolved. Below that threshold, fix inline.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production. Most common misses for a carve like this:

- New module declared in `mod.rs` but `pub use` re-export missing → external callers fail to compile
- Symbol moved but `engine.rs` still has a stale `use` import → unused-import warning
- Visibility over-narrowed: `pub(super)` on something `wave_scheduler` needs from `slot` → compile failure across modules
- Visibility over-widened: `pub` on something only the parent module uses → CODE-REVIEW-1 finding
- Test moved with code but old test file still has a stale `mod tests` reference → compile failure
- `SYNTHETIC_DEADLOCK_SLOT` moved without its handler → defense layer #4 silently broken (test still passes because it exercises the handler directly)

---

## Review Tasks

This PRD uses the lean review path:

| Review                  | Priority | Spawns (priority)                  | Focus                                                                                                |
| ----------------------- | -------- | ---------------------------------- | ---------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | Post-extraction quality, defense-layer preservation, public-surface stability, visibility audit       |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)             | DRY across new modules, complexity hotspots, coupling, contract fidelity                              |
| REVIEW-001              | 99       | `FIX-xxx` (only if escape-hatch fires) | Full unscoped quality suite + dogfood gate (10 iterations × 2 PRDs, byte-identical stderr + DB diff) |

Use the `rust-python-code-reviewer` agent for substantive code review. Spawning template (one shape covers CODE-FIX, WIRE-FIX, REFACTOR-N-xxx):

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

`--depended-on-by` wires the new task into the milestone's `dependsOn` AND syncs the PRD JSON atomically. If no issues found, emit the status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block. Compress if longer than ~25 lines.

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify REVIEW-001 passes (full suite + dogfood gate)
3. Verify no new tasks were created in final review

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked:

1. Document blocker in the progress file
2. Create clarification task via `task-mgr add --stdin`
3. Output:

```
<promise>BLOCKED</promise>
```

---

## Milestones

This PRD has one milestone: **REVIEW-001** (priority 99). It runs the full unscoped quality suite AND the dogfood gate. Trunk-green is the invariant. Pre-existing failures are this milestone's responsibility (with the >12-unrelated-failures escape hatch).

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here.

- **[unify-sequential-and-wave-execution]** — establishing `iteration_pipeline.rs` as the parity-divergence prevention pattern was the wedge. This PRD generalizes the wedge.
- **[2956]** — `RunnerKind` enum static dispatch keeps allocation-free; no `Box<dyn LlmRunner>` on the hot path. Preserve.
- **[merge-back path threading]** — slot 0's worktree path must come from `ensure_slot_worktrees`, never `compute_slot_worktree_path(_, branch, 0)`. The recomputation diverges when the loop runs from inside the matching worktree.
- **[transactional promotion pattern]** — inner helper performs DB writes only, returns `Option<PendingPromotion>`; caller applies it via `apply_pending_promotion` ONLY after `tx.commit()?` returns Ok. Inner/apply pair moves as one commit.
- **[SYNTHETIC_DEADLOCK_SLOT sentinel]** — synthesis must always emit at least one failure record even when every upstream parser rejected the input. Otherwise downstream `is_empty()` checks invert the safety guarantee.
- **[slot-0 SAFETY GUARD]** — `classify_ephemeral_branch` rejects slot=0 and `list_ephemeral_slot_branches` filters slot > 0. Defense layer #5 hinges on this — never broaden the glob without re-adding the rejection.
- **[Run-level config caching]** — `ProjectConfig` and PRD-side `implicit_overlap_files` loaded ONCE at `run_loop` startup, threaded through `WaveIterationParams`. Never call `read_project_config` or `read_prd_implicit_overlap_files` mid-wave.
- **[1626, superseded by Phase 1]** — opt-in cleanup flag threaded through spawn signature was the wrong shape; the structural fix (trait method) replaced it. Same lesson applies here: capability differences belong at the boundary, not as silent destructures.
- **[mid-loop JSON sync]** — Never run bare `task-mgr init --from-json <prd>` — it wipes status / started_at / completed_at. Use `task-mgr loop init <prd>.json --append --update-existing` for incremental sync.
- **[live-loop dogfood risk]** — the maintainer runs `task-mgr loop` against in-progress PRDs daily on this codebase. A bad refactor merge can corrupt a live PRD's task DB or break a running iteration. Hence the byte-identical dogfood gate.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets are extracted from `src/loop_engine/CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file.

**Iteration pipeline (shared) — DO NOT split this pipeline across the carve:**

> Sequential (`run_iteration`) and parallel-wave (`run_slot_iteration` + `process_slot_result`) execution paths share a single post-Claude pipeline: `process_iteration_output` in `src/loop_engine/iteration_pipeline.rs`. The single-pipeline contract makes parity-divergence a compile-time concern (any new step is added in one place; both call sites pick it up).

**Five layers of parallel-slot defenses — all must survive verbatim:**

1. **Slot path threading** — `merge_slot_branches_with_resolver` takes `slot_paths: &[PathBuf]` and uses `slot_paths[0]` as slot 0's path, never recomputing via `compute_slot_worktree_path(project_root, branch, 0)`. The recomputation diverges when the loop runs from inside the matching worktree.
2. **Consecutive-merge-fail halt threshold** — `ProjectConfig::merge_fail_halt_threshold` (default `2`) caps consecutive parallel-slot merge-back failure waves before the engine halts. Reset/halt contract implemented once in `apply_merge_fail_reset_and_halt_check`.
3. **Implicit-overlap baseline + buildy heuristic** — `select_parallel_group` serializes shared-infra contention through a single synthetic `__shared_infra__` slot per wave. Three claim conditions: (a) touchesFiles basename matches IMPLICIT_OVERLAP_FILES ∪ ProjectConfig::implicit_overlap_files ∪ PrdFile::implicit_overlap_files; (b) task id matches BUILDY_TASK_PREFIXES; (c) `claims_shared_infra: Some(true)`.
4. **Cross-wave file affinity** — `ephemeral_overlay: &[(branch, files)]` lists files claimed by un-merged ephemeral slot branches from prior waves. Deadlock guard: when greedy pass yields empty group AND every candidate's only overlap was ephemeral, `ephemeral_block_diagnostics` is populated; engine treats this as `failed_merges` non-empty so FEAT-002 reset/halt fires.
5. **Stale ephemeral branch hygiene at startup** — `reconcile_stale_ephemeral_slots` runs once at loop startup BEFORE `ensure_slot_worktrees`. Slot-0 SAFETY GUARD: `classify_ephemeral_branch` returns `Err` when parsed slot suffix is `0`; `list_ephemeral_slot_branches` filters `slot > 0`.

**Overflow recovery ladder — preserved verbatim:** Entry point `overflow::handle_prompt_too_long`. Five rungs: (1) downgrade effort, (2) escalate model below Opus, (3) escalate to 1M-context Opus, (4) FallbackToProvider (Grok, if configured + enabled + currently Claude), (5) Block.

**Operator escape valve — `check_override_invalidation`:** at the TOP of every iteration (BEFORE `resolve_effective_runner`), compares current `tasks.model` against `overflow_original_task_model[task_id]` snapshot. Divergence clears all six per-task auto-recovery channels in one shot. **The call ordering is contractual** — preserve the "before resolve" position.

**Transactional promotion pattern:** `inner` helper performs DB writes only, returns `Option<PendingPromotion>`; caller applies via `apply_pending_promotion` ONLY after `tx.commit()?` returns Ok. Otherwise in-memory ctx claims a promotion the DB rolled back. Move the inner/apply pair as one commit.

**Touchpoints table** (in `src/loop_engine/CLAUDE.md`) — every row that today says `src/loop_engine/engine.rs::<symbol>` will need an update after the corresponding extraction lands. FEAT-006 owns the update.

---

## Data Flow Contracts

N/A for this refactor. No new cross-module data structures are introduced; existing types (`IterationContext`, `SlotContext`, `WaveIterationParams`, `SlotIterationParams`, `ProcessingParams`, `RunnerOpts`, `RunnerResult`) keep their current layouts and ownership rules. The only data-flow shift is the literal file location of the function that constructs / consumes them, which the PRD's FR table and Public Contracts table track.

---

## Important Rules

- Work on **ONE task per iteration**
- **For high-effort tasks** (this PRD's FEAT-001 through FEAT-005 are all `estimatedEffort: "high"`): no `/ralph-loop` necessary — each FEAT is mechanically a "move + re-export + update imports" sequence with clear acceptance criteria
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- **Check existing patterns** — see `src/loop_engine/CLAUDE.md` excerpts above
- **Boundary contract with runner-trait-hygiene Phase 2 PRD**: that PRD touches `engine.rs::~15 RunnerKind match sites` for the FR-005 audit. If you see a one-line `// kind-correct: identity, not capability` annotation comment from that PRD landing in a hunk you're about to move, preserve it. Coordinate via reviewer overlap; do not delete the annotations.
