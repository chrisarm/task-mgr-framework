# PRD: Multi-PRD Archive Support

**Type**: Enhancement
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-03-03
**Status**: Draft

---

## 1. Overview

### Problem Statement

The `task-mgr archive` command hardcodes `prd_metadata WHERE id = 1` and checks ALL tasks globally via `is_prd_completed()`. When multiple PRDs coexist in the database (common — the DB currently holds 6 PRDs with different `task_prefix` values), the command reports "not fully completed" even though individual PRDs have all tasks in terminal states. Users cannot archive completed work without first manually cleaning up the database.

### Background

Migration v9 (`src/db/migrations/v9.rs`) removed the singleton `CHECK(id=1)` constraint from `prd_metadata`, enabling multiple PRDs to coexist with unique `task_prefix` values. The `init` command fully supports `--append` mode for loading multiple PRDs. The `prefix.rs` module provides LIKE-based SQL utilities for scoping queries to a specific prefix. However, the archive command was never updated to leverage this multi-PRD infrastructure.

The existing Python script (`tasks/archive_completed.py`) demonstrates the desired behavior: iterating each PRD independently and archiving completed ones while leaving incomplete ones untouched.

---

## 2. Goals

### Primary Goals

- [ ] Archive command iterates all PRDs in `prd_metadata` and archives each completed one independently
- [ ] Scoped DB cleanup: only delete tasks, runs, and metadata for the archived PRD
- [ ] Per-PRD reporting in both text and JSON output formats
- [ ] Transaction-safe cleanup to prevent partial DB corruption

### Success Metrics

- `task-mgr archive --dry-run` correctly identifies completed PRDs in a multi-PRD database
- `task-mgr archive` moves only the completed PRD's files; incomplete PRDs' files and tasks are untouched
- All existing tests updated and passing; new multi-PRD tests added

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Prefix-scoped queries must use `make_like_pattern()` with `ESCAPE '\\'` to prevent LIKE wildcard injection
- The dash separator in patterns (`prefix-%`) must prevent cross-prefix contamination (e.g., prefix `P1` must not match prefix `P10`)
- DB cleanup must be atomic (transaction-wrapped) — partial deletion is never acceptable
- `progress.txt` must never be moved (it is shared across PRDs)
- PRDs with NULL `task_prefix` must be skipped (cannot scope queries without a prefix)
- PRDs with zero tasks must be skipped (not archivable)

### Performance Requirements

- Use `NOT EXISTS` instead of `NOT IN` for orphan run cleanup (avoids full table scan)
- No unnecessary queries — exit early if `prd_metadata` is empty

### Style Requirements

- Follow existing `archive.rs` patterns (private helper functions, `TaskMgrResult` return types, `map_err(TaskMgrError::DatabaseError)`)
- Reuse `crate::db::prefix::make_like_pattern` — do not reimplement LIKE escaping
- All new structs derive `Debug, Serialize` for JSON output compatibility

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| PRD with NULL task_prefix | Pre-v9 databases or manual inserts may lack prefixes | Skip with reason "no task prefix" in output |
| PRD with zero matching tasks | Metadata exists but tasks were manually deleted | Skip (treat as incomplete) |
| Multiple PRDs sharing a run | A run may have tasks from different PRDs via `run_tasks` | Only delete `run_tasks` rows for archived PRD's tasks; delete run only if it becomes orphaned |
| `global_state.last_task_id` references a deleted task | After archiving, the pointer becomes dangling | NULL out if referencing a deleted row; reset counters only when all PRDs are gone |
| Archive folder name collision | Two PRDs with same branch slug archived on same day | `fs::create_dir_all` + `fs::rename` will overwrite — acceptable on Linux |
| `prd_files` table missing (pre-v6 DB) | `query_prd_files` returns empty vec | Fall back to project-name-based file discovery |

---

## 3. User Stories

### US-001: Archive Individual Completed PRDs

**As a** developer using task-mgr with multiple PRDs loaded
**I want** the archive command to archive each completed PRD independently
**So that** I can clean up finished work without waiting for all PRDs to complete

**Acceptance Criteria:**

- [ ] `task-mgr archive --dry-run` lists each PRD with its completion status
- [ ] Completed PRDs show which files would be archived and the archive folder name
- [ ] Incomplete PRDs show why they were skipped (e.g., "3 tasks remaining")
- [ ] PRDs without a `task_prefix` show "no task prefix" skip reason

### US-002: Scoped Database Cleanup

**As a** developer
**I want** archiving a PRD to only remove that PRD's tasks and metadata from the DB
**So that** other active PRDs continue working correctly

**Acceptance Criteria:**

- [ ] After archiving PRD A, PRD B's tasks remain in the DB with correct statuses
- [ ] After archiving PRD A, runs shared between PRDs are preserved if PRD B still references them
- [ ] DB learnings are always preserved (never deleted by archive)
- [ ] `global_state` counters are only reset when the last PRD is archived

### US-003: Transaction-Safe Cleanup

**As a** developer
**I want** per-PRD DB cleanup to be atomic
**So that** a crash mid-archive doesn't leave orphaned rows or missing metadata

**Acceptance Criteria:**

- [ ] All DELETE/UPDATE operations for a single PRD are wrapped in a SQLite transaction
- [ ] If any step fails, the entire PRD cleanup is rolled back

---

## 4. Functional Requirements

### FR-001: Query All PRDs

Enumerate all rows from `prd_metadata` including `id`, `project`, `branch_name`, and `task_prefix`.

### FR-002: Per-Prefix Completion Check

For each PRD with a `task_prefix`, check if all tasks matching `{prefix}-%` are in terminal states (`done`, `skipped`, `irrelevant`). A PRD with zero matching tasks is considered incomplete.

### FR-003: Per-PRD File Discovery

Query `prd_files WHERE prd_id = ?` (parameterized, not hardcoded). Fall back to project-name guessing if `prd_files` is empty for that PRD.

### FR-004: Per-PRD Scoped Deletion

Delete only the archived PRD's data: `run_tasks`, orphaned `runs`, `task_relationships`, `task_files`, `tasks`, `prd_files`, and `prd_metadata` row. Wrapped in a transaction.

### FR-005: Learnings Extraction (Once Per Archive Run)

Extract learnings from `progress.txt` once (not per-PRD). Only write to `learnings.md` if at least one PRD was actually archived. Never move `progress.txt`.

### FR-006: Per-PRD Output Reporting

Report each PRD's archive status: archived (with file list and folder name) or skipped (with reason). Both text and JSON formats.

---

## 5. Non-Goals (Out of Scope)

- **CLI flag changes** — Keep `--dry-run` as-is; do not add `--apply` or change default behavior
- **progress.txt management** — Never move or archive `progress.txt`; it's shared state
- **Cross-filesystem moves** — `fs::rename` already handles same-filesystem moves; cross-filesystem is a pre-existing limitation
- **Archiving PRDs without task_prefix** — These are un-scopable; skip with a message

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/archive.rs` — Core rewrite (all logic changes here)
- `src/main.rs:710-714` — Update `run_archive` call (signature change: `&Path` → needs `&mut Connection` for transactions, or open connection internally)
- `src/loop_engine/branch.rs:79` — Same caller update

### Dependencies

- `crate::db::prefix::make_like_pattern` — LIKE pattern construction
- `crate::db::open_connection` — DB connection
- `rusqlite::Connection::transaction()` — Transaction support (requires `&mut Connection`)

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| A: Open connection inside `run_archive`, use `conn.transaction()` for each PRD | Self-contained; callers don't need signature changes | Connection opened internally means callers can't share connections | **Preferred** — matches current pattern where `run_archive` opens its own connection |
| B: Change `run_archive` to accept `&mut Connection` | Allows connection sharing | Breaking change for both callers (`main.rs`, `branch.rs`); callers need to manage connection lifetime | Rejected |

**Selected Approach**: A — Keep `run_archive(dir: &Path, dry_run: bool)` signature. Internally change `open_connection(dir)` to return a mutable connection and use `conn.transaction()` for each PRD's cleanup. This is already how it works — `open_connection` returns `Connection` (owned), so we can make it `mut`.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Orphan run deletion removes runs still referenced by non-archived PRD | Data loss — run history lost | Low (NOT EXISTS subquery is correct) | Test with multi-PRD shared runs |
| `global_state.last_task_id` becomes dangling after partial archive | Next command may error on missing task | Medium | NULL out dangling references after cleanup |
| Pre-v9 databases with NULL task_prefix | Archive skips all PRDs silently | Low (most users are on v9+) | Clear skip message with remediation hint |

### Public Contracts

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|----------------|-------------------|-------------------|-----------|-----------|
| `loop_engine::archive::run_archive` | `fn run_archive(dir: &Path, dry_run: bool) -> TaskMgrResult<ArchiveResult>` | Same signature (no change) | No | N/A |
| `loop_engine::archive::ArchiveResult` | `{archived, learnings_extracted, tasks_cleared, dry_run, message}` | Add `prds_archived: Vec<PrdArchiveSummary>`, `prds_skipped: Vec<PrdSkipReason>` | No (additive) | Existing fields preserved |

#### New Types

| Type | Fields | Purpose |
|------|--------|---------|
| `PrdArchiveSummary` | `prd_id: i64, project: String, task_prefix: String, archive_folder: String, files_archived: usize, tasks_cleared: usize` | Per-PRD archive result |
| `PrdSkipReason` | `prd_id: i64, project: String, reason: String` | Why a PRD was not archived |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|-----------|-------|--------|------------|
| `src/main.rs:710-714` | Calls `run_archive(&cli.dir, dry_run)`, passes result to `output_result` | OK — signature unchanged, new fields are additive | None needed |
| `src/loop_engine/branch.rs:79-85` | Calls `run_archive(dir, false)`, checks `result.archived.is_empty()` and `.len()` | OK — `archived` field preserved as flattened union of all per-PRD items | None needed |
| `src/handlers.rs:102-105` | `impl_text_formattable!(ArchiveResult, format_text)` | OK — `format_text` is updated in same change | None needed |

### Inversion Checklist

- [x] All callers identified and checked (main.rs, branch.rs)
- [x] Routing/branching decisions that depend on output reviewed (branch.rs checks archived.is_empty())
- [x] Tests that validate current behavior identified (20+ tests in archive.rs)
- [x] Different semantic contexts for same code discovered (progress.txt is shared, not per-PRD)

---

## 7. Open Questions

- [ ] None — all questions resolved during planning

---

## Appendix

### Key Files

- `src/loop_engine/archive.rs` — Implementation target
- `src/db/prefix.rs` — `make_like_pattern`, `escape_like` utilities to reuse
- `src/db/migrations/v9.rs` — Multi-PRD schema migration reference
- `src/db/schema/metadata.rs` — `prd_metadata`, `prd_files` schema reference
- `src/db/schema/runs.rs` — `runs`, `run_tasks` schema reference
- `tasks/archive_completed.py` — Python reference implementation

### Glossary

- **PRD**: Product Requirements Document — a JSON task file imported via `task-mgr init`
- **task_prefix**: A unique string (e.g., `9c5c8a1d`) prepended to task IDs to scope them to a PRD
- **Terminal state**: A task status that indicates completion: `done`, `skipped`, or `irrelevant`
