# Loop Review: Quota buckets, remaining headroom, and capability-rung policy (PR-2)

**Worktree:** `/home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-quota-rung-policy-pr2`  
**Branch:** `feat/quota-rung-policy-pr2` (`jq -r .branchName` from `tasks/quota-rung-policy-pr2.json`)  
**Range:** `main...HEAD` — 10 commits (`f6c7c45` … `a45fc73`)  
**Uncommitted:** `tasks/quota-rung-policy-pr2.json` (status-field churn only; not product code)  
**Reviewer:** rust-python-code-reviewer subagent + inline PRD coherence pass  
**Date:** 2026-09-07

## Summary

PR-2 ships the evaluate/apply split, remaining-percent unit, HUD ingest with extra-mark, factory `tierFallback`, proto-channel exclude, and seq/wave sharing of `run_account_quota_gate`. WIRE-FIX-001 (re-ingest with the run `ResolvedModelsConfig`) and CODE-FIX-001 (snapshot honors `runner_overrides`) close the PR-1 pin hole. Production pre-iteration no longer uses `check_and_wait`: `apply_quota` lets account-binding weekly remaining ≤ floor **Proceed** when any other work exists, factory `includeForced: false` plus any `tasks.model` **Defers** the whole loop, `Wait { 0 }` is rewritten to a 300s fallback, and `LOOP_USAGE_CHECK_ENABLED=false` still hits OAuth when Claude is enabled.

**Verdict: NEEDS WORK** — no Critical, four High contract misses that should be fixed before merge. Do not run `/compound` until those are addressed.

---

## Code Review Summary

- **Files reviewed:** 20 product files (`src/loop_engine/*`, `tests/reaction_parity.rs`, `CLAUDE.md`); skill/artifact noise under `.claude/skills/verify-task-mgr` ignored
- **Critical findings:** 0
- **High findings:** 4
- **Medium findings:** 6
- **Low findings:** 5

### Critical

None.

### High

1. **Account-binding weekly remaining ≤ floor with a >12h reset Proceeds if any other work exists**  
   `src/loop_engine/reactions/account.rs:1146-1164`  
   Pre-iteration now runs `evaluate`+`apply`, not `check_and_wait`. Session/weekly `AccountLow` resets are merged into one `latest`, then the >12h band uses `work.other_rungs_runnable` to **Proceed** instead of Stop/Wait. Weekly windows almost always reset in days, so remaining 5% (old used ≥ 95) with standard todos continues burning quota. `compute_remaining_work_snapshot` only subtracts `eval.unavailable` (rung-scoped); weekly-all never lands there, so Claude standard tasks look runnable even when the all-models weekly bucket is empty. Parse-layer inverse (`test_parse_oauth_usage_weekly_all_100_still_gates`) still holds on `UsageInfo`; apply has no matching test (`apply_latest_reset_among_multiple_wait_buckets` sets `other_rungs_runnable: false`). `check_and_wait` (`account.rs:1940-1943`) still waits on remaining ≤ floor, but seq/wave no longer call it.  
   **PRD:** US-001 inverse / FR-004 “account-binding low + reset > 12h + no other runnable rung/**provider** → stop”; architect item 5 (weekly gate must not be dropped). Other Claude rungs share `weekly_all` and are not a working alternative.  
   **Fix:** For account-binding `AccountLow`, >12h → Stop (the anti-5h-cap-loop). Use `other_rungs_runnable` only for **scoped** unavailable (and only count a different *provider*). Add: weekly_all remaining 5%, reset 6d, `other_rungs_runnable: true` → Stop, not Proceed.

2. **Factory `includeForced: false` globally forbids auto-unavailable when any remaining task has `tasks.model`**  
   `src/loop_engine/reactions/account.rs:1032-1041`, `1607-1609`  
   Factory is `{maxDifficulty: high, includeReview: true, includeForced: false}` — that **is** the downgrade instruction. `includeForced: false` is the PR-3 per-task family-match defer, not an opt-out of the whole policy. `compute_remaining_work_snapshot` sets `has_forced` if **any** todo/`in_progress` has a non-empty model column (overflow fallback writes `tasks.model`); `tier_fallback_allows` then returns false → `apply` Defers the whole PRD (`should_stop`) instead of excluding frontier and continuing on standard. Apply unit tests use `RemainingWorkSnapshot::default()` (`has_forced: false`), so this never fires there. Operator copy claims “tierFallback forbade downgrade” even on factory defaults.  
   **Fix:** Do not treat `has_forced && !include_forced` as a global forbid. Leave forced-model tasks selectable (PR-3 walker); factory still marks eligible rungs unavailable.

3. **`Wait { 0 }` (reset already due) is rewritten to `fallback_wait` (300s)**  
   `src/loop_engine/reactions/account.rs:1543-1544` vs `1687-1709`  
   Evaluate emits `reset_secs = 0` for past/now timestamps. Apply maps `secs ≤ 60m` to `Wait { secs }` including 0. Execute then substitutes 300s. `wait_for_usage_reset` already treats 0 as ready-now; `check_and_wait` preserves that.  
   **Fix:** Pass 0 through. Do not treat 0 as unknown.

4. **`LOOP_USAGE_CHECK_ENABLED=false` still hits OAuth when Claude is enabled**  
   `src/loop_engine/iteration.rs:125-144`, `src/loop_engine/wave_orchestration.rs:86-104`, `src/loop_engine/reactions/account.rs:1362-1377`  
   Dual-predicate table (learning 5297/5298): pre-iteration OAuth is `LOOP_USAGE_CHECK_ENABLED ∧ Claude enabled`. Both seq and wave call `run_account_quota_gate` whenever Claude is enabled; `load_usage_info_with_threshold` always runs; `execute_account_action` only skips wait/stop. Post-output dual predicate is unchanged. PRD edge case says evaluate *may* run only post-output when env is false; post-output still uses `check_and_wait`, not evaluate+replace. CLAUDE.md now contradicts itself (dual-predicate table vs “env=false still replaces proto-channel”).  
   **Fix:** Skip the load when `!usage_params.enabled` (keep snapshot), **or** document an intentional exception and also evaluate on the post-output path so proto-channel refresh is not pre-gate-only.

### Medium

1. **Explicit `onLow` wait/stop/ask on a rung-scoped bucket is collapsed to Unavailable**  
   `src/loop_engine/quota.rs:262-268`  
   Evaluate maps `OnLowAction::Wait | Stop | Ask` on rung-scoped buckets to `BucketEval::Unavailable`. Apply’s explicit-rule path only walks `eval.account_low`, so `set-usage-rule --kind weekly_scoped --on-low wait|stop` cannot fire. PRD: “explicit `onLow` on a matching rule wins over the heuristic.” (CLI for that rule is PR-3; the classifier already drops it.)

2. **Remaining banner after the PR-1 pin still uses the builtin ladder**  
   `src/loop_engine/usage.rs:861-867`  
   `format_oauth_remaining_banner` ingests with `builtin_resolved_models()`. Evaluate/apply correctly re-ingest via `buckets_for_run_models`. After `set-tier claude frontier <opus>`, Opus HUD extra-marks frontier+standard in apply, but stderr still says `standard 5% left` not `frontier`. Label-only, not a skip-wait hot-loop.

3. **Horizon Stop is reported as a stop-file**  
   `src/loop_engine/iteration.rs:146-148`, `src/loop_engine/wave_orchestration.rs:106-118`, `src/loop_engine/orchestrator.rs:690-694`  
   `QuotaAccountAction::Stop` and `.stop` during wait both become `UsageCheckResult::StopSignaled`. Sequential Deferred/horizon Stop returns `IterationOutcome::Empty` + `should_stop`, which sets `was_stopped = true` and `exit_reason = "stop signal"`. Operator sees “Stop signal during usage wait” on a 6d frontier-only Stop; batch `--chain` treats `was_stopped` as operator `.stop` and skips remaining PRDs. Account-binding stop stopping the chain is spec; Defer / rung-scoped stop-this-PRD should not look like `.stop`.

4. **No sibling for rung-only empty selection**  
   `src/loop_engine/orchestrator.rs:618-636`, `src/loop_engine/wave_orchestration.rs:222-230`  
   They did **not** reuse `handle_quota_deferral` for rungs (empty blackouts → `Inactive`) — good vs learning 3927. If apply Proceeds and every **todo** is excluded (e.g. snapshot counted `in_progress` as other-rung runnable), the queue falls through to stale-abort. Horizon Stop/Wait covers “nothing else can run” only when the snapshot is honest.

5. **`usagePolicy.askTtlMinutes > 0` is a no-op at execute**  
   `src/loop_engine/reactions/account.rs:1552-1556`  
   Apply emits `Ask` when TTL > 0; execute always maps `Ask` → `Deferred`. Clap `--use-other-models-ttl` is correctly out of PR-2; the config knob is not wired.

6. **Docs still name `account_usage_gate` as the pre-iteration path**  
   `src/loop_engine/config.rs:33-35`, `src/loop_engine/reactions/mod.rs:35`, `src/loop_engine/CLAUDE.md` reactions table (#3 still points at `account_usage_gate` / `iteration.rs:130`)  
   Production seq/wave call `run_account_quota_gate`. `account_usage_gate` remains for parity tests only.

### Low

- `id.contains("REVIEW")` in the snapshot (`account.rs:1611`) matches `PREVIEW-*`; only matters when `includeReview: false`.
- `evaluate_quota` reads `Utc::now()` (`quota.rs:169`) — not I/O, but not clock-pure. Reset math is duplicated in apply (`parse_bucket_reset_secs`).
- `run_account_quota_gate` evaluates/applies twice (work snapshot, then `account_quota_preflight_inner`).
- Remaining banners use `eprintln!` like historical wait banners, not `ui::*` (CONTRACT-LOG-001). Byte-stable wait banners were already stderr.
- Org JSON parse errors are logged unsanitized (`usage.rs` org path); OAuth transport errors go through `sanitize_api_error`. Unlikely to contain tokens.

---

## Coherence Assessment

- **PRD alignment:** PARTIAL (PR-2 slice: FR-003 + FR-004 / US-003 + US-004)
- **Deviations found:**
  - US-003 remaining `% left` banners, remaining-min rename, `LOOP_USAGE_THRESHOLD` hard-break: **satisfied** (parse + banner tests; preflight test).
  - FR-003 HUD table + extra-mark by configured-model **string equality** (`exact_model_for`, not `tier_of` / `model_for` clamp): **satisfied** (WIRE-FIX-001 re-ingest).
  - FR-004 evaluate vs apply split (`evaluate_quota` three-arg, no ask, no `other_rungs_runnable`): **satisfied** at the type boundary.
  - US-004 factory unavailable + exclude-not-account-wait for **scoped** frontier: **satisfied** on the happy path (`apply_factory_rung_low_other_runnable_is_unavailable_not_ask`, proto-channel unit test with empty `provider_blackouts`).
  - US-004 / FR-004 account-binding weekly gate and factory `includeForced` semantics: **not satisfied** (High 1 and 2).
  - Dual predicate unchanged except FR-002 Fable skip: **not satisfied** for pre-iteration (High 4).
  - PR-3 surfaces (`--use-other-models-ttl`, down-only walker, `set-usage-rule` / `set-tier-fallback` CLI, family-match explicit `tasks.model`) were **not** implemented — in scope for this slice.
- **Cross-PRD contract status:**
  - `quota.rs` does not import runners, clap, or SQLite.
  - Proto-channel key is `(Provider, CapabilityTier)`; no model-id literals in `quota.rs` production (self-test + `include_str!` guard).
  - `handle_quota_deferral` is still provider-blackout-only (not reused for rungs).
  - Seq/wave share `run_account_quota_gate` with exhaustive `QuotaPreflightParams` destructure; `tests/reaction_parity.rs` `account_quota_preflight_inner_same_decision_both_shapes` locks that.
  - Absent `routing.tierFallback` → factory `Some`; explicit JSON `null` → `None` (serde tests).
  - Spend stop only at amount ≤ 0; latest reset among wait buckets; severity/`is_active` not default-low; nimbus_quill ignored.

### User-story scorecard (PR-2 only)

| Story | Result |
| --- | --- |
| US-003 Remaining is the unit operators see | Met (banners + remaining-min + legacy env hard-break) |
| US-004 Horizon wait / ask / stop | Partial — scoped factory path works; weekly-all >12h Proceeds; `includeForced` poisons factory unavailable |
| CONTRACT-001 evaluate vs apply | Met at the API; apply horizon band misuses `other_rungs_runnable` for account-binding |
| FR-003 Generic ingest + extra-mark | Met |
| Dual predicate / once-per-wave | Wave once-per-wave met; pre-iteration OAuth env gate regresses |
| Out of scope PR-3 | Correctly omitted |

---

## Parity / wiring notes

**Seq vs wave:** Both call the same `run_account_quota_gate` (`iteration.rs:129-144`, `wave_orchestration.rs:90-104`) with identical `RunAccountQuotaGateParams` (models, policy, `tier_fallback`, `runner_overrides`, `execute_account_action = usage_params.enabled`). Inner `account_quota_preflight_inner` destructures `QuotaPreflightParams` exhaustively. Post-output still shares `react_to_outputs` with the dual predicate (`anthropic_account_io_allowed` = Claude only; `usage_enabled` gates the usage-API leg).

**Ingest → evaluate → apply → proto-channel → excluded ids:**  
`fetch_oauth_usage` stores raw `oauth_json` and a builtin snapshot. `run_account_quota_gate` re-ingests with `buckets_for_run_models(info, params.models)` (WIRE-FIX-001). `evaluate_quota` is three-arg, no ask. `apply_quota` owns ask/wait/stop/unavailable. `replace_unavailable_rungs` on success; API fail keeps the snapshot (`preflight_keeps_snapshot_on_api_fail`). `compute_quota_excluded_ids` does **not** early-return on empty `provider_blackouts` (`pre_spawn.rs:321-324`); unit test pins frontier excluded / standard selectable.

**Held OK (do not re-flag):** extra-mark is configured-model string equality via `exact_model_for`; HUD table is ingest-only; remaining 0–100, never a 0.08 ratio; `LOOP_USAGE_THRESHOLD` hard-error at `preflight_validate_and_probe`; no `--use-other-models-ttl` / `set-usage-rule` / down-only walker; `quota.rs` has no clap/SQLite/runners and no production `unwrap()`.

---

## Action Items

Fix High 1–4 before merge (typical `CODE-FIX` / `WIRE-FIX` via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2.json`). Prefer a human to spawn those; this headless review did not.

Suggested tests that are missing today:

1. `weekly_all` remaining 5 (or 0), reset 6d, `other_rungs_runnable: true` → `QuotaAccountAction::Stop` (not Proceed).
2. Factory `tierFallback` + snapshot with one non-empty `tasks.model` + frontier 5% + standard todos → unavailable+Proceed, not Defer.
3. `Wait { secs: 0 }` → `UsageCheckResult::BelowThreshold` / ready-now, not 300s.
4. `LOOP_USAGE_CHECK_ENABLED=false` + Claude enabled → no `load_usage_info` / OAuth GET at pre-gate (or an explicit, documented exception with a spy).

Do **not** run `/compound` until the review is clean. Capturing learnings from unresolved High items risks baking the weekly-gate Proceed and `includeForced` global-forbid into CLAUDE.md.

---

**Verdict: NEEDS WORK**
