# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Multi-PRD Archive Support** for **task-mgr**.

## Problem Statement

The `task-mgr archive` command hardcodes `prd_metadata WHERE id = 1` and checks ALL tasks globally. When multiple PRDs coexist in the database (common — the DB currently holds 6 PRDs with different `task_prefix` values), it reports "not fully completed" even though individual PRDs have all tasks in terminal states. The archive command needs to iterate all PRDs by prefix and archive completed ones independently, with scoped DB cleanup and per-PRD reporting.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Read `qualityDimensions`** on the task — these define what "good" looks like
2. **Read `edgeCases`/`invariants`/`failureModes`** on TEST-INIT tasks — each must be handled and tested
3. **State assumptions, consider 2-3 approaches**, pick the best
4. **After coding, self-critique**: "Is this correct for all edge cases? Is it idiomatic? Is it efficient?" — revise if improvements exist

---

## Priority Philosophy

What matters most, in order:

1. **PLAN** - Anticipate edge cases. Tests verify boundaries work correctly
2. **FUNCTIONING CODE** - Pragmatic, reliable code that works according to plan
3. **CORRECTNESS** - Code compiles, type-checks, all tests pass deterministically
4. **CODE QUALITY** - Clean code, good patterns, no warnings
5. **POLISH** - Documentation, formatting, minor improvements

**Key Principles:**

- **Tests first**: Write initial tests before implementation to define expected behavior
- **Approach before code**: Consider 2-3 approaches with tradeoffs, pick the best, then implement
- **Self-critique after code**: Review your own implementation for correctness, style, and performance before moving on
- **Quality dimensions explicit**: Read `qualityDimensions` on the task — these define what "good" looks like
- Test boundaries and exceptions—edge cases are where bugs hide
- Handle `Option`/`Result` explicitly; avoid `unwrap()` in production—use `expect()` with messages or proper error propagation
- Implementation goal: make the initial tests pass, then expand coverage

**Prohibited outcomes:**

- Tests that only assert "no crash" or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context

---

## Task Files (IMPORTANT)

These are the files you will read and modify during the loop:

| File | Purpose |
|------|---------|
| `tasks/multi-prd-archive.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/multi-prd-archive-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings (auto-appended, needs periodic curation) |

When review tasks add new tasks, they modify `tasks/multi-prd-archive.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `tasks/multi-prd-archive.json`
2. Read the progress log at `tasks/progress.txt` (if exists)
3. Read `tasks/long-term-learnings.md` for curated project patterns (persists across branches)
4. Read `CLAUDE.md` for project patterns
5. Verify you're on the correct branch from PRD `branchName`
6. **Select the best task** using Smart Task Selection below
7. **Pre-implementation review** (before writing code):
   a. Read the task's `qualityDimensions` if present — these define what "good" looks like
   b. Read `edgeCases`, `invariants`, and `failureModes` on TEST-INIT tasks
   c. State your assumptions explicitly — hidden assumptions create bugs
   d. Consider 2-3 implementation approaches with tradeoffs (even briefly), pick the best
   e. For each known edge case, plan how it will be handled BEFORE coding
   f. Document your chosen approach in a brief comment in `progress.txt`
8. **Implement** that single user story, following your chosen approach
9. **Self-critique** (after implementation, before quality checks):
   - Review for correctness, idiomatic style, and performance. Revise if improvements exist
   - Check each `qualityDimensions` constraint: does the code satisfy it?
   - If the implementation can exit early, avoid redundant work, or be simplified — revise now
10. Run quality checks (see below)
11. If checks pass, commit with message: `feat: FULL-STORY-ID-completed - [Story Title]`
    For multiple tasks: `feat: ID1-completed, ID2-completed - [Title]`
12. Output `<completed>FULL-STORY-ID</completed>` — the loop will mark the task done and update the PRD automatically
13. Append progress to `tasks/progress.txt` (include approach chosen and any edge cases discovered)
14. For TEST-xxx tasks: ensure 80%+ coverage for new methods; use `assert_eq!` for string outputs

---

## Smart Task Selection

Tasks have relationship fields:

```json
{
  "touchesFiles": ["src/module/file.rs"],
  "dependsOn": ["FEAT-001"],
  "synergyWith": ["FEAT-002"],
  "batchWith": [],
  "conflictsWith": []
}
```

### Selection Algorithm

1. **Filter eligible**: `passes: false` AND all `dependsOn` complete
2. **Check synergy**: Prefer tasks where `synergyWith` contains the previous task's ID
3. **Check file overlap**: Prefer tasks with `touchesFiles` matching previous iteration's files
4. **Avoid conflicts**: Skip tasks in `conflictsWith` of recently completed tasks
5. **Tie-breaker**: If priorities tie, choose the one with most file overlap
6. **Fall back**: Pick highest priority (lowest number)

---

## Key Implementation Context

### Primary file: `src/loop_engine/archive.rs`

This is the ONLY file that needs significant modification. All changes are scoped here.

### Key utilities to reuse (DO NOT reimplement):

- `crate::db::prefix::make_like_pattern(prefix)` — builds `"{escaped_prefix}-%"` LIKE pattern
- `crate::db::prefix::escape_like(s)` — escapes LIKE special characters
- `crate::db::open_connection(dir)` — opens SQLite connection with WAL mode
- `strip_branch_prefix()` — already in archive.rs, strips feat/fix/chore/ralph/ prefixes
- `extract_learnings_from_progress()` — already in archive.rs, unchanged
- `append_learnings_to_file()` — already in archive.rs, unchanged

### Database schema reference:

```sql
-- prd_metadata (v9+): multiple rows, AUTOINCREMENT, task_prefix UNIQUE
prd_metadata(id INTEGER PRIMARY KEY AUTOINCREMENT, project TEXT, branch_name TEXT, task_prefix TEXT UNIQUE, ...)

-- prd_files: per-PRD file tracking
prd_files(id, prd_id INTEGER REFERENCES prd_metadata(id) ON DELETE CASCADE, file_path TEXT, file_type TEXT)

-- tasks: IDs are prefixed with task_prefix (e.g., "9c5c8a1d-US-001")
tasks(id TEXT PRIMARY KEY, title, status CHECK(status IN ('todo','in_progress','done','blocked','skipped','irrelevant')), ...)

-- run_tasks: links runs to tasks
run_tasks(id, run_id TEXT, task_id TEXT)

-- global_state: singleton row
global_state(id CHECK(id=1), iteration_counter, last_task_id, last_run_id, ...)
```

### Callers of run_archive (must not break):

- `src/main.rs:711` — `task_mgr::loop_engine::archive::run_archive(&cli.dir, dry_run)`
- `src/loop_engine/branch.rs:79` — `archive::run_archive(dir, false)` — checks `result.archived.is_empty()` and `.len()`

---

## Behavior Modification Protocol

Before implementing any task with `modifiesBehavior: true`:

### 1. Verify Consumer Impact

The PRD identified these consumers — all are OK (no breaking changes):

| File:Line | Usage | Impact |
|-----------|-------|--------|
| `src/main.rs:710-714` | Calls `run_archive`, passes to `output_result` | OK — signature unchanged |
| `src/loop_engine/branch.rs:79-85` | Calls `run_archive`, checks `archived.is_empty()` | OK — field preserved |
| `src/handlers.rs:102-105` | `impl_text_formattable!(ArchiveResult, format_text)` | OK — format_text updated |

### 2. Keep Public Signature Stable

`run_archive(dir: &Path, dry_run: bool) -> TaskMgrResult<ArchiveResult>` — DO NOT CHANGE.

---

## Quality Checks (REQUIRED)

Run from project root:

```bash
# 1. Format check
cargo fmt --check

# 2. Type check
cargo check

# 3. Linting
cargo clippy -- -D warnings

# 4. Tests
cargo test

# 5. Module-specific tests
cargo test --lib loop_engine::archive
```

**If checks fail:**

- Fix the issue (apply linter suggestions unless they conflict with philosophy)
- Re-run all checks
- Do NOT commit broken code

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks are special: they **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found.

### CODE-REVIEW-1 (Priority 13, adds tasks at 14-16)

1. Analyze code against Rust idioms (borrow checker, ownership, lifetimes)
2. Check for: security issues, memory safety, error handling, unwrap() usage
3. Verify quality dimensions were met
4. Verify integration wiring (callers still work)
5. Document findings in `progress.txt`

### REFACTOR-REVIEW-1/2/3

Look for: DRY violations, functions >30 lines, tight coupling, hard-to-change code.

---

## Progress Report Format

APPEND to `tasks/progress.txt`:

```
## [Date/Time] - [Story ID]
- What was implemented
- Files changed
- **Learnings:** (patterns, gotchas)
---
```

---

## Learnings Guidelines

**Read curated learnings first:**

- Before starting work, check `tasks/long-term-learnings.md` for project patterns

**Write concise learnings** (1-2 lines each):

- GOOD: "`make_like_pattern` returns escaped pattern with dash separator — use it, don't hand-roll"
- BAD: Long multi-sentence explanation

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

### Blocked Condition

If blocked: document in `progress.txt`, create clarification task, output `<promise>BLOCKED</promise>`.

---

## Important Rules

- Work on **ONE story per iteration**
- **For high-effort tasks** (`estimatedEffort: "high"` or 10+ acceptance criteria): consider using `/ralph-loop` to iterate within the task
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md`
