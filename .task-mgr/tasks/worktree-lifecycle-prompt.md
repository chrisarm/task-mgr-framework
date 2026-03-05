# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Worktree Lifecycle Management (Phase 2)** for **task-mgr**.

## Problem Statement

Git worktree lifecycle is unmanaged: worktrees accumulate on disk after loop runs, early exit leaves orphaned directories, no command reports worktree state, and lock files don't record which worktree/branch is active. This Phase 2 closes those gaps with cleanup on loop/batch exit, early exit cleanup, a `worktrees` command, enhanced lock metadata, session banner hints, multi-PRD status views, and per-PRD progress files.

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
| `tasks/worktree-lifecycle.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/worktree-lifecycle-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings (auto-appended, needs periodic curation) |

When review tasks add new tasks, they modify `tasks/worktree-lifecycle.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `tasks/worktree-lifecycle.json`
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

## Behavior Modification Protocol

Before implementing any task with `modifiesBehavior: true`:

### 1. Verify ANALYSIS Task Status

Check if an `ANALYSIS-xxx` task exists for this change:

- If ANALYSIS exists and `passes: true` → proceed to step 2
- If ANALYSIS exists and `passes: false` → work on ANALYSIS first
- If no ANALYSIS exists → the consumerAnalysis field on the task contains the analysis; verify it before proceeding

### 2. Check Consumer Impact

Read the task's `consumerAnalysis.consumers` field:

- If any consumer has `impact: BREAKS` → update that consumer as part of the task
- If any consumer has `impact: NEEDS_REVIEW` → verify before implementing
- If all consumers have `impact: OK` → proceed with implementation

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

**New code must be fully wired in.** A common failure mode is code that compiles and passes unit tests but is never called in production because it's not properly integrated.

### After Implementing New Code, Verify:

#### 1. Export Chain Complete

```bash
# Verify module is exported from parent
Grep: "pub mod {new_module}" or "pub use {new_module}"
```

#### 2. Registration/Wiring Points

- **Commands**: Added to Commands enum in cli/commands.rs?
- **Dispatch**: Added to main.rs match arm?
- **Formatting**: TextFormattable implemented in handlers.rs?
- **Module**: Registered in commands/mod.rs or loop_engine/mod.rs?

#### 3. Call Site Verification

```bash
# Find ALL places that SHOULD call the new code
Grep: "{function_name}"
```

#### 4. Dead Code Detection

```bash
cargo check 2>&1 | grep -i "unused"
cargo clippy 2>&1 | grep -i "never used"
```

### Integration Verification Checklist

Before marking any implementation task complete:

- [ ] **Exports**: New module/function exported from parent mod.rs?
- [ ] **Imports**: Consuming modules import the new code?
- [ ] **Registration**: New handler/tool/route registered?
- [ ] **Config**: New config fields wired through from CLI to usage?
- [ ] **Call sites**: All places that should use new code actually call it?
- [ ] **No dead code warnings**: `cargo check` shows no unused warnings for new code?

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found. The task-mgr reads the JSON at each iteration start, so newly added tasks will be picked up automatically.

### CODE-REVIEW-1 (Priority 13)

1. Analyze code against Rust idioms (borrow checker, ownership, lifetimes)
2. Check for: security issues, memory safety, error handling, unwrap() usage
3. Verify quality dimensions were met for each task
4. **CRITICAL - Verify Integration Wiring**: all new code exported, registered, called
5. For each issue: add CODE-FIX-xxx or WIRE-FIX-xxx task to JSON (priority 14-16), add to MILESTONE-1 dependsOn

### REFACTOR-REVIEW-1/2/3

Look for: DRY violations, functions >30 lines, tight coupling, clarity issues.
For each issue: add REFACTOR-N-xxx task, add to corresponding MILESTONE dependsOn.

---

## Key Codebase Patterns

### Command Module Pattern
Each command lives in `src/commands/{name}.rs`:
- `{Name}Result` struct (derives Serialize)
- `pub fn {name}(dir, ...) -> TaskMgrResult<{Name}Result>`
- `pub fn format_text(result: &{Name}Result) -> String`
- Registered in `commands/mod.rs` with `pub mod` and `pub use`
- `impl_text_formattable!` macro in `handlers.rs`
- Dispatched from `main.rs` match arm

### Git Command Pattern (env.rs)
```rust
let output = Command::new("git")
    .args(["worktree", "remove", path_str])
    .current_dir(source_root)
    .output()
    .map_err(|e| TaskMgrError::io_error(source_root.display().to_string(), "description", e))?;
```

### Error Pattern
All functions return `TaskMgrResult<T>`. Use `TaskMgrError::InvalidState`, `TaskMgrError::io_error()`, `TaskMgrError::lock_error()`.

### Lock Pattern (lock.rs)
`LockGuard` uses `fs2::FileExt::try_lock_exclusive()`. Lock is released on `Drop`. `write_holder_info()` writes identity to lock file. `read_holder_info()` reads it for error diagnostics.

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

- GOOD: "`parse_worktree_list()` is private — need to make pub(crate) before reusing in worktrees command"
- BAD: "The parse_worktree_list function in env.rs is currently private and when I wanted to use it from the new worktrees command module I had to change its visibility..."

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
- **Check existing patterns** - see `CLAUDE.md`
