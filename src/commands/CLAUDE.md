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
