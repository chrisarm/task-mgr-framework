# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Phase 4: Large File Assessment (Tier 4)** for **task-mgr**.

## Problem Statement

Completing the large file refactor on the `large-file-refactor` branch, Phase 4 assesses 17 Tier 4 files (596-912 lines) and the legacy `claude-loop.sh` script. Unlike Phases 1-3 (which were extraction-focused), Phase 4 is primarily **assessment** — most files are expected to be cohesive. Only extract where genuine multi-responsibility exists.

---

## Non-Negotiable Process (Read Every Iteration)

1. **Read `qualityDimensions`** on the task
2. **State assumptions, consider 2-3 approaches**, pick the best
3. **After coding, self-critique**: test count unchanged? clippy clean?

---

## Priority Philosophy

1. **PLAN** - Read each file before judging
2. **FUNCTIONING CODE** - Any extraction compiles, all tests pass
3. **CORRECTNESS** - No orphaned tests, identical test count
4. **CODE QUALITY** - Module doc comments, clean visibility
5. **POLISH** - Clean up unused imports

**Prohibited outcomes:** Orphaned tests, changed signatures, circular deps, dead code warnings, test count decrease.

---

## Task Files

| File | Purpose |
|------|---------|
| `tasks/large-file-refactor-p4.json` | Task list |
| `tasks/large-file-refactor-p4-prompt.md` | This prompt (read-only) |
| `tasks/progress.txt` | Progress log |
| `tasks/long-term-learnings.md` | Curated learnings |

---

## Your Task

1. Read the task list at `tasks/large-file-refactor-p4.json`
2. Read `tasks/progress.txt`, `tasks/long-term-learnings.md`, `CLAUDE.md`
3. Verify you're on the `large-file-refactor` branch
4. Select the best task (Smart Task Selection)
5. Pre-implementation: read qualityDimensions, state assumptions, consider approaches
6. Implement (assessment documentation OR extraction OR EXPLICIT_SKIP)
7. Self-critique: test count? clippy? imports?
8. Quality checks, commit, output `<completed>ID</completed>`

---

## Key Architecture Notes

### Assessment Targets (US-014)

**17 Tier 4 files to assess** — for each, determine: COHESIVE (no action), EXTRACT (with plan), or MONITOR (watch for growth).

#### Likely COHESIVE (no action needed)

| File | Lines | Rationale |
|------|-------|-----------|
| `src/loop_engine/batch.rs` | 680 | Well-scoped batch execution |
| `src/learnings/bandit.rs` | 636 | Pure UCB algorithm |
| `src/commands/review.rs` | 708 | Single command, complex state machine |
| `src/commands/recall.rs` | 685 | Single command, 4 filter strategies |
| `src/commands/history.rs` | 635 | Single aggregation command |
| `src/commands/stats.rs` | 611 | Single statistics command |
| `src/commands/list.rs` | 606 | Single list command |
| `src/commands/irrelevant.rs` | 640 | Single command handler |
| `src/models/learning.rs` | 616 | Entity definition + DB parsing |
| `src/models/run.rs` | 608 | Entity definition + DB parsing |
| `src/models/task.rs` | 608 | Entity definition + DB parsing |
| `src/error.rs` | 661 | Structural — error enum + constructors, expected in Rust |

#### Likely EXTRACT or MONITOR

| File | Lines | Concern |
|------|-------|---------|
| `src/commands/complete.rs` | 912 | **HIGH**: mixes completion, dependency validation, test running, run tracking |
| `src/loop_engine/model.rs` | 622 | 50 functions — cohesive domain but very dense |
| `src/commands/curate/mod.rs` | 736 | Dense core logic, minimal tests |
| `src/commands/run.rs` | 724 | Multiple lifecycle concerns |
| `src/commands/learnings.rs` | 731 | CLI types + business logic + formatting mixed |

#### complete.rs Deep Dive (most likely extraction)

**Production code**: ~397 lines, **Tests**: ~515 lines

**Responsibilities identified:**
- Task completion logic (core responsibility)
- Dependency validation: `get_unsatisfied_deps`, `check_dependencies_satisfied`, `are_dependencies_satisfied`
- Test execution: `check_required_tests_pass` (spawns `cargo test`)
- Run tracking: updating runs table, iteration counts

**Potential extraction**: Dependency validation functions → `dependency_checker.rs` or similar. These are called from engine.rs and other command files.

**External callers**: Grep for `complete::` and `are_dependencies_satisfied` to map all import sites.

### claude-loop.sh Assessment (US-015)

**Known references:**
- `README.md:227` — mentions "replaced the external claude-loop.sh script"
- `docs/INTEGRATION.md:10,249-260` — "Reference Implementation" section
- `docs/ARCHITECTURE.md:29` — describes as "original... 1,455 lines of bash"

**Expected verdict**: DEPRECATED — superseded by Rust loop engine.

---

## Assessment Protocol

### For Each File

1. **Read the file** (at least first 100 lines + test module start)
2. **Count**: total lines, production lines, test lines
3. **List responsibilities**: what distinct things does this file do?
4. **Judge SRP**: Is each responsibility tightly coupled to the others?
5. **Verdict**: COHESIVE / EXTRACT / MONITOR
6. **If EXTRACT**: list functions, target file, external callers
7. **Document** in progress.txt

### Assessment Format (in progress.txt)

```
### [filename] — [VERDICT]
- Lines: total / prod / test
- Responsibilities: [list]
- Rationale: [1-2 sentences]
- Action: No action needed / Extract [functions] to [target] / Monitor for growth
```

---

## Extraction Protocol (if needed)

### Before Moving Code
1. Record baseline: `cargo test 2>&1 | grep 'test result'`
2. Map call graph
3. Identify test associations
4. Check external callers: `grep -rn 'module::' src/`

### Moving Code
1. Create new file with `//!` doc comment
2. Move functions, adjust visibility
3. Move tests
4. Update mod.rs, add imports, update external callers
5. Clean up unused imports

### After Moving Code
1. `cargo build` — succeed
2. `cargo test` — match baseline
3. `cargo clippy -- -D warnings` — zero warnings
4. `cargo fmt --check` — clean

---

## Quality Checks

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
```

---

## Progress Report Format

```
## [Date/Time] - [Story ID]
- What was done (assessment findings or extraction)
- Files changed
- Test count: before=X, after=X
- **Learnings:** (patterns, gotchas)
---
```

---

## Important Rules

- Work on **ONE task per iteration**
- **EXPLICIT_SKIP** is the expected outcome for most files — document rationale
- **Read before judging** — do not assess based on line count alone
- **Commit frequently** after each passing task
- **Minimal changes** — only extract where genuinely warranted
- Assessment quality matters — clear verdicts help future maintainers
