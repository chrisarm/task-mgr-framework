# PRD: Reactions Framework — Converge Sequential & Wave Post-Claude Reactions

**Type**: Refactor (internal platform) + embedded Bug Fix (#6 rate-limit)
**Priority**: P1 (High) — a recurring bug class that has already shipped three production incidents; the latest strands and resets in-flight work.
**Author**: Claude Code
**Created**: 2026-05-27
**Status**: Draft

> **Filename note**: the original `/prd` arg named `tasks/prd-unify-sequential-and-wave-execution.md`, but that path already holds an **earlier, completed** PRD (the FEAT-010 prompt-builder split + shared `iteration_pipeline`, i.e. convergence incident #1). This PRD is the *next* convergence effort (the reactions framework) and is filed separately to avoid clobbering that record.

**Authoritative spec:**
- Plan: `~/.claude/plans/what-can-we-do-breezy-shore.md`
- First review (scope decisions): `tasks/grok-review-convergence-plan.md`
- Enforcement spike (SPIKE-001): `tasks/grok-review-spike-enforcement.md`

---

## 1. Overview

### Problem Statement

The loop engine has two execution paths — **sequential** (`iteration.rs::run_iteration` + `orchestrator.rs::run_loop` post-processing) and **parallel/wave** (`wave_scheduler.rs::run_wave_iteration` + `slot.rs`, used when `parallel_slots > 1`). Main-thread post-Claude *reactions* (rate-limit waits, crash escalation, the usage gate, overflow recovery, human-review) were implemented at the call sites of one path and silently omitted or shaped differently in the other. The result is a recurring, high-severity bug class: **behavioral drift between the two paths.**

Three incidents to date:
1. Wave skipped learning extraction / bandit feedback / the already-complete completion fallback → fixed reactively by the shared `iteration_pipeline::process_iteration_output` (learnings #2111, #2224, #2286).
2. Wave's empty-group path lacked the sequential all-complete + recovery handling → false `"no eligible tasks after 3 consecutive stale iterations"` aborts → fixed reactively by `classify_drained_queue`.
3. **(current)** Rate-limit / session-limit waiting exists **only** in sequential. In wave mode every slot returns the limit message instantly, the wave never waits, tasks strand `in_progress`, the todo pool drains, and the loop false-aborts with the same `3 consecutive stale iterations` message — resetting all in-progress work.

### Background

The first two fixes built shared seams reactively. This effort makes convergence **systematic**: a single `src/loop_engine/reactions/` module becomes the physical home for all non-path-specific main-thread post-Claude behavior, both paths route through it, and a compile-time enforcement mechanism makes re-inlining into one path a hard build error. The hard constraint that forces a "shared helper + wave folds N results" design (rather than "call `run_iteration` from a slot") is that `rusqlite::Connection` is `!Send` and slot workers never touch `&Connection`/`&IterationContext` — but every reaction runs on the main thread after `run_parallel_wave` joins, so reactions *can* be shared.

---

## 2. Goals

### Primary Goals

- [ ] Create `src/loop_engine/reactions/` as the single physical home for the converged reactions (functions move here; `recovery.rs`/`usage.rs`/`overflow.rs` slim to near-zero with transition-only deprecated shims).
- [ ] Fix bug #3: wave mode waits on rate/session limits exactly like sequential.
- [ ] Converge **and lock** all six audit items: **#2** crash escalation (incl. effort overrides), **#3** pre-iteration usage gate, **#5** overflow recovery, **#6** rate-limit wait, **#10** human-review, **#13** budget accounting.
- [ ] Implement the compile-time enforcement mechanism so calling a relocated leaf from `iteration.rs`, `wave_scheduler.rs`, or `slot.rs` is a compile error.
- [ ] Stand up `tests/reaction_parity.rs` covering each reaction across sequential (1-item) and wave (N-item, incl. mixed) shapes.

### Success Metrics

- **Bug class closed**: all six items converged + locked; re-inlining produces a `cargo build` failure (demonstrated by a temporary re-inline reverting to red).
- **No regression**: full `cargo test` + `cargo clippy -- -D warnings` green; existing `recovery.rs`/`overflow.rs`/`iteration_pipeline_parity.rs` tests stay green through the relocation.
- **E2e**: a `parallel_slots: 2` loop hitting an injected session limit *waits and resumes* instead of false-aborting.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Wait-once semantics**: a wave with N rate-limited slots performs the usage wait **exactly once** (account-global — one Claude account, one reset window). N sequential 5-hour waits must be structurally impossible.
- **Completion durability before reset (B1)**: the `in_progress→todo` reset of rate-limited tasks must never revert a task that completed in the same wave. Safe only because `process_slot_result` flips completed slots to `done` *before* the reaction runs (`WHERE status='in_progress'` skips `done`). This ordering is load-bearing.
- **Merge-fail streak preservation (B3)**: a rate-limit early return must NOT zero `ctx.consecutive_merge_fail_waves` (the cascade-halt defense). `apply_merge_fail_reset_and_halt_check` resets the streak on empty `failed_merges` (`wave_scheduler.rs:738-739`).
- **Budget accounting (B2 / #13)**: a `WaitedAndRetry`/`RateLimit` wave must not consume an iteration against `max_iterations`. Sequential decrements the loop-bound counter (`iteration -= 1`, `orchestrator.rs:1256`); the wave branch currently only skips the `iterations_completed` *stat* (`orchestrator.rs:1021-1023`), so it must additionally give back the loop-bound `iteration` (`orchestrator.rs:918-919`).
- **Transactional-promotion integrity**: relocating crash escalation (#2) and overflow (#5) must preserve the deferred-promotion-after-commit contract (CLAUDE.md §"Transactional promotion ctx writes are deferred") and the operator escape valve (`check_override_invalidation`).
- **Signal/stop during wait**: a `.stop` file or SIGINT during the wait yields `AccountReaction::Stop` → wave terminal exit 130 with `output: String::new()` semantics (matching sequential `iteration.rs:674-685`).

### Performance Requirements

- Best effort. Reactions run on the main thread once per iteration/wave; no hot-path concern. The wait is bounded (`MAX_WAIT_SECS` = 5h) and probes every `PROBE_INTERVAL_SECS` via `probe_rate_limit_lifted`.

### Style Requirements

- Follow existing codebase patterns. **Mandatory**: all five coordinator functions destructure their param structs exhaustively (no `..` rest pattern), per the `ProcessingParams` precedent — adding a field becomes a compile error in both call sites.
- Enforcement (SPIKE-001, RESOLVED): `#![deny(deprecated)]` at the top of **`iteration.rs`, `wave_scheduler.rs`, AND `slot.rs`**; `#[deprecated(note = "use reactions::… — direct calls are a compile error")]` on relocated leaves; targeted `#[allow(deprecated)]` only at legitimate transition sites (engine re-exports, tests). Established pattern (`recovery.rs:428`, `slot.rs:336`).
- No `.unwrap()` on fallible paths; preserve existing error propagation. Best-effort observability writes (overflow dumps/JSONL) keep the `eprintln!`-and-never-propagate pattern.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --------- | -------------- | ----------------- |
| Mixed wave: 1 slot completed, 1 rate-limited | Reset must not revert the completed task | Completed task stays `done`; rate-limited task → `todo`; wait fires once |
| 3-slot wave, 2 rate-limited | Naive per-slot handling → 2× sequential waits | Single wait; both tasks reset |
| Rate-limit message as slot output flows through pipeline | Pipeline runs before reaction in wave | Limit string produces zero learnings / completions / status-updates |
| Unparseable reset time in output | `parse_reset_from_output` → `None` | Both paths fall back to `fallback_wait` (identical) |
| Rate-limit wave while merge-fail streak = 1 | Early return with empty `failed_merges` | Streak preserved (not reset to 0); next merge-fail reaches threshold and halts |
| Persistent (never-lifting) rate limit | Wait could loop without consuming budget | Bounded by `MAX_WAIT_SECS` + `.stop`/signal breaks it (same as sequential today) |
| Human-review fires on a `WaitedAndRetry` wave | Other slots still rate-limited; ephemerals unmerged | Allowed & intentional (§3.3); interactive session blocks; documented |
| `slot.rs:492` direct `handle_prompt_too_long` call | Was a drift loophole | RESOLVED: routes through `reactions::post_output::handle_overflow`; slot.rs also carries the deny |

---

## 2.6. Boundary Contracts & Modularity Targets

### CONTRACT-001 — `reactions::` module boundary + enforcement + harness skeleton

The foundational abstraction every FEAT below depends on. `taskType: "contract"`. Acceptance criteria:

- `src/loop_engine/reactions/{mod,pre_spawn,account,post_output,post_completion}.rs` exist and are registered in `loop_engine/mod.rs`.
- The five coordinator entry points exist with the signatures in §6 Public Contracts: `resolve_task_execution`, `account_usage_gate`, `react_to_outputs`, `handle_overflow`, `react_to_completions`.
- Each coordinator destructures its param struct exhaustively (no `..`).
- Enforcement active: `#![deny(deprecated)]` on **`iteration.rs` + `wave_scheduler.rs` + `slot.rs`**; relocated leaves carry `#[deprecated]`. A demonstration that calling a relocated leaf from any of the three files fails `cargo build`.
- `tests/reaction_parity.rs` skeleton present with the harness fixtures (two independent DBs, `TASK_MGR_NO_EXTRACT_LEARNINGS=1`, public-API setup) and at least structural + negative-control cases passing.
- "Reaction framework (shared)" section written in `src/loop_engine/CLAUDE.md`, referencing CONTRACT-001; "rate-limit waits" removed from the "Out of scope (kept at call sites)" lists in CLAUDE.md AND `iteration_pipeline.rs:45-50` (same commit).
- The six target items declared `dependsOn: CONTRACT-001`.

Wire all FEAT/FIX dependents with `--depended-on-by CONTRACT-001`.

---

## 3. User Stories

### US-001: Wave-mode loops survive session limits
**As an** operator running a `parallel_slots > 1` loop overnight
**I want** the loop to wait for the usage reset when a session limit hits
**So that** it resumes and finishes instead of false-aborting and resetting all my in-progress tasks.

**Acceptance Criteria:**
- [ ] A wave where slots return "You've hit your session limit" triggers the usage wait (probe logs visible) and resumes.
- [ ] No `"no eligible tasks after 3 consecutive stale iterations"` abort occurs for a rate-limit cause.
- [ ] In-flight tasks are reset to `todo` (not stranded `in_progress`) and re-run after the wait.

### US-002: Engine maintainers cannot silently diverge the paths again
**As a** maintainer adding a new post-Claude reaction
**I want** the compiler to reject inlining it into only one path
**So that** the next reaction can't become bug #4.

**Acceptance Criteria:**
- [ ] Re-inlining a relocated leaf into `iteration.rs`/`wave_scheduler.rs`/`slot.rs` fails `cargo build`.
- [ ] `tests/reaction_parity.rs` exercises every converged reaction on both path shapes.
- [ ] Adding a field to a coordinator param struct breaks compilation at both call sites until both are updated.

### US-003: Wave mode reaches behavioral parity on all six items
**As a** maintainer
**I want** crash escalation (+effort), the usage gate, overflow recovery, rate-limit, human-review, and budget accounting to behave identically across paths
**So that** the loop's behavior is independent of `parallel_slots`.

**Acceptance Criteria:**
- [ ] Each of #2/#3/#5/#6/#10/#13 routes through a single `reactions::` coordinator called by both paths.
- [ ] Parity tests pass for each.

---

## 4. Functional Requirements

### FR-CONTRACT-001: Reactions module + enforcement + harness
See §2.6. Foundational; blocks all FEATs.

### FR-002: Converge pre-spawn task execution (audit #1, #2, #6-effort)
`reactions::pre_spawn::resolve_task_execution` folds override invalidation (#1), crash escalation (#2), **model AND effort overrides** (§3.6 hard requirement), and review-model routing into one coordinator. Sequential calls once (`iteration.rs:363-419`); wave calls once per slot in its pre-spawn loop (`wave_scheduler.rs:981-1011`). Preserves the deferred-promotion contract and operator escape valve.

**Validation:** parity test — identical `TaskExecutionPlan` (model, effort, runner) for the same `(ctx, task, prompt_result, config)` on both paths; existing crash-escalation + override-invalidation tests unchanged.

### FR-003: Converge the pre-iteration usage gate (audit #3)
`reactions::pre_spawn::account_usage_gate` runs the pre-spawn usage check (`iteration.rs:114-142`) on both paths; wave gains the gate it currently lacks.

**Validation:** wave performs the pre-iteration usage check; gate decision identical to sequential for the same usage state.

### FR-005: Relocate overflow recovery as its own reaction (audit #5)
`reactions::post_output::handle_overflow` becomes the single home for `handle_prompt_too_long` (relocated from `overflow.rs`/call sites). NOT folded into `process_iteration_output`. **All three call sites route through it: `iteration.rs:714`, `wave_scheduler.rs` (via `process_slot_result`), AND `slot.rs:492`** (FR-OQ resolved — no leaf call escapes the framework). Preserves the five-rung ladder, prompt dumps, JSONL events, rotation, FallbackToProvider transactional promotion, and `sanitize_id_for_filename`. Ordering relative to the pipeline is a written CLAUDE.md contract.

**Validation:** existing overflow tests green; all three paths call the coordinator; learning #2852 evolution honored (no regression to per-path bypass).

### FR-006: Converge the rate-limit wait — fixes bug #3 (audit #6)
`reactions::account::react_to_outputs(conn, &[OutputReactionItem], &AccountReactionParams) -> AccountReaction` folds N `(outcome, output)`, detects `RateLimit` across the slice, resets affected tasks `in_progress→todo` (`recover_in_progress_for_prefix`), and performs the wait **once** (reusing `usage::check_and_wait` → `parse_reset_from_output` + `wait_for_usage_reset` with the `probe_rate_limit_lifted` probe). `react_to_outputs_inner` takes the wait as an injected `&dyn Fn` for hermetic tests.

- Sequential: replace `iteration.rs:621-688` with a one-item call (`Stop`→`should_stop` early return; `WaitedAndRetry`→fall through).
- Wave: insert after the `process_slot_result` loop (`wave_scheduler.rs:1024`), before `handle_task_failure`/crash/merge-back. On `WaitedAndRetry` return early honoring **B1** (durability), **B2** (give back loop-bound `iteration`), **B3** (preserve merge-fail streak), surfacing `tasks_completed: agg.tasks_completed`. On `Stop` return terminal exit 130.
- Thread `usage_params: &UsageParams` into `WaveIterationParams` (`engine.rs:548-595`) wired from `orchestrator.rs:991`.

**Validation:** wait-once, mixed-wave-durability, RateLimit-output-inert, parse-fail-fallback, and budget parity tests (see §Parity harness); e2e repro extending `tests/wave_runtime_error_fallback.rs`.

### FR-010: Converge human-review (audit #10)
`reactions::post_completion::react_to_completions` (taking the already-computed completed-id set as input) folds human-review (#10) + external-git (#9); wrapper-commit (#8) behind a `wrapper_commit: bool` knob. Wave gains the human-review trigger. **This is an intentional behavior addition** to wave mode — `requires_human` tasks now spawn review sessions in wave mode, and review may fire on a partial `WaitedAndRetry` wave (other ephemerals stay unmerged; the interactive session blocks). Documented as intentional. Input-driven so the wave's intra-wave ordering (post-merge reconcile `wave_scheduler.rs:1172` before external-git shadow `:1196`) is preserved.

**Validation:** human-review fires on both paths for the same completed `requires_human` task; intra-wave ordering preserved; mixed-wave human-review parity case passes.

### FR-013: Shared budget-accounting helper (audit #13)
A single helper owns the `iteration_consumed ↔ iteration -= 1` mapping so a rate-limit/Reorder wave does not consume `max_iterations` and the two paths cannot drift on budget. Generalizes the B2 fix.

**Validation:** `WaitedAndRetry`/`RateLimit` ⇒ no `max_iterations` consumption on both paths; persistent-rate-limit termination test (exits on signal/`.stop`, not unbounded budget).

### FR-CLEANUP-001: Remove transition shims
After all six items route through `reactions::`, delete the `#[deprecated]` shims in `recovery.rs`/`usage.rs`/`overflow.rs` and slim those modules to near-zero (RESOLVED: no permanent dual-home). Audit all `#[allow(deprecated)]` sites; remove those no longer needed. `dependsOn` the six FEATs.

**Validation:** no `#[deprecated]` shim for a relocated leaf remains; `cargo build` + full test suite green; `git grep` shows relocated leaves defined only under `reactions::`.

---

## 5. Non-Goals (Out of Scope)

- **Merging the two paths into one function** — Reason: blocked by the `!Send` Connection constraint; convergence is shared-helpers-with-folding, not unification.
- **Changing slot threading / worktree model** — Reason: orthogonal; high risk.
- **Converging genuinely path-specific behavior** (slot merge-back, merge resolver, post-merge reconcile, worktree hygiene — audit #15) — Reason: requires working-tree state only the wave path has.
- **Preventing copy-paste of reaction *logic*** (vs. calls) — Reason: SPIKE-001 §3.2 limited enforcement to blocking calls to relocated leaves; logic-copy detection rejected as over-engineering.
- **Fixing cross-run prefix collision** — Reason: pre-existing, unsupported (per-worktree DB); not introduced here.
- **Re-doing the iteration_pipeline / prompt-builder unification** — Reason: already shipped (the earlier `prd-unify-sequential-and-wave-execution.md`); this PRD builds on it.

---

## 6. Technical Considerations

### Affected Components

- `src/loop_engine/reactions/` *(new)* — `mod.rs`, `pre_spawn.rs`, `account.rs`, `post_output.rs`, `post_completion.rs`. Physical home of relocated leaves.
- `src/loop_engine/iteration.rs` — replace Step 1.5 (`114-142`), Step 4.5 (`363-419`), Step 7.5 (`621-688`) with coordinator calls; add `#![deny(deprecated)]`.
- `src/loop_engine/wave_scheduler.rs` — pre-spawn loop (`981-1011`); `react_to_outputs` call after `process_slot_result` (`1024`); B3 streak preservation at the boundary (`738-739` interaction); add `#![deny(deprecated)]`.
- `src/loop_engine/slot.rs` — route `slot.rs:492` overflow call through `handle_overflow`; align per-slot pre-spawn resolution; add `#![deny(deprecated)]`.
- `src/loop_engine/orchestrator.rs` — route post-completion block (`1178-1250`) through `react_to_completions`; thread `usage_params` (`991`); B2/#13 budget helper (`1253-1264`).
- `src/loop_engine/engine.rs` — add `usage_params` to `WaveIterationParams` (`548-595`); preserve `pub use` re-exports (apply `#[allow(deprecated)]` as needed).
- `src/loop_engine/recovery.rs`, `usage.rs`, `overflow.rs` — relocate leaf logic to `reactions::`; transition-only deprecated shims, removed by FR-CLEANUP-001.
- `src/loop_engine/CLAUDE.md`, `iteration_pipeline.rs:45-50` — "Reaction framework (shared)" section; reclassify the "Out of scope" list.
- `tests/reaction_parity.rs` *(new)*; extend `tests/wave_runtime_error_fallback.rs` for the e2e.

### Dependencies

- Internal: `TaskLifecycle` verbs (status mutations SSoT), `usage::*` primitives, `classify_drained_queue`, `process_iteration_output`, `apply_merge_fail_reset_and_halt_check`, `probe_rate_limit_lifted`.
- External: none new.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| -------- | ---- | ---- | -------------- |
| A. Thin facade `reactions/` (coordinators call existing leaves in place) | Smallest diff; low relocation risk | "Single home" is fictional; leaves still callable directly → weak enforcement | Rejected (§3.1 chose physical home) |
| B. **Physical home + `#[deprecated]`/`#[deny]` enforcement + exhaustive destructure** | Real single home; compile-time bypass prevention; matches crate's deprecation culture & `ProcessingParams` precedent | Large first increment; transition `#[allow(deprecated)]` noise | **Preferred** (SPIKE-001 + §3 decisions) |
| C. Sealed-trait / marker-type enforcement | Strong invariant | High complexity; poor fit; high maintenance | Rejected (SPIKE-001 §3) |

**Selected Approach**: B. Physical relocation into `reactions/`, enforced by `#![deny(deprecated)]` scoped to the three engine files + `#[deprecated]` on relocated leaves (primary), with exhaustive param-struct destructure on the five coordinators (secondary hygiene). All-or-nothing first increment (no tactical standalone PR).

**Phase 2 Foundation Check**: Strongly favorable (1:10+). The physical-home + compile-time-lock costs more now (relocation + transition shims) but permanently closes a bug class that has already cost three reactive fixes; each future reaction becomes a one-place change protected by the compiler.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| ---- | ------ | ---------- | ---------- |
| Relocating transactional logic (`handle_prompt_too_long` dumps/JSONL/rotation, deferred promotion) breaks an invariant | High | Med | Relocate behind unchanged signatures first, then move call sites; keep `recovery.rs`/`overflow.rs` tests green throughout |
| B3 merge-fail streak silently wiped by rate-limit early return | High | Med-High | Preserve `consecutive_merge_fail_waves`; dedicated test (merge-fail → rate-limit → merge-fail reaches threshold) |
| B2 budget drift / unbounded loop | Med | Med | Shared #13 accounting helper; persistent-rate-limit termination test |
| Very large first deliverable (all-or-nothing) → review/rebase risk | Med | High | Tightly-stacked task sequence under CONTRACT-001; review the contract in isolation first |
| `#[deny(deprecated)]` transition noise across tests / engine re-exports | Med | Med | Narrow deny to the three engine files only; targeted `#[allow(deprecated)]`; aggressive shim cleanup in FR-CLEANUP-001 |
| Human-review now firing on partial waves surprises operators | Low | Low (decision accepted §3.3) | Docs + banner noting other slots in flight |

#### Top 3 Risks (Impact × Likelihood)
1. **B3 merge-fail streak wipe** (High × Med-High) — silently disables the cascade-halt defense. Mitigated by streak preservation on the early return + dedicated test.
2. **Relocating transactional overflow/promotion logic** (High × Med) — could break the deferred-promotion contract. Mitigated by relocate-behind-signature-first + keeping existing tests green.
3. **Large all-or-nothing first deliverable** (Med × High) — review/rebase risk. Mitigated by stacked tasks under CONTRACT-001, contract reviewed in isolation.

### Security Considerations

- No new external surface. `sanitize_id_for_filename` (path-traversal defense in overflow dumps) must survive relocation unchanged.

### Public Contracts

#### New Interfaces

| Module/Function | Signature | Returns (success) | Returns (error) | Side Effects |
| --------------- | --------- | ----------------- | --------------- | ------------ |
| `reactions::pre_spawn::resolve_task_execution` | `(&mut IterationContext, &Connection, &str task_id, &PromptResult, &ProjectConfig)` | `TaskExecutionPlan { model, effort, runner }` | infallible (logs) | Mutates ctx override maps; escalation stderr |
| `reactions::pre_spawn::account_usage_gate` | `(&UsageParams, &Path tasks_dir, &PermissionMode)` | `GateDecision` | — | May block (pre-spawn wait) |
| `reactions::account::react_to_outputs` | `(&mut Connection, &[OutputReactionItem], &AccountReactionParams)` | `AccountReaction { None \| WaitedAndRetry \| Stop }` | — | Resets tasks `in_progress→todo`; blocks for the wait once |
| `reactions::post_output::handle_overflow` | *(finalized in FEAT-005; mirrors `handle_prompt_too_long` args incl. `Option<slot_index>`)* | recovery action | — | DB UPDATE (model/status), ctx override maps, dump/JSONL/rotation |
| `reactions::post_completion::react_to_completions` | `(&mut Connection, &[String] completed_ids, &PostCompletionParams)` | `()` | — | External-git reconcile, human-review spawn, optional wrapper-commit |

#### Modified Interfaces

| Type | Current | Proposed | Breaking? | Migration |
| ---- | ------- | -------- | --------- | --------- |
| `WaveIterationParams<'a>` | no `usage_params` | add `pub usage_params: &'a UsageParams` | No (internal struct) | Wire at `orchestrator.rs:991` |
| `recovery::check_crash_escalation`, `usage::check_and_wait`/`wait_for_usage_reset`, `overflow::handle_prompt_too_long`, `trigger_human_reviews` | callable from orchestrators + slot.rs | relocated to `reactions::`; transition `#[deprecated]` shims (removed by FR-CLEANUP-001) | Yes (intentional compile error from the three files) | Route via coordinators; `#[allow(deprecated)]` at transition sites |

### Data Flow Contracts

N/A — no new cross-module data structure with non-obvious key types. Reactions consume existing typed structs (`IterationContext`, `UsageParams`, `IterationResult`/`SlotResult`) and slices thereof; access is direct field access on typed Rust structs.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --------- | ----- | ------ | ---------- |
| `iteration.rs:114-142,363-419,621-688` | inline reactions | BREAKS (intentional) | Replace with coordinator calls |
| `wave_scheduler.rs:981-1011,1024,1052,738-739` | pre-spawn + post-slot + halt check | NEEDS REVIEW | Add coordinator calls; preserve streak (B3) |
| `slot.rs:492` | direct `handle_prompt_too_long` | BREAKS (intentional, FR-OQ resolved) | Route through `handle_overflow` |
| `orchestrator.rs:1178-1250,1253-1264` | post-completion + budget | NEEDS REVIEW | Route via `react_to_completions`; shared budget helper |
| `engine.rs` `pub use` re-exports | external test paths (FR-008) | NEEDS REVIEW | Keep functional; `#[allow(deprecated)]` |
| `tests/*` calling relocated leaves | unit/integration tests | NEEDS REVIEW | `#[allow(deprecated)]` or repoint to coordinators |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --------- | ------- | ---------------- | --------------------- |
| Per-task overflow (#5) | each task, own model/effort ladder | per-task recovery | Stays per-task (own reaction), NOT account-global; all 3 call sites route through it |
| Account-global rate-limit (#6) | all slots share one account | sequential-only | Folds N slot outcomes → one wait |
| Wrapper-commit (#8) | sequential base branch vs wave ephemeral | sequential commits on behalf; wave merges back | `wrapper_commit: bool` knob; wave keeps merge-back |

### Inversion Checklist

- [ ] All callers of relocated leaves identified (incl. `engine.rs` re-exports, tests, `slot.rs:492`)?
- [ ] Budget/branching decisions depending on `iteration_consumed` reviewed (B2/#13)?
- [ ] Tests validating current per-path behavior identified and repointed or `#[allow]`-ed?
- [ ] Merge-fail streak interaction (B3) covered by a test?
- [ ] Completion-durability-before-reset (B1) pinned by a mixed-wave test?

### Documentation

| Doc | Action | Description |
| --- | ------ | ----------- |
| `src/loop_engine/CLAUDE.md` | Update | New "Reaction framework (shared)" section; reclassify "Out of scope" list; document human-review-on-partial-wave + `iteration_consumed=false` invariants + `handle_overflow`↔pipeline ordering |
| `src/loop_engine/iteration_pipeline.rs:45-50` | Update | Remove "rate-limit waits" from the "Out of scope (kept at call sites)" rustdoc list |
| `tasks/grok-review-convergence-plan.md`, `tasks/grok-review-spike-enforcement.md` | Reference | Authoritative decisions; do not edit |

---

## 7. Open Questions

- [x] **FR-OQ** *(RESOLVED 2026-05-27)*: `slot.rs:492` (`overflow::handle_prompt_too_long`) **must route through** `reactions::post_output::handle_overflow`. #5 overflow is genuinely single-home — no leaf call escapes the framework. FEAT-005 relocates this call site too.
- [x] *(RESOLVED 2026-05-27)*: `recovery.rs`/`usage.rs`/`overflow.rs` are **slimmed to near-zero**: logic moves into `reactions::`; thin `#[deprecated]` shims exist only during transition and are **removed in FR-CLEANUP-001**. End state: `reactions::` is unambiguously the home; no permanent dual-home.
- [x] *(RESOLVED 2026-05-27)*: `#![deny(deprecated)]` is applied to **all three engine files** — `iteration.rs` + `wave_scheduler.rs` + `slot.rs` — consistent with routing slot.rs through the coordinator. Strongest lock; no slot.rs drift loophole.

*(No open questions remain. All three resolved 2026-05-27; folded into CONTRACT-001, FR-005, FR-CLEANUP-001, and the §2.5 enforcement scope.)*

---

## Appendix

### Related Documents
- Plan: `~/.claude/plans/what-can-we-do-breezy-shore.md`
- Reviews: `tasks/grok-review-convergence-plan.md`, `tasks/grok-review-spike-enforcement.md`
- Predecessor (shipped): `tasks/prd-unify-sequential-and-wave-execution.md` (iteration_pipeline + prompt-builder unification)
- Precedent: `src/loop_engine/iteration_pipeline.rs`, `tests/iteration_pipeline_parity.rs`

### Related Learnings
- #2286, #2300, #2157, #2111, #2224 — prior parity convergence (iteration_pipeline).
- #2852 — Wave-mode PromptTooLong recovery evolution (bypass → shared ladder); directly relevant to #5.
- #2136 — wave-mode conversation field not threaded (a prior drift instance).

### Glossary
- **Reaction**: a main-thread post-Claude behavior (rate-limit wait, crash escalation, usage gate, overflow recovery, human-review) that is not slot/worktree-specific.
- **Account-global reaction**: a reaction that must fire once per wave because all slots share one Claude account (rate-limit).
- **Coordinator**: one of the five public `reactions::` entry points both paths call.
- **WaitedAndRetry / Stop**: `AccountReaction` variants — the wait completed (re-run) vs. interrupted by stop/signal (terminate).
