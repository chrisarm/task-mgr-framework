# PRD: Provider-Aware Routing and Fallback Between Claude and Grok

**Type**: Enhancement (with two bug fixes first)
**Priority**: P1 (High) — loops idle for hours when Claude is limited, and Grok's own limit is treated as a crash
**Author**: Remy for Chris Armstrong (from `task-mgr-routing-plan.md`, verified on arcomp2 2026-10-08)
**Created**: 2026-10-08
**Status**: Approved 2026-10-08 2:56 PM PT. Phase 1 loop covers P1c and P1e only (`tasks/model-routing-p1-grok-safety.json`). Phase P6 (per-project class routes and a sensitive-path floor) is specified in [`tasks/planned-model-routing-guide.md`](planned-model-routing-guide.md) and is not part of that loop. P6.1, P6.2, P6.4, P1a, P1b, P1d, P2, and the jsonl stats phase are later work. P6.3, P6.5, P3, and v22 are not scheduled (Pins).

---

## 1. Overview

### Problem Statement

task-mgr already has the pieces for two backends: the `LlmRunner` trait (`RunnerKind {Claude, Grok, Codex}`), a headless `GrokRunner`, provider-first `models` + `routing` config (tiers, `byIdPrefix`, `taskClasses` with `providerPreference` / `byDifficulty`, `spillover`), a per-provider `fallback`, and a quota-blackout reroute. They do not add up to "keep working on Grok when Claude is out" or "start each task on the model most likely to do it well":

1. **The pre-iteration quota gate only knows Claude.** `run_account_quota_gate_inner` (`reactions/account.rs:1745`) runs before task selection. When the Claude session bucket is below its floor and resets within 12h it **sleeps** (capped at 5h), even when Grok is enabled and could run the next task. The Grok spillover path is only reached from a *post-output* Claude limit message, which the pre-gate pre-empts. On 2026-10-08 the mw loops sat about 1h45m this way.
2. **A blackout is recorded against the primary provider, not the one that hit the limit** (`account.rs:756` uses `params.primary_provider`), and Anthropic usage I/O can run for a Grok limit.
3. **Grok's own limit is still not a `RateLimit` outcome.** Grok prints `Error: You’ve hit the rate limit for your plan. Upgrade your account or try again later.` on stderr (curly apostrophe, no reset time; captured in `restaurant_agent_ex/.task-mgr/logs/2df9cb39-…-iter{1,2}-grok-stderr.log`, 2026-07-15). FIX-001 put that phrase in `is_rate_limited` (`detection.rs:254`), not in `analyze_output` (`detection.rs:149`). This loop does not edit `detection.rs` and does not call `is_rate_limited` from the Grok sniff. `GrokRunner::spawn` still sniffs stderr only for auth and 5xx, and only when `exit_code != 0` (`runner.rs:1080-1108`). A non-zero exit stays `Crash`. Exit 0 with empty stdout stays `Empty`. Both burn crash budget.
4. **Grok session cleanup can delete an interactive session.** `GrokRunner::spawn` finds "its" session by diffing `~/.grok/sessions/<urlencoded cwd>/` before and after the run and picks the newest by mtime when several appear (`runner.rs:1044-1061`, `pick_newest_by_mtime` at `:502`). An interactive Grok session started in the same cwd during a loop spawn can be picked and removed by `cleanup_session` (`runner.rs:1130`). This exposure is real: WS7–9 run interactive Grok in the `restaurant_agent_ex` root, where Grok-primary loops have spawned (`prompt_history.jsonl` has loop prompts through 2026-10-01).
5. **Forced routes ignore blackouts.** `first_enabled_provider` (`model.rs:1271`) and `byIdPrefix` do not skip a blacked-out provider.
6. **A retry after an interrupted attempt starts from a dirty tree.** Edits from an attempt that ended in a limit stay in the worktree and get swept into the next attempt's wrapper commit.
7. **No per-attempt record of which provider ran what and how it went**, so routing cannot be tuned from data.

### Background

- arcomp2 runs `task-mgr 0.3.4` built from `0e673bd` (not rebuilt; this branch now includes main `eded39d`). `grok 1.0.50` supports every flag task-mgr passes, plus `-s/--session-id <UUID>` ("use a specific session UUID for a new conversation").
- mw_integrations is Claude-only. restaurant_agent_ex is Grok-primary with Claude disabled. This repo is Grok-primary with Claude disabled.
- Branch `feat/cli-error-gated-detection` (prefix `fe92ec5b`) landed on `main` as `eded39d` (2026-10-09) and is merged into this branch. It rewrote `detection.rs` (`OutputSignals`, classifiers gated on CLI-error provenance) and the Grok stderr block in `runner.rs`. It deliberately does not scan Grok stderr for limits. P1c/P1e build on top of it with small, local hunks.

### Decisions (Chris, 2026-10-08 2:56 PM PT)

| # | Decision | Effect in this PRD |
|---|---|---|
| D1 | restaurant_agent_ex routes between Claude and Grok, not Grok-only | Rollout step R2 after P1 lands. No config change in this branch |
| D2 | Reviews and hard tasks **wait**, never fall back | `spillover.maxDifficulty: "medium"`. Review and `high` do not spill onto another provider because of a blackout. Factory `tierFallback` (`includeReview: true`) may still walk a review from frontier down to a cheaper Claude rung. The P6 standard floor does not stop that walk. `review.differentProvider` defaults off. On the builtin ladders only Claude has a frontier model, so with primary Claude the flag defers every review until `models set-tier` pins a non-null frontier model on another enabled provider. See the planned guide. |
| D3 | Easy and medium tasks may fall back to Grok | Blackout reroute allowed for `low` and `medium` |
| D4 | Easy tasks **start on Grok** now, to collect comparison data | `implementation.byDifficulty.low = ["grok","claude"]`; medium/high Claude-first |
| D5 | An interrupted attempt's edits are **reverted**, not continued | P2 restores exactly the files that attempt touched; no `continue` mode |
| D6 | No per-task `model` pins | Generators already forbid them. P6.2 rejects a `model` field on `task-mgr add`, `loop init`, and `batch init`. A legacy pin keeps its provider and skips the P6 raise and `differentProvider`. Factory `tierFallback.includeForced` is false and does not clamp that pin. When `includeForced` is true, the down-walker may move a matching pin, but not below the sensitive-path floor. |

---

## 2. Goals

### Primary Goals

- [ ] (P1c) A Grok plan rate limit becomes `IterationOutcome::RateLimit`, never `Crash`, on the sequential and wave paths.
- [ ] (P1e) A Grok run deletes only the session directory it created, identified by the UUID task-mgr passed via `--session-id`.
- [ ] (P1b) The blackout names the provider that actually hit the limit; Anthropic usage I/O runs only for a Claude limit.
- [ ] (P1a) When Claude is below its floor and spill-eligible work exists, the loop reroutes that work instead of sleeping.
- [ ] (P1d) Preference lists and `byIdPrefix` routes skip a blacked-out provider, except review-class and high-difficulty tasks, which wait (D2).
- [ ] (P6) A project sets provider-by-difficulty and a sensitive-path floor in config, without a per-task profile. Spec: [`tasks/planned-model-routing-guide.md`](planned-model-routing-guide.md). P6.1, P6.2, and P6.4 are later work. P6.3 and P6.5 are not scheduled.
- [ ] (P2) A retried task starts from a tree where the interrupted attempt's edits are reverted, and operator edits are untouched (D5). Later work. Not this loop.
- [ ] (P4a) Every attempt appends one line to `.task-mgr/logs/routing-events.jsonl` with provider, model, route reason, and outcome. Later work. Not this loop. Not a migration.
- [ ] (P3) A PRD JSON may carry a `routing` / `models` overlay merged over project config at loop start. **Not scheduled.** Cut P3. One project config is the rollout unit. Restore FR-P3 only when two PRDs in one project must route differently, and do not treat unknown keys as a validation error.
- [ ] (P4b) `task-mgr stats --by-provider` reports first-attempt success, attempts per completed task, duration, rework, and fallback count. The jsonl read is later work. **v22 is not scheduled.** Cut v22. One routing-events.jsonl line per attempt, and stats reads that file. No migration. Restore a migration only if that file cannot be kept for the comparison window.

### Success Metrics

- Phase 1 only: replaying the 2026-07-15 Grok stderr line on the phase-1 gate yields `IterationOutcome::RateLimit`, empty output, and 0 crash-counter increments. This phase does not assert that no Anthropic usage call runs. When Claude is enabled, the existing post-output `load_usage` on `RateLimit` (`reactions/account.rs:619`, `has_rate_limit && anthropic_account_io_allowed`) stays until P1b. This phase does not change `account.rs`, blackouts, or the pre-iteration gate.
- Phase 1 only: a test that creates an extra "interactive" session directory during a Grok spawn finds it intact after cleanup.
- Later (P1a, not phase 1): with Claude below floor and a low/medium task ready, the loop starts that task on Grok within one iteration and never calls the wait.
- Later (jsonl stats, not phase 1, no migration): after two weeks of D4 routing, `stats --by-provider` gives first-attempt success per (provider, difficulty) for at least 30 attempts each.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Idempotent, accurate accounting first.** A limit is a limit, not a crash. A blackout names the right provider. A retry starts clean. Cleanup removes exactly one known directory and is safe to repeat.
- **Typed signals, not sentinels.** A Grok stderr limit travels as a typed error/outcome (mirroring `TaskMgrError::TransientBackend`), never as a marker appended to `output`.
- **Narrow patterns.** New limit phrasing is contiguous and specific (`rate limit for your plan`). Existing patterns are not narrowed (learning 5364).
- **Sequential and wave parity.** Every reaction change lands in the shared inner function and is covered by `tests/reaction_parity.rs`.
- **Config-driven.** No model strings in task JSON. New behavior is off unless config opts in, except the two bug fixes (P1c, P1e), which are unconditional.
- **D2 is a hard rule.** Review-class (`CODE-REVIEW-*`, `REVIEW-*`, `MILESTONE-*`) and `high` tasks never spill because of a blackout. They stay on the primary provider unless the project opts into P6 `review.differentProvider` (default off). That flag swaps provider only, and only onto a non-null frontier model. Factory `tierFallback.includeReview` may still walk a review down a cheaper rung on the same provider. The standard path floor does not hold review at frontier.

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
| E6 | Grok limit with exit 0 and empty stdout | ANALYSIS-001 did not recover an exit code | Scan stderr when `exit_code != 0` or stdout is empty. Exit 0 with empty stdout is the typed limit error. Exit 1 is the non-zero stand-in, not an observed status |
| E7 | Stdout contains `<completed>X</completed>` and stderr has the limit line, on exit 0 or on a non-zero exit | A non-zero exit that already printed a completion tag becomes `RateLimit` and the task is reset to `todo` | If stdout contains a completion tag, do not return the typed limit error on either exit arm. Auth inside the fast-fail window still wins. Exit 0 with other non-empty stdout stays a non-match |
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

- **`TaskMgrError`** (`src/error.rs`): a typed rate-limit variant for runner-detected limits (P1c), carrying the `RunnerKind`. Mapped to `IterationOutcome::RateLimit` in `iteration.rs` and `slot.rs`, beside the existing `TransientBackend` arms (`iteration.rs:721`, `slot.rs:312`). The example to copy is those arms, not an uncommitted variant.
- **`GrokRunner::spawn`** (`runner.rs`): passes `--session-id <uuid>` (P1e). `RunnerResult.session_id` is that uuid only when spawn returns `Ok`. A limit returns `Err`, so `dispatch` does not delete that UUID.
- **`OutputReactionItem`** (`account.rs:239`): gains `provider` (P1b). Later work. Not this loop.
- **`RunAccountQuotaGateParams` / `QuotaPreflightParams`**: gain blackout state and spill eligibility, changed together (P1a). Later work. Not this loop.
- **`ExecutionPlan`** (`model.rs:679`): gains `route: RouteReason` (P4a). Later work. Not this loop. Not a migration.
- **`PrdFile`** (`init/parse.rs:290`): gains optional `routing` / `models` (P3). **Not scheduled.** Cut P3. One project config is the rollout unit. Restore FR-P3 only when two PRDs in one project must route differently, and do not treat unknown keys as a validation error.

### Modularity & Coupling Targets

- No new subsystem. Every phase extends an existing seam.
- No `CONTRACT-xxx` task for P1c/P1e: each is one local change with one consumer chain.

---

## 2.7. Production Readiness

| Concern | Answer |
|---|---|
| Change scope | `runner.rs` (`GrokRunner::spawn`), `error.rs`, `iteration.rs`, and `slot.rs`. This phase does not edit `detection.rs`. The existing clause in `is_rate_limited` stays for Claude stdout. The plan-limit fixture already exists and is not this diff. One behavior that stays: Claude stdout classification, the auth sniff, and the 502 `TransientBackend` sniff. This phase does not change `account.rs`, blackouts, or the pre-iteration gate. P6.1 does not land in this edit. |
| Example to copy | The existing `TaskMgrError::TransientBackend` arms in `iteration.rs` (`:721`) and `slot.rs` (`:312`), and Claude’s `--session-id` push immediately before the prompt flag (`runner.rs:632-635`, then `-p`). Do not cite the uncommitted `RunnerRateLimited` variant as the example. |
| Boundary validation | Stderr is scanned only for the contiguous case-insensitive phrase `rate limit for your plan`, and only when `exit_code != 0` or stdout is empty. The sniff does not call `is_rate_limited`. If stdout contains a `<completed>…</completed>` tag, that scan does not return the typed limit error on either exit arm. `--session-id` is a new v4 UUID. `cleanup_session` joins only that UUID under the encoded cwd. Unset `HOME` does not clean up. |
| Failure semantics | A matching Grok limit is `IterationOutcome::RateLimit` with empty output and no crash-counter increment. An auth hit inside the fast-fail window stays `GrokAuthFailure`. A 502 with no limit line stays `TransientBackend`. Exit 0 with non-empty stdout is not this error. If stdout contains a completion tag, do not return the typed limit error on either exit arm. A missing session directory is `Ok`. Unset `HOME` does not panic. `dispatch` still cleans up only when spawn returns `Ok`. |
| Observability | The active PRD prefix is on `.task-mgr/logs/task-mgr-<prefix>.log` and `tasks/progress-<prefix>.txt`. Tracing has no redactor, so stderr, stdout, and the limit sentence stay off tracing events. The existing line is `orchestrator.rs` “Rate limit detected in output, running account reaction...” (`orchestrator.rs:1080`) and it does not name Grok. Do not claim a new line in this phase. P4a’s jsonl is later work, not this phase’s event. |
| Rollout & rollback | Phase 1 has no setting. Chris rebuilds after merge; the loop does not install the binary. Rollback is revert that commit. No migration. A running process keeps the old binary until that rebuild. |
| Proof | P1c tests for this edit: the mock-grok exit-1 and exit-0-empty-stdout tests and the sequential and wave `RateLimit` mapping tests. Those fail before the production edit and pass after. A non-zero stderr buffer that contains `429` and `rate` but not `rate limit for your plan` is not this error. The fixture, straight-apostrophe, and upper-case `is_rate_limited` cases already pass on FIX-001 and are not this diff; do not revert that clause to force a red run. P1e tests: the mock argv/`--session-id` test (other UUID and `prompt_history.jsonl` survive), missing-dir `Ok`, and unset-`HOME`. Those fail before that production edit and pass after. This phase’s proof has no P1a/P1b FakeRunner sentence. |

---

## 3. User Stories

### US-001: A Grok limit does not burn crash budget
As an operator, when Grok hits its plan limit, the loop treats it as a limit (blackout or wait), not a crash, so tasks are not auto-blocked.

### US-002: Loops never delete my interactive Grok session
As Chris running Grok interactively in a project root, a loop spawning Grok in the same directory deletes only its own session.

### US-003: Work continues on Grok while Claude is out
As an operator, when Claude is below its floor, low and medium tasks run on Grok. High tasks and reviews do not spill onto Grok because of that blackout. A review can still move to a cheaper Claude rung when factory `includeReview` is true. `differentProvider` is off in this story.

### US-004: Easy tasks start on Grok so we can compare
As Chris, low-difficulty implementation starts on Grok and every attempt is logged, so I can decide routing from data.

### US-005: A retry starts clean
As an operator, an attempt cut off by a limit leaves no half-edits for the next attempt or the wrapper commit.

### US-006: A project can aim strong models at the trees that need them (P6)
As a project owner, I set provider-by-difficulty and a list of sensitive paths in `.task-mgr/config.json`, and a low-difficulty task that touches those paths still runs at standard tier or above. A present null `standard` rung (builtin Grok and Codex) still runs, with no `-m` flag. New tasks from `add`, `loop init`, and `batch init` must name a difficulty and must not name a model. I do not fill in a per-task profile. Spec: [`tasks/planned-model-routing-guide.md`](planned-model-routing-guide.md).

---

## 4. Functional Requirements (phased, in build order)

P1c and P1e are one loop (`tasks/model-routing-p1-grok-safety.json` only): both edit `GrokRunner::spawn`. Later work, not this loop: P1a, P1b, P1d, P2, P6.1, P6.2, P6.4, and the jsonl stats phase (P4a writes the line; stats reads that file). P6.3, P6.5, P3, and v22 are not scheduled. Do not generate task files for them. The must-before edge that still schedules later work is **P1c+P1e → P6.1**. **P1d → P6.3** and **P4a → P4b** do not schedule P6.3 or v22. P1e does not block P1b. P1b and P1a share `account.rs` and may land in either order, not together. P1a does not block P1d on files; class-list spill stays wrong until P1d, and default-path spill does not. P6 is not one block after P1d. The graph, the same-window file table, and the sets that may be in flight together are in [`tasks/planned-model-routing-guide.md`](planned-model-routing-guide.md) §2. That section wins if this paragraph and the guide disagree. Parallel sets, cuts applied. One loop per checkout. A second checkout may run only a file-clean set from guide §2, with P3, P6.3, P6.5, and v22 removed. P6.1 stays out of any set that still edits iteration.rs, slot.rs, or iteration_pipeline.rs.

Later lists from the guide, not more stories in the P1 file, and not an implementation of P2: **P6-early** (P6.1, P6.2) and **P6.4**. P6.3 and P6.5 are not in those lists. P6.4 and P4a are not the same `resolve_execution_plan` window. P4a may land first, and then `RouteReason` omits `SensitivePathFloor` until P6.4 has a producer. The path floor must not grow a second blackout hop.

### FR-P1c: Recognize Grok's plan rate limit (first loop)
- This phase does not edit `detection.rs`. The existing clause in `is_rate_limited` stays for Claude stdout. Do not narrow that function. Do not revert it to force a red run. The recovery probe and limit-shape recording keep the clause because this phase does not edit the function.
- In `GrokRunner::spawn`, sniff stderr after the auth check (`runner.rs:1086`) and before the transient check (`runner.rs:1102`). The sniff checks the case-insensitive phrase `rate limit for your plan`. It does not call `is_rate_limited`. Do not extract a shared helper. One duplicated literal is the whole cost. Scan only when `exit_code != 0` or stdout is empty. A non-zero stderr buffer that contains `429` and `rate` but not that phrase is not this error. Copy the `TaskMgrError::TransientBackend` match arms (`iteration.rs:721`, `slot.rs:312`). Do not cite the uncommitted `RunnerRateLimited` variant as the example. If that variant is already declared, return it; do not add a second variant.
- If stdout contains a completion tag (`<completed>…</completed>`, `output_parsing::parse_completed_tasks` at `output_parsing.rs:114` is non-empty), do not return the typed limit error on either exit arm. Leave stdout on the `Ok` result so the completion path still sees the tag. Auth inside the fast-fail window still wins. Exit 0 with other non-empty stdout stays a non-match.
- Map that error to `IterationOutcome::RateLimit` with empty output and no crash-counter increment, on the sequential and wave paths.
- Phase 1 asserts that outcome, empty output, and no crash-counter increment only. When Claude is enabled, the existing post-output `load_usage` on `RateLimit` stays until P1b. This phase does not change `account.rs`, blackouts, or the pre-iteration gate.
- Claude stdout classification, the auth sniff, and the 502 `TransientBackend` sniff stay.
- Fixture `tests/fixtures/grok_stderr_plan_rate_limit.txt` already exists. This loop does not edit it. The 91-byte plan line contains the phrase, so the mock-grok tests still prove the positive case.

### FR-P1e: Deterministic Grok session cleanup (first loop)
- Generate `Uuid::new_v4()` per spawn and pass `--session-id <uuid>` immediately before the prompt flag, mirroring Claude (`runner.rs:632-635`). On the `Ok` path, `RunnerResult.session_id` is that uuid.
- Delete the before/after directory snapshot (`runner.rs:1044-1061`) and `pick_newest_by_mtime` (`runner.rs:502`). Do not keep `pick_newest_by_mtime`.
- `cleanup_session` is unchanged (`runner.rs:1130`): it joins only that UUID under the encoded cwd, `NotFound` is `Ok`, and unset `HOME` returns `Ok` without panicking.
- A limit returns `Err`, so `dispatch` (`runner.rs:2038`) does not delete that UUID, same as auth and 502. Do not widen `dispatch`. `session_id` is not always `Some` for Grok.

### FR-P1b: Blackout the provider that hit the limit

Later work. Not this loop. Phase 1 does not change `account.rs`, blackouts, or the pre-iteration gate. When Claude is enabled, the existing post-output `load_usage` on `RateLimit` stays until P1b.
- `OutputReactionItem.provider` from `effective_runner`; `react_to_outputs_inner` records that provider (`account.rs:756`).
- Anthropic usage load and the Claude probe run only when the limited provider is Claude (`account.rs:618, 642, 654`).
- A Grok `RateLimit` with no reset uses `blackoutFallbackSecs`.

### FR-P1a: Provider-aware pre-iteration gate

Later work. Not this loop. Phase 1 does not change the pre-iteration gate.
- In `run_account_quota_gate_inner`, when the Claude decision is an account-binding Wait/Stop **and** another provider is enabled **and** spillover is configured **and** `compute_remaining_work_snapshot` (with the blackout applied) finds a runnable non-Claude task: record `provider_blackouts.record(Claude, now, reset_secs)` and proceed.
- Otherwise behavior is unchanged, including `handle_quota_deferral` when nothing can move.
- Change `QuotaPreflightParams` and `RunAccountQuotaGateParams` together; extend `tests/reaction_parity.rs`.

### FR-P1d: Routes honor blackouts, D2 holds

Later work. Not this loop.
- `first_enabled_provider` (`model.rs:1271`) skips blacked-out providers, so `providerPreference` / `byDifficulty` lists are ordered fallbacks.
- `byIdPrefix` falls to `providers.<p>.fallback` when its provider is blacked out.
- Review-class and `high` tasks never move (D2). Explicit pins never move (D6). No new knobs in P1d.
- The blackout path still never writes `runner_overrides` (keep `blackout_reroute_leaves_runner_overrides_untouched`).

### FR-P6: Per-project class routes and a sensitive-path floor

Spec: [`tasks/planned-model-routing-guide.md`](planned-model-routing-guide.md). That guide wins if this summary and the guide disagree. Schedule is the order paragraph above, not "all of P6 after P1d." P6.1, P6.2, and P6.4 are later work. P6.3 and P6.5 are not scheduled. Do not restore them in this effort.

- P6.1 (later work, not this loop): prints the capability tier on both iteration banners and the shared progress line, beside model and effort. `Model: (default)` remains the null-model-flag wording. The tier is what shows that a difficulty was applied. Keep P6.1. The tier line stays a phase so a null Grok standard rung is not read as "no route." Display-only, after P1c's banner sites are committed. It adds no routing rule.
- P6.2 (later work, not this loop): `task-mgr add`, `loop init`, and `batch init` require `difficulty` ∈ {`low`, `medium`, `high`} and reject a `model` field. Legacy NULL difficulty rows are not rewritten; the resolver keeps treating them as medium.
- P6.3 (**not scheduled**; spec kept; do not restore the subcommands in this effort): `task-mgr models set-class` / `unset-class` write `taskClasses` (`providerPreference`, `byDifficulty`, `forceTier`, review `differentProvider`). Ordinary work stops using `byIdPrefix`. Codex and deliberate overrides keep it. This is the only P6 step that waits for P1d. Cut P6.3. Operators edit the class block in config JSON. Restore the subcommands after P1d only if hand-edited lists are actually mis-set.
- P6.4 (later work, not this loop): `routing.sensitivePaths` globs match `task_files`. A hit raises the already chosen provider to at least `standard`. A present JSON-null `standard` rung is a successful raise (`Model: (default)`). An absent `standard` key excludes the task before claim. Do not call `model_for`. The raise does not change provider and does not hold review at frontier. The shipped down-walker must not cross below that floor, including a matching explicit pin when `includeForced` is true. If no legal rung remains, exclude. `model: None` is not that exclusion. `PlanContext` must carry the file list at every call site, and the exclusion and snapshot queries must load `task_files`.
- P6.5 (**not scheduled**; spec kept; do not restore the review-provider swap in this effort): `review.differentProvider` defaults off (D2). When set, review swaps to the first enabled non-primary provider whose frontier tier is a non-null model string. A missing key or JSON `null` does not count. No such provider → exclude, reason `review.differentProvider:no-frontier`. On the builtin ladders that means primary Claude defers every review until a frontier model is pinned. This is not `routing.reviewCascade`. Cut P6.5. Review does not move to another provider in this effort. D2 stands. Restore from the P6 guide after the floor, never in the same edit, and do not enable reviewCascade.

P6 does not import `taskType`, does not split effort from tier, and does not change the overflow ladder. Defer for the floor is a shared exclusion, the same path as `compute_quota_excluded_ids`, and the horizon snapshot must agree.

### FR-P2: Revert an interrupted attempt (D5)

Later work. Not this loop.
- Before each spawn, record the dirty-path baseline (reuse `git_reconcile::capture_status_paths`).
- When the attempt ends in `RateLimit`, `TransientBackend`, timeout, or crash without completion, restore exactly the paths the attempt touched (current − baseline) to their pre-attempt state: tracked files via `git restore --source=HEAD --staged --worktree -- <paths>`, new untracked files removed. Paths dirty before the attempt are never touched.
- Log the reverted paths to progress. The `routing-events.jsonl` line, including the reverted-path count, is P4a's. P2 does not create that file.
- No prompt section for interrupted work (nothing carries over).

### FR-P4a: Per-attempt routing events

Later work. Not this loop. Not v22. One routing-events.jsonl line per attempt, and stats reads that file. No migration.
- `ExecutionPlan.route: RouteReason` (`Explicit | ByIdPrefix | TaskClass | BlackoutReroute | Anchor | OverflowPivot`). Add `SensitivePathFloor` only when P6.4 has a producer. P4a may land first without that variant. The banner token from the planned guide is the value recorded here. P6 does not create this file.
- One JSON line per attempt in `.task-mgr/logs/routing-events.jsonl`: ts, run_id, iteration, task_id, task_class, difficulty, provider, model, effort, tier, route, fallback_from, blackouts_active, outcome, duration_s, files_changed, self_committed, reverted_paths count.

### FR-P3: Per-PRD routing overlay

**Not scheduled.** Cut P3. One project config is the rollout unit. Restore FR-P3 only when two PRDs in one project must route differently, and do not treat unknown keys as a validation error. Spec kept. Do not restore the overlay in this effort.

- Optional top-level `routing` / `models` objects in the task JSON, deep-merged over project config at loop start (`project_config.rs:337-356`), validated by `validate_models_config` with the PRD path in errors. No DB migration. `RoutingConfig` has no `deny_unknown_fields`, so `sensitivePaths` and `differentProvider` are ignored today. P3 must not assert a validation error for them. When P6.4 and P6.5 land, `validate_models_config` accepts those keys. An explicit deny list before that is new P3 work.

### FR-P4b: Durable stats

**v22 is not scheduled.** Cut v22. One routing-events.jsonl line per attempt, and stats reads that file. No migration. Restore a migration only if that file cannot be kept for the comparison window. The migration spec below is kept and is not restored in this effort. The jsonl stats read is later work, not this loop.

- Migration v22: `run_tasks` gains effort, tier, route, outcome, fallback_from, stamped on every attempt (`iteration_pipeline.rs:505`).
- `task-mgr stats --by-provider` grouped by (provider, class, difficulty). Later work reads `.task-mgr/logs/routing-events.jsonl`. It does not add v22.

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

Review-class IDs keep the built-in frontier force and do not spill (D2). The P6 keys `sensitivePaths` and `review.differentProvider` are not part of this P1 block. They are specified in the planned guide and stay absent until that phase.

---

## 5. Non-Goals (Out of Scope)

- Replacing or reinstalling the task-mgr binary from a loop. Chris rebuilds after merge.
- Changing any project's `.task-mgr/config.json` in this branch (rollout R1/R2 are separate, operator-run steps).
- New spillover knobs named `includeReview` or `includeForced`. The shipped `tierFallback` knobs stay: factory `includeReview` is true, factory `includeForced` is false.
- A `continue` mode for interrupted attempts (D5).
- Grok usage API polling (Grok has none; limits come from stderr).
- Importing `taskType` into SQLite for routing.
- A per-task profile (kind, risk, reasoning depth, context breadth, verification strength) and any routing on `taskType`.
- Choosing effort separately from difficulty, or retrying the same model at a higher effort before stepping the tier. Overflow stays the shipped ladder: lower `xhigh` to `high` once, up one defined tier, Claude `[1m]` at the ceiling, then `providers.<source>.fallback`.
- Treating `review.differentProvider` as the deferred review cascade (`tasks/prd-review-cascade.md`). The cascade still owns `routing.reviewCascade` and `completed_by_provider`.
- Embedding-model switching (`tasks/prd-model-switching-ux.md`).

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
| P1c | Phase 1. `src/loop_engine/runner.rs` (`GrokRunner::spawn` checks `rate limit for your plan`; it does not call `is_rate_limited`), `src/error.rs`, `src/loop_engine/iteration.rs`, `src/loop_engine/slot.rs`. Not `detection.rs`. Not the plan-limit fixture. Not `account.rs`. |
| P1e | Phase 1. `src/loop_engine/runner.rs`. Do not widen `dispatch`. |
| P1b | Later work. `reactions/account.rs`, output-item constructors in `orchestrator.rs` and `wave_scheduler.rs`. Not `iteration.rs`. Not this loop. |
| P1a | Later work. `reactions/account.rs` (`run_account_quota_gate_inner`, `compute_remaining_work_snapshot`), call sites `iteration.rs` and `wave_orchestration.rs`, `tests/reaction_parity.rs`. Not `engine.rs`. Not `pre_spawn.rs`. Not this loop. |
| P1d | Later work. `model.rs` only (`first_enabled_provider`, `byidprefix_route`, `class_route`). Not `pre_spawn.rs`. Not this loop. |
| P6 | Later work for P6.1, P6.2, and P6.4. P6.3 and P6.5 are not scheduled. File table in [`tasks/planned-model-routing-guide.md`](planned-model-routing-guide.md) §7. Same-window rules in §2. P6.1 shares `iteration.rs` and `slot.rs` with P1c, so it stays out of any set that still edits those files. |
| P2 | Later work. `slot.rs` (wave baseline), `git_reconcile.rs` `capture_status_paths`, sequential baseline in `orchestrator.rs`. Not `iteration.rs`. Not `runner.rs`. Not this loop. |
| P4a | Later work (jsonl). `model.rs` (`ExecutionPlan.route`, `resolve_execution_plan`) and the stamp inside `iteration_pipeline.rs` `process_iteration_output`. Not `observability.rs`. Do not add `ProcessingParams` fields. No migration. |
| P3 | **Not scheduled.** `init/parse.rs` (`PrdFile` overlay keys), `startup.rs` load, `project_config.rs` deep-merge. Not `orchestrator.rs`. Do not restore the overlay. |
| P4b | **v22 is not scheduled.** Spec kept: a migration after v21 (no `v22.rs` yet) and the `run_tasks` stamp inside `iteration_pipeline.rs` `process_iteration_output`. The jsonl stats phase is later work: `commands/stats.rs` reads `routing-events.jsonl`. No migration in this effort. |

### Approaches & Tradeoffs (P1c)

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| A. Append the stderr line to `output` so `analyze_output` sees it | Tiny | Sentinel in output; collides with the gated-detection branch's provenance rule | Rejected |
| B. **Typed error from `GrokRunner::spawn`, mapped beside `TransientBackend`** | Matches the existing Grok auth/5xx pattern; no change to `analyze_output`. The sniff checks the plan phrase only and does not call `is_rate_limited`. This phase does not edit `detection.rs` | One new error variant and two match arms | **Preferred** |
| C. Broaden `is_rate_limited` with bare `rate limit` | One line | False positives on Claude prose | Rejected |

### Approaches & Tradeoffs (P1e)

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| A. Keep the dir diff, add a "created after spawn start" filter | No CLI flag | Still a heuristic; still races with interactive sessions | Rejected |
| B. **`--session-id <uuid>`** | Exact; mirrors Claude; deletes code | Depends on grok ≥ the version with `-s` (1.0.50 has it) | **Preferred** |

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| Grok exits 0 on a limit, so an `exit != 0` gate misses it | Med | Settled unknown code | ANALYSIS-001: scan when `exit_code != 0` or stdout is empty (E6). A completion tag is not a limit on either arm (E7) |
| Merge conflict with `feat/cli-error-gated-detection` in `runner.rs` / `detection.rs` | Low | High | This phase does not edit `detection.rs`. P1c adds one stderr branch in `runner.rs`. P1e touches only the session snapshot |
| Grok quality on low tasks (D4) | Med | Med | P4a data; byDifficulty is config |
| Reverting an attempt removes useful partial work (D5) | Low | Med | Chris chose revert; logged paths make it auditable |
| Parity tests are dense | Low | High | Change param structs together; compile errors are the audit |

### Security Considerations

- No new secrets or network calls. Tracing events carry no output or stderr text.

### Consumers of Changed Behavior (P1c/P1e)

| File:Line | Usage | Impact | Mitigation |
|---|---|---|---|
| `detection.rs:254` `is_rate_limited` (not `analyze_output` at `detection.rs:149`) | Claude stdout. The recovery probe and limit-shape recording keep the same function. Not the Grok stderr sniff | Existing plan-phrase clause stays for Claude stdout (FIX-001). This phase does not edit `detection.rs`. The sniff does not call this function | E3 discriminator |
| `runner.rs:1086` auth, then `runner.rs:1102` transient | stderr sniffs today only inside `exit_code != 0` | Limit sniff after auth and before transient. The predicate is the case-insensitive phrase `rate limit for your plan`, not a call to `is_rate_limited`. Scan only when `exit_code != 0` or stdout is empty. A `<completed>…</completed>` tag: do not return the typed limit error on either exit arm. Auth inside the fast-fail window still wins. Exit 0 with other non-empty stdout stays a non-match | E4, E6, E7 |
| `iteration.rs:721` / `slot.rs:312` `TransientBackend` arms | sequential / wave | New arm beside those arms → `RateLimit`, empty output, no crash-counter increment. Copy those arms. Do not cite the uncommitted `RunnerRateLimited` variant as the example | Mirror `TransientBackend` |
| `runner.rs:2038` `dispatch` | cleanup only when spawn returns `Ok` | A limit returns `Err`, so `dispatch` does not delete that UUID, same as auth (`runner.rs:1087`) and 502 (`runner.rs:1103`). Do not widen `dispatch`. `session_id` is not always `Some` | Missing dir is `Ok` only after an `Ok` spawn |
| `tests/grok_runner_unit.rs` dir-diff assert and `tests/runner_cleanup.rs` (`dispatch_grok_cleanup_ok_removes_session_dir`) | pin the heuristic and `pick_newest_by_mtime` | Replaced. Do not keep `pick_newest_by_mtime` | New E8 test: other UUID and `prompt_history.jsonl` survive |

### Documentation

| Doc | Action |
|---|---|
| `src/loop_engine/CLAUDE.md` | Grok limit → `RateLimit` with empty output; Grok session cleanup via `--session-id`. Do not add a UX line that names Grok. `orchestrator.rs:1080` stays “Rate limit detected in output, running account reaction...” |
| `CLAUDE.md` (models section) | After P1a/P1d: dual-provider config and D2 semantics |
| `src/loop_engine/CLAUDE.md` | Pointer to the P6 guide now. Behavior notes for the floor land with P6.4, not before. |
| `docs/designs/model-selection-redesign.md` | Pointer only. Shipped precedence does not change until P6.4. |

---

## 7. Open Questions

- [x] How does Grok exit on a plan limit (code, stdout)? ANALYSIS-001 settled it: the 2026-07-15 exit code and stdout were not recorded. Scan stderr when `exit_code != 0` or stdout is empty. Exit 1 is the non-zero stand-in. Not a question for this loop.
- [ ] Before P1a: should the 2%/1% Claude floors stay as the trigger, or should spillover start at a higher floor so Claude keeps headroom for reviews? Not a phase-1 question. P1a is later work. D1–D6 stay.

---

## Appendix

### Rollout

- R0: P1c + P1e merged; Chris rebuilds and installs.
- R1: this repo and mw_integrations: enable Grok with fallback after P1a–P1d land.
- R2: restaurant_agent_ex: re-enable Claude, apply the target config (D1, D4). Launch Grok-routed loops from worktrees until P1e is installed.

### Related

- Plan: `task-mgr-routing-plan.md` (Remy, 2026-10-08), §7.2 arcomp2 verification
- P6 spec: [`tasks/planned-model-routing-guide.md`](planned-model-routing-guide.md)
- Branch: `feat/cli-error-gated-detection` (prefix `fe92ec5b`)
- Learnings: 5364 (keep widened patterns), 5298 (usage-gate vs probe split), 3754 (cleanup is a trait method), 4696 (cleanup idempotence test flake), 3204 (document provider workarounds)

---

## Pins

These sentences are the acceptance. Do not reopen D1–D6. Do not restore a cut phase in this effort.

1. Keep P6.1. The tier line stays a phase so a null Grok standard rung is not read as "no route." Display-only, after P1c's banner sites are committed. It adds no routing rule.
2. Cut P6.3. Operators edit the class block in config JSON. Restore the subcommands after P1d only if hand-edited lists are actually mis-set.
3. Cut P6.5. Review does not move to another provider in this effort. D2 stands. Restore from the P6 guide after the floor, never in the same edit, and do not enable reviewCascade.
4. Cut P3. One project config is the rollout unit. Restore FR-P3 only when two PRDs in one project must route differently, and do not treat unknown keys as a validation error.
5. Cut v22. One routing-events.jsonl line per attempt, and stats reads that file. No migration. Restore a migration only if that file cannot be kept for the comparison window.
6. Parallel sets, cuts applied. One loop per checkout. A second checkout may run only a file-clean set from guide §2, with P3, P6.3, P6.5, and v22 removed. P6.1 stays out of any set that still edits iteration.rs, slot.rs, or iteration_pipeline.rs.
7. D1–D6 in the parent PRD stay. This acceptance does not reopen them.

---

## AA review (folded)

Architect verdict NEEDS_CHANGES (2026-10-27, production-code-architect) is folded here. Questions for User: none.

1. **§2.7 Change scope was blank.** Resolved in the Change scope row. This phase names `detection.rs` (`is_rate_limited`), `runner.rs` (`GrokRunner::spawn`), `error.rs`, `iteration.rs`, `slot.rs`, and the plan-limit fixture. What stays: Claude stdout classification, the auth sniff, and the 502 `TransientBackend` sniff. `account.rs` is not in the set.
2. **§2.7 Example to copy was blank.** Resolved in that row. The example is the existing `TaskMgrError::TransientBackend` arms in `iteration.rs` and `slot.rs`, and Claude’s `--session-id` push immediately before the prompt flag. The uncommitted `RunnerRateLimited` variant is not the example.
3. **§2.7 Boundary validation was blank.** Resolved in that row. Stderr is scanned only for `rate limit for your plan`, and only when `exit_code != 0` or stdout is empty. `--session-id` is a new v4 UUID. `cleanup_session` joins only that UUID under the encoded cwd. Unset `HOME` does not clean up.
4. **§2.7 Failure semantics was blank.** Resolved in that row. A matching Grok limit is `IterationOutcome::RateLimit` with empty output and no crash-counter increment. Auth inside the fast-fail window stays `GrokAuthFailure`. A 502 with no limit line stays `TransientBackend`. Exit 0 with non-empty stdout is not this error. A missing session directory is `Ok`. Unset `HOME` does not panic. `dispatch` still cleans up only when spawn returns `Ok`. The completion-tag rule from concern 11 is in the same row so a non-zero exit that already printed `<completed>…</completed>` is not this error either.
5. **Observability misstated the UX line and named no correlation id or redactor rule.** Resolved in that row. Correlation is the active PRD prefix on `.task-mgr/logs/task-mgr-<prefix>.log` and `tasks/progress-<prefix>.txt`. Tracing has no redactor, so stderr, stdout, and the limit sentence stay off tracing events. The existing line is `orchestrator.rs:1080`, “Rate limit detected in output, running account reaction...”, and it does not name Grok. This phase claims no new line. P4a’s jsonl is later work, not this phase’s event.
6. **Rollout & rollback was absent, and rollback said “until P4b.”** Resolved as one row. Phase 1 has no setting. Chris rebuilds after merge; the loop does not install the binary. Rollback is revert that commit. No migration. A running process keeps the old binary until that rebuild. “Until P4b” is gone.
7. **Proof did not name tests that fail before and pass after, and it included a later phase.** Resolved in that row. It names the fixture, straight-apostrophe, and upper-case `is_rate_limited` cases, the mock-grok exit-1 and exit-0-empty-stdout tests, the sequential and wave `RateLimit` mapping tests, and the P1e argv/`--session-id`, missing-dir `Ok`, and unset-`HOME` tests. The mock-grok, mapping, and P1e tests fail before that production edit and pass after. The three classifier cases already pass (FIX-001, commit `f6bb069`); reverting them to manufacture a red run is not the proof. The P1a/P1b FakeRunner sentence is not in this phase’s proof.
8. **Cut phases were still specified as work this goal will do.** Resolved in §2, §2.6, §4, FR-P6.3, FR-P6.5, FR-P3, and FR-P4b. P6.3, P6.5, P3, and v22 are not scheduled, under pins 2–5, quoted there and in Pins. The spec text is kept. The subcommands, the review-provider swap, the overlay, and the migration are not restored. P6.1, P6.2, P6.4, P1a, P1b, P1d, P2, and the jsonl stats phase stay later work. The P1 JSON overview sentence in `tasks/model-routing-p1-grok-safety.json` (“Later phases (P1b, P1a, P1d, P2, P4a, P3, P4b) get their own task files from the same PRD.”) is an ACCEPTED residual until a task-mgr update, because the loop reimports that file. Do not hand-edit it. It does not schedule P3 or P4b.
9. **The phase-1 success metric required “no Anthropic usage call,” which is P1b.** Resolved in §2 Success Metrics and FR-P1c. Phase 1 asserts `RateLimit`, empty output, and no crash-counter increment only. When Claude is enabled, the existing post-output `load_usage` on `RateLimit` stays until P1b. This phase does not change `account.rs`, blackouts, or the pre-iteration gate. The FIX-002 acceptance line “No new Anthropic usage I/O on this path; Claude stdout classification unchanged” is an ACCEPTED residual until a task-mgr update. Read it as “do not add any,” not as “the existing Claude-enabled load is gone.” Do not hand-edit the JSON.
10. **FR-P1c and §6 still pointed at pre-rebase sites and said Grok `session_id` is always `Some`.** Resolved in §1 items 3–4, FR-P1c, FR-P1e, and §6. The sniff calls `is_rate_limited` (`detection.rs:254`), not `analyze_output` (`detection.rs:149`). It sits after auth and before transient. The arms to copy are the `TransientBackend` arms. A limit returns `Err`, so `dispatch` (`runner.rs:2038`) does not delete that UUID, same as auth and 502. `dispatch` is not widened. `pick_newest_by_mtime` is not kept. `session_id` is not always `Some`.
11. **A non-zero exit that already printed a completion tag became `RateLimit`, and the task was reset to `todo`.** Resolved in §2.5 E7, FR-P1c, §2.7 Failure semantics, and the §6 sniff row. If stdout contains a `<completed>…</completed>` tag, do not return the typed limit error on either exit arm. Auth inside the fast-fail window still wins. Exit 0 with other non-empty stdout stays a non-match.

**Inversion.** A blank Change scope no longer lets this loop edit `account.rs` or a cut phase to satisfy a usage-call metric. The exit gate is `exit_code != 0` or empty stdout, so an exit-0 empty-stdout limit is not `Empty`. A completion tag on either arm is not stolen. Mapping the limit to `Err` does not widen `dispatch`; cleanup still runs only on `Ok`, same as auth and 502, including the UUID the limit run just created. `session_id` is not treated as always `Some`. P6.3, P6.5, P3, and v22 are not goals of this loop, so rollback stays “revert that commit” with no migration. The UX line still does not name Grok; that is accepted until a later phase, and this phase does not pretend otherwise. The two JSON residuals in concerns 8 and 9 are the remaining holes, and they stay until a task-mgr update.

---

## First-principles review (folded)

First-principles verdict: one cut (2026-10-27). Adversary: CA ALL-UPHELD, cut 1. Chris accepted that cut. No contested cuts. No rejected cuts. Pins are unchanged. P1e is unchanged.

**Accepted cut: drop the full-classifier sniff.** The Grok stderr sniff matches only the case-insensitive phrase `rate limit for your plan`. It does not call `is_rate_limited`. This phase does not edit `detection.rs`. The existing clause in `is_rate_limited` stays for Claude stdout.

Applied in the body, not only here:

- FR-P1c no longer says return a typed error when `is_rate_limited(&stderr)` matches. The sniff checks that phrase, after auth (`runner.rs:1086`) and before the transient sniff (`runner.rs:1102`). No shared helper. A non-zero buffer with `429` and `rate` but not the plan phrase is not this error. Restore a call to `is_rate_limited` only if a captured Grok limit matches one of its other clauses and not the plan phrase. No such line is in this PRD.
- §2.7 Change scope does not list `detection.rs` as a file this loop edits. The fixture is not this diff.
- The §6 consumer row for `is_rate_limited` is Claude stdout, not the Grok sniff. The sniff row names the phrase. `dispatch` is not widened.

`AA review (folded)` stays as written. Concerns 1 and 10 still say this phase names `detection.rs` and that the sniff calls `is_rate_limited`. Those two sentences are superseded by this cut. Do not follow them.

FIX-002’s task text still says to return the typed error when `detection::is_rate_limited(&stderr_str)` matches. This pass does not edit `tasks/model-routing-p1-grok-safety.json`. That sentence is not an accepted residual. The PRD sniff predicate wins. Do not reopen `detection.rs` to satisfy the task text.
