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
