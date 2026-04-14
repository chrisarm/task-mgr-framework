# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Soft-Archive for Tasks, Runs, and Key Decisions** for **task-mgr**.

## Problem Statement

`task-mgr archive --all` currently hard-DELETEs rows from `tasks`, `runs`, `run_tasks`, and `key_decisions`, destroying valuable history. This caused an FK constraint crash and loses key architectural decisions. The fix: replace hard-DELETE with soft-archive (`archived_at TEXT DEFAULT NULL` column), filter archived records from active queries, add `--include-archived` flag to `list` and `history`, and change `init --force` to archive before reimporting.

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
2. **PHASE 2 FOUNDATION** - If a more sophisticated solution costs ~1 day now but saves ~2+ weeks post-launch, take that trade-off (1:10 ratio or better). We are pre-launch; foundations compound enormously
3. **FUNCTIONING CODE** - Pragmatic, reliable code that works according to plan
4. **CORRECTNESS** - Code compiles, type-checks, all tests pass deterministically
5. **CODE QUALITY** - Clean code, good patterns, no warnings
6. **POLISH** - Documentation, formatting, minor improvements

**Key Principles:**

- **Approach from all sides**: Consider 2-3 approaches with tradeoffs, pick the best, then implement
- **Phase 2 foundation**: Prefer solutions that lay strong post-launch foundations. If ~1 day of effort now saves ~2+ weeks of rework later (1:10+ ratio), take the sophisticated path
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
| `tasks/soft-archive.json`                   | **Task list (PRD)** - Read tasks, mark complete, add new tasks   |
| `tasks/soft-archive-prompt.md`              | This prompt file (read-only)                                     |
| `tasks/progress-{TASK_PREFIX}.txt`          | Progress log - append findings and learnings (create if missing) |
| `tasks/long-term-learnings.md`              | Curated learnings by category (create if missing with `# Long-Term Learnings` header) |
| `tasks/learnings.md`                        | Raw iteration learnings (create if missing with `# Learnings` header) |

**File handling**: If `progress-{TASK_PREFIX}.txt`, `long-term-learnings.md`, or `learnings.md` don't exist, create them with a minimal header before appending. Never crash on missing files.

---

## Your Task

1. Read the PRD at `tasks/soft-archive.json`
2. Read the progress log at `tasks/progress-{TASK_PREFIX}.txt` (create if missing)
3. Read `tasks/long-term-learnings.md` for curated project patterns (create if missing)
4. Read `CLAUDE.md` for project patterns
5. Verify you're on the correct branch: `feat/soft-archive`
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
13. Append progress to `tasks/progress-{TASK_PREFIX}.txt` (include approach chosen and any edge cases discovered)
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

## Behavior Modification Protocol

Before implementing any task with `modifiesBehavior: true`:

1. Read the `consumerAnalysis` on the task
2. For consumers with `impact: CHANGES`, verify your implementation handles the change correctly
3. For consumers with `impact: OK`, verify they truly are unaffected

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
```

**If checks fail:**

- Fix the issue (apply linter suggestions unless they conflict with philosophy)
- Re-run all checks
- Do NOT commit broken code

---

## Reference Code

### Existing `retired_at` pattern (learnings table — the template to follow)

The learnings table already implements soft-delete. Follow this exact pattern:

**Column**: `retired_at TEXT DEFAULT NULL`

**Retirement operation** (from `src/learnings/crud/retire.rs`):
```sql
UPDATE learnings SET retired_at = datetime('now') WHERE id = ? AND retired_at IS NULL
```

**Filtered queries** (from `src/learnings/retrieval/fts5.rs`):
```sql
SELECT l.id, l.created_at, ...
FROM learnings l
WHERE l.retired_at IS NULL
```

**Unfiltered count** (from `src/commands/learnings.rs`):
```sql
-- Active count (filtered)
SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL
-- Total count (includes retired) — used for stats display
SELECT COUNT(*) FROM learnings
```

### Migration pattern (from v13.rs):
```rust
pub static MIGRATION: Migration = Migration {
    version: 13,
    description: "Add max_retries and consecutive_failures for per-task retry limits",
    up_sql: r#"
        ALTER TABLE tasks ADD COLUMN max_retries INTEGER NOT NULL DEFAULT 3;
        ...
        UPDATE global_state SET schema_version = 13 WHERE id = 1;
    "#,
    down_sql: r#"
        ALTER TABLE tasks DROP COLUMN max_retries;
        ...
        UPDATE global_state SET schema_version = 12 WHERE id = 1;
    "#,
};
```

### Archive function (current clear_prd_data pattern in archive.rs):
The current function uses DELETE. Replace with UPDATE SET archived_at for: run_tasks, key_decisions, runs, tasks. Keep DELETE for: task_relationships, task_files, prd_files, prd_metadata.

### CLI arg pattern (from commands.rs):
```rust
List {
    #[arg(long, value_enum)]
    status: Option<TaskStatusFilter>,
    #[arg(long)]
    file: Option<String>,
    #[arg(long = "task-type")]
    task_type: Option<String>,
    // Add: --include-archived with optional limit
}
```

---

## Common Wiring Failures

| Symptom                                        | Cause                                   | Fix                    |
| ---------------------------------------------- | --------------------------------------- | ---------------------- |
| Code compiles but feature doesn't work         | Not registered in dispatcher/router     | Add to registration    |
| Tests pass but production doesn't use new code | Test mocks bypass real wiring           | Verify production path |
| Archived tasks still appear in list            | Missing `AND archived_at IS NULL`       | Add filter to query    |
| Archive fails with FK constraint               | key_decisions not archived before runs  | Archive children first |
| init --force PK collision                      | Archived tasks still have same IDs      | Hard-delete tasks after archiving runs/key_decisions |

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found.

### CODE-REVIEW-1 (Priority 13, adds tasks at 14-16)

**CRITICAL - Verify all query filters applied:**
- grep for `FROM tasks`, `FROM runs`, `FROM run_tasks`, `FROM key_decisions` across all src/
- Each aggregation/listing query must have `archived_at IS NULL` filter
- Single-ID lookups (WHERE id = ?) do NOT need filtering

### REFACTOR-REVIEW-1/2/3

Same pattern — check for DRY violations, complexity, coupling. Spawn fix tasks if needed.

---

## Progress Report Format

APPEND to `tasks/progress-{TASK_PREFIX}.txt` (create if missing):

```
## [Date/Time] - [Story ID]
- What was implemented
- Files changed
- **Learnings:** (patterns, gotchas)
---
```

---

## Learnings Guidelines

**Write concise learnings** (1-2 lines each):
- GOOD: "`archived_at IS NULL` filter must use table alias in JOINs: `AND t.archived_at IS NULL`"
- BAD: Long paragraph about how SQL filters work

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

### Blocked Condition

If blocked: document in progress file, create clarification task, output `<promise>BLOCKED</promise>`.

---

## Milestones

Milestones are **review-and-update checkpoints**:

1. Check all `dependsOn` tasks have `passes: true`
2. Review completed work in progress file and git log
3. Update remaining tasks based on actual implementation
4. Only mark `passes: true` when all reviews complete

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see `CLAUDE.md`
