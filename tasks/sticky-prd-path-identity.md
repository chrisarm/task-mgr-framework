# Sticky PRD path identity (prevent dual-prefix / JSON prefix-reversion)

**Type**: plan-tasks lean brief  
**Branch**: `feat/sticky-prd-path-identity`  
**Task list**: `tasks/sticky-prd-path-identity.json`  
**Prompt**: `tasks/sticky-prd-path-identity-prompt.md`  
**Source**: approved session plan (architect review + operator decisions)

## Problem

A single PRD JSON can end up registered under two `prd_metadata` rows (e.g. `be46d0e4` and `394cd46c`). `loop run` then keeps rewriting `taskPrefix` in the file to the Auto-generated hash, undoing operator edits within seconds.

Two independent defects combine:

1. **Prefix is re-derived on every Auto init**, not looked up from the already-registered file. `PrefixMode::Auto` always does `md5(branchName + ":" + filename)[:8]`, ignores JSON `taskPrefix`, and `write_prefix_to_json`s the hash. The orchestrator re-imports whenever the file hash changes, so a mid-loop JSON prefix edit is treated as “re-apply Auto.”
2. **`prd_files` does not uniquely identify a file.** Schema is `UNIQUE(prd_id, file_path)` only. `register_prd_files` strips against `.task-mgr/tasks` (almost never a prefix of `tasks/*.json`) and stores whatever the caller passed — relative on `loop init tasks/foo.json`, absolute after `loop run` canonicalizes. Same file, two strings, two `prd_id`s.

`prd_metadata` upserts on `task_prefix`, so a new generated prefix creates a new PRD row instead of binding the file to the existing one.

Docs/CLI still claim Auto honors JSON `taskPrefix`. Tests assert the opposite (`test_init_auto_prefix_ignores_json_field`).

## Invariant

> **One path identity → at most one `prd_id` → one frozen `task_prefix`.**
>
> Prefix is chosen at first registration. Re-import, `loop run`, worktree remap, relative vs absolute, and JSON edits must not create a second row or silently change the prefix.

## Decisions (frozen)

- **First Auto registration = hash** (`md5(branchName:filename)[:8]`), then freeze. JSON `taskPrefix` is **not** read on first Auto. Human names stay `--prefix` / `PrefixMode::Explicit`.
- **Twins at runtime = refuse** until `task-mgr doctor` (no `LIMIT 1`, no third row).
- **`--force` hatch archives, does not hard-delete tasks** (`archived_at`). Union of identity prefixes + about-to-apply. No file moves. Refuse if loop lock held.
- **`--no-prefix` then Auto loop run refuses** (NULL identity cannot be Auto-run).

## In scope

- Pin-19 path identity (`remap_into_worktree` + `paths_identify`) in `src/git/mod.rs`; returns `(prd_id, prefix)`
- Source-root-relative `prd_files` storage + a **read** helper for archive / add / current / export
- Thread `source_root` + worktree root into `init`
- Single prefix resolver used by pre-lock, init, worktree re-import, orchestrator re-import
- Sticky identity; restore JSON to registered prefix on re-import
- `--force` union + archive-not-delete + same-prefix unarchive/update (no UNIQUE crash)
- Doctor split-brain check; auto-fix only the side with zero unarchived tasks
- CLI/rustdoc/best-practices aligned to `--prefix` > registered identity > hash > write-back
- verify-task-mgr feature recipes for init sticky-prefix and doctor twins

## Out of scope

- Full `commands/context.rs` extraction / agent-task-ops PR-1–3 (identity helper only; later PRs import it)
- New `reprefix` command
- Changing `/prd-tasks` (keep emitting the hash)
- Naive `UNIQUE(file_path)` in v1 (dirty DBs would fail migrate)
- Repairing a live split-brain DB except via doctor
- Multi-file `init()` collapsing N JSONs onto the first file’s `prd_id` ([4601]) unless this change makes it worse — then refuse

## Success bar

- Same file via relative, absolute, or worktree path → one `prd_id`, one prefix
- First Auto still ignores JSON `taskPrefix` (existing test stays)
- Re-import / JSON prefix edit restores registered prefix; never inserts row 2
- Twins: init/loop refuse + doctor hint
- `--force` archives old tasks, drops metadata, registers one; does not move JSON files
- `pre_lock` lock name equals the prefix that actually runs
- verify-task-mgr drives init-and-import + status-and-doctor recipes for the new behavior

## Key files / subsystems

- `src/git/mod.rs` — remapper + identity (only implementation)
- `src/commands/init/{mod,import}.rs` — resolver, register/read paths, force union
- `src/loop_engine/startup.rs` — pre-lock from resolver; pass roots into init
- `src/loop_engine/orchestrator.rs` — re-import already calls init (behavior drops out)
- `src/loop_engine/archive.rs`, `src/commands/add.rs` (`locate_prd_json`) — consume read helper
- `src/commands/doctor/{checks,fixes,output}.rs`
- `src/cli/commands.rs` — help text
- `.claude/skills/verify-task-mgr/features/{init-and-import,status-and-doctor}.md`

## Design (implement this)

### Path identity (pin-19, learning [5576])

Against `prd_files` rows with `file_type = 'task_list'`: join relative `file_path` to `source_root`; hit if `canonicalize(live) == canonicalize(resolved)` **or** `canonicalize(live) == remap_into_worktree(registered, source_root, worktree_root)`.

`remap_into_worktree`: join relative registered to `source_root` first; `strip_prefix` miss returns resolved. Pure path math: no `exists()`, no basename search, **no dest canonicalize**.

2+ matches → refuse + doctor hint.

### Stored path form

Store **source-root-relative POSIX** paths. Never worktree-absolute. Never `strip_prefix(.task-mgr/tasks)`. Prompt rows same rule.

`init()` must receive `source_root` and worktree root (loop `source_root` ≠ `db_dir`). Tests may pass `InitOpts::default()` which falls back to `main_repo_root_at(db_dir)` / parent of `db_dir`. Production startup always passes `run_config.source_root` and `working_root`.

### Prefix resolution order (single resolver)

1. Identity lookup → registered prefix; restore JSON if missing/different (`write_prefix_to_json` must not swallow errors). `branchName` edits do not mint a new hash.
2. Else first registration: Explicit / Disabled / Auto-hash+write-back.
3. Never `INSERT` a new `prd_metadata` row for a known identity.

Mismatch refuses unless `--force`: Explicit(other); `--no-prefix` on non-NULL identity; Auto loop run on NULL identity.

### `--force`

Union of identity prefixes (incl. NULL) **and** about-to-apply. For each: `soft_archive_by_prefix` + `UPDATE tasks SET archived_at` (like `archive_prd_data`) + delete relationships/files/metadata. **Do not move JSON/prompt files.** Then register one new `prd_id`. Same-prefix `--force` must unarchive/update matching archived IDs (`get_existing_task_ids` includes archived — UNIQUE would crash on insert). Refuse if loop lock held. Dry-run lists both prefixes and archive counts. Do **not** call `task-mgr archive`.

### Doctor

Report path-identity twins with unarchived task counts. Auto-fix only when one side has `COUNT(*) … archived_at IS NULL = 0`. Any live row (including all-irrelevant/all-done) → report only. Never move files.

### Docs

`--prefix` > registered identity > hash > write-back. Keep `test_init_auto_prefix_ignores_json_field` for **first** registration.

## Notes for review

- Do not reopen “honor JSON on first Auto” — frozen as hash-then-freeze (batch uniqueness).
- Do not add `UNIQUE(file_path)` in this slice if dirty rows exist; application refuse + DB lock is v1.
- Identity helper must be the only pin-19 implementation (later `context.rs` imports it; do not fork).
- `archive` of a twin can move the live JSON — doctor/force must never go through that command.
- Learnings: [4965] [5576] [5596] [1486] [516] [1505] [5624] [4601] [319]
