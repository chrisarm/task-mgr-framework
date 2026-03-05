# PRD: Large File Decomposition Refactor

**Type**: Refactor
**Priority**: P2 (Medium)
**Author**: Claude Code
**Created**: 2026-03-03
**Status**: Draft

---

## 1. Overview

### Problem Statement

The `loop_engine/` subsystem has accumulated files ranging from 600–4525 lines with multiple distinct responsibilities per file. This increases cognitive load for navigation, makes targeted testing harder, and raises the risk of merge conflicts when multiple features touch the same large file. The three largest files (`engine.rs` at 4525L, `prompt.rs` at 3753L, `env.rs` at 2688L) each contain 4–8 identifiable sub-domains that should be separate modules.

### Background

The project follows a pragmatic module pattern: simple modules are single files, complex ones use subdirectories with `mod.rs`. Examples of well-structured complex modules already exist (`commands/next/`, `commands/curate/`, `learnings/retrieval/`). This refactor extends that pattern to `loop_engine/` files that have outgrown the single-file approach.

All extractions are **mechanical moves** — functions relocate to new files, `mod.rs` gets updated, and the original file adds `use` imports. No signatures change, no behavior changes, no new features.

---

## 2. Goals

### Primary Goals

- [ ] Reduce the largest file (`engine.rs`) from 4525 to ~1500–2000 lines
- [ ] Reduce `prompt.rs` from 3753 to ~1500 lines via section builder extraction
- [ ] Reduce `env.rs` from 2688 to ~1800 lines via worktree extraction
- [ ] Each extracted module has a single, documentable responsibility
- [ ] Eliminate duplicated logic between `commands/worktrees.rs` and `loop_engine/env.rs` worktree code

### Success Metrics

- No file in `src/loop_engine/` exceeds ~1500 lines of production code (tests excluded)
- `cargo build`, `cargo test`, `cargo clippy` all pass after every extraction
- Zero behavior changes — the CLI produces identical output for all commands

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Every extraction must be verified by `cargo build` + `cargo test` + `cargo clippy` immediately after the move
- Function signatures must not change — callers see the same public API via re-exports
- Test modules move with their tested functions (no orphaned tests)
- `#[cfg(test)]` inline tests that reference private helpers must either move with those helpers or the helpers must become `pub(crate)`

### Performance Requirements

- No runtime performance impact — this is purely compile-time module reorganization
- Compilation time should not significantly increase (avoid circular module dependencies that force recompilation)

### Style Requirements

- Follow existing subdirectory pattern: `mod.rs` with `pub mod` declarations and selective re-exports
- Every new module file starts with `//!` module doc comment explaining its single responsibility
- Use `pub(crate)` for internal APIs, `pub` only for items that need to be visible outside the crate
- Maintain alphabetical ordering of `mod` declarations in `mod.rs` (matches existing convention)
- Tests go in separate `tests.rs` files within subdirectories (matches `commands/next/tests.rs` pattern)

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| Functions with `pub(super)` visibility | Moving to a deeper module changes `super` semantics | Change to `pub(crate)` or adjust `use` paths |
| Inline `#[cfg(test)]` modules referencing private helpers | Private helpers won't be visible from a new module | Move helper with test, or make `pub(crate)` |
| Cross-function references within engine.rs | `run_iteration()` calls `mark_task_done()` which calls `update_prd_task_passes()` | Extract the full call chain together into same new module |
| Re-exports for downstream consumers | `main.rs` may import directly from `loop_engine::engine` | Add re-exports in parent `mod.rs` so existing paths still work |
| Platform-specific `#[cfg(unix)]` blocks in claude.rs | Platform conditionals must stay together with their counterpart | Keep paired `#[cfg(unix)]`/`#[cfg(not(unix))]` blocks in same file |
| Constants used across extracted modules | e.g., file name constants in `mod.rs` | Keep constants in `mod.rs`, import from there |

---

## 3. User Stories

### US-001: Extract git reconciliation from engine.rs

**As a** developer modifying git-based task completion detection
**I want** the git reconciliation logic in its own module
**So that** I can find and modify it without scrolling through 4500 lines

**Functions to extract:**
- `reconcile_external_git_completions()` (line 2015)
- `check_git_for_task_completion()` (line 2174)
- `contains_task_id()` (line 2140)

**Target:** `src/loop_engine/git_reconcile.rs`

**Acceptance Criteria:**
- [ ] All three functions live in `git_reconcile.rs`
- [ ] `engine.rs` imports and calls them via `use super::git_reconcile::*` or specific imports
- [ ] `cargo test` passes with no changes to test assertions

---

### US-002: Extract output parsing from engine.rs

**As a** developer working on how Claude output is interpreted
**I want** task completion parsing in its own module
**So that** output detection logic is isolated and independently testable

**Functions to extract:**
- `parse_completed_tasks()` (line 1927)
- `check_output_for_task_completion()` (line 1955)
- `scan_output_for_completed_tasks()` (line 1969)
- `strip_task_prefix()` (line 1913)

**Target:** `src/loop_engine/output_parsing.rs`

**Acceptance Criteria:**
- [ ] All four functions live in `output_parsing.rs`
- [ ] `engine.rs` imports and calls them
- [ ] `cargo test` passes

---

### US-003: Extract PRD reconciliation from engine.rs

**As a** developer working on PRD ↔ DB synchronization
**I want** PRD metadata and pass reconciliation in its own module
**So that** the reconciliation pipeline is traceable in one place

**Functions to extract:**
- `read_prd_metadata()` (line 1549)
- `update_prd_task_passes()` (line 1723)
- `reconcile_passes_with_db()` (line 1811)
- `mark_task_done()` (line 1789)
- `hash_file()` (line 2227)

**Target:** `src/loop_engine/prd_reconcile.rs`

**Acceptance Criteria:**
- [ ] All five functions live in `prd_reconcile.rs`
- [ ] `engine.rs` imports and calls them
- [ ] `cargo test` passes

---

### US-004: Extract prompt section builders from prompt.rs

**As a** developer adding or modifying a prompt section
**I want** each section builder in its own file
**So that** I can work on one section without loading 3700 lines of context

**Target:** `src/loop_engine/prompt_sections/` subdirectory with:
- `mod.rs` — orchestrator re-exports
- `learnings.rs` — learning retrieval, formatting, `record_shown_learnings`
- `synergy.rs` — synergy cluster context + `resolve_synergy_cluster_model`
- `dependencies.rs` — dependency summary generation
- `escalation.rs` — escalation template assembly

Keep `prompt.rs` as the top-level orchestrator managing token budget and calling section builders.

**Acceptance Criteria:**
- [ ] Each section builder is in its own file under `prompt_sections/`
- [ ] `prompt.rs` orchestrates by calling into `prompt_sections::{module}::{function}`
- [ ] Token budget management remains in `prompt.rs`
- [ ] `cargo test` passes

---

### US-005: Extract worktree lifecycle from env.rs

**As a** developer working on worktree creation/removal
**I want** worktree management in a dedicated module
**So that** worktree logic is findable and doesn't interleave with path/git setup

**Functions to extract:** `ensure_worktree`, `remove_worktree`, `is_inside_worktree`, branch name sanitization helpers (~500 lines)

**Target:** `src/loop_engine/worktree.rs`

**Acceptance Criteria:**
- [ ] Worktree lifecycle functions live in `worktree.rs`
- [ ] `env.rs` imports from `worktree.rs`
- [ ] Evaluate consolidation with `commands/worktrees.rs` lock-status detection (document findings, do not necessarily merge yet)
- [ ] `cargo test` passes

---

### US-006: Split claude.rs watchdog and spawn logic

**As a** developer debugging subprocess timeout behavior
**I want** the watchdog loop separated from subprocess spawning
**So that** platform-specific timeout code is isolated

**Target:** `src/loop_engine/watchdog.rs` — platform-specific watchdog loops and kill logic

**Acceptance Criteria:**
- [ ] `claude.rs` handles spawn + env setup
- [ ] `watchdog.rs` handles timeout monitoring, process group kill, exit code interpretation
- [ ] Platform-specific `#[cfg]` pairs stay together in `watchdog.rs`
- [ ] `cargo test` passes

---

### US-007: Split status.rs queries from rendering

**As a** developer modifying the dashboard display
**I want** data queries separated from formatting
**So that** each can evolve independently

**Target:** Either inline submodules or `status/` subdirectory with `queries.rs` + `display.rs`

**Acceptance Criteria:**
- [ ] Query functions (task counts, deadline info, lock detection) in one module
- [ ] Formatting functions (progress bar, icons, text layout) in another
- [ ] `cargo test` passes

---

### US-008: Deduplicate archive.rs learning extraction

**As a** developer maintaining the learning pipeline
**I want** `extract_learnings_from_progress` consolidated with `learnings/ingestion/`
**So that** learning extraction logic exists in exactly one place

**Acceptance Criteria:**
- [ ] Learning extraction in `archive.rs` delegates to or is replaced by `learnings/ingestion/` functions
- [ ] No duplicated parsing logic remains
- [ ] `cargo test` passes

---

### US-009: Split oauth.rs into flow, storage, server

**As a** developer debugging OAuth token issues
**I want** the OAuth flow, token storage, and callback server separated
**So that** each concern is independently testable

**Target:**
- `src/loop_engine/oauth.rs` — top-level token acquisition (orchestrator)
- `src/loop_engine/oauth_server.rs` — local HTTP callback server
- `src/loop_engine/token_store.rs` — token persistence (file-based read/write/refresh)

**Acceptance Criteria:**
- [ ] Three files with clear single responsibilities
- [ ] `cargo test` passes

---

### US-010: Extract DB setup and run dispatch from main.rs

**As a** developer adding new CLI commands
**I want** DB connection setup in a helper and the `run` dispatch block in `commands/run.rs`
**So that** `main.rs` is a thin routing layer

**Acceptance Criteria:**
- [ ] DB path resolution + connection setup extracted to a helper in `db/connection.rs`
- [ ] `run` subcommand handler moved to `commands/run.rs` (or merged with existing)
- [ ] `main.rs` reduced to ~400 lines
- [ ] `cargo test` passes

---

### US-011: Extract calibrate.rs statistical math

**As a** developer modifying weight calibration
**I want** the Pearson correlation and statistical helpers separated from DB queries
**So that** math is testable with pure inputs

**Acceptance Criteria:**
- [ ] Statistical computation functions (correlation, clamping) in a separate module or inline submodule
- [ ] DB query/aggregation remains in `calibrate.rs`
- [ ] `cargo test` passes

---

### US-012: Extract signals.rs SessionGuidance/steering

**As a** developer working on session steering
**I want** `SessionGuidance` and steering.md parsing in its own module
**So that** signal file handling is separate from guidance content

**Target:** `src/loop_engine/steering.rs`

**Acceptance Criteria:**
- [ ] `SessionGuidance` struct and steering.md reader in `steering.rs`
- [ ] Signal file handling (stop/pause) remains in `signals.rs`
- [ ] `cargo test` passes

---

### US-013: Extract detection.rs exit-code classification

**As a** developer modifying outcome detection
**I want** exit-code classification separated from output-string analysis
**So that** each detection method is independently modifiable

**Acceptance Criteria:**
- [ ] Exit-code → crash type mapping in one location
- [ ] Output string pattern matching in another
- [ ] `cargo test` passes

---

### US-014: Assess and extract Tier 4 command files

**As a** developer maintaining command implementations
**I want** an assessment of which command files (complete, curate, learnings, review, recall, etc.) benefit from splitting
**So that** we extract only where there's genuine multi-responsibility, not just length

**Acceptance Criteria:**
- [ ] Each Tier 4 file assessed: single-responsibility confirmed OR extraction targets identified
- [ ] Files confirmed as single-responsibility are documented as "no action needed"
- [ ] Any identified extractions are performed
- [ ] `cargo test` passes

---

### US-015: Assess claude-loop.sh status

**As a** project maintainer
**I want** to know if `scripts/claude-loop.sh` is still actively used or superseded by the Rust engine
**So that** we can deprecate dead code

**Acceptance Criteria:**
- [ ] Usage assessed (referenced in docs? scripts? CI?)
- [ ] If unused: mark as deprecated with comment header
- [ ] If used: document in which scenarios it's still the entry point

---

## 4. Functional Requirements

### FR-001: Module extraction preserves public API

Every extraction must maintain the same `pub` / `pub(crate)` visibility. If `engine.rs` previously exposed `pub(crate) fn reconcile_external_git_completions(...)`, the same function must be accessible at the same path after extraction (via re-exports if necessary).

**Validation:** `cargo build` succeeds; `main.rs` and other callers compile without path changes.

### FR-002: Tests move with their functions

Any `#[cfg(test)]` module or test function that tests an extracted function must move to the new module's test file. Tests must not be orphaned or deleted.

**Validation:** `cargo test` shows identical test count before and after each extraction.

### FR-003: No circular dependencies

Extracted modules must not create circular `use` chains. If function A in module X calls function B in module Y, and B calls back to A, they belong in the same module.

**Validation:** `cargo build` succeeds (Rust compiler rejects circular `mod` dependencies).

### FR-004: Phased delivery

Extractions are grouped into phases matching tiers. Each phase is independently shippable:
- **Phase 1 (Tier 1):** US-001 through US-005 — the three largest files
- **Phase 2 (Tier 2):** US-006 through US-008, US-011, US-013 — 1000+ line files
- **Phase 3 (Tier 3):** US-009, US-010, US-012 — 700–1000 line files
- **Phase 4 (Tier 4):** US-014, US-015 — assessment and cleanup

---

## 5. Non-Goals (Out of Scope)

- **Behavior changes** — No function logic changes. If a function is buggy, that's a separate ticket.
- **New features** — No new capabilities added during refactoring.
- **API redesign** — Function signatures stay the same. Argument types, return types, error types unchanged.
- **Test rewrites** — Tests move as-is. No new tests required (though new module-level doc tests are welcome).
- **Splitting `cli/commands.rs`** — Pure clap derive definitions; size is structural, not a smell.
- **Splitting files that are large due to tests** — `learn.rs`, `context.rs` are mostly test code; production logic is focused.
- **Performance optimization** — No runtime changes.

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/mod.rs` — Add new `pub mod` declarations for every extracted module
- `src/loop_engine/engine.rs` — Loses ~2500 lines across US-001, US-002, US-003
- `src/loop_engine/prompt.rs` — Loses ~2200 lines to `prompt_sections/` (US-004)
- `src/loop_engine/env.rs` — Loses ~500 lines to `worktree.rs` (US-005)
- `src/loop_engine/claude.rs` — Loses ~400 lines to `watchdog.rs` (US-006)
- `src/loop_engine/status.rs` — Restructured into queries + display (US-007)
- `src/loop_engine/archive.rs` — Learning extraction consolidated (US-008)
- `src/loop_engine/oauth.rs` — Split into 3 files (US-009)
- `src/main.rs` — DB setup + run handler extracted (US-010)
- `src/loop_engine/calibrate.rs` — Stats math extracted (US-011)
- `src/loop_engine/signals.rs` — Steering extracted (US-012)
- `src/loop_engine/detection.rs` — Exit-code split (US-013)

### Dependencies

- Rust module system (no external deps added)
- Existing test infrastructure (no test framework changes)

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| **A) Flat files in loop_engine/** | Simple; matches current pattern; minimal mod.rs changes | `loop_engine/` grows from 22 to ~30 files | Preferred for most extractions |
| **B) Subdirectories for large extractions** | Groups related files (e.g., `prompt_sections/`); cleaner navigation | More `mod.rs` boilerplate; deeper import paths | Preferred for prompt.rs (4+ files) |
| **C) Re-export everything from parent mod.rs** | Zero breakage for existing callers | Hides the new module structure; defeats the purpose | Rejected — only re-export what's truly public |

**Selected Approach:** Hybrid A+B. Use flat files for single-module extractions (git_reconcile.rs, output_parsing.rs, worktree.rs). Use subdirectory for prompt_sections/ where 4+ related files are created. Re-export only `pub(crate)` items that are called from outside `loop_engine/`.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Extraction breaks a private helper dependency chain | Build failure | Medium | Map call graphs before extracting; extract full chains together |
| Test count decreases (orphaned tests) | Silent regression | Low | Compare `cargo test` count before/after each extraction |
| Circular module dependency created | Build failure | Low | Rust compiler catches this; resolve by keeping coupled functions together |

### Security Considerations

- No security impact — this is a structural refactor with no logic changes.

### Public Contracts

#### New Interfaces

No new public interfaces. All extracted functions retain their existing signatures and visibility levels.

#### Modified Interfaces

| Module | Current Path | New Path | Breaking? | Migration |
|--------|-------------|----------|-----------|-----------|
| `git_reconcile::*` | `loop_engine::engine::reconcile_external_git_completions` | `loop_engine::git_reconcile::reconcile_external_git_completions` | No (internal) | Re-export from engine.rs during transition, remove later |
| `output_parsing::*` | `loop_engine::engine::parse_completed_tasks` | `loop_engine::output_parsing::parse_completed_tasks` | No (internal) | Same |
| `prompt_sections::*` | `loop_engine::prompt::<section_fn>` | `loop_engine::prompt_sections::<section>::<fn>` | No (internal) | Called from prompt.rs only |

### Inversion Checklist

- [x] All callers identified — engine.rs functions are called from within engine.rs and run_loop; prompt sections called only from prompt.rs
- [x] Tests that validate current behavior identified — inline `#[cfg(test)]` blocks in each file
- [x] Different semantic contexts for same code discovered — `worktree` code exists in both `env.rs` and `commands/worktrees.rs` (documented in US-005)

---

## 7. Open Questions

- [ ] Should `prompt_sections/` be a subdirectory of `loop_engine/` or nested under a new `prompt/` directory that replaces `prompt.rs`?
- [ ] For US-008 (archive learning extraction dedup): should `archive.rs` call into `learnings/ingestion/` or should the shared logic be extracted to a third location?
- [ ] What is the current test count baseline? (Run `cargo test 2>&1 | tail -1` before starting)

---

## Appendix

### Related Documents

- Approved refactoring plan: `$HOME/.claude/plans/velvet-rolling-shannon.md`
- Existing well-structured module examples: `src/commands/next/`, `src/commands/curate/`, `src/learnings/retrieval/`

### Glossary

- **Mechanical extraction**: Moving code to a new file without changing any logic, types, or signatures
- **Re-export**: `pub use submodule::function` in a parent module so callers don't need to change their import paths
- **SRP**: Single Responsibility Principle — each module should have one reason to change
