# Sequential/wave parity audit — quota PR-1+PR-2

Audit of `feat/quota-rung-policy-pr2` HEAD `5a00b15` (PR-1 + PR-2 combined).
Source was not modified. Dual-path files grepped: `iteration.rs`,
`orchestrator.rs`, `wave_orchestration.rs`, `wave_scheduler.rs`, `slot.rs`,
`prompt/sequential.rs`, `prompt/slot.rs`, `reactions/account.rs`,
`reactions/pre_spawn.rs`, `engine.rs`, `recovery.rs`, `quota.rs`.

## Verdict

**Shared inners are aligned. Call-site mapping is not.**

Evaluate/apply, proto-channel replace-on-success, remaining-work snapshot,
rung-scoped Wait-3600, mixed-wave prefer-rung-scoped, and
`handle_rung_only_empty_selection` all live in one coordinator both paths
call. Production leftover `account_usage_gate` is gone from seq and wave.

The dual-path **wrappers** still disagree on operator-visible meaning:

1. **High — `.stop` during post-output RateLimit wait** (FR-002, now
   load-bearing because Fable waits 3600s): sequential exits `1` /
   `was_stopped=false`; wave exits `130` / `was_stopped=true`. Auto-review
   and batch-chain classification diverge.
2. **Medium — pre-gate HorizonStopped / Deferred final-banner reasons:**
   sequential collapses both to `"quota soft-stop"`; wave keeps
   `"quota horizon stop"` vs `"quota ask deferred"`. `was_stopped` is
   correctly `false` on both paths (does **not** look like a `.stop` file).
3. **Medium — iteration budget** on those same pre-gate terminals:
   sequential `Empty` consumes a loop iteration; wave sets
   `iteration_consumed: false`.

`tests/reaction_parity.rs` locks the shared inners and the Fable wave
shape. It does **not** lock call-site `exit_code` / `was_stopped` /
`exit_reason`, and still advertises production gate parity via the unused
`account_usage_gate_inner` helper.

No proto-channel ↔ `runner_overrides` / `provider_blackouts` cross-talk
was found. `slot.rs` does not implement a third quota path.

## Shared inner vs duplicated logic map

| Behavior | Seq site | Wave site | Shared? | Test |
| --- | --- | --- | --- | --- |
| Pre-iteration quota gate | `iteration.rs:131-210` calls `run_account_quota_gate` | `wave_orchestration.rs:91-162` (once/wave, after `.stop` / before crash backoff) | **Yes** — `reactions/account.rs:1454-1568` `run_account_quota_gate` / `_inner` | `account.rs` unit tests (`gate_disabled_skips_usage_load_and_keeps_snapshot`, `preflight_*`). `reaction_parity::account_quota_preflight_inner_same_decision_both_shapes` calls the **same** inner twice — tautology, not call-site lock |
| `LOOP_USAGE_CHECK_ENABLED` skip | `execute_account_action: params.usage_params.enabled` (`iteration.rs:145`) | identical (`wave_orchestration.rs:104`) | **Yes**. `UsageParams.enabled` is `env ∧ Claude` from `startup.rs:1006-1008`. Inner returns `Skipped` with **zero** load and keeps proto-channel (`account.rs:1484-1486`, `1614-1616`) | `gate_disabled_skips_usage_load_and_keeps_snapshot`; **not** in `reaction_parity.rs` |
| Claude-disabled skip | outer `if is_provider_enabled(Claude)` (`iteration.rs:131`) | same (`wave_orchestration.rs:87-90`) | **Yes** — both skip the call entirely. Dual predicate unchanged | no seq/wave call-site test |
| Proto-channel replace vs keep-on-API-fail | via shared inner | via shared inner | **Yes** — `replace_unavailable_rungs` only on `buckets: Some` (`account.rs:1618-1628`); API fail / env-off keep snapshot | `preflight_successful_evaluate_replaces_stale_proto_channel`, `preflight_keeps_snapshot_on_api_fail`, `preflight_disabled_keeps_proto_channel_snapshot` — all unit, not dual-path |
| Remaining `% left` banner | `eprintln` inside `run_account_quota_gate_inner` (`account.rs:1500-1510`) | same function | **Yes** — once per seq iteration / once per wave | usage/account unit tests; **not** `reaction_parity.rs` |
| `Wait { 0 }` ready-now | `execute_quota_account_action` (`account.rs:1659-1665`) | same | **Yes** — `BelowThreshold`, never `fallback_wait` 300 | `execute_wait_zero_is_ready_now` in `account.rs`; **not** `reaction_parity.rs` |
| Horizon Stop / Deferred / StopSignaled / Proceed mapping | `iteration.rs:148-209` → `IterationOutcome::Empty` + `should_stop` + `operator_stopped`; `orchestrator.rs:693-725` maps to exit | `wave_orchestration.rs:107-161` maps to `WaveOutcome` directly | **Inner decision shared; wrapper mapping diverges** (see Issues 2–3) | inner: `execute_horizon_stop_returns_horizon_stopped_not_stop_signaled`. **No** seq vs wave exit_reason / `was_stopped` test |
| Horizon Stop vs `.stop` file | `operator_stopped: false` (`iteration.rs:175`); orchestrator Empty+!operator_stopped → `"quota soft-stop"`, `was_stopped` stays false (`orchestrator.rs:720-724`) | `was_stopped: false` (`wave_orchestration.rs:134`) | **Yes on the stop-file flag.** `.stop` file is `operator_stopped`/`was_stopped` true + `"stop signal"`. Horizon is not that. Banner *reason string* still differs (Issue 2) | inner distinguishes `HorizonStopped` ≠ `StopSignaled`; call-site untested |
| Post-output RateLimit | `iteration.rs:829-890` 1-item slice when `outcome == RateLimit` | `wave_scheduler.rs:1041-1115` N-item slice once/wave after join | **Yes** — `react_to_outputs` → `_with_io_seams` → `_inner` | Fable 3600 + mixed-wave + skip-probe tests in `reaction_parity.rs` |
| Rung-scoped skip of `load_usage_info` / `usage_gate` / `probe_rate_limit_lifted` | via shared wrapper (`account.rs:488-547`) | same | **Yes** — `any()` rung-scoped skips I/O even when mixed with account RateLimit | `fable_rate_limit_skips_usage_gate_and_probe`, `mixed_wave_rung_scoped_still_skips_usage_gate_and_probe` |
| Prefer rung-scoped item in mixed wave | n/a (1 item) | `_inner` `find` rung-scoped else first RateLimit (`account.rs:581-591`) | **Yes** — sequential cannot mix; wave uses the shared prefer | `mixed_wave_prefers_rung_scoped_rate_limit_over_leading_account_hit` |
| Wait 3600 once per wave, no `provider_blackouts.record` | 1-item Wait in shared inner | N-item Wait once; `decide_account_rate_limit` returns `Wait { blackout_fallback_secs }` before spillover Blackout (`account.rs:248-252`, `615-634`) | **Yes** | `fable_rate_limit_wave_waits_once_3600_never_blackouts` |
| Ordinary RateLimit still Blackout / `api_secs` | same inner | same inner | **Yes** | `reroute_and_retry_records_blackout_and_skips_wait_both_shapes`, `slash_model_alone_does_not_take_3600_override` |
| `.stop` during RateLimit wait (`AccountReaction::Stop`) | `iteration.rs:874-888`: `RateLimit` + `should_stop` + **`operator_stopped: false`** → orchestrator `_` arm exit **1** `"stopped"` (`orchestrator.rs:730-733`) | `wave_scheduler.rs:1101-1113`: exit **130**, `was_stopped: true`, `"stop signal during rate-limit wait"` | **No — high divergence** (Issue 1) | `reaction_parity` asserts `AccountReaction::Stop` only; **does not** lock exit_code / `was_stopped` |
| Proto-channel storage | `IterationContext.unavailable_rungs` (`engine.rs:453-461`) | same ctx | **Yes** — main-thread only | — |
| Proto-channel writers | `run_account_quota_gate` only | same (wave preflight) | **Yes**. `slot.rs` has zero quota writes. Tests may `insert` (`wave_scheduler.rs:2192`) | replace tests in `account.rs` |
| Proto-channel readers (selection) | `iteration.rs:277-283` → `compute_quota_excluded_ids` → `prompt/sequential.rs:411-418` `next_excluding` | `wave_scheduler.rs:790-805` → same helper → `select_parallel_group_excluding` | **Yes**. Early-return only when **both** blackouts **and** proto-channel empty (`pre_spawn.rs:322-324`) | `proto_channel_excludes_with_empty_provider_blackouts` |
| `PlanContext.unavailable_rungs` | **does not exist**. `resolve_execution_plan` (`model.rs:990`) takes only `provider_blackouts` | slot prompt same (`prompt/slot.rs:214`) | Selection exclusion only; no resolve-time clamp (PR-3) | — |
| Empty selection after all todos excluded | `orchestrator.rs:608-634` on `NoEligibleTasks`: `handle_rung_only_empty_selection` **before** `handle_quota_deferral` | `wave_orchestration.rs:230-270`: same order | **Yes** — sibling, not `handle_quota_deferral` (learning 3927). Seq auto-recovers *inside* `run_iteration` (`iteration.rs:342-429`) *before* returning `NoEligibleTasks`; wave recovers *after* rung-only. Snapshot includes `in_progress`, so Exhausted vs Inactive agrees | wave: `test_run_wave_iteration_rung_only_empty_soft_stops_not_stale`. Seq: **no** equivalent orchestrator test. Shared helper tests in `account.rs` |
| Remaining-work snapshot | `run_account_quota_gate_inner` (`account.rs:1526-1532`) passes `conn` + `ctx.runner_overrides` | same | **Yes** — `todo` **and** `in_progress` (`account.rs:1710-1712`); pins via `runner_overrides` (`1759-1762`); empty blackouts so spillover is not a working rung. Wave apply runs **before** spawn, so there are no current-wave sibling in_progress rows — leftover `in_progress` from the previous wave/iteration is the same set sequential would see | `snapshot_with_grok_pin_counts_as_other_rungs_runnable` |
| `other_rungs_runnable` vs weekly_all Stop | `apply_quota` (`account.rs:1235-1239`): account-binding beyond horizon **Stops even if** other Claude rungs look runnable | same | **Yes** | `execute_horizon_stop_returns_horizon_stopped_not_stop_signaled` |
| `includeForced` / `tasks.model` | snapshot sets `has_forced`; `tier_fallback_allows` **ignores** it (`account.rs:1113-1114`) | same | **Yes** — factory still auto-unavailable. Exclusion is by resolved `(provider, tier)`, so an explicit frontier `tasks.model` **is** excluded in PR-2. Off-ladder ids fall back to `anchored_tier` (`model.rs:997-999`) — PR-3 family-match | no dual-path test |
| Override-channel discipline | proto-channel sibling HashSet; `promote_once` (`recovery.rs:159-169`) reads only `runner_overrides` | same | **Yes**. Rung-scoped RateLimit never `blackout.record`. `replace_unavailable_rungs` never touches the other two maps | Fable never-blackout tests; `promote_once` unit tests |
| Escalation / `to_1m` onto blacked rungs | `escalate_task_model_if_needed_inner` (`recovery.rs:203`) and overflow `to_1m_model` (`post_output.rs:213`) do **not** read `unavailable_rungs` | both paths share those coordinators | **Same on both paths** (not a seq/wave split). PR-3 clamp gap | — |
| Leftover `account_usage_gate` | **not called** from `iteration.rs` | **not called** from wave | Production uses `run_account_quota_gate` only. Helper retained for old parity tests (`account.rs:71`) | `account_usage_gate_inner_same_decision_both_shapes` still claims to lock the production gate |

## Divergences

### 1. `.stop` during post-output RateLimit wait (Fable 3600 included)

- **Severity:** bug (high) — operator-visible exit code, `was_stopped`, auto-review suppression. Quota-wait is 3600s so operators will hit `.stop`.
- **File:** `src/loop_engine/iteration.rs:874-888` vs `src/loop_engine/wave_scheduler.rs:1101-1113` vs `src/loop_engine/orchestrator.rs:730-733`
- **Description:** Shared inner correctly returns `AccountReaction::Stop` when `wait()` is interrupted (or `StopSpend`). Wrappers disagree:

  | | Sequential | Wave |
  | --- | --- | --- |
  | outcome | `IterationOutcome::RateLimit`, `should_stop=true`, **`operator_stopped=false`** | `WaveTerminal { exit_code: 130, reason: "stop signal during rate-limit wait" }` |
  | orchestrator | RateLimit is not in the `should_stop` match → `_` arm: **exit 1**, `"stopped"`, **`was_stopped` stays false** | **exit 130**, **`was_stopped=true`** |
  | auto-review | `was_stopped=false` → may fire | suppressed |
  | batch `--chain` | nonzero failure | SIGINT-shaped stop |

  Sequential `.stop` at iteration start is exit 0 / `was_stopped=true` (`iteration.rs:100-115` + `orchestrator.rs:714-718`). The RateLimit-wait `.stop` does not reuse that mapping. Wave RateLimit-wait `.stop` looks like SIGINT (130), not a `.stop` file (0). `StopSpend` rides the same `AccountReaction::Stop` arm, so wave also reports spend-stop as “stop signal during rate-limit wait”.
- **Suggestion:** Map `AccountReaction::Stop` on **both** paths to the same triple: if the wait was `.stop`-interrupted → exit 0, `was_stopped=true`, reason `"stop signal during rate-limit wait"` (or the existing `"stop signal"`); if `StopSpend` → a distinct non-signal terminal (not 130). Set `operator_stopped: true` on the sequential RateLimit+Stop return. Lock with a seq 1-item vs wave N-item test that asserts `exit_code` + `was_stopped`, not just `AccountReaction`.
- **Would a test catch it?** Current `reaction_parity` Stop case only checks the inner enum. **No** existing test would catch this. Status: **open**. Pre-existing wrapper bug; PR-1 makes it load-bearing.

### 2. HorizonStopped / Deferred final-banner reasons collapse on sequential

- **Severity:** bug (medium) — operator-visible `print_final_banner` reason; live `ui::emit` strings **are** identical. Not quota-burning. `was_stopped` is correctly false on both (horizon does **not** look like a `.stop` file).
- **File:** `iteration.rs:166-202` + `orchestrator.rs:720-724` vs `wave_orchestration.rs:122-154`
- **Description:**

  | `UsageCheckResult` | Sequential live emit | Sequential `exit_reason` | Wave `reason` | `was_stopped` |
  | --- | --- | --- | --- | --- |
  | `HorizonStopped` | `"Quota horizon stop — …"` | **`"quota soft-stop"`** | **`"quota horizon stop"`** | false / false |
  | `Deferred` | `"Quota ask deferred …"` | **`"quota soft-stop"`** | **`"quota ask deferred"`** | false / false |
  | `StopSignaled` (usage wait) | `"Stop signal during usage wait, exiting"` | `"stop signal"` (via `operator_stopped`) | `"stop signal during usage wait"` | true / true |
  | Rung-only empty | `"All remaining todos are on unavailable rungs…"` | `"quota soft-stop"` (`orchestrator.rs:631`) | `"quota soft-stop"` (`wave_orchestration.rs:262`) | false / false |

  Sequential intentionally aliases horizon + deferred + rung-only to one banner reason. Wave distinguishes all three. A weekly-all horizon Stop on sequential therefore prints the same final reason as “all todos on unavailable rungs”.
- **Suggestion:** Thread distinct reasons through sequential `IterationResult` (new outcome variants, or a `stop_reason: &'static str`) so `orchestrator.rs` can emit the same three strings as `wave_orchestration.rs`. Do not fold horizon into `operator_stopped`.
- **Would a test catch it?** No dual-path test asserts `LoopResult` / `WaveTerminal.reason`. Inner tests only check `UsageCheckResult`. Status: **open**.

### 3. Pre-gate terminal iteration budget

- **Severity:** suggestion (medium) — not quota-burning; skews `iterations_completed` / proximity to `max_iterations`.
- **File:** sequential `Empty` is **not** in the give-back set (`orchestrator.rs:566-571`) so HorizonStopped / Deferred / usage-wait `StopSignaled` **consume** a loop iteration. Wave preflight sets `iteration_consumed: false` (`wave_orchestration.rs:109-110`, `127-128`, `144-145`).
- **Description:** Same `UsageCheckResult`, different budget. Rung-only empty is aligned (seq `NoEligibleTasks` consumes; wave `iteration_consumed: true`). Provider-blackout deferral is an older cousin: seq consumes then `continue`; wave `iteration_consumed: false` + `rate_limited_retry` (`wave_orchestration.rs:304-313`).
- **Suggestion:** Give back the iteration on sequential `Empty` quota terminals (treat like `RateLimit`), or consume on wave — pick one and lock it next to Issue 2.
- **Would a test catch it?** No. Status: **open**.

### 4. Progress-log gap on wave preflight quota terminals

- **Severity:** nit
- **File:** sequential HorizonStopped/Deferred still run `process_iteration_output` → `log_iteration` as `Empty` (`orchestrator.rs:389-413`). Wave returns from `wave_preflight_check` with **no** `log_iteration`. Wave rung-only empty **does** log (`wave_orchestration.rs:247-256`).
- **Description:** Horizon/Deferred progress entries exist only on sequential.
- **Suggestion:** Log `Empty` (or a named outcome) from wave preflight before returning the terminal, matching rung-only.
- **Would a test catch it?** No. Status: **open**.

### 5. Dead `UsageCheckResult::ApiError` match arms

- **Severity:** nit
- **File:** `iteration.rs:204-206` vs `wave_orchestration.rs:156-158`
- **Description:** Both arms exist and both continue. `run_account_quota_gate_inner` never returns `ApiError` (load `None` → `Skipped` / org fallback). Log format differs (`"usage API warning: {} (continuing)"` vs tracing field `msg`). Not reachable.
- **Suggestion:** Delete both arms or have the inner actually return `ApiError` on load failure if operators should see it. Status: **open**.

## reaction_parity.rs coverage gaps

**What is actually locked**

- `react_to_outputs_inner` 1-item vs N-item: wait-once, in_progress→todo, spillover Blackout, Fable Wait 3600 ignoring `api_secs`, never `provider_blackouts.record`, mixed-wave prefer-rung-scoped, hyphenated model-id negative, `/model` alone negative.
- Production I/O seams: Fable / mixed-wave skip `load_usage` / `usage_gate` / probe.
- Dual-predicate probe wiring (env off, Claude on) — pre-existing FEAT-002.
- `account_quota_preflight_inner_same_decision_both_shapes` (~4068): same buckets+policy → same `UsageCheckResult` **and** same proto-channel set — but it constructs two `QuotaPreflightParams` and calls the **same** function. It does not execute `iteration.rs` vs `wave_orchestration.rs`.
- `account_usage_gate_inner_same_decision_both_shapes` (~1083): **leftover**. Production seq/wave no longer call `account_usage_gate`. This test cannot regress PR-2 `execute_account_action`, proto-channel, or horizon mapping.

**Tests that claim parity but only check one path / the inner**

| Test | Claim | Reality |
| --- | --- | --- |
| `account_usage_gate_inner_same_decision_both_shapes` | seq/wave gate parity | leftover helper, unused in production |
| `account_quota_preflight_inner_same_decision_both_shapes` | seq/wave QuotaDecision | same inner, twice; no wrapper |
| `fable_rate_limit_wave_waits_once_3600_never_blackouts` | wave once/wave | inner N-item only; seq 1-item Fable+`api_secs=6d` is implied by sharing, not asserted |
| `fable_rate_limit_skips_usage_gate_and_probe` | seq+wave share skip | 1-item seam test; wave mixed skip is a separate test (good) |

**Missing locks (checklist)**

- Remaining banners seq vs wave (shared `eprintln` — low risk, still unasserted as I/O count).
- Proto-channel **replace** (not accumulate) and keep-on-API-fail — unit-tested in `account.rs`, not in `reaction_parity.rs`, not against both wrappers.
- Weekly-all horizon Stop → `was_stopped=false`, distinct from `.stop` file, **same `exit_reason` on both paths**.
- `LOOP_USAGE_CHECK_ENABLED=false` → zero OAuth/load **and** keep snapshot, from **both** call sites (inner is tested; wrappers only pass `usage_params.enabled`).
- `Wait { 0 }` ready-now vs `fallback_wait` 300 — `account.rs` only.
- `includeForced` / explicit `tasks.model` exclusion vs snapshot `has_forced`.
- `AccountReaction::Stop` → exit_code + `was_stopped` (Issue 1).
- Sequential orchestrator rung-only empty (wave has `test_run_wave_iteration_rung_only_empty_soft_stops_not_stale`; seq has only the shared helper tests).
- Sequential `Empty` + `operator_stopped: false` must not set `was_stopped` (horizon) — no test.

## Residual PR-3 risks that already diverge

These are **not** seq/wave splits today (both paths share the code). They will become splits if PR-3 clamps in only one path.

1. **No resolve-time unavailable-rung clamp.** `PlanContext` has no `unavailable_rungs`. `resolve_execution_plan` (`model.rs:990-1026`) and slot prompt (`prompt/slot.rs:214`) only see `provider_blackouts`. PR-2 exclusion is selection-`excluded_ids` only. A PR-3 down-only walker that is wired into `sequential.rs` but not `slot.rs` (or vice versa) is a classic parity event — grep all six files.
2. **Escalation / overflow ignore proto-channel.** `escalate_task_model_if_needed_inner` writes `tasks.model` up the Claude ladder with no `unavailable_rungs` check (`recovery.rs:231-244`). Overflow `to_1m_model` (`post_output.rs:213`) same. Both paths share the coordinators, so they do **not** currently diverge — they can both re-select a just-escalated frontier task after rung-only would have excluded it (next gate replace may re-exclude). PR-3 must skip escalate/`to_1m` onto blacked rungs in the **shared** helpers, not at one call site.
3. **Off-ladder explicit `tasks.model`.** `tier_of` miss → `anchored_tier` (`model.rs:997-999`). Family-match defer (`includeForced=false`) is PR-3. If implemented only in `compute_quota_excluded_ids` and not in remaining-work snapshot (or only in sequential `build_prompt`), `other_rungs_runnable` / empty-selection will disagree.
4. **Ask TTL continue leaves proto-channel empty when `tierFallback` forbade.** `apply_quota` copies `eval.unavailable` into `applied.unavailable` only when `tier_fallback_allows` (`account.rs:1150-1156`). Forbade + other rungs → `pending_ask`, proto-channel replaced with **empty**. TTL>0 wait then `WaitedAndReset` proceeds and can re-select frontier. Shared apply; PR-3 re-eval-on-cadence must not be seq-only.
5. **Next-PRD inherit of rung-unavailable** is documented as not implemented (`CLAUDE.md`). If PR-3 persists the HashSet, both `run_loop` entry paths (seq and `parallel_active`) must seed it.
6. **Escape valve vs new model writes.** Consecutive-failure escalation already refreshes `overflow_original_task_model` via `and_modify` (`recovery.rs:265-274`) and does **not** write `model_overrides`. A PR-3 clamp that writes `tasks.model` without `model_overrides` **or** `and_modify` on the snapshot will self-trip the valve and wipe `runner_overrides` (`promote_once`). Must be in the shared write site.

## Checks that passed (no seq/wave split)

- Production `account_usage_gate` leftover: **removed** from `iteration.rs` and `wave_orchestration.rs`; both call `run_account_quota_gate`.
- `LOOP_USAGE_CHECK_ENABLED` and Claude-disabled skip predicates match (env ∧ Claude for execute; Claude-enabled outer guard).
- Proto-channel replace-on-success / keep-on-fail / keep-on-env-off is one function.
- Fable/rung-scoped CLI: shared `decide_account_rate_limit` + `any()` I/O skip; wave 1 RateLimit + 2 completions → one 3600s wait, no `provider_blackouts.record`.
- Mixed wave prefers rung-scoped for **both** decide and I/O skip.
- Ordinary RateLimit still Blackout / `api_secs` on the shared inner.
- `unavailable_rungs` is a sibling of `BlackoutState`; `promote_once` does not read or write it; rung-scoped RateLimit does not `record`.
- `handle_rung_only_empty_selection` is a sibling of `handle_quota_deferral`, called first on both no-eligible paths; empty proto-channel + empty blackouts does not stale-abort when Exhausted.
- Remaining-work snapshot includes `in_progress` and `runner_overrides` pins; weekly_all beyond horizon Stops even if other Claude rungs look runnable.
- Horizon Stop does not set `was_stopped` / `operator_stopped` (not a `.stop` file) — only the banner **string** differs.
- `slot.rs` has no RateLimit / quota-gate / proto-channel logic.
