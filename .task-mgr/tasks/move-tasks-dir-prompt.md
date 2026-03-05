# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Move tasks/ under .task-mgr/tasks/** for **task-mgr**.

## Problem Statement

The `tasks/` directory currently sits at the project root alongside `.task-mgr/`. The archive command already writes to `.task-mgr/tasks/archive/` because `run_archive` uses `dir.join("tasks")` where `dir = .task-mgr`. We want ALL task artifacts (JSON, prompts, PRDs, learnings, progress, archive) to live under `.task-mgr/tasks/`. Some functions in `env.rs` incorrectly use `project_dir.join("tasks")` (project root) instead of `db_dir.join("tasks")` (`.task-mgr`).

---

## How to Work

1. Read `.task-mgr/tasks/move-tasks-dir.json` for your task list
2. Read `.task-mgr/tasks/progress.txt` (if exists) for context from previous iterations
3. Read `.task-mgr/tasks/long-term-learnings.md` for project patterns (persists across branches)
4. Read `CLAUDE.md` for project conventions
5. Pick the highest-priority eligible task (`passes: false`, all `dependsOn` complete)
6. **Before coding**: Read the task's DO/DO NOT sections and edge cases. State your approach briefly.
7. **Implement**: Code + tests together in one coherent change
8. **After coding**: Self-critique — check each acceptance criterion, especially negative ones and known-bad discriminators
9. Run quality checks (below)
10. Commit: `feat: MTD-xxx-completed - [Title]`
11. Output `<completed>MTD-xxx</completed>`
12. Append progress to `.task-mgr/tasks/progress.txt`

---

## Key Context

This is a Rust CLI project using SQLite (rusqlite). The codebase follows a pattern where:
- `dir` / `db_dir` = `.task-mgr/` (database directory, default from `--dir` flag)
- `source_root` / `project_dir` = project root (git repo root)
- `tasks_dir` should always be `db_dir.join("tasks")` = `.task-mgr/tasks/`

### Files to modify

| File | Change |
|------|--------|
| `src/loop_engine/env.rs` | `resolve_paths` progress derivation, `ensure_directories` parameter |
| `src/loop_engine/engine.rs` | Caller: pass `db_dir` to `ensure_directories` |
| `src/loop_engine/batch.rs` | `project_root.join("tasks")` → `dir.join("tasks")` |
| `src/cli/tests.rs` | ~24 CLI path + ~9 assertion path updates |
| `src/loop_engine/display.rs` | ~7 test path updates |
| `src/commands/init/tests.rs` | ~5 DB path test updates |
| `.gitignore` | Negation pattern for `.task-mgr/tasks/` |
| `CLAUDE.md` | Doc path updates |
| `.claude/commands/*.md` | Template path updates |

### Key functions/types to reuse

| Function | Location | Notes |
|----------|----------|-------|
| `resolve_paths()` | `src/loop_engine/env.rs:303` | Change progress derivation only |
| `ensure_directories()` | `src/loop_engine/env.rs:393` | Rename param to db_dir |
| `LoopRunConfig` | `src/loop_engine/engine.rs` | Has both `db_dir` and `source_root` |
| `run_batch()` | `src/loop_engine/batch.rs:197` | Has `dir` param (db_dir) |

### Callers to preserve compatibility with

| Caller | Location | Notes |
|--------|----------|-------|
| `run_loop()` | `engine.rs:685` | Calls resolve_paths with source_root |
| `run_loop()` | `engine.rs:705` | Calls ensure_directories — change to db_dir |
| `run_archive()` | `archive.rs:194` | Already uses dir.join("tasks") — correct |
| `init::run()` | `init/mod.rs:449` | Already uses dir.join("tasks") — correct |

---

## What Good Looks Like vs What Bad Looks Like

### Good implementation patterns:

- `db_dir.join("tasks")` for tasks_dir derivation
- `prd_absolute.parent().unwrap_or(project_dir).join(progress_filename)` for progress file
- `.task-mgr/*` + `!.task-mgr/tasks/` for gitignore negation (glob form enables negation)
- Tests that create temp dir structure with `.join("tasks")` under a temp root

### Bad patterns to avoid:

- `project_dir.join("tasks")` or `source_root.join("tasks")` in production code — wrong base dir
- `.task-mgr/` (directory form) in gitignore — negation won't work with directory ignores
- Hardcoded `"tasks/"` in CLI test arguments — should be `".task-mgr/tasks/"`
- Moving tasks.db into .task-mgr/tasks/ — it stays at .task-mgr/tasks.db

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
| `.task-mgr/tasks/move-tasks-dir.json` | Task list — read tasks, mark complete |
| `.task-mgr/tasks/move-tasks-dir-prompt.md` | This prompt (read-only) |
| `.task-mgr/tasks/progress.txt` | Progress log — append findings and learnings |
| `.task-mgr/tasks/long-term-learnings.md` | Curated learnings (read first) |

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

APPEND to `.task-mgr/tasks/progress.txt`:

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
- Work on the correct branch: `feat/move-tasks-dir`
