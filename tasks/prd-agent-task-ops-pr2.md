# PRD: Agent task-ops UX PR-2 — `task-mgr update` + `humanReviewOutcome`

**Type**: Enhancement
**Priority**: P0 (Critical)
**Author**: Grok
**Created**: 2026-09-09
**Status**: Draft
**Goal ledger**: `tasks/prd-goal-agent-task-ops-ux-ledger.md` phase 2 (authoring)
**Related**: learnings **#1561**, **#3440**, **#4224**, **#3419**, **#5345**, **#3498**, **#3283**, **#3156**, **#2667**, **#3923**
**Depends on**: PR-1 public surface (`tasks/prd-agent-task-ops-pr1.md`) at merge, not main HEAD today

---

## PRD-input note (effort check)

Phase 1 of the goal ledger is **looping**, not STATUS-Done / merged. This phase is **not** duplicate or obsolete: `task-mgr update` does not exist; `humanReviewOutcome` is still stripped by `PrdUserStory`; CLARIFY docs still say “embed in the JSON then `loop init --append --update-existing`”. Pins, simplified shape (three serial PRs), and this phase seed are the answers that would otherwise have been Step 3 clarifying questions. This PRD does not add phases, change pins, or propose a different program.

**HEAD at authoring:**

| Tree | Commit | Note |
| --- | --- | --- |
| main (this checkout) | `e552877` (`Merge pull request #42 from chrisarm/chore/v0.3.3`) | no `context.rs` / `prd_json.rs` / `update.rs` |
| PR-1 worktree | `cda7c62` (`feat: a410d276-FEAT-003-completed` — `prd_json.rs` unique-tmp chokepoint) | looping; **do not edit** |

**Re-located extraction targets (PR-1 worktree if the file exists, else PR-1 PRD contracts — do not invent main-HEAD line numbers for files PR-1 will create):**

| Target | Where it is now | What is there |
| --- | --- | --- |
| `commands/context.rs` | PR-1 `src/commands/context.rs` | `ResolvedContext { prefix, source, prd_json_path }`, `ResolutionSource::FromJsonFlag`, `resolve_context(conn, from_json, command)`. `invalid_state` command-name is a parameter. `sole_task_list_path` / `cli_write_path` / `paths_identify` exist. ≥2 refuse is **not** in this module (`Ok(None)` for 0 and 2+); rustdoc still says **add-only** — this PR flips that to **write-only**. |
| `commands/prd_json.rs` | PR-1 `src/commands/prd_json.rs` | `unique_tmp_path` (pid + counter + nanos), `append_user_story`, private `atomic_write`. **Does not import `add`.** `invalid_state` still hardcodes `"add"` in `append_user_story` / `atomic_write` (feed-forward: this PRD must not ship **update** errors that say `"add"`). No `patch_user_story`. |
| `commands/add.rs` | PR-1 `src/commands/add.rs` | `AddTaskInput` (no `humanReviewOutcome`); `into_prd_user_story` does not copy it. Clap `--from-json` exists on `Commands::Add` (PR-1 `cli/commands.rs:667`). `add()` signature on this commit still has no `from_json` param — PR-1 PRD CONTRACT-002 / US-004 will add it before merge. |
| `error_recovery.rs` | **main** `src/cli/error_recovery.rs:32-45` | `WRONG_SUBCOMMAND_HINTS`: `update` / `edit` / `change` all say “no `update`/`edit` yet; edit JSON then `loop init --append --update-existing`”. `set-status` is a separate lifecycle table. Tests: `error_recovery.rs:202` + `tests/cli_tests.rs:2860-2940`. |
| `PrdUserStory` | **main and PR-1** `src/commands/init/parse.rs:16-78` | No `humanReviewOutcome`. Extra JSON keys are silently dropped on deserialize (the original bug). |
| `init::import::update_task` | **main** `src/commands/init/import.rs:437-488` | Full-row `SET` including `title`/`priority`/`notes`/… + **`archived_at = NULL`**. Caller (`init/mod.rs:505-528`) then may raw-`SET status = 'done'` from `passes` (LIFECYCLE-EXCEPTION) and **DELETE+reinsert** files/relationships. **Must not be called by `task-mgr update`.** |
| Enhance CLARIFY | **main** `src/commands/enhance/templates.rs:92-113` | “embed `humanReviewOutcome` directly in the JSON task entry”, then `loop init --append --update-existing`, then `complete`. |
| Intents JSON | **main** `src/commands/intents.rs:159-171` | Don’t hand-edit: `add --stdin`, `loop init --append --update-existing`, `<task-status>`. No `update`. |
| Verify skill | **main** `.claude/skills/verify-task-mgr/SKILL.md` + `features/` | No `update` feature file. PR-1 feature file not on this worktree commit yet. Sandboxes are **not** linked worktrees. |
| Worktree live-path tests | **main** `tests/worktree_db_resolution.rs:159-175` | `add` from worktree cwd lands in **main** `.task-mgr`. Pattern for PR-2 update live-path tests. |

**PR-1 public surface this PRD assumes at merge** (author-ahead; not that it is on main HEAD today):

- `resolve_context(conn, from_json: Option<&Path>, command: &str)` with flag → env → single-prefix → `Ok(None)` (0 **and** 2+). `--from-json` is pin, never register, never remap the write target. `ResolvedContext.prd_json_path` is the write path.
- ≥2 refuse is **write-only** (add **and** update): `ctx.is_none() && load_known_prefixes().len() >= 2`. Zero-prefix / `--no-prefix` still allows the write. `current` stays a probe. PR-1 comments may still say “add-only”; this PR flips them when extracting `refuse_unpinned_write`.
- After DB commit (mixed overlays), JSON writes go through `prd_json` on `ctx.prd_json_path` only. `ctx is None` reuses `sole_task_list_path` + `cli_write_path`. Failure copy names `task-mgr current` and retry `--from-json`, never `export`.
- Empty `ctx.prefix` skips `apply_prefix` **and** `prefix_id` (else `-FEAT-001`).
- Pin order matches add: `--from-json` missing/directory (`preflight_from_json_path` / `is_file()`) **before** overlay parse; unregistered (`resolve_context`) and ≥2 refuse **after** parse, before the write txn. Do not open a second connection only to beat parse.
- `prd_json` must not import `add`. Helpers: `sole_task_list_path`, `cli_write_path`, `strip_prefix_in_id_array`.

**Assumptions (not pins — stated so implementers do not invent):**

1. Overlay is **one JSON object** (not a `userStories` array). `id` lives **inside** the object (lookup-only). No positional CLI task-id; no `--title` / `--notes` flags.
2. ≥2-prefix write-refuse is **shared** by add and update via a helper in `context.rs` (e.g. `refuse_unpinned_write`). It does **not** move into `resolve_context` (that would break `current`). PR-1 may still have the predicate inlined in `add.rs` — PR-2 extracts or duplicates the three-line check; extract is preferred so the copies cannot drift.
3. `priority` in an overlay is an **unknown-key hard-error** (pin 21: not on the whitelist). Do not special-case a “priority not supported yet” message that implies a future flag.
4. Validation order is load-bearing: `status` / `passes` (lifecycle error, even if other unknown keys exist) → other unknown keys → type/null table (CONTRACT-002) → no whitelist field besides `id`. A pasted full story blob with `"passes": false` always gets the lifecycle pointer, not a generic unknown-key line.
5. Type/null is the CONTRACT-002 table (not “null always clears”). `title` null/empty errors. Nullable scalars: null clears. Arrays of strings: `[]` clears, **null errors**. `requiresHuman` must be bool. `maxRetries` must be integer (**null errors** — column NOT NULL). `humanReviewOutcome` must be object or null (null removes the JSON key).
6. `init --append --update-existing` **keeps** calling `import::update_task` (full-row SET + clears `archived_at` + may SET status from `passes`). That is a different verb (re-import revive). This PRD does not change that function’s SQL. `task-mgr update` is a new writer.
7. verify-task-mgr sandboxes are not linked git worktrees. Worktree JSON cases live in rust tests (`worktree_db_resolution` pattern). The skill proves clap + overlay reject + pin + CLARIFY overlay in an isolated `--dir`.
8. Enhance template is the SSoT for the managed CLAUDE.md block; do not hand-edit inside `TASK_MGR:BEGIN/END`. After the template rewrite, **run `task-mgr enhance agents`** so the in-tree fenced block matches. Intents + enhance CLARIFY + that regenerate are in scope; cheatsheet / `task_ops` jq / remaining prompt one-liners / best-practices copy are **PR-3**.
9. JSON-only overlay (`id` + `humanReviewOutcome` only, including `null` remove) has **no DB column to commit**. Missing write path or `patch_user_story` `Err` → `invalid_state` (do **not** `Ok` with a skip note). Mixed overlays (any DB column/table key + outcome) keep pin 11: DB commits first; JSON `Err` is a warning.

---

## 1. Overview

### Problem Statement

Agents cannot change task fields without hand-editing `tasks/*.json` or going through `loop init --append --update-existing`. The latter is a full-row SET that **clears `archived_at`**, DELETE+reinserts files/relationships, and (when `passes: true`) raw-SETs `status` — so a notes-only “update” would clobber priority, title, archive state, and possibly status. Clap has no `update` command; `error_recovery` still says “edit the JSON then loop init”. `humanReviewOutcome` is documented as the CLARIFY persistence path but `PrdUserStory` has no field, so serde **silently drops** it on import and on any typed round-trip (the original bug). `AddTaskInput` would also drop it on a spawned CLARIFY row.

Goal: agents can patch whitelist fields (including `humanReviewOutcome`) without editing JSON and without touching `tasks.status`.

### Background

This is **PR-2 of three serial PRs**. PR-1 (looping) ships the remapper, `commands/context.rs`, `commands/prd_json.rs`, and `add`/`current --from-json`. PR-3 is export scoping + remaining docs/prompt alignment. This PRD implements only the PR-2 slice and **assumes PR-1’s public surface at merge**.

`tasks.status` is lifecycle-only (`src/lifecycle/CLAUDE.md`: every status write goes through a `TaskLifecycle` verb; init’s `passes → done` is the one marked LIFECYCLE-EXCEPTION). `task-mgr update` must never become a second status door. Persistence for `humanReviewOutcome` is the task-list JSON, not a DB column (pin 17).

---

## 2. Goals

### Primary Goals

- [ ] `task-mgr update --stdin` / `--json` is a real command with load-merge-write. It does **not** call `init::import::update_task`.
- [ ] Overlay whitelist is applied as a partial UPDATE. Notes-only does not clobber `priority`, `title`, or `archived_at`. `tasks.status` is never written. `archived_at` is never set or cleared. `dependsOn` present deletes **only** `rel_type = 'dependsOn'` rows (never `delete_task_relationships`).
- [ ] `status` / `passes` in an overlay (including a pasted full `userStories` blob) hard-error and point at `complete` / `fail` / `skip` / `<task-status>`. Unknown overlay keys hard-error. Wrong types / illegal nulls hard-error (CONTRACT-002 table). No silent skip / drop.
- [ ] `prd_json::patch_user_story` Value-merges the existing story (unknown keys on the file survive). Merge **skips** overlay `id` (JSON story `id` byte-identical). `dependsOn` written unprefixed via `strip_prefix_in_id_array`. Never deserialize an existing story to `PrdUserStory` and write it back.
- [ ] `humanReviewOutcome: Option<Value>` on `PrdUserStory` **and** `AddTaskInput` with `skip_serializing_if = "Option::is_none"`. No `tasks.human_review_outcome` column. Survives `loop init --append --update-existing`. A spawned CLARIFY `add` does not drop the key. JSON-only overlay + missing path or patch `Err` → `invalid_state` (not `Ok` + skip).
- [ ] Same `--from-json` pin + live-path write policy as add (`ResolvedContext.prd_json_path`; `ctx is None` → `sole_task_list_path` + `cli_write_path`). Pin order matches add. Empty `ctx.prefix` skips `apply_prefix` **and** `prefix_id` on ids. Update errors name `"update"`, never `"add"`.
- [ ] `update` removed from `WRONG_SUBCOMMAND_HINTS`. `edit` / `change` point at `task-mgr update --stdin`. `set-status` stays. Enhance CLARIFY + intents use `update --stdin` then `complete <clarify-id>`. After the template rewrite, regenerate the managed `CLAUDE.md` block (`task-mgr enhance agents`). Flip `context.rs` “add-only refuse” comments to **write-only**.
- [ ] User-facing proof: create/extend a verify-task-mgr feature recipe, then drive it (do not drive before the file exists). Compile/unit tests alone are not proof.

### Success Metrics

- Notes-only overlay: `priority`, `title`, `status`, `archived_at` byte-identical; `notes` changed in DB and JSON.
- Full story blob with `"passes": false`: non-zero exit; no DB column change; no JSON write; stderr names `complete` / `fail` / `skip` / `<task-status>`.
- Unknown overlay key: non-zero exit; no writes.
- `dependsOn` / `touchesFiles` omitted → tables unchanged; present (including `[]`) → those tables replaced.
- Existing extra JSON key on the story survives a notes-only patch.
- `humanReviewOutcome` present in JSON after update and after `loop init --append --update-existing`; `PRAGMA table_info(tasks)` has no such column.
- JSON-only overlay (`{id, humanReviewOutcome}`) with no write path or `patch_user_story` `Err`: non-zero exit; `invalid_state`; no skip note; no DB write.
- Mixed overlay + JSON patch `Err`: DB committed; warning (pin 11); never `export`.
- Worktree file exists → that file is patched; `cmp` of main JSON is empty; DB row still in main `.task-mgr`.
- `--from-json PATH` writes that PATH (never remapped away).
- `task-mgr update --help` says **pin**, not import.
- `verify-task-mgr` artifacts for the new feature file exist under `.claude/skills/verify-task-mgr/artifacts/<run-id>/`.

---

## 2.5. Quality Dimensions

> Pins 5–7, 10–13, 16–17, 19, 21 are law for this PR. Pins 1–4, 8–9, 14–15, 18, 20 are cross-phase constraints: they appear here so `/prd-tasks` and later PRDs cannot contradict them. **This PRD’s stories must not implement export scoping or remapper/add clap.**

### Correctness Requirements

Pins (verbatim from the ledger):

1. `--from-json` on add/update/current/export ships as “pin this already-registered effort”. `--depended-on-by` cannot pin a worktree-only file.
2. Unregistered `--from-json` path: Refuse (`loop init` first). Identity must treat relative `prd_files` + worktree remap as registered.
3. Ambiguous prefix (≥2 non-NULL, no env, no flag): Refuse the write. Zero prefixes / `--no-prefix` still allow DB insert — loop does *not* always set `TASK_MGR_ACTIVE_PREFIX` (`PrefixMode::Disabled`).
4. Worktree JSON: Pure remap, then CLI existence check. Loop remap stays unconditional. `--from-json PATH` always writes that PATH (never remapped away).
5. `task-mgr update` is a real command with load-merge-write. Must not reuse `init::import::update_task` (full-row SET + clears `archived_at`).
6. Status via `update`: Hard-error `status` / `passes` (including a full story blob). Lifecycle SSoT; do not silently skip.
7. Unknown overlay keys: Hard-error. Silent drop is the original `humanReviewOutcome` bug.
8. Export default: Active-PRD only; `--all` restores today’s dump.
9. Overwrite a registered task-list: Always refuse without `--force`, even when scoped to that PRD. Export is a lossy dump, not a merge.
10. `tasks.status` is lifecycle-only. `update` / JSON patch never write it. `passes` in an overlay is a hard error, not an ignore.
11. JSON sync is best-effort; DB commits first. Failure copy names `task-mgr current` and retry `--from-json`, never `export`.
12. One JSON write chokepoint: unique tmp + rename (reuse `prd_reconcile::unique_tmp_path` scheme: pid + counter + nanos). Preserve unknown keys on patch. Do not deserialize an existing story to `PrdUserStory` and write it back.
13. `--from-json` never registers a PRD and never remaps the write target. It only pins an already-registered effort.
14. Live remap is path math, not discovery. No `exists()`, no basename search. Relative `prd_files` rows are joined to `source_root` before remap.
15. Loop remap stays unconditional. CLI existence checks are caller-side and must not be shared into startup.
16. Refuse-without-pin applies iff ≥2 registered non-NULL prefixes. Zero-prefix / `--no-prefix` is a different mode: DB insert OK; JSON sync only if exactly one `task_list` is registered.
17. `humanReviewOutcome` is not a DB column. Persistence is the task-list JSON; `PrdUserStory` must not strip it on import.
18. Export to a registered `task_list` is opt-in `--force`. `--force` is a dump, not a merge. Take the same `LockGuard` as add if the destination is a live PRD.
19. Path identity (canonicalize + `source_root.join` + worktree remap) is one function, used by add / update / current / export overwrite-guard.
20. Do not ship the multi-prefix refuse before clap has `--from-json` (docs already tell agents to pass it). PR-1 ships both together.
21. Out of scope: claim-scoped short `<task-status>` ids; a generic `set-status` command; `add --from-json` creating/registering a new PRD; rewriting historical `tasks/*-prompt.md`; changing DB anchoring (main checkout `.task-mgr` from a worktree stays); MCP task wrappers; putting `priority` on the update whitelist.

**PR-2-specific correctness:**

- Load-merge-write: load the existing `tasks` row + relationship/file tables; merge **only** overlay keys that are present; write a **partial** `UPDATE` (only those columns) plus conditional table replace. Never `SELECT *` into a `PrdUserStory` and full-row SET.
- Overlay `id` is required and is **lookup-only**. Never `UPDATE tasks SET id`. `patch_user_story` **skips** overlay `id` — the JSON story’s `id` string is byte-identical to before the patch. A `newId` / `renameTo` key is unknown.
- Whitelist (JSON camelCase): `title`, `description`, `notes`, `acceptanceCriteria`, `touchesFiles`, `dependsOn`, `estimatedEffort` / `difficulty`, `model`, `escalationNote`, `requiredTests`, `maxRetries`, `requiresHuman`, `humanReviewTimeout`, `claimsSharedInfra`, `reviewScope`, `severity`, `sourceReview`, `humanReviewOutcome`. Plus lookup key `id`. Nothing else.
- Overlay type/null (CONTRACT-002 — fail closed, no writes):

  | Key | Required shape | Illegal → `invalid_state`, no writes |
  | --- | --- | --- |
  | `title` | non-empty string | null, empty, non-string |
  | nullable scalars (`description`, `notes`, `model`, `escalationNote`, `severity`, `sourceReview`, `estimatedEffort`/`difficulty`, `humanReviewTimeout`, `claimsSharedInfra`, `reviewScope`) | matching JSON type, or `null` to clear | wrong type |
  | `dependsOn` / `touchesFiles` / `acceptanceCriteria` / `requiredTests` | JSON array of strings (`[]` clears) | `null`, non-array, non-string element |
  | `requiresHuman` | bool | `null`, non-bool |
  | `maxRetries` | integer | `null` (column NOT NULL), non-integer |
  | `humanReviewOutcome` | object or `null` (`null` removes the JSON key) | array, string, number, bool |

- `estimatedEffort` and `difficulty` are aliases for `tasks.difficulty` / JSON canonical `estimatedEffort`. If **both** keys are present in the overlay, hard-error (ambiguous). If either is present, SET `difficulty` and patch JSON as `estimatedEffort` (remove a leftover `difficulty` key on that story so both do not remain).
- `dependsOn` **present** (including `[]`) → `DELETE FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'` then insert the overlay values. **Do not** call `delete_task_relationships` (that helper deletes every `rel_type`; old synergy/batch/conflicts rows must survive). `touchesFiles` **present** → `delete_task_files` + insert. **Absent** keys leave those tables. `synergyWith` / `batchWith` / `conflictsWith` are unknown overlay keys, not silently ignored.
- JSON patch via `prd_json::patch_user_story` on `serde_json::Value`. Match story id with prefixed **or** unprefixed form (`strip_task_prefix`), same as `append_user_story`. Merge **does not copy** overlay `id`. `dependsOn` written unprefixed via the same `strip_prefix_in_id_array` as append. Preserve unknown keys already on the story. Preserve trailing newline.
- Same pin + live-path as add: `resolve_context(conn, from_json, "update")`. Pin order matches add: missing/directory **before** overlay parse; unregistered / ≥2 refuse **after** parse, before the write txn. After a mixed-overlay DB commit, `patch_user_story` on `ctx.prd_json_path` **only**. `--from-json PATH` → that canonical PATH. Default → remap then `is_file()` (worktree else registered else skip). `ctx is None` → `sole_task_list_path` then `cli_write_path` (JSON sync iff exactly one `task_list` that is a regular file). ≥2 prefixes + no pin: refuse **before** any DB write (shared helper with add; **write-only**, not add-only).
- **JSON-only overlay** (`id` + `humanReviewOutcome` only, including `null` remove): there is no DB column to commit. Missing write path **or** `patch_user_story` `Err` → `invalid_state("update", …)` naming `task-mgr current` and retry `--from-json`. Do **not** `Ok` with a skip note. Mixed overlays keep pin 11 (DB commits; JSON `Err` is a warning).
- Empty `ctx.prefix` skips `apply_prefix` **and** `prefix_id` on overlay `id` (lookup) and on `dependsOn` ids used for SQL. JSON `dependsOn` is still written unprefixed (`strip_prefix_in_id_array`).
- `invalid_state` command-name for every update / `patch_user_story` / `atomic_write` error this command takes is `"update"`. Parameterize `prd_json` write helpers; do not leave leftover `"add"` on the update path. `prd_json` still must not import `add` **or** `update`.
- JSON-sync failure copy (mixed overlay, pin 11): DB already committed; warning names `task-mgr current` and retry `--from-json`, never `export`. The warning **may** note that a later `loop init --append --update-existing` will SET DB columns from the stale JSON; it must still never name `export`. Same family as PR-1 US-007.
- `humanReviewOutcome` is `Option<serde_json::Value>` (opaque object; do not schema-validate the inner keys in this PR). Overlay type check is only “object or null”. `skip_serializing_if = "Option::is_none"`. Also on `AddTaskInput` and copied in `into_prd_user_story`.
- Help text for `--from-json` on Update says **pin**, not import. Distinct from `init --from-json`.
- Directory `--from-json`: `is_file()` / `preflight_from_json_path` **before** overlay parse (`canonicalize` on a dir succeeds — learning from PR-1). Unregistered is **after** parse (needs DB), still before the write txn. Do not open a second connection only to beat parse.

**Cross-phase (do not implement in this PRD; do not contradict):**

- Pins 8–9, 18 (export default / `--all` / `--force` overwrite) — PR-3.
- Pin 20 (clap `--from-json` + ≥2 refuse on **add**) — PR-1; this PR **consumes** that pin protocol for update, it does not re-ship add clap.
- Pins 1–4, 14–15 remapper / add clap — PR-1. Update **calls** `resolve_context` / `paths_identify`; it does not reimplement remap.
- Cheatsheet / `task_ops` jq `.userStories[]` / remaining prompt one-liners — PR-3. This PRD may mention `update` in `error_recovery` / intents / enhance CLARIFY only.
- `~/.claude/docs/task-mgr-best-practices.md` — operator residual BP after PR-3.

### Performance Requirements

- Best effort. One task row + optional two small table rebuilds per invocation.
- Exit early on overlay validation failure (`status`/`passes`/unknown keys/type-null/missing id) **before** `LockGuard` / write txn when possible; always before any `UPDATE`.
- `--from-json` missing/directory: exit **before** overlay parse (same as `add::preflight_from_json_path`). Unregistered / ≥2 refuse: after parse, after lock/open (needs DB), **before** the write txn. Do not open a second connection only to beat parse.
- Do not walk the worktree or search by basename.
- Do not load or rewrite the whole `userStories` array through `PrdUserStory`.

### Style Requirements

- Follow existing codebase patterns. `TaskMgrError::invalid_state(command, field, expected, actual)`. `ui::emit` / `ui::emit_err` for product UX (CONTRACT-LOG-001). No `tracing` for operator-facing overlay/refuse copy.
- No `.unwrap()` on filesystem or SQLite in `update.rs` / `patch_user_story` unless a prior invariant makes it unreachable.
- `prd_json` must not import `add` or `update`. `update` imports `context` (pin) and `prd_json` (patch). `context` must not import `prd_json` write helpers.
- Comments explain **why** (do not call `import::update_task`; `passes` is a hard error not an ignore; Value merge so unknown keys survive; JSON-only overlay cannot `Ok` a skip). Do not narrate the move.
- Flip PR-1 `context.rs` rustdoc that says the ≥2 refuse is **add-only** to **write-only** (add **and** update) when extracting `refuse_unpinned_write`. Leaving “add-only” will cause implementers to skip the check on update.
- Do not freeze `add.rs:NNN` line numbers in later tasks; grep symbols (`refuse_unpinned_write`, `preflight_from_json_path`, `sole_task_list_path`, `cli_write_path`, `strip_prefix_in_id_array`, `resolve_context`, `into_prd_user_story`).
- Scoped clap unit tests: `cargo test -p task-mgr cli::`. Binary hint tests stay `tests/cli_tests.rs`.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Notes-only overlay | `import::update_task` would clobber title/priority/`archived_at` | Only `notes` + `updated_at` change; title/priority/status/`archived_at` identical |
| Archived row, notes-only | `update_task` SETs `archived_at = NULL` (revive) | `archived_at` stays non-NULL; row stays archived |
| In-progress row, notes-only | Status door must stay lifecycle | `status` stays `in_progress` |
| Full story blob with `"passes": false` | Agents paste a `userStories[]` element | Hard-error naming `complete`/`fail`/`skip`/`<task-status>`; no writes |
| Overlay with `"status": "done"` | Second status door | Same lifecycle hard-error; no writes |
| Overlay with unknown key `foo` (and valid `notes`) | Silent drop is the original bug | Hard-error naming `foo`; **no partial apply** |
| Overlay with `priority` | Pin 21: not on whitelist | Unknown-key hard-error (not a dedicated “coming soon”) |
| Overlay with `synergyWith` / `batchWith` / `conflictsWith` | Import silently ignores these | Unknown-key hard-error on update |
| Both `estimatedEffort` and `difficulty` present | Aliases for one column | Hard-error (ambiguous) |
| `dependsOn` omitted vs `[]` vs `["X"]` | DELETE+reinsert vs leave | omit = leave; present = replace **only** `rel_type = 'dependsOn'` (empty clears); synergy/batch/conflicts rows survive |
| `dependsOn: null` / non-array / `[1]` | Blind SET/merge corrupts tables | `invalid_state`; no writes |
| `touchesFiles` omitted vs `[]` | Same | omit = leave; present = replace; `null` errors |
| `title` null or `""` | Column NOT NULL | `invalid_state`; no writes |
| `maxRetries: null` | Column NOT NULL DEFAULT 3 | `invalid_state`; no writes (do not fall back to 3) |
| `requiresHuman: 1` / `"true"` / null | Must be JSON bool | `invalid_state`; no writes |
| Existing extra JSON key on the story | Pin 12 / 17 | Notes-only patch leaves the extra key |
| JSON-only overlay, no write path (`ctx.prd_json_path` empty / no sole `task_list`) | Pin 11 skip would persist nothing (pin 17) | `invalid_state` naming `current` + retry `--from-json`; **not** `Ok` + skip note; no DB write |
| JSON-only overlay, `patch_user_story` `Err` (story missing in file / IO) | Same data-loss | `invalid_state`; no DB write |
| Mixed overlay (`notes` + `humanReviewOutcome`), JSON `Err` | Pin 11 | DB commits; warning; never `export`; may mention later `loop init --append --update-existing` will SET from stale JSON |
| `humanReviewOutcome` only overlay, write path OK | CLARIFY recipe | JSON gains the object; no `tasks.*` column; other fields unchanged |
| `humanReviewOutcome: null` | Clear outcome | Key removed from JSON story; DB unchanged (JSON-only → still requires a write path) |
| Overlay `id` copied into JSON | Mixed prefixed/unprefixed siblings | Merge **skips** `id`; JSON story `id` byte-identical to before the patch |
| Prefixed overlay `dependsOn` merged as-is | File would not match append | Written unprefixed via `strip_prefix_in_id_array` |
| Spawned `add` of a CLARIFY row that includes the key | `AddTaskInput` would drop it today | Key present in the appended JSON object |
| `loop init --append --update-existing` after update | Import used to strip the field | JSON still has the key; `PRAGMA table_info(tasks)` has no `human_review_outcome` |
| Empty `ctx.prefix` (`--no-prefix` / NULL-prefix pin) | `prefix_id("", id)` → `-FEAT-001` | Skip `apply_prefix` **and** `prefix_id`; lookup/patch unprefixed id |
| ≥2 prefixes, no env, no `--from-json` | Pin 3 / 16 | Refuse; no DB write; no JSON write. `current` still probe |
| `--no-prefix` / 0-prefix DB | Pin 16 | Update of an existing row OK; JSON sync iff exactly one `task_list` |
| `--from-json PATH` | Pin 4 / 13 | Write that PATH; never remapped away |
| Linked worktree, worktree file exists | **#4237** / **#4441** | Patch worktree copy; main JSON bytes unchanged; DB in main `.task-mgr` |
| Linked worktree, only main file exists | Inventing a path is data loss | Patch main; no basename search; no create |
| Directory / missing `--from-json` | `canonicalize` on a dir succeeds | Error **before** overlay parse; no DB |
| Unregistered `--from-json` | Needs DB (identity) | Error **after** parse, before write txn; copy names `loop init`; `SELECT` unchanged. Do not open a second connection only to beat parse |
| Overlay `id` not in DB | Lookup | Error before writes |
| Overlay `id` in DB, not in JSON, **mixed** overlay | Pin 11 best-effort | DB commits; JSON warning names `current` + retry `--from-json`; no rollback |
| Overlay `id` in DB, not in JSON, **JSON-only** overlay | Pin 17 — nothing else to persist | `invalid_state`; no DB write |
| `id`-only overlay (`{"id":"T"}`) | Nothing to merge | Error: no updatable fields; no writes |
| Missing `id` | Lookup | Error; no writes |
| `task-mgr update FOO-1 --title x` | Old hint said “no update command” | Clap fails; must **not** print “has no `update` subcommand”. Hint (if any) points at `--stdin`/`--json` overlay |
| `task-mgr edit` / `change` | Muscle memory | Hint points at `task-mgr update --stdin`, not loop init |
| `task-mgr set-status` | Pin 21 | Unchanged lifecycle hint table |
| Concurrent update vs `update_prd_task_passes` | **#1562** / **#2667** | Distinct tmp via shared `unique_tmp_path` |
| `prd_json` leftover `"add"` in `atomic_write` | Feed-forward | Update path errors say `"update"` |
| Nested `"passes"` inside `humanReviewOutcome` | Only top-level overlay keys are checked | Allowed (opaque `Value`) |
| Match (a) stray copy with same `taskPrefix` | PR-1 residual | Registered via prefix OR; pin 4 writes **that** PATH. Do not reopen |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **CONTRACT-001** owner: `src/commands/update.rs` (DB load-merge-write) + `src/commands/prd_json.rs` (`patch_user_story`). Consumers: US-001, US-003 (JSON-only persist), US-004, US-005, US-007. **Must not** call `init::import::update_task` or `delete_task_relationships`. JSON half **must** go through `prd_json` (unique tmp + Value merge; skip overlay `id`; unprefixed `dependsOn`). JSON-only overlay cannot `Ok` a skip.
- **CONTRACT-002** owner: overlay validation in `src/commands/update.rs` (or a small `update` submodule, not `init::parse`). Consumers: US-002, US-005, US-006 (error copy), US-008. Hard-error `status`/`passes`/unknown keys/id-rename **and** the type/null table; whitelist only.
- **CONTRACT-003** owner: `src/commands/init/parse.rs` (`PrdUserStory.human_review_outcome`) + `src/commands/add.rs` (`AddTaskInput`). Consumers: US-003, US-001 (JSON patch of the field), `add` spawned CLARIFY rows, `loop init --append --update-existing` deserialize. **No DB column. No migration.** JSON-only overlay persistence is this contract’s write policy (Err, not skip).

**Data Flow Contracts:**

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| Overlay stdin/JSON | `&str` → `serde_json::Value` object (string keys, camelCase) | `let v: Value = serde_json::from_str(input)?; let obj = v.as_object().ok_or(...)?;` — **do not** `from_value::<PrdUserStory>(v)` |
| Overlay `id` | JSON string → optional `prefix_id` → `tasks.id` TEXT (lookup only) | `let mut id = obj["id"].as_str()...; if !ctx.prefix.is_empty() { id = prefix_id(&ctx.prefix, &id); }` — skip `prefix_id` when prefix empty. **Never** `obj.insert("id", …)` on the existing story |
| Reject keys | top-level object keys only | `if obj.contains_key("status") \|\| obj.contains_key("passes") { lifecycle_err }` then `for k in obj.keys() { if k != "id" && !WHITELIST.contains(k) { unknown_err } }` then type/null table |
| Partial UPDATE | struct of `Option`s → SQL `SET col = ?` only for `Some` | Never `SET status`, `archived_at`, `priority`, `id`. Always `updated_at = datetime('now')` when any **DB** column/table changes. JSON-only overlay: no `UPDATE tasks` |
| `dependsOn` present | JSON array of strings → `task_relationships` rows `rel_type = 'dependsOn'` only | `tx.execute("DELETE FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'", [id])?; for dep in arr { insert_relationship(tx, id, prefix_id_if_needed(dep), "dependsOn")?; }` — **do not** call `delete_task_relationships`. Skip the delete when key **absent** |
| `dependsOn` JSON write | same array, unprefixed strings | After merge, `strip_prefix_in_id_array(obj, "dependsOn", prefix)` — same helper as `append_user_story` |
| `touchesFiles` present | JSON array of strings → `task_files` | `delete_task_files` + `insert_task_file` iff key present |
| JSON story patch | `userStories[]` elements are `Value` objects | `for entry in arr { if id_matches(entry, overlay_id, prefix) { merge whitelist keys except `id`; break; } }` — **do not** `from_value::<PrdUserStory>(entry)`. After merge: `entry["id"]` == previous bytes |
| Extra keys on existing story | string keys on the `Value` object | merge **does not** remove keys not in the overlay (except canonicalising `difficulty` → `estimatedEffort` when that alias is being set) |
| `humanReviewOutcome` | JSON object/`null` → `Option<Value>` on `PrdUserStory` / `AddTaskInput` only | `#[serde(default, skip_serializing_if = "Option::is_none")] pub human_review_outcome: Option<Value>` — **never** a `tasks` column bind. JSON-only overlay: persist via `patch_user_story` or `invalid_state` — never `Ok` skip |
| `ResolvedContext.prd_json_path` | PR-1 write path | Mixed: after DB commit, `patch_user_story(&ctx.prd_json_path, …)` only. JSON-only: same path, but `Err` if empty/`patch` fails. `ctx is None` → `sole_task_list_path` then `cli_write_path` — no second `locate_prd_json` |
| Tmp name | `.{basename}.{pid}-{n}-{nanos}.tmp` | `prd_json::unique_tmp_path`; same-dir rename |
| `invalid_state` command | `&str` parameter | `"update"` on this command’s path; `atomic_write(target, content, command)` |

### Modularity & Coupling Targets

- **Target public surface**: clap `Commands::Update { json, stdin, from_json }`; `update::update` / `update_with_conn`; `prd_json::patch_user_story`; `PrdUserStory.human_review_outcome`; `AddTaskInput.human_review_outcome`; optional `context::refuse_unpinned_write`. Reuse PR-1 `preflight_from_json_path` (or equivalent), `sole_task_list_path`, `cli_write_path`, `strip_prefix_in_id_array`. **No new DB columns. No new migrations. No MCP wrappers.**
- **Ownership**: overlay validation + DB merge in `update.rs`; JSON bytes in `prd_json.rs`; pin protocol stays in `context.rs`; `humanReviewOutcome` field lives on the JSON structs (`parse.rs` / `AddTaskInput`), not on `models::Task`.
- **Coupling budget**: `update` **must not** call `import::update_task` or `delete_task_relationships`. `prd_json` **must not** import `add` or `update`. `--from-json` **must not** call `init` / `register_prd_files`. Do not share CLI `exists()` into loop startup (PR-1 pin 15). Do not put ≥2 refuse inside `resolve_context`. Do not open a second connection only to beat overlay parse.
- **Cohesion**: whitelist + reject live next to the writer (`update.rs`), not on `PrdUserStory` (`deny_unknown_fields` on `PrdUserStory` would break extra keys on import of old PRDs).

### When to Emit a CONTRACT-xxx Task

- **`CONTRACT-001`** — update load-merge-write SSoT (partial SQL + `patch_user_story`; never `import::update_task` / `delete_task_relationships`; skip overlay `id`; unprefixed `dependsOn`; JSON-only overlay cannot `Ok` skip). Priority 0–1, `taskType: "contract"`. Downstream: US-001, US-003, US-004, US-005, US-007.
- **`CONTRACT-002`** — overlay whitelist + reject (`status`/`passes` lifecycle error; unknown keys hard-error; id lookup-only; **type/null table**). Downstream: US-002, US-005, US-006, US-008.
- **`CONTRACT-003`** — `humanReviewOutcome` JSON-only round-trip (`PrdUserStory` + `AddTaskInput`; no DB column; JSON-only persist is `invalid_state` if the file cannot be patched). Downstream: US-003, spawned `add`, `loop init --append --update-existing` survival.

`dependsOn` on implementation tasks that cite a contract **must name that CONTRACT-xxx** (priorities are not a graph).

---

## 3. User Stories

### US-001: Load-merge-write SSoT (CONTRACT-001)

**As a** loop agent
**I want** `task-mgr update` to patch only the fields I send
**So that** a notes-only change cannot clobber priority, title, status, or `archived_at`

**Acceptance Criteria:**

- [ ] New `src/commands/update.rs` with `update(db_dir, input_json, from_json)` and `update_with_conn` (testable). `LockGuard` like add
- [ ] Grep: `update.rs` does **not** call `import::update_task` or `delete_task_relationships`. `delete_task_files` only when the overlay contains `touchesFiles`
- [ ] Partial `UPDATE tasks SET …` only for present whitelist **DB** columns + `updated_at`. Never `SET status`, `archived_at`, `priority`, `id`. JSON-only overlay issues **no** `UPDATE tasks`
- [ ] Notes-only: title, priority, status, `archived_at` unchanged (seed `archived_at` non-NULL; it stays)
- [ ] In-progress notes-only: status stays `in_progress`
- [ ] `dependsOn` present (including `[]`) → `DELETE FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'` then insert. Seed an old `synergyWith` (or `batchWith`/`conflictsWith`) row; it **survives**. Absent key leaves all relationship rows. `touchesFiles` omit vs replace (including `[]` clears) covered by unit tests
- [ ] JSON-only overlay (`{id, humanReviewOutcome}` or `{id, humanReviewOutcome: null}`) + missing write path **or** `patch_user_story` `Err` → `invalid_state` naming `task-mgr current` and retry `--from-json`. Do **not** `Ok` with a skip note. No DB write
- [ ] Mixed overlay (`notes` + `humanReviewOutcome`) + `patch_user_story` `Err` → pin 11 warning; DB committed; never `export`
- [ ] Missing / empty overlay `id`: error before writes
- [ ] Unknown task id: error before writes
- [ ] `id`-only overlay: error “no updatable fields” before writes

**edgeCases:** archived notes-only; in-progress notes-only; omit vs `[]` vs replace; `dependsOn` must not wipe synergy rows; JSON-only missing path; missing id

---

### US-002: Overlay whitelist + reject (CONTRACT-002)

**As an** operator
**I want** a pasted full story blob or a typo key to fail closed
**So that** `passes` / `status` cannot sneak a lifecycle write and unknown keys cannot silently drop

**Acceptance Criteria:**

- [ ] Parse overlay as `serde_json::Value` object first (not `PrdUserStory`)
- [ ] Top-level `status` or `passes` (any value, including `false` / `null`) → `invalid_state("update", …)` pointing at `complete` / `fail` / `skip` / `<task-status>`. **No silent skip.** No DB/JSON writes
- [ ] Any other non-whitelist top-level key (including `priority`, `synergyWith`, `foo`) → hard-error naming **all** unknown keys. No partial apply
- [ ] Both `estimatedEffort` and `difficulty` present → hard-error (ambiguous)
- [ ] Full blob with `"passes": false` plus valid `notes` still takes the **lifecycle** error (validation order: `status`/`passes` first)
- [ ] Nested `"passes"` inside `humanReviewOutcome` is **not** a top-level overlay key — allowed
- [ ] Never `UPDATE tasks SET id`; never replace JSON `id`
- [ ] Type/null table (no writes on violation):
  - `title`: non-empty string; null/empty/non-string → error
  - nullable scalars: `null` clears; wrong type → error
  - `dependsOn` / `touchesFiles` / `acceptanceCriteria` / `requiredTests`: JSON array of strings (`[]` clears); `null` / non-array / non-string element → error
  - `requiresHuman`: bool; `null` / non-bool → error
  - `maxRetries`: integer; `null` / non-integer → error (column NOT NULL; do not default to 3)
  - `humanReviewOutcome`: object or `null`; array/string/number/bool → error

**edgeCases:** full blob; `passes: false`; `priority`; synergyWith; both effort keys; nested passes inside outcome; `title` empty; `dependsOn: null`; `maxRetries: null`; `requiresHuman: 1`

---

### US-003: `humanReviewOutcome` JSON-only (CONTRACT-003)

**As a** human resolving a CLARIFY task
**I want** `humanReviewOutcome` to persist in the task-list JSON across update and re-import
**So that** downstream tasks see the confirmed values and the DB schema stays lifecycle-clean

**Acceptance Criteria:**

- [ ] `PrdUserStory` grows `human_review_outcome: Option<Value>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` (camelCase `humanReviewOutcome`)
- [ ] `AddTaskInput` grows the same field; `into_prd_user_story` copies it (spawned CLARIFY row must not drop the key)
- [ ] Every existing `PrdUserStory { … }` literal compiles (`human_review_outcome: None`) — helpers in `add.rs`, `init/import.rs` tests, PR-1 `prd_json.rs` tests
- [ ] **No** migration; **no** `tasks.human_review_outcome` column. Test: `PRAGMA table_info(tasks)` after update does not contain that name
- [ ] Round-trip: deserialize a story JSON that includes the object; serialize still includes it
- [ ] Absent field: serialize omits the key (`skip_serializing_if`)
- [ ] After `task-mgr update --stdin` of `{id, humanReviewOutcome}` and then `task-mgr loop init <prd>.json --append --update-existing`, the JSON file still contains the object
- [ ] `init::import::update_task` SQL is unchanged (still no outcome bind) — the field rides on the struct unused during DB SET, which is correct
- [ ] JSON-only overlay + missing write path or `patch_user_story` `Err` → `invalid_state` (CONTRACT-001 split). Exit non-zero; no skip note; `PRAGMA`/sql unchanged. Mixed overlay + same JSON `Err` stays pin 11 (US-001)

**edgeCases:** add-spawned CLARIFY; re-import survival; pragma absence; skip_serializing_if none; JSON-only refuse when no `task_list`

---

### US-004: `prd_json::patch_user_story` (CONTRACT-001)

**As an** update caller
**I want** one Value-merge JSON patch on the write chokepoint
**So that** extra keys on existing stories survive and tmp names cannot collide with add / `update_prd_task_passes`

**Acceptance Criteria:**

- [ ] `prd_json::patch_user_story(prd_path, story_id, overlay: &Value, prefix, command: &str)` — `command` is forwarded into every `invalid_state` (update passes `"update"`)
- [ ] Existing story is a `Value` object; merge overlay whitelist keys onto it. **Do not** `from_value::<PrdUserStory>`
- [ ] Merge **skips** overlay `id`. After a successful patch, the JSON story’s `id` string is **byte-identical** to before the patch (unit: seed a prefixed or unprefixed id; assert `entry["id"]` unchanged)
- [ ] Id **match**: prefixed **or** unprefixed (`strip_task_prefix`), same as `append_user_story`
- [ ] `dependsOn` written unprefixed via the same `strip_prefix_in_id_array` helper as `append_user_story` (promote to `pub(crate)` if needed; do not duplicate). Unit: overlay `dependsOn` with a prefixed id; file array contains the unprefixed form only
- [ ] Extra keys already on the story survive a notes-only overlay
- [ ] `humanReviewOutcome: null` removes the key; a present object replaces
- [ ] Parameterize `atomic_write` with `command: &str` so update errors cannot say `"add"`. `append_user_story` may keep passing `"add"`
- [ ] `prd_json` does **not** import `add` or `update`
- [ ] Unique tmp + rename (existing `unique_tmp_path`); trailing newline preserved
- [ ] Story not found in file: return `Err`. Caller: JSON-only overlay maps that `Err` to `invalid_state` (no DB write); mixed overlay logs the pin-11 warning and does not roll back DB

**edgeCases:** extra key preserved; leftover `"add"` on update path; unprefixed JSON id vs prefixed DB id; overlay `id` not copied; prefixed `dependsOn` stripped on write; trailing newline

---

### US-005: Clap `Update` + pin / write policy (PR-1 CONTRACT-002 consumer)

**As a** loop agent
**I want** `task-mgr update --stdin --from-json tasks/<prd>.json` to pin the same write path as add
**So that** a worktree CLARIFY outcome lands in the worktree JSON

**Acceptance Criteria:**

- [ ] `Commands::Update { json: Option<String>, stdin: bool, from_json: Option<PathBuf> }` next to Add. `--json` conflicts with `--stdin`. Help: **pin** this already-registered effort, not import. Distinct from `init --from-json`
- [ ] `main.rs` dispatch mirrors Add (read `--json` / `--stdin`; neither → `invalid_state("update", "input", …)`; pass `from_json`)
- [ ] Pin order **matches add** (do not invent a second connection to beat parse):
  1. `--from-json` missing/directory: `preflight_from_json_path` (or shared equivalent) **before** overlay parse
  2. Parse overlay JSON
  3. `LockGuard` + open conn
  4. `resolve_context(conn, from_json, "update")` — unregistered pin errors here
  5. ≥2 refuse via shared helper (`ctx.is_none() && load_known_prefixes().len() >= 2`) **before** the write txn. Do **not** put this inside `resolve_context`
- [ ] Print the same `→ active prefix= source= target=` line as add
- [ ] Zero-prefix / `--no-prefix`: update of an existing unprefixed id OK; JSON sync iff `sole_task_list_path` returns `Some` and `cli_write_path` is a regular file
- [ ] Empty `ctx.prefix`: skip `apply_prefix` **and** `prefix_id` on overlay `id` (lookup) and `dependsOn` ids used for SQL
- [ ] Mixed overlay: after DB commit, `patch_user_story` on `ctx.prd_json_path` **only**. `--from-json` → canonical PATH (never remapped away). Default → remap then `is_file()` (worktree else registered else skip). `ctx is None` → `sole_task_list_path` then `cli_write_path`
- [ ] JSON-only overlay: same path selection; missing/empty path **or** `patch_user_story` `Err` → `invalid_state` (US-001 / US-003). Do not `Ok` skip
- [ ] Unregistered `--from-json`: error **after** parse, before write txn; copy names `loop init`; `SELECT` unchanged
- [ ] Mixed-overlay JSON-sync failure: warning names `task-mgr current` and retry `--from-json`; never `export`; DB not rolled back. Warning **may** add that a later `loop init --append --update-existing` will SET DB columns from the stale JSON
- [ ] Clap parse test in `src/cli/tests.rs` (`cargo test -p task-mgr cli::`)
- [ ] `--from-json` does not insert `prd_files` / `prd_metadata` rows

**edgeCases:** ≥2 refuse vs 0-prefix; empty prefix skip `prefix_id`; directory `is_file()` before parse; unregistered after parse; `sole_task_list_path` + `cli_write_path`; JSON-only vs mixed failure; failure copy drops `export`

---

### US-006: error_recovery + CLARIFY docs (enhance + intents)

**As an** agent who types `edit` / `update` / resolves a CLARIFY
**I want** hints and docs to name `task-mgr update --stdin` then `complete`
**So that** I do not hand-edit JSON or run `loop init` as a field patch

**Acceptance Criteria:**

- [ ] Remove `update` from `WRONG_SUBCOMMAND_HINTS`. Once the command exists, `lookup_hint` on argv `update …` must **not** say “has no `update` subcommand”
- [ ] `edit` and `change` hints point at `task-mgr update --stdin` (overlay JSON with `id` + whitelist fields; pin with `--from-json` when ≥2 prefixes). They must **not** tell the user to edit JSON + `loop init`
- [ ] `set-status` hint **unchanged** (lifecycle table)
- [ ] Optional `WRONG_ARG_HINTS` for `update` + `--title`: overlay is `--stdin`/`--json`, `id` in the object (covers `task-mgr update FOO-1 --title x`)
- [ ] Rewrite `src/cli/error_recovery.rs` unit test `update_hint_references_loop_init_workflow` and `tests/cli_tests.rs` cases at the `["update", "FOO-1", "--title", "x"]` rows (currently expect “no `update` subcommand” / loop init)
- [ ] Enhance template CLARIFY block (`templates.rs` Human-in-the-loop section): on resolution, pipe `{id, humanReviewOutcome}` to `task-mgr update --stdin` (with `--from-json tasks/<prd>.json` when pinning), **then** `task-mgr complete <clarify-id>`. Stop telling operators to embed the block by hand-editing the JSON and `loop init --append --update-existing` as the field-write path. Keep the example outcome object (it is the overlay field value). Downstream task field updates in the same resolution also go through `update --stdin`, not `Edit`
- [ ] After the template rewrite, **regenerate** the managed `CLAUDE.md` block with `task-mgr enhance agents` (do not hand-edit inside `TASK_MGR:BEGIN/END`). Merge must not leave the old “embed in JSON then loop init” CLARIFY path live in-tree. Unit: fenced block contains `update --stdin` and does not tell agents to `Edit` the JSON for `humanReviewOutcome`
- [ ] Flip PR-1 `context.rs` rustdoc / comments that say the ≥2 refuse is **add-only** to **write-only** (add **and** update) when extracting `refuse_unpinned_write` (module docs, `resolve_context` docs, `load_known_prefixes` docs, the 2+ probe test comment). Leaving “add-only” will skip the check on update
- [ ] Intents: (a) JSON “Don’t hand-edit” recipe lists `task-mgr update --stdin` for field patches; (b) a CLARIFY intent (keyword bag includes `clarify`) shows `update --stdin` then `complete <clarify-id>`. Do **not** rewrite cheatsheet / `task_ops` / historical `tasks/*-prompt.md` (PR-3)
- [ ] `how` unit test: query containing `clarify` contains `update --stdin` and `complete`

**edgeCases:** `lookup_hint` still fires on clap parse failure for the now-valid `update` subcommand — must not lie; set-status unchanged; in-tree `CLAUDE.md` matches template; `context.rs` no longer says add-only

---

### US-007: Worktree live path + `--from-json` pin (rust tests)

**As an** operator on a linked worktree
**I want** default update to patch the worktree JSON when that file exists
**So that** the loop’s re-import sees the outcome (**#4237**)

**Acceptance Criteria:**

- [ ] Live-path tests following `tests/worktree_db_resolution.rs` (spawn `git worktree add`); **not** the verify-task-mgr sandbox
- [ ] Main checkout cwd → registered path unchanged
- [ ] Linked worktree, worktree file exists → patch worktree copy; `cmp` of main JSON empty; DB row in main-repo `.task-mgr`
- [ ] Linked worktree, only main file exists → patch main; do not invent a worktree path; no basename search
- [ ] `--from-json PATH` always patches that PATH (never remapped away), including a worktree path that is registered via pin-19 identity and/or match (a)
- [ ] Existing `worktree_db_resolution` add assertions stay green (DB anchoring unchanged)

**edgeCases:** worktree file exists vs only main exists; `--from-json` not remapped away; DB still main checkout

---

### US-008: verify-task-mgr sandbox proof (user-facing)

**As a** reviewer
**I want** an isolated-sandbox drive of `update --stdin` / overlay rejects / CLARIFY outcome
**So that** green unit tests cannot ship a clap-less binary

**Acceptance Criteria:**

- [ ] **Create** `.claude/skills/verify-task-mgr/features/update-and-human-review-outcome.md` following `features/README.md` (Sub-features, How to get to it, Driving it, Gotchas) **before** any drive AC. Link it from `features/README.md`
- [ ] Drive via `.claude/skills/verify-task-mgr/SKILL.md` on a **later** FEAT or REVIEW-001 (do not put the skill-drive on a task that runs before the feature file exists)
- [ ] Proof (artifacts kept): notes-only (sql title/priority/status unchanged); full blob with `passes` rejected (no sql change); unknown key rejected; type/null reject (`title` empty or `dependsOn: null`); `humanReviewOutcome` overlay then `loop init --append --update-existing` (jq still has the key; pragma/sql has no column); JSON-only overlay with no registered `task_list` is non-zero (`invalid_state`, not skip); `--from-json` pin; ≥2-prefix unpinned refuse (**two `loop init`s without `--no-prefix`** — `sample_prd --no-prefix` twice yields 0 prefixes); `--no-prefix` update still works; `update --help` says **pin**; `edit` hint names `update --stdin`; managed `CLAUDE.md` fenced block names `update --stdin` for CLARIFY
- [ ] Worktree live-path cases are **not** claimed via this harness (US-007 rust tests)
- [ ] Compile/unit tests alone do not satisfy this story

**edgeCases:** helper unsets `TASK_MGR_ACTIVE_PREFIX`; ≥2-prefix proof is two prefixed inits; create recipe then drive

---

## 4. Functional Requirements

### FR-001: Load-merge-write (CONTRACT-001)

`task-mgr update` loads the existing row, merges present overlay keys, and writes a partial UPDATE. It must not call `init::import::update_task` or `delete_task_relationships`. It must not SET `status`, `archived_at`, `priority`, or `id`. `dependsOn` present deletes only `rel_type = 'dependsOn'`. File tables are replaced iff the overlay contains `touchesFiles`. JSON-only overlay issues no `UPDATE tasks`; missing path / patch `Err` is `invalid_state`, not `Ok` skip.

**Validation:** US-001 notes-only / archived / omit-vs-replace / synergy-row-survives / JSON-only refuse tests; grep `update.rs` for `update_task` and `delete_task_relationships`.

### FR-002: Overlay reject (CONTRACT-002)

Overlays are `Value` objects. Top-level `status`/`passes` → lifecycle hard-error. Other non-whitelist keys → unknown-key hard-error. Type/null table violations → `invalid_state`. No partial apply. Id is lookup-only.

**Validation:** US-002 table of blobs; full story with `passes: false`; `title` empty; `dependsOn: null`; `maxRetries: null`.

### FR-003: JSON patch chokepoint (CONTRACT-001)

All update JSON writes go through `prd_json::patch_user_story` (Value merge, unique tmp, parameterized command name). Merge skips overlay `id` (JSON `id` byte-identical). `dependsOn` written unprefixed via `strip_prefix_in_id_array`. Existing extra keys survive. Never round-trip the existing story through `PrdUserStory`.

**Validation:** US-004 extra-key / id-byte-identical / unprefixed-dependsOn tests; update-path `invalid_state` command == `"update"`.

### FR-004: Pin + write policy (PR-1 consumer)

Same `--from-json` pin and live-path as add. Pin order matches add (missing/directory before parse; unregistered / ≥2 after parse, before write txn). `ctx is None` uses `sole_task_list_path` + `cli_write_path`. Empty prefix skips `prefix_id`. ≥2 unpinned write refuses. Mixed overlays: DB first; failure copy never names `export` (may mention later `loop init --append --update-existing` SET-from-stale-JSON). JSON-only: `invalid_state` if the file cannot be patched.

**Validation:** US-005 / US-007.

### FR-005: `humanReviewOutcome` JSON-only (CONTRACT-003)

Field on `PrdUserStory` and `AddTaskInput`. No DB column. Survives update + `loop init --append --update-existing`. JSON-only overlay cannot succeed with nothing persisted.

**Validation:** US-003 pragma + re-import + JSON-only refuse.

### FR-006: Hints + CLARIFY docs

`update` is a real command; `edit`/`change` point at it; enhance + intents CLARIFY use `update --stdin` then `complete`. After the template rewrite, `task-mgr enhance agents` regenerates the managed `CLAUDE.md` block. `context.rs` refuse comments say **write-only**.

**Validation:** US-006 unit + `cli_tests` + fenced-block grep; US-008 help/hint captures.

### FR-007: User-facing verify

Create the feature recipe, then drive it.

**Validation:** US-008 artifacts.

---

## 5. Non-Goals (Out of Scope)

The following are explicitly **NOT** part of this work:

- Export default / `--all` / `--force` overwrite (pins 8–9, 18) — Reason: PR-3
- Remapper / `add --from-json` / `current --from-json` clap (pins 1–4, 14–15, 20) — Reason: PR-1; this PR consumes the surface
- Claim-scoped short `<task-status>` ids — Reason: pin 21
- A generic `set-status` command — Reason: pin 21; keep the `set-status` hint
- `add --from-json` creating/registering a new PRD — Reason: pin 21 / 13
- Rewriting historical `tasks/*-prompt.md` — Reason: pin 21; remaining prompt one-liners are PR-3
- Changing DB anchoring (main checkout `.task-mgr` from a worktree stays) — Reason: pin 21
- MCP task wrappers — Reason: pin 21
- Putting `priority` on the update whitelist — Reason: pin 21
- `task_ops` jq `.userStories[]` and remaining prompt alignment — Reason: PR-3
- Copying `~/.claude/docs/task-mgr-best-practices.md` — Reason: operator residual BP after PR-3
- Cheatsheet recipe for `update` — Reason: PR-3 docs slice (this PR only error_recovery / intents / enhance CLARIFY)
- Changing `init::import::update_task` SQL (still clears `archived_at` on re-import) — Reason: different verb; revive-on-reimport stays
- Schema-validating inner `humanReviewOutcome` keys (`resolvedAt` / …) — Reason: opaque `Value` is enough; docs show the shape
- Positional `task-mgr update <id> --notes …` flag-per-field CLI — Reason: overlay JSON matches add’s `--stdin`/`--json` shape
- Switching `main.rs::get_project_root` onto `git::worktree_root` — Reason: PR-1 residual; not required

---

## 5.5. Low-Value / High-Effort Areas (Explicit Cuts or Deferrals)

| Area / Capability | Why the value is low relative to cost | Rough effort cost | Recommended action |
| --- | --- | --- | --- |
| Flag-per-field `update --title/--notes` | Overlay JSON is what agents already pipe to `add --stdin`; a second CLI shape splits docs | Medium | **Cut** — `--stdin`/`--json` only |
| DB column for `humanReviewOutcome` | Loop never queries it; persistence is the task-list JSON; a column invites lifecycle-adjacent dumps | High (migration + export + MCP) | **Cut** — pin 17 |
| Schema-validate outcome inner keys | Callers embed arbitrary `confirmedValues`; a struct would silently drop extras again | Medium | **Cut** — `Option<Value>` |
| Reuse `import::update_task` with a “don’t clear archived_at” flag | Still full-row SET; still DELETE+reinsert files; still status-from-passes at the caller | Low now / high later | **Rejected** — pin 5 |
| `deny_unknown_fields` on `PrdUserStory` | Would break extra keys on import of old PRDs; overlay reject is a different type | Medium | **Cut** — reject on the overlay `Value` only |
| Cheatsheet + `task_ops` + historical prompts in this PR | Splits the docs PR; PR-3 owns remaining alignment | Medium | **Defer to PR-3** |

**Rationale:** The user-visible win is “patch fields without touching status or JSON by hand.” A second CLI shape, a DB column, or reusing the import writer would recreate the clobber/`passes` bugs this PR exists to close.

---

## 6. Technical Considerations

### Affected Components

- `src/commands/update.rs` — **new**; DB load-merge-write + overlay validation + clap handler
- `src/commands/prd_json.rs` (PR-1) — add `patch_user_story`; parameterize `atomic_write(command)`; reuse `strip_prefix_in_id_array`
- `src/commands/context.rs` (PR-1) — extract `refuse_unpinned_write` for add+update; flip “add-only refuse” comments to **write-only**; reuse `sole_task_list_path` / `cli_write_path` / `preflight_from_json_path`
- `CLAUDE.md` (managed `TASK_MGR` block) — regenerate via `task-mgr enhance agents` after the template rewrite
- `src/commands/init/parse.rs` — `PrdUserStory.human_review_outcome`
- `src/commands/add.rs` — `AddTaskInput.human_review_outcome` + `into_prd_user_story` copy; possibly call the shared refuse helper
- `src/cli/commands.rs` — `Commands::Update`
- `src/cli/error_recovery.rs` — hint swap
- `src/cli/tests.rs` — clap parse
- `src/main.rs` — dispatch
- `src/commands/mod.rs` — `pub mod update`
- `src/commands/enhance/templates.rs` — CLARIFY block
- `src/commands/intents.rs` — JSON recipe + CLARIFY intent
- `src/commands/how.rs` tests — `clarify` query
- `tests/cli_tests.rs` — update/edit hint cases
- `tests/worktree_db_resolution.rs` (or sibling) — live-path
- `.claude/skills/verify-task-mgr/features/update-and-human-review-outcome.md` — **create**
- `.claude/skills/verify-task-mgr/features/README.md` — link

### Dependencies

- **Internal:** PR-1 `resolve_context` / `ResolvedContext.prd_json_path` / `prd_json::unique_tmp_path` / `sole_task_list_path` / `cli_write_path` / `preflight_from_json_path` / add-equivalent pin policy at merge
- **Internal:** `LockGuard`, `insert_relationship` / `insert_task_file` / `delete_task_files` (touchesFiles only). **Not** `delete_task_relationships`. `prefix_id`, `strip_task_prefix`, `strip_prefix_in_id_array`
- **Internal:** lifecycle SSoT — update never calls `TaskLifecycle`
- **External:** none

### Approaches & Tradeoffs

No `/spike` on this slice. Pins already chose load-merge-write vs `import::update_task`, JSON-only outcome vs a DB column, and Value patch vs `PrdUserStory` round-trip.

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| **A. Dedicated overlay `Value` + partial UPDATE + `patch_user_story`** | Notes-only cannot clobber; `passes` can hard-error; extra JSON keys survive; no migration | New writer to test | **Preferred** |
| **B. Call `import::update_task` (maybe skip `archived_at = NULL`)** | Less new SQL | Full-row SET still clobbers omitted fields; caller still DELETE+reinserts files; `passes` still a status door at `init/mod.rs:509`; pin 5 forbids | **Rejected** |
| **C. Deserialize overlay to `PrdUserStory` (defaults) and SET every column** | One struct | Omitted `title` becomes `""`; omitted `priority` becomes 0/`0`; extra keys stripped; `passes` defaults `false` and gets ignored or applied | **Rejected** |

**Selected Approach**: A. Overlay is a `Value` with an explicit whitelist. DB merge is partial. JSON merge is `prd_json::patch_user_story`. `humanReviewOutcome` is `Option<Value>` on the JSON structs only.

**Phase 2 Foundation Check**: Approach A costs ~1 day of a dedicated writer now and is the only substrate that keeps lifecycle SSoT and extra JSON keys intact for PR-3 export (lossy dump must not be the only way to persist an outcome). Approach B/C would force a rewrite the first time an archived notes-only patch revived a PRD or a CLARIFY outcome vanished. “Approach A costs a small writer now but avoids clobber + silent drop + a status backdoor.”

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| JSON-only overlay `Ok`s with nothing persisted | CLARIFY outcome vanishes (pin 17 vs pin 11 skip) | High if add’s skip path is copied | CONTRACT-001/003 split: JSON-only + missing path / patch `Err` → `invalid_state`. Empirical: US-001 / US-003 |
| Reuse / lookalike of `import::update_task` | Notes-only clears `archived_at`, clobbers priority/title, DELETE+reinserts files | High if “just call the existing updater” | CONTRACT-001: grep `update.rs` for `update_task`; empirical notes-only + archived_at test |
| `delete_task_relationships` wipes old synergy/batch/conflicts rows | Hidden graph data loss | Med (init no longer inserts them; old DBs still have them) | Scoped DELETE `rel_type = 'dependsOn'` only. Empirical: US-001 seed synergy row |
| Silent skip of `passes` / `status` | Second status door; original “ignore passes on update” comment in `update_task` is the trap | High if overlay is typed as `PrdUserStory` | CONTRACT-002: Value parse; lifecycle error **before** whitelist; full-blob fixture. Empirical: US-002 |
| Null/wrong-type overlay | NOT NULL constraint fail or corrupt arrays | High if `null` always clears | CONTRACT-002 type/null table. Empirical: `title` empty, `dependsOn: null`, `maxRetries: null` |
| Merge copies overlay `id` / prefixed `dependsOn` | Mixed JSON convention vs append | High if `prefix_id` then merge | US-004: skip `id`; `strip_prefix_in_id_array`. Empirical: id byte-identical + unprefixed dependsOn |
| Round-trip existing story through `PrdUserStory` | Extra keys + outcome stripped (pin 12 / 17) | High if patch uses `from_value` | CONTRACT-001 JSON: Value merge only. Empirical: extra-key survival test |
| Unregistered pin checked before parse (second conn) | Disagrees with add; extra open | Med | US-005 pin order matches add |
| `lookup_hint` still says “no `update` subcommand” | Docs lie after clap grows the command (`lookup_hint` runs on **any** clap failure; first token `update` still matches `WRONG_SUBCOMMAND_HINTS`) | High if the table row is left | US-006: delete the row; rewrite `cli_tests` |
| Update `invalid_state` says `"add"` | PR-1 `atomic_write` hardcodes `"add"` | High if patch reuses it unchanged | Parameterize `command`; US-004 test |
| ≥2 refuse moved into `resolve_context` | `current` probe breaks | Med | Shared helper **outside** resolver; `current` tests stay green |
| `context.rs` still says “add-only refuse” | Implementers skip the check on update | High | US-006: flip comments to write-only |
| Template rewrite without `enhance agents` | In-tree `CLAUDE.md` still teaches hand-edit + loop init | High | US-006: regenerate managed block |
| Skill-drive AC on a task before the feature file exists | PR-1 mechanical miss | Med | US-008: create recipe then drive on a later FEAT / REVIEW-001 |
| Two `--no-prefix` inits as “≥2 prefix” proof | 0 prefixes; refuse never fires | High in sandbox | Two `loop init`s **without** `--no-prefix` (distinct `taskPrefix`) |

Top 3 = JSON-only skip (High), clobber via `update_task`, silent `passes` skip. All have empirical ACs. No High×High unmitigated blocker after the fold.

### Security Considerations

- Overlay JSON is untrusted CLI input (same class as `add --stdin`). Validate keys before SQL. Parameterized queries only.
- `--from-json` path is trusted CLI input (same class as add pin). Do not follow it into registration.
- Do not write outside `ctx.prd_json_path`. No invent.
- `LockGuard` on update (same as add).
- Tmp files stay in the same directory as the target (rename atomicity; no `/tmp` cross-filesystem — **#2667**).
- Do not log overlay bodies at `tracing` info (may contain review notes).

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `update::update` | `fn update(db_dir: &Path, input_json: &str, from_json: Option<&Path>) -> TaskMgrResult<UpdateResult>` | `{ task_id, fields_updated, prd_path }` | invalid overlay; type/null; not found; unpinned ≥2; unregistered pin; **JSON-only + missing path / patch `Err`** | `LockGuard`; partial DB UPDATE (mixed only); JSON patch (Err not skip for JSON-only) |
| `update::update_with_conn` | `(conn, input, from_json) -> TaskMgrResult<UpdateResult>` | same | same | no lock (caller-owned) |
| `prd_json::patch_user_story` | `fn patch_user_story(prd_path: &Path, story_id: &str, overlay: &Value, prefix: Option<&str>, command: &str) -> TaskMgrResult<()>` | `()` | missing story / invalid JSON / IO | unique tmp + rename; **does not write overlay `id`**; `dependsOn` unprefixed |
| `prd_json::atomic_write` | `(target, content, command: &str)` | `()` | IO | tmp + rename |
| `context::refuse_unpinned_write` (optional extract) | `(conn, ctx: &Option<ResolvedContext>, command: &str) -> TaskMgrResult<()>` | `()` | `invalid_state` naming `--from-json` / `TASK_MGR_ACTIVE_PREFIX` | **none** (no DB write) |
| clap `Commands::Update` | `{ json, stdin, from_json }` | parsed | clap missing/conflict | none |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `PrdUserStory` | no outcome field | `+ human_review_outcome: Option<Value>` | Yes, rust struct literals | Add `None` at each literal (enumerable) |
| `AddTaskInput` | no outcome field | `+ human_review_outcome: Option<Value>` | Yes, rust | `into_prd_user_story` copies it |
| `WRONG_SUBCOMMAND_HINTS` | includes `update` | `update` removed; `edit`/`change` retargeted | Yes, stderr | US-006 tests |
| `Commands` enum | no `Update` | new variant | Non-breaking CLI add | New subcommand |
| `prd_json::atomic_write` | `(target, content)` hardcoded `"add"` | `+ command: &str` | Yes, rust (private) | `append_user_story` passes `"add"` |
| `import::update_task` | full-row SET + `archived_at = NULL` | **unchanged** | No | Not used by this command |

### Data Flow Contracts

See §2.6 table (copy-pasteable). Type transition to flag: overlay is a **string-keyed JSON object**, not a typed struct. The #1 silent bug is `from_value::<PrdUserStory>(overlay)` — extra keys vanish and `passes` defaults to `false` (either ignored or applied). Implementers must keep the overlay as `Value` through validation and JSON merge.

`humanReviewOutcome` is **not** a `tasks` column. `PRAGMA table_info(tasks)` / `SELECT * FROM tasks` must not grow a field. Import may deserialize it onto `PrdUserStory` and ignore it for SQL — that is success, not a missing bind. JSON-only overlay has nothing else to persist: missing path / patch `Err` must not `Ok`.

`--from-json` identity is PR-1 pin 19 (b)+(c) plus match (a) prefix OR. Update does not reimplement identity; it calls `resolve_context`.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `src/commands/init/import.rs:437-488` | `update_task` full-row SET + `archived_at = NULL` | OK if **not** called | US-001 grep; do not change this SQL |
| `src/commands/init/mod.rs:505-528` | `--update-existing` calls `update_task` then maybe SET status from `passes` | OK (re-import verb) | Semantic distinction table; US-003 does not change this path |
| `src/cli/error_recovery.rs:32-45` | `update`/`edit`/`change` hints | BREAKS once `Update` exists if `update` row left | US-006 delete row + retarget |
| `src/cli/error_recovery.rs:202` + `tests/cli_tests.rs:2860-2940` | expect “no `update` subcommand” / loop init | BREAKS | Rewrite cases |
| `src/main.rs:326-340` | `lookup_hint` on any clap failure | NEEDS REVIEW | first token `update` must not match WRONG_SUBCOMMAND |
| `src/main.rs:822-847` | Add dispatch pattern | OK | Mirror for Update with `from_json` |
| `src/commands/enhance/templates.rs:92-113` | CLARIFY embed-in-JSON | BREAKS (intended) | US-006 `update --stdin` then `complete`; then `task-mgr enhance agents` |
| `CLAUDE.md` `TASK_MGR` fenced block | Loop agents read the old embed-in-JSON recipe | BREAKS (intended) | US-006 regenerate; do not leave the old path live |
| PR-1 `context.rs` “add-only refuse” comments | Implementers skip ≥2 check on update | BREAKS if left | US-006 flip to write-only |
| `delete_task_relationships` | Would wipe every `rel_type` | BREAKS old synergy rows | US-001 scoped DELETE |
| `src/commands/intents.rs:159-171` | Don’t-hand-edit list | NEEDS REVIEW | Add `update --stdin` |
| `src/commands/init/parse.rs:16-78` | `PrdUserStory` literals / serde | BREAKS literals | Add field; US-003 |
| `src/commands/add.rs:39-84` / `:113-139` | `AddTaskInput` / `into_prd_user_story` | NEEDS REVIEW | Copy outcome so add does not drop it |
| PR-1 `prd_json.rs:208-228` | `atomic_write` hardcodes `"add"` | BREAKS update errors | Parameterize; US-004 |
| PR-1 `context.rs` `resolve_context` | pin protocol | OK if update passes `"update"` | US-005 |
| `tests/worktree_db_resolution.rs:159+` | DB anchoring | OK | do not change assertions; add update cases |
| `src/lifecycle/**` | status SSoT | OK if update never SETs status | FR-001; lifecycle grep |
| Loop agents following enhance CLARIFY | hand-edit JSON | BREAKS (intended) | New recipe |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| `init --append --update-existing` → `import::update_task` | re-import revive | full-row SET; `archived_at = NULL`; may SET status from `passes`; DELETE+reinsert files/rels | **unchanged** |
| `task-mgr update` mixed overlay | field patch | n/a (no command) | partial UPDATE; never status/`archived_at`/priority/id; `dependsOn` deletes that `rel_type` only; files iff key present; JSON `Err` = pin 11 warning |
| `task-mgr update` JSON-only overlay | CLARIFY outcome | n/a | no `UPDATE tasks`; `patch_user_story` or `invalid_state`; never `Ok` skip |
| `TaskLifecycle` verbs / `<task-status>` | status | SSoT | **unchanged**; overlay `status`/`passes` hard-error pointing here |
| `add --stdin` | insert | typed `AddTaskInput` → `PrdUserStory` → append | same, plus `humanReviewOutcome` copied so it is not stripped |
| `prd_json::append_user_story` | new story | `to_value` of the **new** story only; existing entries Value-preserved | **unchanged**; patch is a new function |
| `prd_json::patch_user_story` | existing story | n/a | Value merge; skip overlay `id`; unprefixed `dependsOn`; never `PrdUserStory` |
| `resolve_context` `Ok(None)` | 0 or 2+ prefixes | add refuses iff `len() >= 2`; current probes | **write-only** refuse (add **and** update) iff `len() >= 2`; current still probes |
| add pin order | `--from-json` | missing/directory before parse; unregistered after parse | **same** for update |
| `delete_task_relationships` | init `--update-existing` | deletes every `rel_type` then reinserts | **not** called by `task-mgr update` |
| `WRONG_SUBCOMMAND_HINTS["update"]` | clap parse fail | “no update yet; edit JSON; loop init” | row **gone**; valid subcommand |
| `WRONG_SUBCOMMAND_HINTS["edit"/"change"]` | clap parse fail | edit JSON; loop init | `task-mgr update --stdin` |
| Enhance CLARIFY | human resolution | embed in JSON; `loop init --append --update-existing`; `complete` | `update --stdin`; `complete`; managed `CLAUDE.md` regenerated |
| Export | dump | today’s full dump | **unchanged** (PR-3) |

### Inversion Checklist

- [x] Callers of `update_task` identified — only init `--update-existing`; must stay that way
- [x] Routing that would treat `passes: false` as “ignore” (the `update_task` comment) must not leak into `task-mgr update`
- [x] Tests that assert “no `update` subcommand” / loop-init hint identified (`error_recovery` + `cli_tests`)
- [x] `lookup_hint` matches the first subcommand even when that command **exists** — delete the `update` row
- [x] `PrdUserStory` struct literals enumerable
- [x] `atomic_write` leftover `"add"` on the update path
- [x] ≥2 refuse must not enter `resolve_context`
- [x] `--no-prefix` tests must **not** expect refuse
- [x] Skill-drive must not run before the feature file exists
- [x] ≥2-prefix sandbox is two prefixed `loop init`s, not `--no-prefix` twice
- [x] Directory `--from-json` is `is_file()` **before parse**; unregistered / ≥2 **after parse**, before write txn (same as add)
- [x] JSON-only overlay cannot `Ok` a skip when the file cannot be patched
- [x] Merge skips overlay `id`; `dependsOn` written unprefixed
- [x] `delete_task_relationships` is not the dependsOn path
- [x] Type/null table fail-closed before writes
- [x] Managed `CLAUDE.md` regenerated after template rewrite
- [x] `context.rs` refuse comments flipped to write-only
- [x] Do not freeze `add.rs:NNN` in later tasks

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/cli/commands.rs` `Commands::Update` rustdoc + `--from-json` help | Create | Pin, not import; overlay via `--stdin`/`--json`; id in the object |
| `src/cli/error_recovery.rs` | Update | US-006 hint swap |
| `src/commands/enhance/templates.rs` | Update | CLARIFY: `update --stdin` then `complete` |
| `CLAUDE.md` inside `TASK_MGR` markers | Update via CLI | After the template rewrite, run `task-mgr enhance agents` so the fenced block matches. Do not hand-edit inside the markers |
| `src/commands/context.rs` | Update comments | ≥2 refuse is **write-only** (add and update), not add-only |
| `src/commands/intents.rs` | Update | JSON recipe + CLARIFY intent |
| `.claude/skills/verify-task-mgr/features/update-and-human-review-outcome.md` | Create | Operator drive |
| `.claude/skills/verify-task-mgr/features/README.md` | Update | Link the new feature file |
| cheatsheet / `task_ops` / historical prompts / ARCHITECTURE.md remaining / best-practices | **PR-3** | Do not rewrite here |
| `~/.claude/docs/task-mgr-best-practices.md` | Residual BP after PR-3 | Not in this repo |

### Institutional memory (recall)

Embed so the loop does not re-learn:

- **#1561** / **#3440**: JSON sync is best-effort after DB commit; do not roll back; do not name `export`.
- **#4224**: CLI-only task ops; never hand-edit JSON.
- **#3419** / **#5345**: all `tasks.status` mutations through `TaskLifecycle`; update is not a status verb.
- **#3498** / **#3283** / **#3156**: `humanReviewOutcome` belongs in the task JSON; resolution must land there (now via `update --stdin`) and then `complete` the CLARIFY.
- **#2667**: same-directory tmp + rename.
- **#3923**: prefixed DB ids vs unprefixed JSON ids — match both on patch (same as append).
- **#1562**: unique tmp (pid-counter-nanos), not a fixed `.task-mgr-add.tmp`.
- PR-1 feed-forward: `prd_json` must not import `add`; `invalid_state` command-name is a parameter — this PRD must not ship update errors that say `"add"`. Match (a) is a separate prefix OR, not pin-19 identity.

---

## 7. Open Questions

None. Pins, shape, phase seed, PR-1 feed-forward, and the architect fold answered the clarifying questions. Architect Questions for User: none. A still-blocking question would have been `PAUSE-NEEDED`.

---

## AA review (folded)

**Source:** `tasks/prd-agent-task-ops-pr2-architect.md` (NEEDS_CHANGES, 2026-09-09). Questions for User: none. First verdict had one High and determinate Mediums; all Suggested Revisions are now ACs / contract text in the body (not this note alone).

| Architect concern | Resolution |
| --- | --- |
| **High — JSON-only overlay can succeed with nothing persisted** (pin 11 skip vs pin 17) | Folded into CONTRACT-001 / CONTRACT-003 / US-001 / US-003 / US-005 / FR-001 / FR-005: overlay with no DB column/table changes (`humanReviewOutcome` only, including `null` remove) + missing write path **or** `patch_user_story` `Err` → `invalid_state` naming `task-mgr current` and retry `--from-json`. Do **not** `Ok` with a skip note. Mixed overlays keep pin 11. |
| **Medium — `patch_user_story` merge can write prefixed ids** | Folded into US-004 / FR-003 / data-flow: merge **skips** overlay `id`; JSON story `id` byte-identical; `dependsOn` written unprefixed via the same `strip_prefix_in_id_array` as append. |
| **Medium — null / wrong-type overlay values unspecified** | Folded into CONTRACT-002 type/null table / US-002 / assumption 5: `title` non-empty string; nullable scalars null-clear; arrays of strings (`[]` clears, **null errors**); `requiresHuman` bool; `maxRetries` integer (**null errors**); `humanReviewOutcome` object or null. Wrong type → `invalid_state`, no writes. |
| **Medium — `delete_task_relationships` deletes every `rel_type`** | Folded into US-001 / FR-001 / data-flow: `DELETE FROM task_relationships WHERE task_id = ? AND rel_type = 'dependsOn'` then insert. Grep: do not call `delete_task_relationships`. Seed synergy row survives. |
| **Medium — US-005 “unregistered before overlay parse” disagrees with add** | Folded into US-005 / FR-004 / performance: pin order matches add — missing/directory **before** overlay parse; unregistered / ≥2 **after** parse, before write txn. Reuse `sole_task_list_path` + `cli_write_path` on `ctx is None`. Do not open a second connection only to beat parse. |
| **Medium — in-tree `CLAUDE.md` keeps the hand-edit recipe until enhance runs** | Folded into US-006 / FR-006 / Documentation: after the template rewrite, run `task-mgr enhance agents`. Do not hand-edit inside `TASK_MGR` markers. |
| **Medium — PR-1 `context.rs` still says ≥2 refuse is “add-only”** | Folded into US-006 / style / semantic distinctions: flip comments to **write-only** (add **and** update) when extracting `refuse_unpinned_write`. |
| **Optional — pin-11 warning copy** | Folded into US-005 / FR-004: warning **may** note that a later `loop init --append --update-existing` will SET DB columns from the stale JSON; still never name `export`. |

**Accepted residuals (not bugs; documented):**

- Match (a) can pin a stray same-prefix copy and write that PATH (PR-1 residual). Path identity remains (b)+(c). Do not reopen.
- Switching `main.rs::get_project_root` onto `git::worktree_root` stays optional (non-goal).
- verify-task-mgr does not spawn git worktrees (US-007 rust tests own live path).
- Inner `humanReviewOutcome` keys are not schema-validated (opaque `Value`; overlay type is object-or-null only).
- `init::import::update_task` SQL is unchanged (re-import revive still clears `archived_at`).

**Inversion table from the architect file — now guarded:** JSON-only skip (US-001/003), prefixed JSON ids (US-004), type/null (US-002), all-rel_type delete (US-001), pin order vs add (US-005), live CLAUDE.md (US-006), add-only comment (US-006). Previously already guarded items (`update_task` clobber, `passes` hard-error, Value merge, `lookup_hint`, leftover `"add"`, refuse site, skill-drive order, ≥2 sandbox trap) stay guarded.

---

## Appendix

### Related Documents

- Goal ledger: `tasks/prd-goal-agent-task-ops-ux-ledger.md`
- PR-1 PRD (consumed surface): `tasks/prd-agent-task-ops-pr1.md`
- Lifecycle SSoT: `src/lifecycle/CLAUDE.md`
- Verify skill: `.claude/skills/verify-task-mgr/SKILL.md`

### Recommended `/prd-tasks` metadata

- `prdFile`: `prd-agent-task-ops-pr2.md`
- `branchName`: `feat/agent-task-ops-pr2`
- No `model`, no `taskPrefix` in generated JSON
- CONTRACT-001 / CONTRACT-002 / CONTRACT-003 as above; implementation `dependsOn` must name those ids when ACs cite them

### Glossary

- **Overlay**: the JSON object piped to `update --stdin` / `--json`. Lookup key `id` plus zero or more whitelist keys. Not a full `userStories[]` document.
- **Pin**: `--from-json PATH` selects an already-registered `task_list`. Never registers, never remaps the write target.
- **Load-merge-write**: load existing DB row/tables; merge present overlay keys; partial UPDATE + conditional table replace. Opposite of full-row SET.
- **JSON-only field**: stored in the task-list file and on `PrdUserStory` for import/serialize; not a `tasks` column.
- **JSON-only overlay**: overlay whose only whitelist key is `humanReviewOutcome` (including `null` remove). No DB column/table changes. Missing write path or `patch_user_story` `Err` → `invalid_state`, not pin-11 skip.
- **Mixed overlay**: any DB column/table whitelist key, optionally plus `humanReviewOutcome`. DB commits first; JSON `Err` is pin 11.
