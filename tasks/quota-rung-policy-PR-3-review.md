# PRD: Pre-PR-3 review-fix slice (quota rung policy)

> **Superseded as implementation SSoT (2026-09-07).** Folded into the parent `tasks/prd-quota-rung-policy.md` (US-008–US-012 / FR-009–FR-011, CONTRACT-002, architect revisions 1–9). Do not `/prd-tasks` this sidecar. Keep it as working notes from the review wave.

**Type**: Bug Fix + Enhancement
**Priority**: P0 (Critical) — PR-3 clamp/TTL/CLI must not ship on the PR-2 base as reviewed
**Author**: Grok
**Created**: 2026-09-07
**Status**: Superseded — folded into parent PRD

> **This is a PRE-PR-3 gate, not PR-3.** Ship this slice (or fold its stories as the first tasks of PR-3 **before** FR-005/006/008) so the down-only walker, ask TTL, inherit, and CLI cannot inherit a poisoned ingest/apply/wrapper. Parent vision: `tasks/prd-quota-rung-policy.md`. Landed implementation: `feat/quota-rung-policy-pr2` `5a00b15` / local `main` `dacccb3`. Review wave: `tasks/review-quota-pr12-*.md` (2026-09-07). PR-3 architect re-pass (human-accepted Suggested Revisions): `tasks/prd-quota-rung-policy-pr3-architect.md`.

**Do not invent a fourth product PR.** This document is the review-intake law for the next loop. `/prd-tasks` may emit a short JSON (`quota-rung-policy-pr2b.json` or the first stories of `quota-rung-policy-pr3.json`) — still one PR-3 delivery after this gate is green.

Parent pins (unchanged):

1. A low **frontier** bucket is not an account emergency — continue on **standard**.
2. Engine language is **capability rungs**, never model ids except at the ingest adapter.
3. Factory `tierFallback.maxDifficulty: high` + `includeReview: true` **is** the downgrade instruction. Ask is the opt-out.

**This PRD strikes one parent clause.** Parent FR-003 / human-review item 3 (“also mark every rung whose configured model string equals the **mapped rung’s** model”) is bidirectional after `set-tier claude frontier <opus>` and extra-marks **standard** on a **Fable** HUD row. Replace with HUD-family identity (FR-001 here). Until that lands, the documented PR-1 pin is **harmful**; the safe operator recipe is **do not pin**.

---

## 1. Overview

### Problem Statement

The 2026-09-07 multi-angle review of PR-1+PR-2 found the evaluate/apply split, remaining `% left`, factory exclude, Fable 3600 coordinator, dual predicates, and proto-channel replace-on-success **sound**. Prior CODE-FIX Highs (hyphen boundary, switch-models line-scope, weekly-all >12h Stop, `includeForced` not global, `Wait{0}`, `LOOP_USAGE_CHECK_ENABLED`, rung-only empty ≠ stale-abort, horizon Stop ≠ `.stop` file) are **closed**.

Two Highs and several Mediums remain on the **landed** tree. They recreate the original park on new paths, or they make PR-3 clamp walk onto an already-blacked standard rung:

1. **Extra-mark uses the mapped rung’s configured model.** After the still-documented `set-tier claude frontier <opus>` pin, HUD `display_name: Fable` → frontier whose configured string **is** opus → standard is extra-marked → factory proto-channel excludes medium work. Tests only cover the Opus-HUD direction.
2. **Unlabeled `seven_day_opus` / `seven_day_sonnet` are family-token-mapped onto standard / cost-efficient.** `live_shaped_oauth_json()` sets them at utilization 100; ingest tests **assert** that mapping. `evaluate_quota` of that payload is untested. Combined with Fable → frontier, default `anchor=standard` never selects cheapest → Claude-only queue quota-empties / horizon-Stops despite week 45% left. A real live sample of `seven_day_sonnet` is utilization **1.0** (`test_parse_oauth_usage_sonnet_one_percent_does_not_park`) — the 100% fixture is artificial, but the mapping is locked in.
3. **Scoped 6h cap-and-repark is undone by the account-remaining probe.** Apply correctly emits `Wait { MAX_WAIT_SECS }`. Production `usage_suggests_lifted` is `UsageInfo.percentage > floor` (session/weekly-all min). Live week 45% left lifts in ~30s → empty-select → soft-stop. Never re-evaluates at 5h.
4. **`AccountReaction::Stop` wrappers diverge.** Sequential `.stop` during Fable 3600 → `RateLimit` + `operator_stopped: false` → exit 1. Wave → exit 130, `was_stopped: true`. `StopSpend` on wave looks like SIGINT.
5. **`has_review` is `id.contains("REVIEW")`**, not `is_frontier_class` (false-positive `REFACTOR-REVIEW-FINAL`, false-negative `MILESTONE-FINAL`).
6. **Prefer-rung-scoped `decide_item` masks `StopSpend`** on a mixed Fable + credits wave (ledger L4).
7. **`extra_usage` dollars ≤ 0 is spend-stop** (`is_spend_kind` includes `extra_usage`); parent default is ignore.
8. **`remainingMinPercent` / `LOOP_USAGE_REMAINING_MIN` are unbounded `u8`.**
9. Post-output `check_and_wait` still prints the **builtin** remaining banner; pre-gate extra-mark with run models is correct.
10. PR-3 architect re-pass (already human-accepted) is still not in the PR-3 **implementation** law as a gate: snapshot ignores clamp-eligible work; clap TTL never reaches `ask_or_defer`; `--chain` aborts on `!prd_complete` for rung-scoped Stop; three clamp sites can ship incomplete.

Shipping PR-3 clamp on (1) extra-marks standard from a Fable row, then the walker finds standard already blacked. Shipping inherit on (4)/(architect-4) still skips the next PRD or treats StopSpend as operator stop.

### Background

Parent PRD ships in three PRs. PR-1: account-binding fold + Fable RateLimit 3600. PR-2: generic buckets, remaining unit, horizon apply, proto-channel exclude without expiry/clamp. PR-3: ask TTL CLI, rung expiry + down-only walker, inherit, policy CLI.

Review artifacts (all 2026-09-07, tree `5a00b15`):

| Angle | File | Verdict |
| --- | --- | --- |
| Adversarial PRD | `tasks/review-quota-pr12-prd-adversarial.md` | PASS-WITH-RESIDUALS (missed Highs 1–2; closed prior CODE-FIX Highs) |
| Seq/wave parity | `tasks/review-quota-pr12-parity.md` | Shared inners aligned; Stop wrappers not |
| Code quality | `tasks/review-quota-pr12-code-quality.md` | REQUEST CHANGES (unlabeled `seven_day_*`) |
| Failure-mode hunt | `tasks/review-quota-pr12-failure-modes.md` | Extra-mark pin inverse, scoped probe lift, StopSpend mask |
| Architecture | conversation synthesis + parent §2.6 | Extra-mark identity wrong for Fable+pin; PR-3 clamp composition |

Related learnings: [5463] replace-on-evaluate; [5475] banner uses run models; [5472] / [5474] rung-only empty ≠ `handle_quota_deferral`; [5454] `includeForced` vs `includeReview`; [5379]/[5380] prefer-rung-scoped decide; [5366]/[5376] Fable skip load/gate/probe; [5075] once per wave.

### Intended Outcome

After this slice:

- HUD **Fable** low marks **frontier** only, even after `set-tier claude frontier <opus>`. HUD **Opus** low still extra-marks every rung whose configured model equals **Opus’s family model** (the PR-1 pin hole, kept).
- Unlabeled `seven_day_opus` / `seven_day_sonnet` are ingested (no allow-list) with `rungs: None` → evaluate Ignore. `evaluate_quota(ingest(live_shaped))` + apply with a default-anchor Claude queue → **Proceed**, unavailable **frontier only**.
- Scoped-only waits probe **wait-driving buckets**, not account remaining. 6h only-frontier cap-and-repark is not lifted at 30s by week 45% left.
- Sequential and wave map `.stop` during RateLimit wait and `StopSpend` the same way (operator-visible I/O + `was_stopped` / `--chain`).
- `has_review` uses `is_frontier_class`. `extra_usage` stays ignore. Remaining-min is 0–100 at preflight.
- Operator docs **strike** “pin required for parallel/wave” for the PR-2+ tree. Pin is optional and must not extra-mark standard on Fable. Automatic frontier→standard remains PR-3.
- PR-3 intake constraints in §8 of this document are binding on `quota-rung-policy-pr3.json` / prompt (architect Suggested Revisions 1–8). This slice does **not** implement them.

---

## 2. Goals

### Primary Goals

- [ ] **FR-001:** Extra-mark is HUD-family identity, not `exact_model_for(mapped_rung)` after operator pins. Fable HUD + frontier=opus pin → frontier only. Opus HUD + same pin → standard **and** frontier.
- [ ] **FR-002:** Named siblings without `scope.model` / `display_name` get `rungs: None`. Canonical live-shaped JSON through evaluate+apply does not mark standard/cost-efficient unavailable.
- [ ] **FR-003:** Production wait probe for apply `Wait` lifts iff the **wait-driving** buckets are no longer low (account-binding waits: account remaining; scoped-only waits: those scoped buckets). Do not call `usage_suggests_lifted` on account remaining for a scoped-only wait.
- [ ] **FR-004:** Split `AccountReaction::Stop` into operator-stop vs spend-stop. Seq and wave wrappers agree. Prefer-rung-scoped decide must not skip a sibling `StopSpend`.
- [ ] **FR-005:** `has_review = is_frontier_class(&id)`. `extra_usage` default ignore (even `dollars <= 0`). `remainingMinPercent` / `LOOP_USAGE_REMAINING_MIN` rejected if `> 100`. Post-output remaining banner uses run `ResolvedModelsConfig` when `oauth_json` is present.
- [ ] **Docs:** CLAUDE.md / `src/loop_engine/CLAUDE.md` / parent PRD extra-mark sentence updated. Pin is **not** required after this slice; it must not recreate the park.

### Success Metrics

- Test: Fable 95% + `models` with frontier configured string = opus → ingest rungs contain frontier, **not** standard. Opus 95% + same pin → both.
- Test: `evaluate_quota(ingest_oauth_value(live_shaped_oauth_json(), builtin), default, 8)` unavailable == `{ (Claude, Frontier) }` only. `apply_quota` with medium+high remaining snapshot + factory `tierFallback` → `Proceed`, not `Stop`.
- Test: only-frontier + 6h reset → apply `Wait { MAX_WAIT_SECS }`; hermetic preflight probe that reports account remaining 45% left must **not** return `WaitedAndReset` in 30s. Probe that reports the scoped Fable bucket remaining 50% **may** lift.
- Test: sequential and wave both treat `.stop` during RateLimit wait as `was_stopped` / `operator_stopped` (chain-abort) and `StopSpend` as quota-stop **without** `was_stopped` / without wave exit 130.
- `grep -n "claude-fable-5\\|fable" src/loop_engine/quota.rs src/loop_engine/engine.rs` still 0. HUD tokens stay in `usage.rs`.
- Parent PRD FR-003 extra-mark sentence rewritten to HUD-family identity (fold in the same change set as the code, or this file is SSoT until fold).

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Extra-mark compares rungs to the **HUD family’s canonical model string** (`FABLE_MODEL` / `OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL` for Claude HUD labels Fable/Opus/Sonnet/Haiku), **or** `scope.model.id` when present — **not** `exact_model_for(mapped_rung)` after pins. Then extra-mark every defined rung whose `exact_model_for` equals **that** identity (string equality, not substring `tier_of`, not `model_for`).
- Unlabeled named keys (`seven_day_opus`) may still be walked and may still get `kind: weekly_scoped` for rule matching; they must **not** carry rungs. Banner already skips unlabeled rows — keep that.
- `usage_suggests_lifted` on `UsageInfo.percentage` is valid **only** for account-binding waits. Scoped-only / proto-channel waits need a different predicate (wait-driving bucket remaining > floor, or skip probe).
- `.stop` during any usage/RateLimit wait is operator stop (`was_stopped: true`). Horizon Stop / Deferred / StopSpend / rung-only empty are **not**. Wave must not map StopSpend to exit 130.
- Spend check runs on **any** RateLimit item in the slice, not only `decide_item`. Prefer-rung-scoped still wins for the 3600 vs Blackout decision when no spend item is present.
- Dual predicates unchanged. Fable phrasing still skips load/gate/probe. Proto-channel still replace-on-success / keep-on-fail. Do not reuse `handle_quota_deferral`.
- `includeForced: false` stays not a global forbid (CODE-FIX-003).

### Performance Requirements

- Extra-mark and unlabeled-sibling changes are ingest-only (already once per sequential iteration / once per wave).
- Wait probe stays on the existing `WaitTiming.probe_secs` (30s). Do not add a second usage GET per tick beyond today’s `load_usage_info_with_threshold` in the probe closure — **reuse** the same load; classify lift from buckets already on `UsageInfo` / `oauth_json`.
- No per-slot usage GET.

### Style Requirements

- Production entry + hermetic `_inner` + exhaustive param destructure (no `..`). Seq and wave share the inner.
- `ui::*` for new operator lines; historical usage-wait banners may stay `eprintln!` (byte-stable stderr contract). Do not move wait banners to `tracing`.
- No `unwrap` on API JSON. HUD tokens only in `usage.rs`.
- `has_review` calls `is_frontier_class` — do not reimplement prefix stripping.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Fable HUD 95% + `set-tier claude frontier <opus>` | Documented pin; extra-mark today parks medium work | Rungs = `{frontier}` only. Factory Proceed + exclude frontier. Medium (standard) runs |
| Opus HUD 95% + same pin | Original pin hole: frontier still resolves to opus | Rungs = `{standard, frontier}`. Both excluded. Low/cost-efficient still runs |
| Opus HUD 95%, **no** pin | Must not over-mark | Rungs = `{standard}` only |
| `live_shaped_oauth_json()` (Fable 95, week 55, session 24, `seven_day_opus`/`sonnet` 100) | Canonical fixture; ingest test currently locks the defect | Evaluate unavailable = `{frontier}` only. Apply factory + medium/high snapshot → Proceed |
| Live `seven_day_sonnet` utilization **1.0** | Real sample; 1% used = 99% left | Ignore (above floor). Must not mark cost-efficient |
| Only-frontier-left, reset 6h, week 45% left | Apply Wait 5h; probe today lifts | Wait capped; probe does not lift on account remaining. Cap-and-repark |
| Only-frontier-left, reset 6h, Fable HUD now 50% left | Genuine recovery | Probe **may** lift; replace proto-channel on next successful evaluate |
| Account-binding session 3h, week 45% left | Must not break CODE-FIX-002 / 3h wait | Account wait still uses account remaining for lift; 3h waits capped |
| Mixed wave: Fable RateLimit + spend/credits RateLimit | Prefer-rung-scoped masks StopSpend | StopSpend wins (stop loop, no 3600, no Blackout) |
| Mixed wave: Fable RateLimit + `hit your limit · resets 4pm` | 3600 vs Blackout | Prefer Fable: Wait 3600, no Blackout (existing CODE-FIX-001) |
| `.stop` during Fable 3600, sequential | Today exit 1, `was_stopped=false` | Operator stop: `operator_stopped=true`, `was_stopped=true`, chain aborts. Not exit 1 “stopped” |
| `.stop` during Fable 3600, wave | Today exit 130 `was_stopped=true` | Same operator-stop meaning as sequential (`was_stopped=true`). Exit 130 vs 0: pick **one** and lock both paths (recommended: 0 + `was_stopped`, matching `.stop` during usage wait) |
| `StopSpend` (credits, no reset), wave | Today exit 130 `was_stopped=true` | Quota stop, `was_stopped=false`, not “stop signal during rate-limit wait” |
| `REFACTOR-REVIEW-FINAL` remaining | `contains("REVIEW")` forbids factory | `is_frontier_class` false → factory still auto-unavailable |
| `MILESTONE-FINAL` remaining + `includeReview: false` | `contains` misses it | `has_review` true → Ask/Defer opt-out path |
| `extra_usage: { dollars: 0 }` | `is_spend_kind` includes extra_usage | Ignore (do not Stop). Spend/credits **named** buckets still stop at amount ≤ 0 |
| `LOOP_USAGE_REMAINING_MIN=200` | u8 unbounded | Preflight error. Same for `usagePolicy.remainingMinPercent > 100` |
| Post-output ordinary RateLimit after pin | Builtin banner | `check_and_wait` prints run-model extra-mark banner when `oauth_json` present |
| `LOOP_USAGE_CHECK_ENABLED=false` | Dual predicate | Pre still skips load. This slice must not re-enable it to “fix” proto-channel |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **Contract owner**: `src/loop_engine/usage.rs` extra-mark + unlabeled sibling rungs (**CONTRACT-002**). `src/loop_engine/reactions/account.rs` wait-probe predicate + `AccountReaction` variants. `is_frontier_class` stays in `model.rs` (do not copy).
- **Consumers (2+ stories → CONTRACT-002):** ingest, remaining banner, `evaluate_quota` (via `QuotaBucket.rungs`), apply proto-channel, PR-3 down-only walker (must see the same extra-mark identity).
- **Data Flow Contracts:**

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| HUD `display_name` → family model → extra-mark | `display_name: &str` → `CapabilityTier` via `hud_tier_from_label` → canonical `&str` (`FABLE_MODEL` / … or `scope.model.id`) → `Vec<(Provider, CapabilityTier)>` | `let family = hud_tier_from_label(name)?; let ident = scope_model_id.or_else(\|\| canonical_model_for(family)); extra_mark_rungs_matching(models, provider, ident)` |
| Named sibling without scope | `key: "seven_day_opus"` → `kind: "weekly_scoped"` → `rungs: None` | `ingest_named_sibling`: do **not** call `map_unlabeled_token` for rungs. Leave `rungs: None` |
| Wait-driving buckets | `QuotaApplyResult.account = Wait { secs }` + the `AccountLow` / scoped ids that produced `latest` | Probe: re-ingest run models; those bucket ids remaining > floor → lifted. Do **not** use `info.percentage` unless the wait was account-binding |
| `AccountReaction` | enum | Callers match `OperatorStopped` vs `StopSpend` vs `WaitedAndRetry` exhaustively (seq `iteration.rs` ~874, wave `wave_scheduler.rs` ~1101) |

### Modularity & Coupling Targets

- **Target public surface**: `pub(crate)` extra-mark helper taking identity `&str`; `AccountReaction` gains a variant (or replaces `Stop`). No new CLI. No new DB columns. No expiry map (PR-3).
- **Ownership**: ingest identity in `usage.rs`; probe classification in `account.rs`; review-class in `model.rs`.
- **Coupling budget**: `quota.rs` still must not import runners/clap/sqlite. `model.rs` still must not parse OAuth JSON. Do **not** copy HUD tokens into `quota.rs` / `engine.rs`.
- **Cohesion**: extra-mark tests live next to `ingest_extra_mark_after_frontier_pin_marks_standard_and_frontier` and **must add the Fable inverse**.

### When to Emit a CONTRACT-xxx Task

**`CONTRACT-002`** — HUD-family extra-mark identity + unlabeled sibling `rungs: None`. Priority 0–1, `taskType: "contract"`. Downstream FEAT/FIX stories depend on it. Do not bury this in a banner-only story.

---

## 3. User Stories

### US-001: Fable HUD + frontier pin must not park standard (CONTRACT-002)

**As a** loop operator
**I want** extra-mark keyed on the HUD family’s model identity
**So that** pinning frontier to opus (or not pinning) never marks standard unavailable from a Fable weekly row

**Acceptance Criteria:**

- [ ] After HUD maps `display_name`/`id` → rung R, extra-mark uses identity I = `scope.model.id` if present, else the **built-in family constant** for that HUD label (Fable→`FABLE_MODEL`, Opus→`OPUS_MODEL`, …), **not** `exact_model_for(R)` on the run config
- [ ] Extra-mark every defined rung where `exact_model_for(provider, tier) == Some(I)` (string equality)
- [ ] Fixture: frontier configured = opus, Fable 95% → rungs contain frontier, **not** standard
- [ ] Fixture: frontier configured = opus, Opus 95% → rungs contain standard **and** frontier (pin hole kept)
- [ ] Fixture: no pin, Opus 95% → standard only
- [ ] Production gate still re-ingests with run `ResolvedModelsConfig` (WIRE-FIX-001 / CODE-FIX-007 stay)
- [ ] `quota.rs` / `engine.rs` still have no `fable` / `opus` / `claude-fable-5` literals

### US-002: Live-shaped JSON must not quota-empty mixed work

**As a** loop operator
**I want** vestigial named `seven_day_opus` / `seven_day_sonnet` keys ignored for rung unavailability
**So that** the canonical live fixture continues on standard when only Fable HUD is low

**Acceptance Criteria:**

- [ ] `ingest_named_sibling` does **not** call `map_unlabeled_token` to fill `rungs`. Unlabeled named keys → `rungs: None`
- [ ] Keys are still walked (no window-name allow-list). `kind` may still be `weekly_scoped` for explicit `onLow` rules by kind
- [ ] `ingest_live_fixture_emits_all_siblings_and_limits` **stops asserting** opus→Standard / sonnet→CostEfficient rungs; asserts `rungs.is_none()` instead
- [ ] New test: `evaluate_quota(ingest(live_shaped), default, 8)` unavailable == `{Frontier}` only
- [ ] New test: `apply_quota` of that eval with factory `tierFallback` and `RemainingWorkSnapshot { other_rungs_runnable: true, max_difficulty: Some("high"), .. }` → `Proceed`, unavailable contains frontier, **not** Stop
- [ ] Live `seven_day_sonnet` utilization 1.0 still Ignore (existing parse test stays)

### US-003: Scoped cap-and-repark is not lifted by healthy week remaining

**As a** loop operator
**I want** the wait probe to look at the buckets the wait is for
**So that** only-frontier-left + 6h reset waits up to `MAX_WAIT_SECS` instead of lifting in 30s and soft-stopping

**Acceptance Criteria:**

- [ ] Account-binding `Wait` (session / weekly_all) may keep `usage_suggests_lifted(&info, floor, _)` (`info.percentage` = min account remaining)
- [ ] Scoped-only `Wait` (apply used `scoped_wait_resets` / only-frontier) must **not** use `info.percentage`. Lift iff re-ingested wait-driving scoped bucket remaining > floor (or skip probe — then only `.stop` / timer ends the wait)
- [ ] Hermetic test: apply Wait `MAX_WAIT_SECS` + probe load returning live-shaped remaining 45% → probe false for scoped-only wait
- [ ] Hermetic test: same wait + scoped Fable remaining 50% → probe true
- [ ] Do not undo Fable CLI 3600 skip of probe (FR-002). This story is **pre-iteration** `account_quota_preflight` wait only
- [ ] `.stop` during the wait still returns `StopSignaled` / operator stop

### US-004: Seq/wave RateLimit Stop mapping + StopSpend vs Fable

**As a** loop operator
**I want** `.stop` and spend-stop to mean the same thing in sequential and wave
**So that** auto-review, exit codes, and `batch --chain` do not disagree, and a mixed Fable+credits wave stops instead of waiting 3600s

**Acceptance Criteria:**

- [ ] Split `AccountReaction::Stop` into `OperatorStopped` (wait interrupted by `.stop`) and `StopSpend` (credits/spend, no reset). Exhaustive match at **both** `iteration.rs` and `wave_scheduler.rs`
- [ ] `OperatorStopped`: `was_stopped=true` / `operator_stopped=true`; chain aborts. Sequential must not fall through orchestrator `_` → exit 1 with `was_stopped=false`
- [ ] `StopSpend`: `should_stop=true`, `was_stopped=false`, reason **not** `"stop signal during rate-limit wait"`. Wave exit **not** 130
- [ ] Horizon Stop / Deferred / rung-only empty unchanged (`was_stopped=false`)
- [ ] Before prefer-rung-scoped decide: if **any** RateLimit item is spend/credits (`is_spend_limit_message`) and `api_secs` is none → `StopSpend`. Fable item still wins 3600 vs Blackout when no spend sibling
- [ ] Parity tests lock both wrappers (not only `react_to_outputs_inner`)

### US-005: Review-class, extra_usage ignore, remaining-min bound, post-output banner

**As a** loop operator
**I want** apply’s review heuristic and leftover ingest/display nits to match existing SSoTs
**So that** `includeReview: false` and promotional buckets do not surprise, and banners stay honest after the pin

**Acceptance Criteria:**

- [ ] `compute_remaining_work_snapshot` sets `has_review` via `is_frontier_class(&id)` only. Tests: `MILESTONE-FINAL` true, `REFACTOR-REVIEW-FINAL` false, claimed `8d71d1f7-CODE-REVIEW-1` true
- [ ] `extra_usage` / promotional / `nimbus_quill` stay evaluate Ignore even when a dollars measurement is 0. Spend/credits **kinds** `spend` / `credits` still stop at amount ≤ 0. Remove `extra_usage` from `is_spend_kind` (or never emit AccountLow for that kind)
- [ ] `resolve_usage_remaining_min` / preflight reject `> 100` (env and `usagePolicy.remainingMinPercent`). Actionable error names `LOOP_USAGE_REMAINING_MIN`
- [ ] `check_and_wait` remaining banner uses `remaining_banner_for_run_models` when `oauth_json` is present (same as pre-gate CODE-FIX-007)
- [ ] Optional polish (do not block): evaluate+apply once in `run_account_quota_gate_inner` (drop double `Utc::now()`); compact `(3m)` vs `(3m 0s)` is **cut** (see §5.5)

---

## 4. Functional Requirements

### FR-001: HUD-family extra-mark (CONTRACT-002)

Replace parent FR-003 step 3.

1. HUD table maps `display_name` / unlabeled `id` token → `CapabilityTier` (`hud_tier_from_label`) — unchanged.
2. Identity I = `scope.model.id` if a non-empty string, else the built-in constant for that HUD tier on Claude (`FABLE_MODEL` for Frontier, `OPUS_MODEL` for Standard, `SONNET_MODEL` for CostEfficient, `HAIKU_MODEL` for Cheapest). Constants already live in `model.rs`; ingest may `use` them.
3. Extra-mark: every defined rung with `exact_model_for(provider, t) == Some(I)`.
4. Unlabeled ids **with** `scope.model` still use HUD table then (2)–(3). Unlabeled ids **without** display_name/id that today call `map_unlabeled_token` for `limits[]` may keep family-token substring **only** for `limits[]` rows that have a model id and no display_name (existing FR-003 step 2). Named object siblings without scope **must not** use that path (FR-002).

**Validation:** Fable+pin inverse test; Opus+pin existing test updated to still pass; no-pin Opus unchanged.

### FR-002: Unlabeled named siblings carry no rungs

`looks_rung_scoped_key` / `kind_for_named_key` may still classify `seven_day_*` as `weekly_scoped`. Do not fill `rungs` without `scope.model`. Evaluate default then Ignores (not account-binding, not rung-scoped). Explicit `onLow` by `kind: weekly_scoped` still matches labeled `limits[]` rows first; matching unlabeled named keys with `rungs: None` + explicit wait/stop emits AccountLow (CODE-FIX-006) — acceptable; default policy has no such rule.

**Validation:** live-shaped evaluate+apply tests in US-002. Change the ingest test that currently **requires** opus→Standard.

### FR-003: Wait-driving probe

Thread enough context from apply into `account_quota_preflight` / `execute_quota_account_action` to know whether `Wait` was account-binding or scoped-only (e.g. `has_account_binding_wait` on the action, or a `Wait { secs, source }` enum). Probe:

- account-binding: `usage_suggests_lifted` (today)
- scoped-only: parse `info.oauth_json` via `buckets_for_run_models`; lift iff every wait-driving scoped bucket is missing or remaining > floor
- Fable CLI post-output 3600: still **no** probe (do not touch `react_to_outputs_with_io_seams` skip)

**Validation:** hermetic preflight tests in US-003. Do not use wall-clock 30s.

### FR-004: AccountReaction discriminator + spend-vs-Fable

```rust
// conceptual — names may match local style
enum AccountReaction {
    None,
    WaitedAndRetry,
    OperatorStopped, // .stop during wait
    StopSpend,       // credits/spend, no reset
    RerouteAndRetry,
    ProceedWithSpillover,
}
```

`react_to_outputs_inner`: scan items for spend RateLimit **before** prefer-rung-scoped decide. If spend wins, do not Wait 3600.

Wrappers:

| Reaction | Sequential | Wave |
| --- | --- | --- |
| `OperatorStopped` | `should_stop`, `operator_stopped=true` → orchestrator `was_stopped=true` (existing Empty+operator_stopped branch **or** RateLimit+operator_stopped — pick one and lock both) | `was_stopped=true`, do not claim SIGINT unless it was a signal |
| `StopSpend` | `should_stop`, `operator_stopped=false`, outcome that orchestrator does **not** treat as `.stop` | `was_stopped=false`, reason `"usage/spend limit"`, exit 0 or 1 **not** 130 |

**Validation:** unit tests on inner + a reaction_parity (or wrapper) test per path. Known-bad: wave StopSpend → 130.

### FR-005: Snapshot / ignore / bounds / banner

As US-005. `is_spend_kind` must not include `extra_usage`. Preflight already hard-errors `LOOP_USAGE_THRESHOLD`; add `> 100` for remaining-min.

### FR-006: Documentation fold

In the same change set:

- CLAUDE.md / `src/loop_engine/CLAUDE.md`: strike “pin **required** for parallel/wave until PR-3”. Replace with: factory exclude unsticks standard work; pin is **not** required; if you pin frontier→opus, extra-mark uses HUD-family identity so Fable rows do not mark standard. Automatic clamp of high/review onto standard is still PR-3. Fable CLI RateLimit still sleeps the wave 3600s if a Fable-routed task actually spawns (`LOOP_USAGE_CHECK_ENABLED=false`, fetch fail, explicit `tasks.model`).
- Parent `prd-quota-rung-policy.md` FR-003 extra-mark bullet: rewrite to HUD-family identity (this FR-001).
- Do not claim PR-3 clamp has landed.

---

## 5. Non-Goals (Out of Scope)

- `--use-other-models-ttl` clap, `effective_ttl` before `ask_or_defer`, stop-check re-eval of `tierFallback` (**PR-3** FR-005; constraints in §8)
- Down-only walker, `PlanContext.unavailable_rungs`, expiry map, `active_rungs`, inherit, `account_quota_stopped`, `models set-usage-rule` / `set-tier-fallback` (**PR-3** FR-006/008; constraints in §8)
- Changing Fable CLI 3600 once-per-wave Wait, spillover non-Blackout, dual predicates, proto-channel replace-on-success
- Reimplementing `evaluate_quota` / `apply_quota` / remaining-percent / factory serde / `HorizonStopped` / `handle_rung_only_empty_selection`
- Dated month-name `parse_reset_from_output`
- Compact banner `(3m)` vs `(3m 0s)`
- Interactive TTY ask prompt
- Grok/Codex usage adapters
- New DB columns / persisting buckets

---

## 5.5. Low-Value / High-Effort Areas (Explicit Cuts or Deferrals)

| Area / Capability | Why the value is low relative to cost | Rough effort cost | Recommended action |
| --- | --- | --- | --- |
| Compact `(3m)` vs `format_duration` `(3m 0s)` | Display-only; AC `contains` already passes | Medium (shared formatter consumers) | **Cut** |
| Credits JSON key ingest (`credits` / `remainingAmount`) | Live HUD uses dollars; apply already tests the unit | Low-medium | **Defer** unless a live payload appears |
| Evaluate-once in the production gate (drop double `Utc::now()`) | Correctness window is 1s; Low in review | Low | Optional polish in US-005; do not block |
| Deaf `askTtlMinutes > 0` continue-when-forbade | Config knob exists; full re-eval is PR-3 | High if done without CLI TTL | **PR-3** (do not half-fix here) |

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/usage.rs` — extra-mark identity; unlabeled named `rungs: None`; live-shaped tests
- `src/loop_engine/model.rs` — ingest may `use` `FABLE_MODEL` / `OPUS_MODEL` / …; `is_frontier_class` unchanged
- `src/loop_engine/reactions/account.rs` — probe predicate; `AccountReaction`; spend-vs-Fable scan; `has_review`; `is_spend_kind`; optional single evaluate
- `src/loop_engine/iteration.rs` / `wave_scheduler.rs` / `orchestrator.rs` — Stop mapping
- `src/loop_engine/project_config.rs` / `config.rs` / `startup.rs` — remaining-min `> 100` preflight
- `tests/reaction_parity.rs` — wrapper Stop + mixed spend
- `CLAUDE.md`, `src/loop_engine/CLAUDE.md`, parent PRD extra-mark sentence

### Dependencies

- Landed PR-2 evaluate/apply, proto-channel, factory `tierFallback`, CODE-FIX-001–011, WIRE-FIX-001
- `CapabilityTier::ALL`, `exact_model_for`, `hud_tier_from_label`, `is_frontier_class`
- Do not depend on PR-3 walker existing

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| Extra-mark vs `exact_model_for(mapped_rung)` (landed) | Implements parent sentence literally | Fable+pin parks standard; poisons PR-3 clamp | **Rejected** |
| Extra-mark vs HUD-family canonical / `scope.model.id` | Pin hole kept for Opus HUD; Fable HUD cannot mark standard; PR-3 clamp sees the right set | Must not copy HUD tokens into `quota.rs` | **Preferred** |
| Extra-mark only same-direction “Opus HUD → also frontier if pin” special case | Small diff | Fourth special case; Fable+other pins still wrong | **Rejected** |
| Drop `seven_day_opus` keys (allow-list) | Simple | Violates “no window-name allow-list”; hides future named siblings | **Rejected** |
| Unlabeled named `rungs: None` | Keeps walk; evaluate Ignores; banner already skips | Explicit `onLow` by kind could still AccountLow those keys | **Preferred** |
| Scoped wait: skip probe entirely | Tiny | Won’t lift if Fable HUD recovers mid-wait | Alternative |
| Scoped wait: probe wait-driving buckets | Cap-and-repark + genuine recovery | Needs apply→preflight source bit | **Preferred** (1:10 vs PR-3 expiry) |
| Keep `AccountReaction::Stop`; fix wrappers only | Less API churn | StopSpend vs `.stop` still one variant; wave reason string hacks | **Rejected** |
| Split `OperatorStopped` / `StopSpend` | Exhaustive seq/wave match; `--chain` honest | Two call sites | **Preferred** |

**Selected Approach:** HUD-family extra-mark + unlabeled `rungs: None` + wait-driving probe + `AccountReaction` split.

**Phase 2 Foundation Check:** Extra-mark identity costs ~0.5 day now and saves PR-3 clamp walking onto blacked standard (~2+ weeks of “why did opus not run”). Probe-by-wait-source costs ~0.5 day and saves a fake 30s-then-stop that PR-3 expiry cannot paper over. Take both.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Extra-mark identity uses `FABLE_MODEL` while HUD `scope.model.id` is a newer snapshot id | Medium (miss extra-mark) | Medium | Prefer `scope.model.id` when present; constants are fallback for label-only rows |
| Live JSON really has `seven_day_opus` at ≥92% used | High (if we still mapped rungs) | Low on 2026-09-06 sample (max used was Fable 95; sonnet sample is 1%) | `rungs: None`; labeled `limits[]` is SSoT |
| Scoped probe skip (if implementer picks Alternative) leaves 5h wait when Fable recovered | Medium | Medium | Prefer wait-driving probe; test recovery lift |
| `AccountReaction` split misses a match arm | High (compile fail or wrong `--chain`) | Low | Exhaustive match; no `..` |
| Docs still say pin required | High (operators self-harm) | High today | FR-006 in the same change set |

No High Impact × High Likelihood residual after mitigations — extra-mark + docs in one slice is the blocker and is in scope.

### Security Considerations

- No new network, credentials, or DB. Probe still uses existing usage load (Claude-enabled ∧ env for pre-gate).
- Do not log OAuth JSON bodies (may contain account identifiers). Banner lines stay remaining `% left` only.

### Public Contracts

#### New Interfaces

| Module | Signature | Returns (success) | Side Effects |
| --- | --- | --- | --- |
| `usage::extra_mark_rungs_matching` (name flexible, `pub(crate)`) | `(models, provider, identity: &str) -> Vec<(Provider, CapabilityTier)>` | Deduped rungs whose `exact_model_for` equals identity | None (pure) |
| `usage::canonical_model_for_hud_tier` | `(CapabilityTier) -> Option<&'static str>` | Built-in family constant | None |

#### Modified Interfaces

| Module | Current | Proposed | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `extra_mark_rungs(models, provider, primary: CapabilityTier)` | Uses `exact_model_for(primary)` | Replace with identity-based helper; keep a thin wrapper only if tests need it | Yes (behavior) | Tests: add Fable+pin inverse; change live-shaped rungs asserts |
| `AccountReaction::Stop` | One variant | `OperatorStopped` + `StopSpend` | Yes (call sites) | `iteration.rs`, `wave_scheduler.rs`, parity tests |
| `is_spend_kind` | includes `extra_usage` | exclude `extra_usage` | Yes (behavior) | Evaluate Ignore for extra_usage dollars 0 |

### Data Flow Contracts

| Data Path | Key Types | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| OAuth limits[] HUD row | `limit["scope"]["model"]["display_name"]` / `["id"]` | See FR-001 steps 1–3 |
| Named sibling | object key `"seven_day_opus"` | `rungs: None`; `kind` from `kind_for_named_key` |
| Apply Wait source | `has_account_binding_wait: bool` computed in `apply_quota` (`account.rs` ~1213) | Thread on `QuotaAccountAction::Wait { secs, account_binding: bool }` or sibling field |
| Snapshot review | task `id: String` | `has_review \|\|= is_frontier_class(&id)` |

### Consumers of Changed Behavior

| File:Line (PR-2 tree) | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `usage.rs:544–590` `extra_mark_rungs` | Fable/Opus HUD → proto-channel | **BREAKS** (intended) | CONTRACT-002 + inverse test |
| `usage.rs:371–378` unlabeled named rungs | `seven_day_opus` → Standard | **BREAKS** (intended) | `rungs: None`; fix ingest test |
| `usage.rs:2100–2111` ingest live fixture | Asserts opus/sonnet rungs | **BREAKS** | Assert `rungs.is_none()` |
| `account.rs:1575–1584` preflight probe | `usage_suggests_lifted` on account remaining | **BREAKS** scoped waits | FR-003 |
| `account.rs:124` `AccountReaction::Stop` | Seq/wave wrappers | **BREAKS** | FR-004 exhaustive match |
| `iteration.rs:874` / `wave_scheduler.rs:1101` | Map Stop | **BREAKS** | Table in FR-004 |
| `account.rs:1741–1743` `has_review` | `includeReview` | **BREAKS** REFACTOR-REVIEW / MILESTONE-FINAL | `is_frontier_class` |
| `account.rs:1344–1348` `is_spend_kind` | extra_usage Stop | **BREAKS** | Drop `extra_usage` |
| `account.rs:2061–2062` `check_and_wait` banner | builtin remaining_banner | **BREAKS** display | `remaining_banner_for_run_models` |
| `CLAUDE.md` pin recipe | Operators pin frontier | **BREAKS** docs | FR-006 |

### Semantic Distinctions

| Context | Current (wrong / residual) | Required |
| --- | --- | --- |
| Extra-mark after pin, **Fable** HUD | Marks standard (configured string = opus) | Frontier only |
| Extra-mark after pin, **Opus** HUD | Marks standard+frontier | Unchanged (keep) |
| `seven_day_opus` named key | Rung-unavailable Standard | No rungs; Ignore unless explicit rule |
| Scoped Wait + week 45% left | Probe lifts | Do not lift |
| Account Wait + week 5% left | Probe does not lift (remaining ≤ floor) | Unchanged |
| `.stop` during 3600, sequential | exit 1, not operator stop | Operator stop / `was_stopped` |
| StopSpend, wave | exit 130 `was_stopped` | Spend stop, not operator stop |
| `has_review` | substring `REVIEW` | `is_frontier_class` |
| extra_usage $0 | Spend stop | Ignore |
| PR-1 pin recipe | Required for parallel/wave | Not required; must not park standard |

### Inversion Checklist

- [ ] Fable+pin extra-mark cannot mark standard (pin 1)
- [ ] Opus+pin extra-mark still marks frontier (pin hole)
- [ ] Live-shaped evaluate+apply cannot Stop a mixed queue
- [ ] Scoped wait probe cannot use `info.percentage`
- [ ] Fable CLI 3600 probe skip untouched
- [ ] Seq/wave `.stop` vs StopSpend distinguished; `--chain` honest
- [ ] Spend sibling beats Fable 3600; non-spend mixed wave still prefers Fable (no Blackout)
- [ ] `handle_quota_deferral` not reused; `includeForced` not global forbid
- [ ] Docs cannot still say pin required
- [ ] PR-3 clamp cannot inherit mapped-rung extra-mark (CONTRACT-002 is the identity PR-3 will read)
- [ ] `reaction_parity.rs` Anthropic I/O matrix re-checked (Fable skip, env, Claude disabled)

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `CLAUDE.md` | Update | Strike pin-required; describe HUD-family extra-mark; Fable CLI 3600 residual until PR-3 clamp |
| `src/loop_engine/CLAUDE.md` | Update | Extra-mark identity; unlabeled named siblings; wait-driving probe; AccountReaction split |
| `tasks/prd-quota-rung-policy.md` FR-003 | Fold | Replace “mapped rung’s model” with HUD-family identity |
| `docs/change_logs/` | Add | Pre-PR-3 review-fix slice |

### Institutional memory (embed)

- [5463] Replace proto-channel on successful evaluate; keep on API fail — do not “fix” scoped wait by skipping evaluate.
- [5475] Banners use run `ResolvedModelsConfig` — extend to `check_and_wait`.
- [5472]/[5474] Rung-only empty is a sibling of blackout deferral — Stop mapping must not route spend-stop into that sibling.
- [5454] `includeForced` ≠ `includeReview` — `has_review` must use `is_frontier_class` so `includeReview: false` is honest.
- [5379]/[5380] Prefer-rung-scoped decide — keep for 3600 vs Blackout; **do not** prefer Fable over StopSpend.
- [5366]/[5376] Fable skip load/gate/probe — out of scope to reopen.

---

## 7. Open Questions

None. Product picks below are binding (review + architect + parent pins). Do not re-ask.

| Topic | Pick |
| --- | --- |
| Extra-mark identity | HUD-family canonical / `scope.model.id`, not `exact_model_for(mapped_rung)` |
| Unlabeled `seven_day_*` rungs | `None` |
| Scoped wait probe | Wait-driving buckets, not account remaining |
| Wave `.stop` vs sequential | Both `was_stopped=true`; prefer exit 0 (match usage-wait `.stop`), not seq exit 1 |
| Compact `(3m)` banner | Cut |
| Ask TTL deaf-continue | PR-3, not this slice |

---

## 8. PR-3 intake constraints (binding, not implemented here)

These are **not** stories in this slice. They **are** law for `tasks/quota-rung-policy-pr3.json` / prompt / parent PRD fold. Source: architect re-pass `tasks/prd-quota-rung-policy-pr3-architect.md` (human accepted 2026-09-07) **plus** this review (High 1 identity, `--chain` `!prd_complete`, StopSpend wrappers).

PR-3 must not start looping until CONTRACT-002 (this slice) is green **or** the first PR-3 tasks **are** CONTRACT-002.

1. **Snapshot counts clamp-eligible as runnable.** `compute_remaining_work_snapshot` treats a task as runnable if the down-only walker would land on a defined non-blacked **lower** rung under current `tierFallback`. Factory + only-frontier-left + 6d → **Proceed + clamp**, not `HorizonStopped`. Missing this keeps the PR-1 pin for all-high/review queues.
2. **Three clamp sites are one path.** (a) snapshot, (b) `compute_quota_excluded_ids` post-clamp, (c) spawn `resolve_execution_plan` actually clamps and sets `plan.model` from `exact_model_for`. Do not ship (b) without (c) (dispatches Fable → 3600s wave). Thread `PlanContext.unavailable_rungs` through `iteration.rs` `BuildPromptParams` / `prompt/sequential.rs` and `wave_scheduler.rs` `SlotPromptParams` / `prompt/slot.rs`. Do not drop those files to keep a 10-file cap. Wrap `EXPLICIT_MODEL` early return. **Never** `model_for` / `finalize_plan` for clamp.
3. **Extra-mark identity is HUD-family (this PRD).** PR-3 walker must not reintroduce `exact_model_for(mapped_rung)` extra-mark. After CONTRACT-002, a Fable-low proto-channel does not contain standard, so clamp onto opus is possible.
4. **CLI TTL reaches `ask_or_defer`.** `effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` **before** apply. `touchesFiles` must include `startup.rs`, `iteration.rs`, `wave_orchestration.rs`. Config `askTtlMinutes: 0` + `--use-other-models-ttl 15` emits `Ask { 15 }`, not Defer. Factory/allowing stays unavailable+Proceed (never Ask). Ask-path TTL 0 = Defer, no sleep. TTL > 0 re-reads `usagePolicy` + `routing.tierFallback` on **stop-check** cadence. Timeout + forbade → Deferred, **not** `WaitedAndReset`. Timeout must not set `was_stopped`. `.stop` during ask still stops the chain.
5. **Inherit discriminator.** `LoopResult` carries expiry map **and** `account_quota_stopped: bool`. `batch.rs:712` `chain && (exit_code != 0 \|\| !prd_complete)` today skips the next PRD after **any** incomplete exit (including rung-scoped HorizonStopped). Exception **only** for rung-scoped Stop: continue chain and seed `ctx.unavailable_rungs` via `orchestrator` (`run_loop` today always `IterationContext::new()`). Do **not** exempt all `HorizonStopped` (weekly-all 6d must still abort the chain). Receiver: `active_rungs(&map, now)`.
6. **`active_rungs` internally.** `replace_unavailable_rungs`, `handle_rung_only_empty_selection`, `compute_quota_excluded_ids` take the map and call `active_rungs`. `wave_scheduler.rs` HashSet `insert` must become map+expiry. Silent ignore if any path still iterates raw keys.
7. **Off-ladder `tasks.model`:** `pub(crate)` `hud_tier_from_label` on the **explicit string** (`claude-fable-5-1` → frontier). Not `family_token_from_id`. `includeForced=false` defers **that id only** (do not regress CODE-FIX-003).
8. **`unset-tier-fallback` writes JSON `null`.** Key delete yields factory Some. `models show` remaining numbers only via `list --remote` opt-in (`check_opt_in` / `TASK_MGR_USE_API=1`); offline show is policy-only.
9. **This review’s wrapper split (FR-004) is a PR-3 dependency.** If PR-3 inherit keys off `was_stopped` / exit 130, StopSpend must already be distinct or inherit will treat credits-stop as operator `.stop`.
10. **Do not reimplement PR-2.** Evaluate/apply, remaining-percent, factory serde, CODE-FIX-008/009/010, proto-channel replace-on-evaluate stay; extend them.

### PR-3 inversion (how PR-3 fails against **landed + this slice**)

1. Clamp without CONTRACT-002 extra-mark → standard already blacked on Fable+pin.
2. Snapshot without clamp-eligible → all-high queue `HorizonStopped` → pin still required.
3. Exclude-post-clamp without spawn clamp → Fable dispatch → 3600s wave.
4. Clap on `LoopConfig` only → config 0 + CLI 15 still Defer.
5. Inherit without `account_quota_stopped` → `--chain` never seeds; or weekly-all Stop continues the chain.
6. Ask timeout → `StopSignaled` / `was_stopped` → kills `--chain`; or `WaitedAndReset` when forbade → continues on standard against human-review 1.
7. `model_for` clamp walks **up** onto blacked frontier.

Until snapshot + exclude + spawn clamp are one path, inherit has a chain-gate discriminator, and CONTRACT-002 identity is green, PR-3 does not retire the need for a frontier pin on all-high/review queues — but it **must not** tell operators to pin in a way that extra-marks standard.

---

## 9. Review finding → disposition

| Finding (2026-09-07 wave) | Sev | This slice | PR-3 |
| --- | --- | --- | --- |
| Extra-mark Fable+pin marks standard | High | **US-001 / FR-001** | Must consume HUD-family identity |
| Unlabeled `seven_day_*` quota-empty live-shaped evaluate | High | **US-002 / FR-002** | — |
| Scoped 6h wait lifted by account remaining | High/Med | **US-003 / FR-003** | Expiry map does not replace this probe |
| Seq/wave `AccountReaction::Stop` | Medium | **US-004 / FR-004** | Inherit/`was_stopped` depends on it |
| Prefer-rung-scoped masks StopSpend | Medium | **US-004** | — |
| `has_review` substring | Medium | **US-005** | `includeReview` Ask path |
| `extra_usage` $0 spend-stop | Medium | **US-005** | — |
| `remainingMinPercent` unbounded u8 | Low | **US-005** | CLI writes same field |
| Post-output builtin banner | Low | **US-005** | — |
| Double evaluate / two `Utc::now()` | Low | Optional US-005 | — |
| Banner `(3m 0s)` | Low | **Cut** | — |
| Ask TTL > 0 deaf continue | Medium | **Not here** | FR-005 / architect #3,#8 |
| `--chain` `!prd_complete` skips next PRD | Medium | **Not here** | Inherit discriminator |
| Snapshot ignores clamp-eligible | Critical (PR-3) | **Not here** | Architect #1 / §8.1 |
| Spawn `PlanContext` empty | Critical (PR-3) | **Not here** | Architect #2 / §8.2 |
| `model_for` clamp walk-up | High (PR-3) | **Not here** | Architect #5 / §8.2 |
| Closed CODE-FIX Highs (hyphen, weekly-all Stop, Wait{0}, env skip, includeForced, rung-only empty, horizon ≠ `.stop` file) | — | **Do not reopen** | Do not reimplement |

---

## Appendix

### Related documents

- `tasks/prd-quota-rung-policy.md` — parent vision
- `tasks/prd-quota-rung-policy-pr3-architect.md` — PR-3 architect re-pass
- `tasks/prd-goal-quota-rung-policy-ledger.md` — goal ledger
- `tasks/review-quota-pr12-prd-adversarial.md`
- `tasks/review-quota-pr12-parity.md`
- `tasks/review-quota-pr12-code-quality.md`
- `tasks/review-quota-pr12-failure-modes.md`

### Suggested `/prd-tasks` shape (not a fourth PR)

Independently shippable JSON slice, then PR-3 JSON as already authored (after fold of §8):

| ID | Title | Depends |
| --- | --- | --- |
| CONTRACT-002 | HUD-family extra-mark + unlabeled named `rungs: None` | — |
| FEAT-010 | Live-shaped evaluate+apply + Fable-pin inverse tests | CONTRACT-002 |
| FIX-012 | Wait-driving probe for scoped Wait | — |
| FIX-013 | `AccountReaction` split + spend-vs-Fable + wrapper parity | — |
| FIX-014 | `is_frontier_class` / extra_usage ignore / remaining-min bound / post-output banner | — |
| DOCS-001 | Strike pin-required; fold parent FR-003 extra-mark sentence | CONTRACT-002 |
| REVIEW-001 | Full suite + independent-ship: live-shaped Proceed; Fable+pin medium work not excluded | all |

IDs are suggestions for `/prd-tasks`; do not hand-edit `tasks/*.json`.
