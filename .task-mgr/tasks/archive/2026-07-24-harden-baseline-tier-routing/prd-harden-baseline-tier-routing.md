# PRD: Harden baseline-tier runner routing + framework follow-ups

**Type**: Bug Fix + Refactor (mixed; sequenced into separate commits)
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-06-02
**Status**: Draft

---

## 1. Overview

### Problem Statement

The `baseline-tier runner routing` change on local `main` (commits `6b27ce2`,
`00fd542`, ahead of `origin/main`) adds `primaryRunner.baselineTierRoutes`
(task-prefix → capability tier `low`/`standard`/`high` → `RunnerSpec`), renames
`fallbackToClaude` → `runtimeErrorFallback` (serde alias + on-disk config
migration), and threads the new tier match into model resolution and the
Codex→Claude recovery path.

A diff review plus two adversarial agent passes (security + architecture), all
findings re-validated firsthand against the source, surfaced:

1. **Three correctness items** in the new code — a read-path that is
   correct-only-by-accident, an untested on-disk config rewrite, and a
   **confirmed baseline-tier derivation divergence** that can route a recovering
   task to the wrong provider (or fail to route it at all).
2. **One real pre-existing security item** — `sanitize_branch_name` does not
   neutralize `..`, so a crafted branch/PRD-derived name can place a git
   worktree outside the intended `-worktrees/` directory. (Three other flagged
   items were validated as NOT-A-RISK and dropped.)
3. **Architectural debt** the new fallback path sits on top of — a baseline-model
   computation duplicated with divergent inputs, a cross-provider idempotency
   guard hand-replicated at three sites, and two god-modules
   (`orchestrator.rs` ~2,995 LOC, `wave_scheduler.rs` ~3,931 LOC) with clean
   extraction seams.

The new code is functionally correct on the happy path and tests pass
(`project_config`: 92, `loop_engine::model`: 97). This work removes latent
fragility, closes the divergence, lifts the config rewrite to the project's own
key-preservation/SSoT bar, and pays down the duplicated-guard + god-module debt.

### Background

Routing precedence (from `src/loop_engine/CLAUDE.md:119-156`):

```
explicit task model
  → direct primaryRunner match (byTaskType > byIdPrefix)
    → compute baseline Claude model (difficulty=high → OPUS, else prd/project/user default)
      → primaryRunner.baselineTierRoutes remap for prefix + baseline tier
        → baseline Claude model
          → None
```

The new `baselineTierRoutes` rung keys on `model_tier(baseline_model)`, so the
**baseline model must be computed identically everywhere the tier is derived**.
Today it is computed inline in `resolve_task_execution_target`
(`model.rs:450-459`) and re-derived with **different inputs** in the
Codex→Claude recovery helper (`recovery.rs:331-336`).

Relevant institutional memory (task-mgr recall):
- **[4418]** "Provider identity must thread from spawn through recovery, not
  resolved anew" — the exact anti-pattern WS-1.3 fixes.
- **[4049]** documents the precedence chain; **[3057]** the layered resolution.
- **[4393]/[4396]/[4378]/[4561]** — key-preserving config writers via
  `serde_json::Value` round-trip + atomic tempfile/rename (WS-1.1/1.2 pattern).
- **[4553]** "insert safe provider only" (Claude never Codex into
  `runner_overrides`) and **[4532]** `source_runner` disambiguation — invariants
  WS-3.1's `promote_once` MUST preserve.

---

## 2. Goals

### Primary Goals

- [ ] Make the config-migration read path correct by construction, not by
      accidental serde-alias coverage.
- [ ] Eliminate the baseline-tier derivation divergence between the primary
      resolution site and the recovery site (full fidelity — identical inputs).
- [ ] Test the on-disk config rewrite (`update_project_config_format`) to the
      project's key-preservation + idempotency bar.
- [ ] Neutralize the `..` path-traversal gap in `sanitize_branch_name`.
- [ ] Extract the two foundational abstractions the fallback matrix needs
      (`compute_baseline_model`, `promote_once`) and split the two largest
      god-modules along their clean seams — each as its own behavior-neutral commit.

### Success Metrics

- Baseline-tier parity: recovery and primary resolve the **same** tier for the
  same task across all default-source permutations (new regression test).
- Zero behavior change on the documented happy path: existing routing-precedence
  assertions in `model.rs` and `tests/primary_runner_routing.rs` pass **without
  edits** (only mechanical `..Default::default()` additions allowed).
- Config rewrite: unrelated keys survive byte-for-byte; second run is a no-op
  (`Ok(false)`).
- Full `cargo test` green + `cargo run --bin gen-docs -- --check` clean.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Single-source baseline model**: `difficulty=high → OPUS_MODEL`, else first
  non-blank of `[prd_default, project_default, user_default]`, computed in
  exactly one place and consumed by both the primary and recovery sites.
- **No dirty ctx on rollback**: the `promote_once` primitive must build a
  `PendingPromotion` only; it must NOT mutate `IterationContext`. Apply happens
  via `apply_pending_promotion` after `tx.commit()?` (deferred) in recovery, or
  immediately in the non-transactional overflow path.
- **Idempotent provider promotion**: a task promoted once in either direction
  (Claude↔Grok, Grok→Claude, Codex→Claude) never re-promotes in the same loop
  run — preserved through the single `runner_overrides.contains_key` guard.
- **Insert-safe-provider invariant [4553]**: Codex→Claude promotion inserts
  `RunnerKind::Claude` into `runner_overrides`, never `RunnerKind::Codex`.
- **Path containment**: no branch/PRD-derived name can resolve a worktree path
  outside the `<repo>-worktrees/` parent.
- **Config migration is atomic & lossless**: malformed/non-object JSON returns
  `Err` without writing; unrelated/unknown keys are preserved; legacy keys map
  to canonical (`byBaselineTier`→`baselineTierRoutes`,
  `fallbackToClaude`→`runtimeErrorFallback`, `opus`/`sonnet`/`haiku`→
  `high`/`standard`/`low`).

### Performance Requirements

- Best effort. `read_project_config` is on the loop hot path — the WS-1.1 fix
  must not add a second full deserialization; deserializing the already-migrated
  `Value` once is the target (no extra clone beyond what exists).
- The refactors (WS-3.x) must not change algorithmic complexity of scheduling,
  recovery, or resolution.

### Style Requirements

- Follow existing codebase patterns (default): `serde_json::Value` round-trip +
  atomic tempfile/`persist` for config writes [4378]; `ui::*` for operator
  output, `tracing` for diagnostics (CONTRACT-LOG-001); exhaustive matches over
  `RunnerKind`/`RunnerCapability` (no `_ =>` wildcard).
- No `.unwrap()` on fallible IO/serde paths; propagate via `TaskMgrError`.
- Refactor commits (WS-3.2/3.3) must be **pure moves** — no logic edits inside
  the relocated code in the same commit (reviewer must see behavior is unchanged).
- Status mutations stay on `TaskLifecycle` verbs (no raw `UPDATE tasks SET
  status`).

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --------- | -------------- | ----------------- |
| Legacy config with `byBaselineTier` + `fallbackToClaude` + `opus`/`sonnet` tier keys | The read path is correct only via serde aliases today (WS-1.1) | Reads into an identical `ProjectConfig` regardless of alias presence; warning fires once |
| Legacy + canonical key collision for same prefix+tier (`opus` and `high`) | `merge_baseline_tier_routes` silently drops the legacy entry | Canonical wins; documented + test-pinned (WS-1.4) |
| Malformed / non-object `config.json` passed to `update_project_config_format` | Must not truncate or half-write | Returns `Err`, file untouched |
| Second run of `update_project_config_format` on already-canonical config | Idempotency | Returns `Ok(false)`, no write |
| Non-high Codex task: prd=sonnet, project=haiku, user=haiku, `claudeFallbackModel` unset | Recovery currently omits `user_default` and substitutes `claude_fallback_model` for `project_default` → different tier than primary | Recovery resolves the **same** tier/route as `resolve_task_execution_target` |
| `difficulty=high` task with a `high`-tier Codex route | Tier remap intentionally discards the OPUS baseline for a provider hint | `model=None`, `provider_hint=Codex` (unchanged; pinned by existing test) |
| Branch names `..`, `..foo`, `a/../b` | Path traversal out of worktrees dir (WS-2.1) | Sanitized to an in-dir stable name; computed path asserted under `-worktrees/` |
| Dotted branch names `release/1.0.0` | Must not over-sanitize legitimate names | Maps to a stable, collision-resistant in-dir path |
| Group-readable original `config.json` (`0o644`) rewritten by migration | Tempfile defaults to `0o600`, silently narrowing perms (WS-1.4) | Original mode re-applied before `persist` (Unix) |
| A task already carrying a `runner_overrides` entry re-entering any promotion site | Ping-pong/ flapping bounded only by max_retries | `promote_once` returns `None`; falls through to normal failure accounting |

---

## 3. User Stories

### US-001: Trustworthy config read path
**As a** task-mgr operator with a legacy `config.json`
**I want** the read path to migrate-and-deserialize from one normalized value
**So that** a future legacy→canonical rename can't silently deserialize wrong
values while only emitting a warning.

**Acceptance Criteria:**
- [ ] `read_project_config` deserializes from the migrated value (or equivalently
      guarantees alias coverage via an explicit locked test).
- [ ] A legacy-key config produces a `ProjectConfig` identical to its canonical
      form; the legacy-key warning still fires exactly once.

### US-002: Tested, lossless on-disk config rewrite
**As a** maintainer
**I want** `update_project_config_format` covered by a key-preservation +
idempotency test
**So that** the migration can never drop an operator's unrelated config keys.

**Acceptance Criteria:**
- [ ] Test seeds legacy routing keys **plus** `additionalAllowedTools`,
      `embeddingModel`, and a `customField`.
- [ ] After rewrite: legacy keys are canonicalized; unrelated keys survive
      byte-for-byte; second run returns `Ok(false)`; malformed/non-object JSON
      returns `Err` with no write.

### US-003: Recovery routes to the right provider
**As a** loop running Codex with `runtimeErrorFallback` + `baselineTierRoutes`
**I want** the recovery path to derive the baseline tier from the same inputs as
the primary resolution
**So that** a recovering task matches the same tier route it was originally
routed by, rather than a different one (or none).

**Acceptance Criteria:**
- [ ] `compute_baseline_model` is the single home for the baseline computation,
      consumed by both `resolve_task_execution_target` and the recovery helper.
- [ ] `project_default_model` + `user_default_model` are threaded through the
      failure-handler chain into the recovery helper.
- [ ] Regression test pins prd=sonnet/project=haiku/user=haiku/no-fallback:
      recovery tier == primary tier.

### US-004: Worktrees stay inside the worktrees dir
**As a** operator running loops on PRD-derived branch names
**I want** `sanitize_branch_name` to neutralize `.`/`..` components
**So that** no name can place a worktree outside `<repo>-worktrees/`.

**Acceptance Criteria:**
- [ ] `..`, `..foo`, `a/../b` cannot escape the worktrees dir; computed path is
      asserted under the worktrees parent.
- [ ] Dotted branch names still map to a stable in-dir path; idempotent.

### US-005: Single cross-provider promotion primitive
**As a** maintainer extending the fallback matrix
**I want** the idempotency guard + `PendingPromotion` construction in one
`promote_once` primitive
**So that** the next added promotion path can't forget the guard (the historical
ping-pong bug).

**Acceptance Criteria:**
- [ ] All three guard sites route through `promote_once`; `apply_pending_promotion`
      and the deferred-commit ordering are unchanged.
- [ ] `tests/codex_recovery.rs`, `tests/codex_runner_overrides_invariant.rs`, and
      the recovery ping-pong unit tests pass unchanged.
- [ ] `loop_engine/CLAUDE.md` "when you add a THIRD cross-provider promotion
      site…" note updated to point at `promote_once`.

### US-006: Readable orchestrator & wave scheduler
**As a** maintainer
**I want** the linear startup phase out of `run_loop` and the wave decision
trio out of the hot path
**So that** the two largest files are reviewable.

**Acceptance Criteria:**
- [ ] `orchestrator.rs` startup (Steps 1–11) extracted to
      `startup::initialize_loop(...) -> LoopInitContext`; `run_loop` body shrinks
      to iteration-loop + post-loop; no behavior change.
- [ ] `wave_scheduler.rs` `handle_no_eligible_tasks` /
      `handle_ephemeral_deadlock` / `wave_preflight_check` moved to a
      `wave_orchestration` submodule returning a `WaveDecision`; hot path stays put.
- [ ] `tests/reaction_parity.rs` + wave/merge integration suites pass; each split
      is its own commit with no in-move logic edits.

---

## 4. Functional Requirements

### FR-001: `model::compute_baseline_model` (CONTRACT-BASE-001)
Single source of truth for baseline-model computation.

**Details:**
- Signature: `pub fn compute_baseline_model(difficulty: Option<&str>,
  prd_default: Option<&str>, project_default: Option<&str>, user_default:
  Option<&str>) -> Option<String>`.
- Body is exactly the current `model.rs:450-459` logic, reusing the private
  `normalize`.
- `resolve_task_execution_target` calls it (pure refactor).

**Validation:** existing `model.rs` resolution tests pass unchanged; new unit
test covers the difficulty=high short-circuit and the three-default find_map.

### FR-002: `recovery::promote_once` (CONTRACT-PROMO-001)
Centralize the idempotency guard + `PendingPromotion` construction.

**Details:**
- Returns `None` when `ctx.runner_overrides.contains_key(task_id)` (already
  promoted), else `Some(PendingPromotion)`.
- Does NOT mutate `ctx`. Carries `source_runner`/`target_runner` so the existing
  direction-neutral banner ([4532]) and the insert-safe-provider invariant
  ([4553]) are preserved.
- Absorbs the guard at `recovery.rs:193`, `recovery.rs:309`, and
  `reactions/post_output.rs:169`.

**Validation:** ping-pong + insert-safe invariant tests pass unchanged.

### FR-003: Full-fidelity baseline derivation in recovery
Thread `project_default_model` + `user_default_model` (cached on the engine,
`engine.rs:159-161`) through `handle_task_failure` →
`handle_task_failure_with_runner` → `escalate_task_model_if_needed_inner` →
`maybe_codex_fallback_to_claude`, update the `escalate_task_model_if_needed` /
`_for_runner` wrappers and both production callers, and replace the recovery
inline derivation with `compute_baseline_model`.

**Validation:** US-003 regression test; existing codex_recovery tests pass with
updated call shapes.

### FR-004: Robust config read + tested rewrite
WS-1.1 (deserialize migrated value) + WS-1.2 (rewrite key-preservation/idempotency
test) + WS-1.4 (collision doc/test + Unix mode preservation).

**Validation:** new `project_config` tests; `cargo test --lib project_config`.

### FR-005: Branch-name path containment
WS-2.1 — neutralize `.`/`..` components in `sanitize_branch_name`
(`worktree.rs:31-39`) and assert containment in `compute_worktree_path` /
`compute_slot_worktree_path`.

**Validation:** new traversal test; existing worktree tests pass.

### FR-006 (optional): Reject route model strings starting with `-`
WS-2.2 — mirror the existing nonblank-model check in
`validate_runner_routing_config`. Include only if it stays a one-liner + one test.

### FR-007 / FR-008: God-module splits
WS-3.2 (`startup::initialize_loop`) and WS-3.3 (`wave_orchestration`), each a
behavior-neutral move in its own commit.

---

## 5. Non-Goals (Out of Scope)

- **No happy-path routing-precedence change** — Reason: the documented order in
  `loop_engine/CLAUDE.md:119-156` is correct; this work only unifies how the
  baseline feeding it is computed.
- **No new provider, fallback direction, or config schema field** — Reason: this
  is hardening/refactor, not feature growth.
- **No change to the `--dangerously-skip-permissions` / `PermissionMode::Dangerous`
  product contract** — Reason: accepted single-trusted-operator risk; mitigated
  by worktree isolation + the Codex `protected_state` guard.
- **No `runner.rs` split** — Reason: validated as already well-modularized (three
  independent runner impls); size is completeness, not god-module smell.
- **No `promote_once` generalization beyond the existing three sites** — Reason:
  the deferred-commit vs immediate-apply contexts are real; the primitive
  centralizes the guard, not the apply timing.

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/model.rs` — add `compute_baseline_model`; call it from
  `resolve_task_execution_target` (`:446-475`).
- `src/loop_engine/recovery.rs` — `promote_once`; thread defaults through
  `:133/:437/:489/:582/:601/:675`; rewrite the baseline derivation in
  `maybe_codex_fallback_to_claude` (`:297-398`); adopt `promote_once` at `:193`,
  `:309`.
- `src/loop_engine/reactions/post_output.rs` — adopt `promote_once` at `:169`.
- `src/loop_engine/project_config.rs` — WS-1.1 read path (`:663-688`); WS-1.2
  rewrite (`:692`); WS-1.4 `merge_baseline_tier_routes` (`~:1043`) + Unix mode.
- `src/loop_engine/worktree.rs` — `sanitize_branch_name` (`:31-39`) +
  containment in `compute_worktree_path`/`compute_slot_worktree_path`
  (`:42-52`,`:265`).
- `src/loop_engine/orchestrator.rs` → new `src/loop_engine/startup.rs` (WS-3.2).
- `src/loop_engine/wave_scheduler.rs` → new `wave_orchestration` submodule (WS-3.3).
- Production callers of `handle_task_failure*`: `orchestrator.rs`,
  `wave_scheduler.rs`.

### Dependencies

- Internal: `engine.rs` cached `project_default_model`/`user_default_model`
  (`:159-161`); `apply_pending_promotion` (`recovery.rs:92`); `TaskLifecycle`.
- External: none. No new crates.

### Approaches & Tradeoffs

#### Item 3 (baseline divergence)

| Approach | Pros | Cons | Recommendation |
| -------- | ---- | ---- | -------------- |
| **Full fidelity** — extract `compute_baseline_model`, thread real project/user defaults into recovery | Removes the divergence entirely; recovery tier == primary tier always; matches [4418] "thread, don't re-derive" | Touches failure-handler signatures + both callers + recovery unit-test call shapes | **Preferred (user-selected)** |
| Helper + minimal — extract helper, keep recovery's `claude_fallback_model`-as-project_default, add `user_default`, document the approximation | Smaller diff | Tier-match at recovery can still differ from primary in edge cases; leaves a documented foot-gun | Rejected |
| Do nothing | No churn | Confirmed wrong-provider routing on recovery in real configs | Rejected |

#### Item 1 (config read path)

| Approach | Pros | Cons | Recommendation |
| -------- | ---- | ---- | -------------- |
| Deserialize from `migrated` | Migrator becomes the single read normalizer; removes the invisible alias invariant | Slightly changes which value feeds serde (behavior identical given current aliases) | **Preferred** |
| Keep deserializing `value` + lock alias coverage with a test + comment | Minimal change | Preserves the fragile coupling; one more invariant to defend | Alternative |

#### Architecture (promote_once / splits)

| Approach | Pros | Cons | Recommendation |
| -------- | ---- | ---- | -------------- |
| `promote_once` returns `Option<PendingPromotion>`, caller applies | Centralizes guard without collapsing the two apply contexts; preserves deferred-commit boundary | Caller still owns apply timing (acceptable) | **Preferred** |
| `promote_once` also applies to ctx | Fewer caller lines | Reintroduces dirty-ctx-on-rollback risk in the transactional path | Rejected |
| Splits as pure moves, separate commits | Reviewable behavior-neutral diffs; matches CLAUDE.md "refactors are separate commits" | Two extra commits; rebase ordering discipline | **Preferred** |

**Selected Approach**: Full-fidelity item 3 on top of `compute_baseline_model`;
`promote_once` as a guard+construct primitive (no ctx mutation); god-module
splits as pure-move commits sequenced last.

**Phase 2 Foundation Check**: Both contracts pay for themselves. `compute_baseline_model`
(~1 hr) removes an entire class of "two sites drift" bugs as more tier routes
and providers are added. `promote_once` (~0.5 day) means the *next* cross-provider
path is guard-correct by construction rather than by remembering to copy the
ping-pong guard — the historical bug this exact note in CLAUDE.md warns about.
Clear 1:10+ trade-off; take both now.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| ---- | ------ | ---------- | ---------- |
| WS-1.3 default-threading misses a `handle_task_failure*` caller | Med | Low | Compile error on signature change; update recovery unit-test call shapes in the same commit |
| `promote_once` accidentally mutates ctx (dirty-on-rollback) | High | Low | Primitive returns `Option<PendingPromotion>` only; `tests/codex_recovery.rs` deferred-apply tests + a no-ctx-mutation assertion |
| WS-3.2/3.3 moves leak a logic change | Med | Med | Pure-move discipline; behavior-neutral commit; full integration suite + reaction_parity gate before merge |
| WS-2.1 over-sanitizes and collides distinct branch names | Med | Low | Map (not drop) `.`/`..` to a stable token; containment assert; test dotted + traversal names |

### Security Considerations

- **WS-2.1** is the security fix: prevent worktree path escape via `..` in
  branch/PRD-derived names. Containment assertion is the backstop even if
  sanitization is later loosened.
- Validated NOT-A-RISK (no action): `progress_file_name` prefix is an 8-hex md5;
  model strings reach the child as discrete argv elements (no shell); PRD
  `touchesFiles` already gated by `validate_safe_path` (`error.rs:433-486`,
  rejects absolute/`~`/`..`/UNC).
- Accepted product risk (documented, out of scope): default Dangerous permission
  mode + inherited `ANTHROPIC_API_KEY`; contained by worktree isolation +
  `protected_state`.

### Public Contracts

#### New Interfaces

| Module/Function | Signature | Returns (success) | Returns (error) | Side Effects |
| --------------- | --------- | ----------------- | --------------- | ------------ |
| `model::compute_baseline_model` | `(difficulty, prd_default, project_default, user_default: Option<&str>) -> Option<String>` | `Some(model)` or `None` | — (total fn) | none (pure) |
| `recovery::promote_once` | `(ctx: &IterationContext, task_id, source: RunnerKind, target: RunnerKind, target_model: String, pre_promotion_model: Option<String>, new_count: i32) -> Option<PendingPromotion>` | `Some(PendingPromotion)` or `None` (already promoted) | — | none (reads ctx only; no mutation, no DB) |
| `startup::initialize_loop` | `(... run config ...) -> TaskMgrResult<LoopInitContext>` | populated init context | `TaskMgrError` | env/git/PRD validation, DB open, signal handler (relocated, unchanged) |

#### Modified Interfaces

| Module/Function | Current Signature | Proposed Signature | Breaking? | Migration |
| --------------- | ----------------- | ------------------ | --------- | --------- |
| `recovery::handle_task_failure` | `(conn, task_id, iter, ctx, cfg, primary_cfg)` | `+ project_default, user_default: Option<&str>` | No (internal `pub`) | update both production callers + tests in same commit |
| `recovery::handle_task_failure_with_runner` | `(…, executed_runner, cfg, primary_cfg)` | `+ project_default, user_default` | No | same |
| `recovery::escalate_task_model_if_needed_inner` | `(conn, task_id, count, ctx, runner, cfg, primary_cfg)` | `+ project_default, user_default` | No | same |
| `recovery::escalate_task_model_if_needed` / `_for_runner` | current | `+ project_default, user_default` | No | same |

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --------- | ----------------------- | ----------------------------- |
| baseline defaults: engine cache → recovery → `compute_baseline_model` | `WaveIterationParams`/run-config fields `project_default_model: Option<&'a str>`, `user_default_model: Option<&'a str>` (typed) → threaded params (typed `Option<&str>`) → helper args | `compute_baseline_model(difficulty.as_deref(), prd_default.as_deref(), project_default, user_default)` — `prd_default` from `SELECT default_model FROM prd_metadata WHERE id=1`; `project_default`/`user_default` threaded from engine cache, NOT re-read in recovery |
| config migration: file → `serde_json::Value` → `ProjectConfig` | JSON object (string keys: `primaryRunner`, `baselineTierRoutes`, tier keys `low`/`standard`/`high`, alias `byBaselineTier`/`fallbackToClaude`/`opus`/`sonnet`/`haiku`) → `serde_json::Value::Object` (string-keyed) → `PrimaryRunnerConfig.baseline_tier_routes: HashMap<String, HashMap<String, RunnerSpec>>` | `value.get("primaryRunner").and_then(|p| p.get("baselineTierRoutes")).and_then(Value::as_object)`; tier keys stay raw strings until `model::parse_baseline_tier_key` normalizes them |

> Type-transition flag: tier keys are **string** map keys at every level
> (`"opus"` vs `"high"`), not enum variants — `parse_baseline_tier_key` is the
> only place they become `ModelTier`. Both `compute_baseline_model` (via
> `model_tier`) and `primary_runner_baseline_tier_match` must agree on that
> normalization, which is why the baseline they key on must come from one helper.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --------- | ----- | ------ | ---------- |
| `recovery.rs:331-336` | recovery baseline derivation (divergent) | BREAKS (intended) | replace with `compute_baseline_model` + threaded defaults |
| `model.rs:450-459` | inline baseline computation | NEEDS REVIEW | replace with `compute_baseline_model` (pure refactor; pinned by existing tests) |
| `recovery.rs:193`, `:309`, `post_output.rs:169` | hand-replicated promotion guard | NEEDS REVIEW | route through `promote_once`; invariant tests unchanged |
| `orchestrator.rs` / `wave_scheduler.rs` callers of `handle_task_failure*` | pass through defaults | NEEDS REVIEW | add the two threaded args (compile-checked) |
| `orchestrator.rs` `run_loop` Steps 1–11 | startup | OK (moved) | pure-move into `startup.rs` |
| `wave_scheduler.rs` `handle_*` trio | wave decisions | OK (moved) | pure-move into `wave_orchestration` |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --------- | ------- | ---------------- | --------------------- |
| recovery `target_model` selection (`recovery.rs:363-376`) | which Claude model to promote *to* | high→OPUS else `claude_fallback_model` else OPUS | **unchanged** — distinct from the baseline-*tier* derivation that decides *whether/where* to route |
| `compute_baseline_model` output | feeds `model_tier` → tier route match | n/a (new) | decides routing tier only; never the promoted model |

### Inversion Checklist

- [ ] All `handle_task_failure*` callers identified and updated (compile-checked)?
- [ ] Tier-route branching reviewed for both primary and recovery sites?
- [ ] Tests validating current routing/recovery behavior identified and kept green?
- [ ] `target_model` vs baseline-tier derivation kept as distinct concerns?
- [ ] `promote_once` proven to not mutate ctx (no dirty-on-rollback)?

### Documentation

| Doc | Action | Description |
| --- | ------ | ----------- |
| `src/loop_engine/CLAUDE.md` | Update | Point the "THIRD cross-provider promotion site" note at `promote_once`; note `compute_baseline_model` as the baseline SSoT; note `startup`/`wave_orchestration` modules |
| `src/loop_engine/model.rs` (rustdoc) | Update | Document `compute_baseline_model` as the single baseline home; one line on tier-route discarding the baseline model for a provider hint |
| `CLAUDE.md` (project) | Update | Add `startup.rs` / `wave_orchestration` to the subsystem map if a module-level CLAUDE.md is warranted |
| task-mgr learnings | Create | Record the divergence fix + `promote_once` extraction on completion (`/compound` after `/review-loop`) |

---

## 7. Open Questions

- [ ] WS-2.2 (reject model strings starting with `-`): include now or drop? Plan
      marks it optional/include-if-cheap. Default: include (one-liner + test).
- [ ] `LoopInitContext` shape (WS-3.2): return a struct vs a tuple — decide
      during implementation based on how many fields `run_loop` threads forward.
- [ ] Should WS-3.2/3.3 land before or after the smaller fixes merge? Plan
      sequences them **last** to minimize rebase churn; confirm at task-gen time.

---

## Appendix

### Related Documents

- Approved plan: `/home/chris/.claude/plans/yes-validate-as-needed-curious-crown.md`
- `src/loop_engine/CLAUDE.md` — routing precedence, fallback contract, reactions
  framework, Codex integration, parallel-slot defenses.
- Learnings: [4418], [4049], [3057], [4393], [4396], [4378], [4561], [4553], [4532], [4537], [4473].

### Glossary

- **Baseline model**: the Claude model a task would run on absent any
  provider/tier remap — `difficulty=high → OPUS_MODEL`, else first non-blank of
  prd/project/user defaults.
- **Baseline tier**: `model_tier(baseline_model)` → `low`/`standard`/`high`; the
  key `baselineTierRoutes` matches on.
- **PendingPromotion**: a staged cross-provider promotion (DB write done, ctx
  mutation deferred) applied via `apply_pending_promotion` after commit.
- **promote_once**: the new primitive owning the idempotency guard +
  `PendingPromotion` construction (not the apply).
