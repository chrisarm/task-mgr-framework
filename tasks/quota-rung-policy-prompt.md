# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Quota buckets, remaining headroom, and capability-rung policy** for **task-mgr**.

## Problem Statement

`parse_oauth_usage_json` folds every utilization window — including `limits[].kind = weekly_scoped` (HUD: Current week (Fable)) with `severity: critical` at 95% used — into one `UsageInfo.percentage = max(used)` and one `reset_at`. The pre-iteration gate then waits whenever that max is ≥ `usage_threshold` (92).

Live HUD: session 24% used (**76% left**), weekly-all 55% (**45% left**), Fable weekly 95% (**5% left**, resets Sep 12). The loop treats frontier 5% left as account 95% used and parks until next week (5h cap, then repeats). Session and **standard** / **cost-efficient** still have headroom. Anthropic’s product: when the Fable weekly bucket is gone, **switch models**.

A second bug: `"You've reached your Fable limit. … switch models with /model."` is not `is_rate_limited`, so the iteration **crashes** and burns auto-block.

Operator constraints (binding):

1. Frontier-low is **not** critical — continue on **standard**.
2. Engine language is **rungs** (`frontier` / `standard` / `cost-efficient` / `cheapest`), never model ids except the ingest adapter.
3. Horizon heuristic (config.json): **wait** if reset ≤ 1h; **stop** if reset > 12h and nothing else can run; **ask** if other rungs work and no downgrade instruction. `--use-other-models-ttl <minutes>` (0 allowed; default 0) is how long ask waits before continuing on working rungs.

**PR-1 (FIX-001, FIX-002) is independently shippable** and must not wait on CONTRACT-001. After PR-1, pin `models set-tier claude frontier` to the **standard** model until FEAT-007.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). If a **Project Verification Skills** section applies to this task, follow that skill after the language gate. Don't add a separate self-critique step; the linters, type-checker, targeted tests, and (when present) the project verification skill catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Folding weekly_scoped / severity=critical / is_active into the account remaining or reset_at
- Storing fable / opus / claude-fable-5 in quota.rs or IterationContext blackout keys
- Treating frontier-low as an account wait or as crash/auto-block
- Soonest-reset among multiple low wait buckets (must be latest)
- Spend stop at remaining-percent floor (must be remainingAmount lte 0)
- Default-low on severity=critical or is_active
- PR-2 skip-wait on unavailable/ask before the rung-blackout channel exists (hot loop)
- Reusing handle_quota_deferral for rung-only exhaustion (stale-abort, learning 3927)
- Collapsing dual Anthropic I/O predicates onto one flag
- Silently ignoring LOOP_USAGE_THRESHOLD (must preflight error)
- Remaining as a 0.08 ratio instead of 0–100 percent
- Live remaining numbers on models show without a remote-gated fetch
- Tests that require live Anthropic network or real ~/.claude credentials
- Manual edits to tasks/*.json for status
- unwrap() in production paths
- Catch-all error handlers that swallow context

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy --all-targets -- -D warnings`
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() in production code paths
- Sequential and wave paths stay parity-locked for account reactions (exhaustive destructure, shared coordinators)
- quota.rs and engine.rs blackout keys contain no model-id literals (frontier/standard/cost-efficient/cheapest only)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Project Verification Skills, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/quota-rung-policy.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need | Command |
| --- | --- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` using the task ID from `## Current Task` |
| List remaining tasks (debug only) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings relevant to a task | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up task (review spawns) | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| --- | --- |
| `tasks/quota-rung-policy-prompt.md` | This prompt file (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log.

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist).

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Work the task in `## Current Task`** — the loop engine already selected and claimed it. If `## Current Task` says there is no eligible task, output `<promise>BLOCKED</promise>` with the reason and stop.
2. **Pull only the progress context you need** — most iterations want just the most recent section.
3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. **Never Read `CLAUDE.md` in full**; grep if needed. Prefer this prompt file.
4. **Verify branch** — `git branch --show-current` is `feat/quota-rung-policy`. Switch if wrong.
5. **Think before coding** — assumptions, edge cases, Data Flow Contracts. For high-effort or modifiesBehavior: one rejected alternative.
6. **Implement** — single task, code and tests in one coherent change.
7. **Run the scoped quality gate**. If a **Project Verification Skills** entry covers this task, Read that SKILL.md after the language gate.
8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `fix:`/`test:`/`refactor:`).
9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`.
10. **Append progress** — ONE post-implementation block, terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

PRD already contains a Consumer Impact Table. Re-read the task's `consumerAnalysis` / this prompt's Semantic Distinctions. `BREAKS` → split via `task-mgr add` rather than shoehorn. Dual-predicate sites must stay dual.

---

## Quality Checks

### Per-iteration scoped gate

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test --lib loop_engine
# narrower: cargo test --lib parse_oauth_usage  (or the module you touched)
```

Do **NOT** run the entire workspace `cargo test` during regular iterations.

**Project verification skill:** if this prompt has a **Project Verification Skills** section, run it after the language gate for covered tasks.

### Final gate at REVIEW-001

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

If ANY test fails — including pre-existing — the milestone fixes them. Below ~12 unrelated failures, just fix them. Above that, spawn `FIX-xxx` via `task-mgr add --stdin --depended-on-by REVIEW-001` and `<promise>BLOCKED</promise>`.

---

## Project Verification Skills

This repo ships a project-level verification skill. Language-level gates are **necessary but not sufficient** for user-facing CLI changes the skill covers. Follow the skill literally — do not invent a second harness, and do not paste the skill body into the progress log.

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** `models-routing` (FEAT-008: `models show` / `set-usage-rule` / `set-tier-fallback`), `loop run --help` (FEAT-006: `--use-other-models-ttl`; harness **refuses** `loop run` / `batch run` — help is the safe probe)

**Per-iteration:** if this task is listed above (or is a FIX / WIRE-FIX spawned from a mapped task), Read that SKILL.md and the matching `features/*.md` and drive that recipe after the scoped language gate.

**REVIEW-001:** drive every listed feature this PRD touched.

**Blocked skill:** emit `<promise>BLOCKED</promise>` with the unmet precondition. Do not skip the drive and mark the task done.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

- Parse results not reaching `check_and_wait`
- Config field read but not passed through `AccountUsageGateParams` (exhaustive destructure)
- New clap flag on `loop run` but not `batch run` (or not the deprecated flat shim)
- `evaluate_quota` unused; old max-used fold still in the gate
- Rung blackout collapsed into `provider_blackouts`
- Wrong JSON key (`remainingMinRatio` vs `remainingMinPercent`; snake vs camelCase)

---

## Contract Tasks

`CONTRACT-001` is **design-only**. Do not write production code. Record the full contract in the progress log under `## CONTRACT-001`. Downstream FEAT-003–FEAT-008 implement against it. FIX-001 and FIX-002 do **not** depend on it (PR-1 ship slice).

---

## Review Tasks

Spawn follow-ups with `task-mgr add --stdin --depended-on-by REVIEW-001`. Commit `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then `<task-status><REVIEW-ID>:done</task-status>`.

---

## Progress Report Format

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block.

---

## Stop and Blocked Conditions

Before `<promise>COMPLETE</promise>`: all stories `passes: true`, no new unfixed review tasks, REVIEW-001 passed.

If blocked: document in progress, `task-mgr add` a CLARIFY if needed, `<promise>BLOCKED</promise>`.

---

## Key Learnings (from task-mgr recall)

- **[5297]** Dual predicates for Anthropic I/O: pre = env∧Claude, post = Claude only
- **[5298]** Collapsing dual predicates breaks kill-switch or RateLimit recovery
- **[5301]** Post path: usage load ANDs env; probe is Claude-only — do not document otherwise
- **[5075]** Account-global usage gate fires once per wave, not per slot
- **[4866]** Account-global gates fire once-per-wave, read-only
- **[4138]** Converging account_usage_gate across seq+wave eliminates the strand-bug
- **[5090]** parse_reset_from_output fallback handles malformed quota messages
- **[3927]** Quota deferral must run before stale-abort; do not classify empty selection as stale when it is quota-deferred

---

## CLAUDE.md Excerpts (only what applies to this PRD)

- Account-global reactions (`account_usage_gate`, `react_to_outputs`) fire **exactly once per wave**.
- Dual predicate: `usage_params.enabled = LOOP_USAGE_CHECK_ENABLED && claude_provider_enabled`; post `anthropic_account_io_allowed = claude_provider_enabled` only; usage-API leg still ANDs env; early-lift probe is Claude-only.
- `provider_blackouts` is a SEPARATE ephemeral channel from `runner_overrides`. This PRD adds a **rung** blackout channel that must also stay separate from both.
- `tier_of` is config exact-match after stripping `[1m]`. Substring matching is dead for tier classification.
- `ui::*` for product UX; `tracing` for diagnostics. Operator wait banners are stderr, exact bytes.
- `LOOP_USAGE_CHECK_ENABLED` is not a Claude kill-switch.

---

## Data Flow Contracts

Use these exactly — do not guess key types.

**OAuth JSON object window** (`serde_json::Value` string keys):

```rust
json.get("five_hour")?.get("utilization")?.as_f64()
// remaining = (100.0 - util).clamp(0.0, 100.0)
json.get("seven_day")?.get("resets_at")?.as_str()
```

**OAuth `limits[]`:**

```rust
let limits = json.get("limits")?.as_array()?;
let kind = limit.get("kind")?.as_str(); // "session" | "weekly_all" | "weekly_scoped"
let percent = limit.get("percent").and_then(|v| v.as_f64()).or_else(|| {
    limit.get("percent").and_then(|v| v.as_u64()).map(|u| u as f64)
});
let name = limit.pointer("/scope/model/display_name").and_then(|v| v.as_str());
```

Account fold (FIX-001) uses only `kind` `session`/`weekly_all` plus named `five_hour`/`seven_day`. Scoped `percent` must not enter account remaining.

**Project config** (camelCase JSON → structs):

```rust
config["usagePolicy"]["remainingMinPercent"]
config["usagePolicy"]["waitIfResetWithinMinutes"]
config["usagePolicy"]["stopIfResetBeyondHours"]
config["usagePolicy"]["askTtlMinutes"]
config["usagePolicy"]["rules"][i]["onLow"]  // wait|unavailable|stop|ask|ignore
config["routing"]["tierFallback"]["maxDifficulty"]  // "low"|"medium"|"high" or absent
```

**QuotaDecision.unavailable:** `Vec<(Provider, CapabilityTier)>`

```rust
decision.unavailable.iter().any(|(p, t)| *p == Provider::Claude && *t == CapabilityTier::Frontier)
```

**PlanContext.unavailable_rungs:** `&HashSet<(Provider, CapabilityTier)>`

```rust
plan.unavailable_rungs.contains(&(Provider::Claude, CapabilityTier::Frontier))
```

**Remaining-min precedence:** `LOOP_USAGE_REMAINING_MIN` (env u8) overrides `usagePolicy.remainingMinPercent` overrides default `8`.

**HUD label → rung (ingest only):** Fable→Frontier, Opus→Standard, Sonnet→CostEfficient, Haiku→Cheapest; else match `model_for` strings. Decision layer never stores those display names.

---

## Feature-Specific Checks

- Live-shaped fixture (no network): session util 24, seven_day 55, limits weekly_scoped Fable 95 critical. After FIX-001, account remaining 45, no wait.
- Inverse: weekly_all 100 still waits weekly.
- `grep -n 'fable\\|claude-fable-5' src/loop_engine/quota.rs src/loop_engine/engine.rs` → 0 after FEAT-003/FEAT-007.
- PR-1 operator recipe until FEAT-007: `task-mgr models set-tier claude frontier` to the **standard** rung model.
- Sequential + wave: both `account_usage_gate` and `react_to_outputs` coordinators; exhaustive destructure; `tests/reaction_parity.rs` for I/O seams.
- `--use-other-models-ttl` on both `loop run` and `batch run` (and keep deprecated flat shims compiling).

---

## Semantic Distinctions

| Code path | Current | Required |
| --- | --- | --- |
| Account-binding remaining low | Wait | Wait / stop by horizon |
| Rung-scoped remaining low | Same wait as account | unavailable / ask / continue on standard — never account wait |
| Spend remaining low | Stop on CLI text | Stop only at amount 0 unless rule says otherwise |
| ask timeout | n/a | Continue on working rungs |
| stop (far reset, nothing runnable) | n/a | Halt this PRD; chain continues if rung-scoped |
| Provider blackout FEAT-008 | Spillover / defer | Unchanged |
| Review class forces frontier | Always frontier request | Request unchanged; clamp/ask after |

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** — never commit failing code
- **Read before writing**
- **Minimal changes** — only implement what's required
- **PR-1 first** — FIX-001 and FIX-002 must not grow CONTRACT/FEAT-003 scope
