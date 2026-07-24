# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Batch Chain Incomplete Stop** for **task-mgr**.

## Problem Statement

When `task-mgr batch run --chain` runs PRDs sequentially, a loop that **exhausts its iteration budget** (or hits a **deadline**) with `todo`/`in_progress` work remaining still returns `exit_code = 0`. Batch chain only stops on `exit_code != 0`, so downstream PRDs start on an incomplete branch tip (e.g. express-lane burns budget on CODE-REVIEW retries, spawns CODE-FIX on the last iteration, then scan-sweep starts with CODE-FIX never run).

**Two-layer fix (approved design: `docs/designs/batch-chain-incomplete-stop.md`):**

1. **Layer 1:** `reconcile_ambiguous_exit` after the outer iteration `while`, **before** step 17.5 task resets — sets `exit_code = 1` when reason is `max iterations reached` or `deadline reached` and `count_remaining_active_tasks > 0`.
2. **Layer 2:** `LoopResult.prd_complete` + batch gate `chain && (exit_code != 0 || !prd_complete)` with message "PRD did not complete".

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — For `modifiesBehavior: true` tasks (FEAT-001, FEAT-002), name the one alternative you rejected and why.

After writing code, run the **scoped** quality gate (Quality Checks § Per-iteration).

---

## Priority Philosophy

**PLAN** → **FOUNDATION** (reuse `count_remaining_active_tasks`) → **FUNCTIONING CODE** → **CORRECTNESS** → **CODE QUALITY** → **POLISH**.

Non-negotiables: reconcile runs **only** on outer post-`while` path; **never** after step 17.5 resets; `chain=false` unchanged; `was_stopped` stays exit 0.

**Prohibited outcomes:**

- Calling `reconcile_ambiguous_exit` inside per-wave terminals, drained short-circuits, merge-halts, or signal paths
- Calling reconcile **after** step 17.5/17.6 task resets
- Downgrading `exit_code` when already nonzero
- Changing `was_stopped` exit 0 semantics
- Surfacing `prd_complete` in batch summary (out of scope)
- Full e2e batch runs spawning real agents
- Populating `synergyWith` / `batchWith` / `conflictsWith`
- Per-task or top-level `model` fields
- Tests that only assert "no crash"

---

## Global Acceptance Criteria

- Rust: `cargo check` and `cargo clippy --all-targets -- -D warnings` clean on touched code
- Rust: `cargo fmt --check` passes
- Scoped tests pass: `cargo test reconcile_ambiguous_exit`, `cargo test -p task-mgr batch::tests`, `cargo test -p task-mgr engine::tests` as applicable
- `chain=false` batch behavior unchanged

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.**

```bash
# taskPrefix is in JSON (b2db855c = md5(feat/batch-chain-incomplete-stop:batch-chain-incomplete-stop.json)[:8])
# loop init rewrites the same value; safe pre- or post-init.
PREFIX=$(jq -r '.taskPrefix' tasks/batch-chain-incomplete-stop.json)
task-mgr next --prefix $PREFIX --claim
```

Pre-init operator commands (`list --prd`, `status`) also work once `taskPrefix` is present:

```bash
task-mgr list --prd tasks/batch-chain-incomplete-stop.json
task-mgr show b2db855c-FEAT-001   # after loop init imports tasks into DB
```

| Need | Command |
|------|---------|
| Claim next task | `task-mgr next --prefix $PREFIX --claim` |
| Show task | `task-mgr show $PREFIX-FEAT-001` |
| Recall | `task-mgr recall --for-task $PREFIX-FEAT-001` |
| Spawn fix | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark done | `<task-status>$PREFIX-FEAT-001:done</task-status>` |

Progress: tail `tasks/progress-$PREFIX.txt` — never Read the whole file.

---

## Quality Checks

### Per-iteration (FEAT / FIX)

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test reconcile_ambiguous_exit
cargo test -p task-mgr batch::tests
cargo test -p task-mgr engine::tests
cargo test -p task-mgr cli::tests   # FEAT-003 only
```

### Full gate (REFACTOR-001 / REVIEW-001 only)

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

---

## Key Learnings (from task-mgr recall)

- **[4933]** Batch `--chain` stacks later PRDs on earlier branch tips — incomplete PRD must stop chain before advancing `chain_base`.
- **[4324]** `count_remaining_tasks` / `count_remaining_active_tasks` is the consolidated SQL helper — do not duplicate incompleteness SQL.
- **[3927]** `classify_drained_queue` handles empty-selection drain vs stale-abort — `reconcile_ambiguous_exit` is only for budget/deadline fallthrough.
- **[3824]** Best-effort features (auto-review) must never change loop/batch exit code.
- **[2695]** Exit code consistency across sequential and wave paths — never downgrade existing nonzero codes in reconcile.
- **[4753]** Stale parallel-slot test binaries can fail fixture reads after slot worktree prune — `touch tests/<binary>.rs` and rebuild before concluding REVIEW regressions.

---

## CLAUDE.md Excerpts (only what applies)

- `task-mgr batch run` accepts `--parallel N` and `--chain`; batch preflight runs `preflight_validate_and_probe` before any PRD.
- Batch chain: each PRD branches from previous PRD's branch via `chain_base` / `LoopResult.branch_name`.
- **Gotcha:** stale slot-worktree test binaries after parallel-slot loop — rebuild if only fixture paths point at removed `-slot-N` worktrees.

---

## Data Flow Contracts

### Incompleteness predicate (single source of truth)

```rust
// wave_scheduler.rs
pub(crate) fn count_remaining_active_tasks(
    conn: &Connection,
    task_prefix: Option<&str>,
) -> i64
// Counts tasks WHERE status NOT IN (done, irrelevant, skipped, blocked)
// AND archived_at IS NULL, scoped by task_prefix LIKE clause
```

### reconcile_ambiguous_exit (producer)

```rust
// wave_scheduler.rs — call from orchestrator ONLY after outer while, BEFORE step 17.5
reconcile_ambiguous_exit(
    &conn,
    task_prefix.as_deref(),
    was_stopped,
    &mut exit_code,
    &mut exit_reason,
    &mut final_run_status,
);
```

### LoopResult → batch (consumer)

```rust
// engine.rs
pub struct LoopResult {
    pub exit_code: i32,
    pub prd_complete: bool,  // NEW: true when count_remaining_active_tasks == 0 at loop end
    // ...
}

// batch.rs after engine::run_loop
let chain_break = chain && (exit_code != 0 || !loop_result.prd_complete);
if chain_break {
    ui::emit("Chain stopped: PRD did not complete, skipping remaining PRDs");
    push_remaining_skipped(...);
    break;
}
```

**Timing invariant:** both reconcile and `prd_complete` must read DB **before** `reset_task_to_todo` (step 17.5) — otherwise active work evidence is erased.

### LoopResult literal audit (FEAT-002)

No `LoopResult` struct literals exist under `tests/` — integration tests consume `run_loop` return values only. Audit is `src/loop_engine/` only:

```bash
rg 'LoopResult \{' --type rust
```

Every literal must set `prd_complete` explicitly or use `..Default::default()` / `Default` (conservative `false` on early-return paths in `startup.rs`).

### chain=false regression (REVIEW-001)

`chain=false` must **not** skip remaining PRDs on incomplete work — only `chain=true` enters the `chain_break` / `push_remaining_skipped` path. Verify with grep: chain gate wrapped in `if chain &&` only.

---

## Important Rules

- Work on **ONE task per iteration**
- Branch: **feat/batch-chain-incomplete-stop**
- Commit: `feat: <TASK-ID>-completed - [Title]`
- Emit `<task-status><TASK-ID>:done</task-status>` — never edit JSON by hand

---

## Stop / Blocked

`<promise>COMPLETE</promise>` only when all tasks `passes: true` and REVIEW-001 full suite green.

`<promise>BLOCKED</promise>` if requirements unclear — spawn clarification task via `task-mgr add --stdin`.