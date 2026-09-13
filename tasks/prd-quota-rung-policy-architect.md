# Architect review: prd-quota-rung-policy (phase 1)

PRD: `tasks/prd-quota-rung-policy.md`

## Pass 2 (folded PRD) — APPROVED

Date: 2026-09-06
Reviewer: production-code-architect `01a07a59-2689-7811-99b0-0790b979d1d1` (resume of `01a07a4a-c6e5-7b60-9d64-f8cada76b04b`)

**Status**: APPROVED

**Pass**: 2 (folded PRD)

**Strengths**: The twelve pass-1 items are now **spec**, not only the `## AA review` table. FR-001/US-001 keep used-percent and drop `seven_day_opus`/`seven_day_sonnet`. FR-002/US-002 is a coordinator contract with tests (a)–(d), including `api_secs = 6 days`. Pins, three-PR shape, and “PR-1 + pin, no auto-downgrade” are consistent. Live code still matches the holes that contract closes: `resolve_wait_secs` prefers API (`account.rs` ~200–205), spillover → `Blackout` (~224–227), non-spillover fallback is `usage_fallback_wait` 300 not 3600 (~229–230), `react_to_outputs_with_io_seams` always loads `api_secs` and wires `usage_gate` + `probe_rate_limit_lifted` (~349–385), `is_rate_limited` still misses the Fable sentence (`detection.rs` ~151–166), RateLimit is already excluded from `handle_task_failure` (`orchestrator.rs` ~586–596).

**Concerns**: All 12 are **closed** in FR/US/contracts/non-goals.

Leftovers (none ≥ high; do not block tasks author):

- **Medium:** Key the 3600 Wait on a **narrow** Fable/rung-scoped predicate (`fable limit` or `reached your`∧`limit`), not on every `RateLimit`. Account copy still uses `hit your` + parsed reset / spillover. Worth a negative test (`You've hit your limit · resets 4pm` still uses `output_secs` / may Blackout). Spec already says this; implementers can still over-apply.
- **Low:** `reached your`∧`limit` is broader than Fable. Live account messages use `hit your`. Acceptable for PR-1.

**Questions for User**: none.

**PR-1 independent-ship check**:

| After PR-1 alone | True? |
| --- | --- |
| False account park on Fable weekly 95% / session 24% / week 55% is gone (account-binding used 55% < 92) | **Yes** |
| Inverse: weekly-all 100% still waits on the weekly reset | **Yes** |
| Fable CLI is `RateLimit`, not Crash; consecutive-failure / auto-block do not increment | **Yes** |
| That RateLimit is `Wait { blackout_fallback_secs }` (3600), ignoring api/output, no Blackout, no probe/usage_gate, no dated Sep 12 parse | **Yes** |
| Automatic frontier→standard | **No** — pin `models set-tier claude frontier <standard-model>` until PR-3 |
| Remaining `% left` banners, horizon stop/ask, `--use-other-models-ttl`, rung clamp | **No** — PR-2 / PR-3 |
| `mw_integrations` unsticks | **PR-1 + the pin**, not PR-1 alone |

No unresolved ≥ high. Parent can continue to the tasks author.

---

## Pass 1 (historical) — NEEDS_CHANGES

Date: 2026-09-06
Reviewer: production-code-architect `01a07a4a-c6e5-7b60-9d64-f8cada76b04b`

**Status**: NEEDS_CHANGES

The three-PR split, rung language, and “do not fold `weekly_scoped` into the account wait” are sound. PR-1 is *not* independently shippable as written: FR-002 collides with the live post-output coordinator (`api_secs` wins, spillover blacks the whole provider, early-lift probe can return in 30s). Those holes recreate a park or a hot-loop after the parse fix.

**Strengths**:
- Pins are honored: frontier-low is not an account emergency; engine state is `(Provider, CapabilityTier)`; horizon wait/ask/stop stays config-overridable.
- PR-1 slice (FR-001 + FR-002) is the right first cut and correctly does **not** wait on CONTRACT-001 or claim automatic frontier→standard.
- Account-binding allow-list (`five_hour` / `seven_day` + `limits[]` `session`/`weekly_all`) matches live OAuth JSON; excluding `severity`/`is_active`/`weekly_scoped`/`extra_usage` is the actual bug.
- “Latest reset among low **wait** buckets,” spend-stop only at amount 0, dual Anthropic I/O predicates, once-per-wave, and a sibling of `handle_quota_deferral` (learning 3927) are the right invariants.
- Coupling budget is right: ingest in `usage.rs`, pure policy in `quota.rs`, apply in `account.rs`, clamp in `model.rs`, ephemeral channel on `IterationContext`.
- FEAT-008 `provider_blackouts` stays provider-keyed; rung unavailability is a new channel. That split is load-bearing.

**Concerns**:

1. **Critical — FR-002 vs live `react_to_outputs` / `decide_account_rate_limit`.** After FR-001, `load_usage_info()` still returns a session `reset_at` (~5h). Production always loads that as `api_secs` (`account.rs` ~349–355). `resolve_wait_secs` prefers API over CLI. Fable phrasing therefore waits the **session** window, not 3600s, unless api_secs is ignored for that phrasing. FR-002 only says “even if a dated weekly reset is parseable” (output path). The live winner is the usage API. Validation “3600 not ~6 days” can pass with `api_secs=None` and still fail in production.

2. **Critical — early-lift probe undoes the 3600s backoff.** `wait_for_usage_reset` probes every 30s. `probe_rate_limit_lifted` spawns `claude -p .` with **no `-m`**. If the CLI default is not Fable, `!is_rate_limited` → lift after 30s → same frontier task again. `usage_suggests_lifted` is unused on that probe, but the usage-gate leg (`check_and_wait`) now sees account used 55% < 92 and returns `BelowThreshold` immediately — it does not hold the wait. PR-1 must disable both the Claude CLI early-lift probe **and** any usage-API “already below threshold” short-circuit for Fable/rung-scoped CLI text.

3. **High — Fable RateLimit + FEAT-008 spillover blacks the whole Claude provider.** `decide_account_rate_limit` with `spillover_enabled` returns `RateLimitAction::Blackout` for every RateLimit. Pin 1 (“continue on standard”) is then violated: standard/cost-efficient Claude work is excluded and `handle_quota_deferral` may wait. FR-002 says “waits 3600,” which is the **Wait** path, not provider Blackout. Must pin: rung-scoped CLI phrasing never records `provider_blackouts`.

4. **High — FR-002 3600 is not the non-spillover fallback.** Without spillover, unknown reset uses `usage_fallback_wait` (**300s**), not `blackoutFallbackSecs` (3600). Ignoring api/output and falling through the existing `else` branch waits 5 minutes. Spec must say: Fable phrasing → `Wait { secs: blackout_fallback_secs }` even when spillover is off; do not use `fallback_wait`.

5. **High — US-001 remaining language in PR-1 will invert the gate.** Pipeline: remaining rename is PR-2. Live compare is `usage.percentage < threshold` (used ≥ 92 waits). If PR-1 stores remaining 45 in `percentage` and does **not** flip the compare, the live fixture luckily passes (45 < 92) and the inverse **fails** (weekly-all remaining 0 < 92 → no wait). Pin PR-1: keep used-percent + `>= usage_threshold`; only change **which windows enter the fold**. Inverse fixture in used-percent: `percentage == 100`, `reset_at` = weekly.

6. **High — dated `parse_reset_from_output("Sep 12, 12:59am")` in PR-1 is a landmine.** Today the token is `"sep"` → `None`. Adding a date parser in the same PR as Fable RateLimit recreates the 6-day wait unless api_secs **and** output_secs are both overridden. Defer dated parse to PR-2/PR-3. PR-1: keep `None` for month-name tokens; Fable 3600 comes from the phrasing override, not from parsing Sep 12.

7. **High — PR-2 “unavailable/ask back off 3600s” must not be account-global wait.** If it is `check_and_wait`/`Wait { 3600 }`, standard work parks for an hour — pin 1 again. Required: skip/exclude tasks whose resolved rung is unavailable; other rungs proceed immediately; 3600s only if **no** remaining work can run. A process-local `HashSet<(Provider, CapabilityTier)>` without expiry is enough until PR-3 adds expiry + clamp. Do not reuse `handle_quota_deferral`.

8. **High — PR-3 clamp must not reuse `ResolvedModelsConfig::model_for`.** `model_for` is bidirectional nearest-defined (down, then up). Walking up can land on a blacked frontier. FR-006 already says walk **down** defined non-blacked rungs; write that as a new helper. Post-resolve clamp after all six rungs (including `EXPLICIT_MODEL`); default `includeForced=false` defers pinned off-ladder ids.

9. **Medium — `evaluate_quota(..., other_rungs_runnable: bool)` cannot express the heuristic.** “`tierFallback` would accept some remaining work” needs the task list, `maxDifficulty`, `includeReview`/`includeForced`. Keep `evaluate_quota` per-bucket (`wait`/`unavailable`/`stop`/`ask`/`ignore`); apply layer in `account.rs` combines remaining work + `tierFallback`. Specify product semantics: account-binding `wait`/`stop` plus rung `unavailable` can coexist; `stop` beats `ask`.

10. **Medium — ingest mapping when two rungs share a model.** After `set-tier claude frontier <standard-model>`, substring `(1)` matches both rungs for an Opus HUD row. Pin: HUD label table wins for `display_name`; a shared binary model does **not** mark every matching rung unavailable. `tier_of` stays exact-match.

11. **Medium — `is_rate_limited("switch models with /model")` as an independent match.** Help text / docs could false-RateLimit and skip auto-block. Match `reached your` ∧ `limit`, or `fable limit`. Do not treat `/model` alone as sufficient.

12. **Low — `askTtlMinutes` still an open question.** Phase seed already pins default **0**. Close §7.

**Questions for User**: none. Pins, phase seed, and live coordinator behavior are enough to revise the PRD without an interview.

**Suggested Revisions** (PRD edits, not code):

- **FR-001 / US-001 (PR-1):** Keep `UsageInfo.percentage` as used 0–100. Fold only account-binding windows. Live fixture: `percentage ≈ 55`, `reset_at` = session (nothing ≥ 92). Inverse: weekly-all 100 → `percentage = 100`, `reset_at` = weekly. When several account-binding windows are ≥ `usage_threshold`, `reset_at` is the **latest** of those. `severity`/`is_active` do not set exhausted. Drop remaining-min wording from US-001. Also drop named `seven_day_opus` / `seven_day_sonnet` from the fold (already implied; say it so `usage.rs:218–223` is updated).

- **FR-002 (PR-1), replace the wait sentence with a coordinator contract:** If `is_rate_limited` matches Fable/rung-scoped CLI text (`You've reached your Fable limit` / `reached your`+`limit`):
  1. Outcome is `RateLimit` (already excluded from `handle_task_failure` at both callers — that AC is already true once classified).
  2. `decide_account_rate_limit` returns `Wait { secs: blackout_fallback_secs }` (default 3600), **ignoring `api_secs` and `output_secs`**, even if spillover is enabled (no `Blackout`, no `provider_blackouts.record`).
  3. `react_to_outputs` must not run `usage_gate` / `probe_rate_limit_lifted` for that phrasing; sleep is stop-signal-aware only.
  4. Do **not** add dated `Sep 12` parsing in PR-1.
  5. Tests: (a) detection of the live sentence; (b) `api_secs = 6 days` + Fable output → wait 3600, not Blackout; (c) `spillover_enabled = true` → still Wait, blackout map empty; (d) probe/usage_gate not invoked.

- **US-003 / FR-008 remaining banners:** stay PR-2. PR-1 stderr may still print `Usage: 55.0% (threshold: 92%)`. Strike PR-1 from the “76% left” success metric.

- **PR-2 backoff:** “exclude unavailable rungs from the next selection; do not account-wait; 3600s only when the remaining queue cannot run.” Proto-channel on `IterationContext` is allowed; full expiry + `resolve_execution_plan` clamp stay PR-3.

- **FR-004:** split evaluate vs apply; pin `askTtlMinutes` default 0 in §7.

- **FR-006:** new down-only walker; do not call `model_for` for blackout clamp.

- **Operator recipe (PR-1 ship note):** after merge, `task-mgr models set-tier claude frontier <standard-model>` until PR-3. Without that pin, a Fable CLI hit still account-waits 3600s (no auto-downgrade). That is accepted, not a bug.

**Inversion**:

| Failure | Closed by PRD? | Still open |
| --- | --- | --- |
| Max-fold `weekly_scoped` + `critical` → 6-day 5h-cap park | FR-001 | — |
| Fable CLI missed → Crash → auto-block | FR-002 detection | — |
| RateLimit waits API/`reset_at` (session 5h or weekly 6d) | Partial (dated output only) | **api_secs wins — Critical** |
| Early-lift / usage-gate short-circuit → 30s Fable retry | No | **Critical** |
| Spillover Blackout of whole Claude on Fable CLI | “must not break FEAT-008” | **High** |
| Unknown-reset uses 300s not 3600 | “blackoutFallbackSecs” named, not wired | **High** |
| Remaining invert in PR-1 → weekly-all 100% never waits | Phase split | **US-001 wording invites it** |
| PR-2 skip-wait hot-loop same frontier task | Noted; 3600s backoff | **Backoff must not park standard** |
| Rung deferral through `handle_quota_deferral` → stale-abort | Explicit sibling | — |
| Engine keys on `fable` / substring `tier_of` | Pins + non-goals | Ingest multi-rung share needs a pin |
| `model_for` walks **up** onto blacked frontier | “walk down” | Implementer reuse risk |
| Soonest reset among two low account windows | Latest | — |
| `severity`/`is_active` as default-low | Explicit | — |
| Unattended `ask` sleeps | Pipeline default 0 | PRD §7 still open |
| Dual-predicate collapse | FR-007 | — |

**PR-1 independent-ship check** (must match the PRD after the revisions above):

| After PR-1 alone | True? |
| --- | --- |
| False account park on Fable weekly 95% / session 24% / week 55% is gone (gate uses account-binding used 55% < 92) | **Yes** |
| Inverse: weekly-all 100% still waits on the weekly reset | **Yes** |
| Fable CLI is `RateLimit`, not Crash; consecutive-failure / auto-block do not increment | **Yes** |
| That RateLimit backs off **3600s** (not Sep 12, not session 5h, not 300s, not 30s probe-lift, not provider blackout) | **Yes, only if FR-002 coordinator contract is added** |
| Automatic frontier→standard | **No** — operators pin `models set-tier claude frontier` to the standard model until PR-3 |
| Remaining `% left` banners, horizon stop/ask, `--use-other-models-ttl`, rung channel | **No** — PR-2 / PR-3 |
| `mw_integrations` unsticks | **PR-1 + the pin**, not PR-1 alone |

Do not fold. Revise FR-001/US-001 units and the FR-002 coordinator contract, then this slice is APPROVE-able.
