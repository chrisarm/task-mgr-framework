# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Loop Engine Reliability Improvements** for **task-mgr**.

## Problem Statement

Analysis of multiple autonomous loop runs surfaced 17 recurring failure patterns. The root causes: (1) no per-task retry limits causing stuck loops, (2) environment failures wasting iterations, (3) unreliable completion detection, (4) research tasks getting wrong model/timeout, (5) inconsistent progress file naming. This PRD addresses all 5 via engine-level changes plus skill/prompt updates.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Internalize quality targets** — Read `qualityDimensions` and define what "done well" looks like for THIS task
2. **Map edge cases to implementation plan** — Read `edgeCases`/`invariants`/`failureModes`; for each, decide HOW it will be handled before coding
3. **Choose your approach** — State assumptions, consider 2-3 approaches with tradeoffs, pick the best, document in progress file
4. **After coding, self-critique** — "Does this satisfy every qualityDimensions constraint? Every edge case? Is it idiomatic and efficient?" — revise before moving on

---

## Priority Philosophy

What matters most, in order:

1. **PLAN** - Anticipate edge cases. Tests verify boundaries work correctly
2. **FUNCTIONING CODE** - Pragmatic, reliable code that works according to plan
3. **CORRECTNESS** - Code compiles, type-checks, all tests pass deterministically
4. **CODE QUALITY** - Clean code, good patterns, no warnings
5. **POLISH** - Documentation, formatting, minor improvements

**Key Principles:**

- **Approach from all sides**: Consider 2-3 approaches with tradeoffs, pick the best, then implement
- **Tests Drive Development**: Write initial tests before implementation to define expected behavior
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

| File                                        | Purpose                                                          |
| ------------------------------------------- | ---------------------------------------------------------------- |
| `tasks/loop-reliability.json`               | **Task list (PRD)** - Read tasks, mark complete, add new tasks   |
| `tasks/loop-reliability-prompt.md`          | This prompt file (read-only)                                     |
| `tasks/progress-99ae54f7.txt`               | Progress log - append findings and learnings (create if missing) |
| `tasks/long-term-learnings.md`              | Curated learnings by category (create if missing)                |
| `tasks/learnings.md`                        | Raw iteration learnings (create if missing)                      |

**File handling**: If `progress-99ae54f7.txt`, `long-term-learnings.md`, or `learnings.md` don't exist, create them with a minimal header before appending. Never crash on missing files.

---

## Your Task

1. Read the PRD at `tasks/loop-reliability.json`
2. Read the progress log at `tasks/progress-99ae54f7.txt` (create if missing)
3. Read `tasks/long-term-learnings.md` for curated project patterns (create if missing)
4. Read `CLAUDE.md` for project patterns
5. Verify you're on the correct branch from PRD `branchName` (and in the correct repo if `externalGitRepo` is set)
6. **Select the best task** using Smart Task Selection below
7. **Pre-implementation review** (before writing code):
   a. Read the task's `qualityDimensions` if present — these define what "good" looks like
   b. Read `edgeCases`, `invariants`, and `failureModes` on TEST-INIT tasks
   c. State your assumptions explicitly — hidden assumptions create bugs
   d. Consider 2-3 implementation approaches with tradeoffs (even briefly), pick the best
   e. For each known edge case, plan how it will be handled BEFORE coding
   f. Document your chosen approach in a brief comment in the progress file
8. **Implement** that single user story, following your chosen approach
9. **Self-critique** (after implementation, before quality checks):
   - Review for correctness, idiomatic style, and performance. Revise if improvements exist
   - Check each `qualityDimensions` constraint: does the code satisfy it?
   - If the implementation can exit early, avoid redundant work, or be simplified — revise now
10. Run quality checks (see below)
11. If checks pass, commit with message: `feat: FULL-STORY-ID-completed - [Story Title]`
    For multiple tasks: `feat: ID1-completed, ID2-completed - [Title]`
12. Output `<completed>FULL-STORY-ID</completed>` — the loop will mark the task done and update the PRD automatically
13. Append progress to `tasks/progress-99ae54f7.txt` (include approach chosen and any edge cases discovered)
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

1. **Filter eligible**: `passes: false` AND all `dependsOn` complete AND `requiresHuman` is not `true`
2. **Check synergy**: Prefer tasks where `synergyWith` contains the previous task's ID
3. **Check file overlap**: Prefer tasks with `touchesFiles` matching previous iteration's files
4. **Avoid conflicts**: Skip tasks in `conflictsWith` of recently completed tasks
5. **Tie-breaker**: If priorities tie, use most file overlap; if still tied, sort by task ID alphabetically
6. **Fall back**: Pick highest priority (lowest number)

### Fast-Path for Simple Tasks

For tasks with `estimatedEffort: "low"` AND fewer than 5 acceptance criteria:
- Skip the "consider 2-3 approaches" step — implement directly
- Skip detailed progress file documentation — one-line summary is sufficient
- Still run quality checks and self-critique

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures used in this feature:

### PRD JSON → Rust structs → DB

```rust
// PrdFile (parse.rs) — top-level PRD fields
prd.default_max_retries  // Option<i32>, JSON key: "defaultMaxRetries"

// PrdUserStory (parse.rs) — per-task fields
story.max_retries     // Option<i32>, JSON key: "maxRetries"

// Resolving max_retries during import (import.rs)
let resolved = story.max_retries
    .unwrap_or(prd.default_max_retries.unwrap_or(3));

// Task struct (models/task.rs) — DB-backed fields
task.max_retries           // i32 (defaults to 3)
task.consecutive_failures  // i32 (defaults to 0)
```

### Stale → NoEligibleTasks rename

```rust
// config.rs — IterationOutcome enum
IterationOutcome::NoEligibleTasks  // was: IterationOutcome::Stale

// progress.rs — format_outcome()
IterationOutcome::NoEligibleTasks => "NoEligibleTasks".to_string()

// engine.rs — all match arms
matches!(result.outcome, IterationOutcome::NoEligibleTasks)
```

### Progress file naming

```rust
// env.rs already does this:
let progress_name = match prefix {
    Some(p) => format!("progress-{}.txt", p),
    None => "progress.txt".to_string(),
};

// branch.rs NEEDS to match:
let progress_name = match task_prefix {
    Some(p) => format!("progress-{}.txt", p),
    None => "progress.txt".to_string(),
};
let progress_path = tasks_dir.join(&progress_name);

// archive.rs NEEDS pattern match:
if file_name == "progress.txt"
    || (file_name.starts_with("progress-") && file_name.ends_with(".txt")) {
    continue;
}
```

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

# 5. Security audit (if available)
cargo audit 2>/dev/null || true
```

**If checks fail:**

- Fix the issue (apply linter suggestions unless they conflict with philosophy)
- Re-run all checks
- Do NOT commit broken code

---

## Common Wiring Failures

| Symptom                                        | Cause                                   | Fix                    |
| ---------------------------------------------- | --------------------------------------- | ---------------------- |
| Code compiles but feature doesn't work         | Not registered in dispatcher/router     | Add to registration    |
| Tests pass but production doesn't use new code | Test mocks bypass real wiring           | Verify production path |
| New config field has no effect                 | Config read but not passed to component | Wire config through    |
| Old behavior persists                          | Conditional still routes to old code    | Update routing logic   |
| "unused import" warning                        | Imported but never called               | Wire call sites        |

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found.

### CODE-REVIEW-1 (Priority 13, adds tasks at 14-16)

**Purpose**: Catch quality, security, and integration/wiring issues.

For each issue found: add CODE-FIX-xxx or WIRE-FIX-xxx task (priority 14-16), add to MILESTONE-1 dependsOn. CODE-FIX tasks MUST include `rootCause` and `exactFix` fields.

### REFACTOR-REVIEW-1/2/3

Same pattern — spawn REFACTOR-N-xxx tasks, add to corresponding milestone dependsOn.

### Task Flow Diagram (TDD)

```
Initial Tests (1-5) ──► Implementation (6-12) ──► CODE-REVIEW-1 (13) ──► REFACTOR-REVIEW-1 (17) ──► MILESTONE-1 (20)
       │                      │                        │                        │
       │                      │                        └─ CODE-FIX-xxx (14-16) ─┘
       │                      │                                                  └─ REFACTOR-1-xxx (18-19) ─┘
       │                      └─ "Make initial tests pass"
       └─ TEST-INIT-xxx: Edge cases + invariants + known-bad discriminators

Comprehensive Tests (25-38) ──► REFACTOR-REVIEW-2 (43) ──► MILESTONE-2 (50)
              │                        │
              └─ TEST-xxx              └─ REFACTOR-2-xxx (44-48) ─┘

Integration (55-65) ──► REFACTOR-REVIEW-3 (70) ──► VERIFY (90) ──► MILESTONE-FINAL (99)
                              │
                              └─ REFACTOR-3-xxx (71-85) ─┘
```

---

## Progress Report Format

APPEND to `tasks/progress-99ae54f7.txt` (create if missing):

```
## [Date/Time] - [Story ID]
- What was implemented
- Files changed
- **Learnings:** (patterns, gotchas)
---
```

---

## Learnings Guidelines

**Read curated learnings first** from `tasks/long-term-learnings.md`.
**Write concise learnings** (1-2 lines each).
**Group related tasks** when reporting.

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in the progress file
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Output `<promise>BLOCKED</promise>`

---

## Milestones

Milestones are **review-and-update checkpoints**. Read the progress file and git log, identify deviations from plan, update remaining tasks.

---

## Important Rules

- Work on **ONE story per iteration**
- **For high-effort tasks** (`estimatedEffort: "high"` or 10+ acceptance criteria): consider using `/ralph-loop`
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md`
