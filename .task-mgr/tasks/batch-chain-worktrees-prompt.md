# Claude Code Agent Instructions

You are an autonomous coding agent implementing **`--chain` flag for batch mode** for **task-mgr**.

## Problem Statement

When running multiple PRD task files in batch mode, each PRD currently creates its worktree/branch independently from HEAD. Phase 2 doesn't see phase 1's code changes. Add a `--chain` flag that makes sequential PRDs build on each other — phase 2 branches from phase 1's branch, phase 3 from phase 2's, etc. This enables per-phase merges or a single merge of the final branch.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Internalize quality targets** — Read `qualityDimensions` and define what "done well" looks like for THIS task
2. **Map edge cases to implementation plan** — Read `edgeCases`/`failureModes`; for each, decide HOW it will be handled before coding
3. **Choose your approach** — State assumptions, consider 2-3 approaches with tradeoffs, pick the best, document in progress.txt
4. **After coding, self-critique** — "Does this satisfy every qualityDimensions constraint? Every edge case? Is it idiomatic and efficient?" — revise before moving on

---

## How to Work

1. Read `tasks/batch-chain-worktrees.json` for your task list
2. Read `tasks/progress.txt` (if exists) for context from previous iterations
3. Read `tasks/long-term-learnings.md` for project patterns (persists across branches)
4. Read `CLAUDE.md` for project conventions
5. Pick the highest-priority eligible task (`passes: false`, all `dependsOn` complete)
6. **Before coding**: Read the task's DO/DO NOT sections, qualityDimensions, and edgeCases. State your approach briefly.
7. **Implement**: Code + tests together in one coherent change
8. **After coding**: Self-critique — check each acceptance criterion, especially negative ones and known-bad discriminators
9. Run quality checks (below)
10. Commit: `feat: CHAIN-xxx completed - [Title]`
11. Output `<completed>CHAIN-xxx</completed>`
12. Append progress to `tasks/progress.txt`

---

## Priority Philosophy

What matters most, in order:

1. **PLAN** — Anticipate edge cases. Consider approaches. Read qualityDimensions first.
2. **FUNCTIONING CODE** — Pragmatic, reliable code that works according to plan
3. **CORRECTNESS** — Self-critique after code. Compiles, type-checks, all tests pass
4. **CODE QUALITY** — Clean code, good patterns, no warnings

**Prohibited outcomes:**

- Tests that only assert "no crash" or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context

---

## Key Context

This feature threads a `start_point` git ref through the system so that batch mode can tell each PRD to branch from the previous PRD's branch instead of HEAD.

**Architecture**: The chain mechanism is purely a git operation — `git worktree add -b <branch> <path> -- <start_point>`. The `--` separator prevents flag injection from malicious branch names.

**Data flow**: `--chain` CLI flag → `chain: bool` in batch.rs → reads `LoopResult.branch_name` after each PRD → sets `LoopRunConfig.chain_base` for next PRD → engine passes `chain_base.as_deref()` to `ensure_worktree(start_point)` → git command.

**Key design decisions**:
- Stop-on-failure: `--chain` causes the batch to stop if any PRD fails (downstream would be garbage)
- Upfront validation: all PRDs must have `branchName` when `--chain` is active
- Chain advancement uses `LoopResult.branch_name` (from DB via engine), not pre-read from JSON
- `--` separator in git args prevents flag injection

### Files to modify

| File | Change |
|------|--------|
| `src/loop_engine/worktree.rs` | Add `start_point: Option<&str>` to `ensure_worktree`, use `--` in git args |
| `src/loop_engine/engine.rs` | Add `chain_base` to `LoopRunConfig`, `branch_name` to `LoopResult` |
| `src/loop_engine/batch.rs` | Add `chain` param, validation, tracking, stop-on-failure, enhanced summary |
| `src/cli/commands.rs` | Add `--chain` flag to `Batch` variant |
| `src/main.rs` | Thread `chain` from CLI to `run_batch` |
| `src/cli/tests.rs` | CLI parsing tests for `--chain` |

### Key functions/types to reuse

- `ensure_worktree()` — `src/loop_engine/worktree.rs:116` — add start_point param
- `LoopRunConfig` — `src/loop_engine/engine.rs:701` — add chain_base field
- `LoopResult` — `src/loop_engine/engine.rs:689` — add branch_name field
- `run_batch()` — `src/loop_engine/batch.rs:197` — add chain param
- `read_branch_name_from_prd()` — `src/loop_engine/status_queries.rs:63` — for upfront validation
- `setup_git_repo_with_file()` — `src/loop_engine/test_utils.rs:188` — for git tests
- `PrdRunResult` — `src/loop_engine/batch.rs:30` — add branch tracking fields

### Callers to preserve compatibility with

- `ensure_worktree` called from `engine.rs:1105` — update to pass `chain_base.as_deref()`
- `ensure_worktree` called from test sites in `worktree.rs` — update to pass `None`
- `LoopRunConfig` constructed in `main.rs:~621` (Run command) — add `chain_base: None`
- `LoopRunConfig` constructed in `batch.rs:~321` — add `chain_base` from tracking
- `run_batch` called from `main.rs:~692` — add `chain` parameter
- All `LoopResult` return sites in `engine.rs` — add `branch_name`

---

## What Good Looks Like vs What Bad Looks Like

### Good implementation patterns:

- `start_point` passed as `--` separated arg: `args.push("--"); args.push(sp);`
- Chain tracking uses authoritative source: `loop_result.branch_name` from engine/DB
- Validation runs once before the loop, not inside it
- `chain=false` is a complete no-op — zero code paths touched
- PrdRunResult carries enough context for a useful summary

### Bad patterns to avoid:

- Missing `--` separator before start_point → flag injection vulnerability
- Using `read_branch_name_from_prd` for chain advancement (pre-read might not match DB)
- Adding chain logic inside engine.rs (engine is plumbing, batch owns the orchestration)
- Validation inside the loop instead of upfront (wastes iterations on partial failures)
- Accidentally touching code paths when `chain=false` (must be zero-overhead)

---

## Smart Task Selection

Tasks have relationship fields:

```json
{
  "touchesFiles": ["src/module/file.rs"],
  "dependsOn": ["CHAIN-001"],
  "synergyWith": ["CHAIN-002"]
}
```

### Selection Algorithm

1. **Filter eligible**: `passes: false` AND all `dependsOn` complete
2. **Check synergy**: Prefer tasks where `synergyWith` contains the previous task's ID
3. **Check file overlap**: Prefer tasks with `touchesFiles` matching previous iteration's files
4. **Fall back**: Pick highest priority (lowest number)

---

## Common Wiring Failures

| Symptom | Cause | Fix |
|---------|-------|-----|
| --chain flag parsed but no effect | chain not threaded to run_batch | Wire through main.rs dispatch |
| start_point ignored silently | Missing from git args | Verify args vec includes start_point |
| Chain doesn't advance | Using pre-read branch name | Use LoopResult.branch_name |
| Phase 2 missing phase 1 commits | start_point not passed to git | Check ensure_worktree call |
| Flag injection possible | Missing -- separator | Verify -- before start_point in args |

---

## Quality Checks (REQUIRED every iteration)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

Fix any failures before committing. Never commit broken code.

---

## Task Files

| File | Purpose |
|------|---------|
| `tasks/batch-chain-worktrees.json` | Task list — read tasks, mark complete |
| `tasks/batch-chain-worktrees-prompt.md` | This prompt (read-only) |
| `tasks/progress.txt` | Progress log — append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings (read first) |

---

## Review Task (REVIEW-001)

When you reach REVIEW-001:

1. Review ALL implementation for quality, security, and integration wiring
2. Verify the full wiring path: CLI → main.rs → batch.rs → engine.rs → worktree.rs → git command
3. Check every acceptance criterion marked "Negative:" — these are the most common failure modes
4. Run full test suite
5. **Review remaining tasks**: Read progress.txt and git log. If implementation changed APIs, data structures, or assumptions, update remaining task descriptions/criteria to reflect reality.
6. If issues found: add FIX-xxx tasks to the JSON file (priority 50-97), commit JSON
7. The loop will pick up new FIX tasks automatically

---

## Progress Report Format

APPEND to `tasks/progress.txt`:

```
## [Date/Time] - [Task ID]
- What was implemented
- Files changed
- **Learnings:** (concise — patterns, gotchas, 1-2 lines each)
---
```

---

## Rules

- **One task per iteration**
- **Commit after each task**
- **Read before writing** — always read files first
- **Minimal changes** — only what's required
- Work on the correct branch: `feat/batch-chain-worktrees`
