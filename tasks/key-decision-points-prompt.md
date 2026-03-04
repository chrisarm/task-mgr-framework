# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Key Decision Points** for **task-mgr**.

## Problem Statement

The loop engine has no mechanism for Claude to flag important architectural decisions discovered during implementation. This feature adds:
1. A `<key-decision>` XML tag that Claude can emit during any iteration
2. DB storage, session-end prompting, and cross-session persistence for these decisions
3. PRD skill guidance to prompt users on architectural forks during PRD creation

---

## How to Work

1. Read `tasks/key-decision-points.json` for your task list
2. Read `tasks/progress.txt` (if exists) for context from previous iterations
3. Read `CLAUDE.md` for project conventions
4. Pick the highest-priority eligible task (`passes: false`, all `dependsOn` complete)
5. **Before coding**: Read the task's DO/DO NOT sections and edge cases. State your approach briefly.
6. **Implement**: Code + tests together in one coherent change
7. **After coding**: Self-critique — check each acceptance criterion, especially negative ones
8. Run quality checks (below)
9. Commit: `feat: TASK-ID-completed - [Title]`
10. Output `<completed>TASK-ID</completed>`
11. Append progress to `tasks/progress.txt`

---

## Key Context

This project is a Rust CLI tool (`task-mgr`) that manages autonomous AI agent loop tasks with SQLite.

### Files to modify

| File | Purpose |
|------|---------|
| `src/loop_engine/config.rs` | Add KeyDecision + KeyDecisionOption structs |
| `src/loop_engine/detection.rs` | Add extract_key_decisions() parser |
| `src/db/migrations/v12.rs` | New migration for key_decisions table |
| `src/db/migrations/mod.rs` | Register v12, bump schema version |
| `src/db/schema/key_decisions.rs` | DB CRUD operations |
| `src/db/schema/mod.rs` | Register key_decisions module |
| `src/loop_engine/engine.rs` | Wire extraction, session-end prompt, session-start resurface |
| `src/loop_engine/prompt.rs` | Add key-decision instructions to prompt builder |
| `.claude/commands/prd.md` | Add Step 4.7 for architectural decision points |

### Key functions/types to reuse

- `extract_reorder_task_id()` in `src/loop_engine/detection.rs:71-86` — pattern for XML tag extraction
- `IterationOutcome`, `CrashType` in `src/loop_engine/config.rs:161-181` — pattern for data structs
- `v11::MIGRATION` in `src/db/migrations/v11.rs` — pattern for migrations
- `build_prompt()` in `src/loop_engine/prompt.rs:110` — integration point for prompt sections
- `run_loop()` in `src/loop_engine/engine.rs:639` — main loop with integration points
- Stdin reading pattern in `engine.rs:1517-1519` — for interactive prompting
- `TaskMgrError::DatabaseError` — for DB error wrapping

### Callers to preserve compatibility with

- `analyze_output()` in detection.rs — must NOT be modified
- `IterationOutcome` enum — must NOT gain new variants for this feature
- `IterationResult` struct — can add optional fields but existing fields unchanged
- `run_loop()` flow — insert new steps without breaking existing step ordering
- `build_prompt()` — add trimmable sections only, don't change critical sections

---

## What Good Looks Like vs What Bad Looks Like

### Good implementation patterns:
- Simple string-based XML parsing (find/split), matching `extract_reorder_task_id()` style
- Parameterized SQL queries for all DB operations
- `map_err(|e| TaskMgrError::DatabaseError(e))` for rusqlite errors
- Trimmable (not critical) prompt sections for nice-to-have context
- Graceful degradation: DB errors → log warning, continue
- `derive(Debug, Clone, PartialEq, Eq)` on data structs

### Bad patterns to avoid:
- Adding regex dependency for XML parsing
- Making key-decision a variant of IterationOutcome (it's sideband data)
- Making the key-decision prompt instruction a critical section (it's trimmable)
- Using `unwrap()` anywhere in production code
- Fatal errors for key-decision failures (always non-fatal)
- SQL without parameterized queries
- Forgetting to handle yes_mode (non-interactive) in session-end prompting

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
| `tasks/key-decision-points.json` | Task list — read tasks, mark complete |
| `tasks/key-decision-points-prompt.md` | This prompt (read-only) |
| `tasks/progress.txt` | Progress log — append findings and learnings |

---

## Review Task (REVIEW-001)

When you reach REVIEW-001:

1. Review ALL implementation for quality, security, and integration wiring
2. Verify all new code is reachable from production entry points
3. Check every acceptance criterion marked "Negative:" — these are the most common failure modes
4. Run full test suite
5. If issues found: add FIX-xxx tasks to the JSON file (priority 50-98), commit JSON
6. The loop will pick up new FIX tasks automatically

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
- Work on the correct branch: `feat/key-decision-points`
