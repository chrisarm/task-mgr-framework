# src/lifecycle — design notes

Cross-file narrative for the lifecycle subsystem. Per-function contracts live
in rustdoc next to the code. Module-level invariants that cut across files are
here.

## Scope

This module is the **single source of truth for all `tasks.status` mutations**.
Every write to the `tasks.status` column must go through a `TaskLifecycle` verb
(the one exception is `commands/init/mod.rs`, marked `LIFECYCLE-EXCEPTION`).

**In scope:**

- Category A — user-intent status transitions (`apply()`)
- Category B — race-safe pre-claim (`try_claim()`)
- Category C — bulk recovery (`recover_in_progress_for_prefix()`,
  `auto_block_after_failures()`, `resurrect_for_iteration()`)
- Category D — PRD-driven reconciliation (`reconcile_from_prd()`)
- Category D — doctor heuristic repair (`repair_stale()`)
- Side effects that travel with a status write: `run_tasks` bookkeeping,
  PRD JSON sync, decay columns (`blocked_at_iteration` /
  `skipped_at_iteration`), audit notes formatting, exact stderr warning shape

**Out of scope (do not fold in):**

- Bootstrap / project initialization — `commands/init/mod.rs` has one
  permitted raw UPDATE site, marked `LIFECYCLE-EXCEPTION`
- Plan-building — `ReconcilePlan` lives in `loop_engine/prd_reconcile.rs`;
  `RepairPlan` lives in `commands/doctor/fixes.rs`. Plans flow INTO this
  module; they are not built here.
- DB schema migrations — `src/commands/migrate/`

## Five hard invariants

These are preserved bit-identically from the pre-extraction behavior and are
**never negotiable**:

1. **Auto-claim on `<task-status>:done` for `Todo` rows.** When `apply()` is
   called with `TransitionChange::Done` and the task is currently `Todo`, it
   auto-claims (`Todo → InProgress`) before dispatching `Done`. Original site:
   `engine.rs:4724` (now folded into `apply_one` in `apply.rs`).

2. **Per-task partial-failure tolerance.** `apply()` returns
   `Vec<TransitionOutcome>`, NEVER `Result<(), Err>` at the batch level. Each
   element records `applied: bool` plus an optional `reject_reason`. This is
   a hard contract (learning #2284 / #2238). Do not convert to a batch-level
   `Result`.

3. **DB-authoritative-PRD-best-effort.** PRD JSON sync (enabled via
   `.with_prd_sync(path, prefix)`) is best-effort. The DB write commits
   regardless of whether the JSON update succeeds. PRD sync failures never
   block the status change.

4. **Exact stderr warning shape.** Locked by `tests/lifecycle_stderr_contract.rs`:
   ```
   Warning: <task-status> dispatched {id} to done in DB but PRD JSON sync failed ({path}): {err}
   ```
   Do not reformat this string. Operators grep for this prefix in CI and
   monitoring scripts.

5. **Conditional-WHERE in `try_claim`.** The expected-status predicate
   (`AND status='todo'` or `AND status IN ('todo','in_progress')`) MUST
   remain explicit in the SQL UPDATE. An unconditional UPDATE is explicitly
   prohibited (PRD FR-005). Optimistic-locking: a zero-row-affected result
   signals a lost race, not an error.

## Construction

```rust
// CLI direct: no run context, no PRD JSON sync
let lc = TaskLifecycle::new(conn);

// Loop iteration: threads run_id into run_tasks bookkeeping
let lc = TaskLifecycle::with_run(conn, run_id);

// With PRD JSON sync (chained onto either constructor)
let lc = TaskLifecycle::with_run(conn, run_id)
    .with_prd_sync(prd_json_path, task_prefix);
```

## FR-006 site→verb mapping

All 25 production `UPDATE tasks SET status` sites were audited in ANALYSIS-001
and mapped to `TransitionSource` variants and lifecycle verbs.

| Category | Representative sites | Lifecycle verb | `TransitionSource` |
|---|---|---|---|
| **A** user-intent | `commands/complete.rs:248`, `commands/fail/transition.rs:83`, `commands/skip.rs:125`, `commands/irrelevant.rs:136`, `commands/unblock.rs:87+146`, `commands/review.rs:215+242+282`, `commands/reset.rs:78` | `apply()` | `Operator` / `LoopStatusTag` |
| **B** race-safe pre-claim | `commands/next/mod.rs:244` (CLI `next --claim`), `loop_engine/engine.rs:786` (`claim_slot_task` — slot wave) | `try_claim()` | `Operator` / `LoopStatusTag` |
| **C** bulk recovery | `loop_engine/engine.rs:1642` (slot reset), `engine.rs:2407` (mid-run stale sweep), `engine.rs:3258` (startup sweep), `engine.rs:5145` (auto-block), `commands/next/decay.rs:129` (decay reset), `loop_engine/overflow.rs:460+466+471` (overflow rungs 3-5) | `recover_in_progress_for_prefix()`, `auto_block_after_failures()`, `resurrect_for_iteration()` | `Recovery` |
| **D** PRD-driven | `loop_engine/prd_reconcile.rs:305` (passes:true), `prd_reconcile.rs:550` (irrelevant mutation) | `reconcile_from_prd()` | `ReconcilePrd` |
| **D** doctor heuristic | `commands/doctor/fixes.rs:30` (`fix_stale_task`), `doctor/fixes.rs:93` (`fix_git_reconciliation`) | `repair_stale()` | `DoctorRepair` |
| **Exception** | `commands/init/mod.rs:518` | _(raw SQL — `LIFECYCLE-EXCEPTION` comment required)_ | n/a |

`reconcile_from_prd` and `repair_stale` are kept as **separate verbs**
intentionally. `DoctorRepair` never consults the PRD JSON and has a narrower
source-allowance set than `ReconcilePrd` (e.g., `DoctorRepair` cannot flip
`Done → Irrelevant`). Folding them is explicitly prohibited per PRD §6
doctor sub-decision.

## Recovery verb families (Recovery vs. Plan-driven)

The three **Recovery** verbs (`recover_in_progress_for_prefix`,
`auto_block_after_failures`, `resurrect_for_iteration`) are intentionally
**not** routed through the plan/matrix path used by ReconcilePrd / DoctorRepair /
DecayReset.

- They carry `TransitionSource::Recovery`.
- `recover_in_progress_for_prefix` and `auto_block_after_failures` keep an
  explicit `WHERE status = 'in_progress'` guard (matching their original
  inline SQL).
- `resurrect_for_iteration` (the per-ID reset used by wave FEAT-002 and
  overflow rungs) **deliberately omits** that guard so callers can list any
  task ID they want forced back to `todo`. This was a conscious contract
  relaxation during the extraction (see TEST-INIT-002 and the
  `reset_task_to_todo` call sites).

In contrast, plan-driven verbs go through `allowed_from_for_plan` +
`matrix::validate` and only produce the transitions listed in the matrix.

This split keeps hot recovery paths simple and avoids forcing every recovery
action through a generic plan builder. The asymmetry is documented here and in
the rustdoc for `resurrect_for_iteration` so future maintainers do not assume
all Recovery verbs behave the same.

## TransitionSource matrix

Full `(from, to, source)` allowance table: `src/lifecycle/matrix.rs`; see the
"Matrix consultation policy" rustdoc section there for which code paths consult
the matrix and which use narrower inline checks instead.
Key expansions beyond the `Operator` baseline:

| Source | Extra-allowed transitions |
|---|---|
| `Recovery` | `InProgress → Todo` (stuck task reset) |
| `ReconcilePrd` | `Done → Irrelevant`, `Todo → Done`, `Todo → Irrelevant` |
| `DoctorRepair` | `InProgress → Todo`, `Todo → Done` |

Same-state transitions (`from == to`) are always permitted as no-ops.
