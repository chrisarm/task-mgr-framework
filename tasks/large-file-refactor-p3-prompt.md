# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Phase 3: Large File Decomposition (Tier 3)** for **task-mgr**.

## Problem Statement

Continuing the large file refactor on the `large-file-refactor` branch, Phase 3 addresses 3 Tier 3 files: oauth.rs (857L), main.rs (836L), signals.rs (784L). Exploration revealed the PRD's assumptions were partially wrong — these tasks are smaller than anticipated.

---

## Non-Negotiable Process (Read Every Iteration)

1. **Read `qualityDimensions`** on the task
2. **State assumptions, consider 2-3 approaches**, pick the best
3. **After coding, self-critique**: test count unchanged? clippy clean?

---

## Priority Philosophy

1. **PLAN** - Map call graphs before moving code
2. **FUNCTIONING CODE** - Compiles, all tests pass
3. **CORRECTNESS** - No orphaned tests, identical test count
4. **CODE QUALITY** - Module doc comments, clean visibility
5. **POLISH** - Clean up unused imports

**Prohibited outcomes:** Orphaned tests, changed signatures, circular deps, dead code warnings, test count decrease.

---

## Task Files

| File | Purpose |
|------|---------|
| `tasks/large-file-refactor-p3.json` | Task list |
| `tasks/large-file-refactor-p3-prompt.md` | This prompt (read-only) |
| `tasks/progress.txt` | Progress log |
| `tasks/long-term-learnings.md` | Curated learnings |

---

## Your Task

1. Read the task list at `tasks/large-file-refactor-p3.json`
2. Read `tasks/progress.txt`, `tasks/long-term-learnings.md`, `CLAUDE.md`
3. Verify you're on the `large-file-refactor` branch
4. Select the best task (Smart Task Selection)
5. Pre-implementation: read qualityDimensions, state assumptions, consider approaches
6. Implement the extraction (or document EXPLICIT_SKIP)
7. Self-critique: test count? clippy? imports?
8. Quality checks, commit, output `<completed>ID</completed>`

---

## Key Architecture Notes

### oauth.rs — Token Storage vs Refresh (REFACTOR-011)

**Critical finding:** oauth.rs has NO browser/PKCE/callback server code. The PRD's 3-file split is not applicable.

**Production code is only ~197 lines** (7 functions). The remaining ~660 lines are tests.

**Functions:**
- **Storage**: `credentials_path` (L49), `read_credentials` (L60), `write_credentials_atomic` (L158, private), `is_token_expiring` (L68)
- **Refresh flow**: `refresh_token` (L84), `ensure_valid_token` (L129)
- **Utility**: `sanitize_oauth_error` (L193, private)

**Types**: `Credentials` struct (L26), `TokenResult` enum (L37)

**External callers**:
- engine.rs: `ensure_valid_token`
- usage.rs: `credentials_path`, `read_credentials`, `is_token_expiring`, `refresh_token`

**Test module**: L197–857 (~660 lines)

**Recommendation**: EXPLICIT_SKIP is reasonable — splitting ~100 lines of storage from ~100 lines of refresh creates two tiny modules.

### main.rs — Command Dispatch (REFACTOR-012)

**Critical finding:** No centralized DB setup block exists. `open_connection` is called per-arm. The `run` handler is only ~30 lines.

**Structure:**
- `get_project_root()` (L33–50) — git rev-parse helper
- `main()` (L52–60) — clap parse + call `run()`
- `run(cli)` (L61–835) — single match with **34 arms**, ~773 lines

**Each arm is 5-30 lines**, well-organized. No test module.

**Recommendation**: A flat match is idiomatic Rust. Grouping into dispatch helpers is optional. EXPLICIT_SKIP is reasonable if each arm is already clean.

### signals.rs — SessionGuidance Extraction (REFACTOR-013)

**SessionGuidance** (L22–93, ~70 lines production):
- Struct + 5 methods: `new`, `add`, `format_for_prompt`, `is_empty`, `format_for_recording`
- Private `GuidanceEntry` struct (L28)
- Accumulates interactive stdin guidance (NOT from steering.md file — PRD was wrong about file parsing)
- Tests: ~130 lines (L309–441)

**Signal handling** (rest of file):
- `SignalFlag` struct (L205–237) — `Arc<AtomicBool>` wrapper for SIGINT/SIGTERM
- Stop/pause file functions: `check_stop_signal`, `check_pause_signal`, `cleanup_signal_files`, `handle_pause`, path helpers
- Tests: ~350 lines

**External callers**:
- engine.rs L44: `use ...signals::{self, SessionGuidance, SignalFlag}`
- usage.rs, batch.rs: `signals::check_stop_signal`
- claude.rs: `use ...signals::SignalFlag`

**After extraction**: engine.rs imports `SessionGuidance` from `guidance`, `SignalFlag` from `signals`. `handle_pause` in signals.rs imports `SessionGuidance` from `super::guidance`.

---

## Extraction Protocol

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
- What was done (extracted or EXPLICIT_SKIP with rationale)
- Files changed
- Test count: before=X, after=X
- **Learnings:** (patterns, gotchas)
---
```

---

## Important Rules

- Work on **ONE task per iteration**
- **EXPLICIT_SKIP** is a valid outcome for small files — document rationale
- **Commit frequently** after each passing task
- **Read before writing**
- **Minimal changes** — only move code, don't refactor logic
