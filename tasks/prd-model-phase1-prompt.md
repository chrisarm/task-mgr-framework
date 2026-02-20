# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Model Selection & Escalation - Phase 1: Data Layer + Pure Logic** for **task-mgr**.

## Problem Statement

task-mgr's loop engine always spawns Claude with the CLI default model. Users who want cost-optimized runs (haiku for easy tasks, opus for hard ones) have no mechanism to control model selection. Phase 1 adds the data layer (model/difficulty/escalationNote fields through the full parse → DB → export pipeline) and pure model resolution logic (no I/O dependencies).

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
|------|---------|
| `tasks/prd-model-phase1.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/prd-model-phase1-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings (auto-appended, needs periodic curation) |

When review tasks add new tasks, they modify `tasks/prd-model-phase1.json` directly. The loop re-reads this file each iteration.

---

## Your Task

1. Read the PRD at `tasks/prd-model-phase1.json`
2. Read the progress log at `tasks/progress.txt` (if exists)
3. Read `tasks/long-term-learnings.md` for curated project patterns (persists across branches)
4. Read `CLAUDE.md` for project patterns
5. Verify you're on the correct branch from PRD `branchName`
6. **Select the best task** using Smart Task Selection below
7. Implement that **single** user story
8. Run quality checks (see below)
9. If checks pass, commit with message: `feat: [Story ID] - [Story Title]`
10. Update `tasks/prd-model-phase1.json` to set `passes: true` for the completed story
11. Append progress to `tasks/progress.txt`
12. For TEST-xxx tasks: ensure 80%+ coverage for new methods; use `assert_eq!` for string outputs

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

## Reference: Key Patterns to Follow

### Migration Pattern (follow v5.rs exactly)
```rust
// src/db/migrations/v6.rs
use super::Migration;

pub static MIGRATION: Migration = Migration {
    version: 6,
    description: "Add model, difficulty, escalation_note to tasks and default_model to prd_metadata",
    up_sql: r#"
        ALTER TABLE tasks ADD COLUMN model TEXT;
        ALTER TABLE tasks ADD COLUMN difficulty TEXT;
        ALTER TABLE tasks ADD COLUMN escalation_note TEXT;
        ALTER TABLE prd_metadata ADD COLUMN default_model TEXT;
        UPDATE global_state SET schema_version = 6 WHERE id = 1;
    "#,
    down_sql: r#"
        UPDATE global_state SET schema_version = 5 WHERE id = 1;
    "#,
};
```

### TryFrom Row Pattern (follow blocked_at_iteration)
```rust
model: row.get("model").ok().flatten(),
difficulty: row.get("difficulty").ok().flatten(),
escalation_note: row.get("escalation_note").ok().flatten(),
```

### Serde Pattern (follow existing fields)
```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub model: Option<String>,
```

### Parse Struct Pattern (follow task_prefix)
```rust
#[serde(default)]
pub model: Option<String>,
```

### Model Resolution Precedence
```
task.model > (difficulty=="high" → OPUS_MODEL) > prd_default > None
```

### Model Tier Constants
```rust
pub const OPUS_MODEL: &str = "claude-opus-4-6";
pub const SONNET_MODEL: &str = "claude-sonnet-4-6";
pub const HAIKU_MODEL: &str = "claude-haiku-4-5-20251001";
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
```

**If checks fail:**
- Fix the issue (apply linter suggestions unless they conflict with philosophy)
- Re-run all checks
- Do NOT commit broken code

---

## Integration Verification Protocol (CRITICAL)

**New code must be fully wired in.** After implementing:

- [ ] **Exports**: New module/function exported from parent mod.rs?
- [ ] **Imports**: Consuming modules import the new code?
- [ ] **Registration**: New migration registered in MIGRATIONS array?
- [ ] **Config**: New fields wired through from parse → import → export?
- [ ] **No dead code warnings**: `cargo check` shows no unused warnings for new code?

---

## Review Tasks (Add Tasks to JSON for Loop)

Review tasks **CAN AND SHOULD add new tasks directly to the JSON file** when issues are found. The loop re-reads the JSON each iteration.

### CODE-REVIEW-1 (Priority 13)
1. Analyze all implementation code for quality/security
2. **Verify integration wiring** (all new code reachable from production paths)
3. Add CODE-FIX-xxx or WIRE-FIX-xxx tasks for issues found
4. Add each to MILESTONE-1's dependsOn array
5. Commit JSON changes

### REFACTOR-REVIEW-1/2/3
Follow the same pattern: add REFACTOR-x-xxx tasks for issues found, add to next MILESTONE's dependsOn.

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

### Blocked Condition
If blocked: document in progress.txt, create CLARIFY-001 task, output `<promise>BLOCKED</promise>`

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - follow v5.rs for migration, blocked_at_iteration for TryFrom
