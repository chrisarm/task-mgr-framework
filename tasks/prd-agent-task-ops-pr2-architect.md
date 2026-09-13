# Architect review: prd-agent-task-ops-pr2

**Status**: APPROVED (pass 2, 2026-09-09)
**Pass 1**: NEEDS_CHANGES (folded)
**Pass 2 reviewer**: `01a08897-0fa8-7321-b879-f9ee429a43e8`
**Questions for User**: none

Pass 2: all seven pass-1 revisions are ACs/contract text. Leftovers ≤ medium (nullable-scalar types bind to PrdUserStory; JSON-only does not bump updated_at; mixed overlay + JSON Err drops outcome under pin 11). Do not re-open authoring.

---

## Pass 1 (folded)

**Status**: NEEDS_CHANGES
**Date**: 2026-09-09
**Reviewer**: production-code-architect (`01a0888a-29a0-7e40-a9db-6cda89e8ef48`)
**Questions for User**: none

Verified against main and the PR-1 worktree: `update_task` still full-row SETs and clears `archived_at`; `PrdUserStory` still has no `humanReviewOutcome`; `error_recovery` still points at JSON + `loop init`; PR-1 `prd_json.rs` has `append_user_story` and no `patch_user_story`.

**Strengths**:
- Scope matches the ledger: `task-mgr update` + JSON-only `humanReviewOutcome` only. Export, remapper, add clap, cheatsheet/`task_ops` stay out. Pins 5–7, 10–13, 16–17, 19, 21 are treated as law.
- Approach A is the only substrate that keeps lifecycle SSoT and extra JSON keys: overlay stays `Value`, partial SQL, `prd_json::patch_user_story`, never `import::update_task` / `PrdUserStory` round-trip of an existing story.
- Pin protocol is consumed, not reimplemented: `resolve_context(..., "update")`, refuse stays outside the resolver, empty prefix skips `prefix_id`, `--from-json PATH` is never remapped away.
- Feed-forward from PR-1 is real: parameterize `atomic_write` so update errors cannot say `"add"`; `prd_json` still must not import `add`/`update`; ≥2 sandbox trap (two prefixed inits, not `--no-prefix` twice) is named.
- Empirical ACs cover the three inversion killers: notes-only vs `archived_at`, full blob `"passes": false`, extra-key survival. `lookup_hint` lying after clap grows `Update` is an explicit rewrite, not a hope.

**Concerns**:

**High — JSON-only overlay can succeed with nothing persisted (pin 11 vs pin 17).**
`humanReviewOutcome` is not a `tasks` column. Pin 11 + add’s skip path (`ctx.prd_json_path` empty → note and `Ok`; `patch_user_story` `Err` → warning, no rollback) copied onto a `{id, humanReviewOutcome}` overlay yields exit 0 and no bytes anywhere. Same for story-not-found in the file. Pin 16 “DB update OK; JSON iff one `task_list`” is correct for `notes`; it is data loss for the CLARIFY field. Split the write policy: JSON-only overlay + missing path or patch `Err` → `invalid_state` (nothing to commit). Mixed overlay (notes + outcome) + patch `Err` stays pin 11 warning.

**Medium — `patch_user_story` merge can write prefixed ids into JSON.**
`append_user_story` strips `id` / `dependsOn` to the unprefixed JSON convention. US-004 specifies match-both, not write-unprefixed. If `update.rs` `prefix_id`s the overlay then merges it, siblings become mixed-convention. `prefix_id` is idempotent so re-import will not double-prefix, but the file will not match append. AC: never copy overlay `id`; strip prefix on `dependsOn` the same way append does.

**Medium — null / wrong-type overlay values are unspecified.**
Assumption 5 clears optional scalars. `title` is `NOT NULL`; `max_retries` is `NOT NULL DEFAULT 3`; `"dependsOn": null` vs `[]` is unnamed. Blind `SET col = NULL` or merging a non-array `dependsOn` either constraint-fails or corrupts tables. Fail closed: `title` null/empty errors; null only on nullable columns; arrays must be string arrays; `requiresHuman` must be bool; wrong type is `invalid_state`, no writes.

**Medium — `delete_task_relationships` deletes every `rel_type`.**
Init no longer inserts synergy/batch/conflicts, but old rows can remain. Overlay `dependsOn` should `DELETE … WHERE task_id = ? AND rel_type = 'dependsOn'`, not the all-types helper.

**Medium — US-005 “unregistered before overlay parse” disagrees with add.**
Add: missing/directory `is_file()` before parse; unregistered via `resolve_context` after parse, after lock, before the write txn (needs DB). Require that order. Do not open a second connection only to beat parse.

**Medium — in-tree `CLAUDE.md` will keep the hand-edit recipe until enhance runs.**
Template change is SSoT, but loop agents read the fenced block. Add an AC: run `task-mgr enhance agents` (or the enhance unit test plus that write) so merge does not leave the old CLARIFY path live.

**Medium — PR-1 `context.rs` still says the ≥2 refuse is “add-only”.**
True at PR-1 merge; PR-2 makes it write-only (add **and** update). Update that comment when extracting `refuse_unpinned_write`, or implementers will skip the check on update.

**Questions for User**: none

**Suggested Revisions**:
1. **JSON-only persistence (US-001 / US-003 / US-005):** If the overlay has no DB column/table changes (only `humanReviewOutcome`, including `null` remove) and there is no write path or `patch_user_story` fails, return `Err` naming `task-mgr current` and retry `--from-json`. Do not `Ok` with a skip note. Mixed overlays keep pin 11.
2. **US-004:** Merge skips `id`. `dependsOn` written unprefixed via the same `strip_prefix_in_id_array` as append. JSON story `id` byte-identical to before the patch.
3. **CONTRACT-002 type/null table:** `title`: non-empty string (null/empty → error). Nullable scalars: null clears. `dependsOn`/`touchesFiles`/`acceptanceCriteria`/`requiredTests`: JSON array of strings (`[]` clears; null → error). `requiresHuman`: bool. `maxRetries`: integer (null → error, column NOT NULL). `humanReviewOutcome`: object or null.
4. **dependsOn SQL:** `DELETE FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'` then insert; do not call `delete_task_relationships`.
5. **Pin order:** Match add — `--from-json` missing/directory before overlay parse; unregistered / ≥2 refuse after parse, before write txn. Reuse `sole_task_list_path` + `cli_write_path` on `ctx is None`.
6. **US-006:** After the template rewrite, regenerate the managed `CLAUDE.md` block (`task-mgr enhance agents`). Flip the `context.rs` “add-only refuse” comment to write-only when the helper is extracted.
7. **Warning copy (optional tighten):** Pin-11 warning may note that a later `loop init --append --update-existing` will SET DB columns from the stale JSON; still never name `export`.
