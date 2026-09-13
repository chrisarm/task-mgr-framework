# Architect review: prd-agent-task-ops-pr1

**Status**: APPROVED (pass 2, 2026-09-09)
**Pass 1**: NEEDS_CHANGES (folded)
**Pass 2 reviewer**: `01a08845-05ab-71a1-b347-28fc89429e4f`
**Questions for User**: none

Pass 2: all eight pass-1 revisions are ACs/contract text. Leftovers ≤ medium (optional tightenings: skip `prefix_id` on `--depended-on-by` when prefix empty; remap-then-`is_file()` on the None+one-task_list path; drop LIMIT 1 as prefix-miss fallback). Do not re-open authoring.

---

## Pass 1 (folded)

**Status**: NEEDS_CHANGES
**Date**: 2026-09-09
**Reviewer**: production-code-architect (`01a08839-bb10-73f3-8db1-500951209bf4`)
**Questions for User**: none

Verified against current tree (not the PRD’s remembered line numbers): `Commands::Add` has no `--from-json` (`src/cli/commands.rs`); `FromJsonFlag` is reserved; `locate_prd_json` still falls back to first `task_list` `LIMIT 1`; add tmp is `.{filename}.task-mgr-add.tmp`; startup Step 8.5 remaps with `strip_prefix` and no `exists()`; `register_prd_files` is relative-or-absolute; `unique_tmp_path` is private in `prd_reconcile.rs`; cheatsheet forbids `add --from-json`; `Current` is a unit variant whose after_help already mentions the flag; worktree add still lands in main `.task-mgr`. Pins 1–4 / 11–16 / 19–21 are not contradicted. Cross-phase pins 5–10 / 17–18 are listed and not implemented. Approach A (pure remap + caller-side exists) is the right split.

---

## Strengths

- Scope matches the ledger: remapper + `context` + `prd_json` + clap `--from-json` on add/current in one PR, with the ≥2-prefix refuse and cheatsheet flip in that same PR (pin 20).
- Loop vs CLI split is correct: `remap_into_worktree` has no `exists()` / basename search; startup Step 8.4 copy-if-missing stays separate; `--from-json PATH` is never remapped away.
- `current` stays a probe on ≥2 prefixes without a flag; refuse is write-only. `--no-prefix` / 0-prefix is treated as a different mode.
- JSON chokepoint (Value round-trip, unique tmp, `prd_json` must not import `add`) actually protects pin 12/17 for PR-2.
- Extraction targets and consumers (`main.rs:76` logging, `current.rs`, cheatsheet_drift, `worktree_db_resolution`) are real. Assumption 3 is true: `loop_engine` already depends on `commands`.
- User-facing proof is in the right harness; worktree live-path stays in Rust tests.

---

## Concerns

**High — add will keep writing `locate_prd_json`’s raw DB path.**
Today display and write are already two calls: stderr `target=` uses `ctx.prd_json_path`, the append uses `locate_prd_json(conn, task_prefix)` *after* insert (`add.rs` ~345 vs ~471). Wiring `FromJsonFlag` and remap into `resolve_context` without changing the append site reproduces **#4237**: agent sees `target=<worktree>`, JSON lands on main. Pins 4/13 require the append to use the resolved write path (`ctx.prd_json_path`: canonical PATH for the flag, remap-then-exists otherwise). When `ctx` is `None`, sync only if exactly one `task_list`. That replacement is not an AC.

**High — `Some(ctx)` + empty prefix runs `apply_prefix("")`.**
`--from-json` of a `--no-prefix` / NULL `task_prefix` file is a valid pin via (b)/(c). PRD sets `ctx.prefix` to `""` and `resolve_context` returns `Some`. `add` then does `if let Some(ref ctx) = resolved_ctx { input.apply_prefix(&ctx.prefix) }`. `prefix_id("", id)` yields `-FEAT-001`. Pin 3/16 require unprefixed insert. No AC says skip `apply_prefix` when the prefix is empty.

**High — ≥2 refuse vs 0-prefix vs `current` probe share `Ok(None)`.**
`resolve_context` still returns `None` for both 0 and 2+ prefixes. The easy folds all violate a pin: error inside `resolve_context` breaks `current`’s exit-0 probe; `if ctx.is_none() { refuse }` breaks `--no-prefix`; leaving add as insert-on-None keeps the LIMIT 1 leak. Refuse must be **add-only**: `ctx is None && load_known_prefixes().len() >= 2`. CONTRACT-002’s signature does not say this.

**High — remap seed drops startup’s source_root canonicalize.**
Step 8.5 exists specifically because `paths.prd_file` is canonicalized by `resolve_paths` and `source_root` may be a symlink (`startup.rs` ~665–671). US-001 says replace that block with the §6 helper “exactly”; the helper is infallible and must not `canonicalize` the dest (missing dest still remaps). If callers also stop canonicalizing `source_root`, `strip_prefix` misses and loop remap silently stays on main. Pin 15 is “unconditional,” not “weaker.”

**Medium — `current target=` when neither copy exists.**
Write policy is skip. Probe output is unspecified (`(none)` vs remapped missing path vs registered path).

**Medium — `Commands::Current` rustdoc still says “Exits 0 in all cases.”**
Unregistered `--from-json` is an error. after_help is in the file list; the `///` on the variant is not.

**Medium — match (a) is prefix identity, not path identity.**
Any file whose `taskPrefix` is in `prd_metadata` is “registered,” then pin 4 writes that PATH. A stray copy with the same prefix is writable. Acceptable if documented as OR-with-path, not as pin 19’s identity function.

**Medium — `prd_files` relative join is `source_root`, not `tasks_dir`.**
Init stores `strip_prefix(db_dir/tasks).unwrap_or(json_path)`: typically `tasks/foo.json` or an absolute path, sometimes a basename in tests that put JSON under `db_dir/tasks`. Identity tests must seed production-shaped rows or relative+worktree will false-refuse.

---

## Inversion (how this design guarantees failure → already guarded?)

| Failure | Guarded? |
|---|---|
| Display remaps, write still `locate_prd_json` → **#4237** | **No** — not an AC |
| `apply_prefix("")` on `--no-prefix` pin | **No** |
| Refuse in `resolve_context` or on any `None` | Semantic table only; not in CONTRACT-002 |
| `exists()` leaks into startup | **Yes** — CONTRACT-001 + grep AC |
| Relative `prd_files` + worktree flag treated unregistered | **Yes** — named US-004 test, match (c) |
| ≥2 refuse ships without clap | **Yes** — same PR + cheatsheet_drift |
| `PrdUserStory` round-trip strips `humanReviewOutcome` | **Yes** — Value round-trip of the file |
| `--from-json` remaps the write target | **Yes** |
| `--from-json` registers a PRD | **Yes** |
| Tmp name collision with `update_prd_task_passes` (**#1562**) | **Yes** |
| Failure copy names `export` | **Yes** — US-007 |
| Symlink `source_root` remap miss | **No** — seed omits caller-side canonicalize |
| Basename discovery / invent worktree path | **Yes** — cuts |

---

## Questions for User

None. The highs have determinate fixes; no pin is ambiguous.

---

## Suggested Revisions

1. **US-004 / FR-003 AC (required):** After DB commit, `append_user_story` is called on `ResolvedContext.prd_json_path` only. Delete the second `locate_prd_json` write. `--from-json` → canonical PATH; default → remap then `is_file()` (worktree else registered else skip). `ctx is None` → JSON sync iff exactly one `task_list` row.

2. **US-004 / US-006 AC (required):** `--from-json` of a NULL-prefix registered file returns `Some(ctx)` with empty `prefix`, `source=from-json`, and the write path set — and **does not** call `apply_prefix`. Add a test that the inserted id is unprefixed (`FEAT-001`, not `-FEAT-001`).

3. **CONTRACT-002 (required):** `resolve_context` keeps `Ok(None)` for both 0 and 2+ prefixes (so `current` stays a probe). **`add` only**, when `ctx.is_none() && load_known_prefixes().len() >= 2`, returns `invalid_state` naming `--from-json` / `TASK_MGR_ACTIVE_PREFIX`. Zero prefixes still insert.

4. **US-001 AC (required):** `remap_into_worktree` stays pure. Startup Step 8.5 **keeps** canonicalize-`source_root` then calls the helper (same as today’s comment). Unit: symlink `source_root` vs canonical `prd_file` still remaps. Do not `canonicalize` the dest inside the helper.

5. **US-005:** When neither remapped nor registered path is a regular file, `current` prints `target=(none)` (writers skip; probe must not invent a path).

6. **US-005:** Update `Commands::Current` rustdoc: exit 0 for no-flag probe; non-zero for unregistered/missing/directory `--from-json`.

7. **CONTRACT-001:** Pin 19 identity function is (b)+(c) (canonicalize + `source_root.join` + remap). Match (a) is a separate prefix OR, not that function. Identity tests seed `prd_files` as init would (`tasks/foo.json` or absolute), never a bare basename unless that is what init stored.

8. Naming: export `git::worktree_root_at` + `git::worktree_root()` to match `main_repo_root_at` / `main_repo_root`. US-001 currently only names `worktree_root`.
