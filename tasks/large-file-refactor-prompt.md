# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Phase 1: Large File Decomposition** for **task-mgr**.

## Problem Statement

The `loop_engine/` subsystem has files ranging from 600–4525 lines with multiple responsibilities. This refactor mechanically extracts functions into focused single-responsibility modules. No behavior changes — only code moves between files, `mod.rs` gets updated, and imports are adjusted.

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

1. **PLAN** - Map call graphs and test associations before moving any code
2. **FUNCTIONING CODE** - Extraction compiles and all existing tests pass
3. **CORRECTNESS** - No orphaned tests, no broken imports, identical test count
4. **CODE QUALITY** - Module doc comments, pub(crate) visibility, alphabetical mod declarations
5. **POLISH** - Clean up unused imports in source file after extraction

**Key Principles:**

- Move complete call chains together — if A calls B, extract both to same module
- Tests move with their tested functions — never orphan a test
- Verify `cargo test` count before and after each extraction
- Use `pub(crate)` for internal APIs, `pub` only for cross-crate visibility
- Follow existing subdirectory pattern (`commands/next/`, `commands/curate/`)

**Prohibited outcomes:**

- Orphaned tests (test exists but tested function moved elsewhere)
- Changed function signatures (parameter types, return types, lifetimes) — visibility adjustments to `pub(crate)` are required and expected
- Circular module dependencies
- Dead code warnings for moved functions
- Test count decrease

---

## Task Files (IMPORTANT)

| File | Purpose |
|------|---------|
| `tasks/large-file-refactor-p1.json` | **Task list (PRD)** - Read tasks, mark complete, add new tasks |
| `tasks/large-file-refactor-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings (auto-appended) |

---

## Your Task

1. Read the PRD at `tasks/large-file-refactor-p1.json`
2. Read the progress log at `tasks/progress.txt` (if exists)
3. Read `tasks/long-term-learnings.md` for curated project patterns (persists across branches)
4. Read `CLAUDE.md` for project patterns
5. Verify you're on the correct branch: `large-file-refactor`
6. **Select the best task** using Smart Task Selection below
7. **Pre-implementation review** (before writing code):
   a. Read the task's `qualityDimensions` if present
   b. Read `edgeCases`, `invariants`, and `failureModes` on TEST-INIT tasks
   c. State your assumptions explicitly
   d. Consider 2-3 approaches for the extraction (e.g., flat file vs subdirectory, visibility choices)
   e. For each known edge case, plan how it will be handled BEFORE coding
   f. Document your chosen approach in `progress.txt`
8. **Implement** that single extraction
9. **Self-critique** (after implementation):
   - Did test count change? Run `cargo test 2>&1 | grep 'test result'`
   - Any dead code warnings? Run `cargo clippy -- -D warnings`
   - Any formatting issues? Run `cargo fmt --check`
   - Are all imports clean? No unused `use` statements?
10. Run quality checks (see below)
11. If checks pass, commit: `refactor: FULL-STORY-ID-completed - [Story Title]`
12. Output `<completed>FULL-STORY-ID</completed>`
13. Append progress to `tasks/progress.txt`

---

## Smart Task Selection

Tasks have relationship fields:

```json
{
  "touchesFiles": ["src/module/file.rs"],
  "dependsOn": ["REFACTOR-001"],
  "synergyWith": ["REFACTOR-002"],
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

## Extraction Protocol (For Each REFACTOR-xxx Task)

### Before Moving Code

1. **Record baseline**: `cargo test 2>&1 | grep 'test result'` — save count
2. **Map the call graph**: For each function to extract, list what it calls and what calls it
3. **Identify test associations**: Which test functions test the functions being moved?
4. **Check for private helper dependencies**: Are there private helpers that must move too?
5. **Check for struct/type dependencies**: Do any types need to move or be shared?

### Moving Code

1. Create the new module file with `//!` doc comment
2. Move functions — adjust visibility to `pub(crate)` if they were private but now need cross-module access
3. Move associated tests to `#[cfg(test)]` in the new file
4. Add `pub mod new_module;` to `mod.rs` (alphabetical order)
5. Add `use super::new_module::{...}` in the original file
6. Update any external callers (check with grep)
7. Remove moved code from original file
8. Clean up unused `use` imports in original file

### After Moving Code

1. `cargo build` — must succeed
2. `cargo test 2>&1 | grep 'test result'` — must match baseline
3. `cargo clippy -- -D warnings` — zero warnings
4. `cargo fmt --check` — clean formatting

---

## Quality Checks (REQUIRED)

```bash
# 1. Format check
cargo fmt --check

# 2. Type check
cargo check

# 3. Linting
cargo clippy -- -D warnings

# 4. Tests (compare count to baseline)
cargo test

# 5. Verify no dead code
cargo check 2>&1 | grep -i "unused"
```

**If checks fail:** Fix the issue, re-run all checks. Do NOT commit broken code.

---

## Key Architecture Notes

### Current Module Pattern

- `src/loop_engine/mod.rs` has 23 `pub mod` declarations (alphabetical)
- Constants (`STOP_FILE`, `PAUSE_FILE`, etc.) and `sanitize_error_tokens()` live in `mod.rs`
- Tests are in inline `#[cfg(test)]` modules within each file
- Complex modules use subdirectories: `commands/next/`, `commands/curate/`, `learnings/retrieval/`

### engine.rs Function Groups (for extraction)

**Git Reconciliation** → `git_reconcile.rs`:
- `reconcile_external_git_completions` (line 2015) — calls `contains_task_id`, `update_prd_task_passes`, `complete_cmd::complete`
- `check_git_for_task_completion` (line 2174) — calls `contains_task_id`
- `contains_task_id` (line 2140) — pure string logic, 12 tests

**Output Parsing** → `output_parsing.rs`:
- `strip_task_prefix` (line 1913) — pure string logic
- `parse_completed_tasks` (line 1927) — pure XML tag parsing, 6 tests
- `check_output_for_task_completion` (line 1955) — pure string check
- `scan_output_for_completed_tasks` (line 1969) — uses DB (rusqlite::Connection)

**PRD Reconciliation** → `prd_reconcile.rs`:
- `read_prd_metadata` (line 1549) — DB query
- `update_prd_task_passes` (line 1723) — JSON file manipulation, calls `strip_task_prefix`
- `mark_task_done` (line 1789) — calls `update_prd_task_passes`, `complete_cmd::complete`
- `reconcile_passes_with_db` (line 1811) — DB + JSON, calls `complete_cmd::are_dependencies_satisfied`
- `hash_file` (line 2227) — pure file hash

**None of the engine.rs extraction functions (above) are called from outside engine.rs** — they are all module-private (`fn`). This does NOT apply to env.rs/worktree extractions below — those have external callers that must be updated.

### prompt.rs Section Builders (for extraction)

**Learnings** → `prompt_sections/learnings.rs`:
- `build_learnings_section` (line 531) + `truncate_to_budget` (line 690)
- `record_shown_learnings` (line 349) — DB side-effect

**Synergy** → `prompt_sections/synergy.rs`:
- `build_synergy_section` (line 429) + `get_synergy_tasks_in_run` (line 464)
- `resolve_synergy_cluster_model` (line 631, pub) + `get_synergy_partner_models` (line 656)

**Dependencies** → `prompt_sections/dependencies.rs`:
- `build_dependency_section` (line 384) + `get_completed_dependencies` (line 408)

**Escalation** → `prompt_sections/escalation.rs`:
- `build_escalation_section` (line 611) + `load_escalation_template` (line 592, pub)

**Shared types staying in prompt.rs**: `PromptResult`, `BuildPromptParams` — imported by engine.rs.

### env.rs Worktree Functions (for extraction)

**Worktree cluster** → `worktree.rs`:
- `cleanup_empty_dir` (line 15), `sanitize_branch_name` (line 183), `compute_worktree_path` (line 196)
- `is_inside_worktree` (line 210), `parse_worktree_list` (line 233)
- `ensure_worktree` (line 280), `remove_worktree` (line 498)
- ~35 tests (lines 2020-2688)

**External callers to update**: engine.rs, batch.rs, commands/worktrees.rs

---

## Progress Report Format

APPEND to `tasks/progress.txt`:

```
## [Date/Time] - [Story ID]
- What was extracted
- Files changed
- Test count: before=X, after=X
- **Learnings:** (patterns, gotchas)
---
```

---

## Learnings Guidelines

**Read curated learnings first** — check `tasks/long-term-learnings.md`.

**Write concise learnings** (1-2 lines each):
- GOOD: "Private fns extracted to new module must become pub(crate) — adjust visibility at move time"
- BAD: Long multi-sentence explanation of Rust module visibility rules

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:
1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

### Blocked Condition

If blocked (circular dependency discovered, can't move function cleanly):
1. Document blocker in `progress.txt`
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Output `<promise>BLOCKED</promise>`

---

## Milestones

MILESTONE-1 (priority 20): All 5 extractions done, code-reviewed, refactor-reviewed.
MILESTONE-FINAL (priority 99): All verification passed, ready for merge.

---

## Important Rules

- Work on **ONE extraction per iteration**
- **Commit frequently** after each passing extraction
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files before modifying
- **Minimal changes** — only move code, don't refactor logic
- **Check existing patterns** — see `CLAUDE.md` and `src/commands/next/` for module structure examples
