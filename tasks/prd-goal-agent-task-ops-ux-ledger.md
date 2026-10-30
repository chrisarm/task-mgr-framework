# Goal ledger: agent-task-ops-ux

Started: 2026-09-09 Source: `/prd-goal` on plan `agent task-ops UX: make add / update / export match the docs` (user: 3 PRDs, one per PR; do not re-confirm)
Worktree root: ../task-mgr-worktrees/
Pipeline overrides: phase 2: rerun production-code-architect and md-to-json-prd-reviewer to update the PRD, prompt, and tasks based on the new code / PR-1 implementation; then recreate `feat/agent-task-ops-pr2` off this main and loop `tasks/agent-task-ops-pr2.json`. Open GitHub PRs: no (`#43` merged sticky + PR-1 onto `origin/main`).
Author-ahead: OK
Verification skill: `.claude/skills/verify-task-mgr/SKILL.md`

## Now

Phase: 3 PR-3 export scoped + docs/prompt alignment
State: ticked
Next: goal exit gates — check remaining criteria; no bonus phase
PRD / JSON / prompt: tasks/prd-agent-task-ops-pr3.md / tasks/agent-task-ops-pr3.json / tasks/agent-task-ops-pr3-prompt.md
Architect / JSON-review: tasks/prd-agent-task-ops-pr3-architect.md / tasks/prd-agent-task-ops-pr3-json-review.md
Worktree / prefix / branch: /home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-agent-task-ops-pr3 / c3c1c195 / feat/agent-task-ops-pr3
Live writer id: none
Fix cycle: 1
Last event: 2026-09-18 PR-3 merged locally `78f34d0` (v0.3.3). CODE-FIX-002 dest identity delta-verified. Compound 2d250bf. Not pushed.

## Pins (verbatim — law for every phase)

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

## Simplified shape (do not re-expand)

Three serial PRs, one PRD each: PR-1 remapper + context module + `add --from-json` + `current --from-json`; PR-2 `task-mgr update` + `humanReviewOutcome`; PR-3 export scoped + docs/prompt alignment.

## Goal exit gates

- [x] `task-mgr add --from-json tasks/<prd>.json --stdin` from a linked worktree with untracked-only JSON appends **that** file; main DB has the prefixed row; main JSON bytes unchanged when the worktree copy exists
- [x] `task-mgr current --from-json` prints the write path; unregistered path refuses
- [x] ≥2 prefixes, no env, no `--from-json`: add refuses; `--no-prefix` / 0-prefix add still inserts
- [x] `task-mgr update --stdin` notes-only does not clobber priority/title/`archived_at`; full story blob with `passes` is rejected
- [x] `humanReviewOutcome` survives update + `loop init --append --update-existing` and is absent from `tasks.*` columns
- [x] `export --to-json` defaults to the active PRD; writing onto a registered task-list requires `--force`
- [x] Cheatsheet contains `add --from-json`; loop `task_ops` jq example is `.userStories[]`; JSON-sync failure copy never names `export`

## Phases

| #   | Phase  | PRD            | JSON         | State   | Land | Notes |
| --- | ------ | -------------- | ------------ | ------- | ---- | ----- |
| 1   | PR-1 remapper + context + add/current --from-json | tasks/prd-agent-task-ops-pr1.md | tasks/agent-task-ops-pr1.json | merged | 3fc38b4 / #43 | squash in #43; CODE-FIX-002/003 in squash; verify 2026-09-18 |
| 2   | PR-2 task-mgr update + humanReviewOutcome | tasks/prd-agent-task-ops-pr2.md | tasks/agent-task-ops-pr2.json | ticked | 9447a21 | 15/15; auto-review CLEAN; compound 1d22b1f; local --no-ff |
| 3   | PR-3 export scoped + docs/prompt alignment | tasks/prd-agent-task-ops-pr3.md | tasks/agent-task-ops-pr3.json | ticked | 78f34d0 | CODE-FIX-002 dest identity; compound 2d250bf; v0.3.3; local --no-ff |

States: pending → authoring → authored → looping → reviewing →
fixing(n) → compounding → merging → merged → ticked
Also: paused | skipped (STATUS already Done)

## Pause reason (cleared 2026-09-18)

PR-1 pause is lifted. PR-3 author-ahead unpaused because PR-2 is looping.

Resume map (disk table):
- **PR-1** `merged` (`3fc38b4`, GitHub `#43`). Prefix `a410d276` 19/19 done.
- **PR-2** `looping` prefix `a8855e28` PID 590882 worktree `feat-agent-task-ops-pr2`. Sequential fallback.
- **PR-3** `authored` (`feat/agent-task-ops-pr3` @ `73633f4`). Isolated `loop init` 17/17. Do not loop until PR-2 is merged and post-merge CI is green.

Stop files unused.

## Residuals (human gates — not merge-blocking unless the goal says so)

| ID | What | Owner |
| --- | --- | --- |
| BP | `~/.claude/docs/task-mgr-best-practices.md` is not in this repo; copy spawn-fixup / add / update recipes after merge (CHANGELOG follow-up) | operator after PR-3 |

## Log

- 2026-09-09 goal opened from approved plan; user named 3 PRDs (one per PR); verification skill already in-tree; author-ahead no
- 2026-09-09 phase 1 authored: PRD + JSON + prompt; architect pass 2 APPROVED; JSON review pass 2 nothing material; isolated loop init 16 tasks; branch feat/agent-task-ops-pr1 @ 2ba0a65
- 2026-09-09 phase 1 looping: `task-mgr loop run -y --parallel 2 --hours 12` PID 337573 prefix a410d276 worktree feat-agent-task-ops-pr1 (+ slot-1)
- 2026-09-09 author-ahead OK (operator); phase 2 PRD author starting; no second loop
- 2026-09-09 phase 2 authored: architect pass 2 APPROVED; JSON review pass 2 nothing material; isolated loop init 14 tasks; branch feat/agent-task-ops-pr2 @ 5e7f26e. Loop not started (PR-1 still live).
- 2026-09-09 phase 1 loop complete: 17/17 done, 1h22m, CODE-FIX-001 spawned in-loop; auto-review grok pid 508224; no second /review-loop
- 2026-09-09 author-ahead: phase 3 PRD author starting while auto-review runs
- 2026-09-09 auto-review NEEDS WORK: H1 NULL-prefix 3-segment JSON strip (CODE-FIX-001→FIX-001); M1 skip-note. Spawned CODE-FIX-002/003 via worktree `add --from-json`. Restarted loop PID 563876. No /compound until H1 closed. Findings: tasks/prd-agent-task-ops-pr1-loop-review.md
- 2026-09-09 PR-3 architect pass 2 APPROVED (folded 7 revisions). Operator: pause after this review. Pipeline stopped. PR-1 fix-cycle counts: 19 done (CODE-FIX-002/003 included); delta-verify + /compound + merge not started.
- 2026-09-18 operator resume to finish PR-1: no loop to relaunch (`a410d276` 19/19 done, 0 todo/blocked). Local `main` already squash-merged PR-1 as `3fc38b4` (tree identical to `feat/agent-task-ops-pr1` tip). Independent gate: `commands::add` 52, `commands::context` 27, `commands::prd_json` 6, `add_integration` 23, `cheatsheet_drift` 9, `worktree_db_resolution` 11 — all pass. `cargo install --path .` so PATH `task-mgr` has add/current `--from-json`. verify-task-mgr `add-and-current-from-json`: pin/append, current pin, unregistered/missing/dir refuse, ≥2-prefix unpinned refuse, `--no-prefix` `BARE-001` insert. Artifacts `20260918T162350-414971` + `20260918T162507-422568`. `/compound` not run (no new code this resume). Next: PR-2 loop from a fresh `feat/agent-task-ops-pr2` off this `main`.
- 2026-09-18 operator override: re-author PR-2 before loop. Rerun production-code-architect then fold, then tasks author (JSON+prompt), then md-to-json-prd-reviewer, against PR-1 on `main` (`120c5a2` / squash `3fc38b4` / GitHub `#43`). Then recreate `feat/agent-task-ops-pr2` off this main and loop `tasks/agent-task-ops-pr2.json`. PR-3 stays authoring (folded PRD + architect APPROVED; JSON not written).
- 2026-09-18 PR-2 re-author complete: architect re-pass NEEDS_CHANGES → fold → pass 2 APPROVED; tasks author refreshed JSON+prompt; JSON-review pass 3 five apply warnings folded; pass 4 nothing material. Isolated `loop init` 14/14. Next: recreate `feat/agent-task-ops-pr2` + loop.
- 2026-09-18 C+D: recreated `feat/agent-task-ops-pr2` @ `7528b86` (docs-only authoring commit) worktree `.../task-mgr-worktrees/feat-agent-task-ops-pr2`. `loop init` 14 tasks prefix `a8855e28`. Loop PID 590882 `--parallel 2 --hours 12`; slot worktrees failed (already inside feature worktree) → sequential fallback. Monitor on main `.task-mgr`. Author-ahead: PR-3 tasks author may start.
- 2026-09-18 PR-3 tasks author (`01a0b6fd-d71e-7cd1-b23c-3de6b5e490ce`) wrote `tasks/agent-task-ops-pr3.json` (17 tasks) + prompt. JSON reviewer spawned. PR-2 at that moment: 4 done, FEAT-002 in progress.
- 2026-09-18 PR-3 JSON-review pass 1: 2 criticals (FEAT-001 `export/mod.rs`, FEAT-002 `main.rs`) + 4 apply warnings; folded; pass 2 nothing material. Isolated loop init 17/17 prefix `c3c1c195` (sandbox). Committed `feat/agent-task-ops-pr3` @ `73633f4`. No PR-3 loop.
- 2026-09-18 PR-2 loop complete: 15/15, 16 iterations, 1h8m, tip `e4f2bcd` REVIEW-001. Auto-review grok pid 1019212 (do not start a second /review-loop). Dirty JSON reconcile committed.
- 2026-09-18 PR-2 auto-review CLEAN (0 C/H/M, 3 Low). `/compound` `1d22b1f` (commands/CLAUDE.md gotchas, learning #5761 supersedes #5631, changelog). Independent gate green. Local `--no-ff` merge `9447a21`. Not pushed (goal: no GitHub PR).
- 2026-09-18 PR-3: rebased onto `9447a21` as `2b58dc3`; worktree `feat-agent-task-ops-pr3`; loop init 17 prefix `c3c1c195`; PID 1104024 sequential fallback.
- 2026-09-18 operator `.stop-c3c1c195`: loop finished FEAT-002 then `Exit reason: stop signal` (5 iterations, 21m35s). 4 done / 13 todo / 0 in_progress. Monitor reported FAILED because work remains — expected for a stop, not a crash. Do not relaunch until asked.
- 2026-09-18 operator resume: doctor orphan-only; loop init updated 17; relaunch PID 31090 starting FEAT-003. Sequential fallback expected.
- 2026-09-18 PR-3 loop complete 18/18 then auto-review NEEDS WORK (High dest-cwd). CODE-FIX-002 `d263194` + delta-verify (worktree_export_dest 5/5, export 47, context 29). Compound `2d250bf`. Local merge `78f34d0`; dropped accidental committed `.stop-c3c1c195` as `fed6f46`. v0.3.3. Not pushed. Goal exit gates ticked. Residual BP remains.
