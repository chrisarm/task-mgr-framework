# PRD: PRD-Scoped Sessions & Concurrent Loop Support

**Type**: Enhancement
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-02-27
**Status**: Draft

---

## 1. Overview

### Problem Statement

`task-mgr loop` has two compounding problems when multiple PRDs coexist in a single database:

1. **No PRD scoping**: Task selection queries ALL tasks regardless of which PRD started the session. When multiple PRDs are imported (via `--append` or `batch`), a loop picks up tasks from the wrong PRD — silently working on unrelated stories.
2. **Single-session lock**: A global `loop.lock` prevents concurrent sessions, even when targeting different PRDs in separate worktrees.

The `batch` command already handles sequential PRD processing but suffers from problem #1 — each batch step can pick tasks from previously imported PRDs.

### Background

- Task IDs are already prefixed during import (e.g., `P1-US-001`, `P2-BUG-003`) via the `task_prefix` field in PRD JSON.
- The `prd_metadata` table enforces `CHECK(id = 1)` — a singleton constraint from when only one PRD was expected.
- `prefix_filter()` exists privately in `status.rs` but is not used by the core task selection or engine queries.
- `LockGuard::acquire_named()` already supports custom lock file names.
- `IterationParams` threads per-iteration state but has no `task_prefix` field.

---

## 2. Goals

### Primary Goals
- [ ] All task selection, recovery, and count queries are scoped by PRD prefix
- [ ] Multiple `loop` sessions run concurrently on different PRDs without interference
- [ ] `--force` reinit deletes only the targeted PRD's data, not the entire database
- [ ] Signal files (`.stop`, `.pause`) support per-session and global variants
- [ ] Backwards compatibility: single-PRD databases work without prefix (legacy fallback)

### Success Metrics
- Concurrent loops: Two terminals running `loop` on different PRDs both complete their own tasks without cross-contamination
- Isolation: `--force` reinit of PRD-A leaves PRD-B's tasks, metadata, and files intact
- Lock accuracy: Second session on same PRD gets a clear lock error; different PRDs proceed
- Signal precision: `.stop-P1` stops only the P1 session; `.stop` stops all sessions
- Zero regressions: All existing `cargo test` tests pass

---

## 2.5. Quality Dimensions

### Correctness Requirements
- Every SQL query that touches `tasks`, `task_relationships`, `task_files`, or `prd_metadata` must be scoped by prefix when a prefix is available
- `LIKE` patterns must escape wildcards (`%`, `_`, `\`) in the prefix to prevent unintended matches
- Migration v9 must preserve existing data — no data loss during schema change
- Lock files must use `flock` (advisory locks) that auto-release on process death

### Performance Requirements
- Best effort — no hard latency targets
- Prefix filtering via `LIKE 'prefix-%'` on the `id` column (TEXT PRIMARY KEY) is acceptable; these tables are small (hundreds of rows max)

### Style Requirements
- Follow existing codebase patterns: `TaskMgrResult<T>`, `rusqlite::params![]`, migration file structure
- Shared utility functions in `src/db/prefix.rs` — no duplicated prefix logic
- All new public functions documented with `///` doc comments

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| Prefix contains LIKE wildcards (`%`, `_`) | Would match unintended task IDs | Escaped via `ESCAPE '\\'` clause |
| Prefix contains filesystem-unsafe chars | Lock/signal filenames would break | Validated during init: `[a-zA-Z0-9._-]` only |
| Legacy DB with no prefix (pre-v5 data) | Old single-PRD databases still need to work | Falls back to unscoped queries (`LIMIT 1 ORDER BY id ASC` for metadata) |
| Two sessions on same PRD | Would cause conflicting task updates | Per-PRD lock prevents; clear error message |
| Session crash mid-loop | Lock file left behind | `flock` auto-releases on process death |
| `--force` on first init (no prefix known) | Can't scope deletion by prefix | Falls back to current global wipe behavior |
| Prefix `P1` vs `P1-extra` | `P1-%` LIKE could match `P1-extra-US-001` | Prefix validation + exact `prefix-%` pattern prevents this in practice; task IDs use `PREFIX-STORY-NNN` format |
| Global `.stop` with multiple sessions | Must stop ALL running sessions | Global `.stop` checked as fallback by all sessions |

---

## 3. User Stories

### US-001: Scoped Task Selection
**As a** developer running `task-mgr loop`
**I want** the loop to only pick tasks from my active PRD
**So that** I don't accidentally work on tasks from a different PRD that shares the database

**Acceptance Criteria:**
- [ ] `select_next_task()` filters by `task_prefix` when provided
- [ ] All 4 helper queries (`get_completed_task_ids`, `get_todo_tasks`, `get_relationships_by_type`, `get_all_task_files`) include prefix filter
- [ ] Decay queries only affect tasks within the current prefix
- [ ] Task count queries in the engine are prefix-scoped

### US-002: Concurrent Loop Sessions
**As a** developer with multiple PRDs imported
**I want** to run `task-mgr loop prd-a.json` and `task-mgr loop prd-b.json` simultaneously
**So that** I can parallelize work across PRDs in separate terminals/worktrees

**Acceptance Criteria:**
- [ ] Each PRD gets its own lock file (`loop-{prefix}.lock`)
- [ ] Two sessions on different PRDs run concurrently without blocking
- [ ] Two sessions on the same PRD get a clear lock error
- [ ] `batch` command inherits per-PRD locking (no changes needed — already calls `run_loop()` per PRD)

### US-003: Scoped Force-Reinit
**As a** developer re-importing a single PRD
**I want** `--force` to only delete that PRD's tasks and metadata
**So that** other PRDs' data in the shared database is preserved

**Acceptance Criteria:**
- [ ] `DROP` scoped to `tasks WHERE id LIKE '{prefix}-%'`
- [ ] `task_relationships`, `task_files` scoped similarly
- [ ] `prd_metadata` and `prd_files` scoped by prefix/prd_id
- [ ] Falls back to global wipe when no prefix is available (first init)

### US-004: Per-Session Signal Files
**As a** developer running concurrent loops
**I want** to stop or pause a single PRD's session without affecting others
**So that** I can manage sessions independently

**Acceptance Criteria:**
- [ ] `.stop-{prefix}` stops only that session
- [ ] `.pause-{prefix}` pauses only that session
- [ ] Global `.stop` / `.pause` stops/pauses ALL sessions (emergency kill-all)
- [ ] `cleanup_signal_files()` only removes the current session's signal files
- [ ] Does NOT remove other sessions' or global signal files

### US-005: Multi-PRD Metadata
**As a** system importing multiple PRDs
**I want** `prd_metadata` to store one row per PRD (keyed by `task_prefix`)
**So that** each PRD retains its own project name, branch, model, and configuration

**Acceptance Criteria:**
- [ ] Migration v9 removes `CHECK(id = 1)` singleton constraint
- [ ] `INSERT` uses `ON CONFLICT(task_prefix) DO UPDATE` (upsert)
- [ ] `prd_files.prd_id` references the correct per-PRD row (not hardcoded 1)
- [ ] `read_prd_metadata()` queries by `task_prefix` with legacy fallback

---

## 4. Functional Requirements

### FR-001: Migration v9 — Remove prd_metadata Singleton
Recreate `prd_metadata` table without `CHECK(id = 1)`, using `AUTOINCREMENT` primary key and `UNIQUE` constraint on `task_prefix`. Preserve existing data via copy-to-new-table pattern.

**Validation:** Migration runs without error on existing v8 databases; data preserved.

### FR-002: Upsert prd_metadata by task_prefix
Replace `INSERT OR REPLACE ... VALUES (1, ...)` with `ON CONFLICT(task_prefix) DO UPDATE`. Return the upserted row's `id` for `prd_files` association.

**Validation:** Importing same PRD twice updates (not duplicates) the row. Importing different PRDs creates separate rows.

### FR-003: Dynamic prd_files.prd_id
Thread the upserted `prd_id` from FR-002 through `register_prd_files()` and `insert_prd_file()`.

**Validation:** `prd_files.prd_id` matches the correct `prd_metadata.id` for each PRD.

### FR-004: Prefix-Scoped --force
`drop_existing_data()` accepts optional prefix. When provided, scopes all DELETE statements. Falls back to global wipe when prefix is `None`.

**Validation:** `--force` on PRD-A leaves PRD-B's tasks intact.

### FR-005: Shared Prefix Utility Module
`src/db/prefix.rs` provides `prefix_where()`, `prefix_and()`, `escape_like()`, `validate_prefix()`. All consumers use this shared module — no inline prefix filtering.

**Validation:** Unit tests cover LIKE escaping, None passthrough, validation rejecting unsafe chars.

### FR-006: Prefix-Scoped Task Selection
All 4 selection helper queries plus `select_next_task()` accept `task_prefix: Option<&str>`.

**Validation:** With prefix `P1`, only `P1-*` tasks are considered; `P2-*` tasks are invisible.

### FR-007: Engine Query Scoping
All engine queries (recovery, counts, reconciliation, calibration) are prefix-scoped.

**Validation:** In-progress recovery only resets the current PRD's tasks. Task counts only reflect the current PRD.

### FR-008: Per-PRD Locks
Lock file name is `loop-{prefix}.lock` when prefix is known, `loop.lock` otherwise.

**Validation:** Two different-prefix loops acquire their locks independently. Same-prefix loop gets clear error.

### FR-009: Per-Session Signals with Global Fallback
Signal check order: session-specific first, then global. Cleanup only removes session-specific files.

**Validation:** `.stop-P1` stops P1 only; `.stop` stops all; cleanup after P1 doesn't remove `.stop-P2`.

### FR-010: Standalone `next` with --prefix
`task-mgr next --prefix P1` filters task selection by prefix.

**Validation:** `next --prefix P1` returns only P1 tasks.

---

## 5. Non-Goals (Out of Scope)

- **Cross-PRD dependencies**: Dependencies are checked within the same prefix only — Reason: Cross-PRD ordering adds complexity with minimal benefit
- **Separate databases per PRD**: A single shared DB is simpler for learnings sharing — Reason: Learnings benefit from cross-PRD visibility
- **Automatic prefix detection from working directory**: User must specify PRD file — Reason: Ambiguous in multi-PRD scenarios
- **Changes to `list`, `complete`, `fail`, `skip`, `reset` commands**: These already use explicit prefixed task IDs — Reason: No scoping needed
- **Cross-machine lock coordination**: `flock` is kernel-level only — Reason: Network filesystem locking is a separate, larger problem

---

## 6. Technical Considerations

### Affected Components
- `src/db/migrations/v9.rs` — **New**: Remove prd_metadata singleton
- `src/db/migrations/mod.rs` — Add v9, bump version to 9
- `src/db/prefix.rs` — **New**: Shared prefix utility
- `src/db/mod.rs` — Add `pub mod prefix`
- `src/commands/init/import.rs` — Upsert by task_prefix, fix prd_files prd_id, scope --force
- `src/commands/next/selection.rs` — Add task_prefix param to all queries
- `src/commands/next/mod.rs` — Thread task_prefix, add --prefix CLI arg
- `src/commands/next/decay.rs` — Prefix-scope decay queries
- `src/loop_engine/engine.rs` — Per-PRD lock, scoped recovery, prefix threading, scope 6+ queries
- `src/loop_engine/prompt.rs` — Add task_prefix to BuildPromptParams
- `src/loop_engine/signals.rs` — Per-session signal files with global fallback
- `src/loop_engine/status.rs` — Use shared prefix_filter, multi-row prd_metadata
- `src/loop_engine/calibrate.rs` — Prefix-scope calibration queries

### Dependencies
- `rusqlite` — SQLite operations (existing)
- `flock` via `fs2` — Advisory file locks (existing)
- `serde_json` — PRD JSON parsing for prefix extraction (existing)

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| **A: Shared DB + Prefix Filtering** — Keep single `tasks.db`, scope all queries by `task_prefix` LIKE pattern | Simple migration; learnings shared; no file management complexity | Every query must be audited and patched; LIKE on TEXT PK is not indexed (acceptable at scale) | **Preferred** |
| **B: Separate DB per PRD** — Each PRD gets `tasks-{prefix}.db` | Perfect isolation; no query changes needed | Learnings not shared across PRDs; file proliferation; connection management complexity; migration must run per-DB | Rejected |
| **C: Schema-level isolation (ATTACH DATABASE)** — Use SQLite's ATTACH for per-PRD schemas | SQL namespace isolation; shared connection | Complex cross-schema queries for learnings; ATTACH limits (10 DBs); not well-supported by rusqlite | Rejected |

**Selected Approach**: **A (Shared DB + Prefix Filtering)**. The task_prefix already exists on all task IDs. Prefix filtering is simple, auditable, and keeps learnings naturally shared. The LIKE pattern cost is negligible for tables with hundreds of rows.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Missed unscoped query — a query without prefix filter leaks cross-PRD data | Med (wrong task picked, wrong count) | Med (many queries to audit) | Comprehensive grep for all `FROM tasks`, `FROM task_relationships`, `FROM task_files` queries; unit tests per story |
| SQLite write contention — two concurrent loops writing to same DB | Low (brief delays) | Med (expected workflow) | WAL mode already enabled; `busy_timeout = 5000ms` handles brief contention; reads are non-blocking in WAL |
| Migration data loss — v9 table recreation loses data | High (all PRD metadata gone) | Low (copy-to-new-table is safe pattern) | Use `INSERT INTO ... SELECT * FROM` before DROP; test migration on real DB |

### Security Considerations
- Prefix values are validated (`[a-zA-Z0-9._-]`) to prevent path traversal in lock/signal filenames
- LIKE wildcards in prefixes are escaped to prevent SQL injection-adjacent pattern matching
- Lock files use advisory locks (not file existence) — no TOCTOU race conditions

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
|----------------|-----------|-------------------|-----------------|-------------|
| `db::prefix::prefix_where()` | `(task_prefix: Option<&str>) -> (String, Option<String>)` | `("WHERE id LIKE ? ESCAPE '\\'", Some("P1-%"))` or `("", None)` | N/A (infallible) | None |
| `db::prefix::prefix_and()` | `(task_prefix: Option<&str>) -> (String, Option<String>)` | `("AND id LIKE ? ESCAPE '\\'", Some("P1-%"))` or `("", None)` | N/A (infallible) | None |
| `db::prefix::validate_prefix()` | `(prefix: &str) -> Result<(), String>` | `Ok(())` | `Err("invalid chars...")` | None |
| `db::prefix::escape_like()` | `(s: &str) -> String` | Escaped string | N/A (infallible) | None |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|----------------|-------------------|-------------------|-----------|-----------|
| `commands::next::next()` | `(dir, after_files, claim, run_id, decay_threshold, recently_completed)` | `+ task_prefix: Option<&str>` | Yes (internal) | All callers updated in same PR |
| `commands::next::selection::select_next_task()` | `(conn, after_files, recently_completed)` | `+ task_prefix: Option<&str>` | Yes (internal) | All callers updated |
| `commands::init::import::insert_prd_metadata()` | `(conn, prd, raw_json) -> TaskMgrResult<()>` | `-> TaskMgrResult<i64>` (returns prd_id) | Yes (return type) | All callers updated |
| `commands::init::import::insert_prd_file()` | `(conn, file_path, file_type)` | `+ prd_id: i64` | Yes (internal) | All callers updated |
| `commands::init::import::drop_existing_data()` | `(conn)` | `+ task_prefix: Option<&str>` | Yes (internal) | All callers updated |
| `loop_engine::signals::check_stop_signal()` | `(tasks_dir)` | `+ prefix: Option<&str>` | Yes (internal) | All callers updated |
| `loop_engine::signals::check_pause_signal()` | `(tasks_dir)` | `+ prefix: Option<&str>` | Yes (internal) | All callers updated |
| `loop_engine::signals::cleanup_signal_files()` | `(tasks_dir)` | `+ prefix: Option<&str>` | Yes (internal) | All callers updated |

### Existing Code to Reuse
- `read_task_prefix_from_prd()` at `src/loop_engine/status.rs:138` — reads taskPrefix from PRD JSON
- `prefix_filter()` at `src/loop_engine/status.rs:276` — move to shared module
- `LockGuard::acquire_named()` at `src/db/lock.rs:62` — already supports custom lock names
- `IterationParams` at `src/loop_engine/engine.rs:76` — add `task_prefix` field

---

## 7. Open Questions

- [x] ~~Should learnings be scoped by prefix?~~ No — learnings are shared cross-PRD (design decision)
- [x] ~~Should calibration weights be per-PRD?~~ No — global calibration is simpler and benefits from more data
- [ ] Should `task-mgr status` show a combined view when no PRD is specified, or require `--prefix`?

---

## Appendix

### Implementation Order (Dependency Graph)

```
Story 1 (Migration v9)
  └── Story 2 (Upsert by prefix)
       ├── Story 2.5 (Fix prd_files prd_id)
       └── Story 2.7 (Scope --force)
Story 4 (Shared prefix utility)  ← can be parallel with Stories 1-2
  ├── Story 3 (read_prd_metadata by prefix)
  ├── Story 5 (Task selection prefix param)
  │    └── Story 6 (Thread through engine)
  │         └── Story 7 (Scope all engine queries)
  ├── Story 8 (Per-PRD locks)
  └── Story 9 (Per-session signals)
Story 10 (Non-loop commands) ← after Stories 3, 5
```

### Glossary
- **PRD**: Product Requirements Document — the JSON task file imported via `task-mgr init`
- **task_prefix**: A short string (e.g., `P1`, `P2`) prepended to all task IDs from a given PRD
- **Prefix scoping**: Filtering SQL queries with `WHERE id LIKE '{prefix}-%' ESCAPE '\\'`
- **Session**: A single `task-mgr loop` invocation bound to one PRD
