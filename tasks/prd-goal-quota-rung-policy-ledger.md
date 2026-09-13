# Goal ledger: quota-rung-policy

Started: 2026-09-06 Source: `/prd-goal tasks/quota-rung-policy.json` (confirm: three serial phases)
Worktree root: ../task-mgr-worktrees/
Pipeline overrides: phase 1: PRD at `tasks/prd-quota-rung-policy.md` — start at architect, then fold, then tasks author emits a PR-1-only JSON. phase 2: same PRD — start at tasks author (PRD already folded); JSON slice CONTRACT-001, FEAT-003–005. phase 2b (operator 2026-09-07 `/prd-goal Pre PR-3 and PR-3`): PRE-PR-3 SSoT is parent `tasks/prd-quota-rung-policy.md` US-008–US-012 / FR-009–FR-011 / CONTRACT-002; sidecar `tasks/quota-rung-policy-PR-3-review.md` superseded; architect notes `tasks/quota-rung-policy-PR-3-review-architect.md` (pass 1 NEEDS_CHANGES folded into parent). Not a fourth product PR. JSON slice `quota-rung-policy-pr2b.json`. phase 3: same parent PRD — start at tasks author; JSON slice FEAT-006–008; do not loop until 2b is merged. Open GitHub PRs: no (local merge onto `main`). askTtlMinutes default: 0.
Author-ahead: no
Verification skill: `.claude/skills/verify-task-mgr/SKILL.md`

## Now

Phase: 3 PR-3 rung blackout + ask TTL + CLI
State: ticked
Next: goal closed — all phases ticked; exit gates checked
PRD / JSON / prompt: tasks/prd-quota-rung-policy.md / tasks/quota-rung-policy-pr3.json / tasks/quota-rung-policy-pr3-prompt.md
Architect / JSON-review: tasks/prd-quota-rung-policy-pr3-architect.md / tasks/prd-quota-rung-policy-pr3-json-review.md
Worktree / prefix / branch: ../task-mgr-worktrees/feat-quota-rung-policy-pr3 / 5d58149d / feat/quota-rung-policy-pr3
Live writer id: none
Fix cycle: 1
Last event: 2026-09-07 PR-3 merged to local main `ff29c5e`. Compound `1740969`. Land-gate `5dffd03`. Not pushed.

## Pins (verbatim — law for every phase)

1. A low **frontier** (HUD: “Current week (Fable)”) bucket is **not** an account emergency — continue on **standard** (and cheaper rungs).
2. Engine language is **capability rungs** (`frontier` / `standard` / `cost-efficient` / `cheapest`), never model ids (`fable`, `opus`, `claude-fable-5`) except at the ingest adapter that maps an API label onto a rung.
3. Default action is a **horizon heuristic** (all config.json-overridable): **wait** if reset is within the next hour; **stop** if reset is more than 12 hours away *and* no other rung can run; **ask** if other rungs still work but no downgrade instruction exists. `--use-other-models-ttl <minutes>` (0 allowed) is how long `ask` waits for a human before continuing on working rungs.

## Simplified shape (do not re-expand)

C, shipped as PR-1 (A) then PR-2 (buckets + heuristic) then PR-3 (rung blackout + `tierFallback` + ask TTL wired to selection).

## Goal exit gates

- [x] Live-shaped fixture (Fable weekly_scoped 95% critical, session 24%, week 55%): gate does not wait (PR-1: used ≈55 < 92; remaining-left banners are PR-2)
- [x] Inverse: weekly-all at 100% still waits on the weekly reset (PR-1)
- [x] Fable CLI text → RateLimit; consecutive-failure / auto-block does not increment (PR-1)
- [x] Operator output is remaining-left, not used-percent (PR-2)
- [x] `quota.rs` / `engine.rs` blackout keys contain no model-id literals (PR-2 proto-channel; PR-3 clamp landed)
- [x] Sequential and wave produce the same account-reaction I/O (PR-1; QuotaDecision is PR-2)
- [x] After PR-3, frontier-low continues on standard without a `set-tier` pin

## Phases

| #   | Phase  | PRD            | JSON         | State   | Land | Notes |
| --- | ------ | -------------- | ------------ | ------- | ---- | ----- |
| 1   | PR-1 account-binding remaining + Fable RateLimit | tasks/prd-quota-rung-policy.md | tasks/quota-rung-policy-pr1.json | ticked | 32f6bbc | Local merge to main. Not pushed (fetch/push SSH denied). |
| 2   | PR-2 generic buckets + remaining rename + horizon | tasks/prd-quota-rung-policy.md | tasks/quota-rung-policy-pr2.json | ticked | dacccb3 | Local merge. Two fix cycles (High then Medium). |
| 2b  | PRE-PR-3 review-fix (HUD-family extra-mark, unlabeled siblings, wait probe, Stop split) | tasks/prd-quota-rung-policy.md | tasks/quota-rung-policy-pr2b.json | ticked | 46cff43 | CLEAN review. Compound a7195d5. Land-gate dropped verify-task-mgr tree. |
| 3   | PR-3 rung blackout + ask TTL + CLI | tasks/prd-quota-rung-policy.md | tasks/quota-rung-policy-pr3.json | ticked | ff29c5e | Local merge. Fix cycle 1 (High+2 Medium). Compound 1740969. Land-gate dropped verify-task-mgr tree. |

States: pending → authoring → authored → looping → reviewing →
fixing(n) → compounding → merging → merged → ticked
Also: paused | skipped (STATUS already Done)

## Human review 2026-09-06 (binding — after architect APPROVED)

Full text: `tasks/prd-quota-rung-policy-human-review.md`

Picked truths:
1. Default auto-downgrade is honest: `tierFallback.maxDifficulty: high`, `includeReview: true`; ask-continue uses the same eligibility as `tierFallback`; if downgrade is forbidden, TTL expiry defers.
2. Horizon middle band (1h–12h): wait, capped at `MAX_WAIT_SECS`; 5h-to-12h is still cap-and-repark.
3. Ingest also marks every rung whose configured model string equals the mapped rung’s model.
4. Off-ladder explicit pins: ingest family match against the explicit model string at resolve time.

## Residuals (human gates — not merge-blocking unless the goal says so)

| ID | What | Owner |
| HR-1 | Default `tierFallback.maxDifficulty: high` + `includeReview: true` (human review item 1) | folded into PRD |
| HR-2 | Horizon middle band wait capped at MAX_WAIT_SECS (item 2) | folded into PRD |
| HR-3 | Extra-mark by mapped-rung model (human-review item 3) | **struck** — PRE-PR-3 HUD-family identity union |
| HR-4 | Explicit off-ladder pins: family match at resolve (item 4) | folded into PRD |
| PR1-PIN | Pin optional after PRE-PR-3 extra-mark + PR-3 down-only clamp | closed |
| L6 | `wait_probe_lifted` vacuous `.all()` on empty nonempty-rungs set | optional fail-closed (learning 5531) |
| L4 | Prefer-rung-scoped can mask StopSpend in mixed waves | PR-2/later; low leftover |
| L5 | `decide_account_rate_limit` rustdoc stale vs spend/output_secs | PR-2 polish |
| PR3-L1 | Unlabeled `family_token_from_id` fallback (review Low) | later |
| PR3-L2 | Ask start banner `eprintln!` vs `ui::emit` (review Low) | later |
| PR3-L3 | Ask TTL silently capped at `MAX_WAIT_SECS` 5h (review Low) | later |
| PR3-L4 | Exclude-site factory “high not excluded” untested (review Low) | later |
| VERIFY-SKILL | verify-task-mgr still untracked (land-gate drop, same as PR-1/2b). Models-routing driven live; evidence `/tmp/pr3-verify-evidence/20260907T132617-2230975`. Full `/maintain-verification-skill` deferred until the skill is in-tree. | operator |
| PUSH | Local `main` is 75 commits ahead of `origin/main`; fetch/push SSH denied. CI not run. | operator |

## Log

- 2026-09-07 `/loop-monitor` already global at `~/.grok/skills/loop-monitor/` (watch-loop.sh + SKILL.md).
- 2026-09-07 delta-verify CODE-FIX-005/006: both wrappers set `account_quota_stopped`; Ask re-eval applies usagePolicy (`ask_wait_mid_on_low_stop_*`). Tests: batch 40, ask_wait 3, execute_ask_ttl 5, reaction_parity 63, loop_engine 2100.
- 2026-09-07 `/compound` `1740969` (gotchas in `src/loop_engine/CLAUDE.md`; changelog PR-3 section). No new learnings (5571/5572/5566 already recorded).
- 2026-09-07 independent gate: fmt 0, clippy -D warnings 0, cargo test green (lib 4266 + no_hardcoded_models 2).
- 2026-09-07 verify-task-mgr models-routing + loop/batch `--use-other-models-ttl` help: ALL CHECKS PASSED. Evidence `/tmp/pr3-verify-evidence/20260907T132617-2230975`.
- 2026-09-07 land-gate `5dffd03` dropped verify-task-mgr tree; kept OPUS_MODEL; no_hardcoded_models still 2 ok.
- 2026-09-07 local merge via temp worktree `ff29c5e` (primary checkout still `feat/quota-rung-policy-pr1`). Phase 3 ticked. Goal exit gate closed. Not pushed.
- 2026-09-07 fix-cycle-1 pid 2177630 EXIT all tasks complete 11/11 tip `c103ae3` (CODE-FIX-006 `2be05cf`, CODE-FIX-005 `c103ae3`). Dirty JSON `passes:` only. Delta-verify vs review High + Medium 1–2; no second `/review-loop`.
- 2026-09-06 goal opened from `tasks/quota-rung-policy.json`; confirm-once presented
- 2026-09-06 user replied `3` → three serial phases (PRD § Phased delivery)
- 2026-09-06 phase 1 authoring; skip PRD author (PRD exists); next = architect
- 2026-09-06 architect `01a07a4a-c6e5-7b60-9d64-f8cada76b04b` → NEEDS_CHANGES; persisted `tasks/prd-quota-rung-policy-architect.md`
- 2026-09-06 paused: unresolved Critical (FR-002 api_secs; early-lift) + High (spillover blackout, 300s fallback, remaining invert, dated parse, PR-2 account-wait, model_for up-walk). Questions for User: none. Fold blocked until human accepts Suggested Revisions.
- 2026-09-06 human accepted all Suggested Revisions
- 2026-09-06 fold writer `01a07a53-3b7e-7e80-a754-74d718238ade` → `prd: tasks/prd-quota-rung-policy.md` folded: yes PAUSE-NEEDED: none. `AA review (folded)` present.
- 2026-09-06 architect pass 2 `01a07a59-2689-7811-99b0-0790b979d1d1` APPROVED; pass-2 verdict folded
- 2026-09-06 tasks author `01a07a5c-9559-7be2-9119-f9e01977b77e` started then **killed**: human review requires items 1–4 in the PRD before `/prd-tasks`. Incomplete `quota-rung-policy-pr1.json` deleted.
- 2026-09-06 human review persisted `tasks/prd-quota-rung-policy-human-review.md`; next = PRD fold of items 1–4 (plus 5–8 + medium so JSON cannot re-encode contradictions)
- 2026-09-06 PRD fold `01a07a64-2c8d-7823-851a-cc3c41a5d170` → folded: yes PAUSE-NEEDED: none. `## Human review (folded)` present. Items 1–4 are spec.
- 2026-09-06 tasks author `01a07a6b-beb3-7d93-816a-d68a1ce97fe5` → `tasks/quota-rung-policy-pr1.json` + prompt
- 2026-09-06 JSON review pass 1 NEEDS_CHANGES (warnings); apply 1/3/4 + S1–S3; pass 2 APPROVED
- 2026-09-06 B.Verify: pins verbatim; `loop init` 4 tasks/17 files/3 rels (isolated /tmp); AA fold + architect + json-review present
- 2026-09-06 C.Handoff: branch `feat/quota-rung-policy-pr1` commit `b8a6f89` (PRD + pr1 JSON + prompt). Ledger/architect/json-review not committed.
- 2026-09-06 D.Loop launched pid 254324 prefix `04391085` `--parallel 1 --hours 12`; iter 1 FIX-001 grok-4.5. Stop: `touch tasks/.stop-04391085`
- 2026-09-07 loop 254324 EXIT max iterations (7); done 5/7 (FIX-001/002, CODE-REVIEW-1, CODE-FIX-001/002). Remaining CODE-FIX-003 (UTF-8 floor) + REVIEW-001. Restart 532796 with LOOP_MAX_ITERATIONS=20.
- 2026-09-07 loop 532796 EXIT all tasks complete 7/7 tip `587e1b1`
- 2026-09-07 `/review-loop` REQUEST CHANGES: High hyphen-as-boundary false 3600; Medium switch-models unanchored; Medium reset_at compile-time 92. Report `tasks/prd-quota-rung-policy-pr1-loop-review.md`. CODE-FIX-004/005/006 spawned; REVIEW-001 reset. Fix-cycle 1 loop pid 874578.
- 2026-09-07 fix-cycle-1 DONE 10/10. Delta-verify: hyphen/line-scope/live-threshold closed. lib 4141 + reaction_parity 58×2 + clippy -D warnings + fmt. Compound `1694ccf`. Local merge to main `32f6bbc`. Phase 1 ticked. Not pushed.
- 2026-09-07 operator override: rerun architect on PRD + PR-3 JSON/prompt grounded in landed main `dacccb3` (PR-1+PR-2). Persist to `tasks/prd-quota-rung-policy-pr3-architect.md`. JSON-apply `01a07c67` not killed (architect is read-only).
- 2026-09-07 JSON-apply `01a07c67` completed: effective_ttl before apply; FEAT-007 stayed one story via `active_rungs` adapter.
- 2026-09-07 architect re-pass `01a07c6c` NEEDS_CHANGES (2 Critical + 3 High + 3 Medium + 1 Low). Report `tasks/prd-quota-rung-policy-pr3-architect.md`. Questions for User: none. Phase paused; fold blocked until human accepts Suggested Revisions.
- 2026-09-07 human accepted PR-3 architect Suggested Revisions; fold writer spawned.
- 2026-09-07 fold `01a07c88` → `prd: tasks/prd-quota-rung-policy.md` folded: yes PAUSE-NEEDED: none. `## Architect re-pass PR-3 (folded)` present.
- 2026-09-07 operator: wait for `quota-rung-policy-PR-3-review.md` to exist and stay stable 60s, then fold those findings into the PRD. Monitor `01a07c8f-b80a-7f20-8e16-1a17693adb76`.
- 2026-09-07 review file stable 60s (`tasks/quota-rung-policy-PR-3-review.md`, 48246 bytes). Operator: PRE-PR-3 effort. Inserted phase 2b; PR-3 paused until 2b merged. Fold parent-spec next.
- 2026-09-07 `/prd-goal Pre PR-3 and PR-3`. Operator: SSoT is parent PRD; sidecar superseded; architect pass 1 NEEDS_CHANGES folded into parent (four Highs). Architect pass 2 on folded parent PRE-PR-3.
- 2026-09-07 architect pass 2 `01a07cb4` APPROVED (≤ medium polish). Fold `01a07cb8`. Tasks author `01a07cbb` → `quota-rung-policy-pr2b.json` (9 stories) + prompt. JSON review APPROVED; should-apply 1–2 applied.
- 2026-09-07 B.Verify: pins verbatim; isolated+worktree `loop init` 9 tasks/40 files/21 rels. C.Handoff worktree `feat-quota-rung-policy-pr2b` commit `8ade945`.
- 2026-09-07 D.Loop pid 1805223 prefix `a593d39e` `--parallel 1 --hours 12` LOOP_MAX_ITERATIONS=20. Stop: `touch tasks/.stop-a593d39e` (worktree or main `tasks/`). PR-3 remains paused until 2b merged. Monitor `01a07ccf-de68-7883-a101-ed95dd18370d`.

