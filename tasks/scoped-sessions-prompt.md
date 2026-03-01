# Claude Code Agent Instructions

You are an autonomous coding agent implementing **PRD-Scoped Sessions & Concurrent Loop Support** for **task-mgr**.

## Problem Statement

`task-mgr loop` has two compounding problems when multiple PRDs coexist in a single database:

1. **No PRD scoping**: Task selection queries ALL tasks regardless of which PRD started the session. When multiple PRDs are imported (via `--append` or `batch`), a loop picks up tasks from the wrong PRD.
2. **Single-session lock**: A global `loop.lock` prevents concurrent sessions, even when targeting different PRDs in separate worktrees.

The solution: scope all task queries by PRD prefix (already in task IDs, e.g., `P1-US-001`), use per-prefix lock files, and add per-session signal files.

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

| File                                | Purpose                                                          |
| ----------------------------------- | ---------------------------------------------------------------- |
| `tasks/scoped-sessions.json`        | **Task list (PRD)** - Read tasks, mark complete, add new tasks   |
| `tasks/scoped-sessions-prompt.md`   | This prompt file (read-only)                                     |
| `tasks/progress.txt`                | Progress log - append findings and learnings                     |
| `tasks/long-term-learnings.md`      | Curated learnings by category (read first)                       |
| `tasks/learnings.md`                | Raw iteration learnings (auto-appended, needs periodic curation) |

When review tasks add new tasks, they modify `tasks/scoped-sessions.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `tasks/scoped-sessions.json`
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
  "dependsOn": ["FEAT-001"], // HARD: Must complete first
  "synergyWith": ["FEAT-002"], // SOFT: Share context
  "batchWith": [], // DIRECTIVE: Do together
  "conflictsWith": [] // AVOID: Don't sequence
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

## Behavior Modification Protocol

Before implementing any task with `modifiesBehavior: true`:

### 1. Verify ANALYSIS Task Status

Check if an `ANALYSIS-xxx` task exists for this change:

- If ANALYSIS exists and `passes: true` → proceed to step 2
- If ANALYSIS exists and `passes: false` → work on ANALYSIS first
- If no ANALYSIS exists → create one and work on it first

### 2. Check Consumer Impact Table

Read `tasks/progress.txt` and find the Consumer Impact Table from the ANALYSIS task:

- If any consumer has `Impact: BREAKS` → the task must be SPLIT
- If any consumer has `Impact: NEEDS_REVIEW` → verify before implementing
- If all consumers have `Impact: OK` → proceed with implementation

### 3. Verify Semantic Distinctions

If the ANALYSIS identified multiple semantic contexts (same code, different purposes):

- Each context may need different handling
- A single change may need to become multiple targeted changes

**If you discover the task should be split:**

1. Do NOT implement the current task
2. Create new split tasks (e.g., FIX-002a, FIX-002b) with specific contexts
3. Update dependencies so original task is replaced by split tasks
4. Commit the JSON changes: `chore: Split [Task ID] for semantic contexts`
5. Mark original task with `passes: true` and note "Split into [new IDs]"

---

## Consumer Analysis Protocol

Before modifying shared code (called from multiple places):

### 1. Identify All Callers

```bash
# Search for direct callers
Grep: function_name
# Search for indirect references (configs, YAML routing)
Grep: "function_name\\|related_config_key"
# Search for tests asserting behavior
Grep: "test.*function_name\\|assert.*expected_value"
```

### 2. Create Consumer Impact Table

Document in progress.txt:

```markdown
## Consumer Impact Table for [Task ID]

| File:Line                | Usage                             | Current Behavior        | Impact | Mitigation                        |
| ------------------------ | --------------------------------- | ----------------------- | ------ | --------------------------------- |
| workflow/executor.rs:456 | Calls function for auto-invoke    | Caches all results      | OK     | No change needed                  |
```

### 3. Decision Matrix

Based on Consumer Impact Table:

- **All OK**: Proceed with single implementation
- **Any BREAKS**: Split task by context, implement each separately
- **NEEDS_REVIEW**: Verify with tests before/after, document assumptions

---

## Quality Checks (REQUIRED)

Run from project root.

### Rust Projects

```bash
# 1. Format check
cargo fmt --check

# 2. Type check
cargo check

# 3. Linting
cargo clippy -- -D warnings

# 4. Tests
cargo test

# 5. Security audit (if available)
cargo audit 2>/dev/null || true
```

**If checks fail:**

- Fix the issue (apply linter suggestions unless they conflict with philosophy)
- Re-run all checks
- Do NOT commit broken code

---

## Error Handling Guidelines

- Never use `unwrap()` in production code
- Use `expect("descriptive message")` for programmer errors
- Use `?` operator with proper `Result` propagation
- Handle `Option::None` explicitly with meaningful defaults or errors

---

## Integration Verification Protocol (CRITICAL)

**New code must be fully wired in.** A common failure mode is code that compiles and passes unit tests but is never called in production because it's not properly integrated.

### After Implementing New Code, Verify:

#### 1. Export Chain Complete

```bash
# Verify module is exported from parent
Grep: "pub mod {new_module}" or "pub use {new_module}"
# Trace up to crate root - every level must re-export
```

#### 2. Registration/Wiring Points

Check that new code is registered where required:

- **Routes/Handlers**: Added to router/dispatcher?
- **Config fields**: Read and passed through?
- **Module declarations**: Added to parent mod.rs?

#### 3. Call Site Verification

```bash
# Find ALL places that SHOULD call the new code
Grep: "{old_function_name}" # If replacing
Grep: "{related_pattern}"   # If adding to existing flow

# Verify new code IS called from those places
Grep: "{new_function_name}"
```

#### 4. Dead Code Detection

```bash
# Check for unused imports/functions
cargo check 2>&1 | grep -i "unused"
cargo clippy 2>&1 | grep -i "never used"
```

### Integration Verification Checklist

Before marking any implementation task complete:

- [ ] **Exports**: New module/function exported from parent mod.rs?
- [ ] **Imports**: Consuming modules import the new code?
- [ ] **Registration**: New handler/tool/route registered?
- [ ] **Config**: New config fields wired through from config source to usage?
- [ ] **Call sites**: All places that should use new code actually call it?
- [ ] **Old code removed**: If replacing, old implementation removed/deprecated?
- [ ] **No dead code warnings**: `cargo check` shows no unused warnings for new code?
- [ ] **Traceable path**: Can trace from entry point to new code?

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks are special: they **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found. The task-mgr reads the JSON at each iteration start, so newly added tasks will be picked up automatically.

**Key principle**: Every milestone must be preceded by a refactor review to ensure code quality improves incrementally.

### CODE-REVIEW-1 (Priority 13, adds tasks at 14-16)

**Purpose**: Catch quality, security, and **integration/wiring** issues before testing phase.

**Execution**:

1. Analyze code against Rust idioms (borrow checker, ownership, lifetimes)
2. Check for: security issues, memory safety, error handling, unwrap() usage
3. **Verify quality dimensions were met**: For each task's `qualityDimensions`, confirm the implementation satisfies correctness, performance, and style constraints
4. **CRITICAL - Verify Integration Wiring**:
   - [ ] All new code is exported and importable
   - [ ] All new handlers/tools/routes are registered
   - [ ] All new config fields are wired through
   - [ ] All call sites that should use new code actually do
   - [ ] No dead code warnings (`cargo check` / `cargo clippy`)
   - [ ] Can trace path from entry point to new code
5. **CRITICAL for this PRD**: Grep for `FROM tasks`, `FROM task_relationships`, `FROM task_files` in ALL source files — verify every query is prefix-scoped when a prefix is available
6. Document findings in `progress.txt`

**Wiring Issues Create WIRE-FIX Tasks**.

**Adding Tasks**:

- For EACH issue found, add a `CODE-FIX-xxx` or `WIRE-FIX-xxx` task to the JSON (priority 14-16)
- **CRITICAL**: Add each CODE-FIX-xxx and WIRE-FIX-xxx to MILESTONE-1's `dependsOn` array
- Commit JSON changes: `chore: CODE-REVIEW-1 - Add CODE-FIX/WIRE-FIX tasks`
- Commit and output `<completed>CODE-REVIEW-1</completed>` once review complete AND all tasks added

**If no issues found**: Output `<completed>CODE-REVIEW-1</completed>` with note "No issues found"

### REFACTOR-REVIEW-1, 2, 3

Follow the same pattern — see task descriptions for specific focus areas.

---

## Feature-Specific Architecture Notes

### Key Files to Understand

| File | Current State | Change |
|------|--------------|--------|
| `src/db/schema/metadata.rs` | `CHECK(id = 1)` singleton | Migration v9 removes constraint |
| `src/commands/init/import.rs` | `INSERT OR REPLACE ... VALUES (1, ...)` | Upsert by task_prefix |
| `src/commands/next/selection.rs` | No prefix filtering on queries | All 4 helpers get prefix param |
| `src/loop_engine/engine.rs` | Global `loop.lock`, unscoped queries | Per-PRD lock, all queries scoped |
| `src/loop_engine/signals.rs` | Global `.stop`/`.pause` | Per-session with global fallback |
| `src/loop_engine/status.rs` | `WHERE id = 1`, private `prefix_filter()` | Query by prefix, move to shared module |

### Existing Code to Reuse

- `read_task_prefix_from_prd()` at `src/loop_engine/status.rs:138` — reads taskPrefix from PRD JSON
- `prefix_filter()` at `src/loop_engine/status.rs:276` — move to shared `src/db/prefix.rs`
- `LockGuard::acquire_named()` at `src/db/lock.rs:62` — already supports custom lock names
- `IterationParams.task_prefix` at `src/loop_engine/engine.rs:108` — field exists, unused by selection

### Query Scoping Pattern

Every query that touches `tasks`, `task_relationships`, or `task_files` must use:

```rust
use crate::db::prefix::{prefix_and, prefix_where};

// For queries with existing WHERE clause:
let (prefix_clause, prefix_param) = prefix_and(task_prefix);
let sql = format!("SELECT ... FROM tasks WHERE status = 'todo' {prefix_clause}");
// Bind prefix_param if Some

// For queries without WHERE clause:
let (prefix_clause, prefix_param) = prefix_where(task_prefix);
let sql = format!("SELECT ... FROM tasks {prefix_clause}");
```

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

- GOOD: "`db::prefix::prefix_and()` returns empty string when prefix is None — safe to format! into SQL"
- BAD: Long explanation of how prefix filtering works...

**Group related tasks** when reporting:

- Instead of separate entries for FEAT-001, FEAT-002, FEAT-003
- Write: "FEAT-001 through FEAT-003: Implemented prefix utility and migration"

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
4. Milestones ensure code review and refactor review happen before proceeding

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md` section 8
