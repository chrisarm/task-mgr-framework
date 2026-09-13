# Architect review: prd-agent-task-ops-pr3

**Status**: APPROVED (pass 2, 2026-09-09)
**Pass 1**: NEEDS_CHANGES (folded)
**Pass 2 reviewer**: `01a088d3-7bc6-7d50-9d6a-3da4db022378`
**Questions for User**: none

Pass 2: all seven pass-1 revisions are ACs/contract text. Leftovers ≤ medium. Ready for `/prd-tasks` (not started — operator pause).

---

## Pass 1 (folded)

**Status**: NEEDS_CHANGES
**Date**: 2026-09-09
**Reviewer**: production-code-architect (`01a088c7-03e1-7521-9496-656d53e2c177`)
**Questions for User**: none

Pins 1–21 match the ledger verbatim. PR-1 export/clap/`task_ops` are still dump-all / `.tasks[]` (confirmed on the worktree). Scope is export + docs/prompt only — `update` is named, not re-specified.

**Strengths**:
- Ledger phase 3 only: scoped dump, `--force` dest guard, `task_ops`/intents/cheatsheet/docs. Pins 5–7/10/17 stay PR-2; remapper stays PR-1.
- Approach A is the only pin-8/9/18-legal design (`--all` = today’s dump; `--force` is a dump; dest is never `cli_write_path`).
- Extraction table matches main (`export()` 72–133, LIMIT 1 metadata, clap 283–296, no lock, `task_ops` 2027 bytes / `.tasks[]`). PR-1 worktree export clap is still identical.
- Fail-closed edges are named: empty prefix must not LIKE `"-%"`; overwrite-guard is pin-19 not match (a); `--all --from-json` clap-conflicts; spawn-fixup (a) stays; pin 11 must not start naming `export`; verify recipe is created before drive.
- Coupling budget is right: `prd_json` ↛ `export`; `export` ↛ `add`/`update`; startup ↛ overwrite-guard.

**Concerns**:

**High — LockGuard specified in two exclusive sites (deadlock).**
Add locks **inside** `add()` (`feat-agent-task-ops-pr1/src/commands/add.rs:216`), not in `main.rs`. This PRD puts dest-exists lock in **both** US-003 (`main.rs` then export) **and** `export::export` (public contract). `LockGuard` is a non-reentrant `flock` on `tasks.db.lock` — both sites in one process hang or `LockError`. Pin 18 is “same LockGuard as add **if dest is a live PRD**.”

**High — US-003 “reuse `preflight_from_json_path`” imports add and mislabels errors.**
That helper is **private in `add.rs`** and hardcodes `invalid_state("add", …)`. Coupling budget forbids `export` → `add`. `resolve_from_json_flag` already does missing/directory with `command`. Following the AC as written ships export errors that say `"add"`.

**High — `scripts/claude-loop.sh` is the live smash caller and is not in the plan.**
Three sites (`:164` cleanup, `:533` per-iteration, `:552` final) run `export --to-json "$PRD_FILE"` with `|| true`. That **is** ARCHITECTURE’s “export after every iteration.” After the breaking default those lines fail (registered dest, no `--force`) and crash recovery **silently dies**. Adding `--force` onto `$PRD_FILE` reopens pin 9. US-005 lists README/INTEGRATION/QUICKSTART, not this script.

**Medium — NULL-prefix metadata has no `prd_id` handle.**
Assumption 4 / US-001 want the pin’s `prd_files.prd_id` row when `ctx.prefix` is empty. `ResolvedContext` is `{prefix, source, prd_json_path}` — no `prd_id`. `find_registered_by_path_identity` returns `Option<Option<String>>` (prefix only). Two `--no-prefix` inits → `WHERE task_prefix IS NULL` stamps the wrong `project`/`branchName`. “When the caller has it” is not an AC.

**Medium — CLI `--no-prefix` export tests omitted from the inversion list.**
`tests/human_review_cli.rs` (and the already-noted `model_fields_cli.rs`) do `init --no-prefix` then `export --to-json exported.json` (new dest). Zero-prefix default is the new error; they need `--all`. Library `PrefixMode::Disabled` → `All` is covered; these CLI tests are not.

**Medium — no-active error names only `task-mgr current`.**
Operators who want today’s dump will not discover `--all`. `invalid_state` expected text should name `--from-json`, `--all`, and `current`.

**Medium — second `enhance agents` can revert PR-2 CLARIFY.**
US-005 says “do not redo CLARIFY” but has no grep that the regenerated fenced block still contains `update --stdin` and does not restore embed-in-JSON.

**Questions for User**: none. The highs have determinate fixes; no pin is ambiguous.

**Suggested Revisions**:
1. **Lock (US-002 / US-003 / public contract):** Acquire `LockGuard` **inside** `export()` only, after `dest.is_file()`, before identity re-check and write — same as add. `main.rs` only forwards `ExportOpts`. Never lock in both. Missing dest → no lock.
2. **Pin preflight (US-003):** Drop “reuse `preflight_from_json_path`”. Missing/directory/unregistered go through `resolve_context(conn, from_json, "export")`. If a pre-open helper is extracted, put it in `context.rs` with a `command` parameter.
3. **US-005:** `scripts/claude-loop.sh` — **remove** (or retarget to an unregistered dump path) the three `export --to-json "$PRD_FILE"` calls. Do **not** add `--force` onto `$PRD_FILE`. Same lossy-dump warning as INTEGRATION.
4. **US-001:** Empty-prefix `--from-json` metadata is `prd_metadata.id = identity-matched prd_files.prd_id` (promote identity to return `prd_id`, or query it in export). Forbid `WHERE task_prefix IS NULL` without that id. `--all` stays `ORDER BY id LIMIT 1`.
5. **Callers:** `human_review_cli.rs` (and any other CLI `export --to-json` after `--no-prefix`) pass `--all`; dest stays a new file so `--force` is not required.
6. **No-active `invalid_state`:** expected names `--from-json` / `--all` / `task-mgr current`.
7. **US-005:** After `task-mgr enhance agents`, fenced block still has spawn-fixup (a) **and** PR-2 `update --stdin` CLARIFY; must not restore hand-edit + `loop init`.
