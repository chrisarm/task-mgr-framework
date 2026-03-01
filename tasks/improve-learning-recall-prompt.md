# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Improve Task-Related Learning Recall** for **task-mgr**.

## Problem Statement

306 learnings are imported into the task-mgr SQLite database, but task-based recall (`recall --for-task`) is severely underperforming because applicability metadata is missing:

- `applies_to_files`: 110/306 (36%) populated
- `applies_to_task_types`: **0/306 (0%)** — task-type matching completely broken
- `applies_to_errors`: **0/306 (0%)** — error matching completely unused

When the loop runs `recall --for-task FEAT-003`, PatternsBackend can only match 36% of learnings by file, 0% by task type, and 0% by error. The remaining 64% are invisible to task-based recall.

Additionally, the `learn` command and LLM extraction pipeline do not auto-populate applicability metadata, meaning every new learning has the same gap.

## Dual-Repo Setup (IMPORTANT)

This effort spans **TWO git repositories**:

| Track | Repo | Path | Tasks |
|-------|------|------|-------|
| **Track A** (data scripts) | external-ref | `$HOME/projects/external-ref` | FEAT-001 (SQL backfill), FEAT-004 (parser) |
| **Track B** (Rust code) | task-mgr | `linked_projects/task-mgr` → `$HOME/projects/task-mgr` | FEAT-002, FEAT-003, FEAT-005, FEAT-006, all TEST-* tasks |

**When working on Track B tasks**: `cd` to `linked_projects/task-mgr` before running `cargo` commands.
**When working on Track A tasks**: Stay in the external-ref root.
**Commits**: Track B commits go to task-mgr repo. Track A commits go to external-ref repo.

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

| File | Purpose |
|------|---------|
| `tasks/improve-learning-recall.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/improve-learning-recall-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings (auto-appended) |

---

## Your Task

1. Read the PRD at `tasks/improve-learning-recall.json`
2. Read the progress log at `tasks/progress.txt` (if exists)
3. Read `tasks/long-term-learnings.md` for curated project patterns
4. Read `CLAUDE.md` for project patterns
5. Verify you're on the correct branch (`feat/improve-learning-recall`) in the appropriate repo
6. **Select the best task** using Smart Task Selection below
7. **Pre-implementation review** (before writing code):
   a. Read the task's `qualityDimensions` if present
   b. Read `edgeCases`, `invariants`, and `failureModes` on TEST-INIT tasks
   c. State your assumptions explicitly
   d. Consider 2-3 implementation approaches with tradeoffs, pick the best
   e. For each known edge case, plan how it will be handled BEFORE coding
   f. Document your chosen approach in a brief comment in `progress.txt`
8. **Implement** that single user story
9. **Self-critique** (after implementation, before quality checks):
   - Review for correctness, idiomatic style, and performance. Revise if improvements exist
   - Check each `qualityDimensions` constraint: does the code satisfy it?
10. Run quality checks (see below)
11. If checks pass, commit with message: `feat: [Story ID] - [Story Title]`
12. Update `tasks/improve-learning-recall.json` to set `passes: true`
13. Append progress to `tasks/progress.txt`
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

## Key Existing Code References

### task-mgr codebase (`linked_projects/task-mgr/`)

**Auto-populate target files:**
- `src/commands/learn.rs` — `learn(conn, params: LearnParams) -> TaskMgrResult<LearnResult>`
- `src/learnings/ingestion/mod.rs` — `extract_learnings_from_output(conn, output, task_id, run_id) -> TaskMgrResult<ExtractionResult>`

**Key helper functions (already exist, reuse):**
- `src/learnings/retrieval/patterns.rs`:
  - `resolve_task_context(conn, task_id) -> TaskMgrResult<(Vec<String>, Option<String>, Option<String>)>` — returns (task_files, task_prefix, task_error)
  - `extract_task_prefix(task_id) -> String` — strips UUID prefix, extracts type prefix
  - `batch_get_learning_tags(conn, learning_ids) -> HashMap<i64, Vec<String>>`

**Scoring constants (in patterns.rs):**
```rust
const FILE_MATCH_SCORE: i32 = 10;
const TYPE_MATCH_SCORE: i32 = 5;
const ERROR_MATCH_SCORE: i32 = 2;
// NEW: const TAG_CONTEXT_MATCH_SCORE: i32 = 3;
```

**Migration pattern (v3.rs for reference):**
- FTS5 external content mode: `content=learnings, content_rowid=id`
- Triggers: `learnings_ai` (after insert), `learnings_ad` (after delete), `learnings_au` (after update)
- Current schema version: 7 (in `src/db/migrations/mod.rs`)
- Next migration: v8.rs (for FTS5 tag indexing)

**Test patterns:**
```rust
fn setup_db() -> (TempDir, Connection) { ... }
fn setup_db_with_fts5() -> (TempDir, Connection) { ... }
fn create_test_learning(conn, title, content, outcome) -> i64 { ... }
```

### external-ref codebase (project root)

**SQL backfill target:**
- `.task-mgr/tasks.db` — SQLite database with 306 learnings
- `tasks/backfill-learnings.sql` — Script to create

**Parser target:**
- `tasks/parse_learnings.py` — Python script (652 lines)
  - `make_learning()` — needs `applies_to_task_types` parameter
  - `parse_raw()` — derives branch name, needs task type extraction
  - `parse_long_term()` — derives category, needs category-to-type mapping

---

## Quality Checks (REQUIRED)

### For Track B (Rust — run from `linked_projects/task-mgr/`)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

### For Track A (Python/SQL — run from project root)

```bash
# Python (if modifying parse_learnings.py)
cd kb-ingest && ruff check --fix && ruff format && cd ..

# SQL backfill (verify)
sqlite3 .task-mgr/tasks.db < tasks/backfill-learnings.sql
```

**If checks fail:** Fix the issue, re-run. Do NOT commit broken code.

---

## Integration Verification Protocol

After implementing new code, verify:

1. **Exports**: New module/function exported from parent mod.rs?
2. **Registration**: v8 migration registered in `run_migrations()`?
3. **Imports**: Consuming modules import `resolve_task_context()` correctly?
4. **Call sites**: learn.rs and ingestion/mod.rs both call auto-populate?
5. **No dead code**: `cargo check` shows no unused warnings?
6. **Traceable path**: Can trace from CLI `learn` command to auto-populate code?

---

## Behavior Modification Protocol

Before implementing any task with `modifiesBehavior: true`:

### 1. Verify Consumer Analysis

Check `consumerAnalysis` on the task:
- If all consumers have `impact: "OK"` → proceed with implementation
- If any consumer has `impact: "BREAKS"` → the task must be SPLIT
- If any consumer has `impact: "NEEDS_REVIEW"` → verify before implementing

### 2. Verify Semantic Distinctions

If multiple semantic contexts exist (same code, different purposes):
- Each context may need different handling
- Example: LLM-invoked vs auto-invoke tool calls have different caching requirements

---

## Review Tasks (Add Tasks to JSON)

Review tasks **CAN AND SHOULD add new tasks** to `tasks/improve-learning-recall.json` when issues are found. The loop re-reads JSON each iteration.

### CODE-REVIEW-1 (adds CODE-FIX-xxx / WIRE-FIX-xxx at priority 14-16)
### REFACTOR-REVIEW-1 (adds REFACTOR-1-xxx at priority 18-19)
### REFACTOR-REVIEW-2 (adds REFACTOR-2-xxx at priority 40-44)
### REFACTOR-REVIEW-3 (adds REFACTOR-3-xxx at priority 66-80)

For each issue: add task to JSON, add to milestone's `dependsOn`, commit JSON.

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
- Raw iteration learnings in `tasks/learnings.md` are auto-appended and need periodic curation

**Write concise learnings** (1-2 lines each):
- GOOD: "`resolve_task_context()` in retrieval/patterns.rs is pub(crate) — import via full path"
- BAD: "The resolve_task_context function which lives in the retrieval patterns module can be accessed because it has pub(crate) visibility so when you want to use it from commands or ingestion modules you need to import it using the full module path..."

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

### Blocked Condition

If blocked:

1. Document blocker in `progress.txt`
2. Create clarification task
3. Output: `<promise>BLOCKED</promise>`

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
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- **Check existing patterns** — see `CLAUDE.md` section 8
- **Dual repo awareness** — cd to correct repo for each task
