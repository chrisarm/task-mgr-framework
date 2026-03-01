# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Learning Curation Phase 1: Soft-Archive Infrastructure** for **task-mgr**.

## Problem Statement

task-mgr's institutional memory system accumulates learnings over time but has no way to maintain quality. After ~306 learnings, stale entries dilute recall quality. Phase 1 adds the soft-archive infrastructure: a `retired_at` column on the `learnings` table, `curate retire` and `curate unretire` CLI commands, and `retired_at IS NULL` filters across all 14 retrieval queries.

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
| --- | --- |
| `tasks/curate-learnings-p1.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/curate-learnings-p1-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings (auto-appended, needs periodic curation) |

When review tasks add new tasks, they modify `tasks/curate-learnings-p1.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `tasks/curate-learnings-p1.json`
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
11. If checks pass, commit with message: `feat: [Story ID] - [Story Title]`
12. Update `tasks/curate-learnings-p1.json` to set `passes: true` for the completed story
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

## Phase 1 Specific Context

### Key Architecture Facts

- **14 queries need `retired_at IS NULL`** across 9 files (see FEAT-002 for full list)
- **5 queries are exempt** (single-record lookups by ID): `get_learning()`, `apply_learning` (2 queries), `get_window_stats`, `refresh_sliding_window`
- **Migration pattern**: follow `src/db/migrations/v7.rs` exactly — static `MIGRATION` struct with version/description/up_sql/down_sql
- **CLI subcommand pattern**: CurateAction enum with `#[derive(Subcommand)]`, nested under `Commands::Curate`
- **Output pattern**: `TextFormattable` trait + `impl_text_formattable!` macro in `handlers.rs`, then `output_result(&result, cli.format)`
- **Command pattern**: `Params` struct → `fn command(conn, params) -> TaskMgrResult<Result>` → dispatch in `main.rs`

### Critical Files Reference

| File | Role |
| --- | --- |
| `src/db/migrations/v7.rs` | Migration pattern to follow |
| `src/db/migrations/mod.rs` | Register migration, bump version |
| `src/cli/commands.rs` | CLI command definitions (clap derive) |
| `src/commands/mod.rs` | Module exports |
| `src/handlers.rs` | TextFormattable impls |
| `src/main.rs` | Command dispatch |
| `src/learnings/crud/read.rs` | get_learning() — EXEMPT from filter |
| `src/learnings/crud/delete.rs` | Pattern for deletion with cascade |
| `src/commands/learnings.rs` | Existing list command to modify |

---

## Quality Checks (REQUIRED)

Run from project root.

```bash
# 1. Format check
cargo fmt --check

# 2. Type check
cargo check

# 3. Linting
cargo clippy -- -D warnings

# 4. Tests
cargo test
```

**If checks fail:**

- Fix the issue (apply linter suggestions unless they conflict with philosophy)
- Re-run all checks
- Do NOT commit broken code

---

## Integration Verification Protocol (CRITICAL)

**New code must be fully wired in.**

### After Implementing New Code, Verify:

#### 1. Export Chain Complete

```bash
# Verify module is exported from parent
Grep: "pub mod curate" in src/commands/mod.rs
# Trace up to crate root
```

#### 2. Registration/Wiring Points

- Routes/dispatch: `Commands::Curate` arm in main.rs?
- Types: imported in handlers.rs?
- Text formatters: `impl_text_formattable!` for all result types?

#### 3. Dead Code Detection

```bash
cargo check 2>&1 | grep -i "unused"
cargo clippy 2>&1 | grep -i "never used"
```

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks can and should add new tasks directly to `tasks/curate-learnings-p1.json` when issues are found. The task-mgr reads the JSON at each iteration start.

### CODE-REVIEW-1 (Priority 13, adds tasks at 14-16)

Focus areas for Phase 1:
1. Count all 14 filtered queries — are ALL present?
2. Count all 5 exempt queries — are NONE filtered?
3. Integer division trap in retire candidate SQL
4. Transaction usage in retire/unretire
5. Error propagation in unretire (per-ID errors)

### REFACTOR-REVIEW-1 (Priority 17, adds tasks at 18-19)

Focus areas:
1. Can the 14 `retired_at IS NULL` additions be centralized? (e.g., shared WHERE fragment)
2. Is the curate module structured for Phase 2/3 extension?
3. Test setup duplication

---

## Error Handling Guidelines

- Never use `unwrap()` in production code
- Use `expect("descriptive message")` for programmer errors
- Use `?` operator with proper `Result` propagation
- Handle `Option::None` explicitly with meaningful defaults or errors

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
- These are curated, categorized learnings that persist across branches

**Write concise learnings** (1-2 lines each):

- GOOD: "`retired_at IS NULL` must be added to FTS5 JOIN as `l.retired_at IS NULL`"
- BAD: "When adding the retired_at filter to the FTS5 query, you need to use the table alias 'l' because the query joins learnings as 'l' with learnings_fts..."

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in `progress.txt`
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Add to JSON and commit: `chore: Add blocker task CLARIFY-001`
4. Output:

```
<promise>BLOCKED</promise>
```

---

## Milestones

Milestones (MILESTONE-xxx) are gate tasks:

1. Check all `dependsOn` tasks have `passes: true`
2. Run verification commands in acceptance criteria
3. Only mark `passes: true` when ALL criteria met

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md` section 8
