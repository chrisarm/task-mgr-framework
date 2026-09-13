# Loop Review: Quota buckets, remaining headroom, and capability-rung policy (PR-3)

**Worktree:** `/home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-quota-rung-policy-pr3`  
**Branch:** `feat/quota-rung-policy-pr3` (from `tasks/quota-rung-policy-pr3.json`; parent PRD `tasks/prd-quota-rung-policy.md` has no matching `.json`)  
**Range:** `main...HEAD` (12 commits; tip `c7b0d2d`)  
**Uncommitted:** `tasks/quota-rung-policy-pr3.json` `passes: true` flips only — ignored as review noise  
**Reviewer:** `rust-python-code-reviewer` (APPROVE, no Critical/High) + inline PRD coherence pass

## Summary

PR-3 lands the three product slices: Ask TTL CLI on `UsageParams` before `ask_or_defer`, expiry-map proto-channel with a down-only clamp at all three sites, and policy CLI (`set-usage-rule` / `set-tier-fallback` / JSON-null unset). Factory + only-frontier-left is Proceed + clamp, not HorizonStopped; sequential and wave share the inner coordinators and both prompt builders pass `PlanContext.unavailable_rungs`. One High remains: post-output `StopSpend` never sets `account_quota_stopped`, so `batch --chain` can continue after a credits stop when the proto-channel is already populated.

## Code Review Summary

- **Files reviewed:** ~25 production files on `main...HEAD` (account.rs, model.rs, engine.rs, batch.rs, startup.rs, iteration.rs, wave_orchestration.rs, wave_scheduler.rs, orchestrator.rs, prompt/{sequential,slot}.rs, pre_spawn.rs, post_output.rs, models/handlers.rs, cli/commands.rs, plus tests)
- **Critical findings:** 0
- **High findings:** 1
- **Medium findings:** 2
- **Low findings:** 4

### Critical

None.

### High

1. **Post-output `StopSpend` does not set `account_quota_stopped`, so `--chain` can continue after a credits stop**
   - `src/loop_engine/reactions/account.rs:1818` (only writer) vs `StopSpend` at `account.rs:145–150`, `714`, `746`; wrappers `iteration.rs:913–928`, `wave_scheduler.rs:1120–1135`; chain gate `batch.rs:719–732`
   - Apply-layer spend (`QuotaAccountAction::Stop { account_binding: true }`) sets the discriminator. CLI spend/credits RateLimit returns `AccountReaction::StopSpend` and never writes the flag. Sequential/wave both exit 0 with `was_stopped: false`.
   - Chain gate then: abort if `account_quota_stopped || exit_code != 0`; else abort only if `!prd_complete && unavailable_rungs.is_empty()`. Empty proto-channel still aborts. **Non-empty proto-channel (factory frontier-unavailable, or synthetic Fable 3600) + later spend StopSpend continues the next PRD** and seeds inherit — the account is already out of credits.
   - PRD: account-binding Stop aborts the chain; StopSpend comment assumes “`--chain` may still abort on `!prd_complete`”, which is no longer true once inherit treats a non-empty map as rung-scoped continue.
   - Fix: set `ctx.account_quota_stopped = true` on `StopSpend` in both wrappers (or inside `react_to_outputs`), and add a batch-gate test: exit 0, `prd_complete: false`, map non-empty, `account_quota_stopped: true` → abort.

### Medium

1. **Batch tests still encode the pre-PR-3 chain-break predicate**
   - `src/loop_engine/batch.rs:1269–1276` (`chain_break = chain && (exit_code != 0 || !prd_complete)`), used at `1339–1341` and `1400`
   - Comment claims this is “the exact chain-break predicate from `run_batch`”. Production is the discriminator at `719–732`. No unit test locks: incomplete + non-empty `unavailable_rungs` + `account_quota_stopped: false` → continue and seed.
   - Fix: delete or rewrite `chain_break` to match production; add inherit-continue and StopSpend-abort cases.

2. **Ask wait re-reads `usagePolicy` but does not apply it**
   - `src/loop_engine/reactions/account.rs:2287–2293`
   - AC: re-read `usagePolicy` + `routing.tierFallback` on the stop-check cadence; “if they set `tierFallback` **or a usage rule**, the next stop-check uses it.” Continue/defer uses only `tier_fallback_allows`. Mid-wait `onLow: stop` is ignored (`let _ = &slice.usage_policy`).
   - Fix: re-run evaluate/apply on the re-read slice, or document that only `tierFallback` eligibility is live during Ask.

### Low

1. **Unlabeled family-match fallback still uses `family_token_from_id`**
   - `src/loop_engine/model.rs:1012–1023`
   - PRD: do **not** use `family_token_from_id` (underscore rsplit). Primary path is `hud_tier_from_label` (covers `claude-fable-5-1`). Fallback is `family_token_from_id` + `map_unlabeled_token` substring + `.first()` (cheapest-first). Vague `tasks.model` without a HUD token can mis-map.
   - Fix: if `hud_tier_from_label` is `None`, return `None` and keep `anchored_tier` as last resort.

2. **Ask start banner uses `eprintln!`, Deferred uses `ui::emit`**
   - `src/loop_engine/reactions/account.rs:2194–2197` vs `iteration.rs:191` / `wave_orchestration.rs:145`
   - CONTRACT-LOG-001: `ui::*` for operator lines. Historical wait banners in this module are `eprintln!`; new Ask banner follows that, not the CODE-FIX-002 helper.
   - Fix: route `emit_ask_banner` through `ui::emit`.

3. **Ask TTL is silently capped at `MAX_WAIT_SECS` (5h)**
   - `src/loop_engine/reactions/account.rs:1196`, `2278`
   - `--use-other-models-ttl 400` waits 5h, not 400 minutes. Usage waits already cap; Ask TTL is specified as the operator’s minutes.
   - Fix: cap Ask independently, or log the cap the way `wait_for_usage_reset_inner` does.

4. **Exclude unit tests pass `tier_fallback: None`, so they do not lock factory non-exclude of clamp-eligible high tasks**
   - `src/loop_engine/reactions/pre_spawn.rs:462`, `479`, `523`
   - Production seq/wave pass `project_config.routing.tier_fallback` (`iteration.rs:288`, `wave_scheduler.rs:802`). Snapshot factory AC is tested (`account.rs:5395`). Exclude-site factory “high is not excluded” is not.
   - Fix: one `compute_quota_excluded_ids` test with factory `tierFallback` + frontier unavailable asserting `t-frontier` is **not** excluded.

## Coherence Assessment

- **PRD alignment:** PARTIAL (PR-3 US-005/006/007 mostly satisfied; inherit discriminator incomplete for post-output spend)
- **Deviations found:**
  - StopSpend vs chain inherit (High above)
  - `family_token_from_id` residual (Low; AC example still works via HUD)
  - Ask mid-wait ignores usage rules (Medium)
- **Cross-PRD contract status:**
  - CONTRACT-002 extra-mark is not reintroduced in the walker (`exact_model_for` after HUD-family identity; no `exact_model_for(mapped_rung)`)
  - `quota.rs` / `engine.rs` have no `fable` / `opus` literals
  - Dual Anthropic predicates unchanged
  - `handle_rung_only_empty_selection` still sits in front of `handle_quota_deferral` on both paths
  - `unset-tier-fallback` writes JSON `null`; offline `models show` has no `% left`; remaining numbers gated on `check_opt_in()` / `TASK_MGR_USE_API=1`

### PR-3 story spot-check

| Story | Status |
| --- | --- |
| US-005 Ask TTL CLI | **Satisfied.** Nested + flat + batch clap; `Some(0)` ≠ omitted; `startup.rs:1010` → `UsageParams.ask_ttl_override`; `effective_ttl` before `ask_or_defer`; TTL 0 = Defer no sleep; timeout ≠ `was_stopped`; seq/wave share `deferred_ask_stop_banner`. |
| US-006 expiry + three clamp sites + inherit | **Mostly satisfied.** Snapshot factory clamp test green; exclude + spawn use post-clamp `resolve_execution_plan`; walker is down-only `exact_model_for`; family-match via `hud_tier_from_label`; orchestrator seeds inherit. **Gap:** StopSpend inherit (High). |
| US-007 policy CLI | **Satisfied.** `set-usage-rule` / `set-tier-fallback` / JSON-null unset; sparse Value round-trip; `models show` policy from config; remaining live-fetch-gated. |

### Sequential vs wave

No load-bearing divergence on TTL, clamp threaders, Deferred banners, or Stop split mappings. Both call `run_account_quota_gate` with `ask_ttl_override` + `account_quota_stopped`. Both pass `active_rungs` + `tier_fallback` into `PlanContext`. Both call `handle_rung_only_empty_selection` before `handle_quota_deferral`. The StopSpend inherit hole is **shared** (neither wrapper sets the flag).

## Action Items

- Do **not** spawn CODE-FIX from this review (High is not trivial; user asked to document for a human).
- Human: set `account_quota_stopped` on `StopSpend`, lock the batch discriminator with inherit-continue vs spend-abort tests, then re-run this review.
- Optional: drop `family_token_from_id` fallback; apply or document Ask `usagePolicy` re-read; `ui::emit` for Ask banner.
- Do **not** run `/compound` until the High is addressed (or an operator explicitly accepts it).

## Verdict

**NEEDS WORK**
