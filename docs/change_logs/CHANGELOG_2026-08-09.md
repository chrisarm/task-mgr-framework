# Changelog — 2026-08-09

## Skip Anthropic usage/OAuth when Claude is disabled

**Branch**: `feat/skip-anthropic-usage-claude-disabled`
**PRD**: `tasks/skip-anthropic-usage-claude-disabled.md`

### What shipped

True Grok-only / non-Claude loops no longer hit Anthropic OAuth refresh or
usage APIs. Pre-iteration gate is `LOOP_USAGE_CHECK_ENABLED ∧ Claude enabled`
(`claude_usage_check_enabled` → `UsageParams.enabled`). Post RateLimit uses a
separate allow-flag `anthropic_account_io_allowed` (Claude only); production wait
skips `check_and_wait` + early-lift probe when Claude is disabled while still
waiting via CLI output parse / fallback. Docs cover Grok-only setup (no invented
`models set-primary`) and the dual-predicate env semantics.

### Why it matters

Claude-disabled runs were hanging on bare `ureq` OAuth refresh (`OAuth token
expiring, refreshing...`) and could wait hours on Anthropic usage. Operators
can now disable Claude and run Grok/Codex without Anthropic account side effects.

### Breaking changes

None for default Claude-enabled configs (pre-gate remains on with env default).
Operators who set `LOOP_USAGE_CHECK_ENABLED=false` should know: pre OAuth/usage
off; post early-lift probe still runs while Claude is enabled.

---
