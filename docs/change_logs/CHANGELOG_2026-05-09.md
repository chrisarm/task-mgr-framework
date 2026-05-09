# Changelog — 2026-05-09

## Parallel-slot prevention hardening

**Branch**: `feat/parallel-slot-prevention`
**PRD**: `tasks/parallel-slot-prevention.json`

### What shipped

Five prevention layers around parallel-slot loops, plus four review-driven hardening fixes:

- **Slot path threading** — `merge_slot_branches_with_resolver` now takes the actual slot worktree paths from `ensure_slot_worktrees` instead of recomputing them, closing the ENOENT cascade that triggered the mw-datalake incident.
- **Consecutive-merge-fail halt** — new `ProjectConfig::merge_fail_halt_threshold` (default 2) halts the loop after N consecutive merge-back failure waves; `0` preserves legacy "log and continue" behaviour. Reset/halt ordering is contractual: failed-slot tasks are reset to `todo` BEFORE any halt so a halted run leaves the DB re-runnable.
- **Implicit-overlap shared-infra detection** — `IMPLICIT_OVERLAP_FILES` baseline (Cargo.lock, uv.lock, package-lock.json, go.sum, …) plus `BUILDY_TASK_PREFIXES` heuristic auto-serialize lockfile-touching tasks through one synthetic `__shared_infra__` slot per wave. New `claimsSharedInfra` per-task escape hatch (migration v19).
- **Cross-wave file affinity + deadlock guard** — `select_parallel_group` reads un-merged files from `{branch}-slot-N` ephemeral branches via `git diff --name-only` and defers candidates that would conflict. When every candidate is exclusively blocked by ephemeral overlap, surfaces named branches and feeds the FEAT-002 halt counter.
- **Stale ephemeral branch hygiene at startup** — new `reconcile_stale_ephemeral_slots` four-case classifier deletes orphans, cleans merged ephemerals, aborts on dirty worktrees, and warns/aborts on un-merged work depending on `halt_threshold`.

Review-driven hardening (post-loop):

- **Slot-0 reconcile guard** — rejects stray `{branch}-slot-0` refs at classify time so reconciliation can never `git worktree remove` the loop's own running worktree.
- **`Vec<FailedMerge>` refactor** — collapsed `WaveOutcome.failed_merges` + `failed_merge_task_ids` parallel arrays into one `Vec<FailedMerge>` so slot/task pairing is a type-level invariant.
- **Synthetic-deadlock sentinel** — when every blocking ephemeral has a malformed slot suffix, insert one `SYNTHETIC_DEADLOCK_SLOT` entry so the halt counter still increments instead of resetting to 0.
- **Run-level config caching** — `ProjectConfig` and PRD `implicit_overlap_files` now load once at run-loop startup and thread through `WaveIterationParams` instead of being re-parsed every wave.

### Why it matters

The mw-datalake incident burned several hours of compute on a parallel run that silently cascaded merge-back failures. These layers prevent the cascade from starting (implicit-overlap detection, shared-infra serialization), halt it cleanly when it does start (consecutive-fail threshold + cross-wave deadlock guard), and recover safely on the next run (stale-branch reconciliation). The post-review hardening closes a sharp corner where the safety guards themselves could fail-open under adversarial inputs.

### Breaking changes

- New `ProjectConfig` fields (`merge_fail_halt_threshold`, `implicit_overlap_files`, …) all use `#[serde(default)]` so older `.task-mgr/config.json` files keep working.
- Migration v19 adds `claims_shared_infra INTEGER DEFAULT NULL` to `tasks`; existing rows default to `NULL` (heuristic decides).
- **Mid-loop edits to `.task-mgr/config.json` or the PRD JSON no longer take effect without a loop restart.** Same restart-required semantics every other run-scoped knob already has.

---
