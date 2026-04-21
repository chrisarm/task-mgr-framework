# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Parallel Task Execution + Relationship Simplification** for **task-mgr**.

## Problem Statement

The task-mgr loop engine executes tasks sequentially: select one task, spawn Claude, wait for completion, repeat. For PRDs with many independent tasks touching disjoint files, this wastes wall-clock time. Two tasks editing different files could safely run in parallel.

Additionally, the `synergyWith`/`batchWith`/`conflictsWith` relationship types are being dropped in favor of runtime file-overlap detection from `touchesFiles`. The scoring algorithm should be simplified to use file-overlap data directly, and that same data becomes the conflict-detection mechanism for parallel execution.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) -> **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later -- take it, we're pre-launch) -> **FUNCTIONING CODE** (pragmatic, reliable) -> **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) -> **CODE QUALITY** (clean, no warnings) -> **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context
- Two tasks with overlapping touchesFiles in the same parallel group
- Sharing IterationContext across threads -- each slot gets its own minimal state
- Using git worktree add on a branch already checked out -- git forbids this

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD -- the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy` output
- Rust: All tests pass with `cargo test`
- Rust: `cargo fmt --check` passes
- No breaking changes to existing APIs unless explicitly required
- `--parallel 1` (default) produces identical behavior to current sequential execution
- Old PRD JSON with synergyWith/batchWith/conflictsWith parses without error

---

## Task Files + CLI (IMPORTANT -- context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts) is already embedded in **this prompt file** -- that is the authoritative copy.

### Getting your PRD's task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/parallel-task-execution.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                   |
| -------------------------------------- | ----------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task    | `task-mgr next --prefix $PREFIX --claim`                                                  |
| Inspect one task                       | `task-mgr show $PREFIX-TASK-ID`                                                           |
| List remaining tasks                   | `task-mgr list --prefix $PREFIX --status todo`                                            |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID`                                              |
| Add a follow-up task                   | `echo '{...}' \| task-mgr add --stdin --depended-on-by MILESTONE-N`                      |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>`                                    |

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/parallel-task-execution-prompt.md` | This prompt file (read-only)                                          |
| `tasks/progress-{{TASK_PREFIX}}.txt` | Progress log -- **tail** for recent context, **append** after each task    |

**Reading progress:**

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

---

## Your Task (every iteration)

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/parallel-task-execution.json)
   task-mgr next --prefix $PREFIX --claim
   ```

2. **Pull only the progress context you need** -- most iterations want just the most recent section.

3. **Recall focused learnings** -- `task-mgr recall --for-task <TASK-ID>`. Do not Read `tasks/long-term-learnings.md` or `tasks/learnings.md` directly. Do not Read `CLAUDE.md` in full -- grep for specific terms if needed.

4. **Verify branch** -- `git branch --show-current` matches branchName.

5. **Think before coding** -- state assumptions, plan edge-case handling, consult Data Flow Contracts for cross-module access.

6. **Implement** -- single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below). Fix failures before committing.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]`

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`

10. **Append progress** -- ONE post-implementation block.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` already picks: eligible tasks (`passes: false`, deps complete), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

1. Check that consumer analysis has been done (in the task notes or progress file).
2. `BREAKS` -> split into per-context subtasks. `NEEDS_REVIEW` -> verify manually. `OK` -> proceed.
3. If multiple semantic contexts exist for the same code path, split rather than shoehorn.

---

## Quality Checks

### Per-iteration scoped gate

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test -p task-mgr <module_or_fn_name>   # scope to touched files
```

### Milestone gate (MILESTONE-1 / -2 / -3 / -FINAL)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

Full unscoped suite. Fix ALL failures including pre-existing.

---

## Common Wiring Failures (CODE-REVIEW reference)

- Not registered in dispatcher/router
- Test mocks bypass real wiring
- Config field read but not passed through
- Unused-import warning on new code
- New CLI flag defined but not threaded into the handler

---

## Review Tasks

| Review                  | Priority | Spawns                           | Before          | Focus                                         |
| ----------------------- | -------- | -------------------------------- | --------------- | --------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16) | MILESTONE-1     | Phase 1: scoring, import, export, show cleanup |
| CODE-REVIEW-2           | 30       | `CODE-FIX` (31-33)              | MILESTONE-2     | Phase 2: thread safety, git, crash policy      |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)          | MILESTONE-FINAL | DRY, complexity, coupling, clarity             |

### Spawning follow-up tasks

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-N: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 14,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by MILESTONE-1
```

---

## Progress Report Format

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence]
Files: [comma-separated paths]
Learnings: [1-3 bullets, one line each]
---
```

---

## Learnings Guidelines

- `task-mgr recall --for-task <TASK-ID>` for indexed retrieval
- `task-mgr learn` to record your own learnings
- Do NOT Read learnings files directly

---

## Stop and Blocked Conditions

### Stop: `<promise>COMPLETE</promise>` when all stories have `passes: true`
### Blocked: `<promise>BLOCKED</promise>` with documented reason

---

## Key Learnings (from task-mgr recall)

- **[522]** Incremental field addition to IterationResult — added key_decisions_count to all 12 construction sites in engine.rs. Follow same pattern for new parallel structs.
- **[1601]** Update all spawn_claude callers in same commit when adding new arg — 8+ call sites across the codebase
- **[135]** Use multi-iteration sequences to test state machine transitions — crash escalation depends on outcome sequences
- **[1005]** 9 consecutive retries on same blocked task wastes iterations — CrashTracker policy must prevent this in parallel mode
- **[791]** Integration test for worktree chaining: setup_git_repo_with_file for temp repo, then verify
- **[1329]** git merge --no-commit --no-ff safely tests prerequisite merges
- **[1448]** Stub migration pattern with #[ignore] tests enables TDD database changes
- **[1027]** Migration tests should use >= assertions for schema version
- **[1251]** Multiple modules may have ignored tests gated on the same migration
- **[1549]** Worktree DB needs migrate before smoke tests — relevant for per-slot worktrees
- **[1444]** Soft-archive queries need archived_at IS NULL filters on all listing operations
- **[311]** Worktree branch points can trail behind main branch commits

---

## CLAUDE.md Excerpts (only what applies to this PRD)

- Database is at `.task-mgr/tasks.db` (relative to project/worktree root). Each worktree has its own copy.
- Worktree main: `$HOME/projects/task-mgr`; feature worktrees: `{repo}-worktrees/<branch>/`
- Task files: `.task-mgr/tasks/<prd-name>.json`; prompts: `<prd-name>-prompt.md`; progress: `progress.txt` (or `progress-{prefix}.txt`)
- Model IDs in `src/loop_engine/model.rs`. After changes: `cargo run --bin gen-docs`
- Loop CLI: `echo '{"id":...}' | task-mgr add --stdin`; `--depended-on-by MILESTONE-ID`; status via `<task-status>ID:done</task-status>`
- Learning creation must go through `LearningWriter` in `src/learnings/crud/writer.rs`
- `curate embed` default: Ollama at localhost:11434 with jina embeddings model

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly -- do NOT guess key types.

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
|---|---|---|
| task_files -> inverted index -> conflict check | `HashMap<String, Vec<String>>` (task_id->files) from `get_all_task_files()` -> inverted to `HashMap<&str, HashSet<usize>>` (file->selected_indices) | `let task_files = get_all_task_files(conn, prefix)?; let mut used_files: HashSet<&str> = HashSet::new(); for candidate in scored_tasks { let files = task_files.get(&candidate.task.id).map(|v| v.as_slice()).unwrap_or(&[]); if files.iter().any(|f| used_files.contains(f.as_str())) { continue; } for f in files { used_files.insert(f.as_str()); } group.push(candidate); }` |
| parallel group -> wave -> slot threads | `Vec<ScoredTask>` from `select_parallel_group()` zipped with `Vec<PathBuf>` worktree paths -> `Vec<SlotContext>` | `let group = select_parallel_group(conn, &files, &completed, prefix, slots)?; let ctxs: Vec<SlotContext> = group.into_iter().zip(worktree_paths).enumerate().map(\|(i, (task, wt))\| SlotContext { slot_index: i, working_root: wt, task }).collect();` |
| wave result -> iteration context merge | `WaveResult { outcomes: Vec<SlotResult> }` merged into `IterationContext` on main thread | `for slot_result in wave.outcomes { ctx.last_files.extend(slot_result.iteration_result.files_modified); if matches!(slot_result.iteration_result.outcome, IterationOutcome::Completed) { tasks_completed += 1; } }` |

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **CRITICAL**: Git worktrees cannot share a branch. Slots 1+ must use ephemeral branches `{branch}-slot-{N}`, not the same branch as slot 0.
- **CRITICAL**: IterationContext is NOT thread-safe. Each slot thread gets its own minimal mutable state. Results merge back on the main thread after the wave completes.
