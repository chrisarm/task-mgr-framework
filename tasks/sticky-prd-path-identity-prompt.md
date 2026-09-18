# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Sticky PRD path identity** for **task-mgr**.

## Problem Statement

A single PRD JSON can be registered under two `prd_metadata` rows (different `task_prefix` values). `loop run` re-derives Auto prefix from `md5(branchName:filename)[:8]` on every init/re-import, writes it back into the JSON, and `prd_files` uniqueness is only `(prd_id, file_path)` — relative vs absolute (worktree-canonical) paths create twins.

**Invariant:** one path identity → at most one `prd_id` → one frozen `task_prefix`.

**Frozen decisions:**

- First `PrefixMode::Auto` registration **always hashes**; JSON `taskPrefix` is not read. Then freeze.
- Twins at runtime **refuse** until `task-mgr doctor` (no `LIMIT 1`).
- `--force` **soft-archives** tasks (`archived_at`); does not `DELETE FROM tasks`; does not move JSON files; union of identity prefixes + about-to-apply.
- `--no-prefix` then Auto loop run **refuses**.

Lean brief: `tasks/sticky-prd-path-identity.md`.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic. If a **Project Verification Skills** section applies to this task, follow that skill after the language gate.

---

## Priority Philosophy

In order: **PLAN** → **PHASE 2 FOUNDATION** → **FUNCTIONING CODE** → **CORRECTNESS** → **CODE QUALITY** → **POLISH**.

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production).

**Prohibited outcomes:**

- Honoring JSON `taskPrefix` on first `PrefixMode::Auto` (batch uniqueness of raw IDs; `test_init_auto_prefix_ignores_json_field` must stay)
- `LIMIT 1` on path-identity twins (must refuse + doctor hint)
- Naive `UNIQUE(file_path)` migration that fails on existing relative+absolute twins
- Calling `task-mgr archive` / moving JSON files to collapse twins or `--force`
- `drop_existing_data` hard-`DELETE FROM tasks` on the `--force` reprefix hatch (must soft-archive `archived_at`)
- Forking pin-19 identity math outside `src/git/mod.rs`
- Deriving `source_root` only from `db_dir` in production loop paths (loop `source_root` ≠ `db_dir`)
- `let _ = write_prefix_to_json` (restore-on-disagree must surface errors)
- `remap_into_worktree` calling `exists()` or `canonicalize(dest)`
- Seeding identity tests with a bare basename unless that is what init stored
- Manual edits to `tasks/*.json` for status (use task-mgr CLI / task-status tags)
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context
- Putting a `model` field on any task or at PRD top level

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top.

- No warnings in `cargo check` output
- No warnings in `cargo clippy --all-targets -- -D warnings`
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() in production code paths
- Identity helper is the only pin-19 implementation (later `context.rs` must import it, not copy it)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything global is already embedded in **this prompt file**. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/sticky-prd-path-identity.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need | Command |
| ---- | ------- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` |
| List remaining tasks (debug only) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add follow-up (review spawns) | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| ---- | ------- |
| `tasks/sticky-prd-path-identity-prompt.md` | This prompt (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — tail / append |

**Reading progress** — never Read the whole log:

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

---

## Your Task (every iteration)

1. **Work the task in `## Current Task`** — claimed by the loop. If none eligible, `<promise>BLOCKED</promise>`.
2. **Pull only needed progress context** (tail or grep one task).
3. **Recall** — `task-mgr recall --for-task <TASK-ID>`. Never Read full `CLAUDE.md`; grep sections.
4. **Verify branch** matches `feat/sticky-prd-path-identity`.
5. **Think then implement** (code + tests together). For `modifiesBehavior: true`, follow the protocol below.
6. **Scoped quality gate** (below). If a verification skill covers this task, follow it after the language gate. Fix before commit.
7. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).
8. **Emit** `<task-status><TASK-ID>:done</task-status>`.
9. **Append progress** one block terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

When a task declares `modifiesBehavior: true`:

1. Read the specific callers named in the task description.
2. Decide per-caller: OK / BREAKS (split via `task-mgr add --stdin`) / NEEDS_REVIEW.
3. **FEAT-001** callers of `init()` and of stored `prd_files.file_path`: `main.rs` loop/batch init, `startup.rs` (step 5 + 8.5), `orchestrator.rs` re-import, `archive.rs` `discover_archivable_files` / `query_prd_files`, `add.rs` `locate_prd_json`. Keep 7-arg `init()` as a Default wrapper. Archive must not `tasks_dir.join` a source-root-relative path. Do not change export.
4. **FEAT-002** callers of init prefix selection only (`init/mod.rs`, `import.rs`). Mismatch refuses; first Auto still hashes.
5. **FEAT-002b** callers of `startup.rs` pre_lock + Step 8.5, `orchestrator.rs` re-import, `batch.rs` `generate_prefix`: must use the sticky resolver; Step 8.5 must call `remap_into_worktree`; Auto loop run on NULL identity refuses.
6. **FEAT-003** callers of `--force` / `drop_existing_data`: prefix-scoped force must archive not hard-delete; refuse if **any** union prefix's loop lock is held; global wipe (`force_prefix None`) is legacy — do not silently change it without tests. Do not apply learning [1165] to this hatch.

---

## Quality Checks

### Per-iteration scoped gate

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
# Scope examples:
cargo test --lib git
cargo test --lib commands::init
cargo test --lib commands::doctor
cargo test --lib loop_engine::archive
```

**Do NOT** run the entire unscoped workspace suite during regular FEAT iterations — that is REVIEW-001's job. A scoped run is a development convenience; before the completion commit still prefer the project's full floor if `bin/gate` exists.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

If ANY test fails — including pre-existing — REVIEW-001 fixes them (or spawns FIX-xxx if >~12 unrelated).

---

## Key Learnings

- **[4965]** Auto first-registration prefix is `md5(branchName:filename)[:8]` — keep that for first init only; do not re-apply on every re-import.
- **[5576]** Pin-19 identity is (b)+(c) only: canonicalize + `source_root.join` + remap. Prefix OR is a separate match (a).
- **[5596]** `--from-json` is a pin, not an import; do not call `register_prd_files` from add.
- **[1486]** Lock prefix must equal the prefix that will actually run.
- **[516]** `prd_metadata.task_prefix` UNIQUE allows multiple NULLs — identity must return `prd_id`.
- **[1505]** FK delete order: `prd_files` before `prd_metadata`.
- **[5624]** `--no-prefix` can still leave a prefix in metadata — mismatch must refuse.
- **[4601]** batch init collapses to first file's prefix — do not worsen; refuse mixed identity prefixes in one `init()`.
- **[319]** LIKE patterns must use `make_like_pattern` (`prefix-%` + ESCAPE), never `prefix%`.

---

## CLAUDE.md Excerpts

- Database: `.task-mgr/tasks.db` per worktree; migrations under `src/db/migrations/v*.rs` (most recent v21). New schema work is v22 only if needed; this effort prefers application refuse over UNIQUE migrate.
- Parallel-slot worktrees: `…-slot-N`; slot 0 reuses the feature worktree. Path identity must treat feature worktree and main checkout as the same PRD file via remap.
- Mid-loop JSON sync: `loop init --append --update-existing` — that path is the re-import this change must make sticky.
- Never edit `tasks/*.json` by hand; use CLI + `<task-status>`.
- `ui::*` for product UX; `tracing` for diagnostics only (CONTRACT-LOG-001).
- Doctor orphan-branch PRDs: remediate with `archive --branch` **only** for completed PRDs — **not** for path twins (archive moves files).

---

## Project Verification Skills

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`  
  Drive the task-mgr CLI the way an operator would — isolated `--dir` + HOME sandbox, no PATH binary, no checkout `.task-mgr`. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.  
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`  
  **This change maps to:** `init-and-import` (FEAT-001, FEAT-002, FEAT-002b sticky-reinit, FEAT-003 force-union-archive, FEAT-005), `status-and-doctor` (FEAT-004 doctor-path-twins, REVIEW-001)  
  Never `loop run` / `batch run` through the helper. Prove `loop init` + `doctor` instead.

---

## Data Flow Contracts

### Stored `prd_files.file_path`

```text
SQLite TEXT
  → PathBuf
  → if absolute: as-is (legacy rows)
  → else: source_root.join(stored)   // POSIX relative, e.g. tasks/foo.json
  → remap_into_worktree(registered, source_root, worktree_root) when cwd is a linked worktree
```

**Write (FEAT-001):** `register_prd_files` stores source-root-relative POSIX. Never `strip_prefix(.task-mgr/tasks).unwrap_or(json_path)`.

**Read:** one helper used by `locate_prd_json`, archive discovery, doctor. Do not `dir.join("tasks").join(stored)` for project `tasks/*.json`.

### Path identity → prefix

```text
live JSON Path
  → find_registered_task_lists(conn, live, source_root, worktree_root)
  → Vec<(prd_id: i64, prefix: Option<String>)>
  → len==0 first registration
  → len==1 sticky prefix (restore JSON)
  → len>=2 invalid_state + doctor hint
```

`prd_metadata` upsert remains `ON CONFLICT(task_prefix)`. Known identity must **not** insert a new prefix row.

### `--force` union

```text
prefixes_to_archive = identity_prefixes ∪ { about_to_apply }
for p in prefixes_to_archive:
    soft_archive_by_prefix + UPDATE tasks.archived_at + DELETE child tables + DELETE prd_files/metadata
then insert_prd_metadata(about_to_apply) + import
same-prefix: unarchive/update existing archived ids (UNIQUE tasks.id includes archived)
```

LIKE: `make_like_pattern(prefix)` → `prefix-%` ESCAPE `'\\'`.

---

## Key Context / Reference

| Path | Role |
| ---- | ---- |
| `src/git/mod.rs` | `main_repo_root*`, `is_inside_worktree*`; add remapper + identity |
| `src/commands/init/mod.rs` | `PrefixMode`, `generate_prefix`, `write_prefix_to_json`, `init()` |
| `src/commands/init/import.rs` | `register_prd_files`, `insert_prd_metadata`, `drop_existing_data` |
| `src/loop_engine/startup.rs` | pre_lock hash, init() twice (step 5 + worktree 8.5) |
| `src/loop_engine/orchestrator.rs` | hash-change re-import |
| `src/loop_engine/archive.rs` | `archive_prd_data` (reuse for --force DB side; do not move files) |
| `src/commands/add.rs` | `locate_prd_json` |
| `src/commands/doctor/` | IssueType enum + checks/fixes |
| `tasks/prd-agent-task-ops-pr1.md` | Pin-19 seed (CONTRACT-001); do not implement that whole PRD |
| Lean brief | `tasks/sticky-prd-path-identity.md` |

### `init()` signature

Today: `init(dir, json_files, force, append, update_existing, dry_run, prefix_mode)`. Add `InitOpts { source_root, worktree_root }` with Default fallback for tests. Production startup/main **must** pass roots.

### Out of scope (do not implement)

- Full `commands/context.rs` / agent-task-ops PR-1–3
- `task-mgr reprefix` command
- Changing `/prd-tasks` prefix generation
- v22 `UNIQUE(file_path)` if dirty twins exist
- Repairing operator mw_integrations DB in this repo

---

## Review Tasks

| Review | Spawns | Focus |
| ------ | ------ | ----- |
| REFACTOR-001 | REFACTOR-FIX-xxx | Identity not copied; InitOpts not a positional pile |
| REVIEW-001 | FIX-xxx | Invariant held; full suite; verify-task-mgr |

Spawn:

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "priority": 50,
  "estimatedEffort": "medium",
  "passes": false,
  "touchesFiles": ["src/..."]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

---

## Progress Log Format

```
## YYYY-MM-DD - TASK-ID
- Done: …
- Gate: …
- Files: …
---
```
