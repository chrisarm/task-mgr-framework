# PRD: Provider-Aware Routing and Fallback Between Claude and Grok

**Type**: Enhancement (with two bug fixes first)
**Priority**: P1 (High) — loops idle for hours when Claude is limited, and Grok's own limit is treated as a crash
**Author**: Remy for Chris Armstrong (from `task-mgr-routing-plan.md`, verified on arcomp2 2026-10-08)
**Created**: 2026-10-08
**Status**: Approved 2026-10-08 2:56 PM PT. Phase 1 loop covers P1c and P1e only (`tasks/model-routing-p1-grok-safety.json`)

---

## 1. Overview

### Problem Statement

task-mgr already has the pieces for two backends: the `LlmRunner` trait (`RunnerKind {Claude, Grok, Codex}`), a headless `GrokRunner`, provider-first `models` + `routing` config (tiers, `byIdPrefix`, `taskClasses` with `providerPreference` / `byDifficulty`, `spillover`), a per-provider `fallback`, and a quota-blackout reroute. They do not add up to "keep working on Grok when Claude is out" or "start each task on the model most likely to do it well":

1. **The pre-iteration quota gate only knows Claude.** `run_account_quota_gate_inner` (`reactions/account.rs:1745`) runs before task selection. When the Claude session bucket is below its floor and resets within 12h it **sleeps** (capped at 5h), even when Grok is enabled and could run the next task. The Grok spillover path is only reached from a *post-output* Claude limit message, which the pre-gate pre-empts. On 2026-10-08 the mw loops sat about 1h45m this way.
2. **A blackout is recorded against the primary provider, not the one that hit the limit** (`account.rs:756` uses `params.primary_provider`), and Anthropic usage I/O can run for a Grok limit.
3. **Grok's own limit is a crash.** Grok prints `Error: You’ve hit the rate limit for your plan. Upgrade your account or try again later.` on stderr (curly apostrophe, no reset time; captured in `restaurant_agent_ex/.task-mgr/logs/2df9cb39-…-iter{1,2}-grok-stderr.log`, 2026-07-15). `is_rate_limited` (`detection.rs:151`) matches none of it, and `GrokRunner::spawn` only sniffs stderr for auth and 5xx (`runner.rs:972-1005`). The iteration becomes `Crash`, burns crash budget, and can auto-block tasks.
4. **Grok session cleanup can delete an interactive session.** `GrokRunner::spawn` finds "its" session by diffing `~/.grok/sessions/<urlencoded cwd>/` before and after the run and picks the newest by mtime when several appear (`runner.rs:806-823, 952-970, 471`). An interactive Grok session started in the same cwd during a loop spawn can be picked and removed by `cleanup_session` (`runner.rs:1023`). This exposure is real: WS7–9 run interactive Grok in the `restaurant_agent_ex` root, where Grok-primary loops have spawned (`prompt_history.jsonl` has loop prompts through 2026-10-01).
5. **Forced routes ignore blackouts.** `first_enabled_provider` (`model.rs:1271`) and `byIdPrefix` do not skip a blacked-out provider.
6. **A retry after an interrupted attempt starts from a dirty tree.** Edits from an attempt that ended in a limit stay in the worktree and get swept into the next attempt's wrapper commit.
7. **No per-attempt record of which provider ran what and how it went**, so routing cannot be tuned from data.

### Background

- arcomp2 runs `task-mgr 0.3.4` built from `0e673bd` (this branch's base). `grok 1.0.50` supports every flag task-mgr passes, plus `-s/--session-id <UUID>` ("use a specific session UUID for a new conversation").
- mw_integrations is Claude-only. restaurant_agent_ex is Grok-primary with Claude disabled. This repo is Grok-primary with Claude disabled.
- Unmerged branch `feat/cli-error-gated-detection` (prefix `fe92ec5b`, REVIEW-001 done) rewrites `detection.rs` (`OutputSignals`, classifiers gated on CLI-error provenance) and the Grok stderr block in `runner.rs`. It deliberately does not scan Grok stderr for limits. This PRD branches from `main`; the P1c/P1e diffs stay small and local so the later merge is mechanical.

### Decisions (Chris, 2026-10-08 2:56 PM PT)

| # | Decision | Effect in this PRD |
|---|---|---|
| D1 | restaurant_agent_ex routes between Claude and Grok, not Grok-only | Rollout step R2 after P1 lands. No config change in this branch |
| D2 | Reviews and hard tasks **wait for Claude**, never fall back | `spillover.maxDifficulty: "medium"`; review class never spills (no `includeReview` knob) |
| D3 | Easy and medium tasks may fall back to Grok | Blackout reroute allowed for `low` and `medium` |
| D4 | Easy tasks **start on Grok** now, to collect comparison data | `implementation.byDifficulty.low = ["grok","claude"]`; medium/high Claude-first |
| D5 | An interrupted attempt's edits are **reverted**, not continued | P2 restores exactly the files that attempt touched; no `continue` mode |
| D6 | No per-task `model` pins | Generators already forbid them; no `includeForced` knob. A legacy pin keeps its provider |

---

## 2. Goals

### Primary Goals

- [ ] (P1c) A Grok plan rate limit becomes `IterationOutcome::RateLimit`, never `Crash`, on the sequential and wave paths.
- [ ] (P1e) A Grok run deletes only the session directory it created, identified by the UUID task-mgr passed via `--session-id`.
- [ ] (P1b) The blackout names the provider that actually hit the limit; Anthropic usage I/O runs only for a Claude limit.
- [ ] (P1a) When Claude is below its floor and spill-eligible work exists, the loop reroutes that work instead of sleeping.
- [ ] (P1d) Preference lists and `byIdPrefix` routes skip a blacked-out provider, except review-class and high-difficulty tasks, which wait (D2).
- [ ] (P2) A retried task starts from a tree where the interrupted attempt's edits are reverted, and operator edits are untouched (D5).
- [ ] (P4a) Every attempt appends one line to `.task-mgr/logs/routing-events.jsonl` with provider, model, route reason, and outcome.
- [ ] (P3) A PRD JSON may carry a `routing` / `models` overlay merged over project config at loop start.
- [ ] (P4b) `task-mgr stats --by-provider` reports first-attempt success, attempts per completed task, duration, rework, and fallback count.

### Success Metrics

- Replaying the 2026-07-15 Grok stderr line yields `RateLimit`, 0 crash-counter increments, and no Anthropic usage call.
- A test that creates an extra "interactive" session directory during a Grok spawn finds it intact after cleanup.
- With Claude below floor and a low/medium task ready, the loop starts that task on Grok within one iteration and never calls the wait.
- After two weeks of D4 routing, `stats --by-provider` gives first-attempt success per (provider, difficulty) for at least 30 attempts each.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Idempotent, accurate accounting first.** A limit is a limit, not a crash. A blackout names the right provider. A retry starts clean. Cleanup removes exactly one known directory and is safe to repeat.
- **Typed signals, not sentinels.** A Grok stderr limit travels as a typed error/outcome (mirroring `TaskMgrError::TransientBackend`), never as a marker appended to `output`.
- **Narrow patterns.** New limit phrasing is contiguous and specific (`rate limit for your plan`). Existing patterns are not narrowed (learning 5364).
- **Sequential and wave parity.** Every reaction change lands in the shared inner function and is covered by `tests/reaction_parity.rs`.
- **Config-driven.** No model strings in task JSON. New behavior is off unless config opts in, except the two bug fixes (P1c, P1e), which are unconditional.
- **D2 is a hard rule.** Review-class (`CODE-REVIEW-*`, `REVIEW-*`, `MILESTONE-*`) and `high` tasks never move off Claude because of a blackout.

### Performance Requirements

- Best effort. The extra stderr scan runs only on the existing stderr buffer after exit. P1e removes two directory listings per spawn.

### Style Requirements

- Follow `loop_engine` patterns: pure classifiers in `detection.rs`, I/O-free `decide_*` helpers, exhaustive destructures in coordinators.
- Product UX through `ui::*`; diagnostics through `tracing` with no output text on events.
- Comments that explain a provider workaround keep the `WORKAROUND(<tag>)` form (learning 3204).

### Known Edge Cases

| # | Edge Case | Why It Matters | Expected Behavior |
|---|---|---|---|
| E1 | Grok stderr is exactly the 2026-07-15 line (U+2019 apostrophe), exit ≠ 0 | The real limit | `RateLimit`; no crash increment |
| E2 | Same line with a straight apostrophe or different case | CLI copy drift | `RateLimit` |
| E3 | Agent prose "the client hit the rate limit in tests" | False-positive risk on Claude stdout | Not `RateLimit` (pattern is `rate limit for your plan`) |
| E4 | Grok stderr has both an auth phrase within the fast-fail window and the limit line | Precedence | Auth failure wins (existing) |
| E5 | Grok stderr is a 502 page with no limit line | Existing transient path | `TransientBackend` (unchanged) |
| E6 | Grok limit with exit 0 and empty stdout | Exit status of a Grok limit is unconfirmed | Handled per ANALYSIS-001 finding; if exit 0 is possible, the scan also runs when stdout is empty |
| E7 | Grok exits 0 with `<completed>X</completed>` in stdout and a limit line on stderr | Completion must not be lost | Completion path wins; not `RateLimit` |
| E8 | An interactive Grok session directory appears in the same cwd during a loop spawn | The cleanup collision | Only the `--session-id` directory is removed |
| E9 | Grok dies before creating its session directory | Cleanup of a missing dir | `Ok(())`, no warning |
| E10 | `HOME` unset | Cleanup root unknown | No cleanup, no panic |
| E11 | Claude below floor; only review/high/pinned work remains | D2 | Existing Wait/Stop/deferral |
| E12 | Claude below floor; low task ready; Grok blacked out too | Both down | Deferral wait (existing) |
| E13 | Grok limit with no reset time | Blackout length | `routing.spillover.blackoutFallbackSecs` |
| E14 | Attempt 1 edits `a.rs`, then RateLimits; an operator file was dirty before attempt 1 | D5 | `a.rs` restored to its pre-attempt content; operator file untouched |
| E15 | Legacy task with an explicit `model` pin; its provider is blacked out | D6 | Task waits on its pinned provider |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **`TaskMgrError`** (`src/error.rs`): a typed rate-limit variant for runner-detected limits (P1c), carrying the `RunnerKind`. Mapped to `IterationOutcome::RateLimit` in `iteration.rs` and `slot.rs`, next to the existing `TransientBackend` arms.
- **`GrokRunner::spawn`** (`runner.rs`): passes `--session-id <uuid>`; `RunnerResult.session_id` is that uuid (P1e).
- **`OutputReactionItem`** (`account.rs:239`): gains `provider` (P1b).
- **`RunAccountQuotaGateParams` / `QuotaPreflightParams`**: gain blackout state and spill eligibility, changed together (P1a).
- **`ExecutionPlan`** (`model.rs:679`): gains `route: RouteReason` (P4a).
- **`PrdFile`** (`init/parse.rs:290`): gains optional `routing` / `models` (P3).

### Modularity & Coupling Targets

- No new subsystem. Every phase extends an existing seam.
- No `CONTRACT-xxx` task for P1c/P1e: each is one local change with one consumer chain.

---

## 2.7. Production Readiness

| Concern | Answer |
|---|---|
| Rollout | Code first, config second. The installed binary is not replaced by the loop; Chris rebuilds after merge. Then R1 (this repo and mw_integrations opt in) and R2 (restaurant_agent_ex switches to dual routing, D1) |
| Rollback | Revert the commit; no migration until P4b. Config opt-ins are removable |
| Observability | `RateLimit` from Grok emits the existing "Rate limit detected" UX line naming Grok. P4a writes `routing-events.jsonl` |
| Proof | P1c: E1–E7 tests fail before, pass after. P1e: E8–E10 tests with a mock grok script. P1a/P1b: FakeRunner integration test (Claude prints a session limit, Grok completes; no wait called) |
| Safety | P1e removes the only path where a loop can delete an interactive Grok session |

---

## 3. User Stories

### US-001: A Grok limit does not burn crash budget
As an operator, when Grok hits its plan limit, the loop treats it as a limit (blackout or wait), not a crash, so tasks are not auto-blocked.

### US-002: Loops never delete my interactive Grok session
As Chris running Grok interactively in a project root, a loop spawning Grok in the same directory deletes only its own session.

### US-003: Work continues on Grok while Claude is out
As an operator, when Claude is below its floor, low and medium tasks run on Grok and reviews and hard tasks wait for Claude.

### US-004: Easy tasks start on Grok so we can compare
As Chris, low-difficulty implementation starts on Grok and every attempt is logged, so I can decide routing from data.

### US-005: A retry starts clean
As an operator, an attempt cut off by a limit leaves no half-edits for the next attempt or the wrapper commit.

---

## 4. Functional Requirements (phased, in build order)

Order: **P1c → P1e → P1b → P1a → P1d → P2 → P4a → P3 → P4b.** P1c and P1e are the first loop. Each later phase gets its own task JSON generated from this PRD when the previous one lands.

### FR-P1c: Recognize Grok's plan rate limit (first loop)
- Add the contiguous, case-insensitive phrase `rate limit for your plan` to `is_rate_limited` (`detection.rs:151`). It has no apostrophe, so both apostrophe forms match. Do not change other patterns.
- In `GrokRunner::spawn`'s stderr classification (`runner.rs:972-1005`), after the auth sniff, return a typed rate-limit error when `is_rate_limited(&stderr)` matches. The exit-status gate follows ANALYSIS-001's finding on how Grok exits on a limit.
- Map that error to `IterationOutcome::RateLimit` in `iteration.rs` (beside the `TransientBackend` arm, ~`:699`) and `slot.rs` (~`:310`).
- Fixture: `tests/fixtures/grok_stderr_plan_rate_limit.txt`, byte-exact.

### FR-P1e: Deterministic Grok session cleanup (first loop)
- Generate `Uuid::new_v4()` per spawn, pass `--session-id <uuid>`, and return it as `RunnerResult.session_id`, mirroring Claude (`runner.rs:601-603`).
- Delete the before/after directory snapshot and `pick_newest_by_mtime`. `cleanup_session` is unchanged (deletes `<encoded cwd>/<uuid>`, `NotFound` is `Ok`).

### FR-P1b: Blackout the provider that hit the limit
- `OutputReactionItem.provider` from `effective_runner`; `react_to_outputs_inner` records that provider (`account.rs:756`).
- Anthropic usage load and the Claude probe run only when the limited provider is Claude (`account.rs:618, 642, 654`).
- A Grok `RateLimit` with no reset uses `blackoutFallbackSecs`.

### FR-P1a: Provider-aware pre-iteration gate
- In `run_account_quota_gate_inner`, when the Claude decision is an account-binding Wait/Stop **and** another provider is enabled **and** spillover is configured **and** `compute_remaining_work_snapshot` (with the blackout applied) finds a runnable non-Claude task: record `provider_blackouts.record(Claude, now, reset_secs)` and proceed.
- Otherwise behavior is unchanged, including `handle_quota_deferral` when nothing can move.
- Change `QuotaPreflightParams` and `RunAccountQuotaGateParams` together; extend `tests/reaction_parity.rs`.

### FR-P1d: Routes honor blackouts, D2 holds
- `first_enabled_provider` (`model.rs:1271`) skips blacked-out providers, so `providerPreference` / `byDifficulty` lists are ordered fallbacks.
- `byIdPrefix` falls to `providers.<p>.fallback` when its provider is blacked out.
- Review-class and `high` tasks never move (D2). Explicit pins never move (D6). No new knobs.
- The blackout path still never writes `runner_overrides` (keep `blackout_reroute_leaves_runner_overrides_untouched`).

### FR-P2: Revert an interrupted attempt (D5)
- Before each spawn, record the dirty-path baseline (reuse `git_reconcile::capture_status_paths`).
- When the attempt ends in `RateLimit`, `TransientBackend`, timeout, or crash without completion, restore exactly the paths the attempt touched (current − baseline) to their pre-attempt state: tracked files via `git restore --source=HEAD --staged --worktree -- <paths>`, new untracked files removed. Paths dirty before the attempt are never touched.
- Log the reverted paths to progress and `routing-events.jsonl`.
- No prompt section for interrupted work (nothing carries over).

### FR-P4a: Per-attempt routing events
- `ExecutionPlan.route: RouteReason` (`Explicit | ByIdPrefix | TaskClass | BlackoutReroute | Anchor | OverflowPivot`).
- One JSON line per attempt in `.task-mgr/logs/routing-events.jsonl`: ts, run_id, iteration, task_id, task_class, difficulty, provider, model, effort, tier, route, fallback_from, blackouts_active, outcome, duration_s, files_changed, self_committed, reverted_paths count.

### FR-P3: Per-PRD routing overlay
- Optional top-level `routing` / `models` objects in the task JSON, deep-merged over project config at loop start (`project_config.rs:337-356`), validated by `validate_models_config` with the PRD path in errors. No DB migration.

### FR-P4b: Durable stats
- Migration v22: `run_tasks` gains effort, tier, route, outcome, fallback_from, stamped on every attempt (`iteration_pipeline.rs:505`).
- `task-mgr stats --by-provider` grouped by (provider, class, difficulty).

### Target routing config (applied in rollout, not in code)

```json
{
  "models": {
    "primaryProvider": "claude",
    "providers": {
      "claude": { "enabled": true, "fallback": "grok" },
      "grok":   { "enabled": true, "fallback": "claude" }
    }
  },
  "routing": {
    "taskClasses": {
      "implementation": { "byDifficulty": { "low": ["grok", "claude"],
                                            "medium": ["claude", "grok"],
                                            "high": ["claude"] } }
    },
    "spillover": { "maxDifficulty": "medium" }
  }
}
```

Review-class IDs keep the built-in frontier force on Claude and do not spill (D2).

---

## 5. Non-Goals (Out of Scope)

- Replacing or reinstalling the task-mgr binary from a loop. Chris rebuilds after merge.
- Changing any project's `.task-mgr/config.json` in this branch (rollout R1/R2 are separate, operator-run steps).
- `spillover.includeReview` and `includeForced` knobs (D2, D6 make them unnecessary).
- A `continue` mode for interrupted attempts (D5).
- Grok usage API polling (Grok has none; limits come from stderr).
- Importing `taskType` into SQLite for routing.

## 5.5. Low-Value / High-Effort Areas

| Area | Why low value | Effort | Action |
|---|---|---|---|
| Parsing a reset time out of Grok's limit message | The message has none | Low | Cut; use `blackoutFallbackSecs` |
| Per-session token/cost via `grok usage` in P4a | Nice for cost tables, not needed to compare success rates | Med | Defer |
| Automatic routing tuning from stats | Needs data first | High | Defer |

---

## 6. Technical Considerations

### Affected Components (by phase)

| Phase | Files |
|---|---|
| P1c | `src/loop_engine/detection.rs`, `src/loop_engine/runner.rs`, `src/error.rs`, `src/loop_engine/iteration.rs`, `src/loop_engine/slot.rs`, `tests/fixtures/grok_stderr_plan_rate_limit.txt` |
| P1e | `src/loop_engine/runner.rs` |
| P1b | `src/loop_engine/reactions/account.rs`, `iteration.rs`, `wave_scheduler.rs` |
| P1a | `reactions/account.rs`, `iteration.rs:131`, `wave_orchestration.rs:91`, `engine.rs`, `tests/reaction_parity.rs` |
| P1d | `model.rs:1192-1337`, `pre_spawn.rs:315` |
| P2 | `orchestrator.rs:340-460`, `slot.rs`, `git_reconcile.rs` |
| P4a | `model.rs:679, 1086-1144`, `iteration_pipeline.rs`, `observability.rs` |
| P3 | `init/parse.rs:290`, `startup.rs:535/772`, `orchestrator.rs:133`, `project_config.rs` |
| P4b | `db/migrations/v22.rs`, `iteration_pipeline.rs:505`, `commands/stats.rs` |

### Approaches & Tradeoffs (P1c)

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| A. Append the stderr line to `output` so `analyze_output` sees it | Tiny | Sentinel in output; collides with the gated-detection branch's provenance rule | Rejected |
| B. **Typed error from `GrokRunner::spawn`, mapped beside `TransientBackend`** | Matches the existing Grok auth/5xx pattern; no change to `analyze_output` | One new error variant and two match arms | **Preferred** |
| C. Broaden `is_rate_limited` with bare `rate limit` | One line | False positives on Claude prose | Rejected |

### Approaches & Tradeoffs (P1e)

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| A. Keep the dir diff, add a "created after spawn start" filter | No CLI flag | Still a heuristic; still races with interactive sessions | Rejected |
| B. **`--session-id <uuid>`** | Exact; mirrors Claude; deletes code | Depends on grok ≥ the version with `-s` (1.0.50 has it) | **Preferred** |

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| Grok exits 0 on a limit, so an `exit != 0` gate misses it | Med | Unknown | ANALYSIS-001 settles it; E6 |
| Merge conflict with `feat/cli-error-gated-detection` in `runner.rs` / `detection.rs` | Low | High | Keep hunks small; P1c adds a pattern and one branch, P1e touches only the session snapshot |
| Grok quality on low tasks (D4) | Med | Med | P4a data; byDifficulty is config |
| Reverting an attempt removes useful partial work (D5) | Low | Med | Chris chose revert; logged paths make it auditable |
| Parity tests are dense | Low | High | Change param structs together; compile errors are the audit |

### Security Considerations

- No new secrets or network calls. Tracing events carry no output or stderr text.

### Consumers of Changed Behavior (P1c/P1e)

| File:Line | Usage | Impact | Mitigation |
|---|---|---|---|
| `detection.rs:151` `is_rate_limited` | Claude stdout, recovery probe, Grok stderr (new) | Widened by one narrow phrase | E3 discriminator |
| `iteration.rs` / `slot.rs` runner error arms | sequential / wave | New arm → `RateLimit` | Mirror `TransientBackend` arm |
| `runner.rs:1853` dispatch cleanup | uses `RunnerResult.session_id` | Now always `Some(uuid)` for Grok | Missing dir is `Ok` |
| `runner.rs:2632` dir-diff test | pins the heuristic | Replaced | New E8 test |

### Documentation

| Doc | Action |
|---|---|
| `src/loop_engine/CLAUDE.md` | Grok limit → RateLimit; Grok session cleanup via `--session-id` |
| `CLAUDE.md` (models section) | After P1a/P1d: dual-provider config and D2 semantics |

---

## 7. Open Questions

- [ ] How does Grok exit on a plan limit (code, stdout)? ANALYSIS-001.
- [ ] Before P1a: should the 2%/1% Claude floors stay as the trigger, or should spillover start at a higher floor so Claude keeps headroom for reviews?

---

## Appendix

### Rollout

- R0: P1c + P1e merged; Chris rebuilds and installs.
- R1: this repo and mw_integrations: enable Grok with fallback after P1a–P1d land.
- R2: restaurant_agent_ex: re-enable Claude, apply the target config (D1, D4). Launch Grok-routed loops from worktrees until P1e is installed.

### Related

- Plan: `task-mgr-routing-plan.md` (Remy, 2026-10-08), §7.2 arcomp2 verification
- Branch: `feat/cli-error-gated-detection` (prefix `fe92ec5b`)
- Learnings: 5364 (keep widened patterns), 5298 (usage-gate vs probe split), 3754 (cleanup is a trait method), 4696 (cleanup idempotence test flake), 3204 (document provider workarounds)
