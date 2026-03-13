# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Key Decisions Management CLI** for **task-mgr**.

## Problem Statement

Users can only interact with key decisions at loop session end (interactive prompt). This feature adds a `task-mgr decisions` subcommand with list/resolve/decline/revert actions and a `/tm-decisions` Claude Code skill for interactive management.

Design constraints:
- No schema migration — the `resolution` and `resolved_at` columns already exist in the DB, we just need to read them
- No prompt injection — resolved decisions are recorded, users act on them in a new loop or via steering.md
- Minimal CLI with positional args (not flags) for the common case

---

## How to Work

1. Read `tasks/key-decisions-mgmt.json` for your task list
2. Read `tasks/progress.txt` (if exists) for context from previous iterations
3. Read `CLAUDE.md` for project conventions
4. Pick the highest-priority eligible task (`passes: false`, all `dependsOn` complete)
5. **Before coding**: Read all files you'll modify. State your approach briefly.
6. **Implement**: Code + tests together in one coherent change
7. **After coding**: Self-critique — check each acceptance criterion, especially negative ones
8. Run quality checks (below)
9. Commit: `feat: TASK-ID - [Title]`
10. Output `<completed>TASK-ID</completed>`
11. Append progress to `tasks/progress.txt`

---

## Key Context

This is a Rust CLI tool using clap derive for argument parsing and rusqlite for database access. The codebase has a consistent layered architecture:

- **DB layer** (`src/db/schema/`) — raw SQL queries, data structs
- **Command layer** (`src/commands/`) — business logic, result structs, format functions
- **CLI layer** (`src/cli/`) — clap enums, argument definitions
- **Entry point** (`src/main.rs`) — dispatch
- **Handlers** (`src/handlers.rs`) — TextFormattable trait registration

### Files to modify

| File | Purpose |
|------|---------|
| `src/db/schema/key_decisions.rs` | Extend StoredKeyDecision, update queries, add functions |
| `src/error.rs` | Add decision_not_found convenience constructor |
| `src/cli/commands.rs` | Add Decisions variant + DecisionAction enum |
| `src/cli/mod.rs` | Add re-export |
| `src/commands/decisions.rs` | **NEW** — result structs, logic, formatting |
| `src/commands/mod.rs` | Add module + re-exports |
| `src/handlers.rs` | Register TextFormattable impls |
| `src/main.rs` | Add dispatch arm |
| `.claude/commands/tm-decisions.md` | **NEW** — skill command |

### Key functions/types to reuse

- `key_decisions_db::resolve_decision(conn, id, resolution)` at `src/db/schema/key_decisions.rs:101` — reuse for both approve and decline
- `key_decisions_db::defer_decision(conn, id)` at `src/db/schema/key_decisions.rs:114` — already exists
- `key_decisions_db::map_row` at `src/db/schema/key_decisions.rs:125` — extend (don't replace)
- `TaskMgrError::task_not_found` at `src/error.rs:215` — pattern for decision_not_found
- `impl_text_formattable!` macro at `src/handlers.rs:61` — register new types
- `output_result` generic at `src/handlers.rs:128` — handles both JSON and text
- Test helper `setup_db()` at `src/db/schema/key_decisions.rs:166` — reuse in new tests
- Test helper `make_decision()` at `src/db/schema/key_decisions.rs:177` — reuse in new tests

### Callers to preserve compatibility with

- `src/loop_engine/engine.rs:1301` — calls `get_all_pending_decisions` at session start
- `src/loop_engine/engine.rs:1908` — calls `get_pending_decisions` at session end
- `src/loop_engine/engine.rs:1922` — calls `defer_decision`
- `src/loop_engine/engine.rs:1978` — calls `resolve_decision`
- `src/loop_engine/engine.rs:1479-1490` — calls `insert_key_decision`

All these callers must continue to compile and work after changes. Do NOT change existing function signatures.

---

## What Good Looks Like vs What Bad Looks Like

### Good implementation patterns:

- `src/commands/review.rs` — closest analogue for the command module structure (result structs, logic functions, format_text, tests)
- `src/cli/commands.rs` WorktreesAction/RunAction — pattern for subcommand enums
- `src/main.rs` Commands::Review dispatch — pattern for read-only vs write dispatch (LockGuard)
- Session-end option parsing at `engine.rs:1970-1987` — reference for letter→index mapping
- Error construction: `TaskMgrError::NotFound { resource_type: "Key Decision".into(), id: id.to_string() }`
- Status validation: `TaskMgrError::InvalidState { resource_type, id, expected, actual }`

### Bad patterns to avoid:

- Using `unwrap()` anywhere in production code — always use `?` or `map_err`
- Changing existing function signatures — extend with new functions instead
- Using `LIKE` queries without `ESCAPE` clause
- Forgetting `#[serde(skip_serializing_if = "Option::is_none")]` on optional fields
- Creating separate test files — tests go in `#[cfg(test)] mod tests` within the source file
- Hardcoding column indices differently in map_row vs SELECT — they MUST match

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
| `tasks/key-decisions-mgmt.json` | Task list — read tasks, mark complete |
| `tasks/key-decisions-mgmt-prompt.md` | This prompt (read-only) |
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
- Work on the correct branch: `feat/key-decisions-mgmt`
