# Changelog — 2026-10-27

## Grok plan-limit and session cleanup

**Branch**: `feat/model-routing`
**PRD**: `tasks/prd-model-routing.md`

### What shipped

A Grok child that prints `rate limit for your plan` on stderr is `IterationOutcome::RateLimit` on the sequential and wave paths, with empty output, so crash counters do not move. The sniff is that phrase only. It does not call `is_rate_limited`. Spawn passes `--session-id` and deletes only that UUID directory. `pick_newest_by_mtime` is gone.

### Why it matters

A plan limit used to look like a crash, and session cleanup could delete a newer interactive Grok session. Operators can now tell a plan limit from a crash, and cleanup targets the directory this process created.

### Breaking changes

None. `detection::is_rate_limited` still matches the same phrase for Claude stdout. No migration and no config change.

---
