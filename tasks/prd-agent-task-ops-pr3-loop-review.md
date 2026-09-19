# Loop Review: Agent task-ops UX PR-3 — export scoped + docs/prompt alignment

**Worktree:** `/home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-agent-task-ops-pr3`  
**Branch:** `feat/agent-task-ops-pr3` (`jq -r .branchName` → `feat/agent-task-ops-pr3` from `tasks/agent-task-ops-pr3.json`)  
**Vs main:** 21 commits (`996d2cd`…`273387c`); ~40 files, +4583/−234  
**Uncommitted:** loop artifacts only (`tasks/agent-task-ops-pr3.json` modified, `.loop-pid-c3c1c195` untracked, `.stop-c3c1c195` deleted). No product-code dirt.  
**Reviewer:** rust-python-code-reviewer + inline PRD coherence (no `/compound`; no CODE-FIX spawned).

## Summary

CONTRACT-001 (scoped dump, matching metadata, trailing-dash LIKE, empty-prefix `ByPrdId`) and most of CONTRACT-002 (`LockGuard` inside `export()` after `dest.is_file()`, dest never remapped, `--force` is a lossy dump, `unique_tmp_path`, clap `--all`/`--from-json` conflict, `claude-loop.sh` off `$PRD_FILE`) are implemented and backed by unit, worktree, and verify-task-mgr artifacts. Docs/`task_ops`/cheatsheet/intents/enhance fenced block tell the same pin/`update`/`.userStories[]` story. One High remains: dest identity uses **cwd’s** worktree root, so an absolute `--to-json` at a linked worktree’s live JSON from main (or `/tmp`) misses pin-19 (c) and overwrites without `--force`.

**Verdict: NEEDS WORK**

## Code Review Summary

- **Files reviewed:** ~40 on `main...HEAD` (export, context identity, clap/main, task_ops, docs, `scripts/claude-loop.sh`, `tests/worktree_export_dest.rs`, verify feature + artifacts)
- **Critical findings:** 0
- **High findings:** 1
- **Medium/Low findings:** 6

## Critical

None.

## High

1. **Dest identity is cwd-bound, not dest-bound** — `src/commands/export/mod.rs:106-120` + `src/commands/context.rs:525-533` + `src/git/mod.rs:176-198`

   After `dest.is_file()`, export canonicalizes dest and calls `find_registered_by_path_identity` with `default_prd_roots(dir)`. That sets `worktree_root` to **cwd** only if cwd is a linked worktree; otherwise it clones `source_root` (main). Pin-19 (c) is `remap_into_worktree(registered, src, wt)` against **that** pair.

   Consequence: `task-mgr --dir <main>/.task-mgr export --to-json /abs/wt/tasks/foo.json` from main or `/tmp` is an identity **miss**, so the worktree copy is overwritten **without `--force`**. `tests/worktree_export_dest.rs` only runs with `cwd = worktree`, so this path is untested.

   PRD US-006 / pin 19: a worktree remapped copy of a registered `task_list` must require `--force`; write that PATH (not remapped away). The tests prove the cwd=worktree case; they do not prove dest-in-worktree from another cwd.

   **Fix (for a human / follow-up):** resolve `worktree_root` from `dest_canon` (keep cwd as fallback). Add a rust test: cwd=main, `--to-json` = worktree JSON, no `--force` → refuse, bytes identical.

## Medium

2. **`scripts/claude-loop.sh:164-166`, `:537-538`, `:557-559` — predictable `/tmp` dump dest**

   Retargeting off `$PRD_FILE` is correct (no `--force` onto the live PRD; grep clean). The new dest is `/tmp/task-mgr-dump-$(basename "$PRD_FILE")`, world-writable, basename-colliding, and still wrapped in `2>/dev/null || true`. A symlink at that path makes export write through it (unregistered dest → no `--force`). Prefer `mktemp` under `$TASK_MGR_DIR` or a unique `/tmp` name. Same pattern in `docs/INTEGRATION.md` (`/tmp/prd-dump.json`).

3. **No rust `export()` test for default active-prefix dump** (`src/commands/export/tests.rs`)

   Library `all: false` cases use `--from-json`, the no-active error, or `--force`/refuse. Single-prefix default (`from_json: None`, `all: false`) is proven only by verify-task-mgr (`export-new-dest`). A regression that required `--from-json` for every scoped dump would still pass `cargo test` for export. US-007 sandbox proof exists (`artifacts/20260918T230107-150350/`); this is a unit-test gap, not missing operator proof.

## Low

4. **`src/commands/export/mod.rs:257-297` — `write_json_atomic` leaves `unique_tmp_path` behind** on create/write/sync/rename failure. Same-dir tmp is correct (learning #2667); orphans accumulate on error.

5. **`CHANGELOG.md:7-25` vs `Cargo.toml:3`** — breaking export notes sit under `[Unreleased]` while the crate is already `0.3.3` with a stub “Version bump on `feat/agent-task-ops-pr3`” section. Confusing if this PR is meant to be the 0.3.3 release.

6. **`README.md:17`** still says “export-after-every-iteration for recovery”. Smash recipes at `:90` / `:529` were rewritten; this Why-bullet still contradicts `docs/ARCHITECTURE.md:472` (`prd_reconcile` / add / update; no Rust `export()`).

7. **`--learnings-file` / `--with-progress` sibling skip the dest identity guard** (`export/mod.rs:234-246`). PRD explicitly scopes the overwrite-guard to `--to-json` dest (`--with-progress` is not a `task_list`). Residual: an operator who passes a registered `task_list` as `--learnings-file` can smash it. Not a contract miss.

## Coherence Assessment

- **PRD alignment:** PARTIAL (functional slice landed; dest live-path pair incomplete when cwd ≠ dest tree)
- **US-001:** Met — `load_tasks` empty→no LIKE; named prefix `prefix_and`; metadata Unscoped / NamedPrefix / ByPrdId; grep forbids `WHERE task_prefix IS NULL` in `export/` production SQL; `--all` keeps `ORDER BY id LIMIT 1`; `ExportedPrd` has no `taskPrefix`.
- **US-002:** Mostly met — registered dest refuses without `--force`; missing dest no lock; dest is `--to-json`; `unique_tmp_path`; `main.rs:609-620` forwards `ExportOpts` only. **Hole:** worktree dest from non-worktree cwd (High #1).
- **US-003:** Met — clap fields + `--all` conflicts `--from-json`; pin help; no-active copy names `--from-json` / `--all` / `task-mgr current` (verify artifacts); `--no-prefix` CLI tests pass `--all`; `resolve_context(..., "export")`; no `preflight_from_json_path` / `refuse_unpinned_write`.
- **US-004:** Met — `.userStories[]`, `--from-json`, `task-mgr update`; no `.tasks[]`; no export-as-JSON-sync; budget test `< 2048`.
- **US-005:** Met except Low #6 README Why-bullet. Enhance spawn-fixup (a) + `update --stdin` CLARIFY still in fenced `CLAUDE.md`. `claude-loop.sh` has no `export --to-json "$PRD_FILE"`. CHANGELOG residual for `~/.claude/docs/task-mgr-best-practices.md` (not vendored).
- **US-006:** Incomplete — cwd=worktree dest cases exist and look correct; cwd=main + dest=worktree not covered (High #1).
- **US-007:** Met — feature file + README link; artifacts under `.claude/skills/verify-task-mgr/artifacts/20260918T230107-150350/` and `20260918T230222-160423/` (prefixed cluster + zero-prefix cluster).
- **Cross-PRD contracts:** Identity promoted to `(prd_id, prefix)`; match (a) stays source-only; `prd_json` does not import `export`; loop engine does not call `export()`.

## Checks that passed

| Concern | Result |
|---|---|
| Dual `LockGuard` | `main.rs` Export arm does not acquire |
| Empty-prefix metadata | `MetadataScope::ByPrdId`; no `task_prefix IS NULL` in export production |
| Dest vs match (a) | Guard is identity-only |
| Dest remap | Write `opts.to_json`; no `cli_write_path` in `export/` |
| SQL LIKE | `prefix_and` + bound `?` + `ESCAPE '\'` |
| Smash callers | No `export --to-json "$PRD_FILE"` in `scripts/claude-loop.sh` |
| `"add"` on export errors | Command name `"export"`; no `preflight_from_json_path` |
| Loop → `export()` | No `src/loop_engine` caller |
| Dump logging | No `tracing` of dump bodies |

## Action items

- **Block merge** until High #1 is fixed (dest identity against dest’s worktree root + rust test cwd=main / dest=worktree).
- Medium #2 (`/tmp` dump path) is worth the same PR if touching `claude-loop.sh` again; not a smash of the live PRD.
- Do **not** spawn CODE-FIX from this review (High, not Critical; not a trivial one-liner).
- Do **not** run `/compound` until High #1 is addressed and `/review-loop` is re-run.

## Unverified

- This review did **not** re-run `cargo test` / clippy; REVIEW-001 progress log claims the suite was green after `ENV_PREFIX_MUTEX` + snapshot refresh.
- `task-mgr how "export"` was not executed (no export intent — Low residual).
