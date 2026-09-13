# Adversarial PRD validation — quota PR-1+PR-2

**Tree:** `feat/quota-rung-policy-pr2` HEAD `5a00b15` (combined PR-1+PR-2).
**Law:** `tasks/prd-quota-rung-policy.md` (human-review pins folded).
**Method:** production code + assertion bodies, not test names or prior review claims.

## Executive verdict

**PASS-WITH-RESIDUALS.** Zero FAIL against in-scope PR-1/PR-2 clauses. Two PARTIAL display residuals (banner duration compactness; post-output `check_and_wait` still prints the fetch-time builtin ladder). Previously reported Highs (hyphen-as-boundary, unanchored “switch models”, compile-time 92, mixed-wave prefer-rung-scoped, skip `load_usage_info` on rung-scoped RateLimit, UTF-8 floor, weekly-all >12h Proceed, `includeForced` global forbid, Wait{0}→300s, `LOOP_USAGE_CHECK_ENABLED=false` pre-OAuth, remaining-banner builtin extra-mark on the pre-dispatch path, horizon Stop as stop-file, rung-only stale-abort, `askTtlMinutes>0` always Deferred) are **CLOSED in production**, with tests that actually assert the discriminator.

PR-3 is not leaked: no `--use-other-models-ttl` clap, no `PlanContext.unavailable_rungs`, no down-only walker, no expiry map, no `set-usage-rule` / `set-tier-fallback` CLI. The PR-1 wave pin is still required (FR-002 3600 is still account-global). Off-ladder `tasks.model` family-match and spawn clamp remain PR-3 holes by spec.

**Count:** FAIL 0 · PARTIAL 2 · UNTESTED 1 (non-blocking).

---

## Requirement matrix

| ID | Requirement | Verdict | Evidence (prod + assertion) |
| --- | --- | --- | --- |
| PIN-1 | Frontier-low is not an account emergency — continue on standard | PASS | `apply_quota` factory + other rungs → `unavailable` + `Proceed` (`account.rs:1150–1250`, test `apply_factory_rung_low_other_runnable_is_unavailable_not_ask` asserts `Proceed` and frontier in `unavailable`). Exclude via `compute_quota_excluded_ids` (`pre_spawn.rs:322–388`, test `proto_channel_excludes_with_empty_provider_blackouts`). |
| PIN-2 | Engine language is capability rungs, never model ids except ingest | PASS | `rg "claude-fable-5\\|fable" src/loop_engine/quota.rs src/loop_engine/engine.rs` → 0. Ingest HUD table lives in `usage.rs:524–542` (`hud_tier_from_label`). `quota.rs` test `quota_module_has_no_model_id_literals` greps production half. `detection.rs` may match `fable` (allowed). |
| PIN-3 | Horizon: wait ≤1h; wait capped 1h–12h; stop >12h and nothing else can run; ask only if other rungs work **and** operator forbade. Factory **is** the downgrade instruction | PASS | Defaults `waitIfResetWithinMinutes: 60`, `stopIfResetBeyondHours: 12`, `askTtlMinutes: 0` (`quota.rs:97–108`, `project_config.rs` test `usage_policy_horizon_defaults_round_trip_sparse`). Factory `tierFallback` high / includeReview true / includeForced false (`project_config.rs:144–150`, test `tier_fallback_absent_key_is_factory_some`). Ask only when `tier_fallback_allows` is false and `other_rungs_runnable` (`account.rs:1161–1162`, `apply_forbade_null_tier_fallback_defers_when_other_runnable`). Weekly-all >12h Stops even if other Claude rungs “look” runnable (fold of pin 3; see FR-004.weekly). |
| FR-001.1 / US-001.a | Live fixture Fable 95 + session 24 + week 55 → account remaining ≈45 (used 55 inverted), reset_at session, no wait | PASS | Combined tree is remaining-unit. `parse_oauth_usage_json_with_threshold` folds only `five_hour`/`seven_day` + `limits[]` session/weekly_all (`usage.rs:647–670`). Test `test_parse_oauth_usage_live_fixture_ignores_scoped_and_named_rungs` asserts `percentage == 45`, `reset_at` session, `percentage > 8`. |
| FR-001.2 / US-001.b | Inverse weekly-all 100 → remaining 0, reset_at weekly | PASS | `test_parse_oauth_usage_weekly_all_100_still_gates` asserts `percentage == 0` and weekly `reset_at`. |
| FR-001.3 / US-001.c | Several account-binding windows ≤ floor → latest reset, not soonest | PASS | `latest_reset` (`usage.rs:761–771`). Test `test_parse_oauth_usage_latest_among_gate_relevant` asserts weekly timestamp wins over session. Live remaining-min 20: `test_parse_oauth_usage_live_remaining_min_20_weekly_15`. |
| FR-001.4 / US-001.d | `severity=critical` / `is_active` on scoped do not exhaust | PASS | Scoped skipped in fold (`usage.rs:668–669`). Tests `test_parse_oauth_usage_scoped_critical_does_not_exhaust`, `evaluate_severity_critical_is_not_default_low`. |
| FR-001.5 / US-001.e | Drop `seven_day_opus` / `seven_day_sonnet` from account fold | PASS | Named-key list is only `five_hour`, `seven_day` (`usage.rs:648`). Test `test_parse_oauth_usage_named_opus_sonnet_dropped` + live fixture. Ingest still walks them as generic buckets (FR-003). |
| FR-001.6 | `UsageInfo.percentage` remaining 0–100 in this combined tree; compare remaining > floor | PASS | Doc + invert `(100-used).clamp` (`usage.rs:697–701`). Gate: `remaining > f64::from(threshold)` (`account.rs:1639`, `check_and_wait` `account.rs:2072`). Old used≥92 ≡ remaining≤8. |
| FR-002.a / US-002.a | Live sentence is RateLimit | PASS | `is_rate_limited` `reached your` ∧ `limit` (`detection.rs:170`). Tests `test_is_rate_limited_positive`, `test_analyze_output_fable_limit_is_rate_limit_not_crash` assert `IterationOutcome::RateLimit`. RateLimit excluded from `handle_task_failure` (`orchestrator.rs:588–592`). |
| FR-002.b / US-002.b | 3600 override: model token + `limit` (hyphen **not** a boundary) **or** “switch models” on the rate-limit **line**; `/model` alone insufficient | PASS | `is_rung_scoped_rate_limit_message` (`account.rs:289–375`): word boundary is start/end/whitespace only (`is_real_word_boundary`). Tests `test_rung_scoped_predicate_narrow` (OPUS_MODEL / FABLE_MODEL / underscore / later-line switch models / `/model` / session / hit-your-limit all false). |
| FR-002.c / US-002.c | Plain session / `reached your … limit` keeps `api_secs`; spillover may Blackout | PASS | `test_decide_plain_session_limit_keeps_api_secs` Wait 7200; `test_decide_plain_session_limit_spillover_may_blackout` Blackout 7200. |
| FR-002.d / US-002.e | `You've hit your limit · resets 4pm` uses `output_secs` / may Blackout | PASS | `test_decide_hit_your_limit_resets_4pm_no_3600_override` Wait 500; `test_decide_model_id_plus_hit_your_limit_still_blackouts_under_spillover` Blackout. |
| FR-002.e / US-002.b-wait | Narrow match: Wait `blackout_fallback_secs` (3600), ignore `api_secs` and `output_secs`, never Blackout, never 300s fallback | PASS | `decide_account_rate_limit` early return (`account.rs:248–251`). Test `test_decide_fable_ignores_api_secs_waits_blackout_fallback` with `api_secs = 6 days` (not None). Spillover: `test_decide_fable_spillover_still_waits_never_blackout`. |
| FR-002.f / US-002.d | No `usage_gate` / `probe_rate_limit_lifted` / `load_usage_info` for that phrasing | PASS | Wrapper skip (`account.rs:488–545`). Tests `fable_rate_limit_skips_usage_gate_and_probe` (boom `load_usage`, `usage_gate_calls==0`, probe unwired, wait 3600) and `mixed_wave_rung_scoped_still_skips_usage_gate_and_probe`. |
| FR-002.g | No dated Sep 12 parse in PR-1 (still None) | PASS | `test_parse_reset_from_output_sep_month_token_stays_none`. Fable 3600 is phrasing, not dated parse. |
| FR-002.h / US-002.g | Wave: one Fable RateLimit + two completions → exactly one 3600s wait, no `provider_blackouts.record` | PASS | `tests/reaction_parity.rs::fable_rate_limit_wave_waits_once_3600_never_blackouts` asserts `spy.calls==1`, `last_secs==3600`, `blackout.active` empty, even with `spillover_enabled` and `api_secs=6d`. |
| FR-002.i | Mixed-wave prefer rung-scoped item for decide | PASS | `react_to_outputs_inner` finds first rung-scoped RateLimit (`account.rs:581–591`). Test `mixed_wave_prefers_rung_scoped_rate_limit_over_leading_account_hit` Wait 3600, blackout empty. |
| FR-002.j | UTF-8 slice floor on token window | PASS | `floor_char_boundary` (`account.rs:366`). Test `test_model_token_window_floors_utf8_char_boundary` (no panic; `opus`+63+`€` false; `opus limit` true). |
| FR-002.k / US-002 pin | PR-1 pin still required for parallel/wave until PR-3 | PASS | FR-002 Wait is still account-global once per wave. Documented in `CLAUDE.md` and `src/loop_engine/CLAUDE.md`. No spawn clamp (`PlanContext` has `provider_blackouts` only — `model.rs:949–961`). |
| FR-003.ingest | Walk every object sibling + `limits[]`; null skip; no window-name allow-list; unknown → None/ignore | PASS | `ingest_oauth_value` (`usage.rs:318–349`). Tests `ingest_live_fixture_emits_all_siblings_and_limits` (null skipped, nimbus_quill no rungs), `ingest_skips_malformed_and_accepts_u64_or_f64_percent`, `evaluate_unknown_kind_low_is_ignore`. |
| FR-003.hud | HUD Fable→frontier, Opus→standard, Sonnet→cost-efficient, Haiku→cheapest | PASS | `hud_tier_from_label` (`usage.rs:524–542`). Live Fable row → frontier only on builtin (`ingest_live_fixture_emits_all_siblings_and_limits`). Opus HUD without pin → standard only (`ingest_hud_only_without_pin_leaves_frontier_unmarked_on_opus`). |
| FR-003.extra | After HUD map, extra-mark every rung whose configured model string equals mapped rung’s model | PASS | `extra_mark_rungs` exact `exact_model_for` equality (`usage.rs:571–590`). Test `ingest_extra_mark_after_frontier_pin_marks_standard_and_frontier`. `tier_of` stays exact-match (`model.rs:802–808`). |
| FR-003.run-models | Production ingest uses run `ResolvedModelsConfig` not builtin | PARTIAL | Pre-dispatch: `run_account_quota_gate` re-ingests via `buckets_for_run_models` + `remaining_banner_for_run_models` (`account.rs:1489–1503`, tests `gate_buckets_for_run_models_extra_mark_under_frontier_opus_pin`, `remaining_banner_extra_marks_frontier_under_opus_pin`). Fetch still snapshots builtin (`usage.rs:185–194`). Post-output `check_and_wait` still `eprintln`s `usage.remaining_banner` (builtin) (`account.rs:2061–2062`). See PARTIAL subsection. |
| FR-003.spend | Spend stop only remaining amount ≤ 0 | PASS | Evaluate: dollars/tokens never use percent floor; amount ≤ 0 → AccountLow (`quota.rs:225–228`, tests `evaluate_spend_amount_positive_ignores_percent_floor`, `evaluate_spend_amount_zero_emits_account_low`). Apply: `low.remaining <= 0` (`account.rs:1196–1200`, `apply_spend_stops_only_at_zero_not_percent_floor`). No `remainingAmount` JSON key ingested (walks `utilization`/`dollars`/`tokens` as specified). |
| FR-004.eval | `evaluate_quota` pure, per-bucket, no ask, no `other_rungs_runnable` | PASS | Signature `(&[QuotaBucket], &UsagePolicy, u8)` (`quota.rs:166–169`). `BucketEval` has Ignore / Unavailable / AccountLow only. Tests `evaluate_account_binding_low_emits_account_low_not_ask`, `evaluate_does_not_take_other_rungs_runnable`, explicit onLow wait/stop/ask emit AccountLow not Ask (`evaluate_explicit_on_low_wait_on_weekly_scoped_emits_account_low`). |
| FR-004.apply | Apply resolves ask/wait/stop/unavailable; stop beats ask; account wait/stop can coexist with rung unavailable | PASS | `apply_quota` (`account.rs:1137–1275`). `apply_stop_beats_ask`. Factory unavailable + Proceed. Explicit stop/ask/wait on scoped honored (`apply_explicit_*`). |
| FR-004.exclude | Exclude unavailable rungs from next selection; do not account-wait for a scoped rung; 3600s only when remaining queue cannot run (apply layer) | PASS | `compute_quota_excluded_ids` runs on empty provider blackouts (`pre_spawn.rs:322`). Scoped heuristic → Unavailable, not AccountLow (`quota.rs:278–281`, `evaluate_rung_scoped_low_emits_unavailable`). Only-frontier + 6d → Stop not Wait 3600 (`apply_only_frontier_6d_no_fallback_stops`). Factory + other rungs → Proceed (`apply_factory_rung_low_other_runnable_is_unavailable_not_ask`). CLI FR-002 3600 is a separate coordinator (pin still required). |
| FR-004.spillover | Spillover is never a working rung | PASS | Snapshot resolves with **empty** blackout set (`account.rs:1728–1755`). |
| FR-004.proto | Proto-channel `HashSet<(Provider, CapabilityTier)>` on `IterationContext`: REPLACE on successful evaluate; KEEP on API fail | PASS | Field (`engine.rs:453–461`). `replace_unavailable_rungs` clear+extend (`account.rs:1388–1393`). Tests `preflight_successful_evaluate_replaces_stale_proto_channel`, `preflight_keeps_snapshot_on_api_fail`, `preflight_disabled_keeps_proto_channel_snapshot`. |
| FR-004.no-deferral | Do not reuse `handle_quota_deferral` for rung-only | PASS | Sibling `handle_rung_only_empty_selection` (`account.rs:760–793`) called **before** deferral in `orchestrator.rs:616` and `wave_orchestration.rs:235`. Tests `rung_only_empty_exhausts_resets_in_progress_no_deferral` (deferral Inactive; in_progress→todo; Exhausted). |
| FR-004.onLow | Explicit onLow wins over heuristic | PASS | Evaluate match (`quota.rs:252–274`). Apply `explicit_on_low_for_bucket` (`account.rs:1176–1193`). Tests wait/stop/ask on weekly_scoped. |
| FR-004.horizon | 3h session waits (capped); 5h–12h cap-and-repark; latest among wait buckets | PASS | `MAX_WAIT_SECS = 5*3600` (`account.rs:1043`). Tests `apply_account_low_3h_waits_capped`, `apply_account_low_3h_waits_capped_even_when_other_rungs_runnable`, `apply_only_frontier_6h_cap_and_repark_not_stop` (`secs == MAX_WAIT_SECS`), `apply_latest_reset_among_multiple_wait_buckets` (2h+6d → Stop). |
| FR-004.weekly | Account-binding weekly remaining ≤ floor with >12h reset STOPs even if other Claude rungs look runnable | PASS | `has_account_binding_wait` beyond horizon → Stop, ignoring `other_rungs_runnable` (`account.rs:1235–1239`). Test `apply_account_weekly_all_6d_stops_even_when_other_rungs_runnable` for remaining 5 and 0. |
| FR-004.includeForced | Factory `includeForced: false` must **not** globally forbid unavailable when any task has `tasks.model` | PASS | `tier_fallback_allows` does not consult `has_forced` (`account.rs:1106–1120`). Test `apply_factory_with_forced_model_still_unavailable_proceed` asserts `tier_fallback_allows`, unavailable frontier, `Proceed` (not Defer). |
| FR-004.wait0 | Wait{0} is ready-now, not 300s fallback | PASS | `execute_quota_account_action` (`account.rs:1659–1665`); `wait_for_usage_reset_inner` (`account.rs:1838–1840`); `resolve_wait_secs` preserves `Some(0)` (`account.rs:215–220`). Tests `execute_wait_zero_is_ready_now_not_fallback_300`, `org_fallback_reset_zero_is_ready_now_not_fallback_300`, `test_wait_zero_is_ready_not_fallback`. |
| FR-004.env | `LOOP_USAGE_CHECK_ENABLED=false` skips pre-iteration OAuth | PASS | `orchestrator.rs:148` `ensure_valid_token` only if `usage_params.enabled`. `run_account_quota_gate_inner` returns Skipped before `load_usage` (`account.rs:1484–1486`). `usage_params.enabled = env ∧ Claude` (`startup.rs:990–1007`, `engine.rs:123–131`). Test `gate_disabled_skips_usage_load_and_keeps_snapshot` (`load_calls==0`). |
| FR-004.askTtl | Config `askTtlMinutes` 0 = Defer no sleep; >0 waits that many minutes (CLI flag is PR-3) | PASS | `ask_or_defer` (`account.rs:1278–1285`). Tests `execute_ask_ttl_0_defers_without_sleep` (wait vec empty, Deferred), `execute_ask_ttl_15_sleeps_900s_then_continues` (exactly `[900]`), `execute_ask_ttl_15_stop_signal_exits` → StopSignaled. Re-eval on stop-check cadence is PR-3. |
| US-003.a / FR-008 | Remaining `% left` banners; no used-percent | PARTIAL | `format_remaining_usage_banner` (`usage.rs:912–962`) labels session/week/`CapabilityTier::as_str()`, never `fable`. Test `test_live_fixture_remaining_banner_shape` asserts `76% left`, `5% left`, `frontier`, `(floor 8%)`, no `95%` / `Usage:` / `threshold:`. Duration uses shared `format_duration` (`display.rs:14–34`) → a 3-minute session prints `(3m 0s)` not spec `(3m)`. Test does not pin the exact AC string (also ORs `45% left \|\| week`). |
| US-003.b | `usage_remaining_min` default 8; env > config `remainingMinPercent` > 8; `LOOP_USAGE_THRESHOLD` preflight ERROR | PASS | Default 8 (`config.rs:98`, `quota.rs:97–98`). `resolve_usage_remaining_min` (`config.rs:171–172`) used at `startup.rs:998–1005` with `usage_policy.remaining_min_percent`. Tests `test_resolve_usage_remaining_min_precedence`, `test_usage_policy_remaining_min_deserializes` (12). Preflight hard-error `project_config.rs:1065–1071`, test `test_preflight_hard_errors_on_legacy_loop_usage_threshold_env` (message names both envs). `from_env` ignores legacy (`test_from_env_ignores_legacy_usage_threshold`). Non-loop commands never call this chokepoint. |
| US-003.c | Dollar/token buckets print in their unit | PASS | `format_measurement_left` (`usage.rs:1024–1030`). Test `test_remaining_banner_dollar_unit` asserts `$12.5 left`. |
| US-004 | Horizon + factory downgrade + ask opt-out + exclude (see FR-004 rows) | PASS | Covered above. JSON-null `tierFallback` is ask opt-out (`tier_fallback_explicit_null_is_none_ask_opt_out`). |
| US-005 / FR-005 / FR-006 / US-006 / US-007 | Ask CLI TTL, expiry map, down-only walker, family-match at resolve, policy CLI | N/A-PR3 | No clap `--use-other-models-ttl` in `src/`. `PlanContext` has no `unavailable_rungs`. No `set-usage-rule` / `set-tier-fallback` handlers. `hud_tier_from_label` is not called from `model.rs`. |
| Proto.empty | Rung-only empty selection must not fall through to stale-abort | PASS | Both no-eligible paths. Sequential Exhausted → `exit_reason = "quota soft-stop"` **without** `was_stopped` (`orchestrator.rs:626–632`). Wave: `was_stopped: false`, reason `"quota soft-stop"` (`wave_orchestration.rs:245–268`). Tests listed under FR-004.no-deferral. |
| Proto.horizon-stop | Horizon Stop must not be reported as operator stop-file | PASS | Distinct `UsageCheckResult::HorizonStopped` (`usage.rs:113–117`). Execute maps Stop → HorizonStopped not StopSignaled (`account.rs:1672`, test `execute_horizon_stop_returns_horizon_stopped_not_stop_signaled`). Sequential: `operator_stopped: false` (`iteration.rs:166–176`) → orchestrator Empty branch `"quota soft-stop"` not `"stop signal"` (`orchestrator.rs:714–724`). Wave: reason `"quota horizon stop"`, `was_stopped: false` (`wave_orchestration.rs:122–136`). |
| Proto.chain | Account-binding stop DOES stop the chain; Defer/rung-scoped must not skip via **was_stopped** | PASS | `was_stopped` is only set on `.stop` (`orchestrator.rs:139`, `714–718`). Batch stop-file skip is `if was_stopped` (`batch.rs:680`). Account-binding HorizonStopped leaves todos (`in_progress` reset) so `prd_complete == false` → `chain && (exit_code != 0 \|\| !prd_complete)` aborts remaining PRDs (`batch.rs:712`). Rung-scoped inherit/continue is PR-3 (`account_quota_stopped` does not exist). Incomplete-PRD chain stop for forbade-Defer is specified OK. |
| FR-007.pre | Pre-iteration OAuth + account gate: `LOOP_USAGE_CHECK_ENABLED ∧ Claude enabled` | PASS | `claude_usage_check_enabled` (`engine.rs:123–131`). Gate `execute_account_action: usage_params.enabled` (`iteration.rs:131–145`, `wave_orchestration.rs:87–104`). |
| FR-007.post-api | Post RateLimit **usage-gate** (`check_and_wait`): `anthropic_account_io_allowed ∧ usage_enabled` | PASS | `account.rs:533`. Matrix `post_output_anthropic_io_matrix_over_env_and_claude`: `(false, true) → usage_gate_calls 0`, load still 1 (api_secs is Claude-only, **not** the usage-gate leg — matches PRD table footnote). |
| FR-007.post-probe | Post RateLimit early-lift probe: Claude only (env does not apply), except FR-002 skip | PASS | Probe wired `(!rung_scoped && anthropic_account_io_allowed)` (`account.rs:544–545`). Matrix `(false, true) → probe_wired Some(true), probe_calls 1`. Fable skip: tests under FR-002.f. Known-bad collapsed-flag test `known_bad_collapsed_flag_drops_the_probe_on_env_off_claude_on` documents the discriminator. |
| NG.blackout | Do not record provider blackout for rung-scoped CLI even when spillover on | PASS | FR-002.e/h tests. |
| NG.persist | Do not persist last-fetched buckets | PASS | `rg` of INSERT/persist bucket → none. Proto-channel is in-memory HashSet. |
| NG.severity | Do not treat severity/is_active as default low | PASS | FR-001.4 + `evaluate_severity_critical_is_not_default_low`. |
| NG.tier_of | Do not reintroduce substring `tier_of` | PASS | `tier_of` is exact `value.as_deref() == Some(base)` (`model.rs:802–808`). Allowed ingest `contains` is unlabeled family token vs configured model string (`usage.rs:593–612`). Extra-mark is string equality. |

---

## FAIL / PARTIAL subsections

### PARTIAL — FR-003.run-models (post-output builtin banner)

**Prod:** `fetch_oauth_usage` always ingests + formats with `builtin_resolved_models()` (`usage.rs:185–194`). Pre-dispatch correctly rebuilds (`account.rs:1489–1503`). Post-output `check_and_wait` (ordinary RateLimit usage-gate, **not** rung-scoped — those skip the gate) still prints `usage.remaining_banner` (`account.rs:2061–2062`), which is the builtin snapshot.

**Attack:** Operator pins `set-tier claude frontier <opus>`. An ordinary (non-Fable) RateLimit that takes the usage-gate prints an Opus HUD line as `standard` only. Frontier remains unlabeled on that stderr path even though evaluate/apply extra-marked both. Pre-iteration banner is correct.

**Correct extra test:** drive `check_and_wait` (or a seam around it) with `oauth_json` + pinned models and assert the printed banner contains both `frontier` and `standard`. Today only `remaining_banner_for_run_models` is asserted, and `check_and_wait` does not call it.

**Residual:** display-only on the post RateLimit usage-gate path. Selection/evaluate extra-mark is correct.

### PARTIAL — US-003.a banner exact shape

**Prod:** labels and remaining percents match the AC. Time-left goes through `display::format_duration` (`display.rs:14–34`): sub-hour is `"{m}m {s}s"`, so a 3-minute session reset is `(3m 0s)` not spec `(3m)`. Day band `5d 13h` does match.

**Attack:** none functional. Operators see `3m 0s` instead of the documented compact form. The live-fixture test would still pass if the session label were missing but `76% left` appeared on a spend/rung segment (`45% left || week` is an OR).

**Correct extra test:** pin `now = 2026-09-07T05:57:00Z` against the live fixture **without** spend (or accept spend as an extra segment per US-003.c) and assert the exact string, including `session 76% left (…)` and `week 45% left (5d 13h)`.

---

## Closed prior findings

### PR-1

| Finding | Status | Evidence |
| --- | --- | --- |
| Hyphen-as-boundary 3600 false positive (`claude-opus-5` + `hit your limit`) | **CLOSED** | `is_real_word_boundary` rejects `-`/`_`. Tests `test_rung_scoped_predicate_narrow`, `test_decide_model_id_plus_hit_your_limit_still_blackouts_under_spillover`, `model_id_in_session_rate_limit_stdout_still_blackouts_not_3600`. |
| Unanchored “switch models” (whole stdout) | **CLOSED** | Line-scoped `switch_models_on_rate_limit_line`. Tests mixed later-line commentary keep `output_secs` (`test_decide_switch_models_on_later_line_keeps_output_secs`). |
| Compile-time 92 for `reset_at` | **CLOSED** | `parse_oauth_usage_json_with_threshold(json, remaining_min)` (`usage.rs:641`). Production load passes live floor (`load_usage_info_with_threshold`). Test `test_parse_oauth_usage_live_remaining_min_20_weekly_15` (floor 20 picks weekly; default 8 keeps session). |
| Mixed-wave prefer-rung-scoped | **CLOSED** | `react_to_outputs_inner` prefer-find (`account.rs:581–591`) + tests in `reaction_parity.rs`. |
| Skip `load_usage_info` on rung-scoped RateLimit | **CLOSED** | Wrapper `rung_scoped` skip (`account.rs:509`) + boom-load tests. Mixed wave too. |
| UTF-8 slice floor | **CLOSED** | `floor_char_boundary` + `test_model_token_window_floors_utf8_char_boundary`. |

### PR-2 Highs

| Finding | Status | Evidence |
| --- | --- | --- |
| Weekly-all >12h Proceed when other rungs look runnable | **CLOSED** | `has_account_binding_wait` → Stop (`account.rs:1235–1239`). Test `apply_account_weekly_all_6d_stops_even_when_other_rungs_runnable`. |
| `includeForced` globally forbidding unavailable | **CLOSED** | `tier_fallback_allows` ignores `has_forced`. Test `apply_factory_with_forced_model_still_unavailable_proceed`. |
| Wait{0} rewritten to 300s | **CLOSED** | Execute + wait-inner + org fallback tests listed under FR-004.wait0. |
| `LOOP_USAGE_CHECK_ENABLED=false` still hitting OAuth (pre) | **CLOSED** | `ensure_valid_token` gated; gate inner returns before load. Test `gate_disabled_skips_usage_load_and_keeps_snapshot`. Post api_secs load with env off + Claude on is the dual-predicate **by design** (FR-007). |
| Remaining banner builtin ladder | **CLOSED** on pre-dispatch extra-mark (`remaining_banner_for_run_models` / `buckets_for_run_models`). **Residual** on post-output `check_and_wait` (PARTIAL above). |
| Horizon Stop reported as stop-file | **CLOSED** | `HorizonStopped` ≠ `StopSignaled`; `operator_stopped: false`; wave `was_stopped: false`; reason `"quota horizon stop"` / `"quota soft-stop"`. Test `execute_horizon_stop_returns_horizon_stopped_not_stop_signaled`. |
| Rung-only empty selection stale-abort | **CLOSED** | Sibling helper before deferral/stale. Tests `rung_only_empty_*`. |
| `askTtlMinutes>0` always Deferred | **CLOSED** | `execute_ask_ttl_15_sleeps_900s_then_continues` asserts `[900]` and `WaitedAndReset`. TTL-expiry defer-if-forbade re-eval is PR-3 (US-005). |

---

## PR-3 leakage / missing PR-2 holes

**No PR-3 leakage into this tree:**

- No `--use-other-models-ttl` clap (only comments that it is PR-3).
- `PlanContext` has `provider_blackouts` only (`model.rs:949–961`).
- No down-only walker; `resolve_execution_plan` still early-returns `EXPLICIT_MODEL` via `tier_of` / `anchored_tier` (`model.rs:994–1007`).
- Proto-channel is a HashSet, not an expiry map; no `active_rungs`.
- No `models set-usage-rule` / `set-tier-fallback` / `unset-tier-fallback` CLI (comment in `apply_explicit_wait_on_weekly_scoped_honors_wait` refers to a future flag; apply honors JSON `usagePolicy.rules` already — that is PR-2).
- Config `askTtlMinutes` consumed at apply time is specified PR-2 (`account.rs` ~1245/1252), not CLI leakage.

**PR-2 holes the PRD said PR-3 closes (not in-scope misses):**

- All-frontier / all-review / explicit-frontier queue: exclude + `handle_rung_only_empty_selection` **soft-stops** instead of clamp-down to standard. Pin still required for wave.
- Off-ladder `tasks.model` (`claude-fable-5-1`): `tier_of` is None → difficulty window, may still dispatch Fable (3600 whole wave). Family-match at resolve is US-006/FR-006.
- Rung-scoped HorizonStopped still breaks `batch --chain` via `!prd_complete` (same as account-binding). Discriminator `account_quota_stopped` is PR-3.
- Ask TTL > 0 then continues (`WaitedAndReset`) without re-reading `tierFallback`; forbade+TTL expiry defer is US-005.

**Not a hole:** factory `includeForced: false` leaving forced on-ladder frontier tasks **excluded per-id** by the proto-channel is correct PR-2 exclude (the High was global Defer of the whole PRD).

---

## Tests that give false confidence

1. **`test_live_fixture_remaining_banner_shape`** (`usage.rs:1554`) — name says “shape”; assertions are `contains` fragments plus `45% left || week`. Passes `3m 0s`, extra `spend $12.5 left`, and would pass a mis-labeled session. Does **not** lock the AC string.

2. **`test_usage_check_result_variants`** (`usage.rs:1623`) — tautological `assert_eq!(BelowThreshold, BelowThreshold)` etc. Proves nothing about production mapping.

3. **`quota_module_has_no_model_id_literals`** — useful grep-in-test, but only covers `quota.rs`. `engine.rs` is the other AC grep target; it is clean at HEAD but this test would not catch a `fable` literal landing there.

4. **`parse_oauth_usage_json_signature_stays_threshold_only`** — compile-time `fn` type alias. Good API freeze; does not prove the live fixture fold.

5. **`evaluate_does_not_take_other_rungs_runnable`** — same: signature freeze only. Behavior is covered by other evaluate tests.

None of these hide a FAIL: the behavioral tests cited in the matrix assert the discriminators (6-day `api_secs`, boom `load_usage`, `has_forced` still Proceed, Wait{0} vs 300, weekly-all Stop with `other_rungs_runnable: true`).

---

## Assumptions / unverified

- Did not run the test suite (per brief). Assertions were read in source.
- Live Anthropic HUD JSON `remainingAmount` key is not ingested; FR-003 specifies `utilization`/`dollars`. Spend stop is on those measurements ≤ 0. Marked UNTESTED for an unseen `remainingAmount` field, not a spec miss.
- `LOOP_USAGE_THRESHOLD` hard-error is only at `preflight_validate_and_probe` (loop/batch). Non-loop commands ignoring it is specified.
- Sequential vs wave `exit_reason` strings differ (`"quota soft-stop"` vs `"quota horizon stop"`) but both keep `was_stopped == false`.
