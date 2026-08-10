# Skip Anthropic usage/OAuth when Claude is disabled

**Type**: plan-tasks lean brief  
**Branch**: `feat/skip-anthropic-usage-claude-disabled`  
**Task list**: `tasks/skip-anthropic-usage-claude-disabled.json`  
**Prompt**: `tasks/skip-anthropic-usage-claude-disabled-prompt.md`  
**Source plan**: session plan (dual-predicate design review approved)

## Problem

When Claude is disabled (true Grok-only / Codex-only: `models.providers.claude.enabled=false`), the loop still treats Anthropic Max/Pro usage and Claude OAuth as loop-global infrastructure:

1. **Pre-iteration (default on via `LOOP_USAGE_CHECK_ENABLED`):** every iteration may call `oauth::ensure_valid_token()` (log: `OAuth token expiring, refreshing...`, bare `ureq` with **no timeout** → indefinite hang) and/or `account_usage_gate` → Anthropic usage API → wait up to **5h** after a successful above-threshold load.
2. **Post-output RateLimit:** when Claude is enabled, `react_to_outputs` may call `check_and_wait` (usage API) and/or `probe_rate_limit_lifted` (Claude CLI). Allow-flag is Claude-only; the usage-API leg still historically ANDs `usage_enabled`. Env is **not** a full Claude kill-switch (probe still runs).

Routing was already correct; the bug is provider-agnostic wiring of Claude-only account metering.

## Dual predicate (load-bearing — do not collapse)

```text
claude_provider_enabled =
  resolve_models_config(...).is_provider_enabled(Provider::Claude)

// PRE only
usage_params.enabled = LOOP_USAGE_CHECK_ENABLED && claude_provider_enabled

// POST allow-flag (Claude only — never assign from usage_params.enabled)
anthropic_account_io_allowed = claude_provider_enabled

// POST production wait splits:
//   check_and_wait     := anthropic_account_io_allowed && usage_enabled  (env still applies)
//   probe_rate_limit   := anthropic_account_io_allowed                   (env does NOT apply)
```

## In scope

- Startup gate for pre-iteration OAuth + usage check
- Post-output RateLimit: skip Anthropic load + Claude probe when Claude disabled; wait via CLI output / fallback
- Spend-limit UX not Claude-hardcoded when Claude disabled
- Docs: env semantics + Grok-only recipe (no invented `models set-primary`)
- Hermetic tests / reaction_parity seams

## Out of scope

- ureq timeouts on OAuth/usage when Claude is enabled
- Merge resolver / PRD mutate host selection (still Claude-hardcoded)
- Per-runner rate-limit metering when Claude enabled but task was Grok
- Removing redundant `ensure_valid_token` when usage gate is on
- Auto-disable Claude when enabling Grok
- Grok/Codex native usage APIs

## Success bar

- Claude-disabled loops never block on Anthropic OAuth/usage (pre or post)
- Claude-enabled defaults byte-identical for pre-gate
- `LOOP_USAGE_CHECK_ENABLED=false` with Claude on: pre off; post usage-API load off; early-lift **probe** still on
- Full quality gate green at REVIEW-001

## Key files / subsystems

- `src/loop_engine/startup.rs` — build `UsageParams`
- `src/loop_engine/reactions/account.rs` — pre gate leaf + post RateLimit
- `src/loop_engine/iteration.rs` / `wave_scheduler.rs` — construct `AccountReactionParams`
- `src/loop_engine/model.rs` — `is_provider_enabled`
- `src/loop_engine/oauth.rs` / `usage.rs` — Anthropic I/O chokepoints
- `tests/reaction_parity.rs` — hermetic parity

## Notes for review

- Do **not** gate post-output on `UsageParams.enabled` alone
- Sparse merge: enabling Grok does **not** disable Claude
- Primary hang symptom is indefinite OAuth refresh, not only the 5h wait
