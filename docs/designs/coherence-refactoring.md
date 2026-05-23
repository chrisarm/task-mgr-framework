# TaskLifecycle Extraction — Design Retrospective

PRD: `tasks/tasklifecycle-extraction.json` (prefix `035925a9`)
Completed: 2026-05-20

---

## Verified site count

The PRD stated "~20 raw `UPDATE tasks SET status` sites" as the motivation.
ANALYSIS-001 found **25–26 production sites** across 13 files:

- Drift (+5/6) came from three places:
  - `loop_engine/overflow.rs` holds three distinct SQL shapes inside one
    `match` block (Blocked rung 5, FallbackToProvider rung 4, rungs 1-3
    todo-reset) — each counted separately because each has its own
    WHERE/SET clause.
  - `loop_engine/engine.rs` contributes 6 sites (including the
    `apply_status_updates` dispatcher auto-claim site at :4724 and three
    recovery sweep variants).
  - `commands/review.rs` has 3 sites (unblock, unskip, resolve — all
    `--auto` / `--resolve` variants of the same unblock shape).

The PRD §6 "Affected Components" table covered every site; the +5/6 was a
documentation gap in §1, not a scope gap.

Final migration outcome: **25 sites migrated** (all production raw SQL moved
inside `src/lifecycle/`). One permitted exception (`commands/init/mod.rs:518`,
marked `LIFECYCLE-EXCEPTION`) remains. The lint at
`tests/lifecycle_exception_lint.rs` enforces a hard ceiling of exactly one
`LIFECYCLE-EXCEPTION` token in non-test production code.

---

## Doctor-vs-reconcile decision rationale

The PRD's architect review (§6 doctor sub-decision) mandated keeping
`reconcile_from_prd` and `repair_stale` as distinct verbs rather than merging
them. Rationale:

- **Source-allowance set differs.** `DoctorRepair` may flip `InProgress → Todo`
  and `Todo → Done` (git-derived). `ReconcilePrd` additionally allows
  `Done → Irrelevant` and `Todo → Irrelevant` (PRD-modification semantics).
  A single merged verb would need a mode flag — worse than two named verbs.
- **PRD JSON dependency.** `reconcile_from_prd` is called with a
  `ReconcilePlan` derived from parsing the PRD JSON; its `passes:true` rows
  drive Done transitions. `repair_stale` (the doctor verb) never consults the
  PRD JSON — it runs from DB-only heuristics (`in_progress` with no recent
  heartbeat, or a git commit that matches the task ID). Merging would require
  threading PRD JSON into the doctor path, which has no caller that provides
  it.
- **Plan-building stays out of `src/lifecycle/`.** Both plans
  (`ReconcilePlan` / `RepairPlan`) are built in `prd_reconcile.rs` and
  `doctor/fixes.rs` respectively and flow *into* the lifecycle service. The
  service only consumes plans; it never builds them. Merging the verbs would
  push plan-coupling concerns into the service boundary.

Result: `TransitionSource::ReconcilePrd` and `TransitionSource::DoctorRepair`
remain as two separate enum variants in `src/lifecycle/matrix.rs`, each with
its own allowance row.

---

## Vertical-slice migration outcome

Per PRD §8, the migration was gated on a vertical slice through a single
command before bulk migration:

- **FEAT-007** (the vertical slice): `commands/skip.rs` only. Approach taken:
  move the skip SQL inline into `lifecycle/apply.rs::skip_one` (breaking the
  circular `apply → skip_cmd → SQL` dependency), then make `commands/skip.rs`
  a thin wrapper that builds a `TransitionIntent` and calls
  `TaskLifecycle::apply`. Pre-validation (`ensure_skippable`) preserves
  all-or-nothing multi-task semantics without an outer transaction.

- **CLARIFY-002** (mini-dogfood gate): required confirmation that the vertical
  slice produced no regressions before bulk migration (FEAT-008+) could start.
  Outcome: gate passed — the shadow-test harness (`tests/lifecycle_shadow.rs`)
  confirmed byte-identical DB diff between the legacy `skip_cmd` path and the
  new `TaskLifecycle::apply(Skipped)` path.

- **Bulk migration** (FEAT-008 through FEAT-011): completed after CLARIFY-002
  cleared. All 25 sites migrated in four passes grouped by category (A/B, C,
  D, then overflow.rs). 16 shadow tests added covering every verb and source
  variant.

The vertical-slice approach was validated: the FEAT-007/CLARIFY-002 gate
caught no regressions, but the shadow harness set up in TEST-INIT-004 did
surface a timestamp-normalization subtlety (`completed_at IS NULL` vs `<ts>`
must be distinguished, not collapsed).

---

## Runner-hygiene ordering gate value

CLARIFY-001 established that `feat/runner-trait-hygiene` was complete
(~20 commits ahead of main, MILESTONE-FINAL passed) but unmerged at the
time this PRD started. The gate required runner-trait-hygiene to be ordered
relative to this PRD before any implementation tasks spawned.

**Resolution**: runner-trait-hygiene was declared complete-and-merge-ready.
The ordering gate allowed this PRD to proceed without waiting for an actual
merge, on the basis that (a) the branches touched disjoint subsets of
`engine.rs` (spawn→post-process window in runner-hygiene vs. status-mutation
sites in lifecycle-extraction) and (b) if a merge conflict arose, it would
surface at review time with a clear resolution path.

Gate value: **ordering was established, not a hard merge prerequisite**. The
overlap in `engine.rs` was acknowledged but judged as a reconcilable conflict
rather than a blocking dependency. Lifecycle-extraction's edits were confined
to status-mutation call sites; runner-hygiene's edits were confined to the
runner/dispatch abstraction layer.

---

## Approach revisions

1. **`TransitionOutcome` shape**: initially designed as a plain enum; revised
   to a struct (`task_id`, `target`, `previous`, `applied`, `reason`) after
   recognizing that callers need field-level access, not just discriminant
   matching.

2. **`try_claim` idempotency for slot pre-claim**: site #13 (`claim_slot_task`
   at `engine.rs:786`) uses `AND status IN ('todo','in_progress')` rather than
   `AND status='todo'`, so a retry after a partial recovery re-claims an
   already-in-progress slot without error. `try_claim` accepts an explicit
   `expected_statuses: &[TaskStatus]` parameter to expose this without hiding
   the predicate inside the method.

3. **`plan_apply.rs` helper module**: the `PlanItemView` trait + shared
   `apply_plan_with_source` helper emerged as a deduplication seam when
   `reconcile_from_prd` and `repair_stale` had identical per-item flow
   (SELECT → validate → conditional UPDATE → audit note). Extracted to
   `src/lifecycle/plan_apply.rs` (pub(crate)) so both verbs share the
   race-safety and notes-append logic without copying SQL.

4. **PRD §1 site count revised from ~20 to 25**: documented in ANALYSIS-001.
   The extra sites did not require scope changes; they were already covered
   by the §6 Components table. The revised count is now the canonical figure
   for future archaeology.
