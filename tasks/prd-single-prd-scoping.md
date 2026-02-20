# PRD: Single-PRD Loop Scoping + PRD Registry

**Type**: Bug Fix + Enhancement
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-02-20
**Status**: Draft

---

## 1. Overview

### Problem Statement

When multiple PRDs are imported into the same database (via `append: true` during `init`), the loop engine picks tasks from ALL PRDs indiscriminately. The `prd_metadata` table is a singleton (`CHECK(id = 1)`) that gets overwritten by the last import, so `task_prefix` and `prd_file` may not match tasks from other PRDs. This causes:

1. "Task in PRD not found" warnings when updating the wrong PRD JSON
2. Wasted iterations on tasks from unrelated PRDs
3. Loss of previously-imported PRD metadata (project name, branch, description)

### Background

The loop engine was designed for single-PRD workflows. The `append: true` mode was added to accumulate tasks from multiple PRDs into one DB, but task selection (`select_next_task()`) and completion tracking (`update_prd_task_passes()`) were never scoped per-PRD. The only per-PRD differentiator is the `task_prefix` embedded in task IDs (e.g., `3c39387a-FIX-001`), but no query uses it for filtering.

---

## 2. Goals

### Primary Goals
- [ ] Loop only selects tasks matching the PRD specified at startup
- [ ] Previously-imported PRD metadata is preserved (not overwritten)
- [ ] When current PRD's tasks are all done, report other PRDs with remaining work

### Success Metrics
- Zero "Task in PRD not found" warnings during normal loop operation
- `task-mgr next` from the loop never returns a task from a different PRD
- After importing two PRDs, both entries exist in `prd_registry`

---

## 2.5. Quality Dimensions

### Correctness Requirements
- Prefix filtering must use parameterized SQL (`LIKE ?`) — no string interpolation
- When `task_prefix` is `None` or empty, no filtering is applied (backward compatibility)
- Dependency resolution (`get_completed_task_ids()`) must remain unscoped — tasks from any PRD can satisfy deps
- Startup recovery of stale `in_progress` tasks must only reset tasks belonging to the current PRD

### Performance Requirements
- Best effort — the `AND id LIKE ?` filter adds negligible overhead to existing queries
- `prd_registry` lookups are O(n) on a table with <10 rows

### Style Requirements
- Follow existing `prefix_filter()` pattern from `src/loop_engine/status.rs:276`
- Migration v8 follows existing migration pattern (static SQL, `up_sql`/`down_sql`)
- No `.unwrap()` on registry queries — graceful degradation if table doesn't exist

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| `task_prefix` is `None` | Legacy DBs / no-prefix mode | No filtering — all tasks eligible (backward compat) |
| `task_prefix` is `Some("")` | Could match `LIKE '-%'` incorrectly | Treat as `None` — no filtering |
| All tasks done but other PRDs have work | User expects clear guidance | Print summary of other PRDs, exit `Completed` |
| Batch mode with multiple PRDs | Already sequential per-PRD | No change needed — each `run_loop()` gets correct prefix |
| Legacy DB without `prd_registry` table | v8 migration not yet applied | `find_other_prd_work()` returns empty — no crash |
| Startup recovery with mixed-PRD in_progress tasks | Previous run crashed mid-task on different PRD | Only reset current PRD's tasks; other PRDs' tasks untouched |

---

## 3. User Stories

### US-001: Loop Scoped to Startup PRD
**As a** developer running the loop with a specific PRD file
**I want** the loop to only work on tasks from that PRD
**So that** I don't waste iterations on unrelated tasks or get "not found" warnings

**Acceptance Criteria:**
- [ ] `select_next_task()` accepts `task_prefix` and filters `get_todo_tasks()` by `id LIKE '{prefix}-%'`
- [ ] `next()` and `BuildPromptParams` thread `task_prefix` from engine to selection
- [ ] "Remaining tasks" check in engine.rs scoped to current prefix
- [ ] Auto-recovery of stale `in_progress` tasks scoped to current prefix
- [ ] Startup recovery moved after `read_prd_metadata()` and scoped
- [ ] CLI `task-mgr next` remains unscoped (passes `None`)

### US-002: PRD Registry Preserves All Imported PRDs
**As a** developer who imports multiple PRDs over time
**I want** each PRD's identity preserved in the database
**So that** I can discover and switch between PRDs

**Acceptance Criteria:**
- [ ] Migration v8 creates `prd_registry` table with `task_prefix UNIQUE`
- [ ] `init` registers each imported PRD in `prd_registry` (prefix + file path + project + branch)
- [ ] Re-importing the same PRD updates rather than duplicates the registry entry
- [ ] Registry survives `prd_metadata` singleton overwrite

### US-003: Completion Reports Other Available PRDs
**As a** developer who finishes one PRD's tasks
**I want** to see what other PRDs have remaining work
**So that** I can decide what to work on next

**Acceptance Criteria:**
- [ ] When all tasks for current prefix are done, query `prd_registry` for other prefixes
- [ ] For each other prefix, count remaining tasks
- [ ] Print summary: "Other PRDs with remaining tasks: {prefix} ({file}): {N} remaining"
- [ ] Exit with `Completed` status (user restarts with different PRD file)

---

## 4. Functional Requirements

### FR-001: Prefix-Based Task Selection Filtering
`select_next_task()` filters TODO tasks by prefix when provided.

**Details:**
- New param: `task_prefix: Option<&str>`
- `get_todo_tasks()` appends `AND id LIKE ?` when prefix is `Some(p)` and `p` is non-empty
- Bind value: `format!("{}-%", prefix)`
- `get_completed_task_ids()` remains unscoped (cross-PRD deps are harmless)

**Validation:**
- Test: two prefixes in DB, scoped selection returns only matching prefix
- Test: `None` prefix returns all tasks

### FR-002: Scoped Engine Queries
All engine.rs queries that determine loop behavior are scoped.

**Details:**
- Remaining-tasks count (line ~269): add `AND id LIKE ?`
- Mid-loop auto-recovery (line ~296): add `AND id LIKE ?`
- Startup recovery (line ~596): move after `read_prd_metadata()` (line 624), add prefix filter
- Output scan (`scan_output_for_completed_tasks`, line 1494): add `AND id LIKE ?`
- External repo reconciliation (line 1572): add `AND id LIKE ?`

**Validation:**
- With mixed-prefix tasks in DB, loop only processes current PRD's tasks

### FR-003: PRD Registry Table
New `prd_registry` table persists all imported PRDs.

**Details:**
- Schema: `id AUTOINCREMENT`, `task_prefix TEXT UNIQUE`, `json_file_path TEXT`, `project TEXT`, `branch_name TEXT`, timestamps
- Populated during `init` via new `register_prd_in_registry()` function
- Uses `INSERT OR REPLACE` keyed on `task_prefix`
- Index on `task_prefix` for fast lookup

**Validation:**
- Import two PRDs, verify both appear in `prd_registry`
- Re-import first PRD, verify entry is updated not duplicated

### FR-004: Other-PRD Discovery on Completion
When current PRD's tasks are all done, report other PRDs.

**Details:**
- New function `find_other_prd_work(conn, current_prefix)` queries `prd_registry`
- For each prefix != current, counts remaining tasks via `id LIKE '{prefix}-%'`
- Returns `Vec<(prefix, json_path, remaining_count)>` for non-zero entries
- Called from the "All tasks complete!" path in `run_iteration()`

**Validation:**
- With two PRDs imported and one completed, loop prints remaining PRD info

---

## 5. Non-Goals (Out of Scope)

- **Interactive PRD switching mid-loop**: Automatically starting the next PRD requires interactive prompts and state management. Reason: complexity; users can restart manually.
- **`--prefix` CLI flag for `task-mgr next`**: Useful but separate feature.
- **Refactoring `prd_metadata` to multi-row**: The registry approach sidesteps this. The singleton remains for "active session" metadata.
- **Cross-PRD dependency support**: Each PRD is self-contained. No need for cross-PRD dep resolution.

---

## 6. Technical Considerations

### Affected Components

| File | Changes |
|------|---------|
| `src/commands/next/selection.rs` | Add `task_prefix` param to `select_next_task()` + `get_todo_tasks()` |
| `src/commands/next/mod.rs` | Add `task_prefix` param to `next()` |
| `src/loop_engine/prompt.rs` | Add `task_prefix` field to `BuildPromptParams` |
| `src/loop_engine/engine.rs` | Scope 5 queries, move startup recovery, add `find_other_prd_work()` |
| `src/main.rs` | Pass `None` to CLI `next` callsite |
| `src/commands/next/tests.rs` | Update ~12 callsites, add 2 new tests |
| `src/loop_engine/prompt.rs` (tests) | Update ~8 `BuildPromptParams` construction sites |
| `src/db/migrations/v8.rs` | **New** — `prd_registry` table |
| `src/db/migrations/mod.rs` | Register v8, bump `CURRENT_SCHEMA_VERSION` to 8 |
| `src/commands/init/import.rs` | Add `register_prd_in_registry()` |
| `src/commands/init/mod.rs` | Call registry registration during init |

### Dependencies
- `rusqlite` (existing) for SQL queries
- Migration framework (existing) for v8

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| **A: Prefix filtering in SQL** (`LIKE ?`) | Minimal changes, uses existing prefix, no schema changes for task filtering | LIKE pattern may not be index-friendly; assumes consistent prefix format | **Preferred** |
| **B: Add `prd_id` FK to tasks table** | Cleaner relational model, exact filtering | Requires v8 migration on tasks table, many callsite changes, breaks init flow | Rejected — too invasive for the benefit |
| **C: Separate DB per PRD** | Complete isolation | Breaks existing DB-per-project model, complicates cross-PRD queries | Rejected — architectural overhaul |

**Selected Approach**: A — Prefix filtering in SQL. The `LIKE '{prefix}-%'` pattern is reliable because prefixes are always 8-char hex strings followed by a hyphen. The `prd_registry` table (new) handles metadata preservation separately.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `LIKE` pattern matches wrong tasks if prefix contains SQL wildcards | Medium — wrong tasks selected | Very Low — prefixes are hex strings | Escape `%` and `_` in prefix before LIKE, or validate prefix is alphanumeric |
| Old DBs have tasks without prefix (raw IDs like `US-001`) | Medium — prefix filter excludes them | Low — only if `PrefixMode::Disabled` was used | When `task_prefix` is `None`, no filter applied |
| `prd_registry` table doesn't exist in old DBs | Low — discovery fails silently | Medium — pre-v8 databases | Guard `find_other_prd_work()` with table-existence check; return empty on error |

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
|----------------|-----------|-------------------|-----------------|-------------|
| `init/import::register_prd_in_registry()` | `(conn, task_prefix, json_file_path, project, branch_name)` | `Ok(())` | `TaskMgrError` | INSERT OR REPLACE into `prd_registry` |
| `engine::find_other_prd_work()` | `(conn, current_prefix: Option<&str>)` | `Vec<(String, String, i64)>` | — (returns empty on error) | None (read-only) |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|----------------|-------------------|-------------------|-----------|-----------|
| `selection::select_next_task()` | `(conn, after_files, recently_completed)` | `(conn, after_files, recently_completed, task_prefix)` | Yes | Add `None` to all callsites |
| `next::next()` | `(dir, after_files, claim, run_id, verbose)` | `(dir, after_files, claim, run_id, verbose, task_prefix)` | Yes | Add `None` to CLI callsite |
| `prompt::BuildPromptParams` | 11 fields | 12 fields (+`task_prefix`) | Yes | Add `task_prefix: None` to all construction sites |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|-----------|-------|--------|------------|
| `next/mod.rs:145` | Calls `select_next_task()` | BREAKS — new param | Add `task_prefix` param, thread from `next()` |
| `main.rs:140` | Calls `next()` | BREAKS — new param | Pass `None` (CLI stays unscoped) |
| `prompt.rs:77` | Calls `next::next()` | BREAKS — new param | Thread from `BuildPromptParams.task_prefix` |
| `engine.rs:249` | Constructs `BuildPromptParams` | BREAKS — new field | Add `task_prefix` from `run_iteration()` param |
| `engine.rs:269` | Remaining-tasks query | Behavior change — scoped | Add prefix filter |
| `engine.rs:296` | Auto-recovery UPDATE | Behavior change — scoped | Add prefix filter |
| `engine.rs:596` | Startup recovery UPDATE | Behavior change — moved + scoped | Move after metadata read, add filter |
| `engine.rs:1494` | Output scan query | Behavior change — scoped | Add prefix filter |
| `engine.rs:1572` | External repo query | Behavior change — scoped | Add prefix filter |
| `next/tests.rs:62-297` | ~12 calls to `select_next_task()` | BREAKS — new param | Add `None` to all test calls |
| `prompt.rs:440-2166` | ~8 `BuildPromptParams` constructions | BREAKS — new field | Add `task_prefix: None` to all |

### Inversion Checklist
- [x] All callers identified and checked (13 `select_next_task`, 2 `next`, 9 `BuildPromptParams`)
- [x] Queries that depend on task scope reviewed (5 engine queries need scoping)
- [x] Tests that validate current behavior identified (~12 selection tests, ~8 prompt tests)
- [x] Different semantic contexts discovered: CLI `next` (unscoped) vs loop `next` (scoped)

---

## 7. Open Questions

- [ ] Should `task-mgr status` also scope its task counts by prefix? (Currently shows all tasks)
- [ ] Should `find_other_prd_work()` also show task counts per-status (todo/in_progress/blocked)?

---

## Appendix

### Glossary
- **task_prefix**: Auto-generated 8-char hex prefix prepended to task IDs during import (e.g., `3c39387a`)
- **prd_metadata**: Singleton table (`id = 1`) storing the active PRD's metadata
- **prd_registry**: New table tracking all imported PRDs by prefix
- **append mode**: Init mode where new tasks are added without deleting existing ones
