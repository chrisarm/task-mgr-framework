# Architect re-pass: quota-rung-policy PR-3 (grounded in landed main)

Date: 2026-09-07
Reviewer: production-code-architect `01a07c6c-a9ce-7330-89bb-281896900a63`
Implementation SSoT: worktree `feat-quota-rung-policy-pr2` (`5a00b15`) / `main` `dacccb3`
Artifacts: `tasks/prd-quota-rung-policy.md` + `tasks/quota-rung-policy-pr3.json` + `tasks/quota-rung-policy-pr3-prompt.md` (post JSON-review apply `01a07c67`)

**Status**: NEEDS_CHANGES

Grounded in this worktree (`quota.rs`, `apply_quota`/`ask_or_defer`, `IterationContext.unavailable_rungs: HashSet`, `PlanContext` without rungs, `LoopResult` without inherit, no `--use-other-models-ttl`). JSON FEAT-006–008 correctly refuse to reimplement PR-2 evaluate/apply, remaining-percent, factory serde, Ask sleep, `HorizonStopped`, and `handle_rung_only_empty_selection`. The remaining holes are wiring, not product-direction.

**Strengths**
- Pins, factory auto-downgrade vs Ask opt-out, `effective_ttl` **before** `ask_or_defer`, down-only walker (not `model_for`), `unset-tier-fallback` = JSON `null`, live-fetch gate, and verify-task-mgr help/models (no `loop run`) are the right PR-3 slice.
- JSON already names the landed Ask footgun: `ask_or_defer(policy.ask_ttl_minutes)` at apply (`account.rs` ~1245/1252); config `0` + CLI `15` is `Defer` unless TTL is injected before apply.
- `active_rungs(&map, now) -> HashSet` is the right *type* adapter for proto-channel readers. Ingest helpers `hud_tier_from_label` / `map_unlabeled_token` / `family_token_from_id` already exist in `usage.rs` (private — `pub(crate)`, do not copy HUD tokens).

---

**Concerns**

1. **Critical — remaining-work snapshot ignores clamp, so factory + only-frontier-left still Stops.**
   `compute_remaining_work_snapshot` resolves with empty blackouts and treats “tier ∈ unavailable_preview” as not runnable (`account.rs` ~1748–1765). Factory `apply_quota` then Stops when `!other_rungs_runnable` and reset > 12h (~1240–1243). PR-2 exclude never selects those tasks; PR-3 clamp in `resolve_execution_plan` never runs because the gate already returned `HorizonStopped`. Goal exit (“frontier-low continues on standard **without** `set-tier`”) fails for the all-high / review / explicit-frontier queue — the only queue the pin still exists for. Mixed standard+frontier already proceeds under PR-2 exclude.

2. **Critical — FEAT-007 10-file cap drops production spawn + inherit seed.**
   `active_rungs` does **not** thread a new `PlanContext` field. Landed spawn:
   - sequential: `iteration.rs` `BuildPromptParams` → `prompt/sequential.rs` `PlanContext` (no rungs)
   - wave: `wave_scheduler.rs` `SlotPromptParams` → `prompt/slot.rs` `PlanContext` (no rungs)
   - `run_loop` always `IterationContext::new()` (`orchestrator.rs:123`) and `LoopResult { … prd_complete }` (`orchestrator.rs:876`) with no map
   FEAT-007 lists sequential/slot/batch/engine but **excludes** `iteration.rs`, `wave_scheduler.rs`, `orchestrator.rs`. If `unavailable_rungs` on `PlanContext`/`BuildPromptParams` defaults empty to keep those files compiling, clamp is dead at spawn. If `excluded_ids` uses post-clamp but spawn does not, eligible tasks are **selected and dispatched on Fable** (3600s whole wave) — worse than today’s exclude. Inherit on `LoopResult` without orchestrator seed is a dead field.

3. **High — FEAT-006 clap never reaches `ask_or_defer`.**
   Gate params are built in `iteration.rs` / `wave_orchestration.rs` from `UsageParams` constructed in `startup.rs:1006`. None of those files are in FEAT-006 `touchesFiles`. `LoopConfig` + `execute_quota_account_action` only is the prohibited outcome already named: config `askTtlMinutes: 0` + `--use-other-models-ttl 15` still `Defer`. Also add `startup.rs` `UsageParams { }` and `wave_scheduler.rs` test `UsageParams { }` or the crate does not compile.

4. **High — next-PRD inherit collides with landed chain gate and one `HorizonStopped`.**
   `batch.rs:712`: `chain && (exit_code != 0 || !loop_result.prd_complete)` aborts. Rung-scoped Stop leaves todos → `prd_complete == false` → **`--chain` never seeds the next PRD**. Account-binding Stop and rung-scoped Stop are the **same** `UsageCheckResult::HorizonStopped` / exit 0 / `was_stopped: false`. Exempting all HorizonStopped from the chain gate lets weekly-all 6d continue the chain. Need a discriminator on `LoopResult` (e.g. `inherited_unavailable` vs `account_quota_stopped`) and a chain-gate exception **only** for rung-scoped Stop.

5. **High — clamp vs exclude is the right remaining shape, but JSON under-specifies the three sites.**
   PR-2 exclude **skips** frontier work; clamp must **run** it on standard. Required together: (a) snapshot counts clamp-eligible tasks as runnable, (b) `compute_quota_excluded_ids` uses post-clamp resolve, (c) spawn `resolve_execution_plan` actually clamps and rewrites `plan.model` via `exact_model_for`, **not** `finalize_plan`/`model_for` (that helper still walks up — `model.rs:1189`). Missing (a) parks; missing (c) after (b) dispatches Fable. Walker `skip >= start` + `exact_model_for` is correct (`CapabilityTier::ALL` is ascending).

6. **Medium — PRD US-005 AC vs landed apply / human-review 1.**
   Landed factory path is `unavailable + Proceed` (never Ask). Ask-path TTL 0 is `Defer`. JSON FEAT-006 matches that. US-005 still says “TTL 0: no sleep; continue on working rungs immediately only if eligibility accepts” and the story text “continues on whatever rung still works,” which implementers can read as factory-through-Ask. Not a product contradiction if JSON is SSoT; tighten the PRD AC.

7. **Medium — stale PRD / `consumerAnalysis` vs landed line numbers.**
   - PRD §2.6 L163: `PlanContext.unavailable_rungs` does not exist (only `provider_blackouts`).
   - L446: rung-exhaustion sibling is already `handle_rung_only_empty_selection` (PR-2).
   - FEAT-006 consumer `account.rs:1673` is the **execute** Ask arm; the break site for CLI TTL is **apply** `ask_or_defer(policy.ask_ttl_minutes)` ~1245/1252.
   - FEAT-007 `model.rs:980` is the QUOTA_BLACKOUT comment; `EXPLICIT_MODEL` **early-returns** at 995–1007 — post-resolve clamp must wrap those returns, not only the default-path tail.
   - `quota.rs:78` “serde lands with FEAT-008” is stale (`UsagePolicy` already lives on `ProjectConfig`).

8. **Medium — Ask re-eval cannot reuse `wait() -> bool` as today.**
   CODE-FIX-010: wait complete → `WaitedAndReset` (continue), `.stop` → `StopSignaled`. Timeout + forbade must be `Deferred`; mid-wait `set-tier-fallback` must continue on the **stop-check** tick (`WaitTiming.stop_check_secs`, not `probe_secs`). Keep `wait_for_usage_reset_inner`; do not keep the Ask match-arm’s post-wait mapping. Re-read `usagePolicy` + `routing.tierFallback` only (batch cache exception is correct).

9. **Low — `models show` gate.** `check_opt_in` is `TASK_MGR_USE_API=1` only; key is the fetch. Call the same path as `list --remote`, do not invent a second gate. JSON “ANTHROPIC_API_KEY + check_opt_in” is slightly overstated.

**Already shipped vs still PR-3:** JSON does **not** reimplement evaluate/apply, remaining-percent, HashSet replace-on-evaluate, factory serde, Ask sleep, `HorizonStopped`, or `handle_rung_only_empty_selection`. Do not spawn those as follow-ups. Expiry map is an upgrade of `replace_unavailable_rungs`, not a rewrite of apply.

**HashSet → expiry map:** Adapter is sound **if** `replace_unavailable_rungs`, `handle_rung_only_empty_selection`, and `compute_quota_excluded_ids` take the map and call `active_rungs` internally (`account.rs` + `pre_spawn.rs` + `engine.rs`). Then `iteration.rs` / `orchestrator.rs` / `wave_orchestration.rs` / `reaction_parity.rs` can pass `&ctx.unavailable_rungs` without treating expired keys as active. **Silent ignore** if any of those still iterate the raw map, or if `PlanContext` is left empty (concern 2). Compile-break not listed: `wave_scheduler.rs:2192` `unavailable_rungs.insert((Provider, Tier))` — HashMap insert needs expiry.

**Off-ladder family-match:** Reuse `hud_tier_from_label` on the **explicit `tasks.model` string** (`claude-fable-5-1` tokens include `fable` → frontier). Do not use `family_token_from_id` (underscore rsplit) or substring `tier_of`. JSON is not inventing helpers; they are private.

**Verification:** Skill refuse of `loop run` / `batch run` is honored. Help captures + `models-routing` are the right proofs. Do not require a live loop.

---

**Questions for User**
None. Human-review inherit vs account-binding chain-stop is already picked; the plan just omitted the discriminator — prescribe it below.

---

**Suggested Revisions** (plan/PRD/JSON, not code)

1. **PRD US-005 / FR-005:** Split factory vs Ask. Factory/allowing = continue-via-unavailable (PR-2, not Ask). Ask-path TTL 0 = Defer, no sleep. CLI overrides `policy.ask_ttl_minutes` **before** `ask_or_defer`. Strike “TTL 0 continues on working rungs” as a factory sentence.

2. **PRD §2.6 L163 / L446 / apply contract:** `PlanContext.unavailable_rungs` is PR-3, not landed. Sibling of `handle_quota_deferral` is shipped. Apply already consumes `ask_ttl_minutes`.

3. **FEAT-006 `touchesFiles` add:** `startup.rs`, `iteration.rs`, `wave_orchestration.rs` (and `wave_scheduler.rs` test `UsageParams` if that struct grows). Consumer break site = apply `ask_or_defer`, not only execute L1673. Ask wait: stop-check re-read + richer outcome (`Deferred` / continue / `StopSignaled`); timeout must not set `was_stopped`.

4. **FEAT-007 — do not drop clamp/inherit/family-match to keep 10 files.** Add production threaders:
   - `iteration.rs`, `wave_scheduler.rs` (params → `PlanContext`)
   - `orchestrator.rs` (seed inherited map on `IterationContext`; copy map onto `LoopResult`)
   - Prefer extra files over a split. If split: **007a** expiry+`active_rungs`+replace+synthetic 3600; **007b** walker+family-match+all three clamp sites; **007c** inherit+chain discriminator. Do **not** ship 007b exclude-post-clamp without 007b spawn clamp.

5. **FEAT-007 AC (new):** `compute_remaining_work_snapshot` treats a task as runnable if the down-only walker would land on a defined non-blacked **lower** rung under current `tierFallback` eligibility. Factory + only-frontier-left + 6d reset → `Proceed` + clamp, **not** `HorizonStopped`. “No fallback” Stop remains the **forbade** / no-cheaper-defined-rung case.

6. **FEAT-007 inherit:** `LoopResult` carries expiry map **and** `account_quota_stopped: bool` (or split result). `batch --chain`: account-binding Stop still aborts; rung-scoped HorizonStopped continues and seeds the next `LoopRunConfig` → `orchestrator` `ctx.unavailable_rungs`. Receiver: `active_rungs(&map, now)`. Ask-timeout / `Deferred` stay off this path (`was_stopped` false; incomplete-PRD chain stop for forbade-defer is OK).

7. **Post-clamp model string:** after walker returns a lower tier, set `plan.model` from `exact_model_for`, never `finalize_plan`/`model_for`. Wrap `EXPLICIT_MODEL` early return. `includeForced=false` off-ladder → exclude/defer that id only (do not regress CODE-FIX-003).

8. **FEAT-008:** keep JSON-null unset and offline show with no `% left`. Drive `list --remote`’s existing opt-in, not a new predicate.

---

## Inversion (how this PR-3 design fails against **landed** code)

Given `apply_quota` already `ask_or_defer(policy.ask_ttl_minutes)`, `excluded_ids` already skipping frontier, snapshot already treating frontier-resolved todos as not runnable, `PlanContext` without rungs, `run_loop` always `IterationContext::new()`, and `batch --chain` aborting on `!prd_complete`:

1. **Pin 1 false-park:** factory + only-high/review queue → snapshot `other_rungs_runnable=false` → `HorizonStopped` → pin still required.
2. **Clamp walks up:** any post-clamp `finalize_plan`/`model_for` lands back on blacked frontier.
3. **CLI TTL ignored:** clap lands on `LoopConfig` but apply still sees config `0` → `Defer`.
4. **Exclude-without-spawn-clamp:** post-clamp exclude selects the task; spawn still returns Fable → 3600s wave.
5. **Inherit missing:** next PRD empty HashSet; `--chain` already bailed on incomplete; same frontier-only Stop repeats.
6. **`handle_quota_deferral` reuse:** not in the JSON path; keep CODE-FIX-009 first. Failure is empty selection after clamp-eligible work was excluded.
7. **Model ids in engine keys:** JSON forbids it; failure is copying HUD tokens into `model.rs` instead of `pub(crate)` ingest.
8. **Ask-timeout → `was_stopped`:** execute `wait()==false` is `.stop` only; mapping timeout to `StopSignaled` kills `--chain`. Mapping timeout to `WaitedAndReset` when forbade continues on standard against human-review 1.

Until snapshot + exclude + spawn clamp are one path, and inherit has a chain-gate discriminator, PR-3 does not retire the PR-1 pin.
