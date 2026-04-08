# Claude Code Agent Instructions

You are an autonomous coding agent implementing **curate count subcommand** for **task-mgr**.

## Problem Statement

There's no quick way to see learning statistics (total, active, retired, embedded). Add a `curate count` subcommand.

---

## Priority Philosophy

1. **FUNCTIONING CODE** — Make it work
2. **CORRECTNESS** — Compiles, tests pass
3. **CODE QUALITY** — Clean, no warnings

**Prohibited:**
- Over-engineering a simple count query
- Adding unnecessary abstractions
- Modifying unrelated code

---

## Task Files

| File | Purpose |
|------|---------|
| `tasks/e2e-test-loop.json` | Task list — read tasks, mark complete |
| `tasks/e2e-test-loop-prompt.md` | This prompt (read-only) |
| `tasks/progress-{{TASK_PREFIX}}.txt` | Progress log (create if missing) |

---

## Your Task

1. Read `tasks/e2e-test-loop.json`
2. Read `tasks/progress-{{TASK_PREFIX}}.txt` (create if missing)
3. Read `CLAUDE.md` for project patterns
4. Verify you're on branch `feat/e2e-test-loop`
5. Select the best eligible task (highest priority with all deps met)
6. Implement it, write tests alongside
7. Run quality checks: `cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test`
8. Commit: `feat: STORY-ID-completed - [Title]`
9. Output `<completed>STORY-ID</completed>`
10. Append progress to `tasks/progress-{{TASK_PREFIX}}.txt`

---

## Existing Patterns

- `CurateAction` enum: `src/cli/commands.rs` — add Count variant (no args needed)
- `curate_retire`/`curate_dedup` in `src/commands/curate/mod.rs` — follow for count logic
- Handler dispatch: `src/main.rs` — match on CurateAction::Count
- Types: `src/commands/curate/types.rs` — add CountResult if needed
- Output: `src/commands/curate/output.rs` — add format_count_text

### SQL Queries

```sql
-- Total
SELECT COUNT(*) FROM learnings;

-- Active
SELECT COUNT(*) FROM learnings WHERE retired_at IS NULL;

-- Retired  
SELECT COUNT(*) FROM learnings WHERE retired_at IS NOT NULL;

-- Embedded (active only)
SELECT COUNT(DISTINCT le.learning_id)
FROM learning_embeddings le
JOIN learnings l ON l.id = le.learning_id
WHERE l.retired_at IS NULL;
```

---

## Quality Checks (REQUIRED)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

Fix any failures before committing.

---

## Stop and Blocked

All tasks `passes: true` and milestones pass → `<promise>COMPLETE</promise>`
Blocked → document in progress file, output `<promise>BLOCKED</promise>`

---

## Important Rules

- **ONE story per iteration**
- **Commit after each passing story**
- **Read before writing**
- **Minimal changes** — only what's required
- **This is a small feature** — do not over-engineer
