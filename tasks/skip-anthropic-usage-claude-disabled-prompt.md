# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Skip Anthropic usage/OAuth when Claude is disabled** for **task-mgr**.

## Problem Statement

When Claude is disabled (true Grok-only / Codex-only: `models.providers.claude.enabled=false`), the loop still treats Anthropic Max/Pro usage and Claude OAuth as loop-global infrastructure:

1. **Pre-iteration** (default `LOOP_USAGE_CHECK_ENABLED=true`): every iteration may call `oauth::ensure_valid_token()` — log `OAuth token expiring, refreshing...` then bare `ureq` with **no timeout** (primary hang) — and/or `account_usage_gate` → Anthropic usage API → wait up to **5h** only after a successful above-threshold load (secondary).
2. **Post-output RateLimit**: when Claude is enabled, production wait may call `check_and_wait` (usage API) and/or `probe_rate_limit_lifted` (Claude CLI). Allow-flag is Claude-only; usage-API leg still ANDs `usage_enabled` historically. Env is **not** a full kill-switch for the probe.

Routing was already fine. Fix = dual predicate (do not collapse):

```text
claude_provider_enabled = resolve_models_config(...).is_provider_enabled(Provider::Claude)

// PRE only
usage_params.enabled = LOOP_USAGE_CHECK_ENABLED && claude_provider_enabled

// POST allow-flag (Claude only — never from usage_params.enabled)
anthropic_account_io_allowed = claude_provider_enabled

// POST production wait splits:
//   check_and_wait   := anthropic_account_io_allowed && usage_enabled  (env still applies)
//   probe_rate_limit := anthropic_account_io_allowed                   (env does NOT apply)
```

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Keying post-output Anthropic I/O solely on `UsageParams.enabled` / `LOOP_USAGE_CHECK_ENABLED` (would silence post when env=false and Claude still enabled)
- Re-reading raw config JSON or inventing a second models merge for the Claude-enabled check
- Using `primaryProvider==grok` as the skip predicate instead of `is_provider_enabled(Claude)`
- Removing orchestrator `ensure_valid_token` as a required deliverable (out of scope)
- Adding ureq timeouts, merge-resolver host changes, or Grok usage APIs in this task list
- Bare `eprintln!` for the new startup skip notice (must use `ui::emit` / product channel)
- Tests that only assert no crash without verifying zero Anthropic seam calls when Claude is disabled
- Tests that require live Anthropic network or real `~/.claude` credentials
- Manual edits to `tasks/*.json` for status (use task-mgr CLI / task-status tags)
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy --all-targets -- -D warnings`
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() in production code paths
- Sequential and wave paths stay parity-locked for account reactions (exhaustive destructure, shared coordinators)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything global is already embedded in **this prompt file**. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/skip-anthropic-usage-claude-disabled.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need | Command |
| ---- | ------- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` |
| List remaining tasks (debug only) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add follow-up (review spawns) | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| ---- | ------- |
| `tasks/skip-anthropic-usage-claude-disabled-prompt.md` | This prompt (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — tail / append |

**Reading progress** — never Read the whole log:

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

---

## Your Task (every iteration)

1. **Work the task in `## Current Task`** — claimed by the loop. If none eligible, `<promise>BLOCKED</promise>`.
2. **Pull only needed progress context** (tail or grep one task).
3. **Recall** — `task-mgr recall --for-task <TASK-ID>`. Never Read full `CLAUDE.md`; grep sections.
4. **Verify branch** matches `feat/skip-anthropic-usage-claude-disabled`.
5. **Think then implement** (code + tests together).
6. **Scoped quality gate** (below). Fix before commit.
7. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).
8. **Emit** `<task-status><TASK-ID>:done</task-status>`.
9. **Append progress** one block terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

When a task declares `modifiesBehavior: true`:

1. Read the specific callers named in the task description.
2. Decide per-caller: OK / BREAKS (split) / NEEDS_REVIEW.
3. FEAT-001 callers of `UsageParams.enabled`: orchestrator OAuth, iteration + wave `account_usage_gate` — already honor the flag.
4. FEAT-002 callers of `AccountReactionParams`: `iteration.rs`, `wave_scheduler.rs` — must set **post** flag from `is_provider_enabled(Claude)`, not from `usage_params.enabled`.

---

## Quality Checks

### Per-iteration scoped gate

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
# Scope examples:
cargo test --lib loop_engine
cargo test --test reaction_parity
```

**Do NOT** run the entire unscoped workspace suite during regular FEAT iterations — that is REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

If ANY test fails — including pre-existing — REVIEW-001 fixes them (or spawns FIX-xxx if >~12 unrelated).

---

## Key Learnings

- **[5131]** Preflight only probes **enabled** providers — Anthropic usage I/O must mirror that discipline.
- **[4126]** Route seq + wave through shared account reactions at convergence points.
- **[4866]/[5075]** Account-global gates fire **once per wave**, not per slot.
- **[4137]** Keep tests hermetic via disabled usage features / inject seams.
- **[4183]** Wave RateLimit reaction chain is already fully wired — extend, don't re-wire.
- **[4446]** Auth/account failures are **provider-specific** — do not treat Anthropic metering as universal.
- **[4439]** Multi-provider configs need explicit mutual-exclusion documentation (Grok enable ≠ Claude disable).
- **[4955]/[5025]** Centralized preflight; pure validate vs I/O probe separation.

---

## CLAUDE.md Excerpts (loop_engine)

- Account-global reactions (`account_usage_gate`, `react_to_outputs`, `react_to_transient`) fire **exactly once per wave**, never once per rate-limited slot.
- Coordinators use exhaustive param destructure (no `..`) + `#[deprecated]` leaves with `#![deny(deprecated)]` on engine files.
- `preflight_validate_and_probe` probes **only** enabled providers.
- models + routing: `primaryProvider` must be enabled; fallback/routes to disabled providers rejected.
- CONTRACT-LOG-001: product UX via `ui::*`; not bare `eprintln!` for new operator notices (legacy OAuth/usage still use eprintln — do not expand that pattern for the skip line).
- Codex/Grok routing is provider-config based; Claude-disabled means no Claude selection via validation.

---

## Data Flow Contracts

### Pre-iteration enablement

```text
ProjectConfig.models + ProjectConfig.routing
  → model::resolve_models_config(&models, &routing) -> ResolvedModelsConfig
  → ResolvedModelsConfig::is_provider_enabled(Provider::Claude) -> bool
  → usage_params.enabled = LOOP_USAGE_CHECK_ENABLED && that bool
```

Built once in `startup.rs` Step 16. Consumed by:

- `orchestrator.rs`: `if usage_params.enabled { oauth::ensure_valid_token(); }`
- `iteration.rs` / `wave_orchestration.rs`: `if usage_params.enabled { account_usage_gate(...) }`

### Post-output Anthropic I/O

```text
ctx.resolved_models.is_provider_enabled(Provider::Claude)
  → AccountReactionParams.anthropic_account_io_allowed  // NOT usage_params.enabled
```

Construction sites (must stay in lockstep):

- `src/loop_engine/iteration.rs` (~AccountReactionParams { ... })
- `src/loop_engine/wave_scheduler.rs` (~AccountReactionParams { ... })

Production `react_to_outputs` / `react_to_outputs_with_io_seams`:

- when `!anthropic_account_io_allowed` → no `check_and_wait`, no `probe_rate_limit_lifted`; wait uses output parse / `fallback_wait` only (no separate `api_reset_secs` field — usage-API reset path unreachable)
- when Claude on + `usage_enabled=false` → no usage-API leg; early-lift probe still wired

### Forbidden collapse

```text
// WRONG
anthropic_account_io_allowed = usage_params.enabled
// That would make LOOP_USAGE_CHECK_ENABLED=false silence the post early-lift probe
// when Claude is still on (env-independent Anthropic path is the probe)
```

---

## Key Context / Reference

| Path | Role |
| ---- | ---- |
| `src/loop_engine/startup.rs` | Builds `UsageParams` today from env only (~Step 16) |
| `src/loop_engine/orchestrator.rs` | Pre-iter `ensure_valid_token` when enabled |
| `src/loop_engine/reactions/account.rs` | `account_usage_gate`, `react_to_outputs` (always load today) |
| `src/loop_engine/model.rs` | `resolve_models_config`, `is_provider_enabled` |
| `src/loop_engine/oauth.rs` / `usage.rs` | Anthropic OAuth + usage GETs (no timeouts — out of scope to fix) |
| `tests/reaction_parity.rs` | Hermetic parity for account reactions |
| Lean brief | `tasks/skip-anthropic-usage-claude-disabled.md` |

### Grok-only operator recipe (DOC-001)

```sh
task-mgr models enable grok
task-mgr models disable claude
# Set "primaryProvider": "grok" in .task-mgr/config.json
# (no task-mgr models set-primary CLI exists)
```

Sparse `{"providers":{"grok":{"enabled":true}}}` leaves Claude **enabled** — Anthropic pre-check remains on.

### Out of scope (do not implement)

- ureq timeouts on OAuth/usage
- Merge resolver / PRD mutate host selection
- Removing `ensure_valid_token` when usage gate is on
- Per-runner metering when Claude enabled but task was Grok
- Auto-disable Claude when enabling Grok

---

## Review Tasks

| Review | Spawns | Focus |
| ------ | ------ | ----- |
| REFACTOR-001 | REFACTOR-FIX-xxx | DRY, dual-predicate not duplicated messily |
| REVIEW-001 | FIX-xxx | Dual predicate preserved; full suite green; wiring |

Spawn:

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 60,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

---

## Progress log format (append after each task)

```text
## YYYY-MM-DD - <TASK-ID>
- Done: <one line>
- Decisions: <if any>
- Follow-ups: <none | FIX ids>
---
```
