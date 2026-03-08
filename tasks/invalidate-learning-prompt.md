# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Invalidate Learning Command** for **task-mgr**.

## Problem Statement

When the Claude subprocess discovers a learning is wrong, it has no way to signal this. Bad learnings persist and get surfaced via UCB ranking, actively misleading future iterations. We need a `task-mgr invalidate-learning <id>` command with two-step degradation: first call downgrades confidence to Low, second call (already Low) sets `retired_at` to soft-archive the learning.

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
| ---- | ------- |
| `tasks/invalidate-learning.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/invalidate-learning-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |

When review tasks add new tasks, they modify `tasks/invalidate-learning.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `tasks/invalidate-learning.json`
2. Read the progress log at `tasks/progress.txt` (if exists)
3. Read `CLAUDE.md` for project patterns
4. Verify you're on the correct branch from PRD `branchName`
5. **Select the best task** using Smart Task Selection below
6. **Pre-implementation review** (before writing code):
   a. Read the task's `qualityDimensions` if present — these define what "good" looks like
   b. Read `edgeCases`, `invariants`, and `failureModes` on TEST-INIT tasks
   c. State your assumptions explicitly — hidden assumptions create bugs
   d. Consider 2-3 implementation approaches with tradeoffs (even briefly), pick the best
   e. For each known edge case, plan how it will be handled BEFORE coding
   f. Document your chosen approach in a brief comment in `progress.txt`
7. **Implement** that single user story, following your chosen approach
8. **Self-critique** (after implementation, before quality checks):
   - Review for correctness, idiomatic style, and performance. Revise if improvements exist
   - Check each `qualityDimensions` constraint: does the code satisfy it?
   - If the implementation can exit early, avoid redundant work, or be simplified — revise now
9. Run quality checks (see below)
10. If checks pass, commit with message: `feat: FULL-STORY-ID-completed - [Story Title]`
    For multiple tasks: `feat: ID1-completed, ID2-completed - [Title]`
11. Output `<completed>FULL-STORY-ID</completed>` — the loop will mark the task done and update the PRD automatically
12. Append progress to `tasks/progress.txt` (include approach chosen and any edge cases discovered)
13. For TEST-xxx tasks: ensure 80%+ coverage for new methods; use `assert_eq!` for string outputs

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

## Key Implementation Details

### Structural Template

Follow `src/commands/apply_learning.rs` exactly:
- Result struct at top (derives Debug, Clone, Serialize)
- Main function
- `format_text()` function
- `#[cfg(test)] mod tests` at bottom

### Critical: Learning struct lacks `retired_at`

The `Learning` model struct does NOT include a `retired_at` field. To check retirement status, use a direct SQL query:

```rust
let retired_at: Option<String> = conn.query_row(
    "SELECT retired_at FROM learnings WHERE id = ?1",
    [learning_id],
    |row| row.get(0),
)?;
```

### Existing Infrastructure (DO NOT recreate)

- `crate::learnings::crud::read::get_learning(conn, id)` — returns `Option<Learning>`
- `crate::learnings::crud::update::edit_learning(conn, id, EditLearningParams)` — updates fields
- `crate::learnings::crud::types::EditLearningParams` — derives Default
- `crate::models::Confidence` — enum: High, Medium, Low
- `crate::TaskMgrError::learning_not_found(id)` — NotFound error
- `crate::TaskMgrError::invalid_state(resource, id, expected, actual)` — InvalidState error
- `crate::learnings::test_helpers::setup_db()` — test database setup
- `crate::learnings::test_helpers::retire_learning(conn, id)` — simulate retirement in tests

### Retire SQL Pattern (from curate/mod.rs)

```rust
conn.execute(
    "UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1",
    [learning_id],
)?;
```

### Wire-up Pattern (from ApplyLearning)

- `src/commands/mod.rs`: `pub mod invalidate_learning;` + `pub use invalidate_learning::{...};`
- `src/cli/commands.rs`: `#[command(name = "invalidate-learning")] InvalidateLearning { learning_id: i64 }`
- `src/main.rs`: Match arm with `LockGuard::acquire`, `open_connection`, `invalidate_learning`, `output_result`
- `src/handlers.rs`: `impl_text_formattable!` or custom impl (check ApplyLearningResult pattern — it has a custom impl adding `\n`)

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

# 5. Full verification command from PRD
cargo check && cargo clippy -- -D warnings && cargo test --lib
```

**If checks fail:**

- Fix the issue (apply linter suggestions unless they conflict with philosophy)
- Re-run all checks
- Do NOT commit broken code

---

## Integration Verification Protocol (CRITICAL)

**New code must be fully wired in.** After implementing:

### Verify:

1. **Export Chain**: `src/commands/invalidate_learning.rs` -> `src/commands/mod.rs` (pub mod + pub use) -> importable from `task_mgr::commands::*`
2. **CLI Variant**: `InvalidateLearning` variant in `Commands` enum with `#[command(name = "invalidate-learning")]`
3. **Dispatch**: Match arm in `main.rs::run()` function
4. **Text Format**: `impl_text_formattable!` or custom `TextFormattable` impl in `handlers.rs`
5. **No Dead Code**: `cargo check 2>&1 | grep -i "unused"` shows nothing for new code

### Trace Entry Point:

```
CLI: task-mgr invalidate-learning 42
  -> clap parse: Commands::InvalidateLearning { learning_id: 42 }
  -> main.rs::run(): match Commands::InvalidateLearning
  -> LockGuard::acquire + open_connection
  -> invalidate_learning(&conn, 42)
  -> output_result(&result, cli.format)
    -> TextFormattable::format_text() or JSON serialize
```

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found.

### CODE-REVIEW-1 (Priority 13)

For each issue: add CODE-FIX-xxx (priority 14-16) + add to MILESTONE-1 dependsOn.

### REFACTOR-REVIEW-1/2/3

For each issue: add REFACTOR-N-xxx + add to corresponding MILESTONE dependsOn.

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

If blocked:

1. Document blocker in `progress.txt`
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Output:

```
<promise>BLOCKED</promise>
```

---

## Milestones

Milestones (MILESTONE-xxx) are gate and prep follow-on tasks:

1. Check all `dependsOn` tasks have `passes: true`
2. Run verification commands in acceptance criteria
3. Only mark `passes: true` when ALL criteria met
4. Update follow-on tasks based on implementation decisions and learnings

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md`
