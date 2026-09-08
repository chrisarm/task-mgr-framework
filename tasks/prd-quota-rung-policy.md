# PRD: Quota buckets, remaining headroom, and capability-rung policy

**Type**: Bug Fix + Enhancement
**Priority**: P0 (Critical) — live loops park for days on a Fable weekly bucket while session and other rungs have headroom
**Author**: Grok
**Created**: 2026-09-06
**Status**: Draft

> **Design context.** Approved plan: session plan `01a07812-5e52-7bd0-8f51-c7981f49a56d` (`plan.md`). Operator review on that plan is binding and is folded in here:
>
> 1. A low **frontier** (HUD: “Current week (Fable)”) bucket is **not** an account emergency — continue on **standard** (and cheaper rungs).
> 2. Engine language is **capability rungs** (`frontier` / `standard` / `cost-efficient` / `cheapest`), never model ids (`fable`, `opus`, `claude-fable-5`) except at the ingest adapter that maps an API label onto a rung.
> 3. Default action is a **horizon heuristic** (all config.json-overridable): **wait** if reset is within the next hour; **stop** if reset is more than 12 hours away *and* no other rung can run; **ask** if other rungs still work but no downgrade instruction exists. `--use-other-models-ttl <minutes>` (0 allowed) is how long `ask` waits for a human before continuing on working rungs.

Factory default (does **not** rewrite pin 3): `routing.tierFallback.maxDifficulty: high` and `includeReview: true` **are** the downgrade instruction. Factory/allowing `tierFallback` continues via PR-2 `unavailable + Proceed` (**never Ask**). Pin 3’s “no instruction → ask” path is the **opt-out** (JSON null / `unset-tier-fallback` / narrower `maxDifficulty` / `includeReview: false`, or explicit `onLow: ask`). Default `includeForced` is **false**. Ask-path TTL 0 = **Defer, no sleep** — not “continue on working rungs.” CLI `--use-other-models-ttl` overrides `policy.ask_ttl_minutes` **before** `ask_or_defer`.

Ship in **three PRs** (plan § Implementation order) plus a **PRE-PR-3 gate (2b)** between PR-2 and PR-3. That gate is **not a fourth product PR**. This PRD is the full vision **and** the PRE-PR-3 implementation SSoT (US-008–US-012 / FR-009–FR-011 / CONTRACT-002). `/prd-tasks` must keep the PR-1 slice independently shippable. Sidecar `tasks/quota-rung-policy-PR-3-review.md` is superseded working notes. Do not turn PRE-PR-3 stories into US-005 / US-006 / US-007 (those stay PR-3).

**Phase 1 / `/prd-tasks` slice:** ship **only PR-1** (FR-001 + FR-002). Remaining `% left` banners, horizon heuristic, ask TTL, and rung blackout stay PR-2 / PRE-PR-3 / PR-3 as listed in the Appendix. Do not invent a fourth product PR.

**Operator pin recipe (timeline).** Human-review item 3 / parent FR-003 extra-mark-as-mapped-rung is **struck** (PRE-PR-3 FR-001). Until CONTRACT-002 lands, the documented `models set-tier claude frontier <standard-model>` pin is **harmful** (Fable HUD extra-marks standard). **Safe recipe: do not pin.** After PRE-PR-3: pin is **optional, not required**. Factory exclude unsticks mixed standard work. Automatic clamp of all-high / review onto standard is still **PR-3**. Fable CLI RateLimit still sleeps the wave 3600s if a Fable-routed task actually spawns (`LOOP_USAGE_CHECK_ENABLED=false`, fetch fail, explicit `tasks.model`).

---

## 1. Overview

### Problem Statement

`parse_oauth_usage_json` (`src/loop_engine/usage.rs`) folds **every** utilization window — including named `seven_day_opus` / `seven_day_sonnet` and `limits[].kind = weekly_scoped` (Fable) with `severity: critical` at 95% used — into one `UsageInfo.percentage = max(used)` and one `reset_at`. The pre-iteration gate then waits whenever that max is ≥ `usage_threshold` (92).

Live HUD / `GET /api/oauth/usage` (2026-09-06, this account):

| HUD row | API | Used | Remaining | Reset |
| --- | --- | --- | --- | --- |
| Current session | `five_hour` / `kind=session` | 24% | **76% left** | ~5h |
| Current week (all models) | `seven_day` / `kind=weekly_all` | 55% | **45% left** | Sep 12 |
| Current week (Fable) | `limits[]` `kind=weekly_scoped` `display_name=Fable` | 95% | **5% left** | Sep 12 |

The loop treats “5% frontier left” as “account 95% used, wait until Sep 12” (capped at 5h, then repeats). `mw_integrations` `onboarding-state-class-close` is stuck on that path. Session and **standard** / **cost-efficient** still have headroom. Anthropic’s own product: when the Fable weekly bucket is gone, **switch models** — do not sit out the session.

A second bug: `"You've reached your Fable limit. … switch models with /model."` matches neither `hit your` nor `usage limit`, so `is_rate_limited` misses it and the iteration classifies as a **crash**, burning auto-block budget.

### Background

Usage monitoring already exists (`load_usage_info` → `account_usage_gate` / `check_and_wait`; post-output `react_to_outputs`). It was designed for a single account percent + one reset. Claude Code now returns many buckets (session, weekly-all, per-rung weekly, spend, extra_usage, promotional code-names). The parser still max-folds them.

Related work that must not be broken:

- FEAT-008 `provider_blackouts` + spillover (whole **provider**, not a rung).
- Dual Anthropic I/O predicates ([5297], [5298], [5301]): pre = `LOOP_USAGE_CHECK_ENABLED ∧ Claude enabled`; post RateLimit load ANDs env; probe is Claude-only.
- Account-global reactions fire **once per wave** ([5075], [4866], [4138]).
- `CapabilityTier` / `tier_of` is **config exact-match** (`src/loop_engine/model.rs`). Substring tier classification is dead.

Prior learnings consulted: [5297], [5298], [5301], [5075], [4866], [4138], [5090] (`parse_reset_from_output` fallback), [3927] (quota deferral must not trip stale-abort).

### Intended Outcome

After all three PRs:

- Remaining **percent left** (and remaining time) is the only unit operators see and the gate compares.
- Each API bucket is a generic `QuotaBucket`; apply-layer policy decides `wait` | `unavailable` | `stop` | `ask` | `ignore`. `evaluate_quota` does **not** emit `ask`.
- **Frontier** low → continue on **standard** (operator: “not critical, use opus”). Factory default `tierFallback.maxDifficulty: high` + `includeReview: true` **is** that instruction (PR-2 `unavailable + Proceed`, **never Ask**). No model id in engine state.
- `ask` is the **opt-out** only. `--use-other-models-ttl` (default **0** from `usagePolicy.askTtlMinutes`) is the Ask wait cap. Ask-path TTL 0 = **Defer, no sleep**. CLI flag overrides `policy.ask_ttl_minutes` **before** `ask_or_defer` (`effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` via `UsageParams`). TTL > 0 re-reads `usagePolicy` + `routing.tierFallback` only on the stop-check cadence (`WaitTiming.stop_check_secs`). Ask-timeout does not set `was_stopped` / does not stop `batch --chain`; operator `.stop` during ask does.
- PR-3 clamp is **three sites as one path**: (a) snapshot counts clamp-eligible tasks as runnable; (b) `compute_quota_excluded_ids` uses post-clamp resolve; (c) spawn `resolve_execution_plan` clamps and rewrites `plan.model` via `exact_model_for`. Factory + only-frontier-left + 6d reset → **Proceed + clamp**, not HorizonStopped. Missing (a) parks. Missing (c) after (b) dispatches Fable. PR-3 walker must **not** reintroduce `exact_model_for(mapped_rung)` extra-mark. PR-3 must not loop until CONTRACT-002 (PRE-PR-3 extra-mark identity) is green.
- Extra-mark is **HUD-family identity union** (PRE-PR-3 CONTRACT-002 / architect rev 1): after HUD maps to rung R, identity set I = **always** the built-in family constant for that HUD tier (`FABLE_MODEL` / `OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL`) **plus** `scope.model.id` when present. Extra-mark every defined **Claude** rung whose `exact_model_for` equals any I. Always include HUD primary. **Do not** prefer snapshot id over the constant (that kills Opus+pin extra-mark). **Not** `exact_model_for(mapped_rung)` after operator pins. Fable HUD + `set-tier claude frontier <opus>` → frontier **only**. Opus HUD + same pin (including `id: "claude-opus-5-SNAPSHOT"`) → standard **and** frontier. Opus HUD, no pin → standard only. Unlabeled `seven_day_opus` / `seven_day_sonnet` ingest with `rungs: None`.
- Live `mw_integrations` unsticks after **PR-1** (false account park gone) + **PR-2 factory exclude** (mixed standard work proceeds without a pin) + **PRE-PR-3** (Fable HUD must not extra-mark standard). Automatic clamp of all-high / review onto standard still needs **PR-3**. Do **not** pin until CONTRACT-002; after PRE-PR-3 the pin is optional.

**After PR-1 alone** (phase-1 ship): the false account park is gone (gate uses account-binding **used** 55% < 92); Fable/rung-scoped CLI (model token + `limit`, or co-occurrence with “switch models”) is `RateLimit` with a 3600s Wait that ignores API/output secs, does not Blackout the provider, and does not early-lift. Remaining `% left` banners are **not** in PR-1. The historical “pin required for parallel/wave” recipe is **struck** for the PR-2+ tree (PRE-PR-3): that pin extra-marks standard from a Fable HUD row.

---

## 2. Goals

### Primary Goals

- [ ] **PR-1:** Account wait uses only **account-binding** buckets (session + weekly-all). A scoped frontier bucket must not set `percentage` / `reset_at` / exhausted. `UsageInfo.percentage` stays **used** 0–100; compare stays `>= usage_threshold` (92). Named `seven_day_opus` / `seven_day_sonnet` are **not** account-binding.
- [ ] **PR-2:** Operators and logs speak **remaining** (`76% left (3m)`), never used-percent. PR-1 stderr may still print `Usage: 55.0% (threshold: 92%)`.
- [ ] **PR-1:** Fable/rung-scoped CLI (model token `fable|opus|sonnet|haiku` + `limit`, **or** co-occurrence with “switch models”) is `RateLimit`, never crash/auto-block. Coordinator: `Wait { secs: blackout_fallback_secs }` (default 3600), ignoring `api_secs` and `output_secs`; no provider Blackout; no `usage_gate` / `probe_rate_limit_lifted` for that phrasing. Plain `reached your … limit` (no model token, no “switch models”) is ordinary RateLimit and **keeps** `api_secs`.
- [ ] **PR-2:** Horizon heuristic: wait (≤1h); **wait capped at `MAX_WAIT_SECS`** (1h–12h, including a 3h session reset); stop (>12h and nothing else can run). Factory default **is** a downgrade instruction (`tierFallback.maxDifficulty: high`, `includeReview: true`). Ask is the **opt-out** when the operator has no instruction. All thresholds live in `.task-mgr/config.json`.
- [ ] **PR-3 Ask TTL:** `--use-other-models-ttl <minutes>` on `loop run` / `batch run` (0 allowed) caps how long **Ask** blocks. Config default `askTtlMinutes` is **0**. Factory/allowing `tierFallback` never goes through Ask (PR-2 `unavailable + Proceed`). Ask-path TTL 0 = **Defer, no sleep**. Flag reaches `ask_or_defer` via `UsageParams` built in `startup.rs` (`effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` **before** `ask_or_defer`).
- [ ] **PR-2 ingest / PRE-PR-3 extra-mark / PR-3 clamp:** Rung unavailability is keyed on `(Provider, CapabilityTier)`. HUD label table maps `display_name` (Fable→frontier, Opus→standard, Sonnet→cost-efficient, Haiku→cheapest). Extra-mark is **HUD-family identity union** (PRE-PR-3 CONTRACT-002 / US-008): identity set I = **always** the built-in family constant **plus** `scope.model.id` when present — **not** `exact_model_for(mapped_rung)`, **not** prefer snapshot id over the constant. Extra-mark every defined Claude rung whose `exact_model_for` equals any I. Output remains `(Provider, CapabilityTier)`. PR-3: expiry map + `active_rungs(&map, now) -> HashSet`; three clamp sites as one path; spawn threads rungs into `PlanContext` (field is **PR-3, not landed**); post-clamp `plan.model` from `exact_model_for` (wrap `EXPLICIT_MODEL` early return); inherit + `account_quota_stopped` chain discriminator on `LoopResult`. PR-3 walker must not reintroduce mapped-rung extra-mark. PR-3 depends on CONTRACT-002.
- [ ] **PRE-PR-3 gate (2b, not a fourth product PR):** CONTRACT-002 HUD-family extra-mark **union** + unlabeled named `rungs: None`; wait-driving probe (`wait_probe_lifted` after apply; post-output `WaitFn` unchanged); `AccountReaction` `OperatorStopped` vs `StopSpend` with sequential `Empty` mapping; `has_review = is_frontier_class`; extra_usage/promotional/nimbus Ignore **at evaluate** before amount-exhausted AccountLow; `remainingMinPercent` / `LOOP_USAGE_REMAINING_MIN` reject `> 100` at loop/batch preflight; post-output remaining banner uses run models. Stories: US-008–US-012 / FR-009–FR-011 in **this** PRD.
- [ ] Sequential and wave produce the same `QuotaDecision` for the same buckets+policy (parity lock). PR-1: same `RateLimit` / Wait-3600 outcome for Fable CLI text; wave: one Fable RateLimit + two completions → **exactly one** 3600s wait, no `provider_blackouts.record`. Seq/wave wrappers agree on `OperatorStopped` vs `StopSpend` (PRE-PR-3).

### Success Metrics

- **PR-1 live fixture** (Fable 95% critical, session 24%, week 55%): gate does **not** wait; `UsageInfo.percentage ≈ 55` (used); `reset_at` = session (nothing ≥ 92).
- **PR-1 inverse:** weekly-all at 100% → `percentage = 100`, `reset_at` = weekly (narrowing does not disable the real weekly gate).
- **PR-1:** Fable CLI text (`You've reached your Fable limit` / model-token+`limit` / co-occurrence with “switch models”) → `IterationOutcome::RateLimit`; consecutive-failure / auto-block does not increment; `decide_account_rate_limit` Wait 3600 (not session 5h, not Sep 12, not 300s fallback, not provider Blackout); `usage_gate` / `probe_rate_limit_lifted` not invoked. Wave: one Fable RateLimit + two completions → exactly one 3600s wait, no `provider_blackouts.record`. Negative: `You've hit your limit · resets 4pm` still uses `output_secs` / may Blackout; `You've reached your session limit` keeps `api_secs`.
- **PR-2 (not PR-1):** stderr for the live fixture contains `76% left` / `5% left` and does not print used `95%`. Strike remaining banners from the PR-1 quality gate. PR-1 stderr may still print `Usage: 55.0% (threshold: 92%)`.
- `grep -n "claude-fable-5\\|fable" src/loop_engine/quota.rs src/loop_engine/engine.rs` → 0 (model ids stay in the ingest adapter + `model.rs` constants only). PR-1 may match `fable` only in `detection.rs` ingest of CLI phrasing.
- `tests/reaction_parity.rs` seq/wave identical on scoped-rung CLI text.
- **PR-3 factory + only-frontier-left + 6d reset:** `compute_remaining_work_snapshot` counts clamp-eligible tasks as runnable → apply **Proceed + clamp**, not `HorizonStopped`. All-high / review / explicit-frontier queue continues on standard without a `set-tier` pin.
- **PR-3 CLI TTL:** config `askTtlMinutes: 0` + `--use-other-models-ttl 15` waits 15m on Ask (not Defer). Flag is on `UsageParams`, not only `LoopConfig`.
- **PRE-PR-3 extra-mark:** Fable 95% + frontier configured string = opus → ingest rungs contain frontier, **not** standard. Opus 95% + same pin → both. Opus 95%, no pin → standard only.
- **PRE-PR-3 live-shaped:** `evaluate_quota(ingest(live_shaped_oauth_json()), default, 8)` unavailable == `{ (Claude, Frontier) }` only. Factory apply + medium/high snapshot → `Proceed`, not `Stop`.
- **PRE-PR-3 scoped probe:** only-frontier + 6h reset → apply `Wait { MAX_WAIT_SECS }`; probe that reports week 45% left must **not** lift. Probe that reports scoped Fable remaining 50% **may** lift. Fable CLI 3600 still skips probe.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **PR-1 keeps used-percent.** `UsageInfo.percentage` is used 0–100; wait is `percentage >= usage_threshold` (default 92). **PR-2** flips the operator/gate unit: remaining percent is 0–100 everywhere (`usage_remaining_min` default 8). Never a 0.08 ratio. Old `used >= 92` ≡ new `remaining <= 8`.
- **Account-binding vs rung-scoped.** Session and weekly-all may `wait`/`stop` the account. A frontier weekly bucket may only mark **frontier** unavailable. Operator: frontier 5% left is “use standard”, not park. PR-1: scoped windows simply **do not enter** the account fold; they do not yet produce a rung-unavailable channel.
- **`severity` / `is_active` are display hints**, not the default low predicate (`is_active=true` on Fable was inferred from **one** sample). They must not set `UsageInfo` exhausted in PR-1. Opt-in via rule `when` in PR-2.
- **Spend `stop` only at remaining amount ≤ 0** (default rule, PR-2) for kinds `spend` / `credits` / `dollars` / `tokens`. **`extra_usage` / promotional / `nimbus_quill` Ignore at evaluate** even at dollars 0 (PRE-PR-3 / architect rev 3) — Ignore **before** the `amount_exhausted` AccountLow, then drop `extra_usage` from `is_spend_kind`. Dropping the name alone still Stops via `account_low_is_amount_only`. 8% credits left must not halt (today stops only on the CLI spend message).
- **`has_review` = `is_frontier_class(&id)`** (PRE-PR-3), not `id.contains("REVIEW")`. `REFACTOR-REVIEW-FINAL` is false; `MILESTONE-FINAL` and claimed `…-CODE-REVIEW-1` are true.
- **`remainingMinPercent` / `LOOP_USAGE_REMAINING_MIN` reject `> 100`** at preflight (PRE-PR-3). Actionable error names `LOOP_USAGE_REMAINING_MIN`.
- **Wait-driving probe (PRE-PR-3 / architect rev 2).** Keep post-output `WaitFn = Fn(u64) -> bool` **unchanged** (Fable 3600 skip). Preflight: `QuotaAccountAction::Wait { secs, account_binding }` with `account_binding = has_account_binding_wait` (mixed session+scoped = **true**: probe account remaining). Pure `wait_probe_lifted(info, floor, account_binding, models)` built **after** apply. Scoped-only: `buckets_for_run_models`; lift iff every nonempty-rungs bucket remaining `> floor` or missing. **No** `evaluate_quota`. **No** extra GET. Thread `models` onto `QuotaPreflightParams`. `Wait { secs: 0, .. }` stays ready-now.
- **`AccountReaction` Stop split (PRE-PR-3 / architect rev 4; PR-3 inherit dependency).** Delete `Stop`. Split `OperatorStopped` vs `StopSpend`. Exhaustive `match` at `iteration.rs:874` (today `== Stop`) and `wave_scheduler.rs:1101`. **`OperatorStopped` uses the pre-gate `.stop` triple:** sequential `Empty` + `operator_stopped: true` → orchestrator exit **0** + `was_stopped: true`. Wave: `was_stopped: true`, **exit 0**, reason `"stop signal during rate-limit wait"` (not 130). **Not** `RateLimit` + `operator_stopped` — that arm does not set `was_stopped`. **`StopSpend`:** sequential `Empty` + `operator_stopped: false` + `should_stop` (HorizonStopped-shaped, **not** RateLimit `_` exit 1). Wave: `was_stopped: false`, **exit 0**, reason `"usage/spend limit"`, not 130. Spend scan **before** prefer-rung-scoped `decide_item`; mixed Fable+spend → StopSpend (`api_secs` None because Fable skip). Mixed Fable + `hit your limit · resets 4pm` still Wait 3600, no Blackout.
- **Dual predicates unchanged** ([5297]/[5298]). `LOOP_USAGE_CHECK_ENABLED=false` still skips pre-gate; post RateLimit still classifies and recovers. **Exception (PR-1 FR-002):** Fable/rung-scoped CLI phrasing skips `usage_gate` **and** `probe_rate_limit_lifted` even when Claude is enabled — otherwise the 3600s Wait is undone in 30s.
- **`handle_quota_deferral` must not see a rung-only deferral** and treat it as a provider blackout (learning 3927 stale-abort). Sibling `handle_rung_only_empty_selection` is **shipped PR-2**. PR-1 Fable CLI must not call `provider_blackouts.record` even when spillover is enabled.
- **PR-3 three clamp sites are one path.** (a) snapshot counts clamp-eligible as runnable; (b) `compute_quota_excluded_ids` uses post-clamp resolve; (c) spawn `resolve_execution_plan` clamps and rewrites `plan.model` via `exact_model_for`. Missing (a) parks. Missing (c) after (b) dispatches Fable (3600s whole wave) — worse than today. Do **not** ship exclude-post-clamp without spawn clamp.
- **`tier_of` stays exact-match.** API→rung mapping is ingest-only; the decision layer only sees `CapabilityTier`. HUD label table maps `display_name`. Extra-mark is **HUD-family identity union** (PRE-PR-3; **strikes** human-review item 3 as written): identity set I = **always** the built-in family constant for that HUD tier **plus** `scope.model.id` when present. Extra-mark every defined Claude rung whose `exact_model_for` equals any I. Always include HUD primary. **Do not** prefer snapshot id over the constant (Opus HUD `id: "claude-opus-5-SNAPSHOT"` + frontier pin must still extra-mark frontier). **Not** `exact_model_for(mapped_rung)` after operator pins. Fable HUD + `set-tier claude frontier <opus>` → frontier **only**. Opus HUD + same pin → standard **and** frontier. Opus HUD, no pin → standard only. PR-3 walker must not reintroduce mapped-rung extra-mark.

### Performance Requirements

- Usage fetch remains once per sequential iteration / once per wave (account-global). No per-slot usage GET.
- `evaluate_quota` is pure and sub-millisecond on a dozen buckets.
- Ask-path TTL 0 must not sleep (**Defer**, not continue). Factory continue is `unavailable + Proceed` with no Ask wait.

### Style Requirements

- Follow existing reaction coordinators: production entry + hermetic `_inner` + exhaustive param destructure (no `..`). Sequential and wave share the inner.
- `ui::*` for operator lines; `tracing` for diagnostics. Byte-stable wait banners stay on stderr.
- No `unwrap` on API JSON; skip malformed buckets.
- Do not reintroduce substring **tier** classification. The only allowed `contains` is ingest: API family token vs configured model **string** to pick a `CapabilityTier` when `display_name` is absent (`limits[]` unlabeled ids). HUD label table maps `display_name`. Extra-mark compares rungs to identity **set** I (always family constant **plus** `scope.model.id` when present; call once per member and union), **not** `exact_model_for(mapped_rung)`. Named object siblings without `scope.model` must **not** family-token-map onto rungs. Family-match at resolve: `pub(crate)` reuse `usage.rs` `hud_tier_from_label` on the explicit `tasks.model` string; do **not** copy HUD tokens into `model.rs`; do **not** use `family_token_from_id` (underscore rsplit) or substring `tier_of`. `has_review` calls `is_frontier_class` — do not reimplement prefix stripping.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Fable `weekly_scoped` 95% `critical` + session 24% + week 55% | Live bug; max-used fold parks 6 days | **PR-1:** `percentage ≈ 55`, `reset_at` = session, **no wait**. **PR-2:** account remaining 45% left; frontier unavailable |
| weekly_all 100%, session 20% | Narrowing must not drop the real weekly gate | **PR-1:** `percentage = 100`, wait on **weekly** `reset_at` (latest if several account-binding windows ≥ `usage_threshold`) |
| `"You've reached your Fable limit"` | `is_rate_limited` misses it → crash → auto-block | `RateLimit`; PR-1 `Wait { 3600 }` ignoring `api_secs`/`output_secs`; no Blackout; no probe. Predicate: model token (`fable\|opus\|sonnet\|haiku`) + `limit`, **or** co-occurrence with “switch models” |
| `You've reached your session limit` / plain `reached your … limit` | Broad `reached your`∧`limit` would discard real `api_secs` | Ordinary RateLimit: **keep** `api_secs`; spillover may Blackout |
| `You've hit your limit · resets 4pm` | Account copy must not take the 3600 override | Still uses `output_secs` / may Blackout. `/model` alone is **not** sufficient |
| Dated `resets Sep 12, 12:59am` | Today token is `"sep"` → `None` | **PR-1: do not add dated month-name parsing.** Keep `None`. Fable 3600 comes from the phrasing override, not from parsing Sep 12. Dated parse is PR-2/PR-3 and still must not be the **account** wait when the bucket is rung-scoped |
| Named `seven_day_opus` / `seven_day_sonnet` in the JSON | PR-1 folded them as account windows; PR-2 ingest family-token-mapped them onto standard / cost-efficient | **PR-1:** drop from the account fold (not account-binding). **PRE-PR-3:** still ingested (no allow-list), `rungs: None`. Must **not** family-token-map onto standard/cost-efficient. Canonical `live_shaped_oauth_json()` evaluate+apply → unavailable `{frontier}` only, factory Proceed |
| Frontier remapped to standard’s model (`set-tier claude frontier <opus>`) | Human-review item 3 extra-mark vs mapped rung’s configured model extra-marks **standard** from a **Fable** HUD row (harmful pin) | **PRE-PR-3 HUD-family identity union.** Identity set I = **always** family constant **plus** `scope.model.id` when present. Fable HUD + pin → frontier **only**. Opus HUD + pin (including snapshot id) → standard **and** frontier. Opus HUD, no pin → standard only. **Not** `exact_model_for(mapped_rung)`. **Not** prefer snapshot id over the constant |
| Explicit `tasks.model` on an off-ladder frontier id (`claude-fable-5-1`) | `tier_of` is `None`; medium difficulty would dispatch to Fable every cycle | Family-match the ingest adapter against the **explicit `tasks.model` string at resolve time**. If that family maps to an unavailable rung and `includeForced=false`, **defer**. Do not document a wait loop as accepted |
| Two `wait` buckets low (session 2h, weekly 6d) | Soonest-reset re-parks at 2h | Wait the **latest** reset among account-binding windows ≥ `usage_threshold` (PR-1) / low wait buckets (PR-2); early-lift may resume at 2h if session was the only low `wait` |
| `ask` TTL 0, other rungs work, operator **unset** `tierFallback` (JSON null / `unset-tier-fallback`) | Pin 3 opt-out (no downgrade instruction) | Ask-path TTL 0 = **Defer, no sleep**. Factory default is **not** this path — default `tierFallback` is set and never Ask |
| `ask` TTL 0, factory defaults (`maxDifficulty: high`, `includeReview: true`) | Honest auto-downgrade | **No Ask.** Clamp **down** / `unavailable + Proceed` for eligible tasks (PR-2). Reviews included. Forced pins still deferred (`includeForced: false`). Strike “TTL 0: no sleep; continue on working rungs immediately…” as a factory sentence — that continue is unavailable, not Ask |
| `ask` TTL 15, no human | Deaf sleep would ignore a config write; `wait() -> bool` maps timeout to StopSignaled or WaitedAndReset wrongly | Re-read `usagePolicy` + `routing.tierFallback` **only** on `WaitTiming.stop_check_secs`. Richer Ask wait outcome: **Deferred** / continue / `StopSignaled`. Timeout + forbade = `Deferred` (not `wait()==false` → `StopSignaled`). Mid-wait `set-tier-fallback` continues on the stop-check tick. Timeout does **not** set `was_stopped` / does **not** stop `batch --chain`; operator `.stop` during ask does |
| Config `askTtlMinutes: 0` + CLI `--use-other-models-ttl 15` | Flag only on `LoopConfig` + execute never reaches apply | `effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` **before** `ask_or_defer`. Flag on `UsageParams` from `startup.rs` through `iteration.rs` / `wave_orchestration.rs`. Apply already consumes `ask_ttl_minutes` at `account.rs` ~1245/1252. LoopConfig-only is the known-bad: still Defer |
| Session reset in 3h (account-binding low) | Horizon table hole between 1h and 12h | **Wait, capped at `MAX_WAIT_SECS`**. With defaults a 3h session reset waits |
| Reset in 6h (5h-to-12h band), only frontier work left | Cap is 5h, stop horizon is 12h; landed probe lifts on week 45% left in ~30s | **Cap-and-repark cycle** (wait `MAX_WAIT_SECS`, re-evaluate). **PRE-PR-3 wait-driving probe:** scoped-only Wait must **not** lift on account remaining (week 45% left). Account-binding Wait may keep `usage_suggests_lifted` on `UsageInfo.percentage`. Fable CLI 3600 still skips probe. Genuine scoped recovery (Fable HUD now 50% left) **may** lift |
| Factory + only-frontier-left + 6d reset (all-high / review / explicit-frontier queue) | Snapshot treats frontier-resolved todos as not runnable → `HorizonStopped`; pin still required | **PR-3 snapshot AC:** walker would land on a defined non-blacked **lower** rung under current `tierFallback` eligibility → task **is runnable**. Apply **Proceed + clamp, not HorizonStopped**. This is the goal exit without a `set-tier` pin. Mixed standard+frontier already proceeds under PR-2 exclude |
| Reset in 6d, only frontier work left, **forbade** / no cheaper defined rung | Long scoped horizon, no clamp | **Stop this PRD** (not 5h-cap-loop). “No fallback” Stop remains this case only. **Next PRD inherits** the rung-unavailable decision **iff** rung-scoped (`LoopResult.account_quota_stopped == false` + expiry map). Account-binding `stop` still stops the chain. Do **not** exempt all HorizonStopped (weekly-all 6d would continue). Stderr names `set-tier-fallback` |
| `LOOP_USAGE_THRESHOLD=92` still set | Old used-percent env | **PR-2:** preflight **error** (legacy hard-break), not silent ignore. **PR-1:** env still drives used-percent threshold |
| Usage API 429 | Common in logs | Keep last rung-blackout snapshot; do not clear |
| `nimbus_quill` 0% remaining / `extra_usage` dollars ≤ 0 | Promotional / extra_usage | Default **`ignore` even at $0**. Spend/credits **kinds** `spend` / `credits` still stop at amount ≤ 0. Drop `extra_usage` from `is_spend_kind` |
| `has_review` via `id.contains("REVIEW")` | False-positive `REFACTOR-REVIEW-FINAL`; false-negative `MILESTONE-FINAL` | **PRE-PR-3:** `has_review = is_frontier_class(&id)` only |
| `LOOP_USAGE_REMAINING_MIN=200` / `remainingMinPercent > 100` | Unbounded `u8` | Preflight **error**. Actionable error names `LOOP_USAGE_REMAINING_MIN` |
| `.stop` during Fable 3600 vs `StopSpend` (seq/wave) | Wrappers diverge; wave StopSpend looks like SIGINT | **PRE-PR-3:** split `AccountReaction` into `OperatorStopped` vs `StopSpend`. `.stop` during wait → `was_stopped=true`. Horizon/Deferred/StopSpend/rung-only empty are **not** operator stop. Wave StopSpend must **not** be exit 130. Prefer-rung-scoped must not skip sibling StopSpend |
| Mixed wave: Fable RateLimit + spend/credits RateLimit | Prefer-rung-scoped masks StopSpend | **StopSpend wins** (stop loop, no 3600, no Blackout). Fable still wins 3600 vs Blackout when no spend sibling |
| PR-2 `unavailable` / `ask` without full blackout channel | Skip-wait hot-loops the same frontier task **or** parks standard if implemented as account Wait | **Exclude unavailable rungs from the next selection; do not account-wait.** 3600s only when the remaining queue cannot run. Proto-channel `HashSet<(Provider, CapabilityTier)>` on `IterationContext` allowed; **replace on each successful evaluate**; keep snapshot on API fail (not run-scoped stickiness). `handle_rung_only_empty_selection` is **shipped PR-2**. Expiry map + `resolve_execution_plan` clamp stay PR-3. Do not reuse `handle_quota_deferral` |
| Exclude-post-clamp without spawn clamp | Eligible tasks selected then dispatched on Fable | **Forbidden.** Three clamp sites are one path: (a) snapshot + (b) `compute_quota_excluded_ids` post-clamp resolve + (c) spawn `resolve_execution_plan` clamps and sets `plan.model` from `exact_model_for`. Missing (c) after (b) sleeps the whole wave 3600s — worse than today |
| `PlanContext` without rungs / `run_loop` always `IterationContext::new()` | Clamp dead at spawn; inherit on `LoopResult` is a dead field | Sequential: `iteration.rs` `BuildPromptParams` → `prompt/sequential.rs`. Wave: `wave_scheduler.rs` `SlotPromptParams` → `prompt/slot.rs`. `orchestrator.rs` seeds inherited map on `IterationContext` and copies it onto `LoopResult`. `PlanContext.unavailable_rungs` is **PR-3, not landed** (landed has `provider_blackouts` only). Prefer extra files over a split. `wave_scheduler.rs` HashSet `.insert((Provider, Tier))` needs expiry when the field becomes a map |
| Post-clamp `finalize_plan` / `model_for` | Bidirectional walker lands back on blacked frontier | After walker returns a lower tier, set `plan.model` from `exact_model_for` **only**. Wrap the `EXPLICIT_MODEL` **early return** (`model.rs` ~995–1007), not only the default-path tail |
| `includeForced=false` off-ladder explicit pin | Global forbid would regress CODE-FIX-003 | Exclude/defer **that id only**. Family-match: `pub(crate)` `hud_tier_from_label` on the explicit `tasks.model` string |
| `batch --chain` after rung-scoped HorizonStopped | Landed `chain && (exit_code != 0 \|\| !prd_complete)` never seeds the next PRD | Discriminator: `LoopResult` expiry map **and** `account_quota_stopped: bool`. Rung-scoped Stop continues and seeds `LoopRunConfig` → `ctx.unavailable_rungs`; receiver `active_rungs(&map, now)`. Account-binding Stop still aborts. Ask-timeout / Deferred stay off this path (`was_stopped` false) |
| `LOOP_USAGE_CHECK_ENABLED=false` | Dual predicate; evaluate may run only post-output | Pre-gate off; first Fable CLI hit still RateLimit (one consumed iteration) then recover via FR-002 Wait 3600 (no usage_gate/probe). PR-2 proto-channel still **replaced** on each successful evaluate |
| PR-1 / PR-2 without the standard pin | One Fable RateLimit sleeps the **whole wave** 3600s **if a Fable-routed task actually spawns** | **Do not pin** until CONTRACT-002. After PRE-PR-3: pin is **optional, not required**. Factory exclude unsticks mixed standard work. Sequential Fable-routed task still waits 3600s (accepted). Automatic clamp of all-high/review is still PR-3 |
| Wave: one Fable RateLimit + two completions | Account-global wait must not Blackout or double-sleep | Exactly **one** 3600s wait; no `provider_blackouts.record`. Spillover is **never** a working rung for rung-scoped decisions |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **Contract owner**: `src/loop_engine/quota.rs` (pure `QuotaBucket` / `UsagePolicy` / `evaluate_quota` / per-bucket low/unavailable + account wait/stop inputs) — **shipped PR-2**. PR-1 stays in `usage.rs` + `detection.rs` + `reactions/account.rs`. Ingest adapters stay in `usage.rs`. Apply in `reactions/account.rs` resolves `ask`/`wait`/`stop`. **Landed vs PR-3:**
  - `UsagePolicy` already lives on `ProjectConfig` (do not re-land serde as FEAT-008; “serde lands with FEAT-008” in `quota.rs` is stale).
  - Apply already consumes `ask_ttl_minutes` via `ask_or_defer(policy.ask_ttl_minutes)` at apply time (`account.rs` ~1245/1252), not only execute. PR-3 injects CLI TTL **before** that call.
  - `handle_rung_only_empty_selection` is **shipped PR-2** (sibling of `handle_quota_deferral`). Do not reimplement it.
  - Landed `IterationContext.unavailable_rungs` is a proto-channel `HashSet<(Provider, CapabilityTier)>` (no expiry, **replaced on each successful evaluate**, keep snapshot on API fail). PR-3 upgrades it to an expiry map; adapter `active_rungs(&map, now) -> HashSet`.
  - Landed `PlanContext` has `provider_blackouts` only. `PlanContext.unavailable_rungs` is **PR-3, not landed**.
  - Landed `run_loop` always `IterationContext::new()` (`orchestrator.rs`); PR-3 seeds the inherited map and copies it onto `LoopResult`.
- Rung clamp is a **new down-only walker** in `model.rs` (not `model_for` / `finalize_plan`). Off-ladder `tasks.model` is family-matched at resolve time via `pub(crate)` `hud_tier_from_label`.
- **Consumers (2+ stories → CONTRACT-001)**: pre-iteration gate, post-output RateLimit, `models show` (policy print), CLI `set-usage-rule`, synthetic CLI→bucket, rung clamp / excluded ids. **Recommended task: `CONTRACT-001`.** PR-1 may ship without it. CONTRACT-001 is a PR-2 predecessor (already in that slice).
- **CONTRACT-002 (PRE-PR-3):** defined once under **When to Emit** below. Stories US-008–US-012 in **this** PRD.
- **Data Flow Contracts:**

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| OAuth JSON object window | `serde_json::Value` string keys → `QuotaBucket` | `json.get("five_hour")?.get("utilization")?.as_f64()` → PR-1: used as-is; PR-2: `remaining = (100.0 - util).clamp(0.0, 100.0)` |
| OAuth `limits[]` HUD extra-mark | `display_name` → `CapabilityTier`; identity **set** I = family constant **plus** `scope.model.id` | HUD table maps `display_name`; always `FABLE_MODEL`/`OPUS_MODEL`/… **plus** `scope.model.id` when present. Extra-mark every defined Claude rung whose `exact_model_for` equals any I. **Not** prefer snapshot id. **Not** `exact_model_for(mapped_rung)` |
| Named sibling without `scope.model` | `key: "seven_day_opus"` → `kind` may be `weekly_scoped` → `rungs: None` | Still ingested (no allow-list). Do **not** call `map_unlabeled_token` for rungs. Canonical live-shaped evaluate+apply → unavailable `{frontier}` only |
| Project config `usagePolicy` | `serde_json::Value` camelCase → `UsagePolicy` on `ProjectConfig` (**already landed PR-2**) | `config["usagePolicy"]["remainingMinPercent"]`; `config["usagePolicy"]["rules"][i]["onLow"]`; `config["usagePolicy"]["askTtlMinutes"]` default **0** |
| `routing.tierFallback` | camelCase JSON → struct; **JSON-null** = unset (not key delete) | `config["routing"]["tierFallback"]["maxDifficulty"]` (string `low\|medium\|high`; **default `"high"`**); `includeReview` default **true**; `includeForced` default **false**. `unset-tier-fallback` writes JSON `null` |
| `QuotaDecision.unavailable` | `Vec<(Provider, CapabilityTier)>` | `decision.unavailable.iter().any(|(p, t)| *p == Provider::Claude && *t == CapabilityTier::Frontier)` |
| `IterationContext.unavailable_rungs` | PR-2: `HashSet<(Provider, CapabilityTier)>`. **PR-3:** expiry map; readers call `active_rungs(&map, now) -> HashSet` | `replace_unavailable_rungs`, `handle_rung_only_empty_selection`, and `compute_quota_excluded_ids` **must call `active_rungs` internally**. Do not iterate the raw map (expired keys would look active) |
| `PlanContext.unavailable_rungs` | **PR-3, not landed.** Landed `PlanContext` has `provider_blackouts` only. Type: expiry map or `HashSet` from `active_rungs` | Sequential spawn: `iteration.rs` `BuildPromptParams` → `prompt/sequential.rs`. Wave spawn: `wave_scheduler.rs` `SlotPromptParams` → `prompt/slot.rs`. Empty default = clamp dead at spawn (dispatches Fable) |
| Loop CLI TTL | clap `Option<u64>` minutes → **`UsageParams`** (not only `LoopConfig`) | Built in `startup.rs`; passed through `iteration.rs` / `wave_orchestration.rs` (and `wave_scheduler.rs` test `UsageParams` if that struct grows). `effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` **before** `ask_or_defer`. LoopConfig + `execute_quota_account_action` only is the known-bad |
| `LoopResult` inherit | expiry map **and** `account_quota_stopped: bool` (or equivalent split) | `orchestrator.rs` copies `ctx.unavailable_rungs` onto `LoopResult`. `batch --chain`: account-binding Stop aborts; rung-scoped HorizonStopped continues and seeds the next `LoopRunConfig` → orchestrator `ctx.unavailable_rungs`. Receiver: `active_rungs(&map, now)`. PRE-PR-3 `OperatorStopped` vs `StopSpend` is a PR-3 inherit dependency (`was_stopped` / exit 130 must not treat credits-stop as `.stop`) |
| Wait-driving probe (PRE-PR-3 / architect rev 2) | `QuotaAccountAction::Wait { secs: u64, account_binding: bool }`; `account_binding = has_account_binding_wait` (mixed session+scoped = **true**) | Keep post-output `WaitFn = Fn(u64) -> bool` **unchanged**. Pure `wait_probe_lifted(info, floor, account_binding, models)` built **after** apply. Account-binding: `usage_suggests_lifted`. Scoped-only: `buckets_for_run_models`; lift iff every nonempty-rungs bucket remaining `> floor` or missing. Thread `models` onto `QuotaPreflightParams`. Fable CLI 3600: still no probe |
| `AccountReaction` (PRE-PR-3 / architect rev 4) | `OperatorStopped` vs `StopSpend` (delete `Stop`) | `OperatorStopped`: sequential `Empty` + `operator_stopped: true` → exit 0 + `was_stopped`. Wave exit **0**, not 130. `StopSpend`: sequential `Empty` + `operator_stopped: false`; wave exit **0**, reason `"usage/spend limit"`. Spend scan **before** prefer-rung-scoped `decide_item` |

### Modularity & Coupling Targets

- **Target public surface**: `quota.rs` types + `evaluate_quota` (shipped PR-2); `UsagePolicy` / `TierFallback` in `project_config.rs` (shipped PR-2); rung blackout on `IterationContext` (key `(Provider, CapabilityTier)`, PR-3 expiry map); clap `--use-other-models-ttl` on `UsageParams`; `models set-usage-rule` / `set-tier-fallback` / `unset-tier-fallback` (JSON-null). No new DB columns.
- **Ownership**: ingest `usage.rs`; policy `quota.rs` (evaluate, per-bucket); apply `account.rs` (remaining work + `tierFallback` + `ask_or_defer`); clamp `model.rs` (new down-only walker + `exact_model_for` rewrite); persist-nothing ephemeral channel `engine.rs`; spawn threaders `iteration.rs` / `wave_scheduler.rs` / `prompt/{sequential,slot}.rs`; inherit seed `orchestrator.rs` + chain discriminator `batch.rs`.
- **Coupling budget**: `quota.rs` must not import runners, clap, or SQLite. `model.rs` must not parse OAuth JSON. Reactions must not hardcode `weekly_scoped`. **`model.rs` must not call `model_for` / `finalize_plan` for blackout clamp.** `model.rs` must not copy HUD tokens; family-match reuses `usage.rs` `hud_tier_from_label` `pub(crate)`.
- **Cohesion**: horizon defaults (`waitIfResetWithinMinutes`, `stopIfResetBeyondHours`, `askTtlMinutes` default 0) live next to `usagePolicy.rules`.
- **FEAT-007 file cap:** do **not** drop clamp/inherit/family-match to keep a 10-file cap. Prefer extra files over a split. If a split is mentioned: **007a** expiry+`active_rungs`+replace+synthetic 3600; **007b** walker+family-match+all three clamp sites; **007c** inherit+chain discriminator. Do **not** ship 007b exclude-post-clamp without 007b spawn clamp. This is a split **inside PR-3**, not a fourth PR.

### When to Emit a CONTRACT-xxx Task

**`CONTRACT-001`** — `QuotaBucket` + `UsagePolicy` + `evaluate_quota` → per-bucket **low / unavailable** plus account **wait/stop inputs**. `evaluate_quota` does **not** emit `ask`. The apply layer in `account.rs` resolves `ask` / `wait` / `stop` from remaining work + `tierFallback`. Gate, post-output, CLI synthetic buckets, and `models show` all implement against that split. Priority 0–1, `taskType: "contract"`. Downstream FEAT/FIX stories depend on it. PR-2 predecessor (already in that slice).

**`CONTRACT-002`** — HUD-family extra-mark identity **union** + unlabeled named sibling `rungs: None` (PRE-PR-3). Consumers: ingest, remaining banner, `evaluate_quota` (via `QuotaBucket.rungs`), apply proto-channel, PR-3 down-only walker. Priority 0–1, `taskType: "contract"`. PR-3 clamp/TTL/inherit **must not loop until this is green**. Do not bury in a banner-only story. Stories: US-008–US-012 in **this** PRD. Extra-mark helper takes an identity **set** (call once per member of I and union) — not a single `identity: &str` of “id or family constant.”

---

## 3. User Stories

### US-001: Account wait ignores rung-scoped buckets (PR-1)

**As a** loop operator
**I want** the pre-iteration gate to wait only on session / weekly-all **used** percent
**So that** a 95%-used frontier weekly bucket cannot park the loop until next week

**Acceptance Criteria:**

- [ ] Live fixture: `UsageInfo.percentage ≈ 55` (max of account-binding **used**: session 24, weekly-all 55); `reset_at` is session (nothing ≥ `usage_threshold` 92); `check_and_wait` returns BelowThreshold
- [ ] Inverse: weekly-all used 100 → `percentage = 100`, `reset_at` = weekly
- [ ] When several account-binding windows are ≥ `usage_threshold`, `reset_at` is the **latest** of those (not soonest)
- [ ] `limits[].severity = critical` and `is_active` on a scoped row do not mark the account exhausted and do not enter `percentage`
- [ ] Named `seven_day_opus` / `seven_day_sonnet` are dropped from the fold (`usage.rs` named-key list becomes `five_hour`, `seven_day` only)

### US-002: Fable CLI is a rate limit, not a crash (PR-1)

**As a** loop operator
**I want** “You’ve reached your Fable limit” classified as RateLimit with a 3600s Wait that does not Blackout the Claude provider
**So that** tasks are not auto-blocked while frontier is merely out. Sequential standard work is not provider-blacked. Wave still sleeps once per wave unless the PR-1 pin is set.

**Acceptance Criteria:**

- [ ] `is_rate_limited` is true for the live sentence `You've reached your Fable limit`
- [ ] The **3600 Wait override** keys on a **narrow** Fable/rung-scoped predicate: a model token (`fable|opus|sonnet|haiku`) followed by `limit`, **or** co-occurrence with “switch models”. `/model` alone is **not** sufficient
- [ ] Plain `You've reached your session limit` / `reached your … limit` (no model token, no “switch models”) is ordinary RateLimit: **keep** `api_secs`; spillover may Blackout
- [ ] Outcome is `IterationOutcome::RateLimit` (already excluded from `handle_task_failure` at both callers — consecutive-failure / auto-block do not increment once classified)
- [ ] For the narrow predicate: `decide_account_rate_limit` returns `Wait { secs: blackout_fallback_secs }` (default 3600), **ignoring `api_secs` and `output_secs`**, even if spillover is enabled. No `RateLimitAction::Blackout`. No `provider_blackouts.record`
- [ ] `react_to_outputs` must **not** run `usage_gate` / `probe_rate_limit_lifted` for that phrasing; sleep is stop-signal-aware only
- [ ] Do **not** add dated `Sep 12` / month-name parsing in PR-1. Keep `parse_reset_from_output("Sep 12, 12:59am")` → `None`
- [ ] Tests: (a) detection of the live sentence; (b) `api_secs = 6 days` + Fable output → wait 3600, not Blackout; (c) `spillover_enabled = true` → still Wait, blackout map empty; (d) probe/usage_gate not invoked; (e) negative `You've hit your limit · resets 4pm` still uses `output_secs` / may Blackout; (f) negative `You've reached your session limit` keeps `api_secs`; (g) wave: one Fable RateLimit + two completions → **exactly one** 3600s wait, no `provider_blackouts.record`
- [ ] PR-1 Fable RateLimit still sleeps the wave 3600s if a Fable-routed task actually spawns. Historical “pin required for parallel/wave” is **struck** for the PR-2+ tree (PRE-PR-3): until CONTRACT-002, that pin is **harmful**; after PRE-PR-3 the pin is **optional, not required**

### US-003: Remaining is the unit operators see (PR-2)

**As a** loop operator
**I want** every usage line to show `% left` and time left
**So that** the HUD and the loop agree (93% used → 7% left)

**Acceptance Criteria:**

- [ ] Banner: `session 76% left (3m) · week 45% left (5d 13h) · frontier 5% left (5d 13h) (floor 8%)`
- [ ] No used-percent in that banner
- [ ] Dollar/token buckets print in their unit when present
- [ ] `usage_threshold` renamed `usage_remaining_min` default 8; `LOOP_USAGE_REMAINING_MIN` overrides config `remainingMinPercent` overrides 8; `LOOP_USAGE_THRESHOLD` preflight errors. **PRE-PR-3:** `remainingMinPercent` / `LOOP_USAGE_REMAINING_MIN` reject `> 100` (actionable error names `LOOP_USAGE_REMAINING_MIN`)
- [ ] **Not in PR-1.** PR-1 stderr may still print `Usage: 55.0% (threshold: 92%)`

### US-004: Horizon heuristic wait / ask / stop (PR-2)

**As a** loop operator
**I want** defaults that wait only for a near reset, auto-downgrade when other rungs work, ask only when I forbade downgrade, and stop when the reset is far and nothing else can run
**So that** I do not sit in 5h-cap loops for six days unless I opted into that, and frontier-out uses standard by default

**Acceptance Criteria:**

- [ ] Config defaults: `waitIfResetWithinMinutes: 60`, `stopIfResetBeyondHours: 12`, `askTtlMinutes: 0`
- [ ] Factory default `routing.tierFallback.maxDifficulty: high`, `includeReview: true`, `includeForced: false` — this **is** the downgrade instruction (pin 3’s ask path is the opt-out)
- [ ] Account-binding low + reset ≤ 60m → `wait`
- [ ] Account-binding low + reset in (60m, 12h] → **wait, capped at `MAX_WAIT_SECS`**. With defaults a **3h session reset waits**; the **5h-to-12h** band is still a cap-and-repark cycle. Same for rung-scoped rows that wait
- [ ] Account-binding low + reset > 12h + no other provider/rung → `stop` (in_progress → todo)
- [ ] Rung-scoped low + other rungs work + factory/allowing `tierFallback` → `unavailable` (downgrade), not ask. Mixed standard+frontier proceeds under PR-2 exclude. **PR-3:** “other rungs work” includes clamp-eligible tasks (snapshot AC in US-006) so factory + only-frontier-left also Proceeds, not HorizonStopped
- [ ] Rung-scoped low + other rungs work + operator **forbade** downgrade (unset `tierFallback`, narrower `maxDifficulty`, `includeReview: false`) → apply emits `ask` (pin 3 opt-out). Ask-path TTL 0 = Defer (US-005); factory never goes through Ask
- [ ] Explicit `onLow` on a matching rule wins over the heuristic
- [ ] Account-binding `wait`/`stop` **can coexist** with rung `unavailable`. `stop` beats `ask`
- [ ] `evaluate_quota` does **not** emit `ask`. Apply layer owns remaining-work + `tierFallback` and resolves `ask` / `wait` / `stop`. PR-2: exclude unavailable rungs from the next selection; do not account-wait; 3600s only when the remaining queue cannot run
- [ ] Spillover is **never** a working rung for rung-scoped decisions

### US-005: `--use-other-models-ttl` (ask timeout) (PR-3)

**As a** loop operator
**I want** to cap how long the loop waits for me on the Ask opt-out path
**So that** unattended Ask (TTL 0) defers immediately with no sleep, and a raised TTL re-reads policy on the stop-check cadence then defers or continues per current `tierFallback` eligibility — never as a factory auto-downgrade through Ask

**Acceptance Criteria:**

- [ ] `task-mgr loop run … --use-other-models-ttl 15` and `batch run` accept minutes including 0
- [ ] **Factory vs Ask split.** Factory/allowing `tierFallback` (`maxDifficulty: high`, `includeReview: true`) = continue-via-unavailable (PR-2 `unavailable + Proceed`, **never Ask**). Ask/Defer is the opt-out: JSON-null `tierFallback` / `unset-tier-fallback` / narrower `maxDifficulty` / `includeReview: false`, or explicit `onLow: ask`
- [ ] Flag overrides `usagePolicy.askTtlMinutes` for this run only. Config default is **0**. Override is `effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` computed **before** `ask_or_defer`
- [ ] **CLI TTL wiring (spec, not JSON).** Flag must reach `ask_or_defer` via `UsageParams` built in `startup.rs` and passed through `iteration.rs` / `wave_orchestration.rs` (and `wave_scheduler.rs` test `UsageParams` if that struct grows). Putting it only on `LoopConfig` + `execute_quota_account_action` is the known-bad: config `askTtlMinutes: 0` + `--use-other-models-ttl 15` still Defer. Apply already consumes `ask_ttl_minutes` via `ask_or_defer(policy.ask_ttl_minutes)` at apply time (`account.rs` ~1245/1252), not only execute
- [ ] **Ask-path TTL 0 = Defer, no sleep.** Strike “TTL 0: no sleep; continue on working rungs immediately…” as a factory sentence. That continue is PR-2 `unavailable + Proceed`, not Ask
- [ ] TTL > 0: re-read `usagePolicy` + `routing.tierFallback` **only** on the stop-check cadence (`WaitTiming.stop_check_secs`, not `probe_secs`). Do not deaf-sleep. Human resolution before TTL (config write / `.stop` is not required — if they set `tierFallback` or a usage rule, the next stop-check uses it)
- [ ] **Richer Ask wait outcome:** Deferred / continue / StopSignaled. Do **not** reuse today’s `wait() -> bool` mapping (complete → `WaitedAndReset`, false → `StopSignaled`) for timeout+forbade. Timeout + forbade = `Deferred`. Mid-wait `set-tier-fallback` continues on the stop-check tick. Keep `wait_for_usage_reset_inner`; do not keep the Ask match-arm’s post-wait mapping
- [ ] Ask-timeout / Deferred does **not** set `was_stopped` and does **not** stop `batch --chain`. Operator `.stop` during ask **does** (`StopSignaled`, exit 130) and stops the chain. Incomplete-PRD chain stop for forbade-defer is OK
- [ ] Distinct stderr names continue vs defer: `ask: frontier 5% left; other rungs available; waiting 15m for policy (--use-other-models-ttl), then continuing on standard` **or** `… then deferring (tierFallback forbids)`

### US-006: Rung blackout + optional `tierFallback` (PR-3)

**As a** loop operator
**I want** frontier-out to run standard unless I forbade downgrade
**So that** the all-high / review / explicit-frontier queue continues on standard **without** a `set-tier` pin, and reviews can still stay on frontier when I opt out (`includeReview: false`)

**Acceptance Criteria:**

- [ ] Blackout key `(Provider, CapabilityTier)` — engine never stores `fable`. Expiry map upgrades the PR-2 proto-channel `HashSet`; **not** a rewrite of apply. Adapter: `active_rungs(&map, now) -> HashSet`. `replace_unavailable_rungs`, `handle_rung_only_empty_selection` (shipped PR-2 sibling of `handle_quota_deferral`), and `compute_quota_excluded_ids` **must call `active_rungs` internally**. Silent ignore if any of those still iterate the raw map. `wave_scheduler.rs` HashSet `.insert((Provider, Tier))` needs expiry when the field becomes a map
- [ ] Factory default `tierFallback.maxDifficulty: high`, `includeReview: true`, `includeForced: false` — **automatic** clamp down for eligible tasks (reviews included). Continue-via-unavailable (PR-2), **never Ask**. This **strikes** “unset means no automatic downgrade”
- [ ] Eligible tasks clamp **down** defined rungs skipping unavailable ones via a **new down-only walker** (must not call `ResolvedModelsConfig::model_for` / `finalize_plan` — those still walk **up**)
- [ ] **Three clamp sites are one path** (PR-2 exclude **skips** frontier work; PR-3 clamp must **run** it on standard). Required together:
  1. (a) `compute_remaining_work_snapshot` treats a task as runnable if the down-only walker would land on a defined non-blacked **lower** rung under current `tierFallback` eligibility
  2. (b) `compute_quota_excluded_ids` uses post-clamp resolve
  3. (c) spawn `resolve_execution_plan` actually clamps and rewrites `plan.model` via `exact_model_for`
  Missing (a) parks (`HorizonStopped`). Missing (c) after (b) **dispatches Fable** (3600s whole wave) — worse than today. Do **not** ship exclude-post-clamp without spawn clamp
- [ ] **Snapshot AC (new).** Factory + only-frontier-left + 6d reset → **Proceed + clamp, not HorizonStopped**. “No fallback” Stop remains the **forbade** / no-cheaper-defined-rung case. Mixed standard+frontier already proceeds under PR-2 exclude; this AC is required for the goal exit (frontier-low continues on standard without a `set-tier` pin) for the all-high / review / explicit-frontier queue
- [ ] If the operator forbids downgrade (JSON-null `unset-tier-fallback`, narrower `maxDifficulty`, `includeReview: false`), there is no clamp; Ask/TTL expiry **defers**, it does not continue
- [ ] Post-resolve clamp after **all six rungs**. Wrap the `EXPLICIT_MODEL` **early return** (landed `model.rs` ~995–1007), not only the default-path tail. After the walker returns a lower tier, set `plan.model` from `exact_model_for`, **never** `finalize_plan` / `model_for`
- [ ] Off-ladder explicit pins: `pub(crate)` reuse `usage.rs` `hud_tier_from_label` on the **explicit `tasks.model` string** at resolve time (do **not** copy HUD tokens into `model.rs`; do **not** use `family_token_from_id` underscore rsplit or substring `tier_of`). If that family maps to an unavailable rung and `includeForced=false`, **exclude/defer that id only** (do not regress CODE-FIX-003 global forbid). Do not drop this AC. Do not document a wait loop as accepted
- [ ] Deferral stderr names the exact flag (`--include-review`, `--include-forced`, `unset-tier-fallback` / `set-tier-fallback`)
- [ ] **Clamp spawn threaders (do not drop to keep a 10-file cap).** Production spawn must pass unavailable rungs into `PlanContext` (this field is **PR-3, not landed** — landed `PlanContext` has `provider_blackouts` only):
  - sequential: `iteration.rs` `BuildPromptParams` → `prompt/sequential.rs`
  - wave: `wave_scheduler.rs` `SlotPromptParams` → `prompt/slot.rs`
  - `orchestrator.rs` seeds inherited map on `IterationContext` (today `run_loop` always `IterationContext::new()`) and copies the map onto `LoopResult`
  Prefer extra files over a split. If a split is mentioned: 007a expiry+`active_rungs`; 007b walker+family-match+all three clamp sites; 007c inherit+chain discriminator. Do **not** ship 007b exclude-post-clamp without 007b spawn clamp
- [ ] **Inherit + chain discriminator.** `LoopResult` carries the expiry map **and** `account_quota_stopped: bool` (or equivalent split). `batch --chain`: account-binding Stop still aborts; **rung-scoped** HorizonStopped continues and seeds the next `LoopRunConfig` → orchestrator `ctx.unavailable_rungs`. Receiver: `active_rungs(&map, now)`. Ask-timeout / Deferred stay off this path (`was_stopped` false; incomplete-PRD chain stop for forbade-defer is OK). Landed `batch.rs` `chain && (exit_code != 0 || !prd_complete)` would otherwise never seed the next PRD after rung-scoped Stop. Account-binding Stop and rung-scoped Stop are the same `UsageCheckResult::HorizonStopped` today — the discriminator is the fix. Do **not** exempt all HorizonStopped (that would let weekly-all 6d continue the chain)
- [ ] All leftover work needs unavailable rungs **and** is not clamp-eligible (forbade / no cheaper defined rung), reset ≤ 12h (including the 1h–12h capped-wait band) → wait; reset > 12h → stop this PRD. Account-binding `stop` still stops the chain
- [ ] Must not fall through `handle_quota_deferral` into stale-abort. `handle_rung_only_empty_selection` is **shipped PR-2** — do not reimplement it

### US-007: Policy CLI + `models show` (PR-3)

**As a** loop operator
**I want** to set per-bucket `onLow` and see remaining via a live fetch
**So that** I can sit out frontier, ask, or stop without editing JSON by hand

**Acceptance Criteria:**

- [ ] `task-mgr models set-usage-rule --kind weekly_scoped --on-low wait|unavailable|stop|ask|ignore`
- [ ] `task-mgr models set-tier-fallback <low|medium|high> [--include-review] [--include-forced]` / `unset-tier-fallback`. **`unset-tier-fallback` writes JSON `null`** (not key delete)
- [ ] `models show` prints policy from config. **Offline `models show` has no remaining percents.** Remaining numbers only via a live fetch
- [ ] Drive `list --remote`’s existing opt-in (`TASK_MGR_USE_API=1` / `check_opt_in`); do **not** invent a second predicate. Key is the fetch (do not require a second `ANTHROPIC_API_KEY` gate beyond what `list --remote` already uses)
- [ ] Sparse `serde_json::Value` round-trip; unrelated keys preserved

### US-008: Fable HUD + frontier pin must not park standard (PRE-PR-3 CONTRACT-002)

**As a** loop operator
**I want** extra-mark keyed on HUD-family identity **union**
**So that** pinning frontier to opus (or not pinning) never marks standard unavailable from a Fable weekly row, and an Opus HUD snapshot id still extra-marks frontier after the pin

**Acceptance Criteria:**

- [ ] After HUD maps `display_name`/`id` → rung R, identity set I = **always** the built-in family constant for that HUD tier **plus** `scope.model.id` when present. Always include HUD primary. **Do not** prefer snapshot id over the constant. **Not** `exact_model_for(R)` on the run config
- [ ] Extra-mark every defined Claude rung where `exact_model_for(provider, tier)` equals any I. Do not iterate Grok/Codex
- [ ] Fixture: frontier configured = opus, Fable 95% (no `scope.model.id`) → rungs contain frontier, **not** standard
- [ ] Fixture: frontier configured = opus, Opus 95% + `id: "claude-opus-5-SNAPSHOT"` → rungs contain standard **and** frontier
- [ ] Fixture: no pin, Opus 95% → standard only
- [ ] Production gate still re-ingests with run `ResolvedModelsConfig` (WIRE-FIX-001 / CODE-FIX-007 stay)
- [ ] `quota.rs` / `engine.rs` still have no `fable` / `opus` / `claude-fable-5` literals

### US-009: Live-shaped JSON must not quota-empty mixed work (PRE-PR-3)

**As a** loop operator
**I want** vestigial named `seven_day_opus` / `seven_day_sonnet` keys ignored for rung unavailability
**So that** the canonical live fixture continues on standard when only Fable HUD is low

**Acceptance Criteria:**

- [ ] `ingest_named_sibling` does **not** call `map_unlabeled_token` to fill `rungs`. Unlabeled named keys → `rungs: None`
- [ ] Keys are still walked (no window-name allow-list). `kind` may still be `weekly_scoped` for explicit `onLow` rules by kind
- [ ] `ingest_live_fixture_emits_all_siblings_and_limits` **stops asserting** opus→Standard / sonnet→CostEfficient rungs; asserts `rungs.is_none()` instead
- [ ] `evaluate_quota(ingest(live_shaped), default, 8).unavailable == {(Claude, Frontier)}` only
- [ ] `compute_remaining_work_snapshot` of medium+high Claude todos → `other_rungs_runnable: true`; `compute_quota_excluded_ids` after replace excludes high/frontier ids and **does not** exclude a medium/standard id — **with and without** the frontier→opus pin (architect rev 5). Apply-only fixture is not enough
- [ ] Live `seven_day_sonnet` utilization 1.0 still Ignore (existing parse test stays)

### US-010: Scoped cap-and-repark is not lifted by healthy week remaining (PRE-PR-3)

**As a** loop operator
**I want** the wait probe to look at the buckets the wait is for
**So that** only-frontier-left + 6h reset waits up to `MAX_WAIT_SECS` instead of lifting in 30s and soft-stopping

**Acceptance Criteria:**

- [ ] Keep post-output `WaitFn = Fn(u64) -> bool` **unchanged** (Fable CLI 3600 skip untouched)
- [ ] `QuotaAccountAction::Wait { secs, account_binding }` with `account_binding = has_account_binding_wait` (mixed session+scoped = **true**: probe account remaining; next apply re-parks scoped)
- [ ] Pure `wait_probe_lifted(info, floor, account_binding, models)` built **after** apply. Production `account_quota_preflight_inner` / `execute_quota_account_action` **must pass** `Wait.account_binding` into `wait_probe_lifted` **after** apply. Landed `account_quota_preflight` still builds `usage_suggests_lifted` **before** apply — known-bad. Thread `models` onto `QuotaPreflightParams` (exhaustive; all `_inner` tests)
- [ ] Account-binding: `usage_suggests_lifted` (today). Scoped-only: `buckets_for_run_models`; lift iff every nonempty-rungs bucket remaining `> floor` or missing. **No** `evaluate_quota`. **No** extra GET
- [ ] Hermetic: `wait_probe_lifted` with `UsageInfo { oauth_json: Some(live_shaped), percentage: 45, .. }` is **false** for scoped-only. Same + scoped Fable remaining 50% **may** be true. No 30s wall clock, no live Anthropic
- [ ] **Production wrapper / `_inner` test:** scoped-only `Wait { account_binding: false }` + `UsageInfo { percentage: 45, oauth_json: live_shaped }` must **not** lift. Keep post-output `WaitFn = Fn(u64)` **unchanged** (do not widen `WaitFn` / `UsageGateFn`)
- [ ] `Wait { secs: 0, .. }` stays ready-now (CODE-FIX-004). Ask / org-fallback waits keep today’s account remaining probe
- [ ] `.stop` during the wait still returns `StopSignaled` / operator stop

### US-011: Seq/wave RateLimit Stop mapping + StopSpend vs Fable (PRE-PR-3)

**As a** loop operator
**I want** `.stop` and spend-stop to mean the same thing in sequential and wave
**So that** auto-review, exit codes, and `batch --chain` do not disagree, and a mixed Fable+credits wave stops instead of waiting 3600s

**Acceptance Criteria:**

- [ ] Split `AccountReaction::{OperatorStopped, StopSpend}`; **delete** `Stop`. Exhaustive `match` at `iteration.rs:874` (today `== Stop`, not a match) and `wave_scheduler.rs:1101`
- [ ] `OperatorStopped`: same triple as pre-gate usage-wait `.stop` — sequential `Empty` + `operator_stopped: true` → orchestrator exit **0** + `was_stopped: true`. Wave: `was_stopped: true`, **exit 0**, reason `"stop signal during rate-limit wait"` (not 130). **Not** `RateLimit` + `operator_stopped` (that arm does not set `was_stopped`)
- [ ] `StopSpend`: sequential `Empty` + `operator_stopped: false` + `should_stop` (HorizonStopped-shaped, **not** RateLimit `_` exit 1). Wave: `was_stopped: false`, **exit 0**, reason `"usage/spend limit"`, not 130. `--chain` may still abort on `!prd_complete` (PR-3 inherit)
- [ ] Spend scan **before** prefer-rung-scoped `decide_item`: if **any** RateLimit item is `is_spend_limit_message` and `api_secs.is_none()` → `StopSpend`. Mixed Fable+spend: wrapper skip forces `api_secs = None` so StopSpend wins. Mixed Fable + `hit your limit · resets 4pm` (non-spend): still prefer Fable Wait 3600, no Blackout
- [ ] Wrapper parity test per path (known-bad: wave StopSpend → 130). Update `reaction_parity.rs` `AccountReaction::Stop` asserts

### US-012: Review-class, extra_usage ignore, remaining-min bound, post-output banner (PRE-PR-3)

**As a** loop operator
**I want** apply’s review heuristic and leftover ingest/display nits to match existing SSoTs
**So that** `includeReview: false` and promotional buckets do not surprise, and banners stay honest after the pin

**Acceptance Criteria:**

- [ ] `compute_remaining_work_snapshot` sets `has_review` via `is_frontier_class(&id)` only. Tests: `MILESTONE-FINAL` true, `REFACTOR-REVIEW-FINAL` false, claimed `8d71d1f7-CODE-REVIEW-1` true. Do **not** change `prompt/core.rs` `contains("REVIEW")` in this slice
- [ ] `evaluate_one` Ignores `extra_usage` / `promotional` / `nimbus_quill` **before** `amount_exhausted` AccountLow. Keep AccountLow at amount ≤ 0 only for `spend` / `credits` / `dollars` / `tokens`. Then drop `extra_usage` from `is_spend_kind`
- [ ] Reject `LOOP_USAGE_REMAINING_MIN > 100` and `usagePolicy.remainingMinPercent > 100` in `preflight_validate_and_probe` **only** (loop/batch), same chokepoint as `LOOP_USAGE_THRESHOLD`. Actionable error names `LOOP_USAGE_REMAINING_MIN`. Non-loop commands stay silent. `u8` 200 is the known-bad
- [ ] Add `models: &ResolvedModelsConfig` to `AccountReactionParams` (exhaustive destructure). `check_and_wait` gets run models via the `react_to_outputs` **closure** over `AccountReactionParams.models` (`remaining_banner_for_run_models` when `oauth_json` is present) — **not** by widening post-output `WaitFn` / `UsageGateFn` (landed `UsageGateFn` stays `(u8, &Path, u64)`). Do not change fetch’s builtin snapshot. Fable skip still does not call `check_and_wait`

---

## 4. Functional Requirements

### FR-001: Account-binding parse (PR-1)

Keep `UsageInfo.percentage` as **used** 0–100. Do **not** rename to remaining in this PR. Compare stays `percentage >= usage_threshold` (default 92).

`parse_oauth_usage_json` must compute the gate’s `percentage` / `reset_at` / exhausted set from **only**:

- named `five_hour`, `seven_day`
- `limits[]` rows with `kind` `session` or `weekly_all`

Must **not** enter that fold:

- named `seven_day_opus`, `seven_day_sonnet` (drop from `usage.rs:218–223`)
- `limits[]` `kind=weekly_scoped`
- `severity=critical` / `is_active` as an exhausted predicate
- promotional keys, `extra_usage`

`percentage` = **max used** among account-binding windows. If **no** account-binding window is ≥ `usage_threshold`, `reset_at` prefers the session (`five_hour` / `kind=session`) window (live fixture: ≈ 55 used, session reset, nothing ≥ 92). If several account-binding windows are ≥ `usage_threshold`, `reset_at` is the **latest** of those (not soonest). Inverse: weekly-all used 100 → `percentage = 100`, `reset_at` = weekly.

**Validation:** unit tests on the live fixture (`percentage ≈ 55`, `reset_at` = session) + weekly-all 100% inverse (`percentage = 100`, `reset_at` = weekly).

### FR-002: Fable/rung-scoped CLI RateLimit coordinator contract (PR-1)

The **3600 Wait override** keys on a **narrow** Fable/rung-scoped predicate, not every `RateLimit`:

- a model token (`fable|opus|sonnet|haiku`) followed by `limit` (e.g. `You've reached your Fable limit`), **or**
- co-occurrence with “switch models”.

`/model` alone is **not** sufficient. Plain `You've reached your session limit` / `reached your … limit` (no model token, no “switch models”) is ordinary RateLimit: **keep `api_secs`**; spillover may Blackout.

If the narrow predicate matches:

1. Outcome is `RateLimit` (already excluded from `handle_task_failure` at both callers — consecutive-failure / auto-block ACs are true once classified).
2. `decide_account_rate_limit` returns `Wait { secs: blackout_fallback_secs }` (default 3600), **ignoring `api_secs` and `output_secs`**, even if spillover is enabled. No `Blackout`. No `provider_blackouts.record`. Do **not** fall through the unknown-reset `else` to `usage_fallback_wait` (300s).
3. `react_to_outputs` must **not** run `usage_gate` / `probe_rate_limit_lifted` for that phrasing; sleep is stop-signal-aware only. (Otherwise `api_secs` from the session window ~5h wins via `resolve_wait_secs`, or the 30s CLI probe with no `-m` lifts, or `check_and_wait` sees used 55% < 92 and returns BelowThreshold.)
4. Do **not** add dated `Sep 12` / month-name parsing in PR-1. Keep `parse_reset_from_output` returning `None` for `"sep"` tokens. Fable 3600 comes from this phrasing override, not from parsing a weekly reset out of the CLI text.
5. Tests: (a) detection of the live sentence; (b) `api_secs = 6 days` + Fable output → wait 3600, not Blackout; (c) `spillover_enabled = true` → still Wait, blackout map empty; (d) probe/usage_gate not invoked; (e) negative `You've hit your limit · resets 4pm` still uses `output_secs` / may Blackout; (f) negative `You've reached your session limit` keeps `api_secs`; (g) wave: one Fable RateLimit + two completions → **exactly one** 3600s wait, no `provider_blackouts.record`.

A Fable-routed task that actually spawns still takes the 3600s Wait in `react_to_outputs_inner` (once per wave). Sequential: that task waits 3600s (accepted, no auto-downgrade in PR-1). Automatic frontier→standard is PR-3. The historical “pin required for parallel/wave” recipe is **struck** (PRE-PR-3): until CONTRACT-002 the pin extra-marks standard from a Fable HUD row (**harmful**); after PRE-PR-3 the pin is **optional, not required**. Factory exclude unsticks mixed standard work without a pin.

**Validation:** detection unit tests; coordinator tests (a)–(g). Do not treat “3600 not ~6 days” with `api_secs=None` as sufficient — test (b) requires `api_secs` populated.

### FR-003: Generic ingest (PR-2) + HUD-family extra-mark (PRE-PR-3 CONTRACT-002)

Walk every object sibling with utilization/dollars and every `limits[]` row into `QuotaBucket` (id, kind, label, measurements remaining-first, resets_at, severity, is_active). Map `scope.model.display_name` / `id` → one or more `(Provider, CapabilityTier)` via:

1. HUD label table maps `display_name` (case-insensitive prefix/token): Fable→frontier, Opus→standard, Sonnet→cost-efficient, Haiku→cheapest. Unchanged.
2. Else configured `model_for` substring of the family token against the **configured** model string (**`limits[]` unlabeled ids only** — rows that have a model id and no `display_name`). Named object siblings without `scope.model` **must not** use this path (step 4).
3. Extra-mark is **HUD-family identity union** (PRE-PR-3 CONTRACT-002 / architect rev 1; **strikes** human-review item 3 / “also mark every rung whose configured model string equals the **mapped rung’s** model”):
   - After HUD maps to rung R, identity set I = **always** the built-in family constant for that HUD tier on Claude (`FABLE_MODEL` / `OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL` via `canonical_model_for_hud_tier` in `usage.rs`, `use` from `model.rs`) **plus** `scope.model.id` when present. Always include HUD primary.
   - Extra-mark every defined **Claude** rung whose `exact_model_for(provider, t)` equals **any** I in that set (string equality, not substring `tier_of`, not `model_for`). Do **not** iterate Grok/Codex.
   - **Do not** prefer snapshot id over the constant (that kills Opus+pin extra-mark when HUD sends `id: "claude-opus-5-SNAPSHOT"` and config stores `OPUS_MODEL`).
   - **Not** `exact_model_for(mapped_rung)` after operator pins.
   - Fable HUD + `set-tier claude frontier <opus>` → frontier **only** (must not mark standard). Live-fixture Fable row (no id) stays `{frontier}` only under the pin.
   - Opus HUD + same pin, including `display_name: "Opus"` **and** `id: "claude-opus-5-SNAPSHOT"` → standard **and** frontier (pin hole kept).
   - Opus HUD, no pin → standard only.
4. Unlabeled named siblings (`seven_day_opus` / `seven_day_sonnet` without `scope.model`): still ingested (no allow-list of window names). `kind` may still be `weekly_scoped` for explicit `onLow` rules. `rungs: None`. Must **not** family-token-map onto standard / cost-efficient. Canonical `live_shaped_oauth_json()` evaluate+apply → unavailable `{frontier}` only, factory Proceed.

Output remains `(Provider, CapabilityTier)`. `tier_of` stays exact-match. Unknown → `None` (rule `ignore`). Null objects skip. PR-3 walker must **not** reintroduce `exact_model_for(mapped_rung)` extra-mark. Constants already live in `model.rs`; ingest may `use` them; HUD tokens stay in `usage.rs`.

### FR-004: `evaluate_quota` + apply layer (CONTRACT-001, PR-2)

**Split evaluate vs apply.** `evaluate_quota` is pure and **per-bucket**: it emits `ignore` / `unavailable` (rung-scoped low) plus account **wait/stop inputs** (remaining, reset_secs, kind, low) from buckets + `UsagePolicy` + resolved remaining-min (0–100). It does **not** emit `ask`. It does **not** take `other_rungs_runnable: bool`. CONTRACT-001 must not promise `ask` from evaluate’s inputs.

The **apply** layer in `account.rs` combines remaining work + `tierFallback` + those inputs and **resolves** `ask` / `wait` / `stop` / `unavailable`:

- Factory default `routing.tierFallback.maxDifficulty: high`, `includeReview: true`, `includeForced: false`. That **is** the downgrade instruction. Factory/allowing = continue-via-unavailable (`unavailable + Proceed`, **never Ask**). Pin 3’s “no instruction → ask” is the **opt-out**.
- Apply already consumes `ask_ttl_minutes` via `ask_or_defer(policy.ask_ttl_minutes)` at apply time (`account.rs` ~1245/1252), not only execute. PR-3 injects CLI TTL **before** that call (`effective_ttl` on `UsageParams`). Ask-path TTL 0 = **Defer, no sleep**.
- Account-binding `wait` / `stop` **can coexist** with rung `unavailable`.
- **`stop` beats `ask`.**
- Exclude unavailable rungs from the next selection; **do not account-wait** for a scoped rung.
- 3600s backoff only when the remaining queue cannot run.
- Spillover is **never** a working rung for rung-scoped decisions.
- Proto-channel on `IterationContext` (`HashSet<(Provider, CapabilityTier)>`, no expiry) is **shipped PR-2**. **Replace on each successful evaluate**; keep snapshot on API fail (not run-scoped stickiness). `handle_rung_only_empty_selection` is **shipped PR-2**. Do not reuse `handle_quota_deferral`. Expiry map + `resolve_execution_plan` clamp stay PR-3 (upgrade of `replace_unavailable_rungs`, not a rewrite of apply).
- `UsagePolicy` already lives on `ProjectConfig` (PR-2). Do not re-land serde as FEAT-008.
- **PR-3 snapshot AC (apply consumer):** `compute_remaining_work_snapshot` today resolves with empty blackouts and treats “tier ∈ unavailable_preview” as not runnable. PR-3 must treat a task as runnable if the down-only walker would land on a defined non-blacked **lower** rung under current `tierFallback` eligibility. Factory + only-frontier-left + 6d reset → **Proceed + clamp, not HorizonStopped**. “No fallback” Stop remains the **forbade** / no-cheaper-defined-rung case. Until this lands, “other rungs runnable” in the table below is computed **without** clamp and parks the all-high / review / explicit-frontier queue.

Default `onLow` if a rule does not pin it — **horizon heuristic** (product semantics the apply layer realizes):

| Condition | Action |
| --- | --- |
| Remaining percent > floor (and no opt-in `when`) | ignore (not low) |
| Low + explicit rule `onLow` | that action |
| Low + account-binding + reset_secs ≤ `waitIfResetWithinMinutes` (default 60) | `wait` |
| Low + account-binding + reset_secs in (60m, 12h] | **wait, capped at `MAX_WAIT_SECS`**. With defaults a **3h session reset waits**; the **5h-to-12h** band is still a cap-and-repark cycle |
| Low + account-binding + reset_secs > `stopIfResetBeyondHours` (default 12h) + no other runnable rung/provider | `stop` |
| Low + rung-scoped + other rungs runnable + factory/allowing `tierFallback` | `unavailable` (downgrade / Proceed). **PR-3:** “other rungs runnable” includes clamp-eligible tasks (snapshot AC) — factory + only-frontier-left is this row, not Stop |
| Low + rung-scoped + other rungs runnable + operator forbade downgrade | apply emits `ask` (evaluate does not). Ask-path TTL 0 = Defer, no sleep |
| Low + rung-scoped + **no** other rung runnable (not clamp-eligible: forbade / no cheaper defined rung) + reset ≤ 12h (including the 1h–12h band) | `wait`, capped at `MAX_WAIT_SECS` |
| Low + rung-scoped + no other rung (not clamp-eligible) + reset > stop horizon | `stop` this PRD; **next PRD inherits** the rung-unavailable decision **iff** rung-scoped (`account_quota_stopped == false`). Do not use this row for factory + only-frontier-left |

Spend default rule: `onLow: stop` only when remaining amount ≤ 0 for kinds `spend` / `credits` / `dollars` / `tokens`. **`extra_usage` / promotional / `nimbus_quill` Ignore at `evaluate_one` before the `amount_exhausted` AccountLow** (PRE-PR-3 / architect rev 3). Then drop `extra_usage` from `is_spend_kind`. Dropping the name alone still Stops via `account_low_is_amount_only`. Tests: `extra_usage` dollars 0 → Ignore; `nimbus_quill` dollars 0 → Ignore; `spend` dollars 0 → AccountLow + apply Stop.

`has_review` on the remaining-work snapshot is `is_frontier_class(&id)` (PRE-PR-3), not `id.contains("REVIEW")`.

`remainingMinPercent` / `LOOP_USAGE_REMAINING_MIN` reject `> 100` at `preflight_validate_and_probe` **only** (loop/batch; PRE-PR-3 / architect rev 7). Non-loop commands stay silent.

**Wait-driving probe (PRE-PR-3 / FR-009):** keep post-output `WaitFn` unchanged. Preflight `Wait { secs, account_binding }`; pure `wait_probe_lifted` after apply. Mixed session+scoped = account_binding **true**. Scoped-only must **not** lift on week 45% left. Fable CLI 3600 still **no** probe. Do not add `--use-other-models-ttl` / walker / inherit in this gate.

`wait` among multiple low wait buckets uses the **latest** reset. Early-lift uses **that rule’s** remaining floor (no magic 0.05). `askTtlMinutes` default is **0**.

### PRE-PR-3 gate (2b — not a fourth product PR)

Operator override 2026-09-07: ship this gate **between PR-2 and PR-3**. Implementation stories are **US-008–US-012 / FR-009–FR-011 in this PRD** (sidecar superseded). **Do not** turn those stories into US-005 / US-006 / US-007. **Do not** add `--use-other-models-ttl`, down-only walker, or inherit into this gate. Closed CODE-FIX Highs stay closed.

Law in this parent:

1. **HUD-family extra-mark union** — FR-003 steps 3–4 / US-008 / CONTRACT-002. Until this lands, do **not** pin.
2. **Unlabeled named siblings** — US-009; `rungs: None`; live-shaped evaluate+apply + snapshot/exclude tests.
3. **Wait-driving probe** — FR-009 / US-010. `wait_probe_lifted` after apply; post-output `WaitFn` unchanged.
4. **`AccountReaction` Stop split** — FR-010 / US-011. Sequential `Empty` mapping; wave exit 0. PR-3 inherit dependency.
5. **Hygiene** — US-012 / FR-011. `has_review = is_frontier_class`; extra_usage Ignore at evaluate; remaining-min `> 100` at loop/batch preflight; post-output run-model banner.
6. **Pin docs** — strike “pin required for parallel/wave”. After this gate: pin is **optional** for mixed standard/medium. Automatic clamp of all-high/review is still PR-3.

PR-3 must not loop until CONTRACT-002 is green.

### FR-005: `ask` vs `stop` (PR-3)

**Split factory vs Ask.** Factory/allowing `tierFallback` = continue-via-unavailable (PR-2 `unavailable + Proceed`, **never Ask**). Ask/Defer is the opt-out (JSON null / `unset-tier-fallback` / narrower `maxDifficulty` / `includeReview: false`) or explicit `onLow: ask`.

- **`stop`**: halt this run; reset in_progress→todo; do not continue on other rungs. **Discriminator on `LoopResult`:** `account_quota_stopped: bool` (or equivalent split) plus the expiry map. Account-binding `stop` **stops the chain**. Rung-scoped HorizonStopped: **next PRD inherits** the rung-unavailable decision (batch/process-local) so it can clamp to standard instead of stopping again — `batch --chain` continues and seeds the next `LoopRunConfig` → orchestrator `ctx.unavailable_rungs`; receiver `active_rungs(&map, now)`. Landed `batch.rs` `chain && (exit_code != 0 || !prd_complete)` would otherwise never seed. Do **not** exempt all HorizonStopped (weekly-all 6d would continue the chain). Beats `ask` when both fire. “No fallback” Stop remains the **forbade** / no-cheaper-defined-rung case — **not** factory + only-frontier-left (that is Proceed + clamp; see FR-006 snapshot AC). Ask-timeout / Deferred stay off this path (`was_stopped` false; incomplete-PRD chain stop for forbade-defer is OK). PRE-PR-3 `OperatorStopped` vs `StopSpend` is a **dependency**: inherit must not treat credits-stop as operator `.stop` (`was_stopped` / exit 130).
- **`ask`**: other rungs still work **and** the operator has no (or a forbidding) downgrade instruction — pin 3 opt-out, not the factory default. CLI `--use-other-models-ttl` overrides `policy.ask_ttl_minutes` **before** `ask_or_defer` (`effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)`). Flag must reach `ask_or_defer` via `UsageParams` built in `startup.rs` and passed through `iteration.rs` / `wave_orchestration.rs` (and `wave_scheduler.rs` test `UsageParams` if that struct grows). Apply already consumes `ask_ttl_minutes` via `ask_or_defer(policy.ask_ttl_minutes)` at apply time (`account.rs` ~1245/1252), not only execute. Putting the flag only on `LoopConfig` + `execute_quota_account_action` is the known-bad (config `0` + CLI `15` still Defer).
- **Ask-path TTL 0 = Defer, no sleep.** Strike “TTL 0: no sleep; continue on working rungs immediately…” as a factory sentence. Factory continue is `unavailable + Proceed`, not Ask.
- TTL > 0 re-reads `usagePolicy` + `routing.tierFallback` **only** on the stop-check cadence (`WaitTiming.stop_check_secs`, not `probe_secs`). Richer Ask wait outcome: Deferred / continue / StopSignaled — do **not** reuse today’s `wait() -> bool` mapping (complete → `WaitedAndReset`, false → `StopSignaled`) for timeout+forbade. Timeout + forbade = `Deferred`. Ask-timeout does **not** set `was_stopped` / does **not** stop `batch --chain`; operator `.stop` during ask does (`StopSignaled`, exit 130) and stops the chain. This **strikes** “implicit downgrade does not require `tierFallback`”.

### FR-006: Rung blackout channel (PR-3)

**Depends on PRE-PR-3 CONTRACT-002.** PR-3 must not loop until HUD-family extra-mark identity is green. The walker must **not** reintroduce `exact_model_for(mapped_rung)` extra-mark. After CONTRACT-002, a Fable-low proto-channel does not contain standard, so clamp onto opus is possible.

Ephemeral `(Provider, CapabilityTier) → expiry`. Never persisted. Never touches `runner_overrides`. Replace-from-decision on successful evaluate; keep snapshot on API fail (same replace rule as the PR-2 proto-channel). Synthetic CLI bucket expiry = 3600 even when spillover is unconfigured. This is an **upgrade of `replace_unavailable_rungs`**, not a rewrite of apply. `handle_rung_only_empty_selection` is **shipped PR-2**.

**Type adapter:** `active_rungs(&map, now) -> HashSet`. `replace_unavailable_rungs`, `handle_rung_only_empty_selection`, and `compute_quota_excluded_ids` **must call it internally**. Then `iteration.rs` / `orchestrator.rs` / `wave_orchestration.rs` / `reaction_parity.rs` can pass `&ctx.unavailable_rungs` without treating expired keys as active. Silent ignore if any of those still iterate the raw map, or if `PlanContext` is left empty. `wave_scheduler.rs` HashSet `.insert((Provider, Tier))` needs expiry when the field becomes a map.

`PlanContext.unavailable_rungs` is **PR-3, not landed** (landed `PlanContext` has `provider_blackouts` only).

**Three clamp sites are one path** (PR-2 exclude **skips** frontier work; PR-3 clamp must **run** it on standard). Required together:

1. **(a) Snapshot.** `compute_remaining_work_snapshot` treats a task as runnable if the down-only walker would land on a defined non-blacked **lower** rung under current `tierFallback` eligibility. Factory + only-frontier-left + 6d reset → **Proceed + clamp, not HorizonStopped**. “No fallback” Stop remains the **forbade** / no-cheaper-defined-rung case. Mixed standard+frontier already proceeds under PR-2 exclude; this AC is the goal exit for the all-high / review / explicit-frontier queue (frontier-low continues on standard **without** a `set-tier` pin). Missing (a) parks.
2. **(b) Exclude.** `compute_quota_excluded_ids` uses post-clamp resolve (a task that clamps to a defined non-blacked lower rung is **not** excluded).
3. **(c) Spawn.** `resolve_execution_plan` actually clamps. After rungs pick `(provider, tier)`, if that pair is blacked and the task may fallback under `tierFallback` eligibility, walk **down** defined rungs to the first non-blacked via a **new helper**. Walker `skip >= start` + `exact_model_for` is correct (`CapabilityTier::ALL` is ascending). After the walker returns a lower tier, set `plan.model` from `exact_model_for`, **never** `finalize_plan` / `model_for` (`model_for` is bidirectional nearest-defined — down, then up — and can land on a blacked frontier). Wrap the `EXPLICIT_MODEL` **early return** (landed `model.rs` ~995–1007), not only the default-path tail. Else defer via excluded ids. Missing (c) after (b) **dispatches Fable** (3600s whole wave) — worse than today. Do **not** ship exclude-post-clamp without spawn clamp.

Default `includeForced=false`. `includeForced=false` off-ladder → exclude/defer **that id only** (do not regress CODE-FIX-003 global forbid).

**Off-ladder explicit pins:** `pub(crate)` reuse `usage.rs` `hud_tier_from_label` on the **explicit `tasks.model` string** at resolve time (`claude-fable-5-1` tokens include `fable` → frontier). Do **not** copy HUD tokens into `model.rs`. Do **not** use `family_token_from_id` (underscore rsplit) or substring `tier_of`. If that family maps to an unavailable rung and `includeForced=false`, **defer that id**. Do not drop this AC. Do not document a wait loop as accepted.

**Clamp spawn threaders (do not drop to keep a 10-file cap).** Production spawn must pass unavailable rungs into `PlanContext`:

- sequential: `iteration.rs` `BuildPromptParams` → `prompt/sequential.rs`
- wave: `wave_scheduler.rs` `SlotPromptParams` → `prompt/slot.rs`
- `orchestrator.rs` seeds inherited map on `IterationContext` (today `run_loop` always `IterationContext::new()`) and copies the map onto `LoopResult`

Prefer extra files over a split. If a split is mentioned: **007a** expiry+`active_rungs`+replace+synthetic 3600; **007b** walker+family-match+all three clamp sites; **007c** inherit+chain discriminator. Do **not** ship 007b exclude-post-clamp without 007b spawn clamp. Split is inside PR-3, not a fourth PR.

**Inherit + chain discriminator.** `LoopResult` carries the expiry map **and** `account_quota_stopped: bool` (or equivalent split). `batch --chain`: account-binding Stop still aborts; **rung-scoped** HorizonStopped continues and seeds the next `LoopRunConfig` → orchestrator `ctx.unavailable_rungs`. Receiver: `active_rungs(&map, now)`. Ask-timeout / Deferred stay off this path (`was_stopped` false; incomplete-PRD chain stop for forbade-defer is OK). Landed `batch.rs` `chain && (exit_code != 0 || !prd_complete)` would otherwise never seed the next PRD after rung-scoped Stop. Account-binding Stop and rung-scoped Stop are the same `UsageCheckResult::HorizonStopped` today — the discriminator is the fix. Do **not** exempt all HorizonStopped (that would let weekly-all 6d continue the chain).

Overflow: skip escalate/`to_1m` targets whose **rung** is blacked.

### FR-007: Dual predicate + once-per-wave

Unchanged, except the FR-002 Fable-phrasing skip of `usage_gate` / `probe_rate_limit_lifted`. `account_usage_gate` / `react_to_outputs` remain the only coordinators; both paths destructure params exhaustively. Wave: one Fable RateLimit + two completions → **exactly one** 3600s wait, no `provider_blackouts.record`.

### FR-008: Operator display

**Remaining `% left` banners: PR-2** (US-003). PR-1 stderr may still print `Usage: 55.0% (threshold: 92%)`.

Rung labels in stderr: `frontier` / `standard` / `cost-efficient` / `cheapest`, not model names (PR-3 CLI / deferral lines). Deferral/ask lines name the exact CLI flag.

**PRE-PR-3:** post-output `check_and_wait` remaining banner uses run `ResolvedModelsConfig` when `oauth_json` is present (same as pre-gate). Offline `models show` still has no remaining percents (below).

**`unset-tier-fallback` writes JSON `null`** (not key delete). Sparse `serde_json::Value` round-trip; unrelated keys preserved.

**`models show` remaining percents:** offline `models show` has **no** remaining percents (policy from config only). Remaining numbers only via a live fetch. Drive `list --remote`’s existing opt-in (`TASK_MGR_USE_API=1` / `check_opt_in`); do **not** invent a second predicate. Key is the fetch. Do not overstate as a second `ANTHROPIC_API_KEY` gate beyond what `list --remote` already uses.

### FR-009: Wait-driving probe (PRE-PR-3)

Keep post-output `WaitFn = Fn(u64) -> bool` **unchanged** (Fable 3600 skip must not gain a usage probe). For preflight only (architect rev 2):

- `QuotaAccountAction::Wait { secs: u64, account_binding: bool }` with `account_binding = has_account_binding_wait` already computed at apply (mixed session+scoped = **true**: probe account remaining; next apply re-parks scoped).
- Extract pure `fn wait_probe_lifted(info: &UsageInfo, floor: u8, account_binding: bool, models: &ResolvedModelsConfig) -> bool`:
  - `account_binding`: `usage_suggests_lifted` (today).
  - scoped-only: `buckets_for_run_models`; lift iff every bucket with nonempty `rungs` has percent remaining `> floor` or is missing. **No** `evaluate_quota`. **No** extra GET beyond the existing probe `load_usage_info_with_threshold`.
- Build that probe **after** apply (move construction into `account_quota_preflight_inner` / `execute_quota_account_action`), not in `account_quota_preflight` before apply. Production wrappers **must pass** `Wait.account_binding` into `wait_probe_lifted` after apply (landed preflight still builds `usage_suggests_lifted` before apply — known-bad). Thread `models` onto `QuotaPreflightParams` (exhaustive; all `_inner` tests). Ask / org-fallback waits keep today’s account remaining probe. `Wait { secs: 0, .. }` stays ready-now (CODE-FIX-004). Do **not** widen post-output `WaitFn` / `UsageGateFn`.

**Validation:** hermetic `wait_probe_lifted` tests in US-010 **plus** a production wrapper / `_inner` test: scoped-only `Wait { account_binding: false }` + `UsageInfo { percentage: 45, oauth_json: live_shaped }` must **not** lift. Keep post-output `WaitFn = Fn(u64)`. Do not use wall-clock 30s.

### FR-010: `AccountReaction` discriminator (PRE-PR-3)

Architect rev 4. Binding pick (not “or”):

- Split `AccountReaction::{OperatorStopped, StopSpend}`; delete `Stop`. Exhaustive `match` at `iteration.rs:874` and `wave_scheduler.rs:1101`.
- `OperatorStopped`: same triple as pre-gate usage-wait `.stop` — sequential `Empty` + `operator_stopped: true` → orchestrator exit 0 + `was_stopped: true`. Wave: `was_stopped: true`, **exit 0**, reason `"stop signal during rate-limit wait"` (not 130). Auto-review suppressed. `--chain` aborts via `was_stopped`.
- `StopSpend`: sequential `Empty` + `operator_stopped: false` + `should_stop` (HorizonStopped-shaped, **not** RateLimit `_` exit 1). Wave: `was_stopped: false`, **exit 0**, reason `"usage/spend limit"`, not 130. `--chain` may still abort on `!prd_complete` (PR-3 inherit). Auto-review may fire the same as horizon Stop — accepted.
- Spend scan **before** prefer-rung-scoped `decide_item`: if **any** RateLimit item is `is_spend_limit_message` and `api_secs.is_none()` → `StopSpend`. Mixed Fable+spend: wrapper skip forces `api_secs = None` so StopSpend wins. Mixed Fable + `hit your limit · resets 4pm` (non-spend): still prefer Fable Wait 3600, no Blackout.

**Validation:** inner tests + a **wrapper** parity test per path (known-bad: wave StopSpend → 130). Update `reaction_parity.rs` `AccountReaction::Stop` asserts.

### FR-011: Snapshot review-class, extra_usage Ignore, remaining-min bound, post-output banner (PRE-PR-3)

- `has_review = is_frontier_class(&id)` only. Do not change `prompt/core.rs` in this slice.
- `evaluate_one` Ignores `extra_usage` / promotional / `nimbus_quill` **before** `amount_exhausted` AccountLow. Then drop `extra_usage` from `is_spend_kind`.
- Reject `> 100` at `preflight_validate_and_probe` only (loop/batch). Non-loop silent.
- `AccountReactionParams` gains `models: &ResolvedModelsConfig`. `check_and_wait` gets run models via the `react_to_outputs` **closure** over `params.models` (`remaining_banner_for_run_models` when `oauth_json` is present) — **not** by widening post-output `WaitFn` / `UsageGateFn` (landed `UsageGateFn` stays `(u8, &Path, u64)`).

---

## 5. Non-Goals (Out of Scope)

- Buying or enabling extra_usage credits — ingested, default `ignore`
- Grok/Codex usage adapters — types must not block them; no fetch in v1
- A general expression language — ordered match rules + horizon heuristic only
- Changing FEAT-008 `provider_blackouts` / `promote_once` / review-forces-frontier **request**. Rung-scoped CLI phrasing must **not** record a provider blackout even when spillover is on
- Persisting last-fetched buckets
- Treating `severity` / `is_active` as low in the default predicate
- Reintroducing substring **tier** classification in `tier_of`
- Automatic frontier→standard in PR-1 / PRE-PR-3 (factory exclude unsticks mixed standard; clamp of all-high/review is PR-3). Do **not** tell operators to pin in a way that extra-marks standard
- `--use-other-models-ttl` clap, down-only walker, inherit, `account_quota_stopped` in the PRE-PR-3 gate (those stay **PR-3**)
- Dated month-name `parse_reset_from_output` in PR-1
- Remaining-percent rename / `% left` banners in PR-1

---

## 5.5. Low-Value / High-Effort Areas (Explicit Cuts or Deferrals)

| Area / Capability | Why the value is low relative to cost | Rough effort cost | Recommended action |
| --- | --- | --- | --- |
| Per-task override of `onLow` | Policy is account-global; task pins already exist via `tasks.model` | High | **Cut** |
| Ingesting every Anthropic code-name as a first-class HUD row | Noise; default ignore is enough | Low-medium | Defer display; still ingest as `ignore` |
| Interactive TTY prompt during `ask` | Loops are often non-TTY / babysat | High | **Cut** — TTL + stderr + config/flag; no stdin interview |
| Compact `(3m)` vs `format_duration` `(3m 0s)` | Display-only; AC `contains` already passes | Medium (shared formatter consumers) | **Cut** |
| `prompt/core.rs` `contains("REVIEW")` | Different consumer (prompt text, not apply `has_review`) | Low | **Out of this slice** (architect Low) |

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/usage.rs` — parse fold (PR-1); ingest adapters (PR-2); PRE-PR-3 HUD-family extra-mark + unlabeled named `rungs: None` (CONTRACT-002)
- `src/loop_engine/quota.rs` — **new**, CONTRACT-001 (PR-2)
- `src/loop_engine/detection.rs` — Fable RateLimit phrasing (PR-1)
- `src/loop_engine/reactions/account.rs` — PR-1 FR-002 coordinator contract; PR-2 apply layer / exclude unavailable rungs / `handle_rung_only_empty_selection` (**shipped**) / `ask_or_defer(policy.ask_ttl_minutes)` at apply (~1245/1252); PRE-PR-3 wait-driving probe, `AccountReaction` `OperatorStopped`/`StopSpend`, `has_review = is_frontier_class`, `is_spend_kind` excludes `extra_usage`, post-output run-model banner; PR-3 CLI `effective_ttl` into that call, snapshot clamp-eligible (`compute_remaining_work_snapshot`), richer Ask wait outcome
- `src/loop_engine/config.rs` / clap `loop run` / `batch run` — remaining-min rename (PR-2), `--use-other-models-ttl` (PR-3)
- `src/loop_engine/startup.rs` — PR-3: build `UsageParams` with CLI TTL so it reaches `ask_or_defer` (not only `LoopConfig`)
- `src/loop_engine/project_config.rs` — `usagePolicy`, `tierFallback` (**already on `ProjectConfig` as of PR-2**; PR-3 JSON-null unset)
- `src/loop_engine/engine.rs` — PR-2 proto-channel `HashSet` on `IterationContext`; PR-3 expiry map + `active_rungs` adapter; `replace_unavailable_rungs` calls `active_rungs` internally
- `src/loop_engine/model.rs` — PR-3 new down-only walker; must not call `model_for` / `finalize_plan` for clamp; `plan.model` from `exact_model_for`; wrap `EXPLICIT_MODEL` early return (~995–1007); family-match via `pub(crate)` `hud_tier_from_label`
- `src/loop_engine/reactions/pre_spawn.rs` — excluded ids (PR-2/PR-3 post-clamp resolve; `compute_quota_excluded_ids` calls `active_rungs`)
- `src/loop_engine/prompt/{sequential,slot}.rs`, `iteration.rs` (`BuildPromptParams` + PRE-PR-3 `AccountReaction` match), `wave_scheduler.rs` (`SlotPromptParams` + HashSet→map insert + PRE-PR-3 wrapper match), `orchestrator.rs` (seed inherited map; copy onto `LoopResult`; today `run_loop` always `IterationContext::new()`), `wave_orchestration.rs` (`UsageParams` passthrough) — **required** spawn/inherit threaders; do not drop to keep a 10-file cap. PRE-PR-3 Stop split is a PR-3 inherit dependency
- `src/commands/batch.rs` — PR-3 chain discriminator (`account_quota_stopped`); landed `chain && (exit_code != 0 || !prd_complete)` never seeds after rung-scoped Stop
- `src/loop_engine/reactions/post_output.rs` — overflow skip by rung (PR-3)
- `src/commands/models/handlers.rs` + clap — rules / fallback CLI (PR-3); `unset-tier-fallback` JSON-null; `models show` live-fetch via `list --remote` opt-in
- `src/loop_engine/CLAUDE.md` — contract
- `tests/reaction_parity.rs` + `usage.rs` / `quota.rs` unit tests

### Dependencies

- Existing OAuth `GET /api/oauth/usage` (no new endpoint)
- `CapabilityTier` / `ResolvedModelsConfig::model_for` / `tier_of` (`model_for` is ingest-only for unlabeled ids; **not** for clamp)
- FEAT-008 spillover number `blackoutFallbackSecs` (borrow the default 3600, not the feature flag; Fable phrasing uses this Wait even when spillover is off)

### Approaches & Tradeoffs

No `/spike` on this area; plan review already compared these.

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| A. Only fix the max-used fold (PR-1 forever) | Unsticks today | Fable CLI still crashes; no remaining UX; next HUD bucket repeats the bug | **Rejected** as the full solution; **accepted as PR-1 slice** |
| B. Blackout by model-family substring (`fable`) | Hits off-ladder ids | Engine speaks model names; user forbade that; `tier_of` exact-match culture | **Rejected** |
| C. Generic buckets + policy + **rung** key `(Provider, CapabilityTier)` + horizon heuristic + ask TTL | Matches HUD language; frontier-out → standard; operator-configurable wait/ask/stop; survives new API keys | Larger than A; needs ingest mapping table | **Preferred** |
| D. Always wait the latest reset of any low bucket | Simple | Recreates the 6-day park | **Rejected** |

**Selected Approach**: C, shipped as PR-1 (A) then PR-2 (buckets + heuristic) then PR-3 (rung blackout + `tierFallback` + ask TTL wired to selection). Operator override 2026-09-07: a **PRE-PR-3 gate (2b)** ships between PR-2 and PR-3. That is **not a fourth product PR**.

**Phase 2 Foundation Check**: Generic `QuotaBucket` + `evaluate_quota` costs ~1 extra day vs a Fable-only special case and avoids a rewrite the next time Anthropic adds a scoped weekly row or dollar remaining. 1:10 holds. Horizon numbers in config avoid another hardcode round.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| PR-1 overpromises: Fable-routed tasks still fail until PR-3; wave sleeps 3600s if a Fable-routed task spawns | High (operator thinks they are unstuck) | High | **Do not pin** until CONTRACT-002 (pin extra-marks standard from Fable HUD). After PRE-PR-3: pin optional, not required. Factory exclude unsticks mixed standard. Sequential Fable-routed task waits 3600s (accepted). Empirical: live fixture + Fable CLI tests (a)–(g) + Fable+pin inverse |
| Extra-mark vs `exact_model_for(mapped_rung)` after pin | High (Fable HUD parks standard; poisons PR-3 clamp onto blacked standard) | High on PR-2 tree | PRE-PR-3 CONTRACT-002 HUD-family identity. Fable+pin inverse test. PR-3 walker must not reintroduce mapped-rung extra-mark |
| Unlabeled `seven_day_opus`/`sonnet` family-token-mapped | High (live-shaped evaluate quota-empties mixed work) | High if ingest test still asserts rungs | `rungs: None`; live-shaped evaluate+apply → `{frontier}` only, factory Proceed |
| Scoped 6h Wait lifted by week 45% left | High (30s then soft-stop; never re-evaluates at 5h) | High on landed probe | Wait-driving probe: scoped-only must not use `info.percentage`. Fable CLI 3600 skip untouched |
| PR-2 skip-wait hot-loop **or** account-wait that parks standard | High | High if implemented as `check_and_wait` / `Wait { 3600 }` | Exclude unavailable rungs from next selection; do not account-wait; 3600s only when remaining queue cannot run. Proto-channel **replaced on each successful evaluate**; expiry + clamp stay PR-3 |
| Horizon heuristic stops a weekly-all 6-day account outage that the operator wanted to ride | Med | Med | Explicit `onLow: wait` on `kind=weekly_all`; 5h-cap cycle documented as accepted when they opt in |
| API `display_name` changes (“Fable 5.1”) | Med | Med | Mapping uses case-insensitive prefix/token; extra-mark identity prefers `scope.model.id` when present else built-in family constant; unknown → ignore, not wait |
| Dual-predicate regression | High | Low if tests hold | `tests/reaction_parity.rs` matrix unchanged ([5301]); FR-002 skip of probe/usage_gate is Fable-phrasing-only |
| Remaining invert in PR-1 (`percentage` stored as remaining, compare still used) | High | Closed by FR-001 | Keep used 0–100 in PR-1; remaining rename is PR-2 |
| PR-3 snapshot ignores clamp: factory + only-frontier-left + 6d → `HorizonStopped` | High (pin 1 false-park; all-high / review / explicit-frontier queue never reaches spawn clamp) | High if snapshot AC is dropped | `compute_remaining_work_snapshot` counts walker-landing lower-rung tasks as runnable. Factory + only-frontier-left + 6d → Proceed + clamp, not HorizonStopped |
| PR-3 exclude-post-clamp without spawn clamp | High (selects the task then dispatches Fable; 3600s whole wave — worse than today) | High if 10-file cap drops `iteration.rs` / `wave_scheduler.rs` / `orchestrator.rs` | Three clamp sites as one path. Prefer extra files over a split. Do not ship 007b exclude without 007b spawn clamp |
| PR-3 CLI TTL only on `LoopConfig` + execute | High (config `0` + CLI `15` still Defer at apply `ask_or_defer`) | High if `startup.rs` / `iteration.rs` / `wave_orchestration.rs` omitted | Flag on `UsageParams`; `effective_ttl` **before** `ask_or_defer` |
| PR-3 inherit missing discriminator / chain gate | High (rung-scoped Stop never seeds next PRD; exempting all HorizonStopped lets weekly-all 6d continue) | High against landed `batch.rs` `!prd_complete` | `LoopResult` expiry map **and** `account_quota_stopped`. Rung-scoped continues; account-binding aborts |
| Post-clamp `finalize_plan` / `model_for`, or `EXPLICIT_MODEL` early return unwrapped | High (lands back on blacked frontier) | Med if only default-path tail is wrapped | `exact_model_for` only; wrap `EXPLICIT_MODEL` early return (~995–1007) |
| Ask `wait() -> bool` maps timeout to `StopSignaled` or `WaitedAndReset` | High (kills `--chain`, or continues on standard against human-review 1 when forbade) | Med | Richer outcome: Deferred / continue / StopSignaled. Timeout does not set `was_stopped` |

Top 3 of the original table = rows 1–3 (PR-1 pin, PR-2 skip-wait, horizon weekly-all). PR-3 inversion rows (snapshot park, exclude-without-spawn-clamp, CLI TTL, inherit discriminator, post-clamp walk-up, Ask wait mapping) are the PR-3 ship gate: until snapshot + exclude + spawn clamp are one path **and** inherit has the chain-gate discriminator, PR-3 does not retire the PR-1 pin. Not a fourth PR.

### Security Considerations

- OAuth tokens stay in `oauth.rs`; quota code never logs them
- Sanitize API errors with existing `sanitize_error_tokens`
- No new network destination

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `quota::evaluate_quota` (PR-2) | `(buckets: &[QuotaBucket], policy: &UsagePolicy, remaining_min: u8) ->` per-bucket `ignore` / `unavailable` plus account wait/stop **inputs**. **Does not emit `ask`.** **No** `other_rungs_runnable: bool` | per-bucket facts | n/a (pure) | none |
| apply layer `account.rs` (PR-2; PR-3 snapshot + TTL inject) | evaluate outputs + remaining work + `tierFallback` + **`effective_ttl`** | resolves `ask`/`wait`/`stop`/`unavailable`; factory = `unavailable + Proceed` (never Ask); `stop` beats `ask`; Ask-path TTL 0 = Defer. Snapshot counts clamp-eligible as runnable | n/a | wait / exclude / stop / defer as specified. `ask_or_defer(effective_ttl)` at apply (~1245/1252), not only execute |
| `quota::ingest_oauth_value` (or `usage::parse_oauth_usage_json` returning buckets+legacy) | `&Value -> Vec<QuotaBucket>` | buckets | empty/skip rows | none |
| clap `--use-other-models-ttl` | `Option<u64>` minutes → **`UsageParams`** via `startup.rs` | parsed minutes | clap error | none — must reach `ask_or_defer`; LoopConfig-only is the known-bad |
| `models set-usage-rule` | `--kind/--id --on-low` | stderr ack | io/validate err | sparse config write |
| `models set-tier-fallback` / `unset-tier-fallback` | difficulty + flags; unset = **JSON `null`** (not key delete) | stderr ack | io/validate err | sparse config write |
| `model.rs` down-only walker (PR-3) | `(provider, start_tier, blacked: &HashSet<(Provider, CapabilityTier)>)` | first defined non-blacked **lower** rung, or none | n/a | none — **must not call `model_for` / `finalize_plan`**. Caller sets `plan.model` from `exact_model_for` |
| `active_rungs` (PR-3) | `(&map, now) -> HashSet<(Provider, CapabilityTier)>` | currently-active (non-expired) rungs | n/a | none — **the** type adapter. `replace_unavailable_rungs` / `handle_rung_only_empty_selection` / `compute_quota_excluded_ids` call it internally |
| Ask wait outcome (PR-3) | richer than `wait() -> bool` | Deferred / continue / `StopSignaled` | n/a | timeout does not set `was_stopped`; `.stop` does |
| `usage::extra_mark_rungs_matching` (PRE-PR-3, `pub(crate)`) | `(models, provider, identities: impl IntoIterator<Item = &str>) -> Vec<(Provider, CapabilityTier)>` **or** call once per member of I and **union** | Deduped rungs whose `exact_model_for` equals **any** member of identity **set** I | n/a | none (pure). I = always family constant **plus** `scope.model.id` when present. **Not** a single `identity: &str` of “id or family constant.” **Not** `exact_model_for(mapped_rung)` |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `UsageInfo.percentage` | used 0–100 | **PR-1: still used 0–100**, fold only account-binding windows. **PR-2:** remaining 0–100 (`account_remaining`) | PR-2 Yes | PR-1: fixture tests only. PR-2: rename field; all tests flip |
| `LoopConfig.usage_threshold` | used 92 | **PR-1: unchanged.** **PR-2:** `usage_remaining_min` 8 | PR-2 Yes | Env rename; old env preflight error in PR-2 |
| `parse_oauth_usage_json` | max-used fold of every window including `seven_day_opus` / `seven_day_sonnet` / `weekly_scoped` | **PR-1:** max-used of account-binding windows only (`five_hour`/`seven_day` + `limits[]` `session`/`weekly_all`); latest reset among windows ≥ `usage_threshold` | Yes (bugfix) | Fixture tests: live ≈55/session; inverse 100/weekly |
| `is_rate_limited` | misses Fable sentence | still classifies the live Fable sentence as RateLimit; 3600 **override** is a **narrower** predicate (model token + `limit`, or “switch models”) | No (widens detection; narrows override) | Tests (a)(e)(f) |
| `decide_account_rate_limit` / `react_to_outputs` | `api_secs` wins; spillover → Blackout; probe every 30s | Narrow Fable/rung-scoped phrasing → `Wait { blackout_fallback_secs }` ignoring api/output; no Blackout; no usage_gate/probe. Plain `reached your … limit` keeps `api_secs` | Yes for that phrasing | Tests (b)(c)(d)(e)(f)(g) |
| `usage_suggests_lifted` | used < 92 / < 95 | **PR-2:** remaining > rule floor | PR-2 Yes | Same sites, new compare |
| `PlanContext` | `provider_blackouts` only (landed) | **PR-3:** add `unavailable_rungs` (expiry map or `active_rungs` HashSet). Field is **not landed** | PR-3 Yes (additive) | Sequential `BuildPromptParams` + wave `SlotPromptParams` must pass it; empty default = Fable dispatch |
| `LoopResult` | `prd_complete` (no inherit) | **PR-3:** expiry map **and** `account_quota_stopped: bool` | PR-3 Yes (additive) | `orchestrator.rs` copies map; `batch --chain` uses discriminator |
| `UsageParams` | no CLI TTL | **PR-3:** carries `--use-other-models-ttl` / `effective_ttl` | PR-3 Yes (additive) | Built in `startup.rs`; passed through `iteration.rs` / `wave_orchestration.rs`; `wave_scheduler.rs` test structs if the field grows |
| `batch.rs` chain gate | `chain && (exit_code != 0 \|\| !prd_complete)` | Rung-scoped HorizonStopped continues and seeds next; account-binding Stop still aborts | PR-3 Yes | Discriminator; do not exempt all HorizonStopped |
| `ask_or_defer` | `ask_or_defer(policy.ask_ttl_minutes)` at apply | `ask_or_defer(effective_ttl)` with `effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` | PR-3 Yes | CLI 15 + config 0 must not Defer |
| `compute_remaining_work_snapshot` | empty blackouts; tier ∈ unavailable_preview → not runnable | Clamp-eligible (walker lands on defined non-blacked lower rung) **is runnable**. `has_review = is_frontier_class(&id)` (PRE-PR-3) | PR-3 snapshot + PRE-PR-3 has_review | Factory + only-frontier-left + 6d → Proceed + clamp |
| Extra-mark helper | `exact_model_for(mapped_rung)` | HUD-family identity I then `exact_model_for == I` | PRE-PR-3 Yes | Fable+pin inverse; live-shaped rungs `None` |
| `AccountReaction::Stop` | One variant | `OperatorStopped` + `StopSpend` | PRE-PR-3 Yes | `iteration.rs` / `wave_scheduler.rs` exhaustive match; PR-3 inherit depends on it |

### Data Flow Contracts

See §2.6 table. Type transition to flag: OAuth `limits[].percent` may be `u64` **or** `f64` (parser already has both); remaining math is always `f64` (PR-2). Config difficulty strings (`"medium"`) stay strings until `difficulty_rank`.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `usage.rs:218–223` named keys include `seven_day_opus` / `seven_day_sonnet` | Fold | BREAKS (bug) | Drop those keys; keep `five_hour`, `seven_day` |
| `usage.rs:274` `percentage = max(util)` | Gate input | BREAKS (bug) | **PR-1:** max used of **account-binding** windows only (not remaining) |
| `account.rs:1068` `percentage < threshold` | Pre-iteration wait | **PR-1: keep used compare** (`>= usage_threshold`). PR-2: remaining > remaining_min |
| `account.rs:350` `load_usage_info` → `api_secs` | Post-output wait | BREAKS for Fable CLI | **FR-002:** ignore `api_secs` and `output_secs` for Fable/rung-scoped phrasing |
| `usage.rs:349` `usage_suggests_lifted` | Early-lift probe | **PR-1:** unused on Fable phrasing because probe is skipped. PR-2: remaining vs rule floor |
| `detection.rs:151` `is_rate_limited` | Outcome class | OK / widen | Live Fable sentence is RateLimit. 3600 override: model token + `limit` or “switch models”; not `/model` alone; not plain `reached your … limit` |
| `recovery.rs:654` uses `is_rate_limited` | Crash vs rate-limit | OK if detection widens | Prevents auto-block |
| `tests/reaction_parity.rs` spies | Seq/wave I/O | NEEDS REVIEW | Same call counts; new Fable fixture; probe/usage_gate **not** invoked on Fable phrasing; wave test (g): one 3600s wait, no `provider_blackouts.record` |
| `config.rs:138` `LOOP_USAGE_THRESHOLD` | Env | **PR-2 BREAKS** | Preflight error + new env in PR-2. PR-1 unchanged |
| `startup.rs:1000` `usage_threshold` | Gate params | **PR-2 BREAKS** | Pass remaining_min in PR-2 |
| FEAT-008 `handle_quota_deferral` | Provider blackout wait | OK if not reused for rungs | Sibling `handle_rung_only_empty_selection` **shipped PR-2**; Fable phrasing never `provider_blackouts.record` |
| `account.rs` apply `ask_or_defer(policy.ask_ttl_minutes)` ~1245/1252 | Ask TTL | BREAKS if CLI flag never arrives | Inject `effective_ttl` via `UsageParams` from `startup.rs` **before** this call |
| `compute_remaining_work_snapshot` (~1748–1765) | remaining-work / `other_rungs_runnable` | BREAKS factory + only-frontier-left (HorizonStopped) | Count clamp-eligible as runnable (FR-006 snapshot AC) |
| `model.rs` `EXPLICIT_MODEL` early return ~995–1007 | post-resolve clamp | BREAKS if only default-path tail is wrapped | Wrap those returns; `plan.model` from `exact_model_for` |
| `orchestrator.rs` `run_loop` always `IterationContext::new()` | inherit seed | BREAKS next-PRD inherit | Seed inherited map; copy onto `LoopResult` |
| `batch.rs` chain `!prd_complete` | `--chain` | BREAKS rung-scoped Stop inherit | Discriminator `account_quota_stopped`; do not exempt all HorizonStopped |
| `prompt/{sequential,slot}.rs` `PlanContext` | no rungs (landed) | BREAKS spawn clamp if left empty | Thread `unavailable_rungs` from `BuildPromptParams` / `SlotPromptParams` |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| Account-binding remaining/used low | Session / weekly-all | Wait | **PR-1:** wait iff used ≥ 92. **PR-2:** wait (≤1h, and 1h–12h capped at `MAX_WAIT_SECS`) / stop (>12h and nothing else can run) |
| Rung-scoped remaining low | Frontier weekly (Fable HUD) | Same wait as account | **PR-1:** not in the fold (no account wait). **PR-2+:** factory default **unavailable** (downgrade); ask only if operator forbade; never account wait. Extra-mark: Fable HUD + pin → frontier only (PRE-PR-3) |
| Fable/rung-scoped CLI RateLimit | `"You've reached your Fable limit"` | Crash / or API wait / or provider Blackout | Narrow predicate → `Wait { 3600 }` ignoring api/output; no Blackout; no probe. Plain `reached your … limit` keeps `api_secs` |
| Spend remaining low | Credits | Stop only on CLI text | Stop only at amount 0 for kinds `spend`/`credits`. **`extra_usage` $0 stays Ignore** (PRE-PR-3) |
| Extra-mark after pin, **Fable** HUD | Mapped-rung extra-mark parks standard | Frontier only (HUD-family identity) |
| Extra-mark after pin, **Opus** HUD | Marks standard+frontier | Unchanged (pin hole kept) |
| `seven_day_opus` named key | Rung-unavailable Standard | `rungs: None`; Ignore unless explicit rule |
| Scoped Wait + week 45% left | Probe lifts | Do not lift (wait-driving probe) |
| `.stop` during 3600 vs StopSpend | Seq/wave diverge | `OperatorStopped` (`was_stopped=true`) vs `StopSpend` (`was_stopped=false`, wave not exit 130) |
| `has_review` | substring `REVIEW` | `is_frontier_class` |
| PR-1 pin recipe | Required for parallel/wave | Until CONTRACT-002: **do not pin** (harmful). After PRE-PR-3: **optional, not required** |
| `ask` timeout | Operator forbade downgrade, other rungs work | n/a | **Ask path only** (factory never Ask). TTL 0 = **Defer, no sleep**. TTL > 0 re-reads `usagePolicy` + `routing.tierFallback` on `WaitTiming.stop_check_secs`. Outcome: Deferred / continue / StopSignaled — not `wait() -> bool`. Timeout does not set `was_stopped` / does not stop `--chain`; `.stop` does |
| `stop` | Far reset, nothing runnable | n/a | Halt this PRD. **Factory + only-frontier-left is not this path** (Proceed + clamp). “No fallback” Stop = forbade / no cheaper defined rung. Rung-scoped: **next PRD inherits** via `LoopResult` map + `account_quota_stopped == false`. Account-binding: chain stops. Beats `ask`. Do not exempt all HorizonStopped |
| Provider blackout (FEAT-008) | Whole Claude out | Spillover / defer | Unchanged. Must **not** fire on rung-scoped CLI phrasing. Spillover is **never** a working rung for rung-scoped decisions |
| Review class forces frontier | `class_route` | Always frontier **request** | Request unchanged; factory default `includeReview: true` **does** clamp. Operator `includeReview: false` defers |

### Inversion Checklist

- [x] Callers of `UsageInfo` / `check_and_wait` / `react_to_outputs` identified
- [x] RateLimit vs crash branching reviewed (`is_rate_limited`, auto-block)
- [x] Tests that pin max-used fold listed (`test_parse_oauth_usage_*`)
- [x] Session vs weekly vs scoped vs spend distinguished
- [x] `api_secs` must not win over Fable 3600 (FR-002)
- [x] Early-lift probe / usage-gate short-circuit skipped for Fable phrasing (FR-002)
- [x] Spillover must not Blackout whole Claude on Fable CLI (FR-002)
- [x] PR-1 remaining invert closed (keep used-percent)
- [ ] Implementer must re-check `tests/reaction_parity.rs` Anthropic I/O matrix after each PR
- [x] PR-3 snapshot / exclude / spawn clamp must be one path (architect re-pass 2026-09-07)
- [x] PR-3 CLI TTL must reach `ask_or_defer` via `UsageParams` (not LoopConfig-only)
- [x] PR-3 inherit needs `account_quota_stopped` discriminator (do not exempt all HorizonStopped)
- [x] Post-clamp `plan.model` from `exact_model_for`; wrap `EXPLICIT_MODEL` early return
- [x] PRE-PR-3 extra-mark cannot mark standard from Fable+pin; PR-3 walker cannot reintroduce `exact_model_for(mapped_rung)`
- [x] Scoped wait probe cannot use `info.percentage`; Fable CLI 3600 probe skip untouched
- [x] Seq/wave `.stop` vs StopSpend distinguished; `--chain` honest (wrapper split is PR-3 inherit dependency)

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/loop_engine/CLAUDE.md` | Update | Account-global reactions: remaining unit (PR-2), FR-002 narrow 3600 predicate, HUD-family extra-mark (PRE-PR-3; strike pin-required), wait-driving probe, `AccountReaction` split, rung vs provider blackout, default `tierFallback` high/`includeReview` true (never Ask), Ask-path TTL 0 = Defer, `effective_ttl` via `UsageParams`, dual predicate unchanged except Fable-phrasing probe skip. Automatic clamp of high/review still PR-3. Fable CLI 3600 residual if a Fable-routed task spawns |
| `CLAUDE.md` workflow / models section | Update | `LOOP_USAGE_REMAINING_MIN` (PR-2; reject `> 100`), `--use-other-models-ttl` (Ask opt-out; TTL 0 Defer), `set-usage-rule`, `set-tier-fallback` / `unset-tier-fallback` (JSON-null); **strike pin-required**; after PRE-PR-3 pin is optional; Grok-only recipe untouched |
| `docs/` architecture | Update if present | Quota buckets + rung policy; else CLAUDE.md is enough |

---

## 7. Open Questions

- [x] Frontier 5% left: wait or use standard? → **use standard** (operator: not critical)
- [x] Engine key: model family vs capability rung? → **rung** (`frontier` / `standard` / `cost-efficient` / `cheapest`)
- [x] `ask` vs `stop`? → horizon + other-rungs + TTL. Factory default is auto-downgrade (`tierFallback` high / `includeReview` true) via **`unavailable + Proceed`, never Ask**. Ask is the opt-out; Ask-path TTL 0 = **Defer, no sleep**.
- [x] PR split? → three PRs; PRE-PR-3 gate (2b) between PR-2 and PR-3 is **not a fourth product PR**. PR-1 does not claim auto-downgrade. Optional FEAT-007a/b/c split is **inside PR-3**.
- [x] Exact default `askTtlMinutes` when the flag is omitted → **0** (unattended loops never pause on frontier-out; `--use-other-models-ttl` raises it). Closed 2026-09-06 (phase seed + architect Suggested Revision 12).
- [x] Default auto-downgrade? → **yes**: `tierFallback.maxDifficulty: high`, `includeReview: true`, `includeForced: false` (human review 2026-09-06). Pin 3 ask path is the opt-out. Factory never Ask.
- [x] Horizon 1h–12h gap? → **wait, capped at `MAX_WAIT_SECS`**. 3h session waits; 5h-to-12h is cap-and-repark.
- [x] Shared-model ingest after the PR-1 pin? → **Struck mapped-rung extra-mark** (PRE-PR-3 / architect rev 1). HUD-family identity **union**: always family constant **plus** `scope.model.id`. Do **not** prefer snapshot id. Fable HUD + pin → frontier only. Opus HUD + pin (including snapshot id) → standard and frontier.
- [x] PRE-PR-3 gate (2b, not a fourth product PR)? → US-008–US-012 / FR-009–FR-011 / CONTRACT-002 **in this PRD**. Sidecar superseded. PR-3 depends on CONTRACT-002.
- [x] Off-ladder `tasks.model`? → `pub(crate)` `hud_tier_from_label` on the explicit `tasks.model` string; defer **that id only** if unavailable and `includeForced=false`. No accepted wait loop. No `family_token_from_id` / substring `tier_of`.
- [x] Snapshot vs clamp (PR-3 architect re-pass 2026-09-07)? → clamp-eligible counts as runnable; factory + only-frontier-left + 6d = Proceed + clamp, not HorizonStopped.
- [x] CLI TTL wiring? → `UsageParams` from `startup.rs` **before** `ask_or_defer`; LoopConfig-only is the known-bad.
- [x] Chain inherit? → discriminator `account_quota_stopped`; rung-scoped seeds next; do not exempt all HorizonStopped.
- [x] Three clamp sites? → snapshot + exclude + spawn as one path; `exact_model_for`; wrap `EXPLICIT_MODEL` early return. Do not ship exclude-post-clamp without spawn clamp.
- [x] Ask wait mapping? → richer Deferred / continue / StopSignaled; timeout does not set `was_stopped`.

---

## AA review (folded)

Architect report: `tasks/prd-quota-rung-policy-architect.md` (2026-09-06). First-pass verdict: **NEEDS_CHANGES**. Human accepted **all** Suggested Revisions 2026-09-06. Questions for User: **none** (none were asked).

| # | Concern | Resolution |
| --- | --- | --- |
| 1 | **Critical** — FR-002 vs live `react_to_outputs` / `decide_account_rate_limit`: after FR-001, `load_usage_info()` still returns a session `reset_at` (~5h); `resolve_wait_secs` prefers `api_secs`; Fable phrasing waits the session window unless api_secs is ignored. Validation “3600 not ~6 days” can pass with `api_secs=None` and still fail in production. | **Folded into FR-002 / US-002.** Coordinator returns `Wait { secs: blackout_fallback_secs }` **ignoring `api_secs` and `output_secs`**. Test (b) requires `api_secs = 6 days` + Fable output → wait 3600, not Blackout. |
| 2 | **Critical** — early-lift probe undoes the 3600s backoff. `probe_rate_limit_lifted` spawns `claude -p .` with no `-m`; usage-gate `check_and_wait` sees used 55% < 92 → BelowThreshold. | **Folded into FR-002 step 3 / US-002 / FR-007.** `react_to_outputs` must not run `usage_gate` / `probe_rate_limit_lifted` for Fable/rung-scoped phrasing; sleep is stop-signal-aware only. Test (d). |
| 3 | **High** — Fable RateLimit + FEAT-008 spillover blacks the whole Claude provider (`RateLimitAction::Blackout`), violating pin 1. | **Folded into FR-002 step 2 / US-002 / Non-Goals.** Even if spillover is enabled: Wait, no Blackout, no `provider_blackouts.record`. Test (c): `spillover_enabled = true` → Wait, blackout map empty. |
| 4 | **High** — FR-002 3600 is not the non-spillover fallback. Unknown reset uses `usage_fallback_wait` (300s). | **Folded into FR-002 step 2.** Fable phrasing → `Wait { secs: blackout_fallback_secs }` even when spillover is off; do not use `fallback_wait`. |
| 5 | **High** — US-001 remaining language in PR-1 inverts the gate. Live compare is used ≥ 92. Storing remaining 45 in `percentage` without flipping the compare luckily passes the live fixture and **fails** the inverse. | **Folded into FR-001 / US-001.** Keep `UsageInfo.percentage` as used 0–100. Live: `percentage ≈ 55`, `reset_at` = session. Inverse: weekly-all 100 → `percentage = 100`, `reset_at` = weekly. Latest reset among account-binding windows ≥ `usage_threshold`. Remaining-min wording dropped from US-001. Named `seven_day_opus` / `seven_day_sonnet` dropped from the fold. Remaining rename stays PR-2 (US-003). |
| 6 | **High** — dated `parse_reset_from_output("Sep 12, 12:59am")` in PR-1 is a landmine. Adding a date parser recreates the 6-day wait unless api_secs **and** output_secs are both overridden. | **Folded into FR-002 step 4 / US-002 / Non-Goals / edge-case table.** Do **not** add dated Sep 12 parsing in PR-1. Keep `None` for month-name tokens. Fable 3600 comes from the phrasing override. Dated parse deferred to PR-2/PR-3. |
| 7 | **High** — PR-2 “unavailable/ask back off 3600s” must not be account-global wait. `Wait { 3600 }` parks standard work. | **Folded into FR-004 apply layer / US-004 / edge-case table / Risks / Appendix PR-2.** Exclude unavailable rungs from the next selection; do not account-wait; 3600s only when the remaining queue cannot run. Proto-channel on `IterationContext` allowed; expiry + clamp stay PR-3. Do not reuse `handle_quota_deferral`. |
| 8 | **High** — PR-3 clamp must not reuse `ResolvedModelsConfig::model_for` (bidirectional, can walk **up** onto blacked frontier). | **Folded into FR-006 / US-006 / Public Contracts.** New down-only walker; do not call `model_for` for blackout clamp. Post-resolve clamp after all six rungs including `EXPLICIT_MODEL`; default `includeForced=false` defers pinned off-ladder ids. |
| 9 | **Medium** — `evaluate_quota(..., other_rungs_runnable: bool)` cannot express the heuristic (`tierFallback` needs the task list). | **Folded into FR-004 / US-004.** Split evaluate (per-bucket) vs apply (`account.rs` remaining work + `tierFallback`). Account-binding `wait`/`stop` can coexist with rung `unavailable`. `stop` beats `ask`. Signature no longer takes `other_rungs_runnable: bool`. |
| 10 | **Medium** — ingest mapping when two rungs share a model. After `set-tier claude frontier <standard-model>`, substring `(1)` matches both rungs for an Opus HUD row. | **Folded into FR-003 / Quality / Goals / edge-case table.** HUD label table **wins** for `display_name`. A shared binary model does not mark every matching rung unavailable. `tier_of` stays exact-match. |
| 11 | **Medium** — `is_rate_limited("switch models with /model")` as an independent match. Help text could false-RateLimit. | **Folded into FR-002 / US-002 / detection modified interface.** Match `reached your` ∧ `limit` or `fable limit`. `/model` alone is **not** sufficient. |
| 12 | **Low** — `askTtlMinutes` still an open question. Phase seed already pins default **0**. | **Folded into §7 (closed), US-004/US-005, FR-004/FR-005, Intended Outcome.** Default **0**. |

**Verdict:** first-pass **NEEDS_CHANGES**; human accepted Suggested Revisions **2026-09-06**. All twelve concerns are now spec (FR/US cited above). The only accepted product residuals from pass 1 are (a) PR-1 does not auto-downgrade (operator pin until PR-3; Fable CLI without the pin still waits 3600s), and (b) remaining `% left` banners / horizon / ask TTL / rung clamp stay PR-2 / PR-3. Those are the three-PR split, not dropped Critical/High items.

**Pass 2 (2026-09-06):** architect pass 2 **APPROVED**; no unresolved ≥ high. The twelve closed items above are not reopened.

**ACCEPTED residual (Medium — carry into PR-1 tasks):** key the 3600 Wait on a **narrow** Fable/rung-scoped predicate, not every RateLimit. Add a negative test: `You've hit your limit · resets 4pm` still uses `output_secs` / may Blackout.

**ACCEPTED residual (Low):** `reached your` ∧ `limit` is broader than Fable; acceptable for PR-1.

---

## Human review (folded)

Source: `tasks/prd-quota-rung-policy-human-review.md` (2026-09-06). Operator review blocked `/prd-tasks` until items 1–4 were spec. Items 5–8 and Medium folded in the same pass so task JSON cannot re-encode the contradictions.

**Verdict:** operator review **2026-09-06**; items 1–4 blocking; 5–8 + medium folded in the same pass. Pins 1–3 unchanged. Factory default now **is** the pin-3 downgrade instruction (`tierFallback.maxDifficulty: high`, `includeReview: true`); pin 3’s ask path is the opt-out.

| Item | Change | Spec |
| --- | --- | --- |
| 1 (blocking) | Default auto-downgrade is honest. Default `maxDifficulty: high`, `includeReview: true`. `ask`-continue uses the same eligibility as `tierFallback`. If the operator forbids downgrade, TTL expiry **defers**. Default `includeForced` stays **false**. | US-004, US-005, US-006, FR-004, FR-005. Struck US-006 “unset means no automatic downgrade”; struck FR-005 “implicit downgrade does not require `tierFallback`”. |
| 2 (blocking) | Horizon middle band 1h–12h: **wait, capped at `MAX_WAIT_SECS`**. With defaults a 3h session reset **waits**; the **5h-to-12h** band is still a cap-and-repark cycle. Same for rung-scoped rows that wait. | US-004, FR-004, edge-case table. |
| 3 (blocking) | After mapping `display_name` to a rung, **also mark every rung whose configured model string equals that rung’s model**. | **Struck PRE-PR-3 (architect rev 1).** That sentence extra-marks **standard** from a **Fable** HUD row after `set-tier claude frontier <opus>`. Replaced by HUD-family identity **union** (US-008 / FR-003 / CONTRACT-002). Pin hole for Opus HUD is kept via the constant **plus** snapshot id. |
| 4 (blocking) | Family-match the ingest adapter against the **explicit `tasks.model` string at resolve time**. If that family maps to an unavailable rung and `includeForced=false`, **defer**. Do not drop the AC. Do not document a wait loop as accepted. | US-006, FR-006, edge-case table. |
| 5 (PR-1) | 3600 override: model token (`fable\|opus\|sonnet\|haiku`) + `limit`, **or** co-occurrence with “switch models”. Plain `reached your … limit` is ordinary RateLimit (keep `api_secs`). | FR-002, US-002, tests (e)(f). |
| 6 (PR-1 recipe) | Pin is **required for parallel/wave**, not recommended: one Fable RateLimit sleeps the whole wave 3600s. | **Struck PRE-PR-3.** Until CONTRACT-002 the pin is **harmful**. After PRE-PR-3: pin **optional**. Factory exclude unsticks mixed standard. All-high/review clamp is PR-3. Fable CLI 3600 still fires if a Fable-routed task spawns. |
| 7 | `evaluate_quota` does **not** emit `ask`. Apply layer resolves ask/wait/stop. CONTRACT-001 must match. | FR-004, CONTRACT-001, public contracts, US-004. |
| 8 | PR-2 proto-channel: **replace on each successful evaluate**; keep snapshot on API fail (not run-scoped stickiness). | FR-004, FR-006, edge-case table, §2.6. |
| M | `models show`: keep US-007 live-fetch gate. | US-007 kept; contradictory §5.5 cut struck. |
| M | Rung-scoped stop: **next PRD inherits** the rung-unavailable decision (batch/process-local). Account-binding stop still stops the chain. | FR-005, US-006, US-004, edge-case table. |
| M | `ask` TTL > 0: **re-evaluate config on the stop-check cadence**. | US-005, FR-005. |
| M | Wave: one Fable RateLimit + two completions → exactly one 3600s wait, no `provider_blackouts.record`. | US-002, FR-002, FR-007, tests (g). |
| M | Spillover is **never** a working rung for rung-scoped decisions. | Glossary, FR-004, FR-005, US-004. |

Items 1–4 are mostly PR-2/PR-3 except item 6 (PR-1 recipe) and item 5 (FR-002 predicate). PR-1 stays independently shippable (used-percent fold + Fable coordinator).

---

## Architect re-pass PR-3 (folded)

Source: `tasks/prd-quota-rung-policy-pr3-architect.md` (2026-09-07). Grounded in landed main / PR-2 (`feat-quota-rung-policy-pr2` `5a00b15` / `main` `dacccb3`). First-pass verdict: **NEEDS_CHANGES**. Human accepted **all** Suggested Revisions 1–8 on 2026-09-07. Questions for User: **none**.

| # | Concern | Resolution |
| --- | --- | --- |
| 1 | **Critical** — remaining-work snapshot ignores clamp; factory + only-frontier-left still `HorizonStopped` | **Folded into US-006 / FR-006 / FR-004 apply.** Snapshot treats a task as runnable if the down-only walker would land on a defined non-blacked **lower** rung under current `tierFallback` eligibility. Factory + only-frontier-left + 6d reset → **Proceed + clamp, not HorizonStopped**. “No fallback” Stop remains forbade / no-cheaper-defined-rung. Mixed standard+frontier already proceeds under PR-2 exclude. |
| 2 | **Critical** — 10-file cap drops production spawn + inherit seed | **Folded into US-006 / FR-006 / §2.6.** Sequential: `iteration.rs` `BuildPromptParams` → `prompt/sequential.rs`. Wave: `wave_scheduler.rs` `SlotPromptParams` → `prompt/slot.rs`. `orchestrator.rs` seeds inherited map on `IterationContext` (today `run_loop` always `IterationContext::new()`) and copies the map onto `LoopResult`. `PlanContext.unavailable_rungs` is **PR-3, not landed**. Prefer extra files over a split. If split: 007a expiry+`active_rungs`; 007b walker+family-match+all three clamp sites; 007c inherit+chain discriminator. Do **not** ship exclude-post-clamp without spawn clamp. |
| 3 | **High** — clap never reaches `ask_or_defer` | **Folded into US-005 / FR-005 / §2.6.** Flag reaches `ask_or_defer` via `UsageParams` built in `startup.rs` and passed through `iteration.rs` / `wave_orchestration.rs` (and `wave_scheduler.rs` test `UsageParams` if that struct grows). `effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` **before** `ask_or_defer`. LoopConfig + execute-only is the known-bad. Apply already consumes `ask_ttl_minutes` at apply (~1245/1252). |
| 4 | **High** — next-PRD inherit collides with landed chain gate and one `HorizonStopped` | **Folded into US-006 / FR-005 / FR-006.** `LoopResult` carries the expiry map **and** `account_quota_stopped: bool`. `batch --chain`: account-binding Stop still aborts; rung-scoped HorizonStopped continues and seeds the next `LoopRunConfig` → orchestrator `ctx.unavailable_rungs`. Receiver: `active_rungs(&map, now)`. Do not exempt all HorizonStopped. Ask-timeout / Deferred stay off this path. |
| 5 | **High** — three clamp sites under-specified | **Folded into US-006 / FR-006.** Required together: (a) snapshot counts clamp-eligible as runnable; (b) `compute_quota_excluded_ids` uses post-clamp resolve; (c) spawn `resolve_execution_plan` clamps and rewrites `plan.model` via `exact_model_for`. Missing (a) parks. Missing (c) after (b) dispatches Fable. |
| 6 | **Medium** — US-005 AC vs landed apply (factory-through-Ask) | **Folded into US-005 / FR-005.** Factory/allowing = continue-via-unavailable (PR-2, never Ask). Ask-path TTL 0 = **Defer, no sleep**. Struck “TTL 0 continues on working rungs” as a factory sentence. Struck US-005 story “continues on whatever rung still works.” |
| 7 | **Medium** — stale PRD vs landed line numbers | **Folded into §2.6 / FR-004 / FR-006 / FR-008 / Affected Components.** `PlanContext.unavailable_rungs` is PR-3, not landed. `handle_rung_only_empty_selection` is shipped PR-2. Apply already consumes `ask_ttl_minutes` via `ask_or_defer` at apply time. `UsagePolicy` already lives on `ProjectConfig`. Wrap `EXPLICIT_MODEL` early return (~995–1007). |
| 8 | **Medium** — Ask re-eval cannot reuse `wait() -> bool` | **Folded into US-005 / FR-005.** Richer Ask wait outcome: Deferred / continue / StopSignaled. TTL > 0 re-reads `usagePolicy` + `routing.tierFallback` only on `WaitTiming.stop_check_secs`. Timeout must not set `was_stopped`. |
| 9 | **Low** — `models show` gate | **Folded into FR-008 / US-007.** Drive `list --remote`’s existing opt-in (`TASK_MGR_USE_API=1` / `check_opt_in`). Offline `models show` has no remaining percents. Key is the fetch. JSON-null `unset-tier-fallback`. |

**Verdict:** first-pass **NEEDS_CHANGES**; human accepted Suggested Revisions **1–8 on 2026-09-07**. All nine concerns are now spec (FR/US cited above). Do not reopen shipped PR-1/PR-2 product decisions (evaluate/apply split, remaining-percent, factory serde, Ask sleep, `HorizonStopped`, `handle_rung_only_empty_selection`, HashSet replace-on-evaluate).

**ACCEPTED residual (tightened PRE-PR-3 / architect rev 8):** after CONTRACT-002, the pin is **optional, not required** for mixed standard/medium work (factory exclude). All-high / review / explicit-frontier still cannot clamp until PR-3 ships snapshot + exclude + spawn as **one path** **and** inherit has the chain-gate discriminator. Do not claim the pin makes high-tier todos runnable under PR-2 exclude. Fable CLI RateLimit still sleeps the wave 3600s if a Fable-routed task actually spawns.

No Questions for User hanging.

---

## Architect review PRE-PR-3 (folded)

Source: `tasks/quota-rung-policy-PR-3-review-architect.md` (production-code-architect `01a07ca3-90f7-7890-b86e-c625551389d0`, 2026-09-07). Grounded in landed PR-2 `5a00b15`. First-pass verdict: **NEEDS_CHANGES**. Folded into **this parent PRD** (not the sidecar). Questions for User: **none**.

| # | Concern | Resolution |
| --- | --- | --- |
| 1 | **High** — preferring `scope.model.id` over the family constant kills Opus+pin extra-mark on snapshot ids | **FR-003 / US-008.** Identity **union**: always family constant **plus** id. Fixture: Opus + `id: "claude-opus-5-SNAPSHOT"` + frontier pin still extra-marks frontier |
| 2 | **High** — `Wait { secs, account_binding }` never reaches the production probe (`WaitFn = Fn(u64)`) | **FR-009 / US-010.** Keep post-output `WaitFn` unchanged. Pure `wait_probe_lifted` after apply. Thread `models` onto `QuotaPreflightParams` |
| 3 | **High** — dropping `extra_usage` from `is_spend_kind` still Stops via `account_low_is_amount_only` | **FR-011 / US-012.** Ignore at `evaluate_one` **before** amount-exhausted AccountLow |
| 4 | **High** — sequential `OperatorStopped` on `RateLimit` does not set `was_stopped` | **FR-010 / US-011.** Sequential `Empty` + `operator_stopped: true`. Wave exit **0** not 130. StopSpend HorizonStopped-shaped, exit 0 |
| 5 | **Medium** — apply-only live-shaped fixture does not prove mixed work is selected | **US-009.** `compute_remaining_work_snapshot` + `compute_quota_excluded_ids` with and without the pin |
| 6 | **Medium** — `check_and_wait` banner has no models | **FR-011.** `AccountReactionParams` gains `models`; `remaining_banner_for_run_models` |
| 7 | **Medium** — remaining-min `> 100` chokepoint | **FR-011.** `preflight_validate_and_probe` only (loop/batch) |
| 8 | **Low** — `prompt/core.rs` still `contains("REVIEW")` | **Out of slice** (§5.5) |
| 9 | **Low** — pin docs must not imply high/review runs on opus after PRE-PR-3 | **FR-006 residual / glossary / phased 2b.** Factory exclude unsticks **standard/medium**; all-high clamp is PR-3 |

**Verdict:** NEEDS_CHANGES; Suggested Revisions 1–9 folded into this parent 2026-09-07. Sidecar `tasks/quota-rung-policy-PR-3-review.md` is superseded as implementation SSoT.

---

## Architect review PRE-PR-3 pass 2 (folded)

Source: `tasks/quota-rung-policy-PR-3-review-architect.md` Pass 2 (production-code-architect `01a07cb4-9285-7313-b86c-83e2fb8bb505`, 2026-09-07). Parent `prd-quota-rung-policy.md` (US-008–US-012 / FR-009–FR-011 / CONTRACT-002). Sidecar superseded.

**Verdict:** **APPROVED**. Pass-1 Highs stay closed. Do not reopen. Do not add PR-3 walker / TTL / inherit. Questions for User: **none**.

| # | Polish (≤ medium) | How folded |
| --- | --- | --- |
| 1 | Public-contracts extra-mark helper still single `identity: &str` (“id or family constant”) vs FR-003/US-008 union | **Public contracts.** Helper takes an identity **set** (call once per member of I and union). Deleted single-`identity` **or** wording |
| 2 | US-010 locks the pure `wait_probe_lifted` function, not that production wrappers pass `Wait.account_binding` after apply | **US-010 / FR-009.** Wrappers / `_inner` must pass `Wait.account_binding` into `wait_probe_lifted` **after** apply. Test: scoped-only `Wait { account_binding: false }` + `UsageInfo { percentage: 45, oauth_json: live_shaped }` must not lift. Keep post-output `WaitFn = Fn(u64)` |
| 3 | FR-011 puts `models` on `AccountReactionParams`; landed `UsageGateFn` is still `(u8, &Path, u64)` | **US-012 / FR-011.** `check_and_wait` gets run models via the `react_to_outputs` closure over `AccountReactionParams.models` — **not** by widening `WaitFn` / `UsageGateFn` |
| 4 | CONTRACT-002 duplicated in §2.6 | **§2.6.** One definition under When to Emit; Public Boundaries is a pointer |

---

## PRE-PR-3 review (folded)

Source: review wave `tasks/review-quota-pr12-*.md` on tree `5a00b15` plus sidecar `tasks/quota-rung-policy-PR-3-review.md` (**superseded**). Human accepted treating this as a **PRE-PR-3 gate (2b)** between PR-2 and PR-3 — **not a fourth product PR**. Implementation stories are **US-008–US-012 in this parent**. Closed CODE-FIX Highs stay closed. Do not add `--use-other-models-ttl` / walker / inherit into this gate.

| Finding | Sev | Parent spec |
| --- | --- | --- |
| Extra-mark vs `exact_model_for(mapped_rung)` after pin (Fable HUD marks standard) | High | **FR-003 / US-008 / CONTRACT-002.** HUD-family identity **union** (architect rev 1). Fable HUD + pin → frontier only. Opus HUD + pin including snapshot id → standard **and** frontier |
| Unlabeled `seven_day_opus` / `seven_day_sonnet` family-token-mapped | High | **FR-003 step 4.** Still ingested (no allow-list), `rungs: None`. Live-shaped evaluate+apply → unavailable `{frontier}` only, factory Proceed |
| Scoped 6h Wait lifted by week 45% left | High/Med | **PRE-PR-3 gate / FR-004 apply.** Account-binding Wait may keep `usage_suggests_lifted` on `UsageInfo.percentage`. Scoped-only Wait must not lift on account remaining. Fable CLI 3600 still skips probe |
| Seq/wave `AccountReaction::Stop`; prefer-rung-scoped masks StopSpend | Medium | **PRE-PR-3 gate.** Split `OperatorStopped` vs `StopSpend`. Seq/wave wrappers agree. Prefer-rung-scoped must not skip sibling StopSpend. `.stop` during wait → `was_stopped=true`. Horizon/Deferred/StopSpend/rung-only empty are not operator stop. Wave StopSpend must not be exit 130. **PR-3 inherit dependency** |
| `has_review` substring `REVIEW` | Medium | **PRE-PR-3 / FR-004.** `has_review = is_frontier_class(&id)` |
| `extra_usage` $0 spend-stop | Medium | **PRE-PR-3 / FR-004.** Default ignore even at $0; drop from `is_spend_kind` |
| `remainingMinPercent` unbounded; post-output builtin banner | Low | **PRE-PR-3 / FR-004 / FR-008.** Reject `> 100`. Banner uses run models when `oauth_json` present |
| Pin recipe “required for parallel/wave” | High (docs) | **Header pin timeline / glossary / phased delivery.** Until CONTRACT-002: pin is **harmful** — **do not pin**. After PRE-PR-3: pin **optional, not required**. Factory exclude unsticks mixed standard. Automatic clamp of all-high/review is still PR-3 |
| PR-3 intake (snapshot, three clamp sites, CLI TTL, inherit, `exact_model_for`) | Critical/High (PR-3) | Already in **Architect re-pass PR-3 (folded)** + FR-005/006. Add: PR-3 depends on CONTRACT-002; walker must not reintroduce mapped-rung extra-mark; wrapper split is inherit dependency |
| Closed CODE-FIX Highs | — | **Do not reopen** |

**Verdict:** PRE-PR-3 **2b gate** accepted 2026-09-07. All findings above are now parent spec (US-008–US-012 / FR-009–FR-011). Sidecar superseded. PR-3 still FR-005 / FR-006 / FR-008. No Questions for User hanging.

---

## Appendix

### Related Documents

- Session plan `plan.md` (quota buckets + review comments)
- Architect report `tasks/prd-quota-rung-policy-architect.md` (folded 2026-09-06)
- Architect re-pass PR-3 `tasks/prd-quota-rung-policy-pr3-architect.md` (folded 2026-09-07)
- PRE-PR-3 architect `tasks/quota-rung-policy-PR-3-review-architect.md` (pass 1 folded; pass 2 **APPROVED** 2026-09-07)
- PRE-PR-3 sidecar `tasks/quota-rung-policy-PR-3-review.md` (**superseded**; folded into this parent 2026-09-07)
- PRE-PR-3 architect `tasks/quota-rung-policy-PR-3-review-architect.md` (folded 2026-09-07)
- Review wave `tasks/review-quota-pr12-prd-adversarial.md`, `tasks/review-quota-pr12-parity.md`, `tasks/review-quota-pr12-code-quality.md`, `tasks/review-quota-pr12-failure-modes.md` (2026-09-07)
- Human review `tasks/prd-quota-rung-policy-human-review.md` (folded 2026-09-06)
- `src/loop_engine/CLAUDE.md` — overflow ladder, blackout channel, dual predicate
- `src/loop_engine/usage.rs` — current fold
- `src/loop_engine/model.rs` — `CapabilityTier`, `tier_of`, `model_for`
- Learnings [5297] [5298] [5301] [5075] [4866] [4138] [5090] [3927]

### Glossary

- **Account-binding bucket**: session (`five_hour` / `kind=session`) or weekly-all (`seven_day` / `kind=weekly_all`). May wait or stop the account. Named `seven_day_opus` / `seven_day_sonnet` are **not** account-binding.
- **Rung-scoped bucket**: HUD per-model weekly (`kind=weekly_scoped`). Maps to a `CapabilityTier`. Does not wait the account.
- **Used percent (PR-1):** `UsageInfo.percentage` 0–100 used. Wait iff `>= usage_threshold` (92).
- **Remaining percent (PR-2):** `100 - used`, clamped 0–100. Floor default 8.
- **Working rungs**: defined, enabled, not-blacked capability rungs on the current provider. **Spillover is never a working rung for rung-scoped decisions.** PR-3: a task is also runnable if it is **clamp-eligible** (down-only walker lands on a defined non-blacked **lower** rung under current `tierFallback` eligibility) even if its resolved tier is currently blacked.
- **Clamp-eligible**: walker would land on a defined non-blacked lower rung under current `tierFallback`. Factory + only-frontier-left is clamp-eligible → Proceed + clamp. Forbade / no-cheaper-defined-rung is not → HorizonStopped when reset > 12h.
- **Three clamp sites (one path)**: (a) snapshot counts clamp-eligible as runnable; (b) `compute_quota_excluded_ids` uses post-clamp resolve; (c) spawn `resolve_execution_plan` clamps and sets `plan.model` from `exact_model_for` (wrap `EXPLICIT_MODEL` early return). Missing (a) parks. Missing (c) after (b) dispatches Fable.
- **Chain discriminator**: `LoopResult` expiry map **and** `account_quota_stopped: bool`. Account-binding Stop aborts `--chain`; rung-scoped HorizonStopped continues and seeds the next `LoopRunConfig`. Do not exempt all HorizonStopped.
- **`ask` TTL**: `--use-other-models-ttl` / `askTtlMinutes`. Default **0**. Ask-path only (factory never Ask). TTL 0 = **Defer, no sleep**. CLI overrides `policy.ask_ttl_minutes` **before** `ask_or_defer` via `UsageParams`. After TTL > 0 timeout: Deferred / continue / StopSignaled per current `tierFallback` eligibility; timeout does not set `was_stopped`.
- **HUD-family extra-mark (PRE-PR-3):** identity set I = **always** the built-in family constant for that HUD tier **plus** `scope.model.id` when present. Extra-mark every defined Claude rung whose `exact_model_for` equals any I. **Not** `exact_model_for(mapped_rung)`. **Not** prefer snapshot id over the constant.
- **Pin recipe:** until CONTRACT-002, `models set-tier claude frontier <standard-model>` is **harmful** — **do not pin**. After PRE-PR-3: pin is **optional, not required**. Factory exclude unsticks mixed standard work. Automatic clamp of all-high / review onto standard is still PR-3. Fable CLI RateLimit still sleeps the wave 3600s if a Fable-routed task actually spawns.

### Phased delivery

| PR | Ships | Unsticks `mw_integrations`? |
| --- | --- | --- |
| 1 | FR-001 + FR-002 only (used-percent account-binding fold; Fable CLI RateLimit coordinator: narrow 3600 Wait ignoring api/output, no Blackout, no probe, no dated Sep 12 parse) | False account park **yes**. Do **not** pin (PRE-PR-3: pin is harmful until CONTRACT-002). Sequential Fable-routed task: 3600s wait — accepted. |
| 2 | FR-003 + FR-004 ingest/heuristic (HUD map; remaining rename / `% left` banners); apply layer excludes unavailable rungs, does not emit `ask` from evaluate, proto-channel **replaced on each successful evaluate**; `handle_rung_only_empty_selection` sibling shipped; `UsagePolicy` on `ProjectConfig`; horizon middle band wait-capped. Extra-mark-as-mapped-rung is **struck** by 2b | Display + stop/wait heuristic; factory-default downgrade via `unavailable + Proceed` (never Ask); mixed standard+frontier proceeds **if extra-mark does not poison standard**. All-high / review / explicit-frontier queue still needs PR-3 snapshot AC |
| 2b PRE-PR-3 | CONTRACT-002 HUD-family extra-mark **union** + unlabeled named `rungs: None`; wait-driving probe (`wait_probe_lifted` after apply); `AccountReaction` `OperatorStopped`/`StopSpend` with sequential `Empty` mapping; `has_review = is_frontier_class`; extra_usage Ignore **at evaluate**; remaining-min `> 100` at loop/batch preflight; post-output run-model banner. Stories **US-008–US-012 / FR-009–FR-011 in this PRD**. **Not a fourth product PR** | Fable HUD + pin (if used) no longer parks standard. Pin **optional, not required** for mixed standard/medium. All-high/review clamp still PR-3. Live-shaped factory Proceed. Scoped 6h Wait not lifted by week 45% left. Seq/wave Stop mapping honest. PR-3 may start |
| 3 | FR-005 Ask TTL (`effective_ttl` before `ask_or_defer` via `UsageParams` from `startup.rs`; Ask-path TTL 0 = Defer no sleep; richer Deferred/continue/StopSignaled; timeout does not `was_stopped`) + FR-006 rung channel (expiry map + `active_rungs`; three clamp sites as one path: snapshot clamp-eligible + post-clamp exclude + spawn clamp/`exact_model_for` wrapping `EXPLICIT_MODEL`; family-match via `hud_tier_from_label`; spawn threaders `iteration.rs`/`wave_scheduler.rs`/`orchestrator.rs`; inherit + `account_quota_stopped` chain discriminator; **must not reintroduce mapped-rung extra-mark**; depends on CONTRACT-002) + FR-008 rung-label CLI + JSON-null `unset-tier-fallback` + US-007 live-fetch (`list --remote` opt-in, offline show has no `% left`) | Automatic frontier→standard **without** a pin, including the all-high / review / explicit-frontier queue (factory + only-frontier-left + 6d → Proceed + clamp, not HorizonStopped) |

**CONTRACT-001** is a PR-2 predecessor. PR-1 may ship without it. **CONTRACT-002** is a PRE-PR-3 predecessor; PR-3 must not loop until it is green.

**PR-1 independent-ship check** (must hold after this fold):

| After PR-1 alone | True? |
| --- | --- |
| False account park on Fable weekly 95% / session 24% / week 55% is gone (gate uses account-binding used 55% < 92) | **Yes** |
| Inverse: weekly-all 100% still waits on the weekly reset | **Yes** |
| Fable CLI is `RateLimit`, not Crash; consecutive-failure / auto-block do not increment | **Yes** |
| That RateLimit backs off **3600s** (not Sep 12, not session 5h, not 300s, not 30s probe-lift, not provider blackout) when the **narrow** predicate matches; plain `reached your … limit` keeps `api_secs` | **Yes** (FR-002 coordinator contract) |
| Wave: one Fable RateLimit + two completions → exactly one 3600s wait, no `provider_blackouts.record` | **Yes** (test g) |
| Automatic frontier→standard | **No** — mixed standard proceeds via PR-2 factory exclude **after** PRE-PR-3 extra-mark identity. Do **not** pin until CONTRACT-002 (harmful). After PRE-PR-3 pin is optional. Clamp of all-high/review is PR-3 |
| Remaining `% left` banners, horizon stop/ask, `--use-other-models-ttl`, rung channel | **No** — PR-2 / PRE-PR-3 / PR-3 |
| `mw_integrations` unsticks | **PR-1** removes the false account park. Mixed standard proceeds after PR-2 exclude **once extra-mark does not poison standard** (PRE-PR-3). All-high/review still needs PR-3 clamp |
