# Loop Review: Agent task-ops UX PR-1

**PRD:** `tasks/prd-agent-task-ops-pr1.md`
**Branch:** `feat/agent-task-ops-pr1` (`branchName` from `tasks/agent-task-ops-pr1.json`)
**Worktree:** `/home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-agent-task-ops-pr1`
**HEAD:** `1d5f494` (20 commits beyond `main`; REVIEW-001 final gate present)
**Uncommitted:** `tasks/agent-task-ops-pr1.json` (loop state) and untracked `verify-task-mgr` artifacts (local proof; not source)

**Verdict: NEEDS WORK**

## Summary

PR-1 ships the pin protocol, remapper, JSON chokepoint, clap `--from-json` on `add`/`current`, ≥2-prefix refuse, cheatsheet recipe, and isolated verify-task-mgr drive. The architect footguns named in the PRD are closed on the primary prefixed-PRD path: remap stays path math, startup Step 8.5 does not `exists()`, `--from-json` is never remapped away, refuse is add-only, append uses `ctx.prd_json_path` only, and help/cheatsheet say pin not import.

One High remains on the new NULL-prefix `--from-json` path: empty `ctx.prefix` skips `apply_prefix`/`prefix_id`, but JSON sync still takes `input.task_prefix()` from id shape, so `CODE-FIX-001` is inserted in the DB and written as `FIX-001` in JSON. The AC test used 2-segment ids (`FEAT-001` / `SEED-001`) and missed it. Not Critical (prefixed loop pin is the live path and is correct); do not merge the `--no-prefix` pin story as done.

## Code Review Summary

- **Files reviewed:** 16 production/test files in `src/` + `tests/` plus verify feature/artifacts (30 files changed vs `main`, +7005/−752)
- **Critical findings:** 0
- **High findings:** 1
- **Medium findings:** 1
- **Low findings:** 5

## Critical

None. `--from-json` does not register, does not invent paths, tmp files stay beside the target, LockGuard remains on add, and DB anchoring to main-checkout `.task-mgr` is unchanged.

## High

### H1. NULL-prefix pin still treats 3-segment ids as a PRD prefix (JSON/DB desync + `--depended-on-by` refuse)

**PRD:** empty `ctx.prefix` skips `apply_prefix` **and** `prefix_id`; inserted id is unprefixed. That holds for `FEAT-001`. It does **not** hold for the ids agents spawn (`CODE-FIX-*`, `WIRE-FIX-*`, `REFACTOR-N-*`).

**(a) JSON strip uses id-shape, not `ctx.prefix`**

```397:449:src/commands/add.rs
    let task_prefix = input.task_prefix().map(String::from);
    let story = input.into_prd_user_story(priority);
    // ...
        Some(path) => match append_user_story(
            &path,
            &story,
            effective_depended_on_by,
            task_prefix.as_deref(),
        )
```

`AddTaskInput::task_prefix()` (`src/commands/add.rs:97–107`) returns the first segment whenever the remainder contains a dash. After the empty-prefix skip, `CODE-FIX-001` stays unprefixed in the DB, but `append_user_story(..., Some("CODE"))` runs `strip_task_prefix` (`src/loop_engine/output_parsing.rs:16–23`) and writes **`FIX-001`** into JSON (`src/commands/prd_json.rs:77`, `123`). Next `loop init --append` / `update_prd_task_passes` cannot round-trip (same class of split as learning **#4237**). Duplicate check against `FIX-001` can also fail a later add.

**(b) `reject_cross_prd_depended_on_by` runs before the empty-prefix skip and treats any 3-segment target as foreign**

```329:337:src/commands/add.rs
    reject_cross_prd_depended_on_by(conn, depended_on_by, resolved_ctx.as_ref())?;
    // ...
        if prefix.is_empty() {
            depended_on_by
```

```605:622:src/commands/add.rs
        let Some(target_prefix) = extract_id_prefix(target_id) else {
            continue;  // 2-segment only (SEED-001)
        };
        if target_prefix == ctx.prefix {  // "CODE" == "" → false
            continue;
        }
```

`--from-json` of a `--no-prefix` file + `--depended-on-by CODE-FIX-001` (the actual DB id) errors as “prefix `CODE` is not registered.” 2-segment targets slip through; that is what `test_from_json_null_prefix_skips_apply_prefix_and_prefix_id` uses (`SEED-001` / `FEAT-001` at `src/commands/add.rs:1473`).

**Why it matters:** this is the new pin path for `PrefixMode::Disabled`. The AC test is 2-segment-only, so the suite is green while spawn-fixup ids desync or refuse. **PRD deviation** (NULL-prefix pin must not prefix). Prefixed 8-hex loop pins are unaffected (`cd58f61b-CODE-FIX-001` strips back to `CODE-FIX-001`).

**Fix (for a human / follow-up):** when `ctx.prefix` is empty (and on the `ctx is None` `--no-prefix` path), pass `None` into `append_user_story`. In `reject_cross_prd`, only refuse 3-segment targets whose first segment is a **known other** `prd_metadata.task_prefix` (empty active prefix is not a foreign-prefix match). Add a test that `--from-json` of a NULL-prefix file with `{"id":"CODE-FIX-001"}` and `--depended-on-by CODE-REVIEW-1` inserts/strips nothing.

Not spawned as `CODE-FIX`: High, not Critical, and the fix touches refuse + JSON strip + tests — not a trivial one-liner.

## Medium

### M1. Skip-note copy lies when a `prd_files` row exists but neither remapped nor registered path is a file

`src/commands/add.rs:426–431`, `668–672`

CLI policy correctly skips (no invent). The note still says “no PRD JSON registered in `prd_files`.” Recovery hint (`task-mgr current` + retry `--from-json`) is right and never names `export`. Weak honesty miss on pin 11, not the never-`export` rule.

## Low

### L1. Pin-19 (c) is not proven through `resolve_context`

`tests/add_integration.rs:962` keeps `"taskPrefix":"WT"` so match **(a)** wins, and it never `chdir`s into the fixture repo. `paths_identify` itself is unit-tested (`src/commands/context.rs:810`). Live worktree matrix in `tests/worktree_db_resolution.rs` covers default (no-flag) remap+exists. A `(c)`-only pin (no `taskPrefix` in JSON, cwd = worktree) is untested end-to-end.

### L2. Empty prefix renders as `prefix=` blank

`src/commands/add.rs:320`, `src/commands/current.rs:52`. Path empty already prints `(none)`. Cosmetic.

### L3. `format_text` still prints `Synced into PRD JSON` when append `Err`s

`src/commands/add.rs:452–454`. Pre-existing; tests now lock `prd_path = Some(target)` on failure. Stderr warning is correct; stdout is not.

### L4. `ResolutionSource::None` is never constructed

`src/commands/context.rs:36`. Dead serde variant; `Ok(None)` is the probe.

### L5. `prd_json` hardcodes `invalid_state("add", …)`

`src/commands/prd_json.rs:79–136`. Fine for PR-1 (only add calls the chokepoint). PR-2 `update` should parameterize the command name.

## Coherence Assessment

- **PRD alignment:** PARTIAL — US-001 through US-009 are implemented on the primary prefixed path; US-004/US-006 NULL-prefix pin is incomplete for 3-segment ids (H1).
- **Deviations:** H1 as above. ARCHITECTURE.md still says “DB + JSON are always updated together” (pre-existing clause next to the new pin sentence; pin 11 is best-effort). Remaining enhance/intents/`task_ops` copy is correctly deferred to PR-3.
- **Cross-PRD contract status:** CONTRACT-001/002/003 surfaces match the seed. Match (a) is a separate prefix OR, not folded into `paths_identify`. `prd_json` does not import `add`. `context` does not import write helpers. Startup does not call `choose_cli_write_path`. No PR-2 `update` command and no export `--force` slipped in.

### User-story spot-check

| Story | Status |
| --- | --- |
| US-001 remapper + startup canonicalize-`source_root` | Satisfied. Helper has no `exists()` / dest canonicalize (`src/git/mod.rs:152–163`). Step 8.5 keeps canonicalize then helper (`src/loop_engine/startup.rs:619–671`). |
| US-002 `commands/context.rs` | Satisfied. `invalid_state` command-name parameterized; `current` stale-pin does not say `"add"`. `resolve_context` stays `Ok(None)` for 0 and 2+. |
| US-003 `prd_json` chokepoint | Satisfied. Shared `unique_tmp_path` (`.{base}.{pid}-{n}-{nanos}.tmp`); `prd_reconcile` imports it; unknown keys on existing stories survive (`humanReviewOutcome` test). |
| US-004 `add --from-json` | Partial. Pin/unregistered/missing/directory/no-register/relative identity helper/write-path-on-ctx are in. NULL-prefix 3-segment JSON strip is H1. |
| US-005 `current` + live worktree matrix | Satisfied. `target=` is write path; `(none)` for empty path; rustdoc no longer says “Exits 0 in all cases”; live tests write worktree and leave main bytes unchanged; DB stays on main `.task-mgr`. |
| US-006 ≥2 refuse, 0-prefix insert | Satisfied on the refuse/insert predicates. 0-prefix JSON strip for 3-segment ids shares H1. |
| US-007 failure copy | Satisfied. Names `task-mgr current` and `--from-json`; grep lock against `task-mgr export`. |
| US-008 cheatsheet | Satisfied. Recipe contains `add --stdin --from-json`; still forbids `set-status`, `recall --top-k`, `learnings show`. |
| US-009 verify-task-mgr | Satisfied. Feature file + artifacts under `.claude/skills/verify-task-mgr/artifacts/20260909T172345-500665/` and `…T172502-505421/` prove pin, unregistered, missing, directory, ≥2 refuse, `--no-prefix` insert (`BARE-001`), and help pin copy. Worktree live-path not claimed here. |

## What looks solid (pins)

| Pin | Status |
| --- | --- |
| 1, 13 — pin not import; never remaps `--from-json` PATH | `resolve_from_json_flag` sets `prd_json_path = canon`; no `prd_files` insert |
| 2 — unregistered names `loop init` | `src/commands/context.rs:168–176` |
| 3, 16 — ≥2 refuse add-only; 0-prefix insert | `src/commands/add.rs:294–308`; resolver stays `Ok(None)` |
| 4, 15 — CLI remap then `is_file()`; loop remap unconditional | `choose_cli_write_path` `418–435`; startup Step 8.5 has no `exists()` |
| 11 — failure copy | `json_sync_recovery_hint`; no `task-mgr export` in `add.rs` |
| 12 — one tmp chokepoint | `prd_json.rs:27–40`; existing stories stay `Value` |
| 14, 19 — remap path math; identity is (b)+(c) only | `git/mod.rs:152–163`; match (a) is a separate OR |
| 20 — clap + cheatsheet same PR | `cli/commands.rs:660–667`, `1252–1261`; recipe in `cheatsheet.rs:57` |
| 21 — DB anchoring | worktree tests still assert `!wt/.task-mgr` |

`FromJsonFlag` is wired (not reserved). Verify artifacts: happy pin appends `cd58f61b-CODE-FIX-001`; unregistered/missing/directory/multi-prefix exits 1; `--no-prefix` inserts `BARE-001`.

## Action Items

1. **Before merge of the NULL-prefix pin story:** fix H1 (pass `None` into `append_user_story` when `ctx.prefix` is empty; do not treat 3-segment `--depended-on-by` as foreign solely because active prefix is empty). Add `CODE-FIX-001` coverage.
2. Optional: correct the skip-note reason string (M1).
3. Do not run `/compound` until H1 is addressed — capturing “NULL-prefix pin skips prefixing” as settled wisdom would be wrong.

No fixup tasks spawned (finding is High, not Critical; fix is not a trivial one-liner).
