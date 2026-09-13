# PRD: Agent task-ops UX PR-1 — remapper, context, add/current `--from-json`

**Type**: Enhancement
**Priority**: P0 (Critical)
**Author**: Grok
**Created**: 2026-09-09
**Status**: Draft
**Goal ledger**: `tasks/prd-goal-agent-task-ops-ux-ledger.md` phase 1 (authoring)
**Related**: learnings **#4441**, **#4237**, **#2236**, **#3240**, **#1562**, **#3440**, **#2667**, **#2303**, **#2798**

---

## PRD-input note (effort check)

Phase 1 of the goal ledger is **authoring**, not STATUS-Done. This is a new goal; no prior `tasks/prd-agent-task-ops-pr1.md` exists. Pins, simplified shape (three serial PRs), and this phase seed are the answers that would otherwise have been Step 3 clarifying questions. This PRD does not add phases, change pins, or propose a different program.

**HEAD at authoring:** `e552877` (`main`, `Merge pull request #42 from chrisarm/chore/v0.3.3`).

**Re-located extraction targets (this HEAD — do not cite memory):**

| Target | File:lines | What is there now |
| --- | --- | --- |
| Context types | `src/commands/add.rs:414-447` | `ResolutionSource` (FromJsonFlag **reserved**), `ResolvedContext { prefix, source, prd_json_path }` |
| `resolve_context` | `src/commands/add.rs:454-478` | env → single-prefix; no `from_json` argument; `invalid_state` hardcodes `"add"` via `resolve_active_prefix` |
| `resolve_active_prefix` | `src/commands/add.rs:518-543` | stale-pin `invalid_state("add", …)`; 0 or 2+ prefixes → `Ok(None)` |
| `load_known_prefixes` | `src/commands/add.rs:484-491` | `WHERE task_prefix IS NOT NULL` |
| `locate_prd_json` | `src/commands/add.rs:709-740` | raw `prd_files.file_path`; prefix miss falls back to **first** `task_list` (`LIMIT 1`) — this is the “write PRD #1” bug |
| `append_task_to_prd_json` / `atomic_write` | `src/commands/add.rs:768-936` | tmp name `.{filename}.task-mgr-add.tmp` (learning **#1562**) |
| JSON-sync failure copy | `src/commands/add.rs:342-360` | comment + warning: DB committed; names **`task-mgr export`** |
| Live remap | `src/loop_engine/startup.rs:663-705` | inline `strip_prefix` + `working_root.join`; **no `exists()` on remap**; re-import later gates on `live_prd_file.exists()` |
| Copy-if-missing (not remap) | `src/loop_engine/startup.rs:618-661` | Step 8.4; `exists()` is copy policy, not discovery |
| Git helpers | `src/git/mod.rs:29-90` | `main_repo_root_at` / `is_inside_worktree_at` only. **No** `worktree_root`. **No** `remap_into_worktree` |
| cwd toplevel (duplicate) | `src/main.rs:35-56` | `get_project_root` shells `git rev-parse --show-toplevel` |
| `prd_files` storage | `src/commands/init/import.rs:519-560` | `strip_prefix(tasks_dir).unwrap_or(json_path)` — relative **or** absolute |
| unique tmp scheme | `src/loop_engine/prd_reconcile.rs:29-43` | `.{base}.{pid}-{n}-{nanos}.tmp`; private; test at `:1318` |
| Cheatsheet forbidden | `src/commands/cheatsheet.rs:174-189` and `tests/cheatsheet_drift.rs:376-393` | forbids `add --from-json` |
| Curated recipe | `src/commands/cheatsheet.rs:57` | `add --stdin --depended-on-by` only |
| Add clap | `src/cli/commands.rs:631-650` | no `--from-json` |
| Current clap | `src/cli/commands.rs:1202-1222` | `Current` unit variant; after_help already says “pass --from-json” |
| Dispatch | `src/main.rs:822-847`, `:902-906` | add/current take no path |
| Logging reuse | `src/main.rs:71-80` | `commands::add::resolve_context(&conn)` |
| DB-from-worktree contract | `tests/worktree_db_resolution.rs:159-175` (and siblings) | add from worktree cwd lands in **main** `.task-mgr`; do not change |
| Verify skill | `.claude/skills/verify-task-mgr/SKILL.md` | isolated `--dir` + `HOME`; no worktree recipe for add/current |

**Assumptions (not pins — stated so implementers do not invent):**

1. `current` without `--from-json` and with ≥2 prefixes stays a **probe** (`context: None`, exit 0). The refuse is a **write** refusal (`add`). `--from-json` unregistered is an error on both.
2. `verify-task-mgr` sandboxes do not create linked git worktrees. Worktree JSON cases live in `tests/worktree_db_resolution.rs` (and new CLI tests that spawn `git worktree add`). The skill proves clap + pin + refuse in an isolated `--dir`.
3. Sharing `unique_tmp_path` by moving it into `prd_json.rs` and having `prd_reconcile` call it is in scope (loop_engine already depends on `commands`). Do not merge `update_prd_task_passes` into the add writer.
4. Switching `main.rs::get_project_root` onto `git::worktree_root` is optional, not required.

---

## 1. Overview

### Problem Statement

Docs and loop prompts already tell agents to pin a destination with `task-mgr add --stdin --from-json tasks/<prd>.json`. Clap does not accept that flag. Cheatsheet CI **forbids** the string. From a linked worktree, `add` still writes the path stored in `prd_files` (usually the main checkout copy). `locate_prd_json` with no pin falls back to the first `task_list` row, so a ≥2-prefix DB silently appends PRD #1.

Real failures already in learnings:

- **#4441**: worktree cwd, `TASK_MGR_DIR` → main `.task-mgr`, JSON sync “No such file or directory” on a bare filename.
- **#4237**: `add` wrote the main JSON; loop `prd_path` is the worktree copy; `update_prd_task_passes` then “Task in PRD not found”.
- **#2236**: omitted `--from-json` leaked `WIRE-FIX` / `CODE-FIX` into the wrong PRD JSON.

The documented pin must exist, default add from a worktree must write the worktree JSON when that file is present, and unpinned writes against ≥2 registered prefixes must fail closed.

### Background

`TASK_MGR_ACTIVE_PREFIX` (`PrefixMode` is not always set — `Disabled` / `--no-prefix` is a real mode). `task-mgr current` already prints `source=from-json` in its help, but the flag is not on the command. Loop startup already remaps `paths.prd_file` with `strip_prefix` + join and does **not** `exists()` on that remap. CLI add does not share that math. DB anchoring to the main checkout from a worktree is already correct (`db::path::resolve_db_dir` + `tests/worktree_db_resolution.rs`) and stays.

This is **PR-1 of three serial PRs**. PR-2 is `task-mgr update` + `humanReviewOutcome`. PR-3 is export scoping + remaining docs/prompt alignment. This PRD implements only the PR-1 slice.

---

## 2. Goals

### Primary Goals

- [ ] `git::worktree_root_at` + `git::worktree_root()` (mirroring `main_repo_root_at` / `main_repo_root`) + `remap_into_worktree` exist as pure path math. Loop startup Step 8.5 **keeps** canonicalize-`source_root` then calls the helper (no `exists()`, no basename search, no dest canonicalize inside the helper).
- [ ] `commands/context.rs` owns `ResolvedContext`, `ResolutionSource`, `resolve_context`, `resolve_active_prefix`, `locate_prd_json`, `load_known_prefixes`, and path identity. `invalid_state` command-name is a parameter, not `"add"`.
- [ ] `commands/prd_json.rs` is the JSON write chokepoint: unique tmp (pid + counter + nanos; **not** `.{name}.task-mgr-add.tmp`) + rename. `append_user_story` moves out of `add.rs`. `prd_json` must not import `add`.
- [ ] Clap `--from-json` on `add` and `current` in the **same PR** as the ≥2-prefix refuse and the cheatsheet recipe. Help text says **pin**, not import. `init --from-json` stays the import shim.
- [ ] Default add from a linked worktree writes the worktree JSON when that regular file exists; main JSON bytes unchanged. `--from-json PATH` always writes that PATH (never remapped away).
- [ ] ≥2 registered non-NULL prefixes, no env, no `--from-json`: `add` refuses (no DB row, no JSON write). `--no-prefix` / 0-prefix DBs still insert.
- [ ] JSON-sync failure copy names `task-mgr current` and retry `--from-json`, never `export`.
- [ ] User-facing proof: drive `.claude/skills/verify-task-mgr/SKILL.md` for add/current `--from-json` in an isolated sandbox. Compile/unit tests alone are not proof.

### Success Metrics

- Worktree file exists → that file gains the new `userStories[]` entry; `cmp` of main JSON is empty.
- Worktree file missing, main file exists → write main; do not invent a worktree path; do not basename-search.
- `task-mgr current --from-json <registered>` prints `source=from-json` and `target=` equal to the write path (canonical PATH).
- Unregistered / missing / directory `--from-json`: non-zero exit, `SELECT COUNT(*) FROM tasks` unchanged.
- Two prefixes, no env, no flag: `add --stdin` non-zero; no row.
- `--no-prefix` DB: existing add unit/integration tests stay green.
- Cheatsheet contains `add --from-json`; still forbids `set-status`, `recall --top-k`, `learnings show`.
- `verify-task-mgr` artifacts for the new feature file exist under `.claude/skills/verify-task-mgr/artifacts/<run-id>/`.

---

## 2.5. Quality Dimensions

> Pins 1–4, 11–16, 19–21 are law for this PR. Pins 5–10, 17–18 are cross-phase constraints: they appear here so `/prd-tasks` and later PRDs cannot contradict them. **This PRD’s stories must not implement them.**

### Correctness Requirements

Pins (verbatim from the ledger):

1. `--from-json` on add/update/current/export ships as “pin this already-registered effort”. `--depended-on-by` cannot pin a worktree-only file.
2. Unregistered `--from-json` path: Refuse (`loop init` first). Identity must treat relative `prd_files` + worktree remap as registered.
3. Ambiguous prefix (≥2 non-NULL, no env, no flag): Refuse the write. Zero prefixes / `--no-prefix` still allow DB insert — loop does *not* always set `TASK_MGR_ACTIVE_PREFIX` (`PrefixMode::Disabled`).
4. Worktree JSON: Pure remap, then CLI existence check. Loop remap stays unconditional. `--from-json PATH` always writes that PATH (never remapped away).
11. JSON sync is best-effort; DB commits first. Failure copy names `task-mgr current` and retry `--from-json`, never `export`.
12. One JSON write chokepoint: unique tmp + rename (reuse `prd_reconcile::unique_tmp_path` scheme: pid + counter + nanos). Preserve unknown keys on patch. Do not deserialize an existing story to `PrdUserStory` and write it back.
13. `--from-json` never registers a PRD and never remaps the write target. It only pins an already-registered effort.
14. Live remap is path math, not discovery. No `exists()`, no basename search. Relative `prd_files` rows are joined to `source_root` before remap.
15. Loop remap stays unconditional. CLI existence checks are caller-side and must not be shared into startup.
16. Refuse-without-pin applies iff ≥2 registered non-NULL prefixes. Zero-prefix / `--no-prefix` is a different mode: DB insert OK; JSON sync only if exactly one `task_list` is registered.
19. Path identity (canonicalize + `source_root.join` + worktree remap) is one function, used by add / update / current / export overwrite-guard.
20. Do not ship the multi-prefix refuse before clap has `--from-json` (docs already tell agents to pass it). PR-1 ships both together.
21. Out of scope: claim-scoped short `<task-status>` ids; a generic `set-status` command; `add --from-json` creating/registering a new PRD; rewriting historical `tasks/*-prompt.md`; changing DB anchoring (main checkout `.task-mgr` from a worktree stays); MCP task wrappers; putting `priority` on the update whitelist.

**PR-1-specific correctness:**

- `--from-json` means **pin destination PRD**. Help text must say “pin”, not “import”. `task-mgr init --from-json` stays the deprecated import shim.
- `FromJsonFlag` is no longer reserved: `resolve_context` precedence is flag → env → exactly one non-NULL prefix → `None`.
- Flag vs env mismatch: flag wins; stderr note; still proceed.
- `--from-json` is registered if **any** of: (a) JSON `taskPrefix` in `prd_metadata` (prefix OR, **not** pin 19); (b) or (c) the pin-19 identity function (canonicalize + `source_root.join` + remap). Else `invalid_state` “not a registered task_list”; name `loop init`.
- After DB commit, `append_user_story` is called on `ResolvedContext.prd_json_path` **only**. Delete the second `locate_prd_json` write. `--from-json` → that field is canonical PATH; default → remap then `is_file()` (worktree else registered else skip, and that result is what `prd_json_path` holds). `ctx is None` → JSON sync iff exactly one `task_list` row.
- `--from-json` of a NULL-prefix registered file returns `Some(ctx)` with empty `prefix`, `source=from-json`, write path set, and **does not** call `apply_prefix`.
- `current` `target=` is the **write path** (post-policy for default; canonical PATH for `--from-json`). When neither remapped nor registered path is a regular file, `target=(none)`.
- Cross-PRD `--depended-on-by` still refuses (existing `reject_cross_prd_depended_on_by`).
- Concurrent add vs `update_prd_task_passes` tmp names do not collide.
- ≥2-prefix refuse is **add-only**: `resolve_context` keeps `Ok(None)` for both 0 and 2+ prefixes; `add` errors iff `ctx.is_none() && load_known_prefixes().len() >= 2`.

**Cross-phase (do not implement in this PRD; do not contradict):**

5. `task-mgr update` is a real command with load-merge-write. Must not reuse `init::import::update_task` (full-row SET + clears `archived_at`).
6. Status via `update`: Hard-error `status` / `passes` (including a full story blob). Lifecycle SSoT; do not silently skip.
7. Unknown overlay keys: Hard-error. Silent drop is the original `humanReviewOutcome` bug.
8. Export default: Active-PRD only; `--all` restores today’s dump.
9. Overwrite a registered task-list: Always refuse without `--force`, even when scoped to that PRD. Export is a lossy dump, not a merge.
10. `tasks.status` is lifecycle-only. `update` / JSON patch never write it. `passes` in an overlay is a hard error, not an ignore.
17. `humanReviewOutcome` is not a DB column. Persistence is the task-list JSON; `PrdUserStory` must not strip it on import.
18. Export to a registered `task_list` is opt-in `--force`. `--force` is a dump, not a merge. Take the same `LockGuard` as add if the destination is a live PRD.

### Performance Requirements

- Best effort. Remap and identity are a handful of `Path` joins + at most a few `canonicalize` / `metadata` calls per invocation.
- Exit early on `--from-json` missing/directory/unregistered **before** opening a write transaction.
- Do not walk the worktree or search by basename.

### Style Requirements

- Follow existing codebase patterns. `TaskMgrError::invalid_state(command, field, expected, actual)`. `ui::emit` / `ui::emit_err` for product UX (CONTRACT-LOG-001). No `tracing` for operator-facing pin/refuse copy.
- No `.unwrap()` on filesystem or SQLite in the new modules unless a prior invariant makes it unreachable (existing `prefixes.len() == 1` next is fine).
- `prd_json` must not import `add`. `context` must not import `prd_json` write helpers (read/identity only). Loop startup must not import CLI existence policy.
- Comments explain **why** (unconditional remap vs caller-side exists; pin vs import). Do not narrate the move.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Main checkout cwd, registered path | Identity must be a no-op | Write registered path; bytes of any other copy untouched |
| Linked worktree, worktree file exists | **#4237** / **#4441** split-brain | Write worktree copy; main JSON bytes unchanged |
| Linked worktree, only main file exists | Inventing a path is data loss | Write main; no basename search; no create |
| Loop dest missing | Pin 15 | `remap_into_worktree` still returns worktree path; helper does not `canonicalize` dest; startup does not `exists()` |
| Symlink `source_root` vs canonical `prd_file` | Today’s Step 8.5 canonicalize; seed helper is infallible | Caller canonicalizes `source_root` then calls helper; `strip_prefix` hits; dest is not canonicalized inside the helper |
| Display remaps, write still `locate_prd_json` | **#4237** | After DB commit, append uses `ctx.prd_json_path` only; no second locate |
| `--from-json` NULL-prefix + `apply_prefix("")` | `prefix_id("", id)` → `-FEAT-001` | `Some(ctx)` with empty prefix; skip `apply_prefix`; inserted id is `FEAT-001` |
| Neither remapped nor registered is a file | Writers skip | `current` prints `target=(none)` |
| Unregistered / never-inited `--from-json` | Pin 2, 13 | Error naming `loop init`; no DB row |
| Missing file / directory `--from-json` | Flag is a pin of a file | Error; no DB row |
| Flag vs env mismatch | Pin 1 precedence | Flag wins; stderr note |
| Cross-PRD `--depended-on-by` | Existing refuse | Still refuses; no row |
| ≥2 prefixes, no pin | Today writes PRD #1 via `LIMIT 1` | Refuse the write |
| `--no-prefix` / 0-prefix DB | Pin 3, 16; `PrefixMode::Disabled` | add still inserts; JSON sync only if exactly one `task_list` |
| Relative `prd_files` (`tasks/foo.json`) + worktree `--from-json tasks/foo.json` | Pin 2 false refuse | Treat as registered |
| `current --from-json` registered vs unregistered | Probe vs pin | Registered: `target=` is write path, `source=from-json`. Unregistered: error |
| Concurrent add vs `update_prd_task_passes` | **#1562** fixed name collides | Distinct tmp (`pid-counter-nanos`) |
| Cheatsheet in the clap PR | Pin 20 | Contains `add --from-json`; still forbids `set-status`, `recall --top-k`, `learnings show` |
| `--depended-on-by` without pin on a worktree-only file, ≥2 prefixes | Pin 1 | Cannot pin; refuse (same as no-pin) |
| DB from worktree cwd | Existing contract | Row lands in main-repo `.task-mgr` |
| Stray copy whose `taskPrefix` matches `prd_metadata` | Match (a) is prefix OR, not path identity | Registered via (a); pin 4 writes **that** PATH. Pin 19 identity is (b)+(c) only |
| Identity test seeded with a bare basename | Init stores `tasks/foo.json` or absolute | Tests seed `prd_files` as init would; never a bare basename unless that is what init stored |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **CONTRACT-001** owner: `src/git/mod.rs` (`worktree_root_at`, `worktree_root`, `remap_into_worktree`) + path-identity in `src/commands/context.rs`. Pin 19 identity is **(b)+(c) only** (canonicalize + `source_root.join` + remap). Match (a) is a **separate prefix OR**, not that function. Consumers: loop startup Step 8.5 (caller canonicalizes `source_root`, then helper), CLI write policy, `--from-json` match (b)/(c), `current` `target=`, later PR-2 update / PR-3 export overwrite-guard.
- **CONTRACT-002** owner: `src/commands/context.rs` (`resolve_context` / `FromJsonFlag` pin protocol). `resolve_context` keeps `Ok(None)` for **both** 0 and 2+ prefixes (so `current` stays a probe). **`add` only**, when `ctx.is_none() && load_known_prefixes().len() >= 2`, returns `invalid_state` naming `--from-json` / `TASK_MGR_ACTIVE_PREFIX`. Zero prefixes still insert. Consumers: `add`, `current`, `main.rs` logging (`from_json: None`).
- **CONTRACT-003** owner: `src/commands/prd_json.rs` (unique tmp + rename + `append_user_story`). Consumers: `add` this PR; PR-2 `update` must call this chokepoint (do not implement update here). `prd_reconcile::update_prd_task_passes` keeps its own mutate but **must** share `unique_tmp_path`.

**Data Flow Contracts:**

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| `prd_files.file_path` | SQLite `TEXT` → `PathBuf` (relative **or** absolute; `register_prd_files` uses `strip_prefix(tasks_dir).unwrap_or(json_path)`) | `let registered = PathBuf::from(row); let resolved = if registered.is_absolute() { registered.clone() } else { source_root.join(&registered) };` |
| PRD JSON `taskPrefix` | `PrdFile.task_prefix: Option<String>` (`camelCase` `taskPrefix`) → `prd_metadata.task_prefix` | `let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?; let prefix = v.get("taskPrefix").and_then(|x| x.as_str());` — do **not** deserialize the whole file to `PrdUserStory` to read this |
| `--from-json PATH` | clap `Option<PathBuf>` → canonical `PathBuf` | `let canon = fs::canonicalize(path).map_err(...)?;` then registered if (a) **or** pin-19 identity (b)/(c). `ResolvedContext.prd_json_path = canon` (never remapped away) |
| Pin 19 identity | (b)+(c) only | `fn paths_identify(flag: &Path, registered: &Path, source_root: &Path, worktree_root: &Path) -> bool` — join relative `registered` to `source_root`, then `canonicalize(flag) == canonicalize(resolved)` **or** `canonicalize(flag) == remap_into_worktree(registered, source_root, worktree_root)`. Match (a) is **not** this function |
| Match (a) prefix OR | JSON `taskPrefix` string → `prd_metadata.task_prefix` | `prefixes.contains(&json_task_prefix)`. A stray copy with the same prefix is registered via (a); pin 4 then writes **that** PATH |
| Remap | `Path` math only | See seed in §6. Caller may canonicalize `source_root` first (startup does). Helper does **not** `canonicalize` dest. `strip_prefix(source_root)` miss → return `resolved` unchanged |
| CLI write target | remap result then `is_file()` **caller-side**; stored on `ctx.prd_json_path` | `let target = remap_into_worktree(...); if target.is_file() { target } else if resolved.is_file() { resolved } else { skip / PathBuf::new() }`. After DB commit: `append_user_story(&ctx.prd_json_path, …)` only — **no** second `locate_prd_json` |
| `ResolvedContext` | struct fields | `ctx.prefix` (`String`; empty when the matched `prd_metadata.task_prefix` is NULL); `ctx.source` (`ResolutionSource::FromJsonFlag` displays `from-json`); `ctx.prd_json_path` = write path. Empty prefix ⇒ skip `apply_prefix` |
| `TASK_MGR_ACTIVE_PREFIX` | env `String` → `prd_metadata.task_prefix` exact match | Existing stale-pin error; command name parameterized |
| JSON `userStories` | `serde_json::Value` object; array under string key `"userStories"` | `root_obj["userStories"].as_array_mut()`; push a `to_value(new_story)` object. **Do not** map existing entries through `PrdUserStory` (strips unknown keys; pin 12 / 17) |
| Tmp name | `.{basename}.{pid}-{n}-{nanos}.tmp` next to target | Shared `unique_tmp_path`; same-directory rename (learning **#2667**) |

### Modularity & Coupling Targets

- **Target public surface**: `git::worktree_root_at`, `git::worktree_root()`, `git::remap_into_worktree`; `context::{ResolvedContext, ResolutionSource, resolve_context, path identity (b)+(c)}`; `prd_json::{append_user_story, unique_tmp_path}`; clap `--from-json` on `Add` and `Current`. No new DB columns. No new subcommand.
- **Ownership**: remap math in `git`; pin protocol + identity in `context`; JSON bytes in `prd_json`; `add` / `current` are callers; startup calls remap only.
- **Coupling budget**: startup **must not** call CLI `exists()` policy. `prd_json` **must not** import `add`. `--from-json` **must not** call `init` / `register_prd_files`.
- **Cohesion**: types that move out of `add.rs` live in `context.rs` (resolution) or `prd_json.rs` (file bytes). Do not leave `FromJsonFlag` in `add.rs`.

### When to Emit a CONTRACT-xxx Task

- **`CONTRACT-001`** — `remap_into_worktree` + pin-19 path identity **(b)+(c)** (canonicalize + `source_root.join` + remap). Match (a) is a separate prefix OR. Identity tests seed `prd_files` as init would (`tasks/foo.json` or absolute), never a bare basename unless that is what init stored. Priority 0–1, `taskType: "contract"`. Downstream: US-001, US-004, US-005; later update/export.
- **`CONTRACT-002`** — `resolve_context(conn, from_json: Option<&Path>, command: &str)` pin protocol + parameterized `invalid_state`. Keeps `Ok(None)` for 0 **and** 2+ prefixes. **`add` only**: `ctx.is_none() && load_known_prefixes().len() >= 2` → `invalid_state` naming `--from-json` / `TASK_MGR_ACTIVE_PREFIX`. Downstream: US-002, US-004, US-006.
- **`CONTRACT-003`** — `prd_json` unique-tmp write chokepoint + `append_user_story`. Downstream: US-003, US-004 (add); PR-2 update.

---

## 3. User Stories

### US-001: Unconditional worktree remapper (CONTRACT-001)

**As a** loop engine
**I want** live PRD path remap to be pure path math shared with CLI writers
**So that** startup still remaps when the dest is missing, and CLI cannot drag `exists()` into startup

**Acceptance Criteria:**

- [ ] Export `git::worktree_root_at(dir)` + `git::worktree_root()` to match `main_repo_root_at` / `main_repo_root` (`git rev-parse --show-toplevel`, canonicalized like `main_repo_root_at`)
- [ ] `remap_into_worktree(registered, source_root, worktree_root) -> PathBuf` implements the seed in §6 exactly (join relative to `source_root` first; `strip_prefix` miss returns `resolved`). Stays **pure**: no `exists()`, no basename search, no directory walk, **no `canonicalize` of dest**
- [ ] `startup.rs` Step 8.5 **keeps** canonicalize-`source_root` (today’s comment at ~665–671: `paths.prd_file` is already canonical; `source_root` may be a symlink) **then** calls the helper. Do not drop that canonicalize. Step 8.4 copy-if-missing stays a separate copy policy
- [ ] Unit: dest path need not exist; return value is still `worktree_root.join(rel)` (helper does not canonicalize dest)
- [ ] Unit: main checkout (`worktree_root == source_root`) returns the joined registered path unchanged
- [ ] Unit: symlink `source_root` vs canonical `prd_file` still remaps (`strip_prefix` hits after the caller canonicalize)
- [ ] Existing worktree re-import `exists()` gate after remap is unchanged (that is not remap)

**edgeCases:** main cwd unchanged; dest missing still remaps; relative registered joined before remap; symlink `source_root`

---

### US-002: Extract `commands/context.rs` (CONTRACT-002)

**As a** CLI maintainer
**I want** active-PRD resolution out of `add.rs`
**So that** `current`, `add`, and later update/export share one pin protocol whose errors name the calling command

**Acceptance Criteria:**

- [ ] New `src/commands/context.rs` owns `ResolvedContext`, `ResolutionSource`, `resolve_context`, `resolve_active_prefix`, `locate_prd_json`, `load_known_prefixes`, pin-19 identity (b)+(c)
- [ ] `add.rs` and `current.rs` import from `context` (re-export from `commands/mod.rs` so existing `commands::add::resolve_context` call sites in `main.rs` logging and tests compile — either keep a thin re-export or update call sites in this PR)
- [ ] `invalid_state` command-name is a `&str` parameter. Stale-pin from `current` must not say `"add"`
- [ ] `FromJsonFlag` is wired (no longer “reserved for future use”)
- [ ] `resolve_context` keeps `Ok(None)` for **both** 0 and 2+ prefixes (probe). It does **not** refuse ≥2 prefixes
- [ ] `claude.rs` comment that points at `add.rs` `resolve_active_prefix` is updated
- [ ] Existing `current` unit tests (env / single-prefix / zero / two prefixes → None) still pass after the move

**edgeCases:** stale env pin names the calling command; logging `resolve_context(&conn)` uses `from_json: None`; 2+ prefixes still `Ok(None)` so `current` exits 0

---

### US-003: `prd_json.rs` write chokepoint (CONTRACT-003)

**As an** add caller
**I want** one atomic JSON writer with unique tmp names
**So that** concurrent add and `update_prd_task_passes` cannot clobber each other, and PR-2 can patch without a second writer

**Acceptance Criteria:**

- [ ] New `src/commands/prd_json.rs`. `append_user_story` (today `append_task_to_prd_json`) lives here. `prd_json` does **not** import `add`
- [ ] Tmp scheme matches `prd_reconcile::unique_tmp_path`: `.{base}.{pid}-{n}-{nanos}.tmp`. Do **not** keep `.{name}.task-mgr-add.tmp`
- [ ] Shared helper: move `unique_tmp_path` here; `prd_reconcile` calls it (same `AtomicU64` so the collision test is structural)
- [ ] Append preserves unknown keys on **existing** stories (Value round-trip of the file; only the **new** story is `to_value`’d from `PrdUserStory`)
- [ ] Existing append unit tests move with the function and stay green (duplicate id, reverse `--depended-on-by`, prefix strip)
- [ ] Test: concurrent add vs `update_prd_task_passes` tmp names do not collide

**edgeCases:** leftover tmp after crash is identifiable; trailing newline preserved (today’s behavior)

---

### US-004: `add --from-json` pin + path identity (CONTRACT-001, CONTRACT-002)

**As a** loop agent
**I want** `task-mgr add --stdin --from-json tasks/<prd>.json` to pin an already-registered effort
**So that** spawned fixups land in that PRD’s JSON, including a worktree copy of a relative `prd_files` row

**Acceptance Criteria:**

- [ ] Clap `--from-json <PATH>` on `Commands::Add`. Help: **pin** this already-registered effort, not import. Distinct from `init --from-json`
- [ ] `FromJsonFlag` wired through `add` → `resolve_context(conn, Some(path), "add")`
- [ ] Happy: pin file, auto-prefix (non-empty prefix only), append `userStories`, reverse `--depended-on-by`
- [ ] After DB commit, `append_user_story` is called on `ResolvedContext.prd_json_path` **only**. Delete the second `locate_prd_json` write
- [ ] `--from-json` → `ctx.prd_json_path` is canonical PATH (never remapped away). Default (no flag) → remap then `is_file()` (worktree else registered else skip); that result is `ctx.prd_json_path`
- [ ] `ctx is None` → JSON sync iff exactly one `task_list` row (else skip; DB already committed)
- [ ] Unregistered / never-inited file → error naming `loop init`; no DB row
- [ ] Missing file / directory → error; no DB row
- [ ] Flag vs env mismatch → flag wins; stderr note
- [ ] Cross-PRD `--depended-on-by` still refuses
- [ ] Relative `prd_files` row `tasks/foo.json` + worktree `--from-json tasks/foo.json` registers (must not refuse) — pin-19 identity (b)/(c) and/or match (a). Identity tests seed `prd_files` as init would (`tasks/foo.json` or absolute), never a bare basename unless that is what init stored
- [ ] `--from-json` of a NULL-prefix / `--no-prefix` registered file returns `Some(ctx)` with empty `prefix`, `source=from-json`, write path set — and **does not** call `apply_prefix`. Test: inserted id is unprefixed (`FEAT-001`, not `-FEAT-001`)
- [ ] `--from-json` does not insert `prd_files` / `prd_metadata` rows
- [ ] `--depended-on-by` alone still cannot pin a worktree-only file when ≥2 prefixes (US-006)
- [ ] Clap parse test in `src/cli/tests.rs`; `main.rs` dispatch passes the path

**edgeCases:** unregistered; missing; directory; relative+worktree identity; env mismatch; no registration side effect; NULL-prefix pin skips `apply_prefix`; display path == write path (`ctx.prd_json_path`)

---

### US-005: CLI write policy + `current --from-json` write path (CONTRACT-001)

**As an** operator
**I want** `task-mgr current` (and its `--from-json`) to print the path `add` would write
**So that** I can see the worktree copy before piping a fixup

**Acceptance Criteria:**

- [ ] Clap `--from-json` on `Commands::Current`. Same pin semantics as add. Help says pin, not import
- [ ] Update `Commands::Current` rustdoc (not only `after_help`): exit 0 for no-flag probe; non-zero for unregistered / missing / directory `--from-json`. Delete “Exits 0 in all cases”
- [ ] Default (no flag): `target=` is the CLI write path (remap, then existence policy). Loop remap remains unconditional and is **not** used here
- [ ] When neither remapped nor registered path is a regular file, `current` prints `target=(none)` (writers skip; probe must not invent a path)
- [ ] `--from-json` registered: `source=from-json`, `target=` = canonical PATH (the write path)
- [ ] `--from-json` unregistered: error (not exit 0); copy names `loop init`
- [ ] Live path tests (worktree harness):
  - Main checkout cwd → registered path unchanged
  - Linked worktree, worktree file exists → write worktree copy; main JSON bytes unchanged
  - Linked worktree, only main file exists → write main (no invent, no basename search)
  - DB row still lands in main-repo `.task-mgr` (existing `worktree_db_resolution` tests stay green)
- [ ] `current` without pin and ≥2 prefixes still exits 0 with `no active PRD` (probe, not refuse) — `resolve_context` returns `Ok(None)`

**edgeCases:** worktree file exists vs only main exists; current unregistered error vs ambiguous probe; neither copy exists → `(none)`

---

### US-006: ≥2-prefix refuse; 0-prefix unchanged (CONTRACT-002)

**As a** multi-PRD operator
**I want** unpinned `add` to refuse when two prefixes are registered
**So that** fixups cannot leak into PRD #1 via `locate_prd_json` `LIMIT 1`

**Acceptance Criteria:**

- [ ] `resolve_context` keeps `Ok(None)` for both 0 and 2+ prefixes (so `current` stays a probe). Do **not** error inside `resolve_context` for 2+ prefixes. Do **not** `if ctx.is_none() { refuse }` (that breaks `--no-prefix`)
- [ ] **`add` only**, when `ctx.is_none() && load_known_prefixes().len() >= 2`, returns `invalid_state` naming `--from-json` / `TASK_MGR_ACTIVE_PREFIX`; **no** DB row; **no** JSON write
- [ ] Zero prefixes / `--no-prefix` DB: `ctx is None` and `load_known_prefixes().len() == 0` → add still inserts (existing unit tests stay green). JSON sync iff exactly one `task_list` row
- [ ] `--from-json` of a NULL-prefix registered file is `Some(ctx)` (empty prefix) — not this refuse path — and does not call `apply_prefix` (US-004 test: `FEAT-001`, not `-FEAT-001`)
- [ ] Exactly one non-NULL prefix: existing auto-prefix + JSON sync via `ctx.prd_json_path` (remap + existence) unchanged
- [ ] Env pin still selects among ≥2 prefixes
- [ ] This refuse ships in the **same PR** as clap `--from-json` (pin 20)

**edgeCases:** 0-prefix + two `task_list` rows → DB insert, skip JSON; 0-prefix + one `task_list` → sync that file; 2+ prefixes + `--from-json` pin proceeds; `current` on 2+ prefixes still exit 0

---

### US-007: JSON-sync failure copy drops `export`

**As an** agent reading stderr after a best-effort sync miss
**I want** the warning to name `task-mgr current` and retry `--from-json`
**So that** I do not run `export` (lossy dump; PR-3 will also require `--force` onto a registered file)

**Acceptance Criteria:**

- [ ] `add.rs` comment and `ui::emit_err` warning no longer mention `task-mgr export`
- [ ] Copy names `task-mgr current` and retry with `--from-json`
- [ ] DB commit still happens first (learning **#3440**); failure does not roll back
- [ ] Test asserts the warning substrings and the absence of `export`

**edgeCases:** `ctx is None` skip note vs sync `Err` on `ctx.prd_json_path` — both drop `export`; neither names `locate_prd_json` as the write site

---

### US-008: Cheatsheet recipe in the clap PR

**As an** agent running `task-mgr cheatsheet`
**I want** `add --from-json` in Common Recipes
**So that** CI stops forbidding the flag the docs already teach

**Acceptance Criteria:**

- [ ] Delete forbidden anchor `add --from-json` from `cheatsheet.rs` unit test **and** `tests/cheatsheet_drift.rs`
- [ ] Add `add --from-json` to `CURATED_RECIPES` (keep `--stdin --depended-on-by`; recipes stay ≤30 lines)
- [ ] Still forbids `set-status`, `recall --top-k`, `learnings show`
- [ ] Same PR as clap `--from-json` (pin 20)

**edgeCases:** drift extractor still parses the new recipe token; `set-status` synthetic test unchanged

---

### US-009: verify-task-mgr sandbox proof (user-facing)

**As a** reviewer
**I want** an isolated-sandbox drive of add/current `--from-json`
**So that** green unit tests cannot ship a clap-less binary

**Acceptance Criteria:**

- [ ] New feature file `.claude/skills/verify-task-mgr/features/add-and-current-from-json.md` following `features/README.md` (Sub-features, How to get to it, Driving it, Gotchas)
- [ ] Drive via `.claude/skills/verify-task-mgr/SKILL.md`: `launch`, `sandbox-new`, `doctor`, `capture` / `cli`, `snapshot-db`, `cleanup`
- [ ] Proof (artifacts kept): happy pin + append; unregistered refuse (no row); missing/directory refuse; ≥2-prefix unpinned refuse; `--no-prefix` add still inserts; `current --from-json` registered `target=`; `current --from-json` unregistered error; `add --help` / `current --help` say **pin**
- [ ] Worktree live-path cases are **not** claimed via this harness; they are US-005 rust tests
- [ ] Compile/unit tests alone do not satisfy this story

**edgeCases:** helper unsets `TASK_MGR_ACTIVE_PREFIX`; use `--dir` injection only

---

## 4. Functional Requirements

### FR-001: Pure remapper

`remap_into_worktree` is path math (no `exists()`, no dest `canonicalize`). Loop startup Step 8.5 **keeps** canonicalize-`source_root` then calls the helper. CLI existence is a different function in `context` (or `add`/`current` callers), never called from `startup.rs`.

**Validation:** unit tests on the helper including dest-missing and symlink-`source_root`; grep that Step 8.5 has no `exists()` and still canonicalizes `source_root` before the helper.

### FR-002: Pin protocol

`resolve_context(conn, from_json: Option<&Path>, command: &str)` precedence:

1. `--from-json PATH` (regular file, parse as PRD JSON). Registered if **(a)** JSON `taskPrefix` in `prd_metadata` **or** pin-19 identity **(b)/(c)**. Else `invalid_state` not a registered `task_list`. `source = FromJsonFlag`. `prd_json_path = canonical PATH`. Env mismatch → stderr note, flag wins. NULL `task_prefix` on the matched row → `prefix = ""` (skip `apply_prefix` at the add caller).
2. `TASK_MGR_ACTIVE_PREFIX` (existing stale-pin error, command-parameterized)
3. exactly one non-NULL `prd_metadata.task_prefix`
4. `None` — for **both** 0 and 2+ non-NULL prefixes (probe). `resolve_context` does not refuse.

`--from-json` never registers and never remaps the write target.

**Add-only refuse (not in `resolve_context`):** `ctx.is_none() && load_known_prefixes().len() >= 2` → `invalid_state` naming `--from-json` / `TASK_MGR_ACTIVE_PREFIX`. Zero prefixes still insert.

**Validation:** US-004 / US-005 / US-006 tests.

### FR-003: CLI write policy + single write path

Default writers: remap into cwd toplevel vs main root, then regular-file check (worktree copy, else registered, else skip). `--from-json PATH`: canonical PATH if regular file, else error before DB write. The chosen path is stored on `ResolvedContext.prd_json_path` (empty/`PathBuf::new()` when skipping).

After DB commit, `append_user_story` is called on `ResolvedContext.prd_json_path` **only**. Delete the second `locate_prd_json` write. `ctx is None` → JSON sync iff exactly one `task_list` row.

`current` `target=` is that same write path; when neither remapped nor registered is a regular file, `target=(none)`.

**Validation:** US-004 write-path tests; US-005 live-path matrix including `(none)`.

### FR-004: JSON chokepoint + failure copy

All add JSON writes go through `prd_json`. Unique tmp. Failure copy: `task-mgr current` + retry `--from-json`, never `export`. DB first.

**Validation:** US-003 / US-007.

### FR-005: Clap + cheatsheet together

`--from-json` on add and current; cheatsheet recipe; ≥2-prefix refuse. One PR.

**Validation:** `src/cli/tests.rs` parse; cheatsheet_drift; US-006.

### FR-006: User-facing verify

Drive the verify-task-mgr skill for add/current `--from-json`.

**Validation:** US-009 artifacts.

---

## 5. Non-Goals (Out of Scope)

- PR-2: `task-mgr update` command, `humanReviewOutcome` field, `error_recovery` hint swap, CLARIFY docs — Reason: next serial PRD
- PR-3: export scoping, `task_ops` prompt, enhance/intents remaining alignment, best-practices copy — Reason: after PR-2
- Pins 5–10, 17–18 implementation — Reason: those stories belong to PR-2 / PR-3; listed in §2.5 so they are not contradicted
- Claim-scoped short `<task-status>` ids — Reason: pin 21
- A generic `set-status` command — Reason: pin 21; cheatsheet still forbids it
- `add --from-json` creating/registering a new PRD — Reason: pin 13 / 21
- Rewriting historical `tasks/*-prompt.md` — Reason: pin 21
- Changing DB anchoring (main checkout `.task-mgr` from a worktree stays) — Reason: pin 21; `worktree_db_resolution` stays
- MCP task wrappers — Reason: pin 21
- Putting `priority` on the update whitelist — Reason: pin 21 / PR-2
- Sharing CLI `exists()` into loop startup — Reason: pin 15
- Basename search / `exists()` inside `remap_into_worktree` — Reason: pin 14
- Shipping ≥2-prefix refuse without clap `--from-json` — Reason: pin 20

---

## 5.5. Low-Value / High-Effort Areas (Explicit Cuts or Deferrals)

| Area / Capability | Why the value is low relative to cost | Rough effort cost | Recommended action |
| --- | --- | --- | --- |
| Auto-register `--from-json` of a new file | Pin 13 forbids it; `loop init` already imports | High (init semantics) | **Cut** |
| Basename search when remap misses | Silent wrong-file writes (**#2236**) | Medium | **Cut** |
| `current` refusing ≥2 prefixes without a flag | Breaks the documented probe (`exit 0`); refuse is write-only | Low but wrong | **Cut** |
| Merging `update_prd_task_passes` into `prd_json` | Different mutate (passes flip vs append); share tmp helper only | High | Defer forever unless a later PRD asks |
| Switching `get_project_root` onto `worktree_root` | Duplicate `show-toplevel`; not user-facing | Low | Optional; not a story |
| verify-task-mgr git worktree recipe | Harness forbids `loop run` and is `--dir` isolated; rust tests already spawn worktrees | High | Defer; US-005 owns live path |

---

## 6. Technical Considerations

### Affected Components

- `src/git/mod.rs` — add `worktree_root_at` / `worktree_root` / `remap_into_worktree`
- `src/loop_engine/startup.rs` — Step 8.5 keeps canonicalize-`source_root`, then calls remapper
- `src/commands/context.rs` — **new**; resolution + identity
- `src/commands/prd_json.rs` — **new**; write chokepoint
- `src/commands/add.rs` — drop extracted types/funcs; take `--from-json`; refuse ≥2; new failure copy
- `src/commands/current.rs` — `--from-json`; write path
- `src/cli/commands.rs` — clap flags + pin help
- `src/cli/tests.rs` — parse tests
- `src/main.rs` — dispatch; logging `resolve_context` signature
- `src/commands/mod.rs` — modules + re-exports
- `src/commands/cheatsheet.rs` — recipe + forbidden list
- `src/loop_engine/prd_reconcile.rs` — call shared `unique_tmp_path`
- `src/loop_engine/claude.rs` — comment path
- `tests/worktree_db_resolution.rs` — live JSON write-path cases (keep DB-anchoring tests)
- `tests/cheatsheet_drift.rs` — drop `add --from-json` from forbidden
- `tests/add_integration.rs` / new CLI tests — pin matrix
- `.claude/skills/verify-task-mgr/features/add-and-current-from-json.md` — **new**
- `docs/ARCHITECTURE.md:464` — mention `--from-json` pin + worktree write path (one sentence; remaining docs are PR-3)

### Dependencies

- Internal: `git` CLI (`rev-parse --show-toplevel`, existing worktree tests), rusqlite `prd_metadata` / `prd_files`, clap derive, `LockGuard` (add already takes it)
- No new crates
- `verify-task-mgr` helper already in-tree

### Approaches & Tradeoffs

No `/spike` on this slice. Pins already chose the remapper vs discovery split.

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| **A. Pure remap + caller-side exists** (seed) | Loop dest-missing still remaps; CLI cannot invent files; one identity function for later PRs | Two call shapes to keep straight | **Preferred** |
| **B. One “resolve live JSON” helper with `exists()` inside** | Fewer functions | Violates pin 15; startup would stop remapping a missing dest | **Rejected** |
| **C. Basename / discovery search** | Might find a stray copy | Wrong-file writes; pin 14 forbids | **Rejected** |

**Selected Approach**: A. Extract remapper in `git`; identity in `context`; existence only in CLI writers. `--from-json` is pin-by-identity, never import.

**Phase 2 Foundation Check**: Path identity + `prd_json` chokepoint cost ~1 day now and are the only safe substrate for PR-2 `update` and PR-3 export `--force`. Approach B/C would force a rewrite of those PRs. “Approach A costs a small extract now but avoids split-brain and a second JSON writer later.”

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| ≥2-prefix refuse lands without clap `--from-json` | Agents following docs fail closed with no pin flag (pin 20) | Med (split PRs) | **Same PR** as clap + cheatsheet. Empirical: `cheatsheet_drift` flips forbidden→required in that commit; `cli/tests.rs` parses `add --from-json` |
| `exists()` leaks into startup remap | Loop stops remapping missing dest; worktree re-import never sees new files | Med | CONTRACT-001: helper has no fs probe. Empirical: unit “dest missing still remaps”; review grep `exists` in Step 8.5 |
| Relative `prd_files` + worktree `--from-json` treated as unregistered | False refuse of a live effort (pin 2) | High if identity is canonicalize-only | Pin-19 identity (b)+(c); join relative to `source_root` before remap; tests seed init-shaped rows. Empirical: named US-004 test |
| Display remaps, write still `locate_prd_json` (**#4237**) | Agent sees worktree `target=`; JSON lands on main | High if append site is left | After DB commit, append `ctx.prd_json_path` only. Empirical: US-004 AC |
| `apply_prefix("")` on NULL-prefix pin | Inserted id `-FEAT-001` | High if `Some(ctx)` always prefixes | Skip `apply_prefix` when prefix is empty. Empirical: US-004 `FEAT-001` test |
| Symlink `source_root` remap miss | Loop remap silently stays on main | Med if caller drops canonicalize | Step 8.5 keeps canonicalize-`source_root` then helper; helper does not canonicalize dest. Empirical: US-001 symlink unit |
| `PrdUserStory` round-trip on append strips unknown keys | Blocks PR-2 `humanReviewOutcome` (pin 12 / 17) | Med | CONTRACT-003: Value round-trip of the **file**; only the new story is typed. Empirical: append a file that already has an extra key; key survives |

Top 3 = clap/refuse same PR, relative identity, and the **#4237** write-path split. All have empirical ACs after the AA fold. No High×High unmitigated blocker.

### Security Considerations

- `--from-json` path is trusted CLI input (same class as `init --from-json`; see `init/mod.rs` trusted vs untrusted). Do not follow it into registration.
- Do not write outside the canonical PATH / remapped registered path. No invent.
- `LockGuard` remains on add (existing).
- Tmp files stay in the same directory as the target (rename atomicity; no `/tmp` cross-filesystem).

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `git::worktree_root_at` | `fn worktree_root_at(dir: &Path) -> Option<PathBuf>` | canonical `--show-toplevel` | `None` if not a repo | none (git CLI read) |
| `git::worktree_root` | `fn worktree_root() -> Option<PathBuf>` | `worktree_root_at(&cwd)` | `None` if cwd unreadable / not a repo | none (git CLI read) |
| `git::remap_into_worktree` | `fn remap_into_worktree(registered: &Path, source_root: &Path, worktree_root: &Path) -> PathBuf` | remapped or original `resolved` | infallible | **none** (no fs; no dest canonicalize) |
| `context::resolve_context` | `fn resolve_context(conn: &Connection, from_json: Option<&Path>, command: &str) -> TaskMgrResult<Option<ResolvedContext>>` | `Some` pin / `None` for **both** 0 and 2+ prefixes | stale env; unregistered/missing `--from-json` | stderr note on flag/env mismatch; **no DB write**; **no** ≥2 refuse |
| `context` pin-19 identity | (b)+(c) only: canonicalize + `source_root.join` + remap | `bool` / matched `prd_files` row | io on canonicalize of the flag path | none |
| `prd_json::append_user_story` | move of today’s `append_task_to_prd_json` | `()` | invalid JSON / dup id | tmp + rename |
| `prd_json::unique_tmp_path` | `fn unique_tmp_path(prd_path: &Path) -> PathBuf` | distinct path per call | infallible | counter increment |
| clap `Add.from_json` | `Option<PathBuf>` `--from-json` | parsed path | clap missing-arg N/A (optional) | none |
| clap `Current.from_json` | `Option<PathBuf>` `--from-json` | parsed path | same | none |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `add::add` | `add(db_dir, input_json, priority, depended_on_by)` | + `from_json: Option<&Path>` | Yes, rust API | Update `main.rs` + tests |
| `current::current` | `current(db_dir)` | + `from_json: Option<&Path>` | Yes, rust API | Update `main.rs` + tests |
| `add::resolve_context` | `(conn)` | moved; `(conn, from_json, command)` | Yes, rust API | Re-export or fix call sites (`main.rs:76`, `current.rs`) |
| `Commands::Add` | no `--from-json` | `--from-json <PATH>` | Non-breaking CLI add | New optional flag |
| `Commands::Current` | unit variant | `{ from_json: Option<PathBuf> }` | Non-breaking CLI | New optional flag |
| `add` ≥2 prefixes no pin | inserts + may write PRD #1 | `invalid_state` refuse | **Yes, CLI** | See Breaking |
| cheatsheet forbidden list | includes `add --from-json` | excludes it; recipe contains it | Test-only | Same PR as clap |

### Data Flow Contracts

See §2.6 table (copy-pasteable). Type transition to flag: `prd_files.file_path` TEXT may be relative (`tasks/foo.json`) or absolute. Pin-19 identity **must** join relative rows to `source_root` before canonicalize/remap. Forgetting that join is the silent false-refuse. Identity tests seed production-shaped rows (`tasks/foo.json` or absolute), never a bare basename unless that is what init stored.

`--from-json` match (a) is a **separate prefix OR** (JSON `taskPrefix` in `prd_metadata`), not pin 19. It reads `taskPrefix` off `serde_json::Value`; do not deserialize **stories**. A stray copy with the same prefix is registered via (a); pin 4 then writes that PATH.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `src/commands/add.rs:345-377` | second `locate_prd_json` after insert (display uses `ctx.prd_json_path`) | BREAKS **#4237** if left | US-004: append `ctx.prd_json_path` only; delete this locate |
| `src/commands/add.rs:262-277` | `if let Some(ref ctx) = resolved_ctx { apply_prefix(&ctx.prefix) }` | BREAKS NULL-prefix pin (`-FEAT-001`) | skip `apply_prefix` when `ctx.prefix` is empty |
| `src/commands/add.rs` ≥2 prefixes no pin | insert + `LIMIT 1` JSON | BREAKS (intended refuse) | US-006 add-only: `ctx.is_none() && load_known_prefixes().len() >= 2` |
| `src/commands/add.rs:342-360` | failure copy names `export` | BREAKS copy | US-007 |
| `src/commands/current.rs:27-48` | `resolve_context(&conn)`; `target=` raw DB path | NEEDS REVIEW | write path; `--from-json` error on unregistered |
| `src/main.rs:76` | logging prefix | OK if `from_json: None` | keep best-effort `None` |
| `src/main.rs:822-906` | dispatch | NEEDS REVIEW | pass new flag |
| `src/loop_engine/startup.rs:663-705` | inline remap with canonicalize-`source_root` | OK if canonicalize is kept | keep canonicalize-`source_root`, then helper; do not canonicalize dest inside helper |
| `tests/worktree_db_resolution.rs:159+` | DB anchoring | OK | do not change assertions |
| `tests/cheatsheet_drift.rs:376-393` | forbids `add --from-json` | BREAKS until updated | US-008 same PR |
| `src/commands/add.rs` unit tests for 2+ prefixes → `resolve_active_prefix` None | still None at resolver; **add** now errors | NEEDS REVIEW | split resolver vs write-policy tests |
| Loop agents with `TASK_MGR_ACTIVE_PREFIX` set | env pin | OK | unchanged |
| `add --stdin --depended-on-by` single-prefix / in-loop | live JSON path | OK if US-005 remap+exists | documented non-breaking |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| `init --from-json` | import shim | registers PRD, imports tasks | **unchanged** (import) |
| `add/current --from-json` | pin | flag does not exist (help lies) | pin only; never register |
| Loop Step 8.5 remap | startup | canonicalize `source_root` then strip_prefix | **keep** canonicalize, then helper; helper does not canonicalize dest |
| CLI write policy | add/current | raw `prd_files` path via second `locate_prd_json` | remap then exists; result on `ctx.prd_json_path`; append that path only; not used by startup |
| `resolve_context` None | 0 or 2+ prefixes | add still inserts | still `Ok(None)` for both; **add** refuses iff `len() >= 2`; 0-prefix inserts |
| `apply_prefix` on `Some(ctx)` | any `Some` | always prefixes | skip when `ctx.prefix` is empty (NULL-prefix `--from-json`) |
| `current` None | ambiguous / empty | exit 0 probe | **unchanged** without `--from-json` |
| `current --from-json` unregistered | new | n/a | error (not probe); rustdoc must not say “Exits 0 in all cases” |
| `current` neither copy exists | new | n/a | `target=(none)` |
| `locate_prd_json` LIMIT 1 | no prefix | first `task_list` | only `ctx is None` **and** exactly one `task_list` |

### Inversion Checklist

- [x] Callers of `resolve_context` / `add` / `current` / startup remap identified
- [x] `locate_prd_json` LIMIT 1 fallback is the ≥2-prefix footgun — refuse, do not keep it for that case
- [x] Second `locate_prd_json` after insert is the **#4237** write-path split — append `ctx.prd_json_path` only
- [x] `apply_prefix("")` on NULL-prefix pin — skip when prefix is empty; test `FEAT-001`
- [x] ≥2 refuse is add-only (`ctx.is_none() && len() >= 2`); not inside `resolve_context`; not on every `None`
- [x] Tests that forbid `add --from-json` must flip in the clap PR
- [x] Tests that allow add with 2+ prefixes and no pin must now expect error
- [x] `--no-prefix` tests must **not** be updated to expect refuse
- [x] Loop remap vs CLI exists are different semantic contexts
- [x] Startup keeps canonicalize-`source_root`; helper does not canonicalize dest
- [x] Pin 19 identity is (b)+(c); match (a) is a separate prefix OR

### Architecture seed (do not re-expand)

Pure remapper (helper does **not** `canonicalize` dest; dest missing still remaps):

```
remap_into_worktree(registered, source_root, worktree_root) -> PathBuf
  resolved = if registered.is_absolute() { registered }
             else { source_root.join(registered) }
  if let Ok(rel) = resolved.strip_prefix(source_root)
    -> worktree_root.join(rel)
  else -> resolved
```

Startup Step 8.5 **keeps** canonicalize-`source_root` then calls the helper (same as today’s comment: `paths.prd_file` is already canonical; `source_root` may be a symlink).

CLI writers:

```
target = remap_into_worktree(registered, main_root, cwd_toplevel)
if target is a regular file -> write target
else if registered (joined) is a regular file -> write that
else warn + skip JSON sync
```

That `target` is stored on `ResolvedContext.prd_json_path`. After DB commit, `append_user_story` uses **that path only** (no second `locate_prd_json`). `ctx is None` → JSON sync iff exactly one `task_list`.

`--from-json PATH` skips remap: write canonical PATH if regular file, else error.

`resolve_context(conn, from_json: Option<&Path>)` precedence: flag (regular file + (a) prefix OR **or** pin-19 (b)/(c); else not a registered `task_list`; `source = FromJsonFlag`; `prd_json_path = canonical PATH`; env mismatch → stderr note, flag wins; NULL prefix → empty `prefix`, skip `apply_prefix`) → `TASK_MGR_ACTIVE_PREFIX` (existing stale-pin) → exactly one non-NULL `prd_metadata.task_prefix` → `None` (0 **and** 2+ prefixes). Add-only refuse: `ctx.is_none() && load_known_prefixes().len() >= 2`.

`--from-json` means pin destination PRD. `init --from-json` stays the import shim. Help text must say “pin”, not “import”.

### Breaking

- **`add` with ≥2 prefixes and no env / no `--from-json` starts failing** instead of writing PRD #1. `--no-prefix` / 0-prefix DBs are **not** this case.
- **Non-breaking:** `add --stdin --depended-on-by` keeps working; it writes the live JSON path (remap + exists). In-loop adds with `TASK_MGR_ACTIVE_PREFIX` set are unchanged. DB anchoring stays main checkout.
- `current --from-json` is new; unregistered is an error. `current` without the flag is unchanged (including ≥2-prefix probe).

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/commands/cheatsheet.rs` | Update | Recipe + drop forbidden anchor (US-008) |
| `.claude/skills/verify-task-mgr/features/add-and-current-from-json.md` | Create | Operator drive for add/current `--from-json` |
| `.claude/skills/verify-task-mgr/features/README.md` | Update | Link the new feature file |
| `docs/ARCHITECTURE.md` (~line 464) | Update | One sentence: pin with `--from-json`; worktree writes the live copy |
| `src/cli/commands.rs` after_help **and** `Commands::Current` rustdoc | Update | Pin, not import; rustdoc: exit 0 for no-flag probe; non-zero for unregistered/missing/directory `--from-json`. Delete “Exits 0 in all cases” |
| `src/loop_engine/claude.rs` | Update | Comment: resolver lives in `context.rs` |
| enhance / intents / `task_ops` / best-practices | **PR-3** | Do not rewrite here |
| `~/.claude/docs/task-mgr-best-practices.md` | Residual BP after PR-3 | Not in this repo |

### Institutional memory (recall)

Embed so the loop does not re-learn:

- **#4441** / **#4237**: add from worktree wrote main (or failed to open a bare filename); loop mutates the worktree copy.
- **#2236**: omitted `--from-json` leaks fixups into the wrong JSON — refuse ≥2 unpinned writes; ship the flag.
- **#3240**: path resolution from a worktree cwd is not “the worktree JSON” today.
- **#1562**: `.{filename}.task-mgr-add.tmp` is the old name; replace with pid-counter-nanos.
- **#3440**: JSON sync is best-effort after DB commit.
- **#2667**: same-directory tmp + rename.
- **#2303** / **#2798**: loop re-imports the worktree copy; writing only main is silently overwritten.

---

## 7. Open Questions

None. Pins, shape, and the phase seed answered the clarifying questions. Architect Questions for User: none. A still-blocking question would have been `PAUSE-NEEDED`.

---

## AA review (folded)

**Source:** `tasks/prd-agent-task-ops-pr1-architect.md` (NEEDS_CHANGES, 2026-09-09). Questions for User: none. First verdict had determinate Highs; all Suggested Revisions are now ACs / contract text in the body (not this note alone).

| Architect concern | Resolution |
| --- | --- |
| **High — add keeps writing `locate_prd_json`’s raw DB path** (display vs write split, **#4237**) | Folded into US-004 / FR-003: after DB commit, `append_user_story` is called on `ResolvedContext.prd_json_path` **only**; delete the second `locate_prd_json` write. `--from-json` → canonical PATH; default → remap then `is_file()` (worktree else registered else skip). `ctx is None` → JSON sync iff exactly one `task_list` row. |
| **High — `Some(ctx)` + empty prefix runs `apply_prefix("")`** → `-FEAT-001` | Folded into US-004 / US-006: `--from-json` of a NULL-prefix registered file returns `Some(ctx)` with empty `prefix`, `source=from-json`, write path set, and **does not** call `apply_prefix`. Test: inserted id is `FEAT-001`, not `-FEAT-001`. |
| **High — ≥2 refuse vs 0-prefix vs `current` probe share `Ok(None)`** | Folded into CONTRACT-002 / US-006: `resolve_context` keeps `Ok(None)` for both 0 and 2+ prefixes. **`add` only**, when `ctx.is_none() && load_known_prefixes().len() >= 2`, returns `invalid_state` naming `--from-json` / `TASK_MGR_ACTIVE_PREFIX`. Zero prefixes still insert. Rejected folds: error inside `resolve_context` (breaks `current`); `if ctx.is_none() { refuse }` (breaks `--no-prefix`). |
| **High — remap seed drops startup’s `source_root` canonicalize** | Folded into US-001 / FR-001: helper stays pure (no dest `canonicalize`). Step 8.5 **keeps** canonicalize-`source_root` then calls the helper. Unit: symlink `source_root` vs canonical `prd_file` still remaps. |
| **Medium — `current target=` when neither copy exists** | Folded into US-005: print `target=(none)`. Writers skip; probe must not invent a path. |
| **Medium — `Commands::Current` rustdoc “Exits 0 in all cases”** | Folded into US-005: rustdoc (not only `after_help`) says exit 0 for no-flag probe; non-zero for unregistered/missing/directory `--from-json`. |
| **Medium — match (a) is prefix identity, not path identity** | Folded into CONTRACT-001: pin 19 identity function is **(b)+(c)**. Match (a) is a separate prefix OR. A stray copy with the same `taskPrefix` is registered via (a); pin 4 writes that PATH. Documented, not cut. |
| **Medium — `prd_files` relative join is `source_root`, not `tasks_dir`** | Folded into CONTRACT-001 / US-004: identity tests seed `prd_files` as init would (`tasks/foo.json` or absolute), never a bare basename unless that is what init stored. |
| Naming: US-001 only named `worktree_root` | Folded into US-001 / public contracts: export `git::worktree_root_at` + `git::worktree_root()` to match `main_repo_root_at` / `main_repo_root`. |

**Accepted residuals (not bugs; documented):**

- Match (a) can pin a stray same-prefix copy and write that PATH (pin 4). Path identity remains (b)+(c). Changing (a) would re-expand the pin.
- Switching `main.rs::get_project_root` onto `git::worktree_root` stays optional (assumption 4).
- verify-task-mgr does not spawn git worktrees (US-005 rust tests own live path).

**Inversion table from the architect file — now guarded:** display/write split (US-004), `apply_prefix("")` (US-004), refuse site (CONTRACT-002), symlink `source_root` (US-001). Previously already guarded items (`exists()` leak, relative identity, clap/refuse same PR, Value round-trip, no remap of `--from-json`, no register, tmp names, failure copy, no basename invent) stay guarded.

---

## Appendix

### Related Documents

- `tasks/prd-goal-agent-task-ops-ux-ledger.md` — pins, shape, phase table
- `.claude/skills/verify-task-mgr/SKILL.md`
- `src/loop_engine/CLAUDE.md` — worktree / live PRD path
- `src/db/path.rs` — worktree DB anchoring (do not change)

### Glossary

- **Pin**: `--from-json PATH` selects an already-registered `task_list`. Not import. Not register.
- **Registered**: a `prd_files` `task_list` row whose identity matches PATH after join + canonicalize + remap, or whose JSON `taskPrefix` is in `prd_metadata`.
- **Write path**: the file CLI add/current will mutate after remap + existence (or the canonical `--from-json` PATH).
- **Live remap**: path math from registered path onto `worktree_root`. Unconditional in the loop.
- **0-prefix / `--no-prefix`**: `PrefixMode::Disabled`; `prd_metadata.task_prefix` is NULL; not the ≥2-prefix refuse case.

### Expected files (this phase)

`src/git/mod.rs`, `src/loop_engine/startup.rs`, `src/commands/context.rs` (new), `src/commands/prd_json.rs` (new), `src/commands/add.rs`, `src/commands/current.rs`, `src/cli/commands.rs`, `src/main.rs`, `src/commands/mod.rs`, `src/commands/cheatsheet.rs`, `src/loop_engine/prd_reconcile.rs` (shared tmp helper only), `src/loop_engine/claude.rs` (comment), `tests/worktree_db_resolution.rs`, `tests/cheatsheet_drift.rs`, `src/cli/tests.rs`, new/extended CLI tests, `.claude/skills/verify-task-mgr/features/add-and-current-from-json.md`.

### Test matrix → story map

| Named case | Story / criterion |
| --- | --- |
| Main checkout cwd → registered unchanged | US-005 |
| Linked worktree, worktree file exists → write worktree; main bytes unchanged | US-005 |
| Linked worktree, only main exists → write main; no invent; no basename search | US-005 |
| DB row in main-repo `.task-mgr` | US-005 (existing tests stay) |
| Loop startup remaps even if dest missing | US-001 |
| Symlink `source_root` vs canonical `prd_file` still remaps | US-001 |
| Happy pin, auto-prefix, append, reverse `--depended-on-by` | US-004 |
| After DB commit, append `ctx.prd_json_path` only (no second `locate_prd_json`) | US-004 / FR-003 |
| `--from-json` NULL-prefix: `Some(ctx)`, skip `apply_prefix`, id `FEAT-001` not `-FEAT-001` | US-004, US-006 |
| `ctx is None` JSON sync iff exactly one `task_list` | US-004, US-006 |
| Unregistered / never-inited → error, no DB row | US-004, US-009 |
| Missing file / directory → error, no DB row | US-004, US-009 |
| Flag vs env mismatch → flag wins, stderr note | US-004 |
| Cross-PRD `--depended-on-by` still refuses | US-004 |
| ≥2 prefixes, no pin → refuse | US-006, US-009 |
| `--no-prefix` / 0-prefix add still works | US-006, US-009 |
| Relative `prd_files` + worktree `--from-json` registers | US-004 |
| `current --from-json` registered vs unregistered; `target=` is write path | US-005, US-009 |
| Neither remapped nor registered is a file → `current` `target=(none)` | US-005 |
| `Commands::Current` rustdoc: exit 0 probe; non-zero for bad `--from-json` | US-005 |
| Add-only refuse: `ctx.is_none() && load_known_prefixes().len() >= 2` | US-006 |
| Concurrent add vs `update_prd_task_passes` tmp names | US-003 |
| Cheatsheet contains `add --from-json` in the clap PR; still forbids `set-status`, `recall --top-k`, `learnings show` | US-008 |
| Identity tests seed init-shaped `prd_files` (`tasks/foo.json` or absolute) | US-004 / CONTRACT-001 |
