# Architect review: prd-agent-task-ops-pr2

**Status**: APPROVED (pass 2 after 2026-09-18 fold; reviewer `01a0b6e9-374a-7641-93a1-89dfeb62a198`)
**Re-pass (unfolded)**: NEEDS_CHANGES (`01a0b6dc-af48-7291-b5f6-9dc5a1a6e9ff`)
**Pass 1**: NEEDS_CHANGES (folded 2026-09-09)
**Historical pass 2**: APPROVED (2026-09-09; assumed looping PR-1 worktree — superseded)
**Questions for User**: none

Folded PRD checked against HEAD (`120c5a2`). The three Highs and all Mediums from the 2026-09-18 re-pass are ACs/contract text, not note-only. No unresolved ≥ High. Leftovers ≤ Low (`default_prd_roots` still private in `add.rs` — copy or extract, do not `use add`; JSON/prompt refresh is tasks author).

---

## Pass 2 after fold (2026-09-18) — APPROVED

**Reviewer**: production-code-architect (`01a0b6e9-374a-7641-93a1-89dfeb62a198`)

**Strengths**: PRD-input states PR-1 is this tree; write-path is `default_prd_roots` → `resolve_context_with_roots`; preflight extract with `command: &str`; CONTRACT-002 splits bool/u32/Value; US-006/007/008 match HEAD; mixed+empty path is pin 11.

**Concerns**: Low — `default_prd_roots` still private in `add.rs` (accepted residual: copy `InitOpts::resolve_roots` fallback or extract next to context helpers; do not `use commands::add`). Low — JSON/prompt still say “PR-1 looping / at merge” (tasks author).

**Suggested Revisions**: none required.

**Inversion**: previously-open items (stale PRD-input, preflight leftover `"add"`, bare `resolve_context`) are guarded.

---

Re-pass against **this** default-branch HEAD (`120c5a2`, includes PR-1 squash `3fc38b4` / GitHub #43). The 2026-09-09 APPROVED report is historical: it assumed a looping PR-1 worktree. The overlay design is still the right substrate; the PRD-input map and two copy-paste call sites would mis-implement against today’s tree.

Existing `tasks/agent-task-ops-pr2.json` / `tasks/agent-task-ops-pr2-prompt.md` still say “PR-1 looping / assumed at merge”. Do not edit them in this phase; they will be wrong after the PRD-input rewrite below.

---

## Re-pass (2026-09-18)

**Status**: NEEDS_CHANGES
**Reviewer**: production-code-architect (`01a0b6dc-af48-7291-b5f6-9dc5a1a6e9ff`)
**Questions for User**: none

### Strengths

- Scope matches the ledger: `task-mgr update` + JSON-only `humanReviewOutcome` only. Pins 8/9/18 are named and not implemented. Approach A (overlay `Value` + partial SQL + `prd_json::patch_user_story`) is the only shape that keeps lifecycle SSoT and extra JSON keys.
- Pass-1 High/Mediums are already ACs: JSON-only cannot `Ok` skip; merge skips overlay `id` and unprefixes `dependsOn`; type/null fail-closed; `dependsOn` DELETE is `rel_type = 'dependsOn'` only; pin order matches add; enhance regenerate; flip “add-only” refuse comments.
- Pin protocol is consumed, not re-invented: refuse stays outside `resolve_context`; empty prefix skips `apply_prefix` **and** `prefix_id`; `--from-json PATH` is never remapped away; `prd_json` must not import `add`/`update`.
- Empirical inversion killers are still named: notes-only vs `archived_at`, full blob `"passes": false`, extra-key survival, leftover `"add"` on `atomic_write`, ≥2 sandbox trap, skill-drive after the feature file, verify-task-mgr is not a linked worktree.

### Concerns

**High — PRD-input still describes a PR-1 worktree, not `120c5a2`.**
Header “Depends on: PR-1 … at merge, **not main HEAD today**”, “Phase 1 is **looping**”, and the extraction table (`main e552877` has no `context.rs`; `add()` has no `from_json`; clap at `commands.rs:667`; `import::update_task` at `import.rs:437-488`; PR-1 feature file “not on this worktree”) are false on this tree. Implementers will look for a worktree, re-ship add clap, or miss HEAD helpers.

**HEAD relocation (do not freeze these as the next line-number gospel; grep the symbols):**

| Target | Where it is **now** (`120c5a2`) | What is there |
| --- | --- | --- |
| `context.rs` | `src/commands/context.rs` | `ResolvedContext`, `ResolutionSource::FromJsonFlag`, `resolve_context` **and** `resolve_context_with_roots`, `sole_task_list_path`, `cli_write_path`, `choose_cli_write_path`, `paths_identify` re-export from `git`. `Ok(None)` for 0 and 2+. Rustdoc still says ≥2 refuse is **add-only**. `resolve_from_json_flag` also `is_file()`s (needs DB; **not** the before-parse preflight). |
| `prd_json.rs` | `src/commands/prd_json.rs` | `unique_tmp_path`, `append_user_story`, private `atomic_write` still hardcodes `"add"`. `strip_prefix_in_id_array` is **private**. No `patch_user_story`. Does not import `add`. Extra-key test already seeds `humanReviewOutcome` on an existing story. |
| `add.rs` | `src/commands/add.rs` | **`add(..., from_json: Option<&Path>)` exists.** `preflight_from_json_path` is **private here**, hardcodes `"add"`. ≥2 refuse **inlined** (`resolved_ctx.is_none() && load_known_prefixes().len() >= 2`). `AddTaskInput` / `into_prd_user_story` still omit `humanReviewOutcome`. JSON skip/warn copy names `current` + `--from-json`, never `export`. Uses `resolve_context_with_roots` + `default_prd_roots(db_dir)` + `choose_cli_write_path` on `ctx is None`. |
| clap Add `--from-json` | `src/cli/commands.rs` ~696 | Pin help, not import. **No** top-level `Commands::Update` overlay. `RunAction::Update` (~2258) is run-session only. |
| `error_recovery.rs` | `src/cli/error_recovery.rs:32-45` | `update`/`edit`/`change` still loop-init. Tests: `error_recovery.rs:202`, `tests/cli_tests.rs:2860-2940`. |
| `PrdUserStory` | `src/commands/init/parse.rs:16-78` | Still no `humanReviewOutcome`. Extra keys dropped on deserialize. Literals: `add.rs` `into_prd_user_story`, `prd_json.rs` tests, `init/import.rs` tests. |
| `import::update_task` | `src/commands/init/import.rs:618-669` | Full-row SET + `archived_at = NULL`. Caller `init/mod.rs:1143-1198` then `apply_update_status` (LIFECYCLE-EXCEPTION from `passes`) + `delete_task_files` + **`delete_task_relationships`**. Must not be called by `task-mgr update`. |
| Enhance CLARIFY | `src/commands/enhance/templates.rs:92-113` | Embed-in-JSON + `loop init --append --update-existing` + `complete`. In-tree `CLAUDE.md` fenced block matches. |
| Intents | `src/commands/intents.rs:159-171` | Don’t-hand-edit: `add --stdin`, `loop init --append --update-existing`, `<task-status>`. **No `clarify` bag** (`how.rs` has no clarify test). |
| Verify | `.claude/skills/verify-task-mgr/features/add-and-current-from-json.md` | **On HEAD.** Sandboxes are not linked worktrees. Pattern for US-008 (absolute paths, two prefixed inits, strip `taskPrefix` for `--no-prefix`). |
| Live-path rust tests | `tests/worktree_db_resolution.rs` **`live_worktree_file_exists_writes_worktree_main_unchanged`** (~412+) and `tests/add_integration.rs::test_from_json_relative_prd_files_worktree_registered` | `:159-175` is **DB anchoring only** (`add_from_worktree_root_lands_in_main_db`). |
| `src/commands/update.rs` | **absent** | — |
| `refuse_unpinned_write` | **absent** | Predicate still inlined in `add.rs`. |
| `prd_reconcile::unique_tmp_path` | re-export/use of `prd_json::unique_tmp_path` | Same chokepoint. |

**High — “Reuse `preflight_from_json_path`” as written ships leftover `"add"` on the update path.**
The helper is `fn preflight_from_json_path` in `add.rs` (not `context.rs`) and hardcodes `invalid_state("add", "--from-json", …)`. US-005 step 1 + §6 “reuse … in context.rs” will either (a) `use commands::add` from update, or (b) call it and print `"add"` for missing/directory pins. Same class as `atomic_write` leftover `"add"`, which the inversion checklist already names — **preflight is not on that list.**

**High — US-005 / public surface still says `resolve_context(conn, from_json, "update")` and omits `resolve_context_with_roots`.**
PR-1 rustdoc: add/current pass **`db_dir`-derived roots** so TempDir / `--dir` relative `prd_files` do not remap onto the developer checkout. `add()` uses `default_prd_roots(db_dir)` → `resolve_context_with_roots` → `choose_cli_write_path` on `ctx is None`. Copying the PRD snippet (`resolve_context` + `cli_write_path` only) breaks US-007 fixtures and verify-task-mgr `--dir`. `cli_write_path` is the live-git probe; `choose_cli_write_path` is the testable one.

**Medium — CONTRACT-002 nullable-scalar bucket is not bound to `PrdUserStory` types.**
`claimsSharedInfra` is `Option<bool>` (null clears; column DEFAULT NULL). `humanReviewTimeout` is `Option<u32>`. `reviewScope` is `Option<Value>` (import `serde_json::to_string`s it). Lumping them with string scalars invites `as_str()` / “null always clears”. Prompt already says bind to field types; the table does not.

**Medium — `sole_task_list_path` rustdoc still says “Used only on the **add** `ctx is None` path”.**
Same implementer trap as “add-only refuse”. US-006 flips refuse comments; it does not name this helper.

**Medium — US-007 cites `worktree_db_resolution.rs:159-175`.**
That test only asserts main `.task-mgr` DB rows. JSON live-path + `cmp` of main is `live_worktree_file_exists_writes_worktree_main_unchanged` / `live_worktree_only_main_exists_writes_main_no_invent`, plus add_integration relative-`prd_files` pin.

**Medium — US-008 “PR-1 feature file not on this worktree”.**
Clone `add-and-current-from-json.md` (absolute sandbox paths; two inits **without** `--no-prefix`; strip `taskPrefix` before `--no-prefix`; helper unsets `TASK_MGR_ACTIVE_PREFIX`). ACs already have the ≥2 trap; the extraction table would send people to invent a recipe.

**Low — `Commands::Update` vs `RunAction::Update`.**
Keep clap paths distinct (`task-mgr update --help` vs `task-mgr run update --help`). Not a pin issue.

**Low — Mixed overlay + empty write path** is only implied (pin 11 / pin 16). JSON-only missing path is specified. Mixed should warn/skip like add, never `invalid_state` that rolls back (DB already the point of pin 11). Optional AC.

### Suggested Revisions (fold into PRD-input + US-005/006/007/008; do not expand into PR-3)

1. **Rewrite PRD-input for `120c5a2`.** Drop “not main HEAD today” / “Phase 1 is looping”. State: PR-1 public surface **is** this tree. Replace the extraction table with the HEAD table above. Grep symbols; do not freeze new `*:NNN` as task text. Fix Consumers line numbers (`import.rs:618-669`, `init/mod.rs:1143-1198`, clap Add `--from-json` ~696, `main.rs` Add ~827).
2. **Extract `preflight_from_json_path(path, command: &str)` next to `refuse_unpinned_write` in `context.rs` (preferred).** Add and update both call it. Do **not** import `add` from `update`. Directory/missing errors on the update path must say `"update"`. Add this to the inversion checklist beside `atomic_write`.
3. **US-001 / US-005: `update()` mirrors `add()` roots.** `default_prd_roots(db_dir)` → `resolve_context_with_roots(..., "update", Some(&source_root), Some(&worktree_root))`. `ctx is None` uses `sole_task_list_path` then **`choose_cli_write_path` when roots are known**, else `cli_write_path`. Do not document bare `resolve_context` as the write-path call.
4. **CONTRACT-002 type rows:** split `claimsSharedInfra` (bool or null), `humanReviewTimeout` (unsigned integer or null), `reviewScope` (JSON value or null). Keep string scalars separate. Wrong type → `invalid_state`, no writes.
5. **US-006:** also flip `sole_task_list_path` rustdoc from add-only to write-only (`ctx is None` JSON sync for add **and** update).
6. **US-007:** follow `live_worktree_file_exists_writes_worktree_main_unchanged` + add_integration relative-`prd_files` pin, not `:159-175`. Keep `:159` add DB-anchoring green.
7. **US-008:** “create/extend by cloning `features/add-and-current-from-json.md`”; link from `features/README.md`. Do not claim the PR-1 feature file is absent.
8. **Optional:** one AC that mixed overlay + no write path is pin 11 (DB committed, skip/warn, never `export`); JSON-only stays `invalid_state`.

After fold, existing JSON/prompt “PR-1 at merge” lines should be refreshed in authoring — not in this review.

### Inversion coverage (this HEAD)

| Failure mode | Status vs this PRD + HEAD |
| --- | --- |
| Notes-only calling `import::update_task` clobbers `priority`/`title`/`archived_at` | **Guarded** — new writer; grep; archived notes-only AC |
| Full blob `"passes": false` skipped or written as status | **Guarded** — lifecycle error first; no writes |
| JSON-only `{id, humanReviewOutcome}` `Ok` with skip (pin 11 vs 17) | **Guarded** in ACs/contracts |
| `patch_user_story` via `PrdUserStory` or copies overlay `id` / prefixed `dependsOn` | **Guarded** |
| ≥2 refuse inside `resolve_context` or `if ctx.is_none() { refuse }` | **Guarded** |
| `--from-json PATH` remapped away | **Guarded** |
| `atomic_write` leftover `"add"` | **Guarded** (parameterize) |
| `preflight_from_json_path` leftover `"add"` | **Open** — reuse-as-is on HEAD |
| `delete_task_relationships` wipes every `rel_type` | **Guarded** |
| `priority` on whitelist; status via update | **Guarded** |
| Stale PR-1-worktree citations / old line numbers | **Open** — PRD-input still that document |
| `resolve_context` without `with_roots` / `choose_cli_write_path` | **Open** — US-005 snippet |
| verify-task-mgr claimed as worktree proof | **Guarded** (US-007 rust; US-008 says not) |
| Skill-drive before feature file; two `--no-prefix` as ≥2 | **Guarded** |
| Export scoping (pins 8/9/18) | **Not in this PRD** (non-goals) |

No High×High product-design hole remains in the overlay/JSON-only split. The re-pass fails on **implementer-facing HEAD truth** and two call-site copies that would recreate leftover `"add"` and wrong write-path roots.

Do not loosen pins. Do not implement export. Do not re-expand the three-PR shape.

---

## Historical — pass 2 APPROVED (2026-09-09)

**Status**: APPROVED (pass 2, 2026-09-09)
**Pass 1**: NEEDS_CHANGES (folded)
**Pass 2 reviewer**: `01a08897-0fa8-7321-b879-f9ee429a43e8`
**Questions for User**: none

Pass 2: all seven pass-1 revisions are ACs/contract text. Leftovers ≤ medium (nullable-scalar types bind to PrdUserStory; JSON-only does not bump updated_at; mixed overlay + JSON Err drops outcome under pin 11). Do not re-open authoring. (Superseded by 2026-09-18 re-pass.)

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
