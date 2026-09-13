# Failure-mode hunt — quota PR-1+PR-2

Workspace: `/home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-quota-rung-policy-pr2` @ `5a00b15`. Source was not modified. Verdicts are against live functions, not the PRD’s hopes.

**Verdict key.** WORKS = a realistic operator/account state reaches the bad outcome through a cited code path. BLOCKED = a defense sits on that path (cite the test if one exists). PARTIAL = the defense exists but is untested, apply-only, or seq-only.

---

## Highest-impact open attacks (first)

1. **Live-fixture named siblings recreate the park as a soft-stop (attack 1 variant / 8–9).** `ingest_oauth_value` maps `seven_day_opus` / `seven_day_sonnet` (fixture utilization 100 → remaining 0) onto standard / cost-efficient. `evaluate_quota` then emits `Unavailable` for those rungs **plus** Fable `weekly_scoped` → frontier. Factory apply Proceeds with three Claude rungs blacked; default-difficulty work (low→cost-efficient, medium→standard, high→frontier) is all excluded; `handle_rung_only_empty_selection` soft-stops. PR-1 correctly dropped those keys from the **account fold**, but PR-2 ingest walks every sibling. There is **no** `evaluate_quota`/`apply_quota` test on `live_shaped_oauth_json()`. If the live HUD JSON still carries those 100% named keys (the PR-1 fixture claims they exist), mixed-work loops do not “continue on opus” — they empty-select and exit 0.

2. **Scoped 6h cap-and-repark is undone by the account-remaining probe (attack 18).** `apply_quota` correctly emits `Wait { secs: MAX_WAIT_SECS }` for only-frontier + 6h reset. Production `account_quota_preflight` then waits with a probe that calls `usage_suggests_lifted` on **account** remaining (`UsageInfo.percentage` = min of session/weekly-all). Live fixture week is 45% left → probe lifts after ~30s → selection excludes frontier → quota-empty soft-stop. Never re-evaluates at 5h.

3. **Prefer-rung-scoped masks StopSpend on a mixed wave (attack 6).** `react_to_outputs_inner` prefers any Fable/rung-scoped RateLimit as `decide_item`. Spend copy is then never inspected. A wave with one spend-limit slot and one Fable slot Wait(3600)s instead of stopping.

4. **`askTtlMinutes > 0` deaf-sleeps, then continues into another Ask (attack 19).** Apply emits `Ask { ttl }`. Execute sleeps `ttl*60` with no config re-eval, returns `WaitedAndReset`, and the next preflight still sees “forbade” → Ask again. TTL 0 correctly Defers (no sleep). PR-3 wiring is incomplete; the knob is live in PR-2.

5. **`--chain` + horizon Stop skips the next PRD even for rung-scoped stop-this-PRD (attack 20).** `was_stopped` is correctly left false, but `prd_complete = (active tasks == 0)` is false because todos were reset to `todo`. `batch.rs` breaks the chain on `!prd_complete`. Next PRD never inherits the proto-channel (new `IterationContext` anyway). Account-binding Stop *does* stop `--chain` by this side path, not by `was_stopped`.

---

## Attack results table

| # | Attack | Verdict | One-line |
| --- | --- | --- | --- |
| 1 | Fable weekly_scoped 95% parks until Sep 12 / 5h-cap-repark | **PARTIAL** | Account fold/gate no longer parks; named opus/sonnet 100% siblings can quota-empty the PRD instead |
| 2 | weekly-all remaining 0% fails to stop because standard todos exist | **BLOCKED** | Account-binding beyond 12h Stops even when `other_rungs_runnable` |
| 3 | False 3600 Wait from `claude-opus-5` / help `switch models` on another line | **BLOCKED** | Word-boundary + same-line `switch models`; hyphenated model ids ignored |
| 4 | Early-lift undoes 3600 after Fable CLI RateLimit | **BLOCKED** | Post-output skips load/usage_gate/probe when any item is rung-scoped |
| 5 | Spillover blacks whole Claude on Fable CLI | **BLOCKED** | Rung-scoped decide returns Wait before spillover Blackout |
| 6 | Mixed wave double-sleep / Blackout; prefer-rung-scoped masks StopSpend | **PARTIAL** | One wait + no Blackout tested; StopSpend+Fable untested and loses |
| 7 | Proto-channel stickiness: 429 vs empty-unavailable replace | **PARTIAL** | 429/None keeps snapshot; successful evaluate always replaces, including empty |
| 8 | Skip-wait hot-loop: unavailable frontier still selected | **PARTIAL** | Difficulty-resolved todos excluded; off-ladder `tasks.model` still dispatched |
| 9 | Empty selection after excluding frontier → stale-abort / `handle_quota_deferral` | **BLOCKED** | `handle_rung_only_empty_selection` first on seq + wave |
| 10 | `includeForced:false` + any `tasks.model` Defers the whole PRD | **BLOCKED** | `has_forced` is not a global forbid; factory still Proceeds |
| 11 | `Wait{0}` becomes 300s fallback | **BLOCKED** | Execute treats 0 as ready-now; org-fallback too |
| 12 | `LOOP_USAGE_CHECK_ENABLED=false` still GETs OAuth / disables FR-002 | **BLOCKED** | Pre skips load; post ordinary probe still allowed; Fable skip independent of env |
| 13 | Remaining inversion off-by-one / silent `LOOP_USAGE_THRESHOLD` / 0.08 ratio | **BLOCKED** | `remaining≤8` ≡ `used≥92`; legacy env hard-errors at loop/batch preflight |
| 14 | Extra-mark miss after `set-tier claude frontier <opus>` | **BLOCKED** | Run `ResolvedModelsConfig` re-ingest extra-marks frontier |
| 15 | Extra-mark over-mark (unrelated rungs / “Opus”→haiku) | **BLOCKED** | HUD table then exact-string extra-mark; haiku not a substring of opus |
| 16 | Explicit `onLow: wait` on `weekly_scoped` collapsed to Unavailable | **BLOCKED** | Evaluate emits AccountLow; apply Waits |
| 17 | Horizon 3h session reset Stops or Proceeds | **BLOCKED** | Middle band Wait, even when other rungs runnable |
| 18 | Horizon 6h does not cap-and-repark | **WORKS** | Apply caps; production probe lifts on healthy account remaining |
| 19 | ask TTL 0 sleeps; TTL>0 deaf-sleeps without re-eval | **PARTIAL** | TTL 0 Defers (no sleep); TTL>0 waits then `WaitedAndReset` (deaf) |
| 20 | Horizon Stop `was_stopped` skips chain; or account-binding Stop does not | **PARTIAL** | `was_stopped` false; `--chain` still breaks on `!prd_complete` |
| 21 | Sequential vs wave disagree | **PARTIAL** | Shared inners for quota; RateLimit `AccountReaction::Stop` seq/wave diverge |
| 22 | UTF-8 panic on rung-scoped window slice | **BLOCKED** | `floor_char_boundary` + unit test |
| 23 | Dated “resets Sep 12” drives account wait | **BLOCKED** | `parse_reset_from_output("sep…")` → None; Fable 3600 is phrasing override |
| 24 | nimbus_quill / extra_usage / promotional become wait/stop | **PARTIAL** | nimbus/promotional Ignore; `extra_usage` dollars≤0 is spend-stop |
| 25 | Spend at 8% credits left stops | **BLOCKED** | Amount stop only at remaining ≤ 0; percent floor ignored for dollars/credits |
| 26 | Dual predicate: Claude-disabled still hits usage API; env=false probe on Fable | **BLOCKED** | Pre gated on Claude; Fable skip; ordinary RateLimit probe is Claude-only by design |
| 27 | Race: proto-channel HashSet vs slot workers | **BLOCKED** | Main-thread only; slots never write `unavailable_rungs` |
| 28 | Selection uses builtin models not run `ResolvedModelsConfig` | **BLOCKED** | Gate + excluded-ids take `ctx.resolved_models`; re-ingest via `oauth_json` |

---

## Detailed open bugs (file:line, reproduction, blast radius, fix sketch)

### A. Live-fixture named opus/sonnet siblings quota-empty mixed work (attacks 1, 8, 9)

**Path.**

```1989:2111:src/loop_engine/usage.rs
            "seven_day_opus": {
                "utilization": 100.0,
                ...
            },
            "seven_day_sonnet": {
                "utilization": 100.0,
```

`kind_for_named_key` / `looks_rung_scoped_key` (`usage.rs:505–516`) classify `seven_day_*` as `weekly_scoped`. `map_unlabeled_token` (`usage.rs:593–612`) substring-matches `"opus"` / `"sonnet"` against configured model strings. `evaluate_one` (`quota.rs:277–281`) marks those rungs Unavailable at remaining 0 ≤ floor 8. Fable `limits[].weekly_scoped` 5% left marks frontier as well.

Default `anchored_tier`: low → cost-efficient, medium → standard, high → frontier. After all three are unavailable, `compute_quota_excluded_ids` (`pre_spawn.rs:314–391`) excludes every default-difficulty todo. `handle_rung_only_empty_selection` (`account.rs:760–793`) then `Exhausted` → seq `orchestrator.rs:616–632` / wave `wave_orchestration.rs:235–268` soft-stop exit 0, **not** the 5h-cap park.

PR-1 account fold still does the right thing: `parse_oauth_usage_json_with_threshold` skips those keys (`usage.rs:647–670`); live fixture remaining is 45 (`usage.rs:1238–1303`). The regression is **PR-2 ingest+evaluate**, which has no test that feeds `live_shaped_oauth_json()` into `evaluate_quota`/`apply_quota`.

**Reproduction.** Use the live-shaped JSON as a successful OAuth body against a PRD of medium/high Claude todos, factory `tierFallback`. Expect: stderr `% left` banners, proto-channel `{frontier, standard, cost-efficient}`, empty selection, “quota empty, soft-stopping”.

**Blast radius.** Any account whose OAuth HUD still emits vestigial `seven_day_opus`/`seven_day_sonnet` at ~100% while the product HUD only shows Fable weekly + week-all 45%. Recreates “cannot run opus while Fable is out”, which is the operator’s original complaint, as a clean exit instead of a Sep 12 wait.

**Fix sketch.** Named `seven_day_opus` / `seven_day_sonnet` should not be rung-unavailable unless they are a real HUD scoped row (prefer `limits[]` + `display_name`). Or require `display_name` / `scope.model` before marking. Add `evaluate_quota(ingest(live_shaped_oauth_json()))` asserting unavailable == `{frontier}` only.

If live JSON does **not** carry those keys at 100%, this is dormant — still untested.

---

### B. Production probe lifts scoped cap-and-repark (attack 18)

**Path.** Apply is correct:

```3345:3363:src/loop_engine/reactions/account.rs
    fn apply_only_frontier_6h_cap_and_repark_not_stop() {
        ...
            QuotaAccountAction::Wait { secs } => assert_eq!(secs, MAX_WAIT_SECS),
```

Production wait is not:

```1572:1588:src/loop_engine/reactions/account.rs
    let wait = |secs: u64| -> bool {
        let probe = || {
            if let Some(info) = load_usage_info_with_threshold(threshold) {
                if usage_suggests_lifted(&info, threshold, false) {
                    return true;
```

`usage_suggests_lifted` (`usage.rs:813–816`) is `info.percentage > floor`. `percentage` is **account-binding min remaining** (`usage.rs:697–701`), not the scoped bucket that caused the Wait. Live week 45% > 8 → lift. First probe is after `probe_secs` (30s) (`account.rs:1864–1882`).

Then `compute_quota_excluded_ids` empties the queue → `handle_rung_only_empty_selection` soft-stops. The 5h cap-and-repark never happens.

**Reproduction.** Only high-difficulty (frontier) todos; Fable weekly_scoped 5% left, reset in 6h; session/week healthy. Preflight logs a wait, lifts within ~30s, then quota-empty exit.

**Blast radius.** Only-frontier leftover work in the 1h–12h scoped band (the documented cap-and-repark case). Operator sees a “stop” instead of sitting out until Fable resets. Session 3h account-low is **not** this bug — account remaining is itself low, so the probe stays down (`apply_account_low_3h_waits_capped`, `account.rs:3259`).

**Fix sketch.** Probe must use the **same rule’s remaining** (the scoped bucket that produced the Wait), or skip the usage-API probe on scoped-only waits the same way FR-002 skips it for Fable CLI. Tests today inject a no-op `wait` and never exercise this production closure.

---

### C. Prefer-rung-scoped masks StopSpend (attack 6, second clause)

**Path.** Spend is decided first **on `decide_item.output` only**:

```234:252:src/loop_engine/reactions/account.rs
    if api_secs.is_none() && is_spend_limit_message(output) {
        return RateLimitAction::StopSpend;
    }
    if is_rung_scoped_rate_limit_message(output) {
        return RateLimitAction::Wait { secs: blackout_fallback_secs };
    }
```

`react_to_outputs_inner` (`account.rs:581–591`) replaces `first_rate_limited` with any rung-scoped RateLimit. Mixed `[spend RateLimit, Fable RateLimit]` → Fable output → Wait 3600, never StopSpend. Spillover Blackout is correctly suppressed (`reaction_parity.rs:3791–3839`); StopSpend is not in that fixture.

Double-sleep is BLOCKED: wait fires once (`account.rs:627–628`, test `fable_rate_limit_wave_waits_once_3600_never_blackouts`).

**Reproduction.** Wave of 2+: slot 0 stdout `You've hit your individual spend limit · run /usage-credits`; slot 1 Fable switch-models. Expect StopSpend; get Wait 3600 + retry.

**Blast radius.** Parallel/wave only. Credits are actually gone; the loop parks an hour then retries spend-blocked work. Sequential never mixes two RateLimit items.

**Fix sketch.** If **any** RateLimit item is spend-phrasing with no API reset, StopSpend wins over prefer-rung-scoped (stop beats wait). Add a mixed-wave test next to CODE-FIX-001.

---

### D. Ask TTL > 0 is a deaf sleep that does not defer (attack 19)

**Path.** TTL 0 is correct:

```1278:1285:src/loop_engine/reactions/account.rs
fn ask_or_defer(ask_ttl_minutes: u64) -> QuotaAccountAction {
    if ask_ttl_minutes == 0 {
        QuotaAccountAction::Defer
    } else {
        QuotaAccountAction::Ask { ttl_minutes: ask_ttl_minutes }
    }
}
```

TTL > 0 execute:

```1673:1685:src/loop_engine/reactions/account.rs
        QuotaAccountAction::Ask { ttl_minutes } => {
            let secs = ttl_minutes.saturating_mul(60);
            if secs == 0 {
                return UsageCheckResult::Deferred;
            }
            if wait(secs) {
                UsageCheckResult::WaitedAndReset
```

Comment on that block admits “Clap `--use-other-models-ttl` / mid-wait config re-eval stay PR-3.” Tests `execute_ask_ttl_15_sleeps_900s_then_continues` pin the deaf 900s sleep (`account.rs:3086–3131`). After `WaitedAndReset`, the next iteration re-enters preflight with the same forbidding `tierFallback` → Ask again. Unavailable rungs are empty on the forbade path (`apply_forbade_ask_ttl_15_emits_ask_not_defer`, `account.rs:3014–3035`), so selection still picks frontier.

**Reproduction.** `usagePolicy.askTtlMinutes: 15`, `routing.tierFallback` unset, frontier 5% left, standard todos exist. Loop sleeps 15m, resumes on frontier, repeats.

**Blast radius.** Opt-out operators only (factory default never Asks). Config knob is already in PR-2 `UsagePolicy`.

**Fix sketch.** PR-3: re-eval on stop-check cadence; on TTL expiry Defer if eligibility forbids, else replace proto-channel and Proceed. Until then, execute Ask as Defer (no sleep) rather than shipping a deaf 15m loop.

---

### E. `--chain` treats every horizon Stop as incomplete (attack 20)

**Path.** Horizon Stop is a distinct result and does **not** set `was_stopped`:

```166:183:src/loop_engine/iteration.rs
            UsageCheckResult::HorizonStopped => {
                ...
                    should_stop: true,
                    operator_stopped: false,
```

Wave: `was_stopped: false` (`wave_orchestration.rs:122–137`). Seq Empty without `operator_stopped` → `"quota soft-stop"` (`orchestrator.rs:720–724`). Test `execute_horizon_stop_returns_horizon_stopped_not_stop_signaled` (`account.rs:3180–3219`) uses **weekly_all** 6d and asserts no `StopSignaled`.

Then `prd_complete = count_remaining_active_tasks == 0` (`orchestrator.rs:762`). Horizon Stop resets `in_progress` → `todo` (`account.rs:1562–1566`), so todos remain → `prd_complete = false`. Batch:

```712:715:src/loop_engine/batch.rs
        if chain && (exit_code != 0 || !loop_result.prd_complete) {
            ui::emit("Chain stopped: PRD did not complete, skipping remaining PRDs");
```

So `--chain` skips remaining PRDs for **both** account-binding Stop (desired per FR-005) and rung-scoped stop-this-PRD (forbidden: next PRD should run and inherit). Proto-channel is process-local on `IterationContext` (`engine.rs:453–461`) and is **not** inherited across `run_loop` calls anyway — inherit is unimplemented.

Without `--chain`, account-binding Stop is exit 0 + `was_stopped false` → batch **continues** to the next PRD (FR-005 miss for non-chain “stop the account”).

**Reproduction.** `batch run --chain` two PRDs; first is only-frontier with Fable reset >12h. First PRD soft-stops; second is skipped as incomplete. Inverse: weekly-all 0% left, `--chain` off → next PRD still runs.

**Fix sketch.** Distinguish account-binding Stop vs rung-scoped Stop in `UsageCheckResult`. Account-binding: `was_stopped=true` or non-zero exit so any batch (chain or not) stops. Rung-scoped: exit 0, `prd_complete` forced true **or** a dedicated “soft-complete” that `--chain` does not treat as incomplete; persist/inherit unavailable rungs into the next `run_loop`.

---

### F. Off-ladder `tasks.model` skip-wait hot-loop (attack 8 remainder; not attack 10)

Attack 10 as stated (whole-PRD Defer) is BLOCKED (`tier_fallback_allows` ignores `has_forced`, `account.rs:1102–1114`, test `apply_factory_with_forced_model_still_unavailable_proceed`).

The inverse hole is live. `resolve_execution_plan` EXPLICIT_MODEL (`model.rs:994–1008`): off-ladder id → `tier_of` None → `anchored_tier(difficulty)` while **`model` stays the explicit string**. `compute_quota_excluded_ids` keys on `(provider, tier)` (`pre_spawn.rs:382–388`). A `tasks.model = claude-fable-5-1` medium task looks like **standard**, is not excluded when only frontier is unavailable, and is still spawned with the Fable id. Overflow writes to `tasks.model` take this path.

**Reproduction.** Frontier unavailable on proto-channel; one todo with explicit off-ladder Fable id, medium difficulty. Next iteration claims it, CLI Fable-limits, Wait 3600, repeat.

**Fix sketch.** PR-3 family-match at resolve time (already in the PRD). Until then, exclude when the explicit model string’s family maps to an unavailable rung.

---

### G. `extra_usage` amount ≤ 0 is spend-stop (attack 24 remainder)

`evaluate_unknown_kind_low_is_ignore` covers `nimbus_quill` percent 0 (`quota.rs:465–472`). Promotional percent-low also falls through to Ignore (`quota.rs:301–302`).

`is_spend_kind` includes `"extra_usage"` (`account.rs:1344–1348`). A named sibling with `dollars: 0` (or tokens/credits 0) is `amount_exhausted` → `AccountLow` (`quota.rs:291–298`) → apply `spend_stop` (`account.rs:1196–1201`). PRD default for extra_usage is **ignore**.

**Reproduction.** OAuth body `"extra_usage": { "dollars": 0 }` with healthy session/week. Preflight HorizonStopped.

**Fix sketch.** Drop `extra_usage` from `is_spend_kind`; keep spend/credits/dollars. Add ingest+apply fixture.

---

### H. Seq vs wave on `AccountReaction::Stop` (attack 21 remainder)

Shared quota preflight is seq/wave identical (`account_quota_preflight_inner_same_decision_both_shapes`). Post-output Fable Wait is shared.

`AccountReaction::Stop` (StopSpend **or** `.stop` during wait):

- Wave: `was_stopped: true`, exit 130 (`wave_scheduler.rs:1101–1113`).
- Seq: `IterationOutcome::RateLimit`, `should_stop: true`, `operator_stopped: false` (`iteration.rs:874–888`) → orchestrator `_` branch exit 1 `"stopped"`, `was_stopped` stays false (`orchestrator.rs:730–733`).

`--chain` still breaks on `exit_code != 0` for seq, so remaining PRDs skip either way. Exit code (1 vs 130) and `was_stopped` (auto-review / “Stop signal detected during PRD”) disagree.

---

## Defenses that look real

- **PR-1 account fold.** Only `five_hour`/`seven_day` + `limits[]` `session`/`weekly_all` (`usage.rs:647–670`). Live fixture remaining 45, session reset (`usage.rs:1238–1303`). Inverse weekly-all 100% remaining 0 (`usage.rs:1306–1341`). `severity`/`is_active` do not exhaust.
- **FR-002 Fable CLI.** `is_rung_scoped_rate_limit_message` (`account.rs:289–375`): real-word `fable|opus|sonnet|haiku` + `limit` within 64 bytes, or `reached your <token> limit`, or `switch models` **on the same line** as `reached`/`limit`. Hyphen/underscore are not boundaries — `claude-opus-5` in a session RateLimit does not 3600 (`account.rs:2338–2359`, `reaction_parity.rs:4020–4059`). `/model` alone Blackouts under spillover (`reaction_parity.rs:3977–4010`). Decide ignores `api_secs`/`output_secs` (`account.rs:248–251`, test with 6-day `api_secs` at `account.rs:2390–2398`). Spillover never records (`account.rs:2402+`, wave test `reaction_parity.rs:3723–3777`).
- **FR-002 probe skip.** Wrapper `any()` rung-scoped skip of load/usage_gate/probe (`account.rs:498–547`); mixed wave too (`reaction_parity.rs:3916–3968`). Inner prefers rung-scoped decide item (`account.rs:581–591`).
- **Weekly-all 0% still Stops.** `has_account_binding_wait` beyond horizon → Stop even if `other_rungs_runnable` (`account.rs:1235–1239`, `apply_account_weekly_all_6d_stops_even_when_other_rungs_runnable`).
- **3h session waits.** Middle band `Wait { secs.min(MAX_WAIT_SECS) }` (`account.rs:1229–1234`); `apply_account_low_3h_waits_capped` and `…_even_when_other_rungs_runnable`.
- **Wait{0} ≠ 300.** `execute_quota_account_action` (`account.rs:1659–1665`); `wait_for_usage_reset_inner` (`account.rs:1837–1840`); org fallback (`account.rs:1642–1648`). Tests `execute_wait_zero_is_ready_now_not_fallback_300`, `org_fallback_reset_zero_is_ready_now_not_fallback_300`.
- **Explicit scoped wait.** `OnLowAction::Wait|Stop|Ask` emit AccountLow, not Unavailable (`quota.rs:264–273`, `evaluate_explicit_on_low_wait_on_weekly_scoped_emits_account_low`; apply `apply_explicit_wait_on_weekly_scoped_honors_wait`).
- **includeForced is not a whole-PRD defer.** `tier_fallback_allows` (`account.rs:1102–1114`); `apply_factory_with_forced_model_still_unavailable_proceed`.
- **Rung-only empty ≠ stale / provider blackout.** Called before `handle_quota_deferral` on seq (`orchestrator.rs:609–634`) and wave (`wave_orchestration.rs:230–268`). Wave test `test_run_wave_iteration_rung_only_empty_soft_stops_not_stale`.
- **Proto-channel replace vs keep.** `replace_unavailable_rungs` only on `buckets: Some` (`account.rs:1618–1628`). API fail / env-disabled keep snapshot (`preflight_keeps_snapshot_on_api_fail`, `gate_disabled_skips_usage_load_and_keeps_snapshot`).
- **Remaining unit.** Ingest `(100 - util).clamp(0, 100)` (`usage.rs:448–454`). Compare `remaining > floor` proceed (`account.rs:2071–2074`). Exact floor waits (`usage.rs:1885–1892`). `LOOP_USAGE_THRESHOLD` hard-error at `preflight_validate_and_probe` (`project_config.rs:1065–1071`); `LoopConfig::from_env` does not honor it (`config.rs:662–674`).
- **Extra-mark after pin.** `extra_mark_rungs` exact `exact_model_for` equality (`usage.rs:569–590`). Production re-ingests with run models (`account.rs:1488–1494`, `buckets_for_run_models` `usage.rs:862–869`). Tests `ingest_extra_mark_after_frontier_pin_marks_standard_and_frontier`, `remaining_banner_extra_marks_frontier_under_opus_pin`. HUD-only Opus without pin does not mark frontier (`ingest_hud_only_without_pin_leaves_frontier_unmarked_on_opus`).
- **Dual predicate.** Pre: `run_account_quota_gate` skipped when `!execute_account_action` before load (`account.rs:1482–1486`); callers only enter when Claude enabled (`iteration.rs:131`, `wave_orchestration.rs:87–90`). Post: `anthropic_account_io_allowed` is Claude-only, not env (`iteration.rs:844–850`, `wave_scheduler.rs:1059–1065`). Env=false + Claude on still probes **ordinary** RateLimit (allowed). Fable skip is a third exception.
- **UTF-8 window.** `floor_char_boundary` (`account.rs:365–367`); `test_model_token_window_floors_utf8_char_boundary`.
- **Dated Sep 12.** `test_parse_reset_from_output_sep_month_token_stays_none` (`account.rs:2639–2645`). Fable 3600 does not use that parse.
- **Spend 8 credits / 8%.** Dollars/credits ignore percent floor (`quota.rs:566–583`); apply stops only at amount ≤ 0 (`account.rs:3383–3411`).
- **Selection models.** `ctx.resolved_models` set once in `run_loop` (`orchestrator.rs:129–132`); excluded-ids and quota gate take that pointer (`iteration.rs:276–282`, `wave_scheduler.rs:785–796`).
- **HashSet race.** `unavailable_rungs` documented main-thread only (`engine.rs:453–456`). Grep: writes only in preflight (`iteration.rs:137`, `wave_orchestration.rs:96`) and tests. Slot modules do not touch it.

---

## Tests that would have caught the open bugs but don't exist

1. **`evaluate_quota(ingest_oauth_value(live_shaped_oauth_json(), builtin))` + factory apply + medium/high remaining work.** Would fail if `seven_day_opus`/`seven_day_sonnet` at 100% mark standard/cost-efficient. Closest tests stop at ingest mapping (`usage.rs:2051–2111`) and account-fold remaining 45 (`usage.rs:1238–1303`).

2. **Production `account_quota_preflight` wait probe with scoped-only Wait + healthy account remaining.** Inject `load_usage` returning percentage 45 while applied action is Wait 18000. Expect: probe must **not** lift. Today’s 6h test only asserts apply (`apply_only_frontier_6h_cap_and_repark_not_stop`) and hermetic `wait` closures never probe.

3. **Mixed wave: spend RateLimit + Fable RateLimit, `api_secs=None`.** Expect `AccountReaction::Stop` (StopSpend), zero waits, empty blackout. CODE-FIX-001 only covers account `hit your limit` + Fable.

4. **Ask TTL 15 + forbidding `tierFallback` across two preflight calls.** After first `WaitedAndReset`, second call must Defer (or Proceed on standard), not Ask/sleep again. `execute_ask_ttl_15_sleeps_900s_then_continues` pins the opposite.

5. **`batch run --chain` after rung-scoped HorizonStopped with leftover todos.** Expect next PRD to run (inherit). Today `--chain` breaks on `!prd_complete`. Mirror: account-binding HorizonStopped without `--chain` should still stop the batch (today continues on exit 0).

6. **Off-ladder `tasks.model = claude-fable-5-1` with frontier in `unavailable_rungs`.** `compute_quota_excluded_ids` must contain that id. `proto_channel_excludes_with_empty_provider_blackouts` only seeds difficulty-based todos with no `model` column.

7. **`extra_usage: { dollars: 0 }` ingest → apply.** Expect Ignore, not HorizonStopped.

8. **Seq vs wave `AccountReaction::Stop` (StopSpend and `.stop` during wait).** Assert `was_stopped` / exit code / `operator_stopped` parity. No such lock.

9. **Thin OAuth 200** (only `five_hour`/`seven_day`, no `limits[]`) after a prior frontier snapshot. Expect keep-or-merge, not `replace_unavailable_rungs` clearing frontier (`preflight_successful_evaluate_replaces_stale_proto_channel` currently requires that replace).

10. **Unlabeled HUD `display_name: "Fabulous"` / `"Opus and Haiku"`.** `hud_tier_from_label` uses `starts_with("fable")` and first-match (`usage.rs:524–541`). Low probability; still no negative test that a non-family label cannot extra-mark haiku.

---

## Assumptions / unverified

- Did not hit live `GET /api/oauth/usage`. Whether `seven_day_opus`/`seven_day_sonnet` are still 100% on this account is unverified; the PR-1 live fixture treats them as present at 100%.
- Slot-worker “no write” is from grep + comments (learning 1810), not a TSAN run.
- `account_usage_gate` (legacy check_and_wait wrapper) has no production caller; pre-iteration is `run_account_quota_gate` only. Post-output ordinary RateLimit still uses `check_and_wait` as `usage_gate` when not rung-scoped.
- PR-3 clamp / `--use-other-models-ttl` clap / family-match walker are correctly absent; holes that the PRD assigned to PR-3 are marked PARTIAL, not WORKS, except where a PR-2 knob already executes (ask TTL wait, extra_usage spend-stop, 6h probe).
