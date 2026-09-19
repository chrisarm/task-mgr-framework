# commands/

## PR-2 update load-merge-write (CONTRACT-001)

`task-mgr update` is a dedicated load-merge-write path: overlay stays
`serde_json::Value`, DB writes are a **partial** `UPDATE tasks SET` (present
whitelist columns + `updated_at` only), and JSON writes go through
`prd_json::patch_user_story`.

Hard rules (do not re-derive):

- Never call `init::import::update_task` or `delete_task_relationships`.
- Never SET `status`, `archived_at`, `priority`, or `id`.
- Overlay `id` is lookup-only; JSON merge **skips** overlay `id` (story `id`
  byte-identical); `dependsOn` JSON is written unprefixed via
  `strip_prefix_in_id_array`.
- JSON-only overlay (`id` + `humanReviewOutcome` only, including `null`
  remove) cannot `Ok` skip — missing path / patch `Err` → `invalid_state`.
  Mixed overlays keep pin 11 (DB commits first; JSON failure is warn/skip).
- Write-path: `default_prd_roots(db_dir)` →
  `resolve_context_with_roots(..., "update", Some(&source_root), Some(&worktree_root))`.
  Do not import `commands::add`; do not use bare `resolve_context` as the
  write-path resolver. Every `invalid_state` on this path names `"update"`.
- `preflight_from_json_path(path, command: &str)` and `refuse_unpinned_write`
  live in `context.rs`. Add and update both call them. Reusing add's old
  private preflight ships leftover `"add"` on missing/directory pins.
- Overlay clap is `Commands::Update`. `pub use run::update` is run-session
  only. Dispatch overlay via `commands::update::update` — never the re-export.
- Merge/unknown-key whitelist SSoT is `prd_json::OVERLAY_WHITELIST` (one
  `pub(crate)` const). Do not keep a second literal array in `update.rs`.

Full copy-pasteable signatures, partial-UPDATE column list, and the
JSON-only vs mixed failure split live under `## CONTRACT-001` in
`tasks/progress-a8855e28.txt`.

## PR-2 overlay whitelist + reject (CONTRACT-002)

`task-mgr update` overlays are `serde_json::Value` objects validated in
`update.rs` (not `init::parse`). Validation order is load-bearing:
top-level `status`/`passes` (any value) → lifecycle `invalid_state("update", …)`
→ other unknown keys (name **all**) → type/null table bound to named
`PrdUserStory` fields (fail-closed; do not reuse serde defaults).

Hard rules (do not re-derive):

- Whitelist + lookup `id` only; `priority` / `synergyWith` / `batchWith` /
  `conflictsWith` / `newId` / `renameTo` are unknown (not “coming soon”).
- Both `estimatedEffort` and `difficulty` → ambiguous hard-error; either
  alone SETs `difficulty` and JSON-patches as `estimatedEffort`.
- Overlay `requiresHuman` / `maxRetries` are stricter than serde `Option`
  (`null` rejects; no default-to-3). Nested `passes` inside
  `humanReviewOutcome` is allowed; top-level `passes` is not.
- Do **not** set `deny_unknown_fields` on `PrdUserStory` (breaks old PRD
  import). Do **not** `from_value::<PrdUserStory>(overlay)`.

Full whitelist, validation order, and type/null table live under
`## CONTRACT-002` in `tasks/progress-a8855e28.txt`.

## PR-2 humanReviewOutcome JSON-only (CONTRACT-003)

`humanReviewOutcome` is a JSON-only CLARIFY payload: `Option<Value>` on
both `PrdUserStory` and `AddTaskInput` (serde camelCase;
`default` + `skip_serializing_if = "Option::is_none"`).
`into_prd_user_story` must copy it so spawned CLARIFY rows do not drop the
key. Opaque object — do not schema-validate inner keys.

Hard rules (do not re-derive):

- **No** `tasks.human_review_outcome` column, migration, or `models::Task`
  field (pin 17). `PRAGMA table_info(tasks)` must never list it.
- `init::import::update_task` SQL stays unchanged — the field rides unused
  during DB SET (success, not a missing bind).
- JSON-only overlay persist is `patch_user_story` or `invalid_state` —
  never `Ok` skip (split with CONTRACT-001). Mixed overlays stay pin 11.
- JSON-only `invalid_state` is missing/empty write path — **not**
  `--no-prefix` (that still has a `task_list`). verify-task-mgr sandboxes
  are not linked worktrees; live-path stays rust tests.

Full field attrs, add-copy, no-column rule, and persist split live under
`## CONTRACT-003` in `tasks/progress-a8855e28.txt`.

## PR-3 scoped export dump (CONTRACT-001)

`task-mgr export` dumps a **scoped** `ExportedPrd` (lossy: no `taskPrefix`,
status collapsed to `passes`). Default source is the active prefix;
`--from-json` pins an already-registered **source** (never the dest);
`--all` restores today's all-unarchived + `prd_metadata ORDER BY id ASC
LIMIT 1`.

Hard rules (do not re-derive):

- `load_tasks(conn, prefix: Option<&str>)` — `None` / empty → all
  unarchived (no LIKE); non-empty → `archived_at IS NULL AND id LIKE ?
  ESCAPE '\'` via `db::prefix::make_like_pattern` (trailing dash).
- `load_prd_metadata` scopes: `Unscoped` (`LIMIT 1`), `NamedPrefix`
  (`WHERE task_prefix = ?`), `ByPrdId` (empty-prefix `--from-json` —
  identity-matched `prd_files.prd_id`). **Grep `export/`: no `WHERE
  task_prefix IS NULL`.**
- Promote `find_registered_by_path_identity` to
  `Option<(prd_id, prefix)>`. Match (a) stays separate; empty-prefix
  metadata uses pin-19 identity `prd_id`, not `IS NULL`.
- `ExportOpts { to_json, with_progress, learnings_file, from_json, all,
  force }`. Scope selection ignores env when `--all`; never write
  `ctx.prd_json_path`. `Ok(None)` from `resolve_context` is the
  no-active error (names `--from-json` / `--all` / `current`).
- Overwrite-guard / `--force` / `LockGuard` / `unique_tmp` → PR-3
  CONTRACT-002 (same progress log).

Full copy-pasteable signatures, SQL shapes, empty-prefix `prd_id` rule,
and `--all` LIMIT 1 live under `## CONTRACT-001` in
`tasks/progress-c3c1c195.txt`.

## PR-3 export overwrite-guard / `--force` dump (CONTRACT-002)

`task-mgr export --to-json PATH` onto a registered `task_list` (pin-19
live-path pair) always requires `--force`, even when scoped to that PRD.
`--force` is a lossy pretty `ExportedPrd` dump (`unique_tmp_path` +
rename), not a merge. `LockGuard` is acquired **inside** `export()` only,
after `dest.is_file()`, before identity re-check and write. Missing dest
→ no lock. Dest is the `--to-json` PATH (never `cli_write_path`).

Hard rules (do not re-derive):

- Guard uses `find_registered_by_path_identity` only — **not** match (a).
  Stray same-`taskPrefix` copies are not registered.
- Dest identity roots come from [`prd_roots_for_dest_identity`](context.rs)
  (`worktree_root` from dest_canon when dest is in a linked worktree). Do
  **not** pass cwd-only [`default_prd_roots`] into the dest probe — from
  main cwd an absolute `--to-json` at a worktree live JSON would miss
  pin-19 (c) and overwrite without `--force`.
- Same-PRD dest and `--all` onto a registered path still need `--force`.
- Directory dest → error before dump. Dest exists + identity miss → lock
  then write without `--force`.
- `write_json_atomic` (dest and `--learnings-file`) uses
  `prd_json::unique_tmp_path`, never `with_extension("json.tmp")`.
- `main.rs` Export arm forwards `ExportOpts` only — **never**
  `LockGuard::acquire` (non-reentrant flock). Match `add()`: lock inside
  the command after validation / existence check.
- Coupling: `prd_json` does not import `export`; `export` does not import
  `add` / `update`; no `preflight_from_json_path` from export.
- Refuse copy: `invalid_state("export", …)` naming `--force` and
  dump-not-merge; dest bytes identical; no serialize-then-refuse.
  Operator UX via `ui::emit` / `ui::emit_err`, never tracing.

Full algorithm, edge table, known-bad discriminators, and grep checklist
live under `## CONTRACT-002` in `tasks/progress-c3c1c195.txt`.
