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
