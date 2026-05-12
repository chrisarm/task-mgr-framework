# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Parallel-slot loop conflict & merge-back cascade prevention** for **task-mgr**.

## Problem Statement

A real-world incident in the `mw-datalake` project exposed a cascading failure in task-mgr's parallel-slot execution:

- A 2-slot loop ran on `feat/iceberg-lakehouse-phase-a`. Slot 1's merge-back failed on iteration 1 with `git rev-parse spawn: No such file or directory` because `merge_slot_branches_with_resolver` (`src/loop_engine/worktree.rs:778`) recomputed slot 0's path via `compute_slot_worktree_path`, which differs from the actual path `ensure_worktree` returns when the loop runs from inside the matching worktree.
- The loop logged a warning and kept launching new waves anyway. By shutdown the two branches had diverged 22 vs 18 commits; `Cargo.lock` and several core source files were modified independently on each side.
- No data was lost, but recovery required a manual 3-way merge.

This PRD closes five gaps so the next parallel-slot incident is either prevented at scheduling time, or detected and halted before it cascades:

1. **Path-drift fix** in merge-back (the cause-fix; this alone would have prevented the incident).
2. **Halt after consecutive merge-back failures** (default threshold 2 — single failures are recoverable; two in a row indicate a cascade).
3. **Implicit-overlap detection** for shared-infra files (`Cargo.lock`, `uv.lock`, `package-lock.json`, `go.sum`, etc.) plus a **buildy-task-type heuristic** so FEAT/REFACTOR tasks contend for one shared-infra slot per wave.
4. **Cross-wave file affinity** — un-merged ephemeral-branch files are claimed against future waves; a deadlock guard halts cleanly when every candidate is blocked solely by ephemeral overlap.
5. **Startup hygiene** for stale `{branch}-slot-N` ephemeral branches.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context
- Recomputing slot worktree paths inside merge-back code (the bug we are fixing)
- New parallel matchers for task-id prefixes — reuse `id_body_matches_prefix` from `selection.rs`
- Silent loop continuation past a merge-back failure — must increment counter and surface diagnostics
- Direct edits to `tasks/*.json` or `tasks.db` from production code paths

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: All tests pass with `cargo test` (scoped per iteration; full at REVIEW-001)
- Rust: `cargo fmt --check` passes
- No breaking changes to existing public APIs unless explicitly required by the task description
- All new fields on serde-deserialized structs use `#[serde(default)]` so older configs Just Work
- Reuse existing helpers (`id_body_matches_prefix`, `ensure_worktree`, `ephemeral_slot_branch`, `read_project_config`, `list_other_roots`) — do not fork parallel implementations

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration:

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/parallel-slot-prevention.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` — priority auto-computed; DB + PRD JSON updated atomically                                                     |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

### Files you DO touch

| File                                              | Purpose                                                                |
| ------------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/parallel-slot-prevention-prompt.md`        | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`                      | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; tail or grep instead:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist).

---

## Your Task (every iteration)

1. **Resolve prefix and claim**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/parallel-slot-prevention.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   Output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes`. If no eligible task or unmet `requires`, output `<promise>BLOCKED</promise>` with the reason.

2. **Pull only the progress context you need** — tail the most recent section unless a `dependsOn` task's rationale matters.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do NOT Read `tasks/long-term-learnings.md` directly.

   **Never Read `CLAUDE.md` in full.** The relevant excerpts are embedded below. If a task description cites a section not shown, `grep -n -A 10 '<header>' CLAUDE.md`.

4. **Verify branch** — `git branch --show-current` matches `feat/parallel-slot-prevention`.

5. **Think before coding** — state assumptions; for each `edgeCases`/`failureModes`, note handling. Cross-module data access → consult Data Flow Contracts below or grep call sites.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks).

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`.

10. **Append progress** — ONE block, terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

Tasks FEAT-002, FEAT-003, FEAT-004 declare `modifiesBehavior: true`. When claiming any of them:

1. Read the specific callers/consumers named in the task description.
2. Decide per-caller: `OK` (proceed), `BREAKS` (split via `task-mgr add --stdin`), or `NEEDS_REVIEW` (verify before implementing).
3. If multiple call sites need different handling, split rather than shoehorn.

Specifically for this PRD:
- **FEAT-002**: changes engine.rs wave-loop control flow. Verify the existing `pending_slot_tasks` cleanup at engine.rs:3464-3482 still drains correctly after the new mid-loop reset path runs.
- **FEAT-003**: adds new fields to Task / ProjectConfig / PrdFile. Verify all serde paths default cleanly for old configs/JSONs.
- **FEAT-004**: extends `select_parallel_group` semantics. Verify all existing call sites of `select_parallel_group` still receive correct results when the cross-wave overlay is empty (the no-ephemeral-branch case).

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **REVIEW-001** runs the full gate.

### Per-iteration scoped gate (FEAT / FIX / REFACTOR-FIX tasks)

```bash
# Format → type-check → lint → scoped tests → all in one shot, piping via tee per CLAUDE.md
cargo fmt --check
cargo check
cargo clippy -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -3 && grep "^error" /tmp/clippy.txt | head -10
cargo test <module_or_fn_name> 2>&1 | tee /tmp/test-results.txt | tail -5 && grep "FAILED\|error\[" /tmp/test-results.txt | head -10
```

Scoping heuristic: start from `touchesFiles`. For most tasks in this PRD, `cargo test worktree`, `cargo test selection`, or `cargo test parallel_group` will scope to the right module.

**Do NOT** run the entire workspace test suite (`cargo test` with no filter) during regular iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test 2>&1 | tee /tmp/full-test.txt | tail -10
```

If ANY test fails — including pre-existing — REVIEW-001 fixes them. Default: **attempt every failure**. Pragmatic escape: if more than ~12 failures AND all clearly unrelated, spawn a single `FIX-xxx` task and `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (REVIEW-001 reference)

- New ProjectConfig field defined but not read in `read_project_config` consumers → grep for the field name across `src/`
- New PrdFile field parsed but never reaches the Task struct or DB row
- New Task struct field round-trips through serde but isn't queried in `select_parallel_group`
- Test mocks bypass real wiring → verify production path separately
- Per-task `claimsSharedInfra` stored in DB but never read by selection logic

---

## Review Tasks

| Review         | Priority | Spawns (priority)                  | Focus                                                                                                          |
| -------------- | -------- | ---------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| REFACTOR-001   | 98       | `REFACTOR-FIX-xxx` (50-97)         | DRY, complexity, coupling, deep-nesting drift in worktree.rs (per learning [2003])                             |
| REVIEW-001     | 99       | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) | Idioms, security, error handling, no `unwrap()`, full-suite green, CLAUDE.md updated, mw-datalake repro clean  |

### Spawning follow-up tasks

```sh
echo '{
  "id": "FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From REVIEW-001: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 60,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

`--depended-on-by` syncs DB + PRD JSON atomically — don't edit JSON yourself.

---

## Progress Report Format

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence]
Files: [comma-separated paths]
Learnings: [1-3 bullets, one line each]
---
```

Target ~10 lines per block.

---

## Stop and Blocked Conditions

### Stop Condition

```
<promise>COMPLETE</promise>
```

Verify ALL tasks have `passes: true`, no new tasks created in final review, REVIEW-001 passed full suite.

### Blocked Condition

Document blocker in progress file; create clarification task; output:
```
<promise>BLOCKED</promise>
```

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this task list. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` unless a task explicitly needs a learning that isn't here.

- **[2010]** Test fixtures (`setup_conflicting_slot1`, `compute_slot_worktree_path`) treat git worktree creation in tempdirs as the standard pattern for multi-slot merge tests — extend that pattern, don't introduce mocks.
- **[1870]** Ephemeral branch naming convention: slot 0 reuses the loop's base branch; slots 1+ use `{branch}-slot-{N}`. Always use `ephemeral_slot_branch` (`worktree.rs:540`) — never construct names with `format!()` inline.
- **[1804]** Each ephemeral slot needs its own branch — git enforces one branch per worktree.
- **[1742]** A single git branch cannot be checked out in two worktrees simultaneously. Confirms why slot 1+ MUST have ephemeral branches.
- **[1853]** Worktree paths sanitize branch names while branch refs preserve slashes. `feat/parallel-slot-1` (ref) becomes `feat-parallel-slot-1` (path).
- **[2003]** Deep match nesting (4+ levels) and 150+ line functions in `worktree.rs` / `merge_resolver.rs` are refactor triggers. Keep new helpers focused; extract case-handlers if they grow.
- **[1825]** Slot-aware worktree functions delivered by FEAT-008 already provide the per-slot helpers (`compute_slot_worktree_path`, `ensure_slot_worktrees`). Don't recreate.
- **[1855]** `select_parallel_group` signature has evolved over time — verify current signature in `src/commands/next/selection.rs:436` rather than trust this learning's version. (Current: `(conn, after_files, task_prefix, max_slots)`.)
- **[1755]** Greedy parallel selection: score all eligible, sort by score DESC + priority ASC, accept-or-skip-on-overlap. The implicit-overlap and cross-wave logic must layer on top of this without breaking it.
- **[1756]** Tasks with empty `touchesFiles` always parallelize. The buildy heuristic in FEAT-003 deliberately changes this for FEAT/REFACTOR types — they claim the synthetic infra slot even with empty `touchesFiles`.
- **[2303]** PRD JSONs in main repo and worktree drift; loop reads worktree copy and re-imports each iteration. When debugging tests that touch JSON, check both copies are in sync.

---

## CLAUDE.md Excerpts (only what applies to this change)

These bullets were extracted from `CLAUDE.md` for the subsystems this change touches. Do NOT Read the full file.

### From "Slot merge-back conflict resolution"

`merge_slot_branches_with_resolver` (in `src/loop_engine/worktree.rs`) runs `git merge --no-edit` from slot 0 for each ephemeral slot branch. On a non-zero exit it lists conflicted files and invokes a `MergeResolver`; the engine wires `ClaudeMergeResolver` from `src/loop_engine/merge_resolver.rs`. The resolver's `Resolved` claim is **never trusted**: the caller re-inspects MERGE_HEAD and HEAD post-spawn and downgrades a lying resolver to `failed_slots` with a forced `git reset --hard pre_merge_head`. `SlotFailureKind::ResolverAttempted` vs `PreResolver` lets engine.rs pick the right warning text without string-sniffing.

Note: merge resolution is intentionally NOT part of the shared `iteration_pipeline` — it requires working-tree state owned by `run_wave_iteration`, not the per-slot post-Claude processing block.

### From "Iteration pipeline (shared)"

Sequential (`run_iteration`) and parallel-wave (`run_slot_iteration` + `process_slot_result`) share a single post-Claude pipeline: `process_iteration_output` in `src/loop_engine/iteration_pipeline.rs`. Out of scope for the pipeline (kept at the call sites): wrapper-commit, external-git reconciliation, human-review trigger, rate-limit waits, pause-signal handling, slot merge resolution.

### From "Soft-dep guard for milestone scheduling"

`build_scored_candidates` in `src/commands/next/selection.rs` applies a soft-dep filter using `SPAWNED_FIXUP_PREFIXES = ["REFACTOR-N", "CODE-FIX", "WIRE-FIX", "IMPL-FIX"]`.

**Token-aware exact-prefix matching, never loose substring**: `id_body_matches_prefix` requires the `{prefix}-` boundary at start-of-id OR after a `-`. Bare `id.contains("CODE-FIX")` would false-match `CODE-FIXTURE-1`.

The `BUILDY_TASK_PREFIXES` list FEAT-003 introduces is a **superset** of `SPAWNED_FIXUP_PREFIXES` — keep the relationship documented in code.

### From "task-mgr Workflow Patterns"

- **Syncing JSON changes into a running effort**: NEVER run bare `task-mgr init --from-json` — it wipes status. Use `--append --update-existing`.
- **Never edit `.task-mgr/tasks/*.json` directly** — use the CLI and `<task-status>` tags.
- **Spawn-fixup PRD targeting**: when CODE-REVIEW spawns ad-hoc fixup tasks, the `task-mgr add --stdin` invocation MUST disambiguate the destination via `--from-json` or `--depended-on-by <milestone-of-correct-prd>` — otherwise entries leak into the wrong PRD.

### From "Database Location"

The Ralph loop database is at `.task-mgr/tasks.db` (relative to the project/worktree root). Each worktree has its own copy.

---

## Data Flow Contracts

These are verified access patterns for cross-module data the agent will access. Use these exactly — do NOT guess key types from variable names.

### ProjectConfig → loop engine consumers (FEAT-002, FEAT-003)

```rust
// Source: src/loop_engine/project_config.rs
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    // ... existing fields ...
    #[serde(default = "default_merge_fail_halt_threshold")]
    pub merge_fail_halt_threshold: u32,
    #[serde(default)]
    pub implicit_overlap_files: Vec<String>,
}

fn default_merge_fail_halt_threshold() -> u32 { 2 }

// Loaded via: read_project_config(db_dir: &Path) -> ProjectConfig
//   (project_config.rs:134; returns Default if file absent / parse fails)
```

`read_project_config` already exists. Both new fields are read by:
- `merge_fail_halt_threshold`: engine.rs wave-loop boundary (FEAT-002 adds this read)
- `implicit_overlap_files`: selection.rs `select_parallel_group` (FEAT-003 adds this read)

### PrdFile → tasks (FEAT-003)

```rust
// Source: src/commands/init/parse.rs:150
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrdFile {
    // ... existing fields ...
    #[serde(default)]
    pub implicit_overlap_files: Option<Vec<String>>,
}
```

Threaded through to selection layer. Selection logic unions baseline + ProjectConfig + PrdFile.

### Task → claimsSharedInfra round-trip (FEAT-003)

```rust
// Source: src/models/task.rs (verify camelCase serde rename convention before editing)
// JSON: "claimsSharedInfra": true | false | null (default null)
// DB column: TEXT (nullable, "true"/"false") — confirm the existing pattern for Option<bool> fields
```

Access pattern in `select_parallel_group`:
```rust
let claims = task.claims_shared_infra; // Option<bool>
let claim_via_path = candidate.files.iter().any(|f| basename_in(implicit_set, f));
let claim_via_prefix = id_has_buildy_prefix(&task.id);
let final_claim = match claims {
    Some(true) => true,
    Some(false) => false,
    None => claim_via_path || claim_via_prefix,
};
```

### Slot path lifecycle (FEAT-001)

```rust
// engine.rs:~2840 — produced once per loop run
let slot_worktree_paths: Vec<PathBuf> = ensure_slot_worktrees(project_root, branch_name, num_slots)?;

// engine.rs:~3129 — threaded through WaveParams
struct WaveParams<'a> {
    // ... existing fields ...
    pub slot_paths: &'a [PathBuf],
}

// engine.rs:1430 — passed into merge-back
let outcomes = worktree::merge_slot_branches_with_resolver(
    params.source_root,
    params.branch,
    params.parallel_slots,
    &resolver,
    params.slot_paths, // ← new
);

// worktree.rs:778 — uses slot_paths[0] for slot 0 ops
fn merge_slot_branches_with_resolver(
    project_root: &Path,
    branch_name: &str,
    num_slots: usize,
    resolver: &dyn MergeResolver,
    slot_paths: &[PathBuf],   // ← new
) -> MergeOutcomes {
    let slot0_path = &slot_paths[0]; // ← was: compute_slot_worktree_path(project_root, branch_name, 0)
    // ...
}
```

`compute_slot_worktree_path` is still used elsewhere (slots 1+ in the same function, and `cleanup_slot_worktrees` at worktree.rs:1095 — those are correct, leave them).

### Ephemeral branch enumeration (FEAT-004, FEAT-005)

```rust
// Existing: src/loop_engine/worktree.rs:85
pub(crate) fn list_other_roots(exclude_root: &Path) -> Vec<PathBuf>

// Use the porcelain output of `git worktree list --porcelain` to enumerate
// ephemeral worktrees, then read their HEAD branch via parse_worktree_list
// (worktree.rs:55).

// New helper (FEAT-004):
pub(crate) fn list_unmerged_branch_files(
    repo_path: &Path,
    base_branch: &str,
    ephemeral_branch: &str,
) -> Result<Vec<String>, String>
// Implementation: `git diff --name-only {base}...{ephemeral}` from repo_path.
// Empty Vec on missing branch (clean recovery).

// Branch-name shape (both FEAT-004 and FEAT-005):
let ephemeral = ephemeral_slot_branch(branch_name, slot); // worktree.rs:540 — single source
```

---

## Reference Code

### Existing soft-dep guard pattern (FEAT-003 reuses this exact matcher)

```rust
// src/commands/next/selection.rs:42
const SPAWNED_FIXUP_PREFIXES: &[&str] = &["REFACTOR-N", "CODE-FIX", "WIRE-FIX", "IMPL-FIX"];

// src/commands/next/selection.rs:51
fn id_body_matches_prefix(id: &str, prefix: &str) -> bool {
    // Token-aware boundary match: requires `{prefix}-` at start-of-id or after `-`
    // ... existing impl ...
}

// FEAT-003 adds:
pub(crate) const BUILDY_TASK_PREFIXES: &[&str] = &[
    "FEAT", "REFACTOR", "REFACTOR-N",
    "CODE-FIX", "WIRE-FIX", "IMPL-FIX",
];
// Note: superset of SPAWNED_FIXUP_PREFIXES.

fn id_has_buildy_prefix(id: &str) -> bool {
    BUILDY_TASK_PREFIXES.iter().any(|p| id_body_matches_prefix(id, p))
}
```

### Existing merge-back call site (FEAT-001 modifies this)

```rust
// src/loop_engine/engine.rs:1430
let outcomes = worktree::merge_slot_branches_with_resolver(
    params.source_root,
    params.branch,
    params.parallel_slots,
    &resolver,
);
for (slot, detail, kind) in &outcomes.failed_slots {
    if *kind == worktree::SlotFailureKind::ResolverAttempted {
        eprintln!("Warning: slot {} merge-back failed after Claude resolution attempt: {} ...", slot, detail);
    } else {
        eprintln!("Warning: slot {} merge-back failed: {} ...", slot, detail);
    }
}
```

FEAT-002 augments this loop with the reset/halt-check contract; FEAT-001 adds the new `params.slot_paths` argument.

### Existing test fixture pattern (all FEATs reuse)

```rust
// src/loop_engine/worktree.rs:2485
fn setup_conflicting_slot1(branch: &str) -> (tempfile::TempDir, PathBuf, String) {
    // tempdir + git init + commit base + branch + slot worktree + commit on slot
    // ... existing impl ...
}

// FEAT-001 adds a variant where the test runs from inside slot 0's worktree,
// so compute_slot_worktree_path's recomputation would diverge from the actual path.
```

---

## Feature-Specific Checks

Before claiming a task in this PRD complete, additionally verify:

- **FEAT-001**: pre-fix, `test_merge_slot_branches_succeeds_when_invoked_from_inside_slot0_worktree` should fail with the exact ENOENT message from the incident; post-fix, it passes. Confirm by temporarily reverting the path-passing change.
- **FEAT-002**: with `merge_fail_halt_threshold: 0`, the legacy "log and continue" behavior is preserved bit-for-bit (compare stderr output between old and new code on the same forced-fail input).
- **FEAT-003**: existing parallel-group tests in `src/commands/next/tests.rs` still pass without modification — the buildy heuristic is additive, not breaking.
- **FEAT-004**: when no ephemeral branches exist (cleanly-recovered loop), `select_parallel_group` returns identical results to the current (FEAT-003) implementation. The cross-wave overlay must be a strict superset filter.
- **FEAT-005**: idempotent — running reconciliation twice in a row produces identical state on the second run.

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- Work on the correct branch: **feat/parallel-slot-prevention**
