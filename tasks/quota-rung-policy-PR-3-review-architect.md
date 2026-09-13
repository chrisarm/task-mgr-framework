# Architect review: quota-rung-policy-PR-3-review (pre-PR-3 gate)

Date: 2026-09-07
Reviewer: production-code-architect `01a07ca3-90f7-7890-b86e-c625551389d0`
Implementation SSoT: worktree `feat-quota-rung-policy-pr2` (`5a00b15`) / `main` `dacccb3`
PRD under review: `tasks/quota-rung-policy-PR-3-review.md` (sidecar). **Fold target is the parent** `tasks/prd-quota-rung-policy.md`.

**Status**: NEEDS_CHANGES

**Pass**: 1

Product direction is right: gate slice, HUD-family extra-mark is the 1:10 identity, unlabeled `seven_day_*` → `rungs: None`, clamp/TTL/inherit stay PR-3, pin-required docs must be struck. Four Highs would fail independent-ship if implementers follow the sidecar literally (prefer `scope.model.id` over family constant; `Wait { account_binding }` never reaches the probe; drop `extra_usage` from `is_spend_kind` without evaluate Ignore; sequential `OperatorStopped` on `RateLimit` does not set `was_stopped`).

Suggested Revisions 1–9 are folded into `tasks/prd-quota-rung-policy.md` (2026-09-07). Sidecar is superseded as the implementation SSoT.

Full concern/revision text: conversation turn of architect `01a07ca3-90f7-7890-b86e-c625551389d0`.

---

## Pass 2 (2026-09-07)

Reviewer: production-code-architect `01a07cb4-9285-7313-b86c-83e2fb8bb505`
PRD under review: parent `tasks/prd-quota-rung-policy.md` (US-008–US-012 / FR-009–FR-011 / CONTRACT-002). Sidecar superseded.

**Status**: APPROVED

The four pass-1 Highs are in the parent as implementation law. They still match the landed call sites that would fail independent-ship if implementers followed the sidecar. Sidecar is correctly non-law. Clamp/TTL/inherit stay PR-3.

**Strengths**
- Extra-mark is HUD-family **union** (`FABLE_MODEL`/`OPUS_MODEL`/… **plus** snapshot id), not prefer-id and not `exact_model_for(mapped_rung)`. Human-review item 3 is struck.
- Wait-driving probe is preflight-only: post-output `WaitFn` stays; `wait_probe_lifted` after apply; `models` on `QuotaPreflightParams`; mixed session+scoped is `account_binding = true`.
- `extra_usage` Ignore at `evaluate_one` **before** amount-exhausted AccountLow, then drop from `is_spend_kind`.
- Sequential `OperatorStopped` is `Empty` + `operator_stopped: true` → exit 0 / `was_stopped`. Wave StopSpend HorizonStopped-shaped (exit 0, not 130).
- Pin optional for mixed standard/medium; all-high clamp remains PR-3.

**Concerns** (≤ medium leftovers)
1. Public-contracts row for `usage::extra_mark_rungs_matching(..., identity: &str)` still says id **or** family constant. FR-003/US-008 are union.
2. US-010 locks the pure `wait_probe_lifted` function, not that `execute_quota_account_action` / `account_quota_preflight_inner` actually pass `Wait.account_binding` into it. Landed `account_quota_preflight` still builds `usage_suggests_lifted` **before** apply.
3. FR-011 puts `models` on `AccountReactionParams` for `check_and_wait`; landed `UsageGateFn` is still `(u8, &Path, u64)`. Close over `params.models` in `react_to_outputs` — do not widen post-output `WaitFn`.
4. CONTRACT-002 is duplicated in §2.6.

**Questions for User**: none.

**Suggested Revisions** (non-blocking polish)
1. Rewrite the public-contracts extra-mark helper to an identity **set** (or “call once per member of I and union”). Delete the single-`identity` **or** wording.
2. Add one production-wrapper / `_inner` test: scoped-only `Wait { account_binding: false }` + `UsageInfo { percentage: 45, oauth_json: live_shaped }` must not lift. Keep post-output `WaitFn` as `Fn(u64)`.
3. Note that `check_and_wait` gets run models via the `react_to_outputs` closure, not by changing `WaitFn` / `UsageGateFn`.

**Inversion vs landed** (`5a00b15`)
- `usage.rs` extra-mark keys on `exact_model_for(primary)` — Fable HUD + frontier→opus pin extra-marks **standard**. Parent union is the fix.
- `QuotaAccountAction::Wait { secs }` + preflight probe on `UsageInfo.percentage` — scoped 6h Wait lifts on week 45%. Parent `wait_probe_lifted` after apply is the fix.
- `evaluate_one` AccountLow at dollars 0; name-only drop from `is_spend_kind` still Stops. Parent Ignore-at-evaluate is the fix.
- `iteration.rs:874` maps `Stop` → RateLimit / `operator_stopped: false` (exit 1). `wave_scheduler.rs:1101` maps every Stop → exit **130**. Parent Empty mapping + Stop split is the fix.

Do not reopen pass-1 Highs. Do not pull walker / `--use-other-models-ttl` / inherit into this gate.
