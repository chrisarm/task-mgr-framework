# JSON review: agent-task-ops-pr1

**Status**: pass 2 — nothing material (2026-09-09)
**Pass 2 reviewer**: `01a08864-c729-7402-b29b-fad34e5c3d90`

All six pass-1 warnings applied. Ready for loop.

---

## Pass 1

**Date**: 2026-09-09
**Reviewer**: md-to-json-prd-reviewer (`01a08857-6988-70a1-af38-ffc2ea7fbd9c`)
**Artifacts**: `tasks/prd-agent-task-ops-pr1.md` · `tasks/agent-task-ops-pr1.json` · `tasks/agent-task-ops-pr1-prompt.md`

## Summary

- **Total tasks:** 16 (3 CONTRACT + 10 FEAT + 3 review)
- **Tasks needing revision:** 4
- **Critical issues:** 0
- **Warnings:** 6
- **PR-2 / PR-3 leak:** none. Pins 5–10 / 17–18 are named only as “do not implement.”

No loop-aborting defects. The list covers remapper, `context`, `prd_json` unique tmp, add/current `--from-json` in the same PR as the ≥2-prefix refuse and cheatsheet flip, worktree write policy, failure copy, and verify-task-mgr. Dependency graph is acyclic; `requires: []` is present; paths match this tree (new files are clearly new).

The warnings below are real agent traps, not nits. None of them contradict a pin.

---

## Critical Issues (Must Fix)

None. Pin 20 is enforced as **same branch** (FEAT-006/008 `dependsOn` FEAT-004; `mergeStrategy` + REVIEW-001 grep), not same commit, which is what the pin requires.

---

## Warnings (Should Fix)

1. **[FEAT-004] Oversized (11 ACs, `estimatedEffort: high`) plus a skill drive of a feature file that does not exist yet.**
   US-004 was correctly split (pin vs write-path → 004 / 004b), but 004 still bundles clap, identity (b)+(c)+match (a), unregistered/missing/directory, env mismatch, NULL-prefix `apply_prefix`/`prefix_id`, relative+worktree registration, and a verify-task-mgr drive. Over 7 ACs is the coherence line; 11 is inside the hard cap of 12.
   **Worse:** FEAT-004/005/006 all tell the agent to drive `features/add-and-current-from-json.md`, but FEAT-009 is the task that **creates** that file and it depends on 004–008. The prompt’s Project Verification Skills section lists those FEATs as mapped and says to Read the matching `features/*.md`. A missing file plus “if the skill is blocked, emit BLOCKED” can stall 004 for three iterations. The 004 escape hatch (“if the feature file is not yet present, follow SKILL.md + this task’s ACs”) fights that, but global AC + prompt mapping do not repeat it.
   **Fix:** Drop skill-drive ACs from 004/005/006 (keep rust tests). Create the feature file **before** any drive — either split FEAT-009 into “write recipe” (priority ~6, `dependsOn: []`) + “drive” (after 004–008), or keep all sandbox proof on FEAT-009 + REVIEW-001 only.

2. **[Prompt vs JSON] FEAT-004b is mapped to verify-task-mgr in the prompt, not in the JSON.**
   Prompt “This PRD maps to” includes `FEAT-004b write-path as sandbox-visible pin/append`. JSON 004b has no skill AC. Sandboxes are not worktrees (PRD assumption 2), so a 004b skill drive cannot prove the live write path anyway (that is FEAT-005 rust). Align the prompt map with the JSON, or add an explicit “not this skill” note on 004b.

3. **[FEAT-002] Pin-19 identity is in the AC, but `dependsOn` is only `CONTRACT-002`.**
   AC: own “pin-19 identity (b)+(c) stub/function matching CONTRACT-001”. Without a `CONTRACT-001` edge, 002 can start from 002’s contract only. Priorities (0 then 1 then 4) usually serialize this; the graph does not.
   **Fix:** `dependsOn: ["CONTRACT-002", "CONTRACT-001"]`. Keep the stub-vs-FEAT-004 split in notes.

4. **[FEAT-005] Clap rustdoc is updated; `current.rs` rustdoc still lies.**
   Today `src/commands/current.rs` says it **never** errors for “no active PRD” and `current(db_dir)` has no `from_json`. After this task, unregistered/missing/directory `--from-json` is `Err`. US-005 only names `Commands::Current` rustdoc / `after_help`. `touchesFiles` already includes `current.rs` (signature change).
   **Add AC:** update `current()` / module rustdoc the same way (exit 0 for no-flag probe; non-zero for bad `--from-json`). Delete “Never returns an error for no active PRD.”

5. **[FEAT-004 / FEAT-006 / FEAT-009] ≥2-prefix sandbox proof vs seeded fixture.**
   `tests/fixtures/sample_prd.json` has `taskPrefix: "3019e47c"`. SKILL.md’s canned recipe is `loop init "$PRD" --no-prefix`. Two `--no-prefix` imports ⇒ `load_known_prefixes().len() == 0` ⇒ refuse never fires; `--no-prefix` insert still passes, so a naive drive looks green. FEAT-009 notes mention a second non-NULL prefix; 004/006 skill ACs do not.
   **Fix:** On any remaining skill AC (or only on 009): “≥2-prefix refuse requires two `loop init`s **without** `--no-prefix` (or two files with distinct `taskPrefix`). Do not use two `--no-prefix` copies of sample_prd.”

6. **[FEAT-004] Directory `--from-json` is not a canonicalize failure.**
   `fs::canonicalize` on a directory succeeds. AC says “Missing file / directory → error” but not *how*. Naive impl: canonicalize → parse JSON → opaque parse error, and a write txn might already be open.
   **Add:** reject if `!metadata.file_type().is_file()` **before** parse and **before** a write txn; distinct copy from “not a registered task_list” / missing path.

---

## Suggestions (Nice to Have)

1. **[FEAT-004] Directory/unregistered discriminators are present; a known-bad for “canonicalize-only identity without `source_root.join`” is only in `failureModes`, not a named rust test title.** Keep the named US-004 relative+worktree test; it is already there — don’t drop it when shrinking ACs.
2. **[FEAT-001] “Match `main_repo_root_at`” vs `--show-toplevel`.** `main_repo_root_at` uses `--git-common-dir` (main checkout). `worktree_root_at` must be `--show-toplevel` (linked worktree). The parenthetical in the AC is correct; an agent who copies `main_repo_root_at` body will remap onto main. One “do not copy git-common-dir” clause would help.
3. **[Prompt quality checks]** `cargo test --test cli_tests` does **not** run `src/cli/tests.rs` (that is `cargo test -p task-mgr cli::`). FEAT-004/005 put parse tests in `src/cli/tests.rs`. Add that filter to the scoped gate list.
4. **[consumerAnalysis line numbers]** Frozen at authoring (`add.rs:345` locate, `:238` resolve). After FEAT-002 those lines move. Later FEATs should grep `locate_prd_json` / `apply_prefix`, not trust the table.
5. **[CODE-REVIEW-1]** Description says “early review” but `dependsOn` includes FEAT-009. Harmless in the lean skeleton; reword to “post-implementation wiring review.”
6. **No TEST-INIT / SEC-xxx.** Acceptable here: CONTRACTs carry discriminators; `--from-json` is trusted CLI (PRD security). Tests live on FEAT ACs. Don’t add a generic `set-status` or update/export task.

---

## Missing Tasks

None required for PR-1 intent. Optional only if you take warning 1: a small **FEAT-009a** (write `features/add-and-current-from-json.md` + README link, no drive) with priority before FEAT-004.

Do **not** add: `task-mgr update`, `humanReviewOutcome`, export `--all`/`--force`, `set-status`, claim-scoped short ids, DB-anchoring changes, historical prompt rewrites.

---

## Pin / intent coverage (no contradiction)

| Intent | Where |
| --- | --- |
| Remapper pure path math, no `exists()`, loop unconditional | CONTRACT-001, FEAT-001; startup keeps canonicalize-`source_root` |
| `context.rs` pin protocol; `Ok(None)` for 0 and 2+ | CONTRACT-002, FEAT-002; refuse **not** inside resolver |
| `prd_json` unique tmp + rename; share helper only | CONTRACT-003, FEAT-003; `update_prd_task_passes` not merged |
| `--from-json` = pin, never register, never remap PATH | FEAT-004; help “pin” not “import” |
| Default worktree write; flag path unmapped | FEAT-004b + FEAT-005 live matrix |
| ≥2-prefix refuse in the **same PR** as clap | FEAT-006 after FEAT-004; cheatsheet FEAT-008; REVIEW-001 pin 20 |
| Failure copy names `current` + retry `--from-json`, never `export` | FEAT-007 |
| Cheatsheet flip | FEAT-008 |
| verify-task-mgr drive | FEAT-009 (+ extra drives on 004/005/006 — see warning 1) |
| `--depended-on-by` cannot pin worktree-only | FEAT-006 AC |
| Identity one function (b)+(c); match (a) separate OR | CONTRACT-001 |
| `--no-prefix` / 0-prefix still insert | FEAT-006; global AC |
| DB anchoring unchanged | global AC + FEAT-005 existing tests |

---

## Task-by-Task Review (only those that need changes)

**FEAT-004** Add clap `--from-json` pin protocol
- **Issue:** 11 ACs + skill drive of a not-yet-written feature file; directory reject unspecified; ≥2-prefix sandbox recipe not restated.
- **Recommendation:** Keep rust ACs 1–10; move skill drive to FEAT-009 (or a 009a recipe task that lands first). Add directory `is_file()` before txn.
- **Do not drop:** NULL-prefix skip `apply_prefix` **and** `prefix_id` (`FEAT-001` not `-FEAT-001`); relative `tasks/foo.json` identity; no `prd_files` insert.

**FEAT-002** Extract `commands/context.rs`
- **Issue:** Identity stub required without `dependsOn: CONTRACT-001`.
- **Recommendation:** Add that dependency. Do not implement ≥2 refuse here (already stated).

**FEAT-005** `current --from-json` + live worktree matrix
- **Issue:** Clap rustdoc only; `current.rs` still claims never-error. Prompt maps 004b to the skill; this task correctly says worktree cases are **not** the harness.
- **Recommendation:** One extra rustdoc AC on `current.rs`. If skill drives are removed from 004/006, keep this task’s rust live-path matrix — that is the #4237 proof.

**FEAT-006** ≥2-prefix refuse
- **Issue:** Skill drive before feature file; `--no-prefix` fixture trap.
- **Recommendation:** Same as warning 1 and 5. Keep the add-only predicate `ctx.is_none() && load_known_prefixes().len() >= 2` — that is the pin 3/16 implementation and it is correctly **not** in `resolve_context`.

**FEAT-009** verify-task-mgr feature + drive
- **Issue:** File creation is sequenced after the tasks that already try to drive it.
- **Recommendation:** Split write vs drive, or make 004/005/006 stop requiring the drive.

---

JSON structure, global AC, priority philosophy, `requires: []`, and out-of-scope locks are in good shape. `synergyWith` / `batchWith` / `conflictsWith` are omitted; that matches current engine behavior (deprecated, `touchesFiles` is the conflict source).
