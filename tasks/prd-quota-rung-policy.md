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

Factory default (does **not** rewrite pin 3): `routing.tierFallback.maxDifficulty: high` and `includeReview: true` **are** the downgrade instruction. Pin 3’s “no instruction → ask” path is the **opt-out** (operator unsets `tierFallback`, narrows `maxDifficulty`, or sets `includeReview: false`). Default `includeForced` is **false**. `ask`-continue uses the **same eligibility** as `tierFallback`; if the operator forbade downgrade, TTL expiry **defers**, it does not continue.

Ship in **three PRs** (plan § Implementation order). This PRD is the full vision; `/prd-tasks` must keep the PR-1 slice independently shippable.

**Phase 1 / `/prd-tasks` slice:** ship **only PR-1** (FR-001 + FR-002). Remaining `% left` banners, horizon heuristic, ask TTL, and rung blackout stay PR-2 / PR-3 as listed in the Appendix. Do not invent a fourth PR.

**PR-1 operator recipe (required for parallel/wave):** after merge, `task-mgr models set-tier claude frontier <standard-model>` until PR-3. One Fable RateLimit in `react_to_outputs` sleeps the **whole wave** 3600s (account-global, once per wave), so the pin is **required** for parallel/wave, not recommended. Sequential without the pin: that task waits 3600s (accepted, no auto-downgrade). Automatic frontier→standard is PR-3.

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
- **Frontier** low → continue on **standard** (operator: “not critical, use opus”). Factory default `tierFallback.maxDifficulty: high` + `includeReview: true` **is** that instruction. No model id in engine state.
- `ask` waits `--use-other-models-ttl` minutes (default **0** from `usagePolicy.askTtlMinutes`). Continue on working rungs only if the same eligibility as `tierFallback` would accept the task; if the operator forbade downgrade, TTL expiry **defers**. TTL > 0 re-evaluates config on the stop-check cadence.
- Live `mw_integrations` unsticks after **PR-1** + a frontier→standard pin (**required** for parallel/wave) until PR-3’s automatic rung fallback exists.

**After PR-1 alone** (phase-1 ship): the false account park is gone (gate uses account-binding **used** 55% < 92); Fable/rung-scoped CLI (model token + `limit`, or co-occurrence with “switch models”) is `RateLimit` with a 3600s Wait that ignores API/output secs, does not Blackout the provider, and does not early-lift. The pin is **required** for parallel/wave (one Fable RateLimit sleeps the whole wave 3600s). Remaining `% left` banners are **not** in PR-1.

---

## 2. Goals

### Primary Goals

- [ ] **PR-1:** Account wait uses only **account-binding** buckets (session + weekly-all). A scoped frontier bucket must not set `percentage` / `reset_at` / exhausted. `UsageInfo.percentage` stays **used** 0–100; compare stays `>= usage_threshold` (92). Named `seven_day_opus` / `seven_day_sonnet` are **not** account-binding.
- [ ] **PR-2:** Operators and logs speak **remaining** (`76% left (3m)`), never used-percent. PR-1 stderr may still print `Usage: 55.0% (threshold: 92%)`.
- [ ] **PR-1:** Fable/rung-scoped CLI (model token `fable|opus|sonnet|haiku` + `limit`, **or** co-occurrence with “switch models”) is `RateLimit`, never crash/auto-block. Coordinator: `Wait { secs: blackout_fallback_secs }` (default 3600), ignoring `api_secs` and `output_secs`; no provider Blackout; no `usage_gate` / `probe_rate_limit_lifted` for that phrasing. Plain `reached your … limit` (no model token, no “switch models”) is ordinary RateLimit and **keeps** `api_secs`.
- [ ] **PR-2:** Horizon heuristic: wait (≤1h); **wait capped at `MAX_WAIT_SECS`** (1h–12h, including a 3h session reset); stop (>12h and nothing else can run). Factory default **is** a downgrade instruction (`tierFallback.maxDifficulty: high`, `includeReview: true`). Ask is the **opt-out** when the operator has no instruction. All thresholds live in `.task-mgr/config.json`.
- [ ] **PR-3:** `--use-other-models-ttl <minutes>` on `loop run` / `batch run` (0 allowed) caps how long `ask` blocks. Config default `askTtlMinutes` is **0**. Continue vs defer uses the **same eligibility** as `tierFallback`.
- [ ] **PR-2 ingest / PR-3 clamp:** Rung unavailability is keyed on `(Provider, CapabilityTier)`. HUD label table maps `display_name` (Fable→frontier, Opus→standard, Sonnet→cost-efficient, Haiku→cheapest); then **also mark every rung whose configured model string equals that mapped rung’s model**. Output remains `(Provider, CapabilityTier)`.
- [ ] Sequential and wave produce the same `QuotaDecision` for the same buckets+policy (parity lock). PR-1: same `RateLimit` / Wait-3600 outcome for Fable CLI text; wave: one Fable RateLimit + two completions → **exactly one** 3600s wait, no `provider_blackouts.record`.

### Success Metrics

- **PR-1 live fixture** (Fable 95% critical, session 24%, week 55%): gate does **not** wait; `UsageInfo.percentage ≈ 55` (used); `reset_at` = session (nothing ≥ 92).
- **PR-1 inverse:** weekly-all at 100% → `percentage = 100`, `reset_at` = weekly (narrowing does not disable the real weekly gate).
- **PR-1:** Fable CLI text (`You've reached your Fable limit` / model-token+`limit` / co-occurrence with “switch models”) → `IterationOutcome::RateLimit`; consecutive-failure / auto-block does not increment; `decide_account_rate_limit` Wait 3600 (not session 5h, not Sep 12, not 300s fallback, not provider Blackout); `usage_gate` / `probe_rate_limit_lifted` not invoked. Wave: one Fable RateLimit + two completions → exactly one 3600s wait, no `provider_blackouts.record`. Negative: `You've hit your limit · resets 4pm` still uses `output_secs` / may Blackout; `You've reached your session limit` keeps `api_secs`.
- **PR-2 (not PR-1):** stderr for the live fixture contains `76% left` / `5% left` and does not print used `95%`. Strike remaining banners from the PR-1 quality gate. PR-1 stderr may still print `Usage: 55.0% (threshold: 92%)`.
- `grep -n "claude-fable-5\\|fable" src/loop_engine/quota.rs src/loop_engine/engine.rs` → 0 (model ids stay in the ingest adapter + `model.rs` constants only). PR-1 may match `fable` only in `detection.rs` ingest of CLI phrasing.
- `tests/reaction_parity.rs` seq/wave identical on scoped-rung CLI text.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **PR-1 keeps used-percent.** `UsageInfo.percentage` is used 0–100; wait is `percentage >= usage_threshold` (default 92). **PR-2** flips the operator/gate unit: remaining percent is 0–100 everywhere (`usage_remaining_min` default 8). Never a 0.08 ratio. Old `used >= 92` ≡ new `remaining <= 8`.
- **Account-binding vs rung-scoped.** Session and weekly-all may `wait`/`stop` the account. A frontier weekly bucket may only mark **frontier** unavailable. Operator: frontier 5% left is “use standard”, not park. PR-1: scoped windows simply **do not enter** the account fold; they do not yet produce a rung-unavailable channel.
- **`severity` / `is_active` are display hints**, not the default low predicate (`is_active=true` on Fable was inferred from **one** sample). They must not set `UsageInfo` exhausted in PR-1. Opt-in via rule `when` in PR-2.
- **Spend `stop` only at remaining amount ≤ 0** (default rule, PR-2). 8% credits left must not halt (today stops only on the CLI spend message).
- **Dual predicates unchanged** ([5297]/[5298]). `LOOP_USAGE_CHECK_ENABLED=false` still skips pre-gate; post RateLimit still classifies and recovers. **Exception (PR-1 FR-002):** Fable/rung-scoped CLI phrasing skips `usage_gate` **and** `probe_rate_limit_lifted` even when Claude is enabled — otherwise the 3600s Wait is undone in 30s.
- **`handle_quota_deferral` must not see a rung-only deferral** and treat it as a provider blackout (learning 3927 stale-abort). New sibling for rung exhaustion. PR-1 Fable CLI must not call `provider_blackouts.record` even when spillover is enabled.
- **`tier_of` stays exact-match.** API→rung mapping is ingest-only; the decision layer only sees `CapabilityTier`. HUD label table maps `display_name`; ingest then **also marks every rung whose configured model string equals** that mapped rung’s model (string equality, not substring `tier_of`). After the PR-1 pin, an Opus HUD row that shares the frontier configured model marks **both** standard and frontier unavailable.

### Performance Requirements

- Usage fetch remains once per sequential iteration / once per wave (account-global). No per-slot usage GET.
- `evaluate_quota` is pure and sub-millisecond on a dozen buckets.
- `ask` TTL 0 must not sleep.

### Style Requirements

- Follow existing reaction coordinators: production entry + hermetic `_inner` + exhaustive param destructure (no `..`). Sequential and wave share the inner.
- `ui::*` for operator lines; `tracing` for diagnostics. Byte-stable wait banners stay on stderr.
- No `unwrap` on API JSON; skip malformed buckets.
- Do not reintroduce substring **tier** classification. The only allowed `contains` is ingest: API family token vs configured model **string** to pick a `CapabilityTier` when `display_name` is absent. HUD label table maps `display_name`. Extra shared-model marks use configured model **string equality**, not substring `tier_of`.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Fable `weekly_scoped` 95% `critical` + session 24% + week 55% | Live bug; max-used fold parks 6 days | **PR-1:** `percentage ≈ 55`, `reset_at` = session, **no wait**. **PR-2:** account remaining 45% left; frontier unavailable |
| weekly_all 100%, session 20% | Narrowing must not drop the real weekly gate | **PR-1:** `percentage = 100`, wait on **weekly** `reset_at` (latest if several account-binding windows ≥ `usage_threshold`) |
| `"You've reached your Fable limit"` | `is_rate_limited` misses it → crash → auto-block | `RateLimit`; PR-1 `Wait { 3600 }` ignoring `api_secs`/`output_secs`; no Blackout; no probe. Predicate: model token (`fable\|opus\|sonnet\|haiku`) + `limit`, **or** co-occurrence with “switch models” |
| `You've reached your session limit` / plain `reached your … limit` | Broad `reached your`∧`limit` would discard real `api_secs` | Ordinary RateLimit: **keep** `api_secs`; spillover may Blackout |
| `You've hit your limit · resets 4pm` | Account copy must not take the 3600 override | Still uses `output_secs` / may Blackout. `/model` alone is **not** sufficient |
| Dated `resets Sep 12, 12:59am` | Today token is `"sep"` → `None` | **PR-1: do not add dated month-name parsing.** Keep `None`. Fable 3600 comes from the phrasing override, not from parsing Sep 12. Dated parse is PR-2/PR-3 and still must not be the **account** wait when the bucket is rung-scoped |
| Named `seven_day_opus` / `seven_day_sonnet` in the JSON | `usage.rs:218–223` currently folds them as account windows | **Drop from the PR-1 fold.** Not account-binding. Do not wait on them |
| Frontier remapped to standard’s model (`set-tier claude frontier <standard-model>`) | After the PR-1 pin, HUD-only mapping left frontier resolving to the exhausted model | HUD table maps the label (Opus→standard, Fable→frontier); ingest then **also marks every rung whose configured model string equals** that mapped rung’s model. Output remains `(Provider, CapabilityTier)` |
| Explicit `tasks.model` on an off-ladder frontier id (`claude-fable-5-1`) | `tier_of` is `None`; medium difficulty would dispatch to Fable every cycle | Family-match the ingest adapter against the **explicit `tasks.model` string at resolve time**. If that family maps to an unavailable rung and `includeForced=false`, **defer**. Do not document a wait loop as accepted |
| Two `wait` buckets low (session 2h, weekly 6d) | Soonest-reset re-parks at 2h | Wait the **latest** reset among account-binding windows ≥ `usage_threshold` (PR-1) / low wait buckets (PR-2); early-lift may resume at 2h if session was the only low `wait` |
| `ask` TTL 0, other rungs work, operator **unset** `tierFallback` | Pin 3 opt-out (no downgrade instruction) | Do not sleep; **defer** (do not continue). Factory default is **not** this path — default `tierFallback` is set |
| `ask` TTL 0, factory defaults (`maxDifficulty: high`, `includeReview: true`) | Honest auto-downgrade | No ask; clamp **down** / `unavailable` for eligible tasks. Reviews included. Forced pins still deferred (`includeForced: false`) |
| `ask` TTL 15, no human | Deaf sleep would ignore a config write | Re-evaluate config on the **stop-check cadence**. On timeout: continue only if `tierFallback` eligibility would accept the task; else **defer** |
| Session reset in 3h (account-binding low) | Horizon table hole between 1h and 12h | **Wait, capped at `MAX_WAIT_SECS`**. With defaults a 3h session reset waits |
| Reset in 6h (5h-to-12h band), only frontier work left | Cap is 5h, stop horizon is 12h | **Cap-and-repark cycle** (wait `MAX_WAIT_SECS`, re-evaluate). Same for rung-scoped rows that wait |
| Reset in 6d, only frontier work left, no fallback | Long scoped horizon | **Stop this PRD** (not 5h-cap-loop). **Next PRD inherits** the rung-unavailable decision (batch/process-local) so it can clamp to standard instead of stopping again. Account-binding `stop` still stops the chain. Stderr names `set-tier-fallback` |
| `LOOP_USAGE_THRESHOLD=92` still set | Old used-percent env | **PR-2:** preflight **error** (legacy hard-break), not silent ignore. **PR-1:** env still drives used-percent threshold |
| Usage API 429 | Common in logs | Keep last rung-blackout snapshot; do not clear |
| `nimbus_quill` 0% remaining | Code-name bucket | Default `ignore` |
| PR-2 `unavailable` / `ask` without full blackout channel | Skip-wait hot-loops the same frontier task **or** parks standard if implemented as account Wait | **Exclude unavailable rungs from the next selection; do not account-wait.** 3600s only when the remaining queue cannot run. Proto-channel `HashSet<(Provider, CapabilityTier)>` on `IterationContext` allowed; **replace on each successful evaluate**; keep snapshot on API fail (not run-scoped stickiness). Expiry + `resolve_execution_plan` clamp stay PR-3. Do not reuse `handle_quota_deferral` |
| `LOOP_USAGE_CHECK_ENABLED=false` | Dual predicate; evaluate may run only post-output | Pre-gate off; first Fable CLI hit still RateLimit (one consumed iteration) then recover via FR-002 Wait 3600 (no usage_gate/probe). PR-2 proto-channel still **replaced** on each successful evaluate |
| PR-1 without the standard pin | One Fable RateLimit sleeps the **whole wave** 3600s | Pin is **required for parallel/wave**, not recommended. Sequential without the pin: that task waits 3600s (accepted). Recipe: `task-mgr models set-tier claude frontier <standard-model>` |
| Wave: one Fable RateLimit + two completions | Account-global wait must not Blackout or double-sleep | Exactly **one** 3600s wait; no `provider_blackouts.record`. Spillover is **never** a working rung for rung-scoped decisions |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **Contract owner**: new `src/loop_engine/quota.rs` (pure `QuotaBucket` / `UsagePolicy` / `evaluate_quota` / per-bucket low/unavailable + account wait/stop inputs) — **PR-2**. PR-1 stays in `usage.rs` + `detection.rs` + `reactions/account.rs`. Ingest adapters stay in `usage.rs`. Apply in `reactions/account.rs` resolves `ask`/`wait`/`stop`. Rung blackout lives on `IterationContext` in `engine.rs` (PR-2 proto-channel without expiry, **replaced on each successful evaluate**, keep snapshot on API fail; PR-3 expiry + clamp). Rung clamp is a **new down-only walker** in `model.rs` (not `model_for`). Off-ladder `tasks.model` is family-matched at resolve time.
- **Consumers (2+ stories → CONTRACT-001)**: pre-iteration gate, post-output RateLimit, `models show` (policy print), CLI `set-usage-rule`, synthetic CLI→bucket, rung clamp / excluded ids. **Recommended task: `CONTRACT-001`.** PR-1 may ship without it.
- **Data Flow Contracts:**

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| OAuth JSON object window | `serde_json::Value` string keys → `QuotaBucket` | `json.get("five_hour")?.get("utilization")?.as_f64()` → PR-1: used as-is; PR-2: `remaining = (100.0 - util).clamp(0.0, 100.0)` |
| OAuth `limits[]` | array of objects; `kind: String`; `scope.model.display_name` | `limit["kind"].as_str()`; `limit["scope"]["model"]["display_name"]` → HUD table maps `display_name` to a rung; then **also mark every rung whose configured model string equals** that mapped rung’s model |
| Project config `usagePolicy` | `serde_json::Value` camelCase → `UsagePolicy` | `config["usagePolicy"]["remainingMinPercent"]`; `config["usagePolicy"]["rules"][i]["onLow"]`; `config["usagePolicy"]["askTtlMinutes"]` default **0** |
| `routing.tierFallback` | camelCase JSON → struct | `config["routing"]["tierFallback"]["maxDifficulty"]` (string `low\|medium\|high`; **default `"high"`**); `includeReview` default **true**; `includeForced` default **false** |
| `QuotaDecision.unavailable` | `Vec<(Provider, CapabilityTier)>` | `decision.unavailable.iter().any(|(p, t)| *p == Provider::Claude && *t == CapabilityTier::Frontier)` |
| `PlanContext.unavailable_rungs` | `&HashSet<(Provider, CapabilityTier)>` | `plan.unavailable_rungs.contains(&(Provider::Claude, CapabilityTier::Frontier))` |
| Loop CLI TTL | clap `u64` minutes → `LoopConfig` / run params | `--use-other-models-ttl 15` overrides `usagePolicy.askTtlMinutes` for this run |

### Modularity & Coupling Targets

- **Target public surface**: `quota.rs` types + `evaluate_quota`; `UsagePolicy` / `TierFallback` in `project_config.rs`; `ModelBlackoutState` **renamed in spirit to rung blackout** on `IterationContext` (key `(Provider, CapabilityTier)`); clap `--use-other-models-ttl`; `models set-usage-rule` / `set-tier-fallback`. No new DB columns.
- **Ownership**: ingest `usage.rs`; policy `quota.rs` (evaluate, per-bucket); apply `account.rs` (remaining work + `tierFallback`); clamp `model.rs` (new down-only walker); persist-nothing ephemeral channel `engine.rs`.
- **Coupling budget**: `quota.rs` must not import runners, clap, or SQLite. `model.rs` must not parse OAuth JSON. Reactions must not hardcode `weekly_scoped`. **`model.rs` must not call `model_for` for blackout clamp.**
- **Cohesion**: horizon defaults (`waitIfResetWithinMinutes`, `stopIfResetBeyondHours`, `askTtlMinutes` default 0) live next to `usagePolicy.rules`.

### When to Emit a CONTRACT-xxx Task

**`CONTRACT-001`** — `QuotaBucket` + `UsagePolicy` + `evaluate_quota` → per-bucket **low / unavailable** plus account **wait/stop inputs**. `evaluate_quota` does **not** emit `ask`. The apply layer in `account.rs` resolves `ask` / `wait` / `stop` from remaining work + `tierFallback`. Gate, post-output, CLI synthetic buckets, and `models show` all implement against that split. Priority 0–1, `taskType: "contract"`. Downstream FEAT/FIX stories depend on it.

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
- [ ] PR-1 pin is **required for parallel/wave** (one Fable RateLimit sleeps the whole wave 3600s)

### US-003: Remaining is the unit operators see (PR-2)

**As a** loop operator
**I want** every usage line to show `% left` and time left
**So that** the HUD and the loop agree (93% used → 7% left)

**Acceptance Criteria:**

- [ ] Banner: `session 76% left (3m) · week 45% left (5d 13h) · frontier 5% left (5d 13h) (floor 8%)`
- [ ] No used-percent in that banner
- [ ] Dollar/token buckets print in their unit when present
- [ ] `usage_threshold` renamed `usage_remaining_min` default 8; `LOOP_USAGE_REMAINING_MIN` overrides config `remainingMinPercent` overrides 8; `LOOP_USAGE_THRESHOLD` preflight errors
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
- [ ] Rung-scoped low + other rungs work + factory/allowing `tierFallback` → `unavailable` (downgrade), not ask
- [ ] Rung-scoped low + other rungs work + operator **forbade** downgrade (unset `tierFallback`, narrower `maxDifficulty`, `includeReview: false`) → apply emits `ask` (pin 3 opt-out)
- [ ] Explicit `onLow` on a matching rule wins over the heuristic
- [ ] Account-binding `wait`/`stop` **can coexist** with rung `unavailable`. `stop` beats `ask`
- [ ] `evaluate_quota` does **not** emit `ask`. Apply layer owns remaining-work + `tierFallback` and resolves `ask` / `wait` / `stop`. PR-2: exclude unavailable rungs from the next selection; do not account-wait; 3600s only when the remaining queue cannot run
- [ ] Spillover is **never** a working rung for rung-scoped decisions

### US-005: `--use-other-models-ttl` (ask timeout) (PR-3)

**As a** loop operator
**I want** to cap how long the loop waits for me when policy is ambiguous
**So that** after that many minutes it continues on whatever rung still works

**Acceptance Criteria:**

- [ ] `task-mgr loop run … --use-other-models-ttl 15` and `batch run` accept minutes including 0
- [ ] Flag overrides `usagePolicy.askTtlMinutes` for this run only. Config default is **0**
- [ ] `ask`-continue uses the **same eligibility** as `tierFallback` (`maxDifficulty` / `includeReview` / `includeForced`). If the operator forbade downgrade (unset `tierFallback`, narrower `maxDifficulty`, `includeReview: false`), TTL expiry **defers**, it does not continue
- [ ] TTL 0: no sleep; continue on working rungs immediately **only if** that eligibility accepts the task; else **defer**
- [ ] TTL > 0: **re-evaluate config on the stop-check cadence** (not a deaf fixed sleep); on timeout same eligibility rule (continue if allowed, else defer)
- [ ] Human resolution before TTL (config write / `.stop` is not required — if they set `tierFallback` or a usage rule, the next stop-check / evaluate uses it)
- [ ] `ask` that times out does **not** stop `batch --chain`; `ask` that is still waiting when the operator `.stop`s does
- [ ] Distinct stderr names continue vs defer: `ask: frontier 5% left; other rungs available; waiting 15m for policy (--use-other-models-ttl), then continuing on standard` **or** `… then deferring (tierFallback forbids)`

### US-006: Rung blackout + optional `tierFallback` (PR-3)

**As a** loop operator
**I want** frontier-out to run standard unless I forbade downgrade
**So that** reviews can stay on frontier (defer) while implementation continues

**Acceptance Criteria:**

- [ ] Blackout key `(Provider, CapabilityTier)` — engine never stores `fable`
- [ ] Factory default `tierFallback.maxDifficulty: high`, `includeReview: true`, `includeForced: false` — **automatic** clamp down for eligible tasks (reviews included). This **strikes** “unset means no automatic downgrade”
- [ ] Eligible tasks clamp **down** defined rungs skipping unavailable ones via a **new down-only walker** (must not call `ResolvedModelsConfig::model_for`)
- [ ] If the operator forbids downgrade (unset `tierFallback`, narrower `maxDifficulty`, `includeReview: false`), there is no clamp; `ask`/TTL expiry **defers**, it does not continue
- [ ] Post-resolve clamp after all six rungs (including `EXPLICIT_MODEL`)
- [ ] Off-ladder explicit pins: family-match the ingest adapter against the **explicit `tasks.model` string at resolve time**. If that family maps to an unavailable rung and `includeForced=false`, **defer**. Do not drop this AC. Do not document a wait loop as accepted
- [ ] Deferral stderr names the exact flag (`--include-review`, `--include-forced`, `unset-tier-fallback` / `set-tier-fallback`)
- [ ] All leftover work needs unavailable rungs, reset ≤ 12h (including the 1h–12h capped-wait band) → wait; reset > 12h → stop this PRD. **Next PRD inherits** the rung-unavailable decision (batch/process-local) so it can clamp to standard instead of stopping again. Account-binding `stop` still stops the chain
- [ ] Must not fall through `handle_quota_deferral` into stale-abort

### US-007: Policy CLI + `models show` (PR-3)

**As a** loop operator
**I want** to set per-bucket `onLow` and see remaining via a live fetch
**So that** I can sit out frontier, ask, or stop without editing JSON by hand

**Acceptance Criteria:**

- [ ] `task-mgr models set-usage-rule --kind weekly_scoped --on-low wait|unavailable|stop|ask|ignore`
- [ ] `task-mgr models set-tier-fallback <low|medium|high> [--include-review] [--include-forced]` / `unset-tier-fallback`
- [ ] `models show` prints policy from config; remaining numbers only with the same live-fetch gate as `models list --remote`
- [ ] Sparse `serde_json::Value` round-trip; unrelated keys preserved

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

The PR-1 pin (`models set-tier claude frontier <standard-model>`) is **required for parallel/wave**: `Wait` in `react_to_outputs_inner` fires once per wave and sleeps the whole loop, so a single Fable-routed task stalls the standard slots for 3600s. Sequential without the pin: that task waits 3600s (accepted, no auto-downgrade). Automatic frontier→standard is PR-3.

**Validation:** detection unit tests; coordinator tests (a)–(g). Do not treat “3600 not ~6 days” with `api_secs=None` as sufficient — test (b) requires `api_secs` populated.

### FR-003: Generic ingest (PR-2)

Walk every object sibling with utilization/dollars and every `limits[]` row into `QuotaBucket` (id, kind, label, measurements remaining-first, resets_at, severity, is_active). Map `scope.model.display_name` / `id` → one or more `(Provider, CapabilityTier)` via:

1. HUD label table maps `display_name` (case-insensitive prefix/token): Fable→frontier, Opus→standard, Sonnet→cost-efficient, Haiku→cheapest.
2. Else configured `model_for` substring of the family token against the **configured** model string (unlabeled ids only).
3. After mapping `display_name` to a rung, **also mark every rung whose configured model string equals that mapped rung’s model** (string equality, not substring `tier_of`). Output remains `(Provider, CapabilityTier)`.

This **replaces** the earlier “shared binary model must not mark every matching rung unavailable” (that was the hole after the PR-1 pin: an Opus HUD row marked only standard, while frontier still resolved to the same exhausted model). After `set-tier claude frontier <standard-model>`, an Opus HUD row maps to standard **and** marks frontier if frontier’s configured model string equals standard’s.

`tier_of` stays exact-match. Unknown → `None` (rule `ignore`). No allow-list of window names. Null objects skip.

### FR-004: `evaluate_quota` + apply layer (CONTRACT-001, PR-2)

**Split evaluate vs apply.** `evaluate_quota` is pure and **per-bucket**: it emits `ignore` / `unavailable` (rung-scoped low) plus account **wait/stop inputs** (remaining, reset_secs, kind, low) from buckets + `UsagePolicy` + resolved remaining-min (0–100). It does **not** emit `ask`. It does **not** take `other_rungs_runnable: bool`. CONTRACT-001 must not promise `ask` from evaluate’s inputs.

The **apply** layer in `account.rs` combines remaining work + `tierFallback` + those inputs and **resolves** `ask` / `wait` / `stop` / `unavailable`:

- Factory default `routing.tierFallback.maxDifficulty: high`, `includeReview: true`, `includeForced: false`. That **is** the downgrade instruction. Pin 3’s “no instruction → ask” is the **opt-out**.
- `ask`-continue uses the **same eligibility** as `tierFallback`. If the operator forbade downgrade (unset / narrower `maxDifficulty` / `includeReview: false`), TTL expiry **defers**, it does not continue.
- Account-binding `wait` / `stop` **can coexist** with rung `unavailable`.
- **`stop` beats `ask`.**
- Exclude unavailable rungs from the next selection; **do not account-wait** for a scoped rung.
- 3600s backoff only when the remaining queue cannot run.
- Spillover is **never** a working rung for rung-scoped decisions.
- Proto-channel on `IterationContext` (`HashSet<(Provider, CapabilityTier)>`, no expiry) is allowed in PR-2. **Replace on each successful evaluate**; keep snapshot on API fail (not run-scoped stickiness). Expiry + `resolve_execution_plan` clamp stay PR-3. Do not reuse `handle_quota_deferral`.

Default `onLow` if a rule does not pin it — **horizon heuristic** (product semantics the apply layer realizes):

| Condition | Action |
| --- | --- |
| Remaining percent > floor (and no opt-in `when`) | ignore (not low) |
| Low + explicit rule `onLow` | that action |
| Low + account-binding + reset_secs ≤ `waitIfResetWithinMinutes` (default 60) | `wait` |
| Low + account-binding + reset_secs in (60m, 12h] | **wait, capped at `MAX_WAIT_SECS`**. With defaults a **3h session reset waits**; the **5h-to-12h** band is still a cap-and-repark cycle |
| Low + account-binding + reset_secs > `stopIfResetBeyondHours` (default 12h) + no other runnable rung/provider | `stop` |
| Low + rung-scoped + other rungs runnable + factory/allowing `tierFallback` | `unavailable` (downgrade) |
| Low + rung-scoped + other rungs runnable + operator forbade downgrade | apply emits `ask` (evaluate does not) |
| Low + rung-scoped + **no** other rung runnable + reset ≤ 12h (including the 1h–12h band) | `wait`, capped at `MAX_WAIT_SECS` |
| Low + rung-scoped + no other rung + reset > stop horizon | `stop` this PRD; **next PRD inherits** the rung-unavailable decision |

Spend default rule: `onLow: stop` only when `remainingAmount lte 0`.

`wait` among multiple low wait buckets uses the **latest** reset. Early-lift uses **that rule’s** remaining floor (no magic 0.05). `askTtlMinutes` default is **0**.

### FR-005: `ask` vs `stop` (PR-3)

- **`stop`**: halt this run; reset in_progress→todo; do not continue on other rungs. Account-binding `stop` **stops the chain**. Rung-scoped stop-this-PRD: the **next PRD inherits** the rung-unavailable decision (batch/process-local) so it can clamp to standard instead of stopping again. Beats `ask` when both fire.
- **`ask`**: other rungs still work **and** the operator has no (or a forbidding) downgrade instruction — pin 3 opt-out, not the factory default. Wait `--use-other-models-ttl` / `askTtlMinutes` (**0** = no wait). TTL > 0 **re-evaluates config on the stop-check cadence**. Continue on working rungs **only if** the same eligibility as `tierFallback` would accept the task; if the operator forbade downgrade, TTL expiry **defers**. This **strikes** “implicit downgrade does not require `tierFallback`”. Stop-signal during ask aborts (exit 130) and stops the chain.

### FR-006: Rung blackout channel (PR-3)

Ephemeral `(Provider, CapabilityTier) → expiry`. Never persisted. Never touches `runner_overrides`. Replace-from-decision on successful evaluate; keep snapshot on API fail (same replace rule as the PR-2 proto-channel). Synthetic CLI bucket expiry = 3600 even when spillover is unconfigured.

`resolve_execution_plan`: after rungs pick `(provider, tier)`, if that pair is blacked and the task may fallback under `tierFallback` eligibility, walk **down** defined rungs to the first non-blacked via a **new helper**. **Do not call `ResolvedModelsConfig::model_for`** for blackout clamp — `model_for` is bidirectional nearest-defined (down, then up) and can land on a blacked frontier. Else defer via excluded ids.

Post-resolve clamp after **all six rungs** (including `EXPLICIT_MODEL`). Default `includeForced=false`.

**Off-ladder explicit pins:** family-match the ingest adapter against the **explicit `tasks.model` string at resolve time**. If that family maps to an unavailable rung and `includeForced=false`, **defer**. Do not drop this AC. Do not document a wait loop as accepted.

Overflow: skip escalate/`to_1m` targets whose **rung** is blacked.

### FR-007: Dual predicate + once-per-wave

Unchanged, except the FR-002 Fable-phrasing skip of `usage_gate` / `probe_rate_limit_lifted`. `account_usage_gate` / `react_to_outputs` remain the only coordinators; both paths destructure params exhaustively. Wave: one Fable RateLimit + two completions → **exactly one** 3600s wait, no `provider_blackouts.record`.

### FR-008: Operator display

**Remaining `% left` banners: PR-2** (US-003). PR-1 stderr may still print `Usage: 55.0% (threshold: 92%)`.

Rung labels in stderr: `frontier` / `standard` / `cost-efficient` / `cheapest`, not model names (PR-3 CLI / deferral lines). Deferral/ask lines name the exact CLI flag.

---

## 5. Non-Goals (Out of Scope)

- Buying or enabling extra_usage credits — ingested, default `ignore`
- Grok/Codex usage adapters — types must not block them; no fetch in v1
- A general expression language — ordered match rules + horizon heuristic only
- Changing FEAT-008 `provider_blackouts` / `promote_once` / review-forces-frontier **request**. Rung-scoped CLI phrasing must **not** record a provider blackout even when spillover is on
- Persisting last-fetched buckets
- Treating `severity` / `is_active` as low in the default predicate
- Reintroducing substring **tier** classification in `tier_of`
- Automatic frontier→standard in PR-1 (operators pin `set-tier` until PR-3; pin **required** for parallel/wave)
- Dated month-name `parse_reset_from_output` in PR-1
- Remaining-percent rename / `% left` banners in PR-1

---

## 5.5. Low-Value / High-Effort Areas (Explicit Cuts or Deferrals)

| Area / Capability | Why the value is low relative to cost | Rough effort cost | Recommended action |
| --- | --- | --- | --- |
| Per-task override of `onLow` | Policy is account-global; task pins already exist via `tasks.model` | High | **Cut** |
| Ingesting every Anthropic code-name as a first-class HUD row | Noise; default ignore is enough | Low-medium | Defer display; still ingest as `ignore` |
| Interactive TTY prompt during `ask` | Loops are often non-TTY / babysat | High | **Cut** — TTL + stderr + config/flag; no stdin interview |

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/usage.rs` — parse fold (PR-1); ingest adapters (PR-2)
- `src/loop_engine/quota.rs` — **new**, CONTRACT-001 (PR-2)
- `src/loop_engine/detection.rs` — Fable RateLimit phrasing (PR-1)
- `src/loop_engine/reactions/account.rs` — PR-1 FR-002 coordinator contract; PR-2 apply layer / exclude unavailable rungs; PR-3 ask TTL, rung-exhaustion sibling of `handle_quota_deferral`
- `src/loop_engine/config.rs` / clap `loop run` / `batch run` — remaining-min rename (PR-2), `--use-other-models-ttl` (PR-3)
- `src/loop_engine/project_config.rs` — `usagePolicy`, `tierFallback` (PR-2/PR-3)
- `src/loop_engine/engine.rs` — PR-2 proto-channel `HashSet` on `IterationContext`; PR-3 expiry
- `src/loop_engine/model.rs` — PR-3 new down-only walker; must not call `model_for` for clamp
- `src/loop_engine/reactions/pre_spawn.rs` — excluded ids (PR-2/PR-3)
- `src/loop_engine/prompt/{sequential,slot}.rs`, `iteration.rs`, `wave_scheduler.rs`, `orchestrator.rs`, `wave_orchestration.rs` — thread context
- `src/loop_engine/reactions/post_output.rs` — overflow skip by rung (PR-3)
- `src/commands/models/handlers.rs` + clap — rules / fallback CLI (PR-3)
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

**Selected Approach**: C, shipped as PR-1 (A) then PR-2 (buckets + heuristic) then PR-3 (rung blackout + `tierFallback` + ask TTL wired to selection).

**Phase 2 Foundation Check**: Generic `QuotaBucket` + `evaluate_quota` costs ~1 extra day vs a Fable-only special case and avoids a rewrite the next time Anthropic adds a scoped weekly row or dollar remaining. 1:10 holds. Horizon numbers in config avoid another hardcode round.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| PR-1 overpromises: Fable-routed tasks still fail until PR-3; wave sleeps 3600s | High (operator thinks they are unstuck) | High | PR-1 pin is **required for parallel/wave**: `models set-tier claude frontier <standard-model>` until PR-3. Sequential without the pin: that task waits 3600s (accepted). Empirical: live fixture + Fable CLI tests (a)–(g) |
| PR-2 skip-wait hot-loop **or** account-wait that parks standard | High | High if implemented as `check_and_wait` / `Wait { 3600 }` | Exclude unavailable rungs from next selection; do not account-wait; 3600s only when remaining queue cannot run. Proto-channel **replaced on each successful evaluate**; expiry + clamp stay PR-3 |
| Horizon heuristic stops a weekly-all 6-day account outage that the operator wanted to ride | Med | Med | Explicit `onLow: wait` on `kind=weekly_all`; 5h-cap cycle documented as accepted when they opt in |
| API `display_name` changes (“Fable 5.1”) | Med | Med | Mapping uses case-insensitive prefix/token; HUD table maps the label then extra-marks by configured model **string equality**; unknown → ignore, not wait |
| Dual-predicate regression | High | Low if tests hold | `tests/reaction_parity.rs` matrix unchanged ([5301]); FR-002 skip of probe/usage_gate is Fable-phrasing-only |
| Remaining invert in PR-1 (`percentage` stored as remaining, compare still used) | High | Closed by FR-001 | Keep used 0–100 in PR-1; remaining rename is PR-2 |

Top 3 = rows 1–3. Row 1 empirical test is the live fixture + operator recipe. Not a design blocker: operator already accepted the three-PR split.

### Security Considerations

- OAuth tokens stay in `oauth.rs`; quota code never logs them
- Sanitize API errors with existing `sanitize_error_tokens`
- No new network destination

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `quota::evaluate_quota` (PR-2) | `(buckets: &[QuotaBucket], policy: &UsagePolicy, remaining_min: u8) ->` per-bucket `ignore` / `unavailable` plus account wait/stop **inputs**. **Does not emit `ask`.** **No** `other_rungs_runnable: bool` | per-bucket facts | n/a (pure) | none |
| apply layer `account.rs` (PR-2) | evaluate outputs + remaining work + `tierFallback` + ask TTL | resolves `ask`/`wait`/`stop`/`unavailable`; `stop` beats `ask`; `ask`-continue uses same eligibility as `tierFallback` | n/a | wait / exclude / stop / defer as specified |
| `quota::ingest_oauth_value` (or `usage::parse_oauth_usage_json` returning buckets+legacy) | `&Value -> Vec<QuotaBucket>` | buckets | empty/skip rows | none |
| clap `--use-other-models-ttl` | `Option<u64>` minutes | parsed minutes | clap error | none |
| `models set-usage-rule` | `--kind/--id --on-low` | stderr ack | io/validate err | sparse config write |
| `models set-tier-fallback` | difficulty + flags | stderr ack | io/validate err | sparse config write |
| `model.rs` down-only walker (PR-3) | `(provider, start_tier, blacked: &HashSet<(Provider, CapabilityTier)>)` | first defined non-blacked **lower** rung, or none | n/a | none — **must not call `model_for`** |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `UsageInfo.percentage` | used 0–100 | **PR-1: still used 0–100**, fold only account-binding windows. **PR-2:** remaining 0–100 (`account_remaining`) | PR-2 Yes | PR-1: fixture tests only. PR-2: rename field; all tests flip |
| `LoopConfig.usage_threshold` | used 92 | **PR-1: unchanged.** **PR-2:** `usage_remaining_min` 8 | PR-2 Yes | Env rename; old env preflight error in PR-2 |
| `parse_oauth_usage_json` | max-used fold of every window including `seven_day_opus` / `seven_day_sonnet` / `weekly_scoped` | **PR-1:** max-used of account-binding windows only (`five_hour`/`seven_day` + `limits[]` `session`/`weekly_all`); latest reset among windows ≥ `usage_threshold` | Yes (bugfix) | Fixture tests: live ≈55/session; inverse 100/weekly |
| `is_rate_limited` | misses Fable sentence | still classifies the live Fable sentence as RateLimit; 3600 **override** is a **narrower** predicate (model token + `limit`, or “switch models”) | No (widens detection; narrows override) | Tests (a)(e)(f) |
| `decide_account_rate_limit` / `react_to_outputs` | `api_secs` wins; spillover → Blackout; probe every 30s | Narrow Fable/rung-scoped phrasing → `Wait { blackout_fallback_secs }` ignoring api/output; no Blackout; no usage_gate/probe. Plain `reached your … limit` keeps `api_secs` | Yes for that phrasing | Tests (b)(c)(d)(e)(f)(g) |
| `usage_suggests_lifted` | used < 92 / < 95 | **PR-2:** remaining > rule floor | PR-2 Yes | Same sites, new compare |

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
| FEAT-008 `handle_quota_deferral` | Provider blackout wait | OK if not reused for rungs | New sibling; Fable phrasing never `provider_blackouts.record` |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| Account-binding remaining/used low | Session / weekly-all | Wait | **PR-1:** wait iff used ≥ 92. **PR-2:** wait (≤1h, and 1h–12h capped at `MAX_WAIT_SECS`) / stop (>12h and nothing else can run) |
| Rung-scoped remaining low | Frontier weekly (Fable HUD) | Same wait as account | **PR-1:** not in the fold (no account wait). **PR-2+:** factory default **unavailable** (downgrade); ask only if operator forbade; never account wait |
| Fable/rung-scoped CLI RateLimit | `"You've reached your Fable limit"` | Crash / or API wait / or provider Blackout | Narrow predicate → `Wait { 3600 }` ignoring api/output; no Blackout; no probe. Plain `reached your … limit` keeps `api_secs` |
| Spend remaining low | Credits | Stop only on CLI text | Stop only at amount 0 unless rule says otherwise |
| `ask` timeout | Operator forbade downgrade, other rungs work | n/a | Same eligibility as `tierFallback`: continue if allowed, else **defer**. Default TTL **0**. Re-evaluate config on stop-check cadence |
| `stop` | Far reset, nothing runnable | n/a | Halt this PRD. Rung-scoped: **next PRD inherits** rung-unavailable. Account-binding: chain stops. Beats `ask` |
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

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/loop_engine/CLAUDE.md` | Update | Account-global reactions: remaining unit (PR-2), FR-002 narrow 3600 predicate, wave pin required, rung vs provider blackout, default `tierFallback` high/`includeReview` true, ask TTL default 0, dual predicate unchanged except Fable-phrasing probe skip |
| `CLAUDE.md` workflow / models section | Update | `LOOP_USAGE_REMAINING_MIN` (PR-2), `--use-other-models-ttl`, `set-usage-rule`, `set-tier-fallback`; **PR-1 pin required for parallel/wave** `models set-tier claude frontier <standard-model>`; Grok-only recipe untouched |
| `docs/` architecture | Update if present | Quota buckets + rung policy; else CLAUDE.md is enough |

---

## 7. Open Questions

- [x] Frontier 5% left: wait or use standard? → **use standard** (operator: not critical)
- [x] Engine key: model family vs capability rung? → **rung** (`frontier` / `standard` / `cost-efficient` / `cheapest`)
- [x] `ask` vs `stop`? → horizon + other-rungs + TTL. Factory default is auto-downgrade (`tierFallback` high / `includeReview` true). Ask is the opt-out; TTL expiry continues only if the same eligibility would accept the task, else **defers**.
- [x] PR split? → three PRs; PR-1 does not claim auto-downgrade
- [x] Exact default `askTtlMinutes` when the flag is omitted → **0** (unattended loops never pause on frontier-out; `--use-other-models-ttl` raises it). Closed 2026-09-06 (phase seed + architect Suggested Revision 12).
- [x] Default auto-downgrade? → **yes**: `tierFallback.maxDifficulty: high`, `includeReview: true`, `includeForced: false` (human review 2026-09-06). Pin 3 ask path is the opt-out. TTL expiry defers if the operator forbade.
- [x] Horizon 1h–12h gap? → **wait, capped at `MAX_WAIT_SECS`**. 3h session waits; 5h-to-12h is cap-and-repark.
- [x] Shared-model ingest after the PR-1 pin? → HUD table maps the label; **also mark every rung whose configured model string equals** that mapped rung’s model.
- [x] Off-ladder `tasks.model`? → family-match at resolve time; defer if unavailable and `includeForced=false`. No accepted wait loop.

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
| 3 (blocking) | After mapping `display_name` to a rung, **also mark every rung whose configured model string equals that rung’s model**. Replaces “shared binary model must not mark every matching rung unavailable”. HUD table still maps the label; extra mark is string equality, not substring `tier_of`. | FR-003, Goals, Quality, §2.6 ingest row, edge-case table. |
| 4 (blocking) | Family-match the ingest adapter against the **explicit `tasks.model` string at resolve time**. If that family maps to an unavailable rung and `includeForced=false`, **defer**. Do not drop the AC. Do not document a wait loop as accepted. | US-006, FR-006, edge-case table. |
| 5 (PR-1) | 3600 override: model token (`fable\|opus\|sonnet\|haiku`) + `limit`, **or** co-occurrence with “switch models”. Plain `reached your … limit` is ordinary RateLimit (keep `api_secs`). | FR-002, US-002, tests (e)(f). |
| 6 (PR-1 recipe) | Pin is **required for parallel/wave**, not recommended: one Fable RateLimit sleeps the whole wave 3600s. | PR-1 recipe, US-002, FR-002, Risks, Appendix. |
| 7 | `evaluate_quota` does **not** emit `ask`. Apply layer resolves ask/wait/stop. CONTRACT-001 must match. | FR-004, CONTRACT-001, public contracts, US-004. |
| 8 | PR-2 proto-channel: **replace on each successful evaluate**; keep snapshot on API fail (not run-scoped stickiness). | FR-004, FR-006, edge-case table, §2.6. |
| M | `models show`: keep US-007 live-fetch gate. | US-007 kept; contradictory §5.5 cut struck. |
| M | Rung-scoped stop: **next PRD inherits** the rung-unavailable decision (batch/process-local). Account-binding stop still stops the chain. | FR-005, US-006, US-004, edge-case table. |
| M | `ask` TTL > 0: **re-evaluate config on the stop-check cadence**. | US-005, FR-005. |
| M | Wave: one Fable RateLimit + two completions → exactly one 3600s wait, no `provider_blackouts.record`. | US-002, FR-002, FR-007, tests (g). |
| M | Spillover is **never** a working rung for rung-scoped decisions. | Glossary, FR-004, FR-005, US-004. |

Items 1–4 are mostly PR-2/PR-3 except item 6 (PR-1 recipe) and item 5 (FR-002 predicate). PR-1 stays independently shippable (used-percent fold + Fable coordinator).

---

## Appendix

### Related Documents

- Session plan `plan.md` (quota buckets + review comments)
- Architect report `tasks/prd-quota-rung-policy-architect.md` (folded 2026-09-06)
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
- **Working rungs**: defined, enabled, not-blacked capability rungs on the current provider. **Spillover is never a working rung for rung-scoped decisions.**
- **`ask` TTL**: `--use-other-models-ttl` / `askTtlMinutes`. Default **0**. After timeout, continue on working rungs **only if** `tierFallback` eligibility would accept the task; else defer.
- **PR-1 pin recipe**: `task-mgr models set-tier claude frontier <standard-model>` until PR-3 automatic clamp exists. **Required for parallel/wave** (one Fable RateLimit sleeps the whole wave 3600s). Sequential without the pin: that task waits 3600s (accepted, no auto-downgrade).

### Phased delivery

| PR | Ships | Unsticks `mw_integrations`? |
| --- | --- | --- |
| 1 | FR-001 + FR-002 only (used-percent account-binding fold; Fable CLI RateLimit coordinator: narrow 3600 Wait ignoring api/output, no Blackout, no probe, no dated Sep 12 parse; pin **required** for parallel/wave) | False account park **yes**. Frontier tasks still need the standard pin (**required** for wave). Sequential without the pin: 3600s wait — accepted. |
| 2 | FR-003 + FR-004 ingest/heuristic (HUD map + extra-mark by configured model string equality; remaining rename / `% left` banners); apply layer excludes unavailable rungs, does not emit `ask` from evaluate, proto-channel **replaced on each successful evaluate**; horizon middle band wait-capped | Display + stop/wait heuristic; factory-default downgrade via `unavailable`; standard work proceeds |
| 3 | FR-005 TTL (same eligibility as `tierFallback`; re-evaluate on stop-check; next PRD inherits rung-unavailable) + FR-006 rung channel (expiry + down-only walker + family-match explicit `tasks.model`) + FR-008 rung-label CLI + US-007 live-fetch | Automatic frontier→standard; long-horizon exit |

**CONTRACT-001** is a PR-2 predecessor. PR-1 may ship without it.

**PR-1 independent-ship check** (must hold after this fold):

| After PR-1 alone | True? |
| --- | --- |
| False account park on Fable weekly 95% / session 24% / week 55% is gone (gate uses account-binding used 55% < 92) | **Yes** |
| Inverse: weekly-all 100% still waits on the weekly reset | **Yes** |
| Fable CLI is `RateLimit`, not Crash; consecutive-failure / auto-block do not increment | **Yes** |
| That RateLimit backs off **3600s** (not Sep 12, not session 5h, not 300s, not 30s probe-lift, not provider blackout) when the **narrow** predicate matches; plain `reached your … limit` keeps `api_secs` | **Yes** (FR-002 coordinator contract) |
| Wave: one Fable RateLimit + two completions → exactly one 3600s wait, no `provider_blackouts.record` | **Yes** (test g) |
| Automatic frontier→standard | **No** — pin `models set-tier claude frontier` to the standard model until PR-3. **Required for parallel/wave** |
| Remaining `% left` banners, horizon stop/ask, `--use-other-models-ttl`, rung channel | **No** — PR-2 / PR-3 |
| `mw_integrations` unsticks | **PR-1 + the pin** (required for wave), not PR-1 alone |
