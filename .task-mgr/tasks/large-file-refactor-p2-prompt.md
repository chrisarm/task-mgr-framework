# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Phase 2: Large File Decomposition (Tier 2)** for **task-mgr**.

## Problem Statement

Continuing the large file refactor on the `large-file-refactor` branch, Phase 2 decomposes 5 Tier 2 files in `loop_engine/`: claude.rs (1181L), status.rs (1236L), archive.rs (1213L), calibrate.rs (1568L), detection.rs (1013L). Mechanical extraction only — no behavior changes.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing ANY code for a task:

1. **Read `qualityDimensions`** on the task — these define what "good" looks like
2. **Read `edgeCases`/`invariants`/`failureModes`** on TEST-INIT tasks
3. **State assumptions, consider 2-3 approaches**, pick the best
4. **After coding, self-critique**: "Is this correct for all edge cases? Is it idiomatic? Is it efficient?"

---

## Priority Philosophy

1. **PLAN** - Map call graphs and test associations before moving any code
2. **FUNCTIONING CODE** - Extraction compiles and all existing tests pass
3. **CORRECTNESS** - No orphaned tests, no broken imports, identical test count
4. **CODE QUALITY** - Module doc comments, pub(crate) visibility, alphabetical mod declarations
5. **POLISH** - Clean up unused imports in source file after extraction

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
| `tasks/large-file-refactor-p2.json` | **Task list** - Read tasks, mark complete, add new tasks |
| `tasks/large-file-refactor-p2-prompt.md` | This prompt file (read-only) |
| `tasks/progress.txt` | Progress log - append findings and learnings |
| `tasks/long-term-learnings.md` | Curated learnings by category (read first) |
| `tasks/learnings.md` | Raw iteration learnings |

---

## Your Task

1. Read the task list at `tasks/large-file-refactor-p2.json`
2. Read the progress log at `tasks/progress.txt` (if exists)
3. Read `tasks/long-term-learnings.md` for curated project patterns
4. Read `CLAUDE.md` for project patterns
5. Verify you're on the `large-file-refactor` branch
6. **Select the best task** using Smart Task Selection below
7. **Pre-implementation**: read qualityDimensions, state assumptions, consider approaches
8. **Implement** the extraction following the Extraction Protocol below
9. **Self-critique**: test count unchanged? clippy clean? imports clean?
10. Run quality checks
11. Commit: `refactor: FULL-STORY-ID-completed - [Story Title]`
12. Output `<completed>FULL-STORY-ID</completed>`
13. Append progress to `tasks/progress.txt`

---

## Smart Task Selection

1. **Filter eligible**: `passes: false` AND all `dependsOn` complete
2. **Check synergy**: Prefer tasks sharing `synergyWith` with previous task
3. **Check file overlap**: Prefer tasks with `touchesFiles` matching previous
4. **Avoid conflicts**: Skip tasks in `conflictsWith` of recently completed
5. **Fall back**: Pick highest priority (lowest number)

---

## Extraction Protocol

### Before Moving Code

1. **Record baseline**: `cargo test 2>&1 | grep 'test result'`
2. **Map the call graph**: What does each function call? What calls it?
3. **Identify test associations**: Which tests test which functions?
4. **Check for private helper dependencies**: Must any helpers move too?
5. **Check for struct/type dependencies**: Do types need to move or be shared?
6. **Check external callers**: `grep -rn 'module_name::' src/` — what imports must change?

### Moving Code

1. Create new module file with `//!` doc comment
2. Move functions — adjust visibility to `pub(crate)` as needed
3. Move associated tests to `#[cfg(test)]` in new file
4. Add `pub mod new_module;` to `mod.rs` (alphabetical)
5. Add `use` imports in original file
6. Update external callers' import paths
7. Remove moved code from original file
8. Clean up unused `use` in original file

### After Moving Code

1. `cargo build` — must succeed
2. `cargo test 2>&1 | grep 'test result'` — must match baseline
3. `cargo clippy -- -D warnings` — zero warnings
4. `cargo fmt --check` — clean

---

## Key Architecture Notes

### Phase 1 Context

Phase 1 already extracted from engine.rs, prompt.rs, env.rs. These modules now exist:
- `git_reconcile.rs`, `output_parsing.rs`, `prd_reconcile.rs` (from engine.rs)
- `prompt_sections/` subdirectory (from prompt.rs)
- `worktree.rs` (from env.rs)

### claude.rs — Watchdog Extraction (REFACTOR-006)

**Watchdog functions** → `watchdog.rs`:
- `TimeoutConfig` struct + `from_difficulty` (line 42) — pub, used by engine.rs
- `watchdog_loop` (lines 290-349 unix, 351-384 non-unix) — spawned as thread by spawn_claude
- `kill_process_group` (line 248) — called by watchdog_loop
- `exit_code_from_status` (line 226) — called by spawn_claude after child exits

**Stays in claude.rs**:
- `spawn_claude` (line 93) — the public entry point
- Two `#[cfg(unix)]` inline blocks inside spawn_claude (setpgid line 124, tcsetpgrp line 159)

**External callers** (no changes needed for spawn_claude callers):
- `engine.rs` — calls `spawn_claude` AND `TimeoutConfig::from_difficulty` (must update TimeoutConfig import)
- `curate/enrich.rs`, `learnings/ingestion/mod.rs` — call `spawn_claude` only

**Test module** starts at line 372.

### status.rs — Query/Render Split (REFACTOR-007)

**Query functions** → `status_queries.rs` (lines 198-421):
- `read_task_prefix_from_prd`, `query_project_info`, `query_dashboard_task_counts`
- `query_pending_tasks`, `query_distinct_prefixes`, `read_active_lock_prefix`
- `read_deadline_info`, `read_single_deadline`, `prd_basename_from_path`

**Render functions** → `status_display.rs` (lines 432-566):
- `format_text` (pub — called from handlers.rs), `format_remaining`, `progress_bar`, `status_icon`

**Stays in status.rs**: `show_status` orchestrator + struct definitions (DashboardResult, etc.)

**External callers**: main.rs (show_status), engine.rs (read_task_prefix_from_prd), handlers.rs (DashboardResult, format_text)

**Test module** starts at line 568.

### archive.rs — Documentation + Minor Extraction (REFACTOR-008)

**Key finding**: `extract_learnings_from_progress` (archive.rs) and `extract_learnings_from_output` (learnings/ingestion/) are NOT duplicates:
- archive.rs: parses formatted markdown from progress.txt → appends to learnings.md file
- ingestion: LLM-based extraction from raw Claude output → stores in DB

Primary task: document this finding. Optional: extract `format_text` (~65 lines) if warranted.

**External callers**: main.rs, branch.rs, handlers.rs

**Test module** starts at line 458.

### calibrate.rs — Math Extraction (REFACTOR-009)

**Pure math** → `calibrate_math.rs`:
- `compute_correlation` (line 365) — point-biserial correlation, takes `&[TaskOutcome]`
- `adjust_weight` (line 405) — `default * (1 + correlation * factor)`
- `clamp_weight` (line 63), `clamp_negative_weight` (line 70)

**Stays in calibrate.rs**: All DB functions, orchestrators, SelectionWeights struct, TaskOutcome struct

**External callers**: selection.rs (load_dynamic_weights), engine.rs (recalibrate_weights, SelectionWeights)

**Test module** starts at line 423.

### detection.rs — Pragmatic Decision (REFACTOR-010)

**Exit-code classifier**: `categorize_crash` (line 100, ~18 lines) — the only pure exit-code function
**Everything else**: output-string analysis (extract_reorder_task_id, is_rate_limited, is_task_reported_already_complete, analyze_output)

Given the tiny size of the exit-code function, the agent should make a pragmatic judgment call: extract to `exit_codes.rs` for consistency, or organize in-place with section markers. Either satisfies the AC.

**External callers**: engine.rs (analyze_output, is_task_reported_already_complete)

**Test module** starts at line 144.

---

## Quality Checks (REQUIRED)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

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

## Milestones

- MILESTONE-1 (priority 20): All 5 extractions done, code-reviewed, refactor-reviewed
- MILESTONE-FINAL (priority 99): All verification passed, ready to continue on branch

---

## Important Rules

- Work on **ONE extraction per iteration**
- **Commit frequently** after each passing extraction
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files before modifying
- **Minimal changes** — only move code, don't refactor logic
