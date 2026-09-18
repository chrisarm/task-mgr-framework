# Changelog — 2026-09-18

## Usage floors 2% / 1% and horizon CLI

**Branch**: `wip/usage-floor-horizon-cli`

### What shipped

Factory remaining floors are now **2% left** on session/other percent windows
and **1% left** on weekly (`weekly_all` / `weekly_scoped`). Horizon defaults
stay wait ≤ 60 min / stop > 12 h. Both floors and horizon are settable:

- Persist: `task-mgr models set-usage-policy` (sparse; does not wipe `rules`)
- Per-run: `--usage-remaining-min`, `--usage-remaining-min-weekly`,
  `--wait-if-reset-within`, `--stop-if-reset-beyond` on `loop run` / `batch run`
- Env: `LOOP_USAGE_REMAINING_MIN` (other only) and
  `LOOP_USAGE_REMAINING_MIN_WEEKLY`

Evaluate, `reset_at`, `check_and_wait`, and lift probes compare each window to
**its** floor (session 3% + weekly 1.5% proceeds). `models show` prints
`remainingMinWeeklyPercent`. `task-mgr how "quota"` and the cheatsheet list
the recipes.

### Why it matters

The old 8% floor left a large unused remainder. The new reserve is thin enough
to keep driving the loop, and still enough for wrap-up (`/compound`,
extract-learnings) plus a couple of extra manual session turns after a
horizon Stop.

### Breaking changes

- Factory `usagePolicy.remainingMinPercent` is **2**, not 8. Existing configs
  that already set `remainingMinPercent: 8` keep session/other at 8.
- Weekly uses a **new** field (`remainingMinWeeklyPercent`, default 1). Setting
  only `remainingMinPercent` does not change weekly.
- `LOOP_USAGE_REMAINING_MIN` no longer applies to weekly windows.

### Pick-up

```sh
task-mgr models show
task-mgr how "quota"
task-mgr models set-usage-policy --remaining-min 2 --remaining-min-weekly 1
```
