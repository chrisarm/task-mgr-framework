# PRD: Agent task-ops UX PR-3 — export scoped + docs/prompt alignment

**Type**: Enhancement
**Priority**: P0 (Critical)
**Author**: Grok
**Created**: 2026-09-09
**Status**: Draft
**Goal ledger**: `tasks/prd-goal-agent-task-ops-ux-ledger.md` phase 3 (authoring)
**Related**: learnings **#1252**, **#4114**, **#3332**, **#2236**, **#5596**, **#5577**, **#2667**, **#4564**, **#1562**, **#1561**, **#2588**, **#3756**
**Depends on**: PR-1 public surface (`tasks/prd-agent-task-ops-pr1.md`) at merge; PR-2 `task-mgr update` (`tasks/prd-agent-task-ops-pr2.md`) at merge for the `task_ops` / cheatsheet update one-liner — not on main HEAD today

---

## PRD-input note (effort check)

Phase 1 of the goal ledger is **reviewing** (loop complete, auto-review running). Phase 2 is **authored**, waiting for PR-1 merge. This phase is **not** duplicate or obsolete: `export` still dumps every unarchived task with `prd_metadata ORDER BY id LIMIT 1`; `--to-json` onto a live `task_list` is unguarded; `task_ops` still teaches `jq '.tasks[]'` (2027 / 2048 bytes); operator docs still show `export --to-json tasks/my-project.json`. Pins, simplified shape (three serial PRs), and this phase seed are the answers that would otherwise have been Step 3 clarifying questions. This PRD does not add phases, change pins, or propose a different program.

**HEAD at authoring:**

| Tree | Commit | Note |
| --- | --- | --- |
| main (this checkout) | `e552877` (`Merge pull request #42 from chrisarm/chore/v0.3.3`) | no `context.rs` / `prd_json.rs`; export is dump-all; cheatsheet still **forbids** `add --from-json` |
| PR-1 worktree | `1d5f494` (`feat: a410d276-REVIEW-001-completed`) | looping/reviewing; **do not edit**. `context.rs` / `prd_json.rs` / add+current `--from-json` exist. **Export, `task_ops`, intents, enhance spawn-fixup are byte-identical to main** (confirmed `diff -q`). Cheatsheet already contains `add --from-json` |

**Re-located extraction targets (main unless noted — do not cite memory; do not invent PR-1 line numbers for files this PRD will change):**

| Target | File:lines | What is there now |
| --- | --- | --- |
| `export()` | `src/commands/export/mod.rs:72-133` | `(dir, to_json, with_progress, learnings_file)` — no scope, no `--force`, **no `LockGuard`**. Loads all tasks + first metadata. |
| `write_json_atomic` | `src/commands/export/mod.rs:136-178` | tmp = `path.with_extension("json.tmp")` (not pid-counter-nanos). Pretty-print + rename. |
| `load_prd_metadata` | `src/commands/export/prd.rs:110-162` | `FROM prd_metadata ORDER BY id ASC LIMIT 1`. Does **not** bind `task_prefix`. Empty table → `project: "unknown"`. |
| `load_tasks` | `src/commands/export/prd.rs:165-301` | `FROM tasks WHERE archived_at IS NULL ORDER BY id` — **all prefixes**. `passes = status == Done`. No `humanReviewOutcome`. No `taskPrefix` on `ExportedPrd`. |
| Export clap | `src/cli/commands.rs:283-296` | `--to-json` required; `--with-progress`; `--learnings-file`. No `--from-json`, `--all`, `--force`. |
| Export dispatch | `src/main.rs:596-604` | `export(&cli.dir, &to_json, …)` — **no lock**. Stay that way: `main.rs` **only** forwards `ExportOpts`. `LockGuard` lives **inside** `export()` (same as add). |
| Clap unit tests | `src/cli/tests.rs:734-822` | four `Commands::Export { to_json, with_progress, learnings_file }` matches — **will not compile** when the variant grows fields. |
| Prefix LIKE | `src/db/prefix.rs:47-96` | `make_like_pattern` → `"{escaped}-%"`; `prefix_and("id")` with `ESCAPE '\'`. Trailing-dash SSoT. |
| Path identity | PR-1 `src/commands/context.rs` `paths_identify` / `find_registered_by_path_identity` (task_list JOIN `prd_metadata`) | pin 19 (b)+(c) + absolute canonicalize. Today returns `Option<Option<String>>` (**prefix only**, no `prd_id`). **Promote** to return matched `prd_files.prd_id` + prefix. **Reuse; do not reimplement remap.** |
| `cli_write_path` | PR-1 `context.rs` | remap then `is_file()`. Export dest is **`--to-json` PATH**, never this. |
| `unique_tmp_path` | PR-1 `src/commands/prd_json.rs:27-41` | `.{base}.{pid}-{n}-{nanos}.tmp`; `pub(crate)`. |
| `task_ops` | `src/loop_engine/prompt_sections/task_ops.rs:40-74` | jq **`.tasks[]`**; add example has **no** `--from-json`; **no** `task-mgr update`. **2027 bytes** (21 bytes of headroom). Budget test at `:196-203`. |
| Intents | `src/commands/intents.rs:121-158` | “where will my add land” / “view active” mention `from-json` in prose but examples are bare `task-mgr current`. JSON recipe (PR-2 will add `update --stdin`) has no pin. |
| Enhance spawn-fixup | `src/commands/enhance/templates.rs:115-127` | form **(a)** `--from-json tasks/<correct-prd>.json` already present. CLI cheat sheet add example (`:49-59`) still has **no** `--from-json`. |
| Cheatsheet | main `src/commands/cheatsheet.rs:181` forbids `add --from-json`; PR-1 already flipped + recipe. No `update` / export `--force` recipe. | PR-1 at merge is the baseline; this PR **adds** update + export, does not re-forbid `add --from-json`. |
| Operator smash recipes | `README.md:90`, `:526`; `docs/INTEGRATION.md:72,82,209,212,373,380,627`; `docs/QUICKSTART.md:299` | `export --to-json tasks/<prd>.json` / `$PRD_FILE` with no `--force`. |
| Live smash caller | `scripts/claude-loop.sh:164` (cleanup), `:533` (per-iteration), `:552` (final) | `export --to-json "$PRD_FILE" 2>/dev/null \|\| true`. This **is** ARCHITECTURE’s “export after every iteration.” After the breaking default these fail (registered dest, no `--force`) and crash recovery **silently dies**. **Do not** add `--force` onto `$PRD_FILE`. |
| ARCHITECTURE | `docs/ARCHITECTURE.md:464-475` | “Export PRD JSON after every iteration” — Rust loop does not call `export()` (`prd_reconcile`); **the bash script does**. |
| CLI `--no-prefix` export | `tests/human_review_cli.rs` (`init_from_fixture` `:36` `--no-prefix`; export `:211`, `:248`, `:289`, `:311`); `tests/model_fields_cli.rs`; `tests/cli_tests.rs:555` (`setup_initialized_tempdir` uses `--no-prefix`) | CLI `export --to-json exported.json` (new dest) after `--no-prefix` init. Zero-prefix default is the new error; they need `--all`. Dest stays a new file (no `--force`). |
| Verify skill | `.claude/skills/verify-task-mgr/SKILL.md` + `features/` | No export feature file. Sandboxes are **not** linked worktrees. Helper unsets `TASK_MGR_ACTIVE_PREFIX`. |
| Library callers | `tests/import_export.rs`, `tests/model_fields_round_trip.rs`, `tests/e2e_loop.rs`, `tests/prd_max_retries_round_trip.rs`, `src/commands/export/tests.rs` | `export::export(dir, path, false, None)` after `PrefixMode::Disabled` init, dest = **new** `exported.json` (not the imported file). |
| Loop engine | `src/loop_engine/**` | **no** `export::export` call. Do not add one. |

**PR-1 / PR-2 public surface this PRD assumes at merge** (author-ahead; not that it is on main HEAD today):

- `resolve_context(conn, from_json, command)` flag → env → single-prefix → `Ok(None)` (0 **and** 2+). `--from-json` is pin, never register, never remap **its** PATH. `ResolvedContext.prefix` empty on NULL-prefix pin.
- `paths_identify` / `find_registered_by_path_identity` (promote `pub(crate)` if needed) — pin 19 (b)+(c) + absolute. **This PR promotes the identity return to include `prd_files.prd_id`** (today prefix-only). Match (a) is a **separate** prefix OR used by `--from-json` registration, **not** the export overwrite-guard.
- `prd_json::unique_tmp_path`. `prd_json` must not import `export`.
- `task-mgr update --stdin --from-json` exists (PR-2). Cite `tasks/prd-agent-task-ops-pr2.md` for the `task_ops` / cheatsheet update one-liner — do not re-derive overlay rules here.
- Cheatsheet already contains `add --from-json` and no longer forbids it (PR-1). Enhance spawn-fixup form (a) is already true after PR-1.

**Assumptions (not pins — stated so implementers do not invent):**

1. `--to-json PATH` stays **required**. `--from-json` on export is a **source pin** (which effort’s metadata + tasks). Dest is always the `--to-json` PATH (pin 4/13 analogue: never remapped away). Do not default dest to `ctx.prd_json_path`.
2. Overwrite-guard is **path identity of dest** against `prd_files` rows with `file_type = 'task_list'` (reuse `find_registered_by_path_identity` / pin 19, including the live-path pair). **Not** match (a). Dest must be an existing regular file to match (`canonicalize` fails if missing — same as PR-1 `paths_identify`). A non-existent dest never requires `--force`.
3. `--all` restores **today’s dump byte-for-byte in content**: all `archived_at IS NULL` tasks + `prd_metadata ORDER BY id ASC LIMIT 1`. Do not “fix” `--all` into a multi-PRD array. `--all` still honors the overwrite-guard on dest.
4. Empty `ctx.prefix` (NULL-prefix `--from-json` pin): dump **all** unarchived tasks; metadata is `prd_metadata.id = identity-matched prd_files.prd_id`. **Forbid** `WHERE task_prefix IS NULL` without that id (two `--no-prefix` inits would stamp the wrong `project`/`branchName`). `--all` stays `ORDER BY id LIMIT 1`. Do **not** LIKE `"-%"`.
5. `--with-progress` / `--learnings-file` stay DB-global (not prefix-scoped). Out of scope to filter them.
6. `ExportedPrd` stays lossy: **no** `taskPrefix` field, no extra story keys (`humanReviewOutcome`, …), status collapsed to `passes`. That lossiness is **why** `--force` is required even for the same-PRD path. Do not add `taskPrefix` to make dump “safe”.
7. Library `export()` grows an explicit scope. In-tree `PrefixMode::Disabled` callers pass `All`. Dest-is-new-file callers do not need `force`. Do not keep a silent smash default on the library function that disagrees with the CLI.
8. `task_ops` is 2027 bytes. Adding `--from-json` + `update` + `.userStories[]` **requires trimming** existing prose. Keep every phrase `test_section_contains_critical_phrases` already asserts; add new assertions; stay `< 2048`.
9. verify-task-mgr sandboxes are not linked git worktrees. Worktree dest-identity cases live in rust tests. The skill proves clap + default-scope + `--force` refuse + `--all` + ≥2-prefix default error in an isolated `--dir`.
10. `~/.claude/docs/task-mgr-best-practices.md` is **not in this repo**. CHANGELOG follow-up residual only — **do not** copy that file into the tree, **do not** add a story that edits it.
11. Do not rewrite historical `tasks/*-prompt.md`.
12. PR-2 already owns enhance **CLARIFY** (`update --stdin` then `complete`) and the intents JSON-recipe `update` line. This PRD does **not** redo those. Spawn-fixup form (a) **stays**. After any template rewrite, run `task-mgr enhance agents`. The regenerated fenced block must still contain spawn-fixup (a) **and** PR-2 `update --stdin` CLARIFY — it must **not** restore hand-edit + `loop init`.
13. `LockGuard` is acquired **inside** `export()` only, after `dest.is_file()`, before identity re-check and write (same as add). `main.rs` only forwards `ExportOpts`. Never lock in both (non-reentrant `flock` on `tasks.db.lock`). Missing dest → no lock.
14. Missing/directory/unregistered `--from-json` go through `resolve_context(conn, from_json, "export")`. Do **not** call `add::preflight_from_json_path` (private; hardcodes `"add"`). If a pre-open helper is extracted, it lives in `context.rs` with a `command` parameter. Export errors must not say `"add"`.

---

## 1. Overview

### Problem Statement

`task-mgr export --to-json PATH` dumps **every** unarchived task in the DB and stamps **the first** `prd_metadata` row (`ORDER BY id LIMIT 1`) onto the file. Operator docs and crash-recovery recipes point that dump at `tasks/<prd>.json`. Export is lossy (no `taskPrefix`, extra keys stripped, status collapsed to `passes`), so the write is not a merge — it **smashes** the live task-list, including sibling PRDs’ tasks and any `humanReviewOutcome` PR-2 just made persist.

Agents are told to pin with `--from-json` and to jq `.userStories[]`, but `task_ops` still shows `.tasks[]` (learning **#1252** / **#4114**) and has no `add --from-json` / `task-mgr update` one-liners. Intents “where will my add land” / “view active” talk about `from-json` without showing the real flag. README still teaches the smash recipe.

Goal: export cannot smash a PRD; every agent-facing surface tells the same story.

### Background

This is **PR-3 of three serial PRs**. PR-1 (reviewing) ships the remapper, `commands/context.rs`, `commands/prd_json.rs`, and `add`/`current --from-json`. PR-2 (authored) ships `task-mgr update` + `humanReviewOutcome`. This PRD implements only the PR-3 slice and **assumes PR-1’s public surface at merge** (and PR-2’s `update` for the prompt one-liner).

The Rust loop engine does **not** call `export()` (persistence is `prd_reconcile` / add / update). `scripts/claude-loop.sh` **does** (`:164` cleanup, `:533` per-iteration, `:552` final: `export --to-json "$PRD_FILE" || true`). That is the live smash caller ARCHITECTURE describes. This PRD must remove or retarget those three lines — **not** add `--force` onto `$PRD_FILE`.

---

## 2. Goals

### Primary Goals

- [ ] Export **default** = active prefix (env / single-prefix / `--from-json` pin). `--all` restores today’s dump. `--all` **conflicts** with `--from-json`.
- [ ] `--to-json` onto a registered `task_list` (path identity including live-path pair) **always** requires `--force`, even when scoped to that PRD. `--force` is a **lossy dump**, not a merge. `LockGuard` is acquired **inside** `export()` only, after `dest.is_file()`, before identity re-check and write. `main.rs` only forwards `ExportOpts`. Missing dest → no lock.
- [ ] Scoped metadata comes from the **matching** `prd_metadata` row (`task_prefix = ?` when named; empty-prefix `--from-json` uses `prd_metadata.id = identity-matched prd_files.prd_id`). Never `WHERE task_prefix IS NULL` without that id. `--all` keeps `ORDER BY id LIMIT 1`. Tasks filtered by `db::prefix` trailing-dash LIKE (`"{prefix}-%"` + `ESCAPE '\'`), plus `archived_at IS NULL`.
- [ ] No active PRD and no `--from-json` / `--all` → `invalid_state` naming **`--from-json` / `--all` / `task-mgr current`**. Zero-prefix DBs can still `--all`.
- [ ] `task_ops` stays **< 2048 bytes**: `--from-json` one-liner on add, `task-mgr update` one-liner (cite PR-2 PRD), jq `.userStories[]` not `.tasks[]`.
- [ ] Enhance spawn-fixup form (a) **stays**. After `task-mgr enhance agents`, the fenced block still has form (a) **and** PR-2 `update --stdin` CLARIFY (must not restore hand-edit + `loop init`). Intents “where will my add land” / “view active” treat `--from-json` as real. Cheatsheet gains update + export `--force`. Operator README / INTEGRATION / QUICKSTART smash recipes are rewritten. `scripts/claude-loop.sh` three `$PRD_FILE` dumps are **removed or retargeted** (never `--force` onto `$PRD_FILE`). CHANGELOG notes the breaking default **and** the best-practices residual (do not copy that file).
- [ ] User-facing proof: **create** a verify-task-mgr feature recipe, then drive it on a later FEAT / REVIEW-001. Compile/unit tests alone are not proof. Worktree dest cases are rust tests.

### Success Metrics

- Two prefixed PRDs, no env, no `--from-json`, no `--all`: `export --to-json /tmp/out.json` non-zero; dest not created (or unchanged); stderr names `--from-json`, `--all`, and `task-mgr current`.
- Same DB + `--from-json tasks/prd-a.json --to-json /tmp/out.json`: only `"{prefixA}-"` tasks; metadata.project / branch from PRD A’s `prd_metadata` row (not PRD B’s even if B has a lower `id`).
- `--all --to-json /tmp/out.json`: task count = all unarchived (today’s dump); metadata = `ORDER BY id LIMIT 1`.
- `--all --from-json X`: clap error (conflict).
- `--to-json` onto the registered task-list path without `--force`: non-zero; dest **byte-identical**; no lock-free smash.
- Same dest + `--force`: dest replaced with `ExportedPrd` JSON; extra keys / `taskPrefix` **gone** (lossy). `LockGuard` held **inside** `export()` (not in `main.rs`).
- Worktree copy of a registered path as dest: identity hits → `--force` required; `--to-json PATH` writes **that** PATH (not remapped away).
- Zero-prefix DB: default export errors; `--all` dumps (existing `--no-prefix` round-trip tests pass via `All`).
- `task_ops.len() < 2048`; contains `.userStories[]`, `--from-json`, `task-mgr update`; does **not** contain `.tasks[]`.
- `task-mgr how 'where will my add land'` recipe contains `current --from-json` (or `add --from-json`).
- Spawn-fixup form (a) **and** PR-2 `update --stdin` CLARIFY still in the enhance template **and** the regenerated `CLAUDE.md` fenced block (no hand-edit + `loop init` CLARIFY path).
- `verify-task-mgr` artifacts for the new feature file exist under `.claude/skills/verify-task-mgr/artifacts/<run-id>/`.

---

## 2.5. Quality Dimensions

> Pins 8–9, 18–19 (export) and the docs/prompt slice of 1, 11, 13, 21 are law for this PR. Pins 5–7, 10, 12, 17 (update / humanReviewOutcome) and 1–4, 14–16, 20 (remapper / add clap) are cross-phase: they appear here so `/prd-tasks` cannot contradict them. **This PRD’s stories must not reimplement remapper, add clap, or `task-mgr update`.**

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

**PR-3-specific correctness:**

- **Scope selection** (after clap, after dest overwrite-guard):
  1. `--all` → today’s dump (`load_tasks` all unarchived; `load_prd_metadata` `ORDER BY id ASC LIMIT 1`). Ignore env / single-prefix.
  2. `--from-json PATH` → `resolve_context(conn, Some(path), "export")`. Missing / directory / unregistered: **all** go through that call (command-name `"export"`). Unregistered copy names `loop init`. Do **not** call `add::preflight_from_json_path` (hardcodes `"add"`). If a pre-open helper is extracted, it lives in `context.rs` with a `command` parameter. Use `ctx.prefix` for the task filter. Empty prefix: metadata via identity-matched `prd_id` (below). **Do not** write `ctx.prd_json_path`.
  3. Else → `resolve_context(conn, None, "export")`. `Some(ctx)` with non-empty prefix → scoped dump. `Ok(None)` (0 **or** 2+ prefixes, no env) → `invalid_state("export", …)` naming **`--from-json` / `--all` / `task-mgr current`**. Do **not** call `refuse_unpinned_write` (that copy is add/update; export is a dump, and `current` stays a probe).
- **Task filter** (scoped, non-empty prefix): reuse `db::prefix::prefix_and` / `make_like_pattern` — `AND id LIKE ? ESCAPE '\'` with pattern `"{escaped_prefix}-%"`, plus existing `archived_at IS NULL`. Trailing dash is load-bearing (`FEAT` must not match `FEATX-001`; `001` must not match as a prefix). Do not `starts_with(prefix)` without the dash. Do not hand-roll LIKE without `ESCAPE` (prefix may contain `_`).
- **Metadata** (scoped, named prefix): `SELECT … FROM prd_metadata WHERE task_prefix = ?` (UNIQUE). **Empty-prefix `--from-json`:** `SELECT … FROM prd_metadata WHERE id = ?` with `prd_id` from the identity match of the pin path (`prd_files.prd_id`). **Forbid** `WHERE task_prefix IS NULL` in `export/` without that id (SQLite UNIQUE allows multiple NULLs; two `--no-prefix` inits would stamp the wrong row). Promote `find_registered_by_path_identity` (or a `pub(crate)` sibling) to return `(prd_id, prefix)` — today it returns prefix only. Missing row → same `project: "unknown"` defaults as today. **`--all` keeps `ORDER BY id ASC LIMIT 1`.**
- **Overwrite-guard** (before dump): if `--to-json` dest **exists as a regular file** and `find_registered_by_path_identity` (pin 19 live-path pair, `file_type = 'task_list'`) returns `Some`, then `--force` is required. Else (missing dest, or dest is an unregistered file) write is allowed without `--force`. Directory dest → error. Identity miss on a stray same-`taskPrefix` copy is **not** registered (match (a) is not the guard).
- **`--force`**: full replace of dest with pretty-printed `ExportedPrd`. Not a Value-merge. Extra keys on the previous file do not survive. `taskPrefix` is not written. Do not deserialize dest to `PrdUserStory`.
- **`--to-json PATH`**: always that PATH (never `cli_write_path` / remap). From a worktree cwd, a relative dest is the worktree-relative path the operator passed.
- **LockGuard**: acquire **inside** `export()` only, after `dest.is_file()`, before identity re-check and write — same site as add (`add()` locks inside the command, not `main.rs`). `main.rs` **only** forwards `ExportOpts`. Never lock in both (non-reentrant `flock` on `tasks.db.lock` → hang / `LockError`). Missing dest → **no lock**. Dest exists (registered or not) → lock, then identity, then `--force` or write.
- **Tmp**: dest (and `--learnings-file`) writes go through `prd_json::unique_tmp_path` + rename, not `with_extension("json.tmp")`. Same-directory (learning **#2667** / **#4564**). Export must not import `add`/`update`; `prd_json` must not import `export`.
- **`--from-json` help** on Export says **pin**, not import. Distinct from `init --from-json`.
- **Clap**: `--all` conflicts with `--from-json`. `--force` is independent (needed whenever dest is registered, including `--all` onto a live path). `--to-json` remains required.
- **Pin 11 is not export’s job**: add/update failure copy must still never name `export`. This PRD must not reintroduce that copy in docs or `task_ops`.
- **`invalid_state` command-name** on this path is `"export"`.

**Cross-phase (do not implement in this PRD; do not contradict):**

- Pins 1–4, 14–15, 20 remapper / add+current clap — PR-1. Export **calls** `resolve_context` / `paths_identify`; it does not reimplement remap or add `exists()` to startup.
- Pins 5–7, 10, 12, 17 `task-mgr update` / overlay / `humanReviewOutcome` — PR-2. `task_ops` / cheatsheet **name** `update --stdin`; they do not re-specify the whitelist.
- Pin 16 ≥2 refuse is **write-only** (add/update). Export’s ≥2 default is the **no-active-PRD** error (names `--from-json` / `--all` / `task-mgr current`), not `refuse_unpinned_write`.
- Historical `tasks/*-prompt.md` — pin 21. `~/.claude/docs/task-mgr-best-practices.md` — residual BP, not a story.

### Performance Requirements

- Best effort. One metadata query + one task query (prefixed LIKE or full scan). Identity is a handful of `Path` joins + canonicalize per `task_list` row (typically 1–3).
- Exit early: clap conflict; dest-is-directory **before** dump; `--from-json` missing/directory/unregistered via `resolve_context(conn, from_json, "export")` **before** dump (command-name `"export"`, not `"add"`); dest-registered without `--force` **before** serializing tasks. Do not open a second connection only to beat pin parse — one conn inside `export()`.
- Do not walk the worktree or search by basename.
- Do not load dest JSON in order to “merge” (there is no merge).

### Style Requirements

- Follow existing codebase patterns. `TaskMgrError::invalid_state(command, field, expected, actual)`. `ui::emit` / `ui::emit_err` for product UX (CONTRACT-LOG-001). No `tracing` for operator-facing refuse/`--force` copy.
- No `.unwrap()` on filesystem or SQLite in export unless a prior invariant makes it unreachable.
- Reuse `db::prefix::{make_like_pattern, prefix_and}` — do not duplicate LIKE escaping.
- Reuse PR-1 `resolve_context` / `paths_identify` / `find_registered_by_path_identity` (promoted to return `prd_id`) / `unique_tmp_path`. Do not copy remap math into `export/`.
- `prd_json` must not import `export`. `export` must not import `add`/`update`. **Do not call `preflight_from_json_path`** (private in `add.rs`, hardcodes `"add"`). Loop startup must not import CLI overwrite-guard. `main.rs` must not `LockGuard::acquire` on the Export arm.
- Comments explain **why** (lossy dump ⇒ `--force` even for same-PRD; `--all` keeps LIMIT 1; dest is `--to-json` not `ctx.prd_json_path`; trailing-dash LIKE). Do not narrate the move.
- Do not freeze `export/prd.rs:NNN` in later tasks; grep symbols (`load_prd_metadata`, `load_tasks`, `write_json_atomic`, `find_registered_by_path_identity`).
- Scoped clap unit tests: `cargo test -p task-mgr cli::`. Binary/operator proof is verify-task-mgr + rust worktree tests.
- `task_ops` trim must keep existing unit-test phrases (`MUST NOT read or edit`, `past the model window`, `task-mgr show`, `NEVER`, `next --claim`, `## Current Task`, `jq`, `task-mgr add --stdin`, `<task-status>`, `--depended-on-by`, `in response to a milestone`, all five statuses, `tasks/*.json` and not `.task-mgr/tasks/`).

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Two prefixes, no env, no `--from-json`, no `--all` | Today dumps PRD #1 metadata + **both** task sets | Error naming `--from-json` / `--all` / `task-mgr current`; dest unchanged |
| Two prefixes + `--from-json` A + dest new file | Scoped dump | Only `"{A}-"` tasks; metadata from A’s row even if B.`id` is smaller |
| Two prefixes + `--all` + dest new file | Pin 8 restore | All unarchived tasks; metadata LIMIT 1 (today) |
| `--all --from-json X` | Conflicting pins | Clap conflict; no write |
| `--to-json` registered path, no `--force`, scoped to that PRD | Pin 9 — same-PRD still lossy | Refuse; dest byte-identical |
| `--to-json` registered path + `--force` | Pin 18 dump | Replace with `ExportedPrd`; `taskPrefix` / extra keys gone; `LockGuard` **inside** `export()` after `dest.is_file()` |
| `--all --to-json` registered path, no `--force` | Smash via the restored dump | Still refuse |
| Dest is worktree remapped copy of a `task_list` | Live-path pair (pin 19) | Identity hits; `--force` required; write **that** PATH |
| Dest is main registered path, cwd is worktree | `--to-json` not remapped | Writes the PATH given; if that PATH identifies, `--force` |
| Dest does not exist | `canonicalize` fails | No `--force`; **no lock**; create |
| Dest exists but is not a `task_list` (e.g. `/tmp/dump.json`) | Not a live PRD | Write without `--force` |
| Dest is a directory | `canonicalize` would succeed | Error; no dump |
| Stray copy with same `taskPrefix` as dest | Match (a) vs pin 19 | Overwrite-guard does **not** fire (not in `prd_files`). `--from-json` of that stray still pins via (a) for **source** |
| Relative `prd_files` (`tasks/foo.json`) + dest `tasks/foo.json` | Pin 2 / 19 | Identity joins to `source_root` then canonicalize/remap; `--force` |
| Zero-prefix / `--no-prefix` DB, default export | `resolve_context` is `None` | Error naming `--from-json` / `--all` / `current`; `--all` still dumps |
| `--from-json` NULL-prefix registered file | Empty `ctx.prefix`; two `--no-prefix` inits | All unarchived tasks; metadata `WHERE id = identity-matched prd_id`. **Not** `WHERE task_prefix IS NULL`. Do not LIKE `"-%"` |
| Unregistered `--from-json` | Pin 2, 13 | `resolve_context(..., "export")` error naming `loop init`; dest unchanged; errors do **not** say `"add"` |
| Missing / directory `--from-json` | Must not reuse add’s preflight | Same: through `resolve_context(..., "export")`; dest unchanged; command-name `"export"` |
| Prefix containing `_` (e.g. `P_1`) | LIKE `_` is a wildcard | `ESCAPE '\'` via `db::prefix`; only `P_1-…` ids |
| Task id `FEAT-001` vs prefix `FE` | Trailing dash | `FE-%` does not match `FEAT-001` |
| Archived rows | Today excludes them | Still excluded (scoped and `--all`) |
| `--with-progress` sibling `progress.json` | Not a `task_list` | Still written next to dest; overwrite-guard is dest only |
| Concurrent add vs `--force` dump | **#1562** / **#2667** | Distinct tmp; `LockGuard` **inside** `export()` on `dest.is_file()` |
| `LockGuard` in `main.rs` **and** `export()` | Non-reentrant `flock` hang / `LockError` | Lock **only** inside `export()`. `main.rs` forwards `ExportOpts` |
| `scripts/claude-loop.sh` three `$PRD_FILE` dumps | Live smash; `\|\| true` hides the new refuse | **Remove** or retarget to an unregistered dump path. **Do not** add `--force` onto `$PRD_FILE` |
| CLI `export` after `--no-prefix` (`human_review_cli.rs`, `model_fields_cli.rs`, `cli_tests.rs:555`) | Zero-prefix default errors | Pass `--all`; dest stays a **new** file (no `--force`) |
| Second `enhance agents` reverts PR-2 CLARIFY | Hand-edit + `loop init` returns | Fenced block still has spawn-fixup (a) **and** `update --stdin`; no embed-in-JSON |
| `write_json_atomic` `.json.tmp` collision | Two dumps to same stem | unique_tmp_path |
| Library `export()` after `PrefixMode::Disabled` | In-tree round-trips | Callers pass `All`; dest new file ⇒ no `force` |
| CLI `export --to-json prd.json` in README | Smash recipe | Docs rewritten; `--force` named as lossy |
| Loop “export after every iteration” | ARCHITECTURE + `claude-loop.sh` | Docs corrected; **no** new `export()` call in the Rust engine; bash script retargeted/removed |
| `task_ops` 2027 bytes + two one-liners | Budget test fails | Trim; keep critical phrases; `.userStories[]` |
| jq `.tasks[]` left in `task_ops` | **#1252** empty results | Must be `.userStories[]` |
| Enhance spawn-fixup form (a) dropped | Pin 20 already shipped the flag | Form (a) **stays** |
| Two `--no-prefix` inits as “≥2 prefix” proof | 0 prefixes; default error is the zero-prefix path | Two `loop init`s **without** `--no-prefix` |
| `task_ops` / docs name `export` as JSON-sync recovery | Pin 11 | Must not. Recovery is `current` + retry `--from-json` |
| `ExportedPrd` round-trip through `PrdUserStory` | Pin 12 / 17 | Do not. Serialize `ExportedPrd` only |
| Adding `taskPrefix` to `ExportedPrd` to “avoid --force” | Weakens pin 9 | **Forbidden** — dump stays lossy |
| Historical `tasks/*-prompt.md` | Pin 21 | Do not rewrite |
| `~/.claude/docs/task-mgr-best-practices.md` | Not in repo | CHANGELOG residual only |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **CONTRACT-001** owner: `src/commands/export/{mod.rs,prd.rs}` — scoped dump (active prefix / `--from-json` / `--all`) + matching `prd_metadata` + trailing-dash task filter. Empty-prefix metadata is `prd_metadata.id = identity-matched prd_files.prd_id` (identity helper returns `prd_id`; **no** `WHERE task_prefix IS NULL` without that id). Consumers: US-001, US-003, US-006, US-007. Reuses `resolve_context` and `db::prefix`. Does **not** write `ctx.prd_json_path`.
- **CONTRACT-002** owner: export overwrite-guard + dest write — `--force` dump, pin-19 identity including live-path pair, `LockGuard` **inside** `export()` after `dest.is_file()` (not in `main.rs`), `unique_tmp_path`. Consumers: US-002, US-003, US-006, US-007. **Must not** merge dest; **must not** deserialize dest to `PrdUserStory`.

Docs / `task_ops` / intents / enhance / cheatsheet are **not** a CONTRACT-xxx (single narrative surface, no shared abstraction beyond clap help). They are US-004 / US-005.

**Data Flow Contracts:**

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| Active prefix | `resolve_context` → `ResolvedContext.prefix: String` | `let ctx = resolve_context(conn, from_json, "export")?;` — `None` + not `--all` → error naming `--from-json` / `--all` / `current`. Empty prefix → **no** LIKE filter |
| Task filter | prefix `String` → LIKE pattern `"{escaped}-%"` | `let (clause, pat) = db::prefix::prefix_and(Some(&prefix));` then `WHERE archived_at IS NULL {clause}` bind `pat`. **`--all` / empty prefix:** omit clause |
| Metadata scoped (named) | `prd_metadata.task_prefix` TEXT UNIQUE | `SELECT … FROM prd_metadata WHERE task_prefix = ?` with `&ctx.prefix` when non-empty |
| Metadata empty-prefix pin | `prd_files.prd_id` INTEGER → `prd_metadata.id` | Identity match of `--from-json` PATH returns `prd_id`. `SELECT … FROM prd_metadata WHERE id = ?`. **Grep `export/`: no `WHERE task_prefix IS NULL`.** |
| Metadata `--all` | first row | **keep** `ORDER BY id ASC LIMIT 1` |
| `--from-json` source pin | clap `Option<PathBuf>` → `resolve_context(conn, path, "export")` | Missing / directory / unregistered **all** go through this call. Registered via (a) **or** pin-19 (b)/(c). `prd_json_path` on ctx is **ignored for dest**. Errors command-name `"export"` |
| `--to-json` dest | clap `PathBuf` (required) | Write **that** PATH. Never `cli_write_path`, never remap |
| Overwrite-guard | dest `Path` → identity `Option<(prd_id, prefix)>` | Inside `export()`, after `dest.is_file()` + `LockGuard`: `find_registered_by_path_identity(conn, &dest.canonicalize()?)` → `Some` ⇒ require `--force`. Match (a) is **not** this function |
| Live-path pair | pin 19 (b)+(c) | `paths_identify(dest_canon, registered, source_root, worktree_root)` after joining relative `prd_files.file_path` to `source_root` |
| Dump body | `ExportedPrd { user_stories: Vec<ExportedUserStory> }` | `serde_json::to_string_pretty(&prd)` — **no** dest read, **no** `PrdUserStory`, **no** `taskPrefix` field |
| Tmp name | `.{basename}.{pid}-{n}-{nanos}.tmp` next to dest | `prd_json::unique_tmp_path(dest)` then rename (learning **#5577**) |
| `LockGuard` | `db_dir` | **Inside** `export()` only: `if dest.is_file() { let _lock = LockGuard::acquire(db_dir)?; }` then identity then write. `main.rs` does **not** acquire. Missing dest → no lock |
| `passes` | `tasks.status` TEXT → JSON bool | **unchanged**: `status == Done`. Dump is not a lifecycle write |

### Modularity & Coupling Targets

- **Target public surface**: clap `Commands::Export { to_json, with_progress, learnings_file, from_json, all, force }`; `export::ExportOpts` (or equivalent) replacing the 4-arg `export()`; reuse PR-1 identity + `unique_tmp_path`. **No new DB columns. No new subcommand. No MCP wrappers.**
- **Ownership**: scope + SQL in `export/`; identity stays in `context.rs`; tmp stays in `prd_json.rs`; docs in enhance/intents/cheatsheet/`task_ops`/README family.
- **Coupling budget**: export **must not** call `cli_write_path` for dest. export **must not** import `add` or call `preflight_from_json_path`. startup **must not** call overwrite-guard. `--from-json` **must not** call `init` / `register_prd_files`. Do not put ≥2 dump-refuse inside `resolve_context`. Do not share `refuse_unpinned_write` (wrong copy). `main.rs` Export arm **must not** `LockGuard::acquire`.
- **Cohesion**: `--all` LIMIT 1 lives next to `--all` task load (today’s dump is one code path). Overwrite-guard lives next to the dest write, not in `resolve_context`.

### When to Emit a CONTRACT-xxx Task

- **`CONTRACT-001`** — scoped export dump (default active prefix; `--from-json` pin; `--all` = today’s dump; named metadata `WHERE task_prefix = ?`; empty-prefix metadata `WHERE id = identity-matched prd_id`; trailing-dash LIKE). Priority 0–1, `taskType: "contract"`. Downstream: US-001, US-003, US-006, US-007.
- **`CONTRACT-002`** — overwrite-guard (`--force` dump, pin-19 live-path identity, `LockGuard` **inside** `export()` after `dest.is_file()`, unique tmp; not a merge). Downstream: US-002, US-003, US-006, US-007.

`dependsOn` on implementation tasks that cite a contract **must name that CONTRACT-xxx**.

---

## 3. User Stories

### US-001: Scoped dump + matching metadata (CONTRACT-001)

**As an** operator with two PRDs in one DB
**I want** default export to dump only the active PRD
**So that** PRD B’s tasks cannot land in PRD A’s file

**Acceptance Criteria:**

- [ ] `load_tasks(conn, prefix: Option<&str>)` — `None` = all unarchived (today). `Some(p)` if `p` is non-empty → `archived_at IS NULL AND id LIKE ? ESCAPE '\'` with `db::prefix::make_like_pattern(p)`. Empty string treated as `None` (do not LIKE `"-%"`)
- [ ] `load_prd_metadata` — `--all` / unscoped keeps `ORDER BY id ASC LIMIT 1`. Named prefix → `WHERE task_prefix = ?`. Empty-prefix `--from-json` → `WHERE id = ?` with `prd_id` from the identity match of the pin path. **Grep `export/`: no `WHERE task_prefix IS NULL`.** Promote `find_registered_by_path_identity` (or a `pub(crate)` sibling) to return `(prd_id: i64, prefix: Option<String>)` — today it returns prefix only
- [ ] Two prefixed inits, `--from-json` A, dest new file: exported ids all start with `"{A}-"`; none start with `"{B}-"`; `project` / `branchName` from A’s metadata even if B has a lower `prd_metadata.id`
- [ ] Two `--no-prefix` inits (distinct `project` / `branchName`), `--from-json` of file B: exported metadata is B’s row (`prd_id` match), **not** A’s (LIMIT 1 / `IS NULL` would pick the wrong one)
- [ ] `--all` on a two-prefix DB: task count = A+B unarchived; metadata = `ORDER BY id LIMIT 1` (today; **not** empty-prefix `prd_id` lookup)
- [ ] Archived rows excluded in both modes (seed one archived; it is absent)
- [ ] Prefix `_` wildcard: seed prefix `P_1`; only those ids (unit on `make_like_pattern`, not a naive `format!("{prefix}-%")`)
- [ ] `ExportedPrd` still has **no** `taskPrefix` field (lossy on purpose)

**edgeCases:** two prefixes + lower-id metadata; two `--no-prefix` inits + pin B; empty prefix must not `IS NULL`; archived; `_` in prefix; `--all` LIMIT 1 unchanged

---

### US-002: Overwrite-guard + `--force` dump (CONTRACT-002)

**As an** operator
**I want** `export --to-json` onto a live `task_list` to refuse unless I pass `--force`
**So that** a lossy dump cannot smash extra keys / sibling tasks / `humanReviewOutcome`

**Acceptance Criteria:**

- [ ] Dest exists + `find_registered_by_path_identity` hits a `task_list` row + no `--force` → `invalid_state("export", …)` naming `--force` and that export is a **dump not a merge**. Dest bytes **identical**. No serialize-then-refuse
- [ ] Same dest + `--force` → dest replaced with pretty `ExportedPrd`. Extra key seeded on the previous file is **gone**. `taskPrefix` absent. `LockGuard` acquired **inside** `export()` after `dest.is_file()`, before identity re-check and write
- [ ] Dest does not exist → write without `--force` (create); **no** `LockGuard`
- [ ] Dest exists but identity miss (`/tmp/dump.json`) → write without `--force`
- [ ] Dest is a directory → error; no dump
- [ ] `--to-json PATH` is the write target (never remapped via `cli_write_path`). Unit: grep `export/` for `cli_write_path` is empty
- [ ] `--force` is not a merge: do not read dest as `Value` / `PrdUserStory` to preserve keys
- [ ] Dest + learnings writes use `prd_json::unique_tmp_path` (not `with_extension("json.tmp")`). `prd_json` does not import `export`
- [ ] `--all --to-json` onto a registered path still requires `--force`
- [ ] Grep: `LockGuard` is **not** acquired in `main.rs` on the Export arm. `export()` is the only acquire site for this command. Never lock in both (deadlock)

**edgeCases:** same-PRD dest still needs `--force`; missing dest (no lock); unregistered existing file (lock then identity miss); directory; `--all` onto live path; unique tmp; dual-lock hang

---

### US-003: Clap pin / `--all` / `--force` + no-active error

**As a** loop agent
**I want** `export --from-json` / `--all` / `--force` on clap with pin help
**So that** docs that already mention `--from-json` are true, and ≥2-prefix default cannot smash

**Acceptance Criteria:**

- [ ] `Commands::Export { to_json, with_progress, learnings_file, from_json: Option<PathBuf>, all: bool, force: bool }`. `--all` **conflicts** with `--from-json`. `--to-json` still required. Help: `--from-json` **pin** this already-registered effort, not import
- [ ] `main.rs` dispatch **only** forwards `ExportOpts` (to_json / with_progress / learnings_file / from_json / all / force). **No** `LockGuard` in the Export arm
- [ ] `--from-json` missing / directory / unregistered: **all** go through `resolve_context(conn, from_json, "export")` before dump. Copy for unregistered names `loop init`. Do **not** call `preflight_from_json_path`. If a helper is extracted, it lives in `context.rs` with a `command` parameter. Grep `export/`: no `"add"` in `invalid_state` command-name
- [ ] No `--from-json`, no `--all`, `resolve_context` `None` → `invalid_state("export", …)` naming **`--from-json` / `--all` / `task-mgr current`**. Zero-prefix DBs: same error on default; `--all` succeeds
- [ ] `--all --from-json X`: clap parse failure (`cargo test -p task-mgr cli::`)
- [ ] Existing four Export clap tests updated for the new fields; new tests for `--from-json`, `--all`, `--force`, and the conflict
- [ ] `--from-json` does not insert `prd_files` / `prd_metadata` rows
- [ ] Flag vs env mismatch: flag wins (resolve_context already notes); dest still `--to-json`
- [ ] CLI callers after `--no-prefix` pass `--all`; dest stays a **new** file so `--force` is not required: `tests/human_review_cli.rs` (export sites `:211`, `:248`, `:289`, `:311`), `tests/model_fields_cli.rs`, `tests/cli_tests.rs:555` (`test_export_roundtrip`). Library `PrefixMode::Disabled` → `All` is US-001 / inversion, not this bullet

**edgeCases:** ≥2 vs 0-prefix default error names three tokens; clap conflict; directory `--from-json` says `"export"` not `"add"`; unregistered names `loop init`; `cli::` tests; `human_review_cli.rs --all`

---

### US-004: `task_ops` alignment (< 2048 bytes)

**As a** loop agent
**I want** the injected task-ops section to show the real pin, `update`, and jq path
**So that** I do not jq `.tasks[]` or add without `--from-json`

**Acceptance Criteria:**

- [ ] jq example uses `.userStories[]` (not `.tasks[]`) — learning **#1252** / **#4114** / **#3332**
- [ ] Add example includes `--from-json tasks/<prd>.json` (and still `--stdin` / `--depended-on-by` / “in response to a milestone”)
- [ ] One-liner for `task-mgr update --stdin` (cite `tasks/prd-agent-task-ops-pr2.md`: overlay JSON with `id` + whitelist fields; pin with `--from-json` when ≥2 prefixes). Do **not** re-specify overlay reject rules here
- [ ] `task_ops_section().len() < 2048`. Trim existing prose as needed. **Keep** every phrase `test_section_contains_critical_phrases` / `test_section_uses_correct_path` already asserts
- [ ] New asserts: contains `.userStories[]`, `--from-json`, `task-mgr update`; does **not** contain `.tasks[]`; does **not** tell agents to recover JSON-sync via `export`
- [ ] Suggested trim (implementer may vary, budget is the gate): drop “For anything else … `task-mgr --help`”; fold priority sentence; keep the jq example to one line

**edgeCases:** 2027→budget overflow; `.tasks[]` leftover; pin 11 `export` sneaking in as recovery copy

---

### US-005: Agent-facing docs tell the same story

**As an** agent following enhance / intents / cheatsheet / README
**I want** every surface to agree that `--from-json` is a pin and export onto a `task_list` needs `--force`
**So that** I cannot smash a PRD by following the docs

**Acceptance Criteria:**

- [ ] Enhance spawn-fixup form **(a)** `--from-json tasks/<correct-prd>.json` **stays** (already true after PR-1). Do not drop it. Do not rewrite historical `tasks/*-prompt.md`
- [ ] Enhance CLI cheat sheet add example includes `--from-json` (template `:49-59` currently does not). After the template rewrite, run `task-mgr enhance agents` (do not hand-edit inside `TASK_MGR:BEGIN/END`). Do **not** redo PR-2’s CLARIFY block. **Grep the regenerated fenced block:** still contains spawn-fixup (a) `--from-json tasks/<correct-prd>.json` **and** `update --stdin`; does **not** tell agents to embed `humanReviewOutcome` by hand-editing JSON and `loop init --append --update-existing` as the field-write path
- [ ] Intents “where will my add land”: recipe shows `task-mgr current --from-json tasks/<prd>.json` as a **real** pin (and/or `add --from-json`). `how.rs` / `intents.rs` tests: query `where will my add land` contains `--from-json`
- [ ] Intents “view active”: example includes `task-mgr current --from-json tasks/<prd>.json`. Query `view active` still matches; recipe still lists `from-json (--from-json flag)` as a real source
- [ ] Cheatsheet (PR-1 baseline already has `add --from-json`): add an **update** recipe (`update --stdin --from-json`) and an **export** recipe (default = active PRD; `--all` restores dump-all; registered dest needs `--force`). Stay ≤30 curated lines. Still forbids `set-status`, `recall --top-k`, `learnings show`. Do **not** re-add `add --from-json` to the forbidden list
- [ ] Rewrite smash recipes: `README.md:90` / `:526`, `docs/INTEGRATION.md` (`prd.json` / `$PRD_FILE` / `tasks/project.json` dests), `docs/QUICKSTART.md:299`. Either dest is an unregistered dump path, or the command includes `--force` **and** a lossy-dump warning. Prefer `--from-json` when showing a scoped dump
- [ ] `docs/ARCHITECTURE.md:472` “Export PRD JSON after every iteration”: correct to loop persistence via `prd_reconcile` / add / update; operator `export --to-json` onto a registered `task_list` requires `--force`. **Do not** add an `export()` call to the Rust loop
- [ ] `scripts/claude-loop.sh`: **remove** or retarget the three `export --to-json "$PRD_FILE"` calls (`:164` cleanup, `:533` per-iteration, `:552` final). Do **not** add `--force` onto `$PRD_FILE`. If retargeted, dest is an unregistered dump path and the same lossy-dump warning as INTEGRATION applies. Grep: no `export --to-json "$PRD_FILE"` remains
- [ ] `CHANGELOG.md` `[Unreleased]`: breaking export default + `--force`; `--all` restores dump-all. **Residual note** (not a code story): operator should copy spawn-fixup / add / update / export recipes into `~/.claude/docs/task-mgr-best-practices.md` after merge — **do not implement a copy of that file in this repo**

**edgeCases:** form (a) dropped; CLARIFY block accidentally reverted by `enhance agents`; cheatsheet line budget; CHANGELOG residual vs implementing best-practices; ARCHITECTURE invents a loop `export()`; `claude-loop.sh` `--force` onto `$PRD_FILE`

---

### US-006: Worktree dest identity (rust tests)

**As an** operator on a linked worktree
**I want** `--to-json` of the worktree task-list to count as a registered dest
**So that** `--force` is required on the live copy (**#4237** / pin 19)

**Acceptance Criteria:**

- [ ] Live-path tests following `tests/worktree_db_resolution.rs` (spawn `git worktree add`); **not** the verify-task-mgr sandbox
- [ ] Worktree file exists + `--to-json <worktree>/tasks/foo.json` without `--force` → refuse; worktree bytes identical; main JSON identical
- [ ] Same dest + `--force` → worktree file replaced (lossy); main JSON **unchanged** (`--to-json` was the worktree PATH, never remapped away)
- [ ] `--to-json` of an unregistered path from worktree cwd → no `--force`
- [ ] DB anchoring unchanged: rows still in main-repo `.task-mgr`
- [ ] `--from-json` of the worktree file (registered via pin 19 (c)) scopes the **source**; dest is still `--to-json`

**edgeCases:** worktree dest vs main dest; `--from-json` source vs `--to-json` dest; DB still main checkout

---

### US-007: verify-task-mgr sandbox proof (user-facing)

**As a** reviewer
**I want** an isolated-sandbox drive of scoped export / `--force` / `--all` / default error
**So that** green unit tests cannot ship a clap-less smash dump

**Acceptance Criteria:**

- [ ] **Create** `.claude/skills/verify-task-mgr/features/export-scoped-and-force.md` following `features/README.md` (Sub-features, How to get to it, Driving it, Gotchas) **before** any drive AC. Link it from `features/README.md`
- [ ] Drive via `.claude/skills/verify-task-mgr/SKILL.md` on a **later** FEAT or REVIEW-001 (do not put the skill-drive on a task that runs before the feature file exists)
- [ ] Proof (artifacts kept): single-prefix default dump to a **new** file (task count = that PRD); `--to-json` onto the registered PRD path without `--force` is non-zero and jq/sql dest unchanged; `--force` replaces dest (lossy: no `taskPrefix`); `--all` on a two-prefix DB dumps both prefixes’ tasks; default export with ≥2 prefixes and no pin is non-zero and names `--from-json`, `--all`, and `current`; `--all --from-json` clap-fails; `--from-json` pin dumps only that prefix; zero-prefix DB default errors (same three names), `--all` works; `export --help` says **pin**; `task_ops` not in this harness (unit test). ≥2-prefix proof is **two `loop init`s without `--no-prefix`**
- [ ] Worktree dest-identity cases are **not** claimed via this harness (US-006 rust tests)
- [ ] Compile/unit tests alone do not satisfy this story

**edgeCases:** helper unsets `TASK_MGR_ACTIVE_PREFIX`; ≥2-prefix proof is two prefixed inits; create recipe then drive; dest-new-file vs dest-registered

---

## 4. Functional Requirements

### FR-001: Scoped dump (CONTRACT-001)

Default export dumps the active PRD’s tasks (`"{prefix}-"` LIKE) and that row’s `prd_metadata`. `--from-json` pins the source effort. `--all` restores today’s all-tasks + LIMIT 1 metadata dump. Empty prefix does not LIKE `"-%"`. Empty-prefix metadata is `prd_metadata.id = identity-matched prd_files.prd_id`; `WHERE task_prefix IS NULL` without that id is forbidden.

**Validation:** US-001 two-prefix fixtures; two `--no-prefix` inits + pin B; `--all` count; grep no `IS NULL` in `export/`.

### FR-002: Overwrite-guard (CONTRACT-002)

`--to-json` onto a registered `task_list` (pin-19 identity including live-path pair) requires `--force`. `--force` is a lossy `ExportedPrd` dump (unique tmp + rename), not a merge. `LockGuard` is acquired **inside** `export()` after `dest.is_file()`, before identity re-check and write. `main.rs` only forwards `ExportOpts`. Missing dest → no lock. `--to-json PATH` is never remapped.

**Validation:** US-002 dest-registered refuse; `--force` drops extra keys; grep no `LockGuard` in `main.rs` Export arm; US-006 worktree dest.

### FR-003: Clap + no-active error

`--from-json` / `--all` / `--force` on Export; `--all` conflicts `--from-json`; help says pin. Missing/directory/unregistered `--from-json` go through `resolve_context(..., "export")` (not `preflight_from_json_path`). No active PRD and no `--from-json`/`--all` → `invalid_state` naming `--from-json` / `--all` / `task-mgr current`. Zero-prefix DBs can `--all`. CLI `--no-prefix` export tests pass `--all`.

**Validation:** US-003 `cargo test -p task-mgr cli::`; `human_review_cli.rs` / `model_fields_cli.rs` / `cli_tests.rs:555`; US-007 sandbox.

### FR-004: `task_ops` < 2048

jq `.userStories[]`; add `--from-json` one-liner; `task-mgr update` one-liner (PR-2). Budget held. No `export` as JSON-sync recovery.

**Validation:** US-004 unit tests on the section string.

### FR-005: Docs alignment

Spawn-fixup (a) stays. After `enhance agents`, fenced block still has form (a) **and** PR-2 `update --stdin` CLARIFY (no hand-edit + `loop init`). Intents where/land + view active treat `--from-json` as real. Cheatsheet update + export `--force`. README / INTEGRATION / QUICKSTART / ARCHITECTURE smash recipes rewritten. `scripts/claude-loop.sh` three `$PRD_FILE` dumps removed or retargeted (**not** `--force` onto `$PRD_FILE`). CHANGELOG breaking + best-practices residual (no copy in repo).

**Validation:** US-005 greps / `how` tests / fenced-block grep / `claude-loop.sh` grep.

### FR-006: User-facing verify

Create the feature recipe, then drive it.

**Validation:** US-007 artifacts.

---

## 5. Non-Goals (Out of Scope)

The following are explicitly **NOT** part of this work:

- Remapper / `add --from-json` / `current --from-json` clap (pins 1–4, 14–15, 20) — Reason: PR-1; this PR consumes the surface
- `task-mgr update` overlay / `humanReviewOutcome` field / error_recovery CLARIFY (pins 5–7, 10, 12, 17) — Reason: PR-2; this PR only **names** `update --stdin` in `task_ops` / cheatsheet
- Claim-scoped short `<task-status>` ids — Reason: pin 21
- A generic `set-status` command — Reason: pin 21
- `add --from-json` creating/registering a new PRD — Reason: pin 13 / 21
- Rewriting historical `tasks/*-prompt.md` — Reason: pin 21
- Changing DB anchoring (main checkout `.task-mgr` from a worktree stays) — Reason: pin 21
- MCP task wrappers — Reason: pin 21
- Putting `priority` on the update whitelist — Reason: pin 21 / PR-2
- Copying or editing `~/.claude/docs/task-mgr-best-practices.md` — Reason: not in this repo; CHANGELOG residual only
- Adding `taskPrefix` (or extra keys / `humanReviewOutcome`) to `ExportedPrd` so dump is “safe” without `--force` — Reason: pin 9 / 18; lossiness is the guard
- Making `--force` a merge / Value-preserve of dest — Reason: pin 18 dump
- Calling `export()` from the loop engine to make ARCHITECTURE true — Reason: Rust loop already persists via `prd_reconcile`; do not add a smash path
- Adding `--force` onto `scripts/claude-loop.sh` `$PRD_FILE` — Reason: pin 9; remove or retarget instead
- Reusing `add::preflight_from_json_path` from export — Reason: private; hardcodes `"add"`; coupling budget forbids `export` → `add`
- Prefix-scoping `--with-progress` / `--learnings-file` — Reason: out of this slice; keep DB-global
- Using `refuse_unpinned_write` for export default — Reason: wrong copy; `current` stays a probe; export names `--from-json` / `--all` / `task-mgr current`
- Sharing CLI overwrite-guard / `exists()` into loop startup — Reason: pin 15
- `WHERE task_prefix IS NULL` for empty-prefix metadata — Reason: multiple NULLs; must key by identity-matched `prd_id`

---

## 5.5. Low-Value / High-Effort Areas (Explicit Cuts or Deferrals)

| Area / Capability | Why the value is low relative to cost | Rough effort cost | Recommended action |
| --- | --- | --- | --- |
| Make `ExportedPrd` a lossless merge (extra keys + `taskPrefix`) | Would invite dropping `--force` and re-smash via a “safe” overwrite; PR-2 persist path is `update` / `prd_json` patch | High | **Cut** — dump stays lossy; `--force` stays required |
| `--all` as a JSON array of PRDs | New schema; breaks every round-trip test; pin 8 says restore **today’s** dump | High | **Cut** — LIMIT 1 + all tasks |
| Default dest = `ctx.prd_json_path` when `--to-json` omitted | Hidden smash; `--to-json` is required today | Medium | **Cut** — dest always explicit |
| verify-task-mgr git worktree recipe | Harness is `--dir` isolated; rust tests already spawn worktrees | High | **Defer** — US-006 owns live dest |
| Prefix-filter progress/learnings export | Rare; not the smash bug | Medium | **Cut** |
| Implement best-practices.md in-tree | File is global, not in the repo | Low now / process-wrong | **Cut** — CHANGELOG residual |

**Rationale:** The user-visible win is “export cannot smash a PRD, and agents are told the same pin/`update`/jq story.” A lossless exporter or a new `--all` schema would be a different feature and would undermine `--force`.

---

## 6. Technical Considerations

### Affected Components

- `src/commands/export/mod.rs` — `ExportOpts` / flags; overwrite-guard; `LockGuard` policy; unique tmp
- `src/commands/export/prd.rs` — `load_tasks(prefix)`, `load_prd_metadata(prefix)`; keep LIMIT 1 on `--all`
- `src/commands/export/tests.rs` — `PrefixMode::Disabled` callers pass `All`; dest-registered cases
- `src/cli/commands.rs` — Export flags + pin help
- `src/cli/tests.rs` — parse + conflict (`cargo test -p task-mgr cli::`)
- `src/main.rs` — dispatch **only** forwards `ExportOpts`. **No** `LockGuard` on the Export arm
- `src/commands/context.rs` (PR-1) — reuse `resolve_context` / `find_registered_by_path_identity`; **promote** identity to return `prd_id` + prefix. **No remap rewrite**. Optional: extract missing/directory helper here with a `command` parameter (not in `add.rs`)
- `src/commands/prd_json.rs` (PR-1) — reuse `unique_tmp_path` only
- `src/db/prefix.rs` — reuse LIKE helpers
- `src/loop_engine/prompt_sections/task_ops.rs` — jq / add pin / update; budget
- `src/commands/intents.rs` + `src/commands/how.rs` tests — where/land + view active
- `src/commands/enhance/templates.rs` — add example `--from-json`; spawn-fixup (a) stays
- `CLAUDE.md` managed `TASK_MGR` block — regenerate via `task-mgr enhance agents`
- `src/commands/cheatsheet.rs` — update + export recipes
- `README.md`, `docs/INTEGRATION.md`, `docs/QUICKSTART.md`, `docs/ARCHITECTURE.md` — smash recipes
- `scripts/claude-loop.sh` — remove/retarget three `$PRD_FILE` dumps
- `CHANGELOG.md` — breaking + BP residual
- `tests/import_export.rs`, `tests/model_fields_*.rs`, `tests/e2e_loop.rs`, `tests/prd_max_retries_round_trip.rs` — library `All`
- `tests/human_review_cli.rs`, `tests/model_fields_cli.rs`, `tests/cli_tests.rs` — CLI `--no-prefix` export → `--all`; dest new file
- `tests/worktree_db_resolution.rs` (or sibling) — dest identity
- `.claude/skills/verify-task-mgr/features/export-scoped-and-force.md` — **create**
- `.claude/skills/verify-task-mgr/features/README.md` — link

### Dependencies

- **Internal:** PR-1 `resolve_context` / `paths_identify` / `find_registered_by_path_identity` (promoted to return `prd_id`) / `unique_tmp_path` at merge. **Not** `preflight_from_json_path`
- **Internal:** PR-2 `task-mgr update` at merge for the prompt one-liner (cite `tasks/prd-agent-task-ops-pr2.md` if the binary on the branch does not yet have it — author-ahead: do not stub a fake update)
- **Internal:** `db::prefix`, `LockGuard`
- **External:** none. Loop engine does not call export

### Approaches & Tradeoffs

No `/spike` on this slice. Pins already chose scoped default vs dump-all, `--force` dump vs merge, and path identity vs basename search.

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| **A. CLI default = active prefix; `--all` = today’s dump; dest identity `--force` dump** | Stops smash; operators who want the old dump keep `--all`; `--force` is honest about lossiness | Breaking CLI default; in-tree `--no-prefix` tests must pass `All` | **Preferred** |
| **B. Keep dump-all default; only add `--force` when dest is registered** | Smaller CLI break | Two-prefix dump still mixes tasks into a new file; docs still lie about “the” PRD; pin 8 forbids | **Rejected** |
| **C. `--force` Value-merges dest so extra keys survive** | Looks friendlier | Silent partial merge; `passes` / sibling tasks still clobber; pin 18 forbids; reimplements PR-2 patch in the wrong command | **Rejected** |

**Selected Approach**: A. One `ExportOpts` with explicit `from_json` / `all` / `force`. Default CLI path uses `resolve_context`. Dest write is always `--to-json`. Overwrite-guard is pin-19 identity. Dump serializes `ExportedPrd` through `unique_tmp_path`.

**Phase 2 Foundation Check**: Approach A costs ~1 day of scope + guard now and is the only substrate that makes PR-2’s JSON-only `humanReviewOutcome` survive an operator “backup”. Approach B/C would force a rewrite the first time a two-PRD DB or a CLARIFY outcome met `export --to-json tasks/foo.json`. “Approach A costs a scoped dump + `--force` now but avoids smash + a fake merge later.”

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| `--no-prefix` / 0-prefix default export breaks in-tree round-trips | CI red; operators on `--no-prefix` DBs see a new error | High | CONTRACT-001: `None` → error unless `--all`. Empirical: library `PrefixMode::Disabled` → `All`; CLI `human_review_cli.rs` / `model_fields_cli.rs` / `cli_tests.rs:555` pass `--all`. US-003 / US-007 |
| Same-PRD dest allowed without `--force` because “it’s scoped” | Lossy smash of extra keys / `humanReviewOutcome` | High if guard is “other PRD only” | Pin 9: identity hit **always** needs `--force`. Empirical: US-002 extra-key seed |
| Overwrite-guard uses match (a) | `--force` required on a stray same-prefix copy; live path might miss | Med | CONTRACT-002: pin-19 `find_registered_by_path_identity` only. Empirical: stray-copy unit |
| Dest remapped via `cli_write_path` | `--to-json` writes the worktree copy when the operator named main (or vice versa) | High if reuse is sloppy | Grep `cli_write_path` in `export/`. Empirical: US-002 / US-006 |
| `--all` “fixed” to drop LIMIT 1 | Pin 8 violated; round-trips change metadata | Med | US-001: `--all` metadata test vs today’s LIMIT 1 |
| LIKE without trailing dash / ESCAPE | Wrong PRD’s tasks; `_` wildcards | Med | Reuse `db::prefix`. Empirical: `P_1` + `FE` vs `FEAT` |
| Empty prefix LIKE `"-%"` | Matches `-FEAT-001` junk | High if `Some("")` is naively formatted | Empty prefix = no LIKE. Empirical: US-001 |
| `task_ops` exceeds 2048 | Prompt budget / unit fail | High (2027 today) | US-004 trim list + budget test |
| jq `.tasks[]` left in | Agents get empty jq (**#1252**) | High if only the add line is edited | Negative assert `.tasks[]` |
| ≥2 sandbox = two `--no-prefix` | Refuse/default-error never fires | High | Two prefixed `loop init`s. US-007 gotcha |
| Skill-drive before feature file | PR-1 mechanical miss | Med | US-007: create then drive on a later FEAT / REVIEW-001 |
| ARCHITECTURE “fix” adds loop `export()` | New smash path inside the engine | Med | Non-goal; docs-only correction |
| Best-practices.md copied into the repo | Wrong tree; pin 21 spirit | Med | CHANGELOG residual only |
| Clap tests not updated | `cli::` compile fail | High | US-003 first; `cargo test -p task-mgr cli::` |
| `refuse_unpinned_write` reused for export | `current` probe confused; wrong stderr | Med | Distinct error naming `--from-json` / `--all` / `task-mgr current` |
| `LockGuard` in `main.rs` **and** `export()` | Deadlock / `LockError` (non-reentrant flock) | High if dispatch mirrors other write commands | CONTRACT-002 / US-002 / US-003: lock **inside** `export()` only. Grep Export arm. Empirical: missing dest has no lock file |
| `preflight_from_json_path` reused from export | Export errors say `"add"`; `export` → `add` import | High if US-003 AC followed as first written | US-003: `resolve_context(..., "export")`. Grep `"add"` in export invalid_state |
| `claude-loop.sh` `$PRD_FILE` dumps left | Crash recovery silently dies (`\|\| true`) or `--force` smash | High | US-005: remove or retarget; never `--force` onto `$PRD_FILE` |
| Empty-prefix `WHERE task_prefix IS NULL` | Wrong `project`/`branchName` with two `--no-prefix` inits | High | CONTRACT-001: `id = identity-matched prd_id`. Empirical: US-001 two-NULL fixture |
| Second `enhance agents` restores hand-edit CLARIFY | PR-2 recipe dies | Med | US-005 grep: fenced block has `update --stdin` **and** spawn-fixup (a) |

Top 3 = dual `LockGuard` deadlock, `claude-loop.sh` silent smash, empty-prefix wrong metadata. All have empirical ACs after this fold. No High×High unmitigated blocker.

### Security Considerations

- `--from-json` / `--to-json` paths are trusted CLI input (same class as add pin / today’s export dest). Do not follow `--from-json` into registration.
- Do not write outside the `--to-json` PATH. No invent, no remap dest.
- `LockGuard` **inside** `export()` after `dest.is_file()` (live dest). Missing dest → no lock. Tmp files stay in the dest directory (rename atomicity; no `/tmp` staging — **#2667**).
- Parameterized LIKE (`?` + `ESCAPE '\'`); do not interpolate prefix into SQL.
- Do not log dump bodies at `tracing` info (may contain notes).

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `export::ExportOpts` | `{ to_json: &Path, with_progress: bool, learnings_file: Option<&Path>, from_json: Option<&Path>, all: bool, force: bool }` | n/a | n/a | n/a |
| `export::export` | `fn export(dir: &Path, opts: &ExportOpts) -> TaskMgrResult<ExportResult>` | `{ prd_file, tasks_exported, … }` | no active PRD (names `--from-json` / `--all` / `current`); unregistered pin; dest registered without `--force`; clap-equivalent flag conflict is CLI-only | **inside** `export()`: `dest.is_file()` ⇒ `LockGuard` then identity then write. Missing dest ⇒ no lock. Unique tmp + rename dump |
| `export::load_tasks` | `(conn, prefix: Option<&str>)` | `Vec<ExportedUserStory>` | sqlite | none |
| `export::load_prd_metadata` | named: `(conn, prefix: &str)`; empty-prefix: `(conn, prd_id: i64)`; `--all`: no prefix | `PrdMetadata` | sqlite | none (`unknown` defaults if missing). Empty-prefix **must** take `prd_id`, not `IS NULL` |
| identity helper | pin-19; returns `(prd_id, prefix)` | `Option<(i64, Option<String>)>` | none | none (read `prd_files` + `prd_metadata`) |
| clap `Export.from_json` | `Option<PathBuf>` `--from-json` | parsed path | optional | none |
| clap `Export.all` | `bool` `--all`, conflicts `--from-json` | parsed | clap conflict | none |
| clap `Export.force` | `bool` `--force` | parsed | none | none |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `export::export` | `(dir, to_json, with_progress, learnings_file)` = dump-all, no force | `ExportOpts` with explicit scope | **Yes, rust API** | In-tree tests pass `all: true` when they used `PrefixMode::Disabled` / want today’s dump; `force: true` only if dest is a registered `task_list` |
| `Commands::Export` | `{ to_json, with_progress, learnings_file }` | `+ from_json, all, force` | **Yes, CLI default** | Default is scoped; `--all` restores dump-all; registered dest needs `--force` |
| `load_prd_metadata` | LIMIT 1 always | LIMIT 1 iff `--all` / unscoped | Yes, internal | Callers in export tests updated |
| `load_tasks` | all unarchived | optional prefix LIKE | Yes, internal | `--all` / empty prefix keep all rows |
| `write_json_atomic` tmp | `with_extension("json.tmp")` | `unique_tmp_path` | Internal | Same rename semantics |
| README / INTEGRATION export dest | live `tasks/*.json` | dump path or `--force` | **Yes, docs** | US-005 |

### Data Flow Contracts

See §2.6 table. Type transition to flag: dest overwrite-guard is **path identity**, not JSON `taskPrefix`. Using match (a) here is the silent wrong-file `--force` (or missed live path). `--from-json` source pin **does** use (a) OR (b)/(c) via `resolve_context`. Dest write **never** uses `ResolvedContext.prd_json_path`.

`--all` metadata is still `ORDER BY id ASC LIMIT 1` — that is a type-level “we do not have a multi-PRD export schema,” not a bug to fix in this PR.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `src/commands/export/mod.rs:72` | dump-all `export()` | BREAKS (intended scope) | `ExportOpts`; default CLI scoped |
| `src/commands/export/prd.rs:132` | LIMIT 1 always | BREAKS scoped metadata | LIMIT 1 only on `--all` |
| `src/commands/export/prd.rs:165` | all tasks | BREAKS (intended filter) | prefix LIKE |
| `src/commands/export/mod.rs:136` | `.json.tmp` | NEEDS REVIEW | `unique_tmp_path` |
| `src/main.rs:596-604` | no lock, 4-arg export | BREAKS compile | new flags only — **do not** add `LockGuard` here |
| `src/cli/tests.rs:734-822` | 3-field Export match | BREAKS compile | US-003 |
| `src/commands/export/tests.rs` | `PrefixMode::Disabled` + 4-arg | BREAKS default | pass `All`; dest is new file |
| `tests/import_export.rs` and siblings listed in effort check | same | BREAKS | `All` |
| `tests/model_fields_cli.rs` | CLI `export --to-json` after `--no-prefix` | BREAKS default | `--all`; dest stays new file |
| `tests/human_review_cli.rs:211` etc. | same | BREAKS default | `--all`; dest stays new file |
| `tests/cli_tests.rs:555` | `test_export_roundtrip` after `--no-prefix` init | BREAKS default | `--all`; dest stays new file |
| `README.md:90`, `docs/INTEGRATION.md:*`, `docs/QUICKSTART.md:299` | smash dest | BREAKS (intended docs) | US-005 |
| `scripts/claude-loop.sh:164,533,552` | `export --to-json "$PRD_FILE" \|\| true` | BREAKS (silent smash / silent skip) | US-005 remove or retarget; **not** `--force` |
| `docs/ARCHITECTURE.md:472` | “export after every iteration” | NEEDS REVIEW | docs-only; no Rust engine call; bash script is the live caller |
| `src/loop_engine/prompt_sections/task_ops.rs:58` | `.tasks[]` | BREAKS (intended) | `.userStories[]` |
| `src/commands/cheatsheet.rs:181` (main) | forbids `add --from-json` | OK after PR-1 merge | do not re-forbid |
| PR-1 `context.rs` `resolve_context` | pin protocol | OK if export passes `"export"` | US-003 |
| `src/commands/enhance/templates.rs:123` | spawn-fixup (a) | OK if left | US-005 stays |
| add/update JSON-sync warning | pin 11 never names `export` | OK | do not reintroduce |
| Loop engine | no `export()` caller | OK | do not add one |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| `init --from-json` | import shim | registers PRD | **unchanged** |
| `add/update/current --from-json` | pin dest/context | PR-1/PR-2 | **unchanged**; export `--from-json` pins **source** only |
| `export --from-json` | source pin | flag does not exist | pin source prefix/metadata; dest is `--to-json` |
| `export --to-json PATH` | dest | always dump-all, always write | write that PATH; `--force` if identity hits |
| `cli_write_path` | add/update dest | remap then `is_file()` | **not** used for export dest |
| `refuse_unpinned_write` | add/update ≥2 | write refuse | **not** used for export; export names `--from-json` / `--all` / `current` |
| `resolve_context` `Ok(None)` | 0 or 2+ | probe | export **errors** unless `--all` (three-token copy); current still probe |
| `add::preflight_from_json_path` | add pin, missing/directory | `invalid_state("add", …)` | **not** called by export; `resolve_context(..., "export")` |
| `LockGuard` in `add()` | add write | inside `add()`, not `main.rs` | export matches: inside `export()`, not `main.rs` |
| `LockGuard` in `main.rs` Export arm | n/a (today no lock) | **must stay unlocked** | dual acquire deadlocks |
| `--all` dump | restore today | n/a (today is the only mode) | all tasks + LIMIT 1 metadata |
| `prd_json::patch_user_story` / append | merge/append | PR-1/PR-2 | **not** called by export (`ExportedPrd` dump) |
| Loop `prd_reconcile` | iteration persist | passes flip, unique tmp | **unchanged**; not `export()` |
| Enhance spawn-fixup (a) | docs | already present | **stays** after `enhance agents` |
| Enhance CLARIFY | PR-2 | `update --stdin` then `complete` | **do not redo**; regenerated block must still contain it (no hand-edit + `loop init`) |
| `claude-loop.sh` `$PRD_FILE` dump | bash crash recovery | smash dest + `\|\| true` | remove or unregistered dest; never `--force` onto `$PRD_FILE` |
| `task_ops` jq | prompt | `.tasks[]` | `.userStories[]` |
| JSON-sync recovery copy | add/update | `current` + retry `--from-json` | still never `export` |

### Inversion Checklist

- [x] All `export::export(` / CLI `export --to-json` callers identified (`export/tests.rs`, `import_export.rs`, `model_fields_*`, `e2e_loop.rs`, `prd_max_retries_round_trip.rs`, `model_fields_cli.rs`, **`human_review_cli.rs`**, **`cli_tests.rs:555`**, **`scripts/claude-loop.sh`**)
- [x] `--no-prefix` library round-trips must pass `All`; CLI `--no-prefix` export tests pass `--all`; dest stays a new file
- [x] `LockGuard` only inside `export()` after `dest.is_file()`; never in `main.rs` Export arm; missing dest → no lock
- [x] Do not call `preflight_from_json_path`; pin errors go through `resolve_context(..., "export")`; export errors must not say `"add"`
- [x] Empty-prefix metadata is `prd_id` from identity, not `WHERE task_prefix IS NULL`
- [x] No-active error names `--from-json` / `--all` / `task-mgr current`
- [x] `claude-loop.sh` three `$PRD_FILE` dumps removed or retargeted; never `--force` onto `$PRD_FILE`
- [x] After `enhance agents`, fenced block has spawn-fixup (a) **and** `update --stdin` CLARIFY
- [x] Same-PRD dest still needs `--force` (lossy)
- [x] `--to-json` must not go through `cli_write_path`
- [x] Overwrite-guard is pin-19, not match (a)
- [x] Empty prefix must not LIKE `"-%"`
- [x] `--all` keeps LIMIT 1
- [x] `cli::` Export matches will not compile until updated
- [x] `task_ops` budget 2027 — trim required
- [x] `.tasks[]` negative assert
- [x] Two prefixed inits for ≥2 default error, not `--no-prefix` twice
- [x] Skill-drive after feature file exists
- [x] Do not add loop `export()`
- [x] Do not copy best-practices.md into the repo
- [x] Pin 11 copy must not start naming `export` again
- [x] Spawn-fixup (a) stays; CLARIFY is PR-2 and must survive `enhance agents`

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/cli/commands.rs` `Commands::Export` rustdoc + flags | Update | Pin, not import; `--all` restores dump-all; `--force` dump not merge |
| `src/loop_engine/prompt_sections/task_ops.rs` | Update | US-004 |
| `src/commands/intents.rs` / `how.rs` tests | Update | where/land + view active |
| `src/commands/enhance/templates.rs` | Update | add example `--from-json`; spawn-fixup (a) stays |
| `CLAUDE.md` inside `TASK_MGR` markers | Update via CLI | `task-mgr enhance agents` after template rewrite. Grep: spawn-fixup (a) **and** `update --stdin`; no hand-edit + `loop init` CLARIFY |
| `src/commands/cheatsheet.rs` | Update | update + export `--force` recipes |
| `README.md`, `docs/INTEGRATION.md`, `docs/QUICKSTART.md` | Update | smash dest recipes |
| `docs/ARCHITECTURE.md` crash-recovery bullet | Update | `prd_reconcile`; `--force` if dest is a `task_list` |
| `scripts/claude-loop.sh` | Update | Remove or retarget `:164` / `:533` / `:552`. Never `--force` onto `$PRD_FILE` |
| `CHANGELOG.md` `[Unreleased]` | Update | breaking default + `--force` + `--all`; residual BP (not in repo) |
| `.claude/skills/verify-task-mgr/features/export-scoped-and-force.md` | Create | Operator drive |
| `.claude/skills/verify-task-mgr/features/README.md` | Update | Link |
| `~/.claude/docs/task-mgr-best-practices.md` | Residual BP after merge | **Not a story** |
| historical `tasks/*-prompt.md` | **Do not touch** | pin 21 |

### Institutional memory (recall)

Embed so the loop does not re-learn:

- **#1252** / **#4114** / **#3332** / **#3756**: PRD JSON key is `userStories`, not `tasks`. `task_ops` jq must match.
- **#2236**: omitted `--from-json` leaks fixups — pin + refuse; docs must show the flag.
- **#5596**: `--from-json` is a pin, not an import (export source pin too).
- **#5577** / **#2667** / **#4564** / **#1562**: unique tmp `.{base}.{pid}-{n}-{nanos}.tmp`, same-dir rename — export dump must leave `.json.tmp`.
- **#1561**: JSON sync is best-effort; recovery names `current` + retry `--from-json`, **never** `export`.
- **#2588**: `export/prd.rs` already uses graceful fallbacks on missing columns — do not hide a wrong prefix filter behind `.unwrap_or` of all rows.
- PR-1 feed-forward: reuse `resolve_context` / path identity / `cli_write_path` is **not** dest. Do not reimplement remap. Match (a) ≠ pin 19. Promote identity to return `prd_id`. Do not import `add::preflight_from_json_path`.
- PR-2 feed-forward: `task_ops` update one-liner from `tasks/prd-agent-task-ops-pr2.md` (`update --stdin` overlay; pin with `--from-json`). Do not re-open overlay whitelist. `enhance agents` must not revert the CLARIFY block.

---

## 7. Open Questions

None. Pins, shape, phase seed, PR-1/PR-2 feed-forward, the effort-check extraction table, and the architect fold answered the clarifying questions. Architect Questions for User: none. A still-blocking question would have been `PAUSE-NEEDED`.

---

## AA review (folded)

**Source:** `tasks/prd-agent-task-ops-pr3-architect.md` (NEEDS_CHANGES, 2026-09-09). Questions for User: none. First verdict had three Highs and determinate Mediums; all Suggested Revisions are now ACs / contract text in the body (not this note alone).

| Architect concern | Resolution |
| --- | --- |
| **High — LockGuard specified in two exclusive sites (deadlock)** | Folded into CONTRACT-002 / US-002 / US-003 / public contract / `main.rs` consumers: acquire `LockGuard` **inside** `export()` only, after `dest.is_file()`, before identity re-check and write. `main.rs` only forwards `ExportOpts`. Never lock in both. Missing dest → no lock. |
| **High — US-003 “reuse `preflight_from_json_path`” imports add and mislabels errors** | Folded into US-003 / FR-003 / coupling / style: missing/directory/unregistered go through `resolve_context(conn, from_json, "export")`. If extracted, helper lives in `context.rs` with a `command` parameter. Export errors must not say `"add"`. Do not call `add::preflight_from_json_path`. |
| **High — `scripts/claude-loop.sh` is the live smash caller** | Folded into US-005 / FR-005 / consumers / docs: **remove** or retarget the three `export --to-json "$PRD_FILE"` calls (`:164`, `:533`, `:552`). Do **not** add `--force` onto `$PRD_FILE`. Same lossy-dump warning as INTEGRATION. |
| **Medium — NULL-prefix metadata has no `prd_id` handle** | Folded into CONTRACT-001 / US-001 / data-flow / assumption 4: empty-prefix `--from-json` metadata is `prd_metadata.id = identity-matched prd_files.prd_id`. Promote identity to return `prd_id`. **Forbid** `WHERE task_prefix IS NULL` without that id. `--all` stays `ORDER BY id LIMIT 1`. Two `--no-prefix` inits + pin B is an AC. |
| **Medium — CLI `--no-prefix` export tests omitted** | Folded into US-003 / inversion / consumers: `human_review_cli.rs`, `model_fields_cli.rs`, and `cli_tests.rs:555` pass `--all`; dest stays a new file. |
| **Medium — no-active error names only `task-mgr current`** | Folded into US-003 / US-007 / FR-003 / success metrics: `invalid_state` expected names `--from-json` / `--all` / `task-mgr current`. |
| **Medium — second `enhance agents` can revert PR-2 CLARIFY** | Folded into US-005 / FR-005 / documentation: after `task-mgr enhance agents`, fenced block still has spawn-fixup (a) **and** PR-2 `update --stdin` CLARIFY; must not restore hand-edit + `loop init`. |

**Accepted residuals (not bugs; documented):**

- Match (a) can pin a stray same-prefix copy as **source**; overwrite-guard stays pin-19 (b)+(c) only. Do not reopen.
- `--all` keeps `ORDER BY id LIMIT 1` (today’s dump; not a multi-PRD schema).
- `~/.claude/docs/task-mgr-best-practices.md` stays a CHANGELOG residual (not in this repo).
- verify-task-mgr does not spawn git worktrees (US-006 rust tests own live dest).
- `ExportedPrd` stays lossy (no `taskPrefix` / extra keys) — that is why `--force` is required.

**Inversion table from the architect file — now guarded:** dual LockGuard (US-002/003), leftover `"add"` on the export path (US-003), `claude-loop.sh` smash (US-005), empty-prefix `IS NULL` (US-001), CLI `--no-prefix` `--all` (US-003), three-token no-active copy (US-003/007), enhance-agents CLARIFY revert (US-005). Previously already guarded items (same-PRD `--force`, `.tasks[]`, two prefixed inits, skill-drive order, pin 11 never names `export`) stay guarded.

---

## Appendix

### Related Documents

- Goal ledger: `tasks/prd-goal-agent-task-ops-ux-ledger.md`
- PR-1 PRD (consumed surface): `tasks/prd-agent-task-ops-pr1.md`
- PR-2 PRD (update one-liner): `tasks/prd-agent-task-ops-pr2.md`
- Prefix LIKE SSoT: `src/db/prefix.rs`
- Verify skill: `.claude/skills/verify-task-mgr/SKILL.md`

### Recommended `/prd-tasks` metadata

- `prdFile`: `prd-agent-task-ops-pr3.md`
- `branchName`: `feat/agent-task-ops-pr3`
- No `model`, no `taskPrefix` in generated JSON
- CONTRACT-001 / CONTRACT-002 as above; implementation `dependsOn` must name those ids when ACs cite them
- FEAT that **creates** the verify feature file must precede the FEAT/REVIEW that **drives** it
- ≥2-prefix export default error: two prefixed `loop init`s, never `--no-prefix` twice
- Empty-prefix metadata: identity returns `prd_id`; no `WHERE task_prefix IS NULL` in `export/`
- `scripts/claude-loop.sh` is in US-005 (remove/retarget, never `--force` onto `$PRD_FILE`)

### Glossary

- **Pin (export)**: `--from-json PATH` selects which already-registered effort to dump (source prefix + metadata). Never registers. Never remaps `--to-json`.
- **Dest**: `--to-json PATH`. Always that PATH. Overwrite-guard applies here, not to the pin.
- **Scoped dump**: unarchived tasks whose ids match `"{prefix}-"` (escaped LIKE) + matching `prd_metadata` row.
- **`--all` dump**: today’s export — all unarchived tasks + `prd_metadata ORDER BY id LIMIT 1`.
- **`--force`**: opt-in lossy replace of a registered `task_list`. Not a merge.
- **Live-path pair**: pin-19 identity of dest against a `prd_files` `task_list` row after `source_root.join` + `remap_into_worktree`.
- **Lossy**: `ExportedPrd` has no `taskPrefix`, no extra story keys, status collapsed to `passes`.
- **Identity `prd_id`**: `prd_files.prd_id` of the pin-19-matched `task_list` row. Empty-prefix metadata keys `prd_metadata.id` to this value. Overwrite-guard only needs `Some`.
