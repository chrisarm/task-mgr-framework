# Changelog — 2026-09-07

## Quota buckets, remaining headroom, and capability-rung policy (PR-1)

**Branch**: `feat/quota-rung-policy-pr1`
**PRD**: `tasks/prd-quota-rung-policy.md`

### What shipped

Account wait no longer folds Fable weekly / `weekly_scoped` / `seven_day_opus` /
`seven_day_sonnet` into the session clock. Gate remaining uses only session +
weekly-all **used** percent. `"You've reached your Fable limit"` is RateLimit
with a 3600s Wait that ignores API/CLI reset dates, does not Blackout the whole
Claude provider, and does not early-lift. Hyphenated model ids in ordinary
session stdout do not take that override.

### Why it matters

Live loops were parking for days on a 95% Fable weekly bucket while session and
other rungs still had headroom. After PR-1, that false account park is gone.
Frontier-routed tasks still need
`task-mgr models set-tier claude frontier <standard-model>` until PR-3
automatic clamp; the pin is **required** for parallel/wave (one Fable RateLimit
sleeps the whole wave).

### Breaking changes

None for operators. `UsageInfo.percentage` is still used 0–100 (remaining
rename is PR-2). Old `LOOP_USAGE_THRESHOLD` still applies in PR-1.

---

## Quota buckets, remaining headroom, and capability-rung policy (PR-2)

**Branch**: `feat/quota-rung-policy-pr2`
**PRD**: `tasks/prd-quota-rung-policy.md`

### What shipped

Operators and the gate now speak **remaining percent left** (`usage_remaining_min`
default 8; `LOOP_USAGE_REMAINING_MIN`; old `LOOP_USAGE_THRESHOLD` is a preflight
error). Generic `QuotaBucket` ingest extra-marks rungs by configured model
string. Horizon: wait ≤1h, 1h–12h capped at 5h, weekly-all >12h **stops** this
PRD. Factory `tierFallback` auto-marks frontier unavailable and continues on
standard; `includeForced: false` does not park the whole run. Remaining banners
use the run's model pin.

### Why it matters

Frontier weekly-out no longer waits the account. Standard work proceeds. A 6-day
weekly-all outage stops instead of 5h-cap-looping. Dual predicate still skips
pre-gate OAuth when `LOOP_USAGE_CHECK_ENABLED=false`.

### Breaking changes

- `LOOP_USAGE_THRESHOLD` (used-percent) is a hard preflight error. Use
  `LOOP_USAGE_REMAINING_MIN` (default 8).
- Wait banners print `% left`, not `% used`.

---

## Quota buckets, remaining headroom, and capability-rung policy (PRE-PR-3 / 2b)

**Branch**: `feat/quota-rung-policy-pr2b`
**PRD**: `tasks/prd-quota-rung-policy.md`

### What shipped

HUD extra-mark is an identity **union** (family constant plus snapshot id): Fable HUD + frontier→opus pin marks **frontier only**, so mixed standard work continues. Unlabeled `seven_day_*` siblings ingest with `rungs: None`. Preflight waits probe after apply (`Wait { secs, account_binding }`); post-output `WaitFn` stays `Fn(u64)`. Operator `.stop` vs spend/horizon stop are `OperatorStopped` vs `StopSpend` (sequential Empty mapping; both wave paths exit 0). `extra_usage` is Ignore at evaluate, not AccountLow. Pin is optional after extra-mark.

### Why it matters

The PR-2 extra-mark hole parked standard from a Fable HUD row when operators pinned frontier off Fable. After this gate the pin is optional; factory exclude already unsticks mixed work. All-high / review clamp remains PR-3. A Fable-routed spawn still 3600s-sleeps the wave.

### Breaking changes

None for operators. Sequential operator-stop is Empty + `operator_stopped` (exit 0), not RateLimit. Wave StopSpend is exit 0, not 130.

---

## Quota buckets, remaining headroom, and capability-rung policy (PR-3)

**Branch**: `feat/quota-rung-policy-pr3`
**PRD**: `tasks/prd-quota-rung-policy.md`

### What shipped

`--use-other-models-ttl` on `loop run` / `batch run` (`Some(0)` ≠ omitted; TTL 0 defers with no sleep). Ask wait re-runs evaluate/apply on each stop-check so mid-wait `onLow: stop` is a horizon stop, not operator `.stop`. Proto-channel expiry map + down-only clamp at all three sites (exclude / spawn / overflow); walker uses `exact_model_for` only. Factory + only-frontier-left is Proceed + clamp. `models set-usage-rule` / `set-tier-fallback` / JSON-null unset. Batch `--chain` aborts on `account_quota_stopped` even when the expiry map is non-empty (`StopSpend` sets the flag in both seq and wave wrappers).

### Why it matters

Frontier-low continues on standard without a `set-tier` pin: factory marks frontier unavailable and the walker clamps all-high / review onto a working rung. A credits/spend stop no longer lets `--chain` inherit into the next PRD just because some rungs were already blacked.

### Breaking changes

- `--use-other-models-ttl 0` is explicit defer (not “use config”). Omit the flag to keep `usagePolicy.askTtlMinutes`.
- `models unset-tier-fallback` writes JSON `null` (ask opt-out); it does not delete the key.

---
