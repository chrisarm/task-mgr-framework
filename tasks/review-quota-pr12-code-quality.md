# Code quality review — quota PR-1+PR-2

Reviewer: rust-python-code-reviewer
HEAD: `5a00b15` `feat/quota-rung-policy-pr2` vs `origin/main` merge-base `0dc02f7`
Scope: current tree (CODE-FIX-001–011 + WIRE-FIX-001 landed). No source edits.

## Summary

PR-1 (account-binding fold + Fable 3600 Wait) and PR-2 (remaining unit, `QuotaBucket` / `evaluate_quota` vs `apply_quota`, proto-channel exclude, horizon Stop/Ask/Defer) are structurally sound: `quota.rs` stays pure, remaining is 0–100 not a ratio, `LOOP_USAGE_THRESHOLD` hard-breaks with an actionable message, Wait `{0}` is ready-now, HorizonStopped is not a `.stop` file, and proto-channel replace-on-success / keep-on-API-fail holds. **REQUEST CHANGES.** One High remains: unlabeled named `seven_day_opus` / `seven_day_sonnet` at utilization 100 are family-token-mapped onto standard + cost-efficient; combined with the labeled Fable HUD at 5% left this marks the entire default anchor window unavailable and horizon-Stops a Claude-only remaining queue despite week 45% left. CODE-FIX-006–011 and WIRE-FIX-001 stay closed.

## Findings (numbered, severity, file:line, description, suggestion, status open)

1. **High** — `src/loop_engine/usage.rs:371-378`, `usage.rs:593-612`, `usage.rs:1989-1996`; `src/loop_engine/quota.rs:277-281`; `src/loop_engine/reactions/account.rs:1150-1160`, `account.rs:1240-1243`; `src/loop_engine/model.rs:2053-2055`.
   Unlabeled named siblings `seven_day_opus` / `seven_day_sonnet` are classified `weekly_scoped` and mapped by family-token substring (`"opus"` / `"sonnet"` contained in `exact_model_for` model strings) onto Standard and CostEfficient. The live-shaped fixture sets both at utilization 100 → remaining 0, so `evaluate_quota` emits Unavailable on those rungs. The labeled Fable `limits[]` row at 5% left marks Frontier. Default anchor=standard window is `low→cost-efficient`, `medium→standard`, `high→frontier` — cheapest is never selected. Any remaining Claude-only todo/in_progress set therefore has `other_rungs_runnable=false` and a ~5d scoped reset, so `apply_quota` **Stops**. Account-binding session 76% / week 45% are Ignore (above floor 8). Banner skips unlabeled rows (`usage.rs:993-996`), so stderr shows `session 76% left · week 45% left · frontier 5% left` then the loop horizon-Stops. Recreates the original P0 park on a different path. HUD never showed Opus/Sonnet rows; the live sample comment at `usage.rs:1148` is sonnet utilization **1.0** (1% used), not 100. `ingest_live_fixture_emits_all_siblings_and_limits` **asserts** the Standard/CostEfficient mapping (`usage.rs:2100-2111`), locking the defect in.
   **Suggestion:** attach rungs on named siblings only when a HUD `display_name` / `scope.model` is present (same gate the banner already uses). Keep walking the keys (no allow-list) but leave `rungs: None` so evaluate Ignores them. Extra-mark stays on labeled HUD rows + configured-model string equality. Add `evaluate_quota(ingest(live_shaped))` + `apply_quota` with a medium/high remaining snapshot → Proceed + Frontier-only unavailable.
   **Status:** open (same High as the a4dd3c9 re-review; still unfixed at 5a00b15).

2. **Medium** — `src/loop_engine/reactions/account.rs:1741-1743`.
   `has_review` is `id.contains("REVIEW") || id.contains("CODE-REVIEW")`. The SSoT is `model::is_frontier_class` (`model.rs:240-250`): claimed `CODE-REVIEW-` / `REVIEW-` / `MILESTONE-FINAL`, and **rejects** `REFACTOR-REVIEW-FINAL`. False-negative: remaining `MILESTONE-FINAL` does not set `has_review`, so `includeReview: false` still allows auto-unavailable and the review is proto-channel-excluded instead of Ask/Defer. False-positive: `REFACTOR-REVIEW-FINAL` forbids downgrade and asks. `CODE-REVIEW` is redundant with `contains("REVIEW")`.
   **Suggestion:** `has_review = is_frontier_class(&id)`. Pin `MILESTONE-FINAL` (true) and `REFACTOR-REVIEW-FINAL` (false).
   **Status:** open.

3. **Low** — `src/loop_engine/reactions/account.rs:1524-1535` then `account.rs:1618-1622`.
   `run_account_quota_gate_inner` evaluates+applies once to compute `work` / `horizon_stop`, then `account_quota_preflight_inner` evaluates+applies again and executes. `in_progress` reset is gated on **both** first-apply Stop and result `HorizonStopped`. The two `Utc::now()` calls can theoretically disagree on a horizon boundary; more importantly the first apply is dead weight.
   **Suggestion:** compute work from `evaluate_quota` once, apply once, execute that result, reset `in_progress` from that same Stop.
   **Status:** open.

4. **Low** — `src/loop_engine/reactions/account.rs:1503`, `account.rs:2062`; CONTRACT-LOG-001.
   Remaining banners and wait copy use `eprintln!`, not `ui::*`. Matches historical usage-wait banners (byte-stable stderr) and is not `tracing`, but new operator UX (horizon Stop / ask copy in `iteration.rs:167` / `wave_orchestration.rs:123` correctly use `ui::emit`). Split surface.
   **Status:** open (accepted if wait banners stay eprintln by contract).

5. **Low** — `src/loop_engine/usage.rs:448-468`, `usage.rs:471-497`.
   `MeasurementUnit::Credits` is formatted and apply-tested (`account.rs:3383-3408`) but ingest never reads a `credits` or `remainingAmount` JSON key — only `utilization` / `percent` / `dollars` / `tokens`. A credits-only exhausted object is skipped (no measurement → no bucket). Not a live-HUD bug; the spend fixture is `dollars`.
   **Status:** open.

## Test gaps that hide defects

- No `evaluate_quota(ingest_oauth_value(live_shaped_oauth_json(), builtin), default, 8)` pin. Ingest tests stop at “opus maps to Standard”. Banner tests pin unlabeled skip. Together they hide HorizonStopped on the production-shaped payload.
- No `apply_quota` of that eval with a remaining snapshot of medium+high Claude todos (the default window). Would fail today: `account == Stop`, `unavailable` contains Standard+CostEfficient+Frontier.
- `has_review` has no `MILESTONE-FINAL` / `REFACTOR-REVIEW-FINAL` case. Snapshot tests only seed `pinned-frontier` with difficulty high.
- Post-output `check_and_wait` still prints `info.remaining_banner` (builtin ingest), not `remaining_banner_for_run_models`. CODE-FIX-007 only covers the pre-gate path.
- `when: {severity: critical}` is only a rule filter; a remaining-above-floor critical bucket still Ignores. No test documents that (not raised as a defect — default policy has no rules).

## What looks solid (short)

- `quota.rs` imports only chrono/serde/`Provider`/`CapabilityTier`. No runners/clap/sqlite. No model-id literals in prod. Three-arg `evaluate_quota` (no `other_rungs_runnable`, no Ask).
- Remaining unit 0–100 everywhere; invert is `(100 - used).clamp(0, 100)`; floor compare is `remaining > floor` proceed. Old used≥92 ≡ remaining≤8.
- Explicit `onLow` wait/stop/ask on rung-scoped emits AccountLow (CODE-FIX-006); `Wait { 0 }` is BelowThreshold not 300s (CODE-FIX-004); weekly-all >12h Stops even if other Claude rungs look runnable (CODE-FIX-002); `includeForced: false` is not a global forbid (CODE-FIX-003).
- Extra-mark is `exact_model_for` string equality, not substring `tier_of` / `model_for` clamp (WIRE-FIX-001 / CODE-FIX-007). HUD-only Opus does not mark Frontier without the pin.
- FR-002: hyphen is not a word boundary; `switch models` is same-line; UTF-8 `floor_char_boundary`; 3600 ignores `api_secs`/`output_secs`; load+gate+probe skipped for rung-scoped. Mixed-wave prefers the Fable item.
- Proto-channel on `IterationContext`: replace on successful evaluate, keep on API fail / `LOOP_USAGE_CHECK_ENABLED=false`. Exclusion runs with empty `provider_blackouts`. `handle_rung_only_empty_selection` before `handle_quota_deferral` (CODE-FIX-009). HorizonStopped → `operator_stopped: false` (CODE-FIX-008).
- Config: absent `tierFallback` → factory Some(high / includeReview true / includeForced false); JSON `null` → None. `usagePolicy` camelCase + serde field defaults. `LOOP_USAGE_THRESHOLD` preflight names `LOOP_USAGE_REMAINING_MIN` (`project_config.rs:1065-1070`). Dual predicate: pre = env ∧ Claude; post load ANDs env; probe is Claude-only (CODE-FIX-005).
- Seq and wave share `run_account_quota_gate`. Spend stops at amount ≤ 0, not the percent floor. nimbus_quill / extra_usage percent-only Ignore. Malformed siblings skip.
