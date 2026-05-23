# PRD: TaskLifecycle Extraction (Phase 1, PRD 1 of 2)

**Type**: Refactor
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-05-19
**Status**: Draft
**Design doc**: `docs/designs/coherence-refactoring.md` (Phase 1, Item 1)
**Sibling/follow-up PRD**: "Engine Orchestration Boundaries" (Phase 1, PRD 2 of 2 — not yet authored; runs **after** the dogfood gate defined in §7)
**Related parallel effort**: `docs/designs/runner-trait-hygiene.md` — see §6 "Boundary Contract"

---

## 1. Overview

### Problem Statement

`tasks.status` is mutated by roughly **20 raw `UPDATE tasks SET status …` SQL sites** scattered across the codebase, split as follows (verified count, supersedes the design-doc estimate of ~15):

- **7 user-facing command modules** (`complete`, `fail/transition`, `skip`, `irrelevant`, `unblock`, `reset`, `review`)
- **1 race-safe pre-claim site** (`commands/next/mod.rs`)
- **~14 loop-side recovery sites** (12 in `loop_engine/engine.rs`, 2 in `loop_engine/overflow.rs`)
- **~4 reconcile / PRD-driven sites** (`loop_engine/prd_reconcile.rs`, `commands/doctor/fixes.rs`)
- **1 bootstrap site** (`commands/init/mod.rs`)

`TaskStatus::can_transition_to` in `src/models/task.rs:78` is documented as the single source of truth, but is currently consulted by **only 2 of those ~20 sites** (`complete.rs:199`, `fail/transition.rs:123`). The "SSoT" is aspirational, not enforced.

The same cluster of side effects — `run_tasks` row bookkeeping, PRD JSON sync (`update_prd_task_passes`), notes / `last_error` / `blocked_at_iteration` / `started_at` / `completed_at` column writes, audit logging — is partially duplicated, partially missing, and partially inlined at each site. Behavioral drift between sites is silent and is caught only by integration tests when a particular code path is exercised under specific conditions (see learnings #2284, #2238, #2304, #2796 — every one of those bug surfaces lives at a status-write site).

### Background

The design doc `docs/designs/coherence-refactoring.md` identifies six refactoring directions; the highest-ROI is the **TaskLifecycle service** (Item 1). It is the prerequisite for the planned "Engine Orchestration Boundaries" carve (Item 2) — without a stable lifecycle seam to call into, breaking up `engine.rs` (currently 9,644 lines) just moves the same scattered SQL into smaller files.

`src/loop_engine/iteration_pipeline.rs` (537 lines) is the design template: a single typed function with explicit pipeline steps, called from both sequential (`engine.rs` ~L3204) and wave (`engine.rs` ~L1166) paths. Learnings #2065, #2086, #2286 document the value of that unification. TaskLifecycle is the larger version of that win for the *status-mutation* axis.

The codebase is dogfooded daily — the maintainer runs `task-mgr loop` against in-progress PRDs in this repo. A refactor that touches `apply_status_updates`, the command modules, and the iteration pipeline simultaneously is one bad merge away from corrupting a live PRD's DB. The "transition shadow test" harness and the post-PRD dogfood gate (§7) are the safety net.

---

## 2. Goals

### Primary Goals

- [ ] Collapse all 20 status-write sites in the audit table into a small named surface (TaskLifecycle service) — except Category E (bootstrap), which stays out by design.
- [ ] `TaskStatus::can_transition_to` (or its successor inside the service) is the **only** code path that decides whether a transition is legal. No raw SQL `WHERE status = …` predicate in production code may write a new status without flowing through the service.
- [ ] All five **contract-level invariants** in §6 are preserved bit-identically — verified by the transition shadow test harness.
- [ ] The next PRD (Engine Orchestration Boundaries) can call a single stable interface for every status change in the engine, instead of inlined SQL.
- [ ] Dogfood gate (§7): `task-mgr loop` runs continuously for **N = 10 iterations across two distinct in-progress PRDs** on the refactored code with **zero** loop-internal regressions before the engine-carve PRD is spawned.

### Success Metrics

- **Site count**: ≤ 3 production-code call sites perform raw `UPDATE tasks SET status …` after this PRD lands (the Category E init write, and at most 2 documented exceptions with rationale checked into source).
- **Test coverage of recovery paths**: every Category C verb has a unit test (today most are integration-only).
- **Shadow-test green rate**: 100% on the legacy-vs-service DB diff harness across every audited site.
- **Stderr contract**: the `"PRD JSON sync failed"` warning string in `apply_status_updates` is byte-identical pre/post; operators grep for it.
- **Zero regressions in `cargo test`** measured against the baseline test count taken before any extraction work begins (per learning #2807).

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Auto-claim invariant**: When the loop emits `<task-status>TASK-ID:done</task-status>` for a `todo` task without a prior claim, the service MUST silently promote `todo → in_progress` before completing. Refusing this transition would break single-iteration completions. Today: `engine.rs:4730` performs this auto-claim with a side-band `UPDATE`.
- **Per-task partial-failure tolerance**: `apply_status_updates` returns `Vec<(task_id, change, applied: bool)>`. One task's transition failing must NOT abort the iteration. Batch-level `Result<(), Err>` at this entry point is explicitly disallowed (learning #2284).
- **DB authoritative, PRD JSON best-effort**: The DB write happens inside a transaction; the subsequent `update_prd_task_passes` call (`engine.rs:4785`) is best-effort and emits a specific stderr warning on failure. The exact warning shape is part of the public contract (operators grep stderr for it). **`TransitionOutcome.applied = true` reflects DB-write success regardless of PRD JSON sync outcome** — a `Display`-formatted error on stderr is the only externally visible signal of PRD-sync failure; the in-memory return value still reports the transition as applied.
- **`run_tasks` row bookkeeping**: The MAX(iteration)+1 insert (`engine.rs:4739–4748`) currently happens only on the loop path, not on direct command invocations. Post-refactor: the service owns this bookkeeping; the command callers don't need to know `run_tasks` exists. The bookkeeping fires only when a run is active.
- **Conditional-WHERE predicates are part of the API**: `claim_slot_task` at `engine.rs:787` uses `WHERE id = ? AND status IN ('todo', 'in_progress')` for slot-resumption idempotency. The service's `try_claim` API exposes the expected-status set explicitly — it must not be hidden behind an unconditional method.
- **Transition matrix completeness**: every (from_status, to_status) pair in the existing `can_transition_to` table must yield identical accept/reject outcomes from the new service. Verified by an exhaustive matrix unit test.
- **Status-tag completion gate**: the gate at `iteration_pipeline.rs:275-286` checks the *claimed* task's per-task dispatch outcome, not a global `status_updates_applied > 0` count (learning #2238). The service must preserve per-task outcome reporting.
- **Terminal-status pruning**: when a task reaches Done/Failed/Skipped/Irrelevant via the service, callers can still observe the terminal status to prune tracking maps (learnings #2796, #2304). The service does not mutate caller-side maps; it just reports the per-task outcome.

### Performance Requirements

- **No new DB round-trips**: the refactor MUST NOT add a SELECT-before-UPDATE pattern where today's SQL writes via a conditional WHERE. Pre-read for transition validity is acceptable only if a single read covers the entire batch (e.g., one query loads current statuses for all task_ids in an `apply` call).
- **No new transactions**: every existing transaction boundary is preserved. The service does not introduce a wrapping transaction around code that previously ran outside one (this would change failure semantics). A "no new transaction boundaries" lint test (or manual review checklist item) asserts this.
- **Specific latency target**: **median iteration latency for a 5-task FEAT-only loop** measured on `main` immediately before the first PRD-1 commit lands becomes the baseline. Post-refactor median for the same workload must stay within **±10%** of baseline. Recorded in the PRD-2 ("Engine Orchestration Boundaries") sign-off package as well.

### Style Requirements

- Follow existing codebase patterns (rusqlite `Connection`, `?` propagation, `anyhow::Result` at boundaries, internal `Result<…, TransitionError>` for the service surface).
- No `.unwrap()` on `prepare`/`execute` results — propagate with `?`. The only acceptable `.unwrap` is in test code or after a `match` that has already discriminated the variant.
- Service module is `src/lifecycle/` (new crate-internal module at the same level as `commands/`, `loop_engine/`, `models/`). Rationale: it owns task-state semantics that span commands AND loop_engine; placing it inside either would invert the dependency. See §4.6 for the module-name decision matrix.
- Public service struct: `TaskLifecycle`. Public verb methods: `apply`, `try_claim`, `recover_in_progress_for_prefix`, `auto_block_after_failures`, `resurrect_for_iteration`, `reconcile_from_prd`. Names match the design doc verbatim.
- Re-export the previously-public `apply_status_updates` from `loop_engine` (per learning #440) during the transition so wave-mode tests don't need import changes; mark `#[deprecated]` on the re-export.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| `<task-status>TASK-X:done</task-status>` emitted for a `todo` task with no prior claim | Loops complete single-iteration tasks this way; auto-claim is silent today | Service auto-claims `todo → in_progress` then transitions `in_progress → done` in the same transaction. Returns `applied: true`. |
| Two `<task-status>` tags for the same task in one iteration (e.g. `:done` and `:failed`) | Race in LLM output | Apply in emission order. Per-task outcomes returned for each; observer can detect the chain. |
| `apply_status_updates` called with N tasks where task K fails validation | Per-task partial-failure tolerance | Returns `Vec<(id, change, applied)>` of length N. The K-th entry has `applied: false`. The other N-1 still apply and commit. |
| `update_prd_task_passes` fails mid-batch (read-only file, missing PRD JSON) | Common in CI / read-only filesystems | DB transaction still commits. Stderr emits `"PRD JSON sync failed for {task}: {err}"`. Iteration continues. |
| Slot-resumption: same task already in `in_progress` from a prior crash | `claim_slot_task`'s conditional WHERE preserves idempotency | `try_claim(id, &[Todo, InProgress])` succeeds. `try_claim(id, &[Todo])` fails. The contract is explicit. |
| Bulk `recover_in_progress_for_prefix("FEAT-")` when no tasks match the prefix | Common at iteration start when prefix has none in progress | Returns `Ok(0)`. No transaction commit overhead. |
| `auto_block_after_failures` for a task already in `done` (race with manual complete) | Recovery vs. operator intent collision | Service no-ops (transition `done → blocked` is disallowed by the matrix). Returns `applied: false`. No stderr emission — matches legacy `WHERE … AND status = 'in_progress'` 0-row-affected behavior at `engine.rs:5151`. |
| `reconcile_from_prd` plan targets a `done` task with PRD action `irrelevant` | Reconcile is allowed to flip terminal states by design | Service honors the plan via a dedicated `reconcile_apply` path that bypasses the user-facing matrix. Audit-logged. |
| Service called outside a run (no active `runs` row) | Direct CLI invocations (`task-mgr complete X`) | `run_tasks` bookkeeping skipped silently. DB write + PRD sync still happen. |
| `apply` called with empty input slice | Defensive boundary | Returns `Ok(Vec::new())`. No transaction. |

---

## 3. User Stories

### US-001: Maintainer adds a new terminal task status

**As a** task-mgr maintainer
**I want** to add a new terminal status (e.g. `superseded`) by editing one transition matrix and one set of side effects
**So that** I don't have to grep ~20 files and risk forgetting one

**Acceptance Criteria:**

- [ ] Adding a status enum variant + one transition matrix entry + one side-effect arm in `TaskLifecycle::apply` is sufficient for both CLI verbs and loop-side recovery to honor it.
- [ ] No raw `UPDATE tasks SET status = 'superseded'` exists anywhere in production code.

### US-002: Loop-engine author adds a new recovery primitive

**As a** loop-engine author
**I want** to add a new bulk recovery operation (e.g. `re_block_after_overflow_ceiling`) by adding one method to `TaskLifecycle`
**So that** I don't repeat the iteration-claim-guard SQL idiom across `engine.rs` again

**Acceptance Criteria:**

- [ ] All Category C sites in §"Affected Components" delegate to a named service verb.
- [ ] A new recovery verb is tested in isolation (unit test against an in-memory DB) without needing a full iteration harness.

### US-003: Operator runs the loop and observes existing behavior

**As an** operator running `task-mgr loop run` daily
**I want** the loop to behave **bit-identically** post-refactor: same `<task-status>` outcomes, same stderr warnings, same PRD JSON sync timing, same run_tasks rows
**So that** my live PRDs are not corrupted by the refactor

**Acceptance Criteria:**

- [ ] Shadow-test harness asserts byte-identical DB diff for every Category A site between legacy SQL and service call.
- [ ] Snapshot tests on `apply_status_updates` stderr output assert the `"PRD JSON sync failed"` warning shape is unchanged.
- [ ] Dogfood gate (§7) shows 10 successful iterations across two PRDs.

### US-004: CLI command author calls the service

**As a** maintainer adding a new CLI verb (or modifying `complete`/`skip`/`fail`)
**I want** to call `TaskLifecycle::apply(intent)` with a typed intent struct
**So that** I don't need to know about `run_tasks`, `update_prd_task_passes`, `notes` formatting, or `blocked_at_iteration`

**Acceptance Criteria:**

- [ ] `commands/complete.rs`, `commands/fail/transition.rs`, `commands/skip.rs`, `commands/irrelevant.rs`, `commands/unblock.rs`, `commands/reset.rs`, `commands/review.rs` each become thin shells: parse args → build `TransitionIntent` → call service → format output.
- [ ] Each command file shrinks by ≥ 30% (measured by line count, excluding tests).

---

## 4. Functional Requirements

### FR-001: `TaskLifecycle` service exists and owns transition logic

**Module location**: `src/lifecycle/` (sibling of `src/commands/`, `src/loop_engine/`, `src/models/`).

**Public types**:

```rust
pub struct TaskLifecycle<'a> {
    conn: &'a rusqlite::Connection,
    run_id: Option<&'a str>,
    iteration: Option<u32>,
    prd_json_path: Option<&'a std::path::Path>,
    task_prefix: Option<&'a str>,
}

pub struct TransitionIntent {
    pub task_id: String,
    pub target: TaskStatus,
    pub notes: Option<String>,
    pub error: Option<String>,
    pub source: TransitionSource,  // Operator | LoopStatusTag | Recovery | ReconcilePrd
}

pub struct TransitionOutcome {
    pub task_id: String,
    pub target: TaskStatus,
    pub previous: Option<TaskStatus>,
    pub applied: bool,
    pub reason: Option<TransitionRejectReason>,
}
```

**Public methods** (signatures formalized in §6 "Public Contracts"):

- `apply(intents: &[TransitionIntent]) -> Result<Vec<TransitionOutcome>>`
- `try_claim(task_id: &str, allowed: &[TaskStatus]) -> Result<bool>`
- `recover_in_progress_for_prefix(prefix: Option<&str>) -> Result<usize>`
- `auto_block_after_failures(task_id: &str, last_error: &str, iteration: u32) -> Result<bool>`
- `resurrect_for_iteration(prefix: &str, ids: &[String]) -> Result<usize>`
- `reconcile_from_prd(plan: ReconcilePlan) -> Result<ReconcileReport>`
- `repair_stale(plan: RepairPlan) -> Result<RepairReport>` — owns `doctor/fixes.rs` repair sites (time-based heuristic provenance, NOT PRD JSON). See §6 Approaches & Tradeoffs for the rationale on keeping this separate from `reconcile_from_prd`.

**Concurrency model**: `TaskLifecycle` is **per-process, per-connection** (not per-slot). The borrow `&Connection` is held for the lifetime of the value. The implicit invariant — only one `TaskLifecycle` writes to a given `task_id` at a time — is enforced by sqlite row locking, NOT by service-level locking. Wave-mode parallel slots each operate on disjoint task IDs (the slot scheduler guarantees this); the service does not add a guard against violation. Documented as an inherited invariant from the existing slot scheduler.

**Error model**: `anyhow::Error` at the public surface (consistent with the rest of the codebase). Internal validator uses a typed `TransitionRejectReason` enum:

```rust
pub(crate) enum TransitionRejectReason {
    InvalidTransition { from: TaskStatus, to: TaskStatus },
    UnknownTaskId,
    SourceMismatch { from: TaskStatus, to: TaskStatus, source: TransitionSource },
}
```

`TransitionOutcome.reason` is `Option<TransitionRejectReason>` — present on `applied: false`. The validator's typed error is surfaced to callers via this field, not via `Result::Err`. Only infrastructure failures (DB lock, IO error) yield `Result::Err` at the public surface.

### FR-002: Transition matrix is the service's private validator

`TaskStatus::can_transition_to` is moved (or re-exported) into the service module and becomes `pub(crate)` rather than `pub`. The two existing public callers (`commands/complete.rs:199`, `commands/fail/transition.rs:123`) are removed; both now go through the service.

A new exhaustive unit test (`src/lifecycle/tests.rs::transition_matrix_complete`) iterates the cross-product of every **`(from, to, source)` triple** (status × status × `TransitionSource` variant) and asserts the service's accept/reject outcome matches the documented matrix. The triple-not-pair shape is load-bearing: `TransitionSource::ReconcilePrd` is allowed to flip terminal states that `TransitionSource::Operator` cannot, and a `(from, to)`-only test would let regressions land green. The existing tests in `src/models/task.rs:513-571` are moved/duplicated into the service's test module and extended along the source axis.

### FR-003: Side-effect ownership migrates into the service

Per the design doc §"Contract-Level Invariants":

- `run_tasks` row insertion (`engine.rs:4739–4748`) moves into `TaskLifecycle::apply` when `run_id.is_some() && iteration.is_some()`.
- `update_prd_task_passes` invocation (`engine.rs:4785`) moves into `TaskLifecycle::apply` when `prd_json_path.is_some()`.
- The stderr warning on PRD-JSON-sync failure (`engine.rs:4787-4793`) moves with it, byte-identical (asserted by snapshot test).
- Decay column writes (`blocked_at_iteration`, `skipped_at_iteration`) move into the appropriate transition arm.
- `notes` formatting per legacy SQL strings is preserved (the test harness asserts byte-identical notes column values).

### FR-004: All Category A sites become thin callers

The 7 user-facing command modules (`complete.rs`, `fail/transition.rs`, `skip.rs`, `irrelevant.rs`, `unblock.rs`, `reset.rs`, `review.rs`) are converted to:

```rust
let lifecycle = TaskLifecycle::for_cli(&conn);
let intent = TransitionIntent { /* … parsed from args … */ };
let outcome = lifecycle.apply(&[intent])?.pop().unwrap();
// format human-readable output from outcome
```

Each file's raw `UPDATE tasks SET status …` SQL string is deleted. The `notes`, `error_count`, `last_error`, `started_at`, `completed_at`, `blocked_at_iteration` writes that previously lived in the SQL are now the service's job.

### FR-005: Category B site uses `try_claim`

`commands/next/mod.rs:244` becomes `lifecycle.try_claim(&task_id, &[TaskStatus::Todo])?`. The conditional-WHERE predicate is expressed via the `allowed` slice; the service builds the appropriate SQL.

`claim_slot_task` at `engine.rs:787` uses `lifecycle.try_claim(&task_id, &[TaskStatus::Todo, TaskStatus::InProgress])` to preserve slot-resumption idempotency. The expected-status set is the public, documented difference between CLI and slot paths.

### FR-006: Category C sites use bulk recovery verbs

The 12 in-engine sites + 2 overflow sites map to:

| Site | New call |
| --- | --- |
| `engine.rs:1645` (per-task `in_progress → todo`) | `lifecycle.recover_in_progress_for_prefix(Some(prefix))` if scoped, else `recover_in_progress_for_prefix(None)` |
| `engine.rs:2410`, `3264`, `5750`, `5775`, `5811`, `5900`, `6258`, `6284` (bulk `in_progress → todo` with optional prefix) | `lifecycle.recover_in_progress_for_prefix(prefix_opt)` |
| `engine.rs:4730` (auto-claim `todo → in_progress` in `apply_status_updates`) | Internal to `lifecycle.apply` — no separate call site |
| `engine.rs:5151` (`auto_block_task`: `in_progress → blocked` with last_error + blocked_at_iteration) | `lifecycle.auto_block_after_failures(id, err, iter)` |
| `overflow.rs:460` (overflow ceiling: `in_progress → blocked`) | `lifecycle.auto_block_after_failures(id, err, iter)` |
| `overflow.rs:473` (overflow recovery: `in_progress → todo`) | `lifecycle.resurrect_for_iteration(prefix, &[id])` |

The mapping table is **load-bearing** — it is the literal checklist for FR-006 acceptance. If a site doesn't fit a named verb, that is a design-doc gap and requires a written exception in source.

### FR-007: Category D sites split into `reconcile_from_prd` (PRD provenance) and `repair_stale` (heuristic provenance)

`prd_reconcile.rs` keeps the *plan-building* logic (it already produces a structured plan). The *plan-application* moves into the service via two distinct verbs:

- `prd_reconcile.rs:305` (`todo|in_progress → done` from PRD) → `lifecycle.reconcile_from_prd(plan)`.
- `prd_reconcile.rs:550` (`* → irrelevant` from PRD) → `lifecycle.reconcile_from_prd(plan)`.
- `doctor/fixes.rs:30` (stale `in_progress → todo` repair, time-based heuristic) → `lifecycle.repair_stale(plan)`.
- `doctor/fixes.rs:93` (stale `* → done` repair, time-based heuristic) → `lifecycle.repair_stale(plan)`.

Rationale for two verbs instead of folding doctor into reconcile (revised per architect review): doctor and PRD reconcile share *side effects* (run_tasks, audit log, PRD JSON sync) but NOT *provenance*. Doctor is a time-based heuristic ("this row has been `in_progress` for too long without progress"); PRD reconcile is an external-truth check ("the PRD JSON says this task is done"). The service's `apply` core can be shared internally (both produce a list of `(task_id, target)` items and run them through the same side-effect plumbing); the public surface stays distinct so a future maintainer reading `lifecycle.reconcile_from_prd` doesn't have to know it also handles doctor repairs.

`TransitionSource` variants: `Operator`, `LoopStatusTag`, `Recovery`, `ReconcilePrd`, `DoctorRepair` — five variants total. Each carries the rules the validator applies (e.g., `ReconcilePrd` allows `done → irrelevant`; `DoctorRepair` allows `in_progress → todo` and `in_progress → done`; `Operator` follows the strict user-facing matrix).

### FR-008: Transition shadow test harness

A new integration test harness `tests/lifecycle_shadow.rs`:

1. For each audited site, set up an in-memory DB with a representative task in the relevant pre-state.
2. Run the legacy raw-SQL path against a clone of the DB.
3. Run the new service call against another clone of the DB.
4. Diff every column of the `tasks`, `run_tasks`, and (where applicable) PRD JSON file.
5. Assert byte-identical diff (modulo timestamps, which are normalized).
6. **Write-ordering assertion**: For each site where the legacy path performs N writes (e.g., `tasks` UPDATE followed by `run_tasks` INSERT), assert the service performs the writes in the same order. Wave-mode concurrency exposes intermediate states; inverting the order changes observable in-flight state to a second connection.
7. **PRD JSON atomicity test**: For an `apply` call with N intents, the service must not produce a partial PRD JSON state on mid-batch failure that the legacy single-write callers never produced. Cover this with a crash-test: inject a failure in the `update_prd_task_passes` call after item k of N, assert the PRD JSON state is either pre-batch or post-item-k consistent (no torn writes).
8. **Stderr-vs-commit ordering**: snapshot test captures both the stderr line shape AND the commit-vs-stderr order. Operators may rely on "warning appears AFTER the row is durable"; an inverted order would silently break tooling that grep+timestamps.

The harness covers all 5 audit categories. A site is considered "migrated" only when its shadow test is green.

**Known gap (documented, not closed)**: byte-identical post-commit DB diff does NOT catch changes in *observability of in-flight state* — i.e., what a second connection sees during the transaction. The §2.5 "no new transactions" rule plus the write-ordering assertion (point 6 above) are the mitigations; an explicit in-flight observability test is out of scope for this PRD.

### FR-009: Category E (bootstrap) stays out by written exception, lint-enforced

`commands/init/mod.rs:517` retains its raw SQL. A `// LIFECYCLE-EXCEPTION: bootstrap ingest — see tasks/prd-tasklifecycle-extraction.md and docs/designs/coherence-refactoring.md §"TaskLifecycle Scope Decision"` comment is added at that line. This is the only such exception allowed in production code without further discussion.

A **grep-based CI lint** (or a `#[test]` that asserts on a `grep -r "LIFECYCLE-EXCEPTION" src/`) verifies:
- Exactly one `LIFECYCLE-EXCEPTION:` token exists in `src/` outside of test code.
- It lives in `src/commands/init/mod.rs`.

Otherwise the "exactly one exception" promise rots silently when the comment is removed in a future edit. The lint test lives in `tests/lifecycle_exception_lint.rs` and runs as part of `cargo test`.

### FR-010: Pre-Phase-1 coverage gates land first

Before any extraction work begins, land a small preparatory commit (or first task in the list) that:

- Records baseline `cargo test` counts (learning #2807): `cargo test 2>&1 | tee /tmp/baseline.txt | grep "test result"`. Number is recorded in the prompt file.
- Records baseline median iteration latency for a 5-task FEAT-only loop (per §2.5 Performance Requirements). Number is recorded in the prompt file.
- Adds unit tests for the Category C primitives that today only have integration coverage: `recover_in_progress_for_prefix`, `auto_block_after_failures`, `resurrect_for_iteration`, `claim_slot_task` predicate semantics.
- Adds a snapshot test for the stderr warning shape. **Exact format string locked at this PRD's level**: `"PRD JSON sync failed for {task_id}: {err}\n"` using `Display` (NOT `Debug`) on the error chain. The snapshot test asserts this exact prefix + format. If the legacy code currently uses `Debug` somewhere, the snapshot test captures the current bytes verbatim and the service preserves them — the lock is "whatever bytes legacy produces are what the service produces."
- Adds the exhaustive `(from, to, source)` triple matrix test (in the current `task.rs` location for the `(from, to)` portion; extended along the source axis and moved with the matrix later).

---

## 5. Non-Goals (Out of Scope)

- **Engine carve (Item 2)**: Breaking `engine.rs` into `orchestrator.rs` / `iteration.rs` / `wave_scheduler.rs` is the **next** PRD ("Engine Orchestration Boundaries"), executed *after* the dogfood gate. Reason: serializing the two reduces overlap on the critical edit surface.
- **Prompt assembler (Item 3)** and **learnings retrieval refactor (Item 4)**: Phase 2. Out of scope here.
- **Compat shim isolation (Item 5)**: Phase 0; can run before or in parallel; out of scope for this PRD.
- **Event journal (Item 6)**: Research direction; explicitly deferred per design doc.
- **Bootstrap rewrite (Category E)**: Documented exception; stays as-is.
- **Touching the five parallel-slot cascade defenses**: Explicitly preserved. Refactor must not weaken synthetic shared-infra slot, buildy-prefix heuristic, ephemeral overlay, consecutive-merge-fail halt, or stale-ephemeral hygiene. Reason: hard-won safety, documented in `src/loop_engine/CLAUDE.md`.
- **Changing the `<task-status>` side-band tag wire format**: The tag contract is stable across both efforts. Only its *handling* changes.
- **Replacing `apply_status_updates` with a `Result<(), Err>` batch entry point**: Explicitly disallowed. Per-task outcomes are a contract (learning #2284).
- **Adding a wrapping transaction around `apply_status_updates` callers**: Reason: would change failure semantics — preserved as-is.
- **Renaming `TaskStatus` enum variants** or **changing on-disk task status strings**: Reason: DB migration impact; orthogonal to the refactor goal.

---

## 6. Technical Considerations

### Affected Components

**New module**:
- `src/lifecycle/mod.rs` — public surface: `TaskLifecycle`, `TransitionIntent`, `TransitionOutcome`, `TransitionSource`, `TransitionRejectReason`, `ReconcilePlan`, `ReconcileReport`, `RepairPlan`, `RepairReport`.
- `src/lifecycle/matrix.rs` — `pub(crate) fn validate(from: TaskStatus, to: TaskStatus, source: TransitionSource) -> Result<(), TransitionRejectReason>`. Owns the transition table.
- `src/lifecycle/apply.rs` — `apply()` implementation incl. auto-claim, per-task outcomes, `run_tasks` insert, PRD JSON sync, stderr warning. Internal `apply_plan_with_source` helper shared with reconcile/repair paths.
- `src/lifecycle/claim.rs` — `try_claim()` implementation with allowed-status guard.
- `src/lifecycle/recovery.rs` — `recover_in_progress_for_prefix`, `auto_block_after_failures`, `resurrect_for_iteration`.
- `src/lifecycle/reconcile.rs` — `reconcile_from_prd` (consumes a `ReconcilePlan` built by `loop_engine/prd_reconcile.rs`).
- `src/lifecycle/repair.rs` — `repair_stale` (consumes a `RepairPlan` built by `commands/doctor/fixes.rs`; provenance: time-based heuristic).
- `src/lifecycle/tests.rs` — exhaustive `(from, to, source)` transition matrix; per-verb unit tests against in-memory DB.

**Existing files modified** (Category A — 7):
- `src/commands/complete.rs:248` — delete raw SQL, call `TaskLifecycle::apply`.
- `src/commands/fail/transition.rs:83` — same.
- `src/commands/skip.rs:125` — same. **Vertical-slice migration target** (smallest side-effect set; lowest blast radius — see §8).
- `src/commands/irrelevant.rs:136` — same.
- `src/commands/unblock.rs:87, 146` — same.
- `src/commands/reset.rs:78-79` — same (raw `UPDATE tasks SET status = ?, …` multi-line SQL confirmed by grep audit; was missed by the initial single-line grep but exists).
- `src/commands/review.rs:215, 242, 282` — same.

**Existing files modified** (Category B — 1):
- `src/commands/next/mod.rs:244` — use `try_claim(id, &[Todo])`.

**Existing files modified** (Category C — 2 files, 14 sites total = 12 in engine.rs + 2 in overflow.rs):
- `src/loop_engine/engine.rs` — 12 production UPDATE sites at lines `789, 1645, 2410, 3264, 4730, 5151, 5750, 5775, 5811, 5900, 6258, 6284` (line 9454 is in a test). Note: `auto_block_task` function declaration is at line `5140` but its single UPDATE is at `5151` — they are the SAME site, not two. Route each through service verbs per FR-006 table.
- `src/loop_engine/overflow.rs:460, 473` — `auto_block_after_failures` / `resurrect_for_iteration`.

**Existing files modified** (Category D — 2):
- `src/loop_engine/prd_reconcile.rs:305, 550` — split into plan-build (stays here as `ReconcilePlan` constructor) + plan-apply (moves to `lifecycle.reconcile_from_prd`). The plan-building logic is **explicitly NOT consolidated into the service** — a future implementer must not "helpfully" move it.
- `src/commands/doctor/fixes.rs:30, 93` — call `lifecycle.repair_stale(plan)` (dedicated verb, NOT folded into reconcile; see §6 doctor sub-decision).

**Untouched** (Category E):
- `src/commands/init/mod.rs:517` — documented exception comment added.

**New test files**:
- `tests/lifecycle_shadow.rs` — shadow test harness (FR-008).
- `tests/lifecycle_dogfood_smoke.rs` — small smoke test the dogfood gate (§7) can rely on.

### Dependencies

- **Internal**: `src/models/task.rs::TaskStatus`, `src/db/` (no schema changes), `src/loop_engine/prd_reconcile.rs::update_prd_task_passes` (call site moves into service).
- **External**: None. No new crates.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| **A. New `src/lifecycle/` module, narrow public API, command + loop both delegate** | Symmetric placement (sibling of `commands/` and `loop_engine/`); no circular deps; recovery + reconcile co-located with user-facing transitions; mirrors `iteration_pipeline.rs` win | One more top-level module; "lifecycle" is a slightly abstract name; needs `pub use` re-exports during transition | **Preferred** |
| **B. Place service inside `src/commands/lifecycle/`** | Closer to today's seven command modules; lower-noise import paths for commands | Loop-engine becomes dependent on `commands/`; "command" is the wrong noun for recovery + reconcile + claim verbs; surfaces semantic confusion at API design time | Rejected — design-doc note in §"TaskLifecycle Scope Decision" explicitly anticipates this |
| **C. Place service inside `src/loop_engine/`** | Co-located with the dominant client | Commands now depend on `loop_engine/`, which is the inverse of today's correct layering; encourages future engine-specific code creep into the service | Rejected |
| **D. Two services: one for user intent, one for recovery + reconcile** | Stronger separation of concerns; CLI-facing service stays narrow | Doubles the surface and the test harness; the side-effect set (run_tasks, PRD JSON sync, notes formatting) is identical across both, leading to immediate duplication | Rejected — design doc §"TaskLifecycle Scope Decision" explicitly says one service with different verbs |
| **E. No new service; just move `apply_status_updates` to `commands/` and have every site call it** | Smallest diff; reuses existing function | Doesn't address `try_claim`, recovery, or reconcile; leaves Category C inlined SQL; defeats the goal of "fewer patch surfaces" | Rejected |
| **F. Vertical-slice migration: land service skeleton + migrate one Category A verb (skip.rs) + mini-dogfood gate, THEN bulk-migrate the rest** | Smallest blast radius if shadow harness misses something; rollback is one verb's worth of files, not 22; exercises the service against a live loop before the bulk migration commits | Adds ~1 day for the mini-gate; introduces an interleaving where some sites are migrated and others aren't | **Preferred sequencing strategy** — applied on top of Approach A; see §8 for the gate placement |

**Selected Approach**: **A + F** — new `src/lifecycle/` module (Approach A) with **vertical-slice migration** (Approach F) on top. Mirrors the previous unification win (learning #2065/#2086/#2286 unified post-Claude work into `iteration_pipeline.rs` as a *new* module, not by stuffing it into one of the existing ones).

**Phase 2 Foundation Check**: Approach A + F costs ~2-3 extra days vs. Approach E (the minimum diff), but unblocks all of Phase 2 (engine carve, prompt assembler) by providing a stable seam. Without it, the engine carve PRD has to invent the seam mid-flight, which design-doc §"Recommended Phasing" calls "footgun." The extra day for vertical-slice (F) catches drift on one verb instead of 22, a ≥7× blast-radius reduction. 1:10 ratio comfortably satisfied — explicit phase-2 foundation.

**Doctor/fixes.rs sub-decision** (Category D): Use a **dedicated `repair_stale` verb**, NOT a fold-in to `reconcile_from_prd`. Rationale (revised per architect review):
- Both verbs share *side effects* (run_tasks insert, audit log, PRD JSON sync) but NOT *provenance*. Doctor is a time-based heuristic; reconcile is an external-truth check.
- Public API readers should not have to know `reconcile_from_prd` handles doctor; the verb name is the documentation.
- Internally, both verbs can reduce to a shared `apply_plan_with_source` helper — duplication is zero.
- `TransitionSource` carries `DoctorRepair` as a sibling of `ReconcilePrd`; the validator's matrix branches on source.

The original "one-verb fold-in" was rejected because the abstraction muddies under future maintenance pressure: when someone adds a third time-based heuristic, they'd cargo-cult it into reconcile rather than a new verb.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Subtle behavioral drift in `apply_status_updates` (auto-claim, per-task outcomes, stderr shape) — silently breaks live loops | High (data loss / DB corruption) | Medium | Shadow test harness (FR-008) asserts byte-identical DB diff + stderr snapshot. Pre-Phase-1 coverage gates (FR-010) land first. Dogfood gate (§7) catches anything the harness misses. |
| Engine modifications collide with the parallel `runner-trait-hygiene` effort on the spawn → post-process window | Medium (merge conflicts; double-rewrite of the same code) | Medium-High | §"Boundary Contract" in design doc + reciprocal pointer added to `runner-trait-hygiene.md`. Coherence Phase 1 owns `apply_status_updates`, `iteration_pipeline`, and the audit sites; runner hygiene owns `runner.rs`, `dispatch`, and provider-specific cleanup. First to land leaves clear seams. |
| Live PRD corruption during refactor (the dogfood concern) | High (irrecoverable for the affected PRD) | Low-Medium without gate; very low with gate | Dogfood gate (§7): N=10 iterations across two distinct live PRDs before the engine-carve PRD spawns. PRDs in this cluster are serialized, not parallelized. |
| Per-task partial-failure tolerance silently regressed to batch-level `Result` | Medium (loop abort instead of per-task fail) | Medium (LLM agents naturally reach for `Result<(), Err>`) | Disallowed in §5 explicitly; FR-002 / FR-003 enforced by per-task outcome assertion test; learning #2284 referenced in task notes. |
| Module placement (`src/lifecycle/`) introduces circular deps with `models/` or `commands/` | Low (build break, caught fast) | Low | Lifecycle depends on `models::TaskStatus` only; commands depend on `lifecycle`; loop_engine depends on `lifecycle`. No cycle. Verified by `cargo build`. |
| Hidden compat-break in `task-mgr` CLI surface (e.g. exit code change) | Medium (script breakage for users) | Low | Each Category A command keeps its current exit-code mapping; outcome → exit code is part of the thin shell, not the service. Smoke test the public CLI verbs end-to-end. |

**Top 3 Risks (impact × likelihood ranked)**:
1. **Subtle behavioral drift** — High × Medium. Shadow harness (FR-008 incl. write-order + atomicity) + full dogfood gate + mini-dogfood gate on the skip vertical slice.
2. **Concurrent-effort merge collision with runner hygiene** — Medium × Medium-High. Boundary contract + reciprocal pointer + **explicit ordering gate** in §8 step 0.5 (promoted from "open question" to "must-clear before first task spawns").
3. **Per-task partial-failure regression** — Medium × Medium. Explicit disallowed in §5 + assertion test + `TransitionOutcome` shape locked at FR-001.

None are High×High. No blockers. Proceed.

### Security Considerations

- SQL: all new queries use parameterized statements (rusqlite `params!` / `execute_named`). No string interpolation of user data into SQL — same as today.
- No new attack surface (no new CLI flag, no new file path, no new external call).
- Audit log: the service emits a structured audit row (or stderr line for now) per transition with `(task_id, from, to, source)`. Source enum makes it easy to spot e.g. a `ReconcilePrd` flipping a `done` task without operator intent. Today this is partially logged via `notes` column edits; the service preserves that behavior and optionally enriches it.

### Public Contracts

#### New Interfaces

| Module/Function | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `lifecycle::TaskLifecycle::new` | `fn new(conn: &Connection) -> Self` | `Self` (no run context, suitable for direct CLI use) | n/a | None |
| `lifecycle::TaskLifecycle::with_run` | `fn with_run(conn: &Connection, run_id: &str, iteration: u32) -> Self` | `Self` with run context | n/a | None |
| `lifecycle::TaskLifecycle::with_prd_sync` | `fn with_prd_sync(self, path: &Path, prefix: &str) -> Self` | `Self` with PRD JSON sync enabled | n/a | None |
| `lifecycle::TaskLifecycle::apply` | `fn apply(&self, intents: &[TransitionIntent]) -> Result<Vec<TransitionOutcome>>` | `Vec<TransitionOutcome>` (length = input length) | `anyhow::Error` only on infrastructure failure (DB lock, etc.), NOT on per-task rejection | DB UPDATE on `tasks`, optional INSERT on `run_tasks`, optional `update_prd_task_passes` call, stderr warning on PRD sync failure |
| `lifecycle::TaskLifecycle::try_claim` | `fn try_claim(&self, task_id: &str, allowed: &[TaskStatus]) -> Result<bool>` | `true` if claimed, `false` if pre-state didn't match | `anyhow::Error` on DB failure | DB UPDATE on `tasks` (status, started_at) conditional on allowed-status set |
| `lifecycle::TaskLifecycle::recover_in_progress_for_prefix` | `fn recover_in_progress_for_prefix(&self, prefix: Option<&str>) -> Result<usize>` | count of rows reverted | `anyhow::Error` on DB failure | bulk UPDATE `in_progress → todo` (optionally prefix-scoped) |
| `lifecycle::TaskLifecycle::auto_block_after_failures` | `fn auto_block_after_failures(&self, task_id: &str, last_error: &str, iteration: u32) -> Result<bool>` | `true` if blocked | n/a | UPDATE `in_progress → blocked` with `last_error` + `blocked_at_iteration` |
| `lifecycle::TaskLifecycle::resurrect_for_iteration` | `fn resurrect_for_iteration(&self, prefix: &str, ids: &[String]) -> Result<usize>` | count of resurrected | n/a | bulk UPDATE `* → todo`, clears `started_at` |
| `lifecycle::TaskLifecycle::reconcile_from_prd` | `fn reconcile_from_prd(&self, plan: ReconcilePlan) -> Result<ReconcileReport>` | summary report | n/a | per-row UPDATE driven by plan, audit-logged, `TransitionSource::ReconcilePrd` |
| `lifecycle::TaskLifecycle::repair_stale` | `fn repair_stale(&self, plan: RepairPlan) -> Result<RepairReport>` | summary report | n/a | per-row UPDATE driven by plan, audit-logged, `TransitionSource::DoctorRepair` |

#### Modified Interfaces

| Module/Function | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `loop_engine::engine::apply_status_updates` | `pub fn apply_status_updates(conn: &Connection, run_id: &str, iter: u32, extracted: &[StatusUpdate], …) -> Vec<…>` | Re-exports `TaskLifecycle::apply` semantics; thin shim that builds intents and calls `lifecycle.apply`. Old signature preserved for tests until end of PRD. | No (preserved for transition; deprecated after) | Internal callers move to `lifecycle.apply` directly. Public re-export marked `#[deprecated(note = "use TaskLifecycle::apply")]`. |
| `loop_engine::engine::auto_block_task` | `pub fn auto_block_task(conn, id, err, iter) -> bool` | Becomes a re-export of `TaskLifecycle::auto_block_after_failures`. | No | Same as above. |
| `loop_engine::engine::claim_slot_task` | `fn claim_slot_task(conn, id) -> bool` (private) | Becomes a thin wrapper around `TaskLifecycle::try_claim(id, &[Todo, InProgress])` (private). | No | Mechanical. |
| `models::task::TaskStatus::can_transition_to` | `pub fn can_transition_to(&self, target) -> bool` | Moves to `pub(crate) fn validate(from, to, source) -> Result<(), TransitionRejectReason>` inside `lifecycle::matrix`. Existing `can_transition_to` removed. | Yes (internal-only — no external consumers) | The 2 callers (`complete.rs`, `fail/transition.rs`) become thin shells calling `lifecycle.apply`, so `can_transition_to` is no longer needed at those sites. |

### Data Flow Contracts

This refactor touches three cross-module data structures. Key types verified by reading source:

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| `TransitionIntent → service → DB UPDATE` | Rust struct (typed fields) → SQL parameterized statement (positional `?` params) | `conn.execute("UPDATE tasks SET status = ?, … WHERE id = ?", params![intent.target.as_str(), &intent.task_id])?` |
| `ReconcilePlan` (built in `prd_reconcile`, consumed by `lifecycle::reconcile_from_prd`) | `Vec<ReconcileItem { task_id: String, target: TaskStatus, source: ReconcileSource }>` | `for item in plan.items { lifecycle.apply_reconcile_item(item)? }` — keys are owned `String` task IDs; no JSON traversal across this boundary. |
| `extracted_status_updates → apply_status_updates → run_tasks INSERT` | `Vec<StatusUpdate { task_id: String, change: StatusChange }>` (Rust struct) → SQL `INSERT INTO run_tasks (run_id, task_id, iteration, status) VALUES (?, ?, ?, ?)` | All keys are owned `String` or typed enum; no implicit string-key/atom-key transition. |

**No type transitions** across module boundaries — every key in this refactor is either a typed Rust enum variant or an owned `String` field. The "wrong-key-type" hazard from the PRD template (JSON string-keyed map under a typed struct field) does not apply here. PRD JSON sync (`update_prd_task_passes`) is the only string-keyed JSON access, and it lives behind a single existing function the service calls.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `src/commands/complete.rs:199` (`can_transition_to`) | Validates before UPDATE | NEEDS REVIEW — call removed; service validates internally | Thin shell calls `lifecycle.apply`; outcome carries `reason` if rejected |
| `src/commands/fail/transition.rs:123` | Same as above | NEEDS REVIEW | Same |
| `src/loop_engine/iteration_pipeline.rs:275-286` | Reads `status_updates_applied` per-task outcomes | OK — service preserves per-task outcome semantics | Snapshot/unit test asserts identical outcome vector shape |
| `tests/crash_escalation_per_task.rs` | Asserts auto_block behavior under crash | NEEDS REVIEW — call site moves; assertions still valid | Shadow test covers; run before merge |
| All `tests/*.rs` that exercise `complete`/`skip`/`fail` flows | Indirect — go through CLI | OK — public CLI contract unchanged | Full suite must remain green |
| Operators grep stderr for `"PRD JSON sync failed"` | External tooling / scripts | OK — string preserved bit-identically | Snapshot test (FR-010) |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| `commands/next/mod.rs:244` (`WHERE id = ? AND status = 'todo'`) | CLI claim | Fails silently if not in `todo` | `try_claim(id, &[Todo])` — same semantics, explicit allowed set |
| `engine.rs:787` (`WHERE id = ? AND status IN ('todo','in_progress')`) | Slot resumption | Idempotent re-claim during slot recovery | `try_claim(id, &[Todo, InProgress])` — same, explicit |
| `engine.rs:4730` (auto-claim inside `apply_status_updates`) | LLM emits `done` without prior claim | Silent `todo → in_progress` then `in_progress → done` | Internal to `lifecycle.apply` — silent, same semantics |
| `prd_reconcile.rs:305` (`WHERE id = ? AND status IN ('todo','in_progress')`) | PRD authoritatively marks done | Can flip non-terminal tasks; refuses if already terminal | `reconcile_from_prd` plan item — same predicate, named source |
| `prd_reconcile.rs:550` (`* → irrelevant` from PRD) | PRD removes a task | Flips *any* prior state (including terminal) | `reconcile_from_prd` plan item — same, explicit reconcile-source bypass of user-facing matrix |

The auto-claim and the PRD-driven terminal-state flip are NOT bugs — they are deliberate semantics that the service must preserve via different code paths than the user-facing `apply`. The `source: TransitionSource` discriminator is how the matrix knows which rules to enforce.

### Inversion Checklist

- [x] All callers identified — audit table in design doc + grep verification in this PRD.
- [x] Routing/branching decisions reviewed — per-task outcomes, status-tag gate (learning #2238), terminal-status pruning (learning #2796).
- [x] Tests validating current behavior identified — `models/task.rs:513-571` matrix tests, `tests/crash_escalation_per_task.rs`, integration tests across `tests/*.rs`.
- [x] Different semantic contexts for same code discovered — auto-claim vs. user-claim, PRD-reconcile vs. operator intent, slot-resumption vs. CLI claim.

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `docs/designs/coherence-refactoring.md` | Update (after merge) | Append "Phase 1 PRD 1 retrospective" with verified site count (20, not 15), any approach revisions, learnings captured via `task-mgr learn`. |
| `src/lifecycle/CLAUDE.md` | Create | Module-level notes: scope (status transitions + recovery + reconcile, NOT bootstrap), invariants (auto-claim, per-task outcomes, DB-authoritative-PRD-best-effort, stderr warning shape), the FR-006 site→verb mapping table as a quick reference. |
| `src/loop_engine/CLAUDE.md` | Update | Remove or rewrite any "if you touch status, also update X, Y, Z" rules — replace with "call `TaskLifecycle`." |
| `CLAUDE.md` (project root) | Update | Add a 1-line pointer under "Subsystem design notes": `src/lifecycle/CLAUDE.md — task status transitions, recovery primitives, PRD reconciliation`. |
| `docs/system-design-overview.md` (if exists) | Update | Add the lifecycle service to the architecture diagram / module map. (If file doesn't exist: skip.) |

---

## 7. Open Questions

- [ ] **Module name**: `lifecycle` vs. `task_lifecycle` vs. `task_state` vs. `domain`. **Proposed**: `lifecycle` (concise; the only "lifecycle" in the codebase is task lifecycle). Decide before first task spawns; not a blocker for the PRD itself.
- [ ] **Dogfood-gate `N` value for PRD 2**: design doc proposes default N=10 iterations across two PRDs. **Proposed**: confirm N=10. Decide before the **next** PRD ("Engine Orchestration Boundaries") is spawned, NOT before this PRD.
- [ ] **Mini-dogfood-gate `M` value** (vertical-slice mini-gate after the skip.rs migration; see §8): **Proposed**: M=3 iterations on a single live PRD. Acceptable to decide during FR-004; not a blocker.

### Resolved during review (no longer open)

- [x] **Doctor vs. reconcile** — **RESOLVED**: dedicated `repair_stale` verb (NOT folded into `reconcile_from_prd`). Rationale in §6 doctor sub-decision. Locked.
- [x] **`reconcile_from_prd` API shape** — **RESOLVED**: `ReconcilePlan` is a struct + `items: Vec<ReconcileItem>`; plan-building stays in `prd_reconcile.rs`; the service consumes the plan only. Locked in §6 Public Contracts.
- [x] **`reset.rs` audit** — **RESOLVED 2026-05-19**: `reset.rs:78-79` has a multi-line `UPDATE tasks SET status = ?, …` SQL site. Confirmed Category A. Migrate per FR-004.
- [x] **`runner-trait-hygiene` coordination** — **PROMOTED to §8 step 0.5 as a gate**: before the first task in this PRD spawns, the runner-hygiene PRD (`tasks/prd-runner-trait-hygiene.md`) must reach one of {merged to main, paused with written status, ordering decision recorded}. The merge-collision risk on `engine.rs` spawn sites is too sharp to leave as a non-blocking open question.

---

## 8. Sequencing & Dogfood Gates

This PRD is **PRD 1 of 2** in Phase 1 of the coherence refactoring. The order is **strict**:

**0.5. Runner-hygiene ordering gate (must clear before any task in this PRD spawns)**:
   - The parallel `tasks/prd-runner-trait-hygiene.md` PRD must be in one of: {merged to main; paused with written status; resolved as "runs after this PRD"}.
   - Confirm a reciprocal pointer to this PRD's filename exists in `docs/designs/runner-trait-hygiene.md` §"Boundary Contract" (or the runner PRD's equivalent section).
   - Reason: both efforts edit overlapping spawn → post-process windows in `engine.rs`. The design-doc boundary contract is necessary but not sufficient — explicit ordering eliminates the merge-collision risk entirely.

**1. Pre-Phase-1 coverage gates (FR-010)**: Land as a small initial commit / first task.
   - Baseline test counts recorded.
   - Baseline iteration latency for 5-task FEAT loop recorded.
   - Category C unit tests added.
   - Stderr snapshot test added.
   - `(from, to, source)` matrix test added.

**1.5. Vertical-slice migration (new — per architect review Approach F)**:
   - Build the full `src/lifecycle/` module skeleton (matrix, apply, claim, recovery, reconcile, repair) per FR-001.
   - Migrate **one Category A verb** — `commands/skip.rs` (smallest side-effect set, lowest blast radius).
   - Land FR-008 shadow tests for the skip path.
   - Run a **mini-dogfood gate**: `task-mgr loop` runs M=3 iterations on a single live PRD with the skip-migration in place. Zero regressions.
   - **Only if the mini-gate passes**, proceed to step 2. Otherwise, root-cause and re-test before continuing.
   - Rollback cost if the harness misses something here: one verb's worth of files, not 22.

**2. Bulk migration**: Land the remaining Category A (6 verbs) + B + C + D sites per FR-004 through FR-007.

**3. Full dogfood gate**: main-branch `task-mgr loop` runs continuously for **N=10 iterations across two distinct in-progress PRDs** on the refactored code with zero loop-internal regressions.

**4. Only after step 3 is satisfied**, author and spawn the follow-up PRD: "Engine Orchestration Boundaries" — which carves `engine.rs` along the seams the new `TaskLifecycle` exposes.

**5.** After PRD 2 also passes its own dogfood gate, append retrospectives to `docs/designs/coherence-refactoring.md` and feed concrete learnings via `task-mgr learn`.

Doing the engine carve *without* the full dogfood gate (step 3) is forbidden by §"Recommended Phasing" of the design doc and §"Risks" of this PRD. Skipping the mini-gate (step 1.5) is permitted only if the runner-hygiene effort is fully merged and the PRD's first task spawns with an explicit waiver.

---

## Appendix

### Related Documents

- `docs/designs/coherence-refactoring.md` — Parent design (Phase 1 = Items 1 + 2).
- `docs/designs/runner-trait-hygiene.md` — Parallel effort; §"Boundary Contract" defines non-interference.
- `src/loop_engine/iteration_pipeline.rs` — Design template (post-Claude pipeline unification).
- `src/models/task.rs` — Current home of `TaskStatus` + `can_transition_to`.
- Learnings (institutional memory):
  - #2284 — `apply_status_updates` per-task outcomes (success pattern).
  - #2238 — Status-tag completion gate must check claimed task's outcome.
  - #2304 — Step 7 per-task crash tracking nuance.
  - #2796 — Prune terminal-status tasks from tracking maps.
  - #2065 / #2086 / #2286 — `iteration_pipeline` unification success (design template for this PRD).
  - #2807 — Baseline test counts before refactoring.
  - #2740 — Test modules lose implicit access during extraction.
  - #440 — Re-export pattern avoids caller import changes during extraction.
  - #2806 — Consolidate test helpers in test_utils.

### Glossary

- **Category A–E**: Audit categories for status-write sites — see design doc §"Status-Write Site Audit". A = user-facing command, B = race-safe pre-claim, C = loop-side recovery, D = reconcile / PRD-driven, E = bootstrap.
- **Shadow test**: A test that runs both the legacy and refactored code paths against cloned state and asserts byte-identical results.
- **Dogfood gate**: Per design doc §"Risks" — main-branch loop runs N iterations across two PRDs on refactored code before the next PRD in the cluster is spawned.
- **Transition source**: The discriminator carried in `TransitionIntent.source` that tells the validator which rules apply (operator intent uses the strict matrix; PRD-reconcile is allowed to flip terminal states; recovery has its own rules).
- **`<task-status>` tag**: Side-band tag the LLM emits in iteration output to request a status change. Recognized statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`. Contract preserved by this refactor.
