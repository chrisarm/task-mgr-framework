# Goal ledger: agent-task-ops-ux

Started: 2026-09-09 Source: `/prd-goal` on plan `agent task-ops UX: make add / update / export match the docs` (user: 3 PRDs, one per PR; do not re-confirm)
Worktree root: ../task-mgr-worktrees/
Pipeline overrides: none. Open GitHub PRs: no (local merge onto `main`).
Author-ahead: OK
Verification skill: `.claude/skills/verify-task-mgr/SKILL.md`

## Now

Phase: 1 PR-1 remapper + context + add/current --from-json
State: paused
Next: operator resume — see pause reason
PRD / JSON / prompt: tasks/prd-agent-task-ops-pr1.md / tasks/agent-task-ops-pr1.json / tasks/agent-task-ops-pr1-prompt.md
Architect / JSON-review: tasks/prd-agent-task-ops-pr1-architect.md / tasks/prd-agent-task-ops-pr1-json-review.md
Worktree / prefix / branch: ../task-mgr-worktrees/feat-agent-task-ops-pr1 / a410d276 / feat/agent-task-ops-pr1
Live writer id: none
Fix cycle: 1
Last event: 2026-09-09 operator pause after PR-3 architect pass 2 APPROVED. PR-1 fix-cycle loop PID 563876 exited with all 19 done (not reviewed/compounded). No second loop. No /prd-tasks for PR-3.
Author-ahead track:
Phase: 2 PR-2 task-mgr update + humanReviewOutcome
State: authored
Next: D. Execute — after phase 1 merged (no second loop)
PRD / JSON / prompt: tasks/prd-agent-task-ops-pr2.md / tasks/agent-task-ops-pr2.json / tasks/agent-task-ops-pr2-prompt.md
Architect / JSON-review: tasks/prd-agent-task-ops-pr2-architect.md / tasks/prd-agent-task-ops-pr2-json-review.md
Live writer id: none

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

- [ ] `task-mgr add --from-json tasks/<prd>.json --stdin` from a linked worktree with untracked-only JSON appends **that** file; main DB has the prefixed row; main JSON bytes unchanged when the worktree copy exists
- [ ] `task-mgr current --from-json` prints the write path; unregistered path refuses
- [ ] ≥2 prefixes, no env, no `--from-json`: add refuses; `--no-prefix` / 0-prefix add still inserts
- [ ] `task-mgr update --stdin` notes-only does not clobber priority/title/`archived_at`; full story blob with `passes` is rejected
- [ ] `humanReviewOutcome` survives update + `loop init --append --update-existing` and is absent from `tasks.*` columns
- [ ] `export --to-json` defaults to the active PRD; writing onto a registered task-list requires `--force`
- [ ] Cheatsheet contains `add --from-json`; loop `task_ops` jq example is `.userStories[]`; JSON-sync failure copy never names `export`

## Phases

| #   | Phase  | PRD            | JSON         | State   | Land | Notes |
| --- | ------ | -------------- | ------------ | ------- | ---- | ----- |
| 1   | PR-1 remapper + context + add/current --from-json | tasks/prd-agent-task-ops-pr1.md | tasks/agent-task-ops-pr1.json | fixing(1) | —    | H1 CODE-FIX-002/003; loop PID 563876 |
| 2   | PR-2 task-mgr update + humanReviewOutcome | tasks/prd-agent-task-ops-pr2.md | tasks/agent-task-ops-pr2.json | authored | —    | handoff 5e7f26e; wait for PR-1 merge |
| 3   | PR-3 export scoped + docs/prompt alignment | tasks/prd-agent-task-ops-pr3.md | tasks/agent-task-ops-pr3.json | authoring | —    | PRD folded; architect pass 2 APPROVED; JSON not written (paused) |

States: pending → authoring → authored → looping → reviewing →
fixing(n) → compounding → merging → merged → ticked
Also: paused | skipped (STATUS already Done)

## Pause reason (2026-09-09)

Operator: “pause after this review finishes” (PR-3 architect pass 2). Do not continue until unpaused.

Resume map (disk table):
- **PR-1** `fixing(1)` counts clean (19 done) but **not** delta-verified / compounded / merged. Next: D. Review (delta-verify CODE-FIX-002/003 vs H1/M1) → `/compound` → independent gate → merge. Auto-review of the first loop: `tasks/prd-agent-task-ops-pr1-loop-review.md`.
- **PR-2** `authored` (`feat/agent-task-ops-pr2` @ `5e7f26e`). Next: D. Loop **after PR-1 merge**.
- **PR-3** `authoring` with folded PRD + architect APPROVED. Next: A. Author — Tasks author (`/prd-tasks`), then JSON reviewer. No JSON/prompt yet.

No live loop PID. Stop files unused.

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
