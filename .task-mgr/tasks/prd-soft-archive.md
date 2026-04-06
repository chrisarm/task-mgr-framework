# PRD: Soft-Archive for Tasks, Runs, and Key Decisions

**Type**: Enhancement
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-03-22
**Status**: Draft

---

## 1. Overview

### Problem Statement

`task-mgr archive --all` currently hard-DELETEs rows from `tasks`, `runs`, `run_tasks`, and `key_decisions`. This destroys valuable history — especially key architectural decisions that informed the codebase. It also just caused an FK constraint crash because `key_decisions` wasn't being cleaned up before its parent tables were deleted.

### Background

The `learnings` table already implements soft-delete via a `retired_at TEXT DEFAULT NULL` column, with most read queries filtering `WHERE retired_at IS NULL`. This pattern is proven and should be extended to the remaining core tables. A temporary hard-delete fix for the FK crash was committed (a80bb46) but the user wants the proper soft-archive approach instead.

---

## 2. Goals

### Primary Goals

- [ ] Replace hard-DELETE with soft-archive (`archived_at` timestamp) for `tasks`, `runs`, `run_tasks`, `key_decisions`
- [ ] Filter archived records from all listing/aggregation queries
- [ ] Add `--include-archived` flag to `list` and `history` commands with optional limit
- [ ] Change `init --force` to archive existing tasks before reimporting

### Success Metrics

- `task-mgr archive --all` succeeds without FK errors
- `task-mgr list` shows zero archived tasks by default
- `sqlite3 .task-mgr/tasks.db "SELECT count(*) FROM tasks WHERE archived_at IS NOT NULL"` confirms records preserved
- `task-mgr list --include-archived` shows both active and archived tasks with markers
- `task-mgr init --force --from-json ...` archives old tasks instead of deleting them

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Archived records must NEVER appear in task selection, loop engine queries, calibration, or dependency checking
- Shared runs (referencing tasks from multiple PRDs) must only be archived when ALL their run_tasks are archived
- Re-archiving already-archived records must be idempotent (preserve original `archived_at` timestamp)
- `init --force` must archive, not delete — no data loss on reimport

### Performance Requirements

- Indexes on `archived_at` columns to keep filtered queries fast as archived rows accumulate
- No full table scans — `WHERE archived_at IS NULL` must use index

### Style Requirements

- Follow existing `retired_at` pattern from learnings for consistency
- Use `AND archived_at IS NULL` inline in SQL (not a view or wrapper), matching learnings style

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Shared run across PRDs | run-shared has run_tasks for PA-001 and PB-001; archiving PA must not archive the run | Only archive run when ALL its run_tasks have `archived_at IS NOT NULL` |
| No files on disk but DB records exist | Learning #565: archive skipped cleanup when no files found | DB soft-archive must happen regardless of file presence |
| `init --force` on already-archived prefix | User archives PA, then re-inits PA from updated JSON | Archive existing active PA tasks first, then import fresh ones; previously archived PA tasks stay archived with original timestamp |
| Counter reset with archived tasks | `iteration_counter` reset logic checks `SELECT COUNT(*) FROM tasks` | Must filter `WHERE archived_at IS NULL` so archived tasks don't prevent counter reset |
| `--include-archived` with large history | User may have thousands of archived records | Optional `--limit` parameter caps archived record count |

---

## 3. User Stories

### US-001: Preserve History on Archive

**As a** developer using task-mgr loops
**I want** archived tasks and key decisions preserved in the database
**So that** I can review past work, decisions, and patterns without losing history

**Acceptance Criteria:**

- [ ] `archive --all` sets `archived_at` instead of deleting rows
- [ ] Archived tasks, runs, run_tasks, and key_decisions remain queryable via SQL
- [ ] Active workflows (list, next, loop) see zero archived records

### US-002: View Archived Records

**As a** developer reviewing past work
**I want** `--include-archived` flag on `list` and `history`
**So that** I can see both active and archived records with clear markers

**Acceptance Criteria:**

- [ ] `task-mgr list --include-archived` shows all tasks with `[archived]` marker on archived ones
- [ ] `task-mgr history --include-archived` shows all runs with `[archived]` marker
- [ ] Optional limit: `--include-archived 50` caps archived records shown

### US-003: Safe Reimport

**As a** developer updating a task list
**I want** `init --force` to archive existing tasks before reimporting
**So that** I don't lose history when refreshing tasks from an updated JSON

**Acceptance Criteria:**

- [ ] `init --force` archives (not deletes) existing tasks for the prefix
- [ ] Fresh tasks import normally after archival
- [ ] Previously archived tasks are untouched (original timestamp preserved)

---

## 4. Functional Requirements

### FR-001: Migration v14 — Add `archived_at` Columns

Add `archived_at TEXT DEFAULT NULL` to `tasks`, `runs`, `run_tasks`, `key_decisions`. Add indexes on each.

**Validation:** Migration tests confirm columns exist, default NULL, and indexes are created.

### FR-002: Soft-Archive in `archive_prd_data`

Replace DELETE with UPDATE SET `archived_at = datetime('now')` for tasks, runs, run_tasks, key_decisions. Keep hard-DELETE for `task_relationships`, `task_files`, `prd_files`, `prd_metadata`.

**Details:**

- `run_tasks`: archive by task prefix
- `key_decisions`: archive by task prefix, then archive any with orphaned runs
- `runs`: archive only when ALL their run_tasks are archived
- `tasks`: archive by prefix
- Global state: NULL out `last_task_id`/`last_run_id` if they reference archived records
- Counter reset: only count non-archived tasks

**Validation:** Archive test confirms PA records have `archived_at IS NOT NULL`, PB records unchanged.

### FR-003: Filter Archived Records from All Read Queries

Add `AND archived_at IS NULL` (or `AND t.archived_at IS NULL` in JOINs) to all listing, aggregation, and selection queries across `loop_engine/` and `commands/`.

**Details:**

- ~35 task queries in loop_engine and commands
- ~10 run queries in history, export, stats, doctor
- ~6 run_tasks queries in complete, skip, irrelevant, fail, calibrate, export
- ~3 key_decisions queries in get_pending, get_all_pending, get_all

Single-ID lookups (show, complete, skip, fail by explicit ID) do NOT need filtering — archived tasks won't be in flight.

**Validation:** After archiving PA, `task-mgr list` returns zero PA tasks; loop engine never selects archived tasks.

### FR-004: `--include-archived` Flag on `list` and `history`

Add `--include-archived [limit]` optional positional argument to both commands. When set, queries omit the `archived_at IS NULL` filter and add `[archived]` markers to output. Optional integer limit caps the number of archived records shown.

**Validation:** `task-mgr list --include-archived` shows both active and archived; `--include-archived 10` caps at 10 archived records.

### FR-005: `init --force` Archives Before Reimport

Change `drop_existing_data()` to call `archive_prd_data()` (for prefix-scoped deletes) instead of hard-deleting. For global wipe (no prefix), keep hard-delete behavior since it's a full reset.

**Validation:** `init --force --from-json tasks.json` with existing prefix data: old tasks get `archived_at`, new tasks import fresh.

---

## 5. Non-Goals (Out of Scope)

- **`purge` command** — Hard-deleting archived records. Schema supports it trivially (`DELETE WHERE archived_at IS NOT NULL`) but the command is deferred.
- **`--include-archived` on `stats`, `export`, `decisions`** — Can be added later using the same pattern.
- **Archiving learnings differently** — Learnings already have `retired_at`. No change needed.
- **Archive individual tasks** — This is PRD-level archival only.

---

## 6. Technical Considerations

### Affected Components

- `src/db/migrations/v14.rs` (new) — migration adding `archived_at` columns + indexes
- `src/db/migrations/mod.rs` — bump `CURRENT_SCHEMA_VERSION` to 14, register v14
- `src/loop_engine/archive.rs` — refactor `clear_prd_data` → `archive_prd_data`, DELETE→UPDATE
- `src/loop_engine/status_queries.rs` — add archive filters to task count/listing queries
- `src/loop_engine/prd_reconcile.rs` — add archive filters (lines 70, 192, 251, 442)
- `src/loop_engine/calibrate.rs` — add archive filters (lines 137, 215-219)
- `src/loop_engine/engine.rs` — add archive filters (lines 336, 3312, 3335)
- `src/loop_engine/git_reconcile.rs` — add archive filter (line 79)
- `src/loop_engine/output_parsing.rs` — add archive filter (line 78)
- `src/loop_engine/prompt_sections/dependencies.rs` — add JOIN filter (lines 30-36)
- `src/loop_engine/prompt_sections/siblings.rs` — add JOIN filter (lines 76-79)
- `src/loop_engine/prompt_sections/synergy.rs` — add JOIN filter (lines 46-55)
- `src/commands/list.rs` — add archive filter + `--include-archived` support (lines 92-168)
- `src/commands/history.rs` — add archive filter + `--include-archived` support (lines 124-261)
- `src/commands/stats.rs` — add archive filters
- `src/commands/review.rs` — add archive filter
- `src/commands/reset.rs` — add archive filters (lines 137, 173)
- `src/commands/next/selection.rs` — add archive filters (lines 284, 300-304)
- `src/commands/next/decay.rs` — add archive filters (lines 71, 181)
- `src/commands/dependency_checker.rs` — add archive filter (line 28)
- `src/commands/export/progress.rs` — add archive filters (lines 135, 333, 350)
- `src/commands/export/prd.rs` — add archive filter (line 166)
- `src/commands/init/mod.rs` — change `--force` to archive-then-reimport (lines 199-223)
- `src/commands/init/import.rs` — change `drop_existing_data()` prefix path to archive
- `src/commands/doctor/checks.rs` — add archive filters (lines 26, 85, 195)
- `src/db/schema/key_decisions.rs` — add archive filters (lines 69, 87, 154)
- `src/cli/commands.rs` — add `--include-archived` args to List and History structs

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| Soft-delete with `archived_at` column | Consistent with `retired_at` pattern; preserves history; simple queries | Requires adding filter to ~50 queries; slight storage overhead | **Preferred** |
| Move archived data to separate `_archive` tables | Physical separation; no filter needed on hot tables | Migration complexity; JOINs across archive tables harder; 2x table count | Rejected |
| Keep hard-delete, export to JSON before deleting | No schema change; history in files | Archived data not queryable; file management overhead; can't `--include-archived` | Rejected |

**Selected Approach**: Soft-delete with `archived_at TEXT DEFAULT NULL` on tasks, runs, run_tasks, key_decisions. Follows the proven `retired_at` pattern from learnings.

**Phase 2 Foundation Check**: This approach lays the foundation for future `purge`, per-task archival, and archive analytics. The column + index cost is minimal now but enables significant future capability without schema changes.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Missing a query filter — archived records leak into active loops | High — loop picks archived task, wastes iteration | Medium — many queries to update | Comprehensive query inventory (done); grep for `FROM tasks` / `FROM runs` after implementation to catch stragglers |
| `init --force` archive + reimport creates duplicate task IDs | High — primary key collision | Low — archive changes `archived_at`, not `id` | Archive sets `archived_at` on existing rows; new import inserts new rows with same IDs. Need to handle PK conflict: delete archived tasks with same prefix before inserting, or use INSERT OR REPLACE |
| Performance degradation with many archived rows | Medium — slower queries | Low — indexes mitigate | Indexes on `archived_at`; future `purge` command for cleanup |

### Security Considerations

- No security implications — this is internal DB schema change with no external API surface.

### Public Contracts

#### Modified Interfaces

| Function | Current Signature | Proposed Change | Breaking? |
| --- | --- | --- | --- |
| `clear_prd_data()` in archive.rs | `fn clear_prd_data(conn, prd_id, prefix) -> Result<usize>` | Rename to `archive_prd_data()`, same signature, UPDATE instead of DELETE | No — internal function |
| `drop_existing_data()` in import.rs | `fn drop_existing_data(conn, prefix) -> Result<()>` | Prefix path calls `archive_prd_data()` instead of DELETE | No — internal function |
| `list()` in list.rs | `fn list(dir, status, file, task_type) -> Result<ListResult>` | Add `include_archived: Option<usize>` parameter | No — additive |
| `history()` in history.rs | `fn history(dir, limit, run_id) -> Result<HistoryResult>` | Add `include_archived: Option<usize>` parameter | No — additive |

#### New CLI Arguments

| Command | Flag | Type | Default | Description |
| --- | --- | --- | --- | --- |
| `list` | `--include-archived` | `Option<usize>` | None | Show archived tasks with `[archived]` marker; optional limit |
| `history` | `--include-archived` | `Option<usize>` | None | Show archived runs with `[archived]` marker; optional limit |

### Data Flow Contracts

N/A — no cross-module data access paths introduced. All changes are inline SQL filter additions within existing query functions.

### Consumers of Changed Behavior

| File | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `src/loop_engine/engine.rs` | Calls `clear_prd_data` indirectly via archive flow | OK — renamed function, same call pattern | Update call site |
| `src/commands/init/import.rs:25-68` | `drop_existing_data()` hard-deletes tasks | CHANGES — prefix path now archives | Change prefix branch to call `archive_prd_data()`; global wipe branch unchanged |
| All loop_engine query files | Read `FROM tasks`/`FROM runs` without archive filter | NEEDS FILTER | Add `AND archived_at IS NULL` |
| All command query files | Read `FROM tasks`/`FROM runs` without archive filter | NEEDS FILTER | Add `AND archived_at IS NULL` |

### `init --force` PK Conflict Note

When `init --force` archives existing tasks and then reimports from JSON, the new tasks will have the same `id` values (e.g., `PA-001`). Since archived rows still exist with those IDs, we need to handle this. **Approach**: After archiving, hard-delete the just-archived tasks for this prefix only (they've been timestamped, but keeping zombie rows with conflicting PKs is worse). Alternatively, use `INSERT OR REPLACE`. The simpler approach: archive sets `archived_at`, then the existing hard-delete logic in `drop_existing_data()` runs on the now-archived rows. This preserves the archive timestamp in key_decisions and runs (which don't have PK conflicts) while cleaning up tasks that need reimporting.

**Revised approach**: The `init --force` prefix path should:
1. Archive run_tasks, key_decisions, runs (soft-archive, preserving history)
2. Hard-delete task_relationships, task_files (derived data)
3. Hard-delete tasks (needed for PK-clean reimport)
4. Hard-delete prd_files, prd_metadata (config data)

This preserves the valuable history (runs, key_decisions) while allowing clean task reimport.

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `CLAUDE.md` | No change needed | Archive behavior is internal implementation detail |

### Inversion Checklist

- [x] All callers of `clear_prd_data` identified? Yes — archive.rs line 173
- [x] All callers of `drop_existing_data` identified? Yes — init/mod.rs lines 221-223
- [x] All task/run/run_tasks/key_decisions SELECT queries inventoried? Yes — ~55 queries total
- [x] Tests that validate current delete behavior identified? Yes — archive.rs test module
- [x] PK conflict on init --force addressed? Yes — see note above

---

## 7. Open Questions

- [x] Should `task_relationships` and `task_files` be soft-archived? **No** — derived data, hard-delete is fine
- [x] Shared run handling? **Archive only when all run_tasks are archived**
- [x] `init --force` behavior? **Archive runs/key_decisions, hard-delete tasks for clean reimport**
- [ ] Should `--include-archived` default limit be unbounded or capped (e.g., 100)?

---

## Appendix

### Related Learnings

- **#565**: `clear_prd_data` skipped when no files on disk — must archive DB regardless of file presence
- **#566**: Always test the no-files-on-disk edge case for archive operations
- **#581**: Copy fixtures locally for archive integration tests
- **#582**: Multi-PRD archive test pattern uses P1/P2 fixtures with DB manipulation

### Existing Pattern Reference

The `learnings` table soft-delete implementation serves as the template:
- Column: `retired_at TEXT DEFAULT NULL`
- All listing queries: `WHERE retired_at IS NULL`
- Retirement: `UPDATE learnings SET retired_at = datetime('now') WHERE ...`
- No index on `retired_at` (we'll add indexes for the higher-volume tables)
