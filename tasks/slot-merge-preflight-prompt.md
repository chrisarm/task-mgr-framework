# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Slot merge-back preflight + reconcile auto-recovery** for **task-mgr**.

## Problem Statement

In task-mgr's parallel-slot loop engine, `merge_slot_branches_with_resolver`
(`src/loop_engine/worktree.rs:1246`) iterates slots 1..N and runs
`git merge --no-edit <ephemeral>` inside slot 0's worktree. When slot 0 has
uncommitted changes — most commonly the per-PRD progress file
`tasks/progress-<prefix>.txt` appended during the just-finished wave, plus
stray build artifacts (`payrollapp.log.1`, `target/`, etc.) — git aborts the
merge with **"Your local changes to the following files would be overwritten
by merge"** and exits non-zero with **no conflict markers**. The Claude
resolver then correctly short-circuits with `"no conflicts reported, refusing
to spawn"` (it has nothing to act on), and the slot's commits get stranded on
the ephemeral branch. On the next loop startup,
`reconcile_stale_ephemeral_slots` aborts the loop because the stale
ephemeral has un-merged work.

This PRD eliminates the class of problem in task-mgr itself via three layered
fixes, ordered by impact:

1. **Gitignore progress files** (FEAT-001 — primary cause-fix). The per-PRD
   progress file `tasks/progress-*.txt` is the most common source of slot-0
   dirtiness, and slot 1 commits to it too (each wave iteration appends
   progress entries). Adding the pattern to `.gitignore` means git never
   sees the file as dirty, the merge precondition never trips, and
   `cleanup_slot_worktrees` doesn't refuse-on-dirty for slot 1 either.
   One-time migration uses `git rm --cached` to untrack any progress files
   in existing repos without deleting their content on disk.

2. **Stash-based preflight + configurable bounded halt** (FEAT-002/003/004 —
   defense-in-depth). For residual non-progress dirtiness (log files,
   build artifacts the project hasn't gitignored, stray test fixtures), the
   preflight stashes everything dirty before merge and pops after. Pop
   conflicts are warned-and-continued; once per-slot per-run stash count
   exceeds `slot_stash_limit` (ProjectConfig, default 5), the slot is
   demoted to `failed_slots(PreResolver)` and the consecutive-merge-fail
   halt threshold trips. No auto-commit — would pollute base-branch
   history with `chore(progress)` commits.

3. **Reconcile auto-recovery** (FEAT-005 — backfill for legacy strandings).
   Extends `reconcile_stale_ephemeral_slots` to optionally attempt
   automatic merge-back of stale ephemerals at startup using the same
   preflight + resolver path, so pre-existing stranded slots clean up
   automatically.

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
- Auto-commits to the base branch (`chore(progress)` or similar) — the gitignore approach (FEAT-001) makes this unnecessary
- Stashes that leak across iterations because cleanup was skipped on an exit path
- Bare `git stash@{N}` positional refs in production code — always look up by tag
- `git rm` (non-cached) on progress files during migration — only `git rm --cached` is permitted; the file content must survive on disk

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: no warnings in `cargo check`
- Rust: no warnings in `cargo clippy -- -D warnings`
- Rust: scoped tests pass (`cargo test -p task-mgr loop_engine::worktree::` or narrower)
- Rust: `cargo fmt --check` passes
- No breaking changes to existing public APIs unless the task description explicitly authorizes it (signature changes on `merge_slot_branches_with_resolver` and `reconcile_stale_ephemeral_slots` ARE authorized — every caller must be updated in the same task)
- No `unwrap()` in production code paths; use `map_err` with `TaskMgrError` variants

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/slot-merge-preflight.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` — priority auto-computed; DB + PRD JSON updated atomically                                                    |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

### Files you DO touch

| File                                       | Purpose                                                                |
| ------------------------------------------ | ---------------------------------------------------------------------- |
| `tasks/slot-merge-preflight-prompt.md`     | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`               | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration. Two targeted patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on, or a dependsOn task)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing; never crash on absent files.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/slot-merge-preflight.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes` — everything you need. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `task-mgr next` listed a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log. Skip entirely on the first iteration.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, or the task touches a file that's likely documented there, `grep` for the relevant term and read only the surrounding lines. The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, CLAUDE.md excerpts that matter here) are already embedded in **this prompt file**. Prefer it over re-reading source docs.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed (`feat/slot-merge-preflight`). Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section or grep 2-3 existing call sites. Never guess key types from variable names.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — one rejected alternative with a one-line reason is enough.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). Fix failures before committing; never commit broken code.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:` as appropriate).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON.

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---` so the next iteration's tail works.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **REVIEW-001** runs the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (FEAT / FIX / REFACTOR-FIX tasks)

```bash
# Rust — scope tests to the touched module(s)
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test -p task-mgr loop_engine::worktree::            # for worktree.rs touches
cargo test -p task-mgr loop_engine::project_config::      # for project_config.rs touches
cargo test -p task-mgr loop_engine::engine::              # for engine.rs touches

# Pipe to tee + grep per CLAUDE.md "Test & build output":
cargo test -p task-mgr loop_engine::worktree:: 2>&1 | tee /tmp/test-results.txt | tail -10 && grep -E "FAILED|test result" /tmp/test-results.txt | head -20
```

**Do NOT** run the entire workspace test suite during regular iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test 2>&1 | tee /tmp/test.txt | tail -10 && grep -E "FAILED|test result" /tmp/test.txt | head -20
```

If ANY test fails — including pre-existing failures — REVIEW-001 fixes them inline. Above ~12 unrelated failures, spawn a single FIX-xxx with the failing test names and `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (REVIEW-001 reference)

New code must be reachable from production — REVIEW-001 verifies. Most common misses for THIS PRD:

- `prepare_slot0_for_merge` / `cleanup_preparation` defined but never called from `merge_slot_branches_with_resolver` (FEAT-001 ships helpers; FEAT-003 wires them)
- `slot_stash_limit` field added to `ProjectConfig` but never read at the engine.rs call site (always hardcoded 5 instead)
- Auto-recovery wired into reconcile but engine.rs still calls reconcile with `None` for `auto_recovery` (FEAT-004 must update the engine call site)
- `merge_resolver.rs` diagnostic tweak not applied because FEAT-003 forgot the 1-line edit
- New struct field defined but not threaded into the `WaveIterationParams` (or equivalent) used at the call site

---

## Review Tasks

REFACTOR-001 and REVIEW-001 spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

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

`--depended-on-by` wires the new task into REVIEW-001's `dependsOn` AND syncs the PRD JSON atomically. **CRITICAL**: always pass `--depended-on-by REVIEW-001` (or `--depended-on-by REFACTOR-001` for REFACTOR-spawned fixes) so the prefix is unambiguous from the dependency edge — otherwise the new task may land in a different PRD's JSON.

Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it **tight** — future iterations tail this; verbosity here bloats every later context.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block. If your entry is longer than ~25 lines, compress it.

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL tasks have `passes: true`
2. Verify no new tasks were created in final review
3. Verify REVIEW-001 passed with full suite green

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in the progress file
2. Create clarification task via `echo '{...}' | task-mgr add --stdin --depended-on-by <blocked-task>` (priority 0)
3. Output:

```
<promise>BLOCKED</promise>
```

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this task list. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

- **[1806]** In `cleanup_slot_worktrees`, remove worktrees first (tolerating failures if dirty), then delete branches separately. Slot 0 / dirty paths are preserved. Pattern carries over: **never destroy user data on cleanup error** — log, leave the state inspectable, and let the operator decide.
- **[1825]** Slot-aware worktree helpers already exist (`compute_slot_worktree_path`, `ensure_slot_worktrees`, `merge_slot_branches_with_resolver`, `cleanup_slot_worktrees`). Do not reinvent. FEAT-004's auto-recovery should call the same `run_slot_merge_attempt` extracted in FEAT-003, not a parallel implementation.
- **[1805]** After implementing slot-worktree changes, un-ignore the scoped tests immediately and verify. Don't ship code with `#[ignore]` left on tests that this PRD's behavior depends on.
- **[1859]** Slot worktree cleanup ordering: remove worktree directories first via `remove_worktree`, then delete the branches. This PRD does not change cleanup ordering — preflight cleanup is separate from worktree cleanup (different functions, different lifecycles).
- **[2010]** Tests use real git worktrees (`setup_git_repo_with_file` + `ensure_slot_worktrees`), not mocks. The integration tests for FEAT-003 must use this pattern; unit tests for FEAT-001 helpers may use only `setup_git_repo_with_file` since they don't need multi-slot setup.

---

## CLAUDE.md Excerpts (only what applies to this change)

These bullets were extracted from `CLAUDE.md` for the subsystems this change touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file.

### Slot merge-back conflict resolution

- `merge_slot_branches_with_resolver` runs `git merge --no-edit` from slot 0 for each ephemeral slot branch. On non-zero exit it lists conflicted files and invokes a `MergeResolver` (callback seam, `pub(crate) trait`).
- The engine wires `ClaudeMergeResolver` from `src/loop_engine/merge_resolver.rs`, which spawns Claude in slot 0's already-conflicted worktree (`PermissionMode::Auto`, `working_dir = slot0_path`, 600s timeout).
- The resolver's `Resolved` claim is **never trusted**: the caller re-inspects MERGE_HEAD and HEAD post-spawn and downgrades a lying resolver to `failed_slots` with a forced `git reset --hard pre_merge_head`.
- `SlotFailureKind::ResolverAttempted` vs `PreResolver` lets engine.rs pick the right warning text without string-sniffing.
- **Pre-merge preflight (this PRD) is intentionally NOT part of `iteration_pipeline`** — it requires working-tree state owned by the wave-merge step, not per-slot post-Claude processing.

### Parallel-slot scheduling — load-bearing invariants

- **Slot path threading**: `merge_slot_branches_with_resolver` takes `slot_paths: &[PathBuf]` and uses `slot_paths[0]` as slot 0's path, never recomputing via `compute_slot_worktree_path`. This PRD must preserve that — pass slot_paths through unchanged.
- **Slot-0 SAFETY GUARD**: `classify_ephemeral_branch` returns `Err` for slot suffix `0`. **Never broaden** the glob without re-adding the rejection. FEAT-004's auto-recovery iterates the un-merged ephemerals returned by `classify_stale_branches` — that function already honors the guard; do not work around it.
- **Run-level config caching**: `ProjectConfig` is loaded ONCE at `run_loop` startup and threaded through `WaveIterationParams::project_config`. FEAT-003 must read `slot_stash_limit` from this cached reference, NOT call `read_project_config` from the hot path. Mid-loop edits to `.task-mgr/config.json` do NOT take effect — operators must restart the loop.
- **Failed-merge accounting**: `WaveOutcome.failed_merges: Vec<FailedMerge>` carries `(slot, task_id)` as a struct. FEAT-003's StashLimitExceeded demotion must use this same vector — `failed_slots.push((slot, msg, kind))` per the existing pattern.
- **Stale ephemeral branch hygiene at startup**: `reconcile_stale_ephemeral_slots` runs once at loop startup BEFORE `ensure_slot_worktrees`. FEAT-004 extends this function; the slot-0 guard, dirty-worktree case-4 abort, and halt_threshold semantics all stay intact.

### Iteration pipeline (shared)

- Wave merge-back is **out of scope** for the shared `iteration_pipeline` — kept at the call sites because it requires working-tree state. This PRD reinforces that boundary: preflight + cleanup live in `worktree.rs`, not in `iteration_pipeline.rs`.

### task-mgr Workflow Patterns (relevant subsets)

- **Spawn-fixup PRD targeting**: when REVIEW-001 or REFACTOR-001 spawns fixup tasks (`CODE-FIX-`, `WIRE-FIX-`, `IMPL-FIX-`, `REFACTOR-N-`, `FIX-`, `REFACTOR-FIX-`), the `task-mgr add --stdin` invocation MUST disambiguate the destination PRD or the entry leaks into another PRD's JSON. Use `--depended-on-by REVIEW-001` or `--depended-on-by REFACTOR-001` so the prefix is unambiguous from the dependency edge.
- **Permission guard**: loop iterations deny Edit/Write on `tasks/*.json` via `--disallowedTools`. Never edit the JSON yourself — use `<task-status>` tags and `task-mgr add --stdin`.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments.

### Gitignore marker block (FEAT-001)

**Source**: new helper in `src/commands/init/` (e.g., `ensure_progress_gitignore`)
**Sink**: project root `.gitignore` (relative to `task-mgr init` cwd)
**Pattern**: marker-block delimited

```
# task-mgr begin: progress files (untracked)
tasks/progress-*.txt
# task-mgr end: progress files (untracked)
```

Mirrors `merged_attributes_contents` (worktree.rs:467) and `ensure_progress_union_merge` (worktree.rs:517). Idempotent: content outside the markers is preserved; if the block drifts, only the body inside the markers is rewritten.

**Migration sink**: `git rm --cached <path>` for each match from `git ls-files tasks/progress-*.txt`. **Never** `git rm` without `--cached`.

### ProjectConfig → cleanup_preparation (FEAT-002 → FEAT-004)

**Source**: `src/loop_engine/project_config.rs` (FEAT-002 adds `slot_stash_limit: u32`)
**Sink**: `src/loop_engine/worktree.rs::cleanup_preparation(prep: &MergePreparation, slot0_path: &Path, slot: usize, run_id: &str, stash_limit: u32) -> CleanupOutcome`
**Bridge**: `src/loop_engine/engine.rs::merge_slot_branches_with_resolver` call site (~line 1810)

Both `run_id` AND `stash_limit` must reach `cleanup_preparation` — `run_id` for per-run stash-tag scoping, `stash_limit` for the bounded-halt threshold. Dropping either parameter breaks the per-slot per-run guarantee.

```rust
// engine.rs (FEAT-003 wiring):
let outcomes = worktree::merge_slot_branches_with_resolver(
    project_root,
    branch_name,
    num_slots,
    &resolver,
    &slot_paths,
    params.run_id,                               // FEAT-003: thread run_id
    params.project_config.slot_stash_limit,      // FEAT-003: read from cached config
);
```

`params.project_config` is the cached reference — do NOT call `read_project_config()` from the hot path. Read it once at run start (already done elsewhere in `run_loop`).

### ResolverContext fields (FEAT-004 reuse)

**Source**: `src/loop_engine/worktree.rs::ResolverContext` (already defined, ~line 1188)
**Fields**: `slot: usize`, `slot0_path: &Path`, `ephemeral_branch: &str`, `conflicted_files: &[String]`, `pre_merge_head: &str`

FEAT-004 constructs `ResolverContext` for auto-recovery with `slot0_path = project_root` (slot 0 IS the loop's main worktree at startup; no separate slot-0 worktree exists). The `slot` field is the parsed slot number from the ephemeral branch name (already exposed by `classify_ephemeral_branch`).

### AutoRecoveryConfig (FEAT-004 new struct)

**Defines** in `src/loop_engine/worktree.rs` (alongside `MergePreparation` / `CleanupOutcome`):

```rust
pub(crate) struct AutoRecoveryConfig<'a> {
    pub model: &'a str,
    pub effort: &'a str,
    pub claude_timeout: Duration,
    pub signal_flag: Arc<AtomicBool>,
    pub db_dir: Option<&'a Path>,
    pub run_id: &'a str,
    pub stash_limit: u32,
}
```

Field-for-field carries what `ClaudeMergeResolver::new(model, effort, claude_timeout, signal_flag, db_dir)` already takes, PLUS the preflight's `run_id` and `stash_limit`. `reconcile_stale_ephemeral_slots` accepts `Option<&AutoRecoveryConfig<'_>>` as a new trailing parameter; `None` preserves today's abort-on-unmerged behavior bit-for-bit.

Engine startup constructs one before calling reconcile:

```rust
// src/loop_engine/engine.rs early in run_loop (before ensure_slot_worktrees):
let auto_recovery_cfg = AutoRecoveryConfig {
    model: project_config.merge_resolver_model.as_deref().unwrap_or(/* loop default */),
    effort: project_config.merge_resolver_effort.as_deref().unwrap_or("medium"),
    claude_timeout: Duration::from_secs(project_config.merge_resolver_timeout_secs.unwrap_or(600)),
    signal_flag: signal_flag.clone(),
    db_dir: Some(db_dir.as_path()),
    run_id: &run_id,
    stash_limit: project_config.slot_stash_limit,
};
reconcile_stale_ephemeral_slots(
    &project_root, &branch_name,
    project_config.merge_fail_halt_threshold,
    Some(&auto_recovery_cfg),
)?;
```

### Stash tag format (FEAT-001, used by FEAT-003 + FEAT-004)

```
task-mgr-slot-{slot}-{run_id}-{epoch_ms}
```

- `{slot}`: usize, slot index (1..N for wave merge; parsed from ephemeral branch name for reconcile auto-recovery)
- `{run_id}`: &str, current loop's run id (engine: `params.run_id`; reconcile: `AutoRecoveryConfig::run_id`)
- `{epoch_ms}`: u128 (from `SystemTime::now().duration_since(UNIX_EPOCH).as_millis()`)

Per-slot per-run count check uses prefix `task-mgr-slot-{slot}-{run_id}-` (NOT `task-mgr-slot-` — that would let sibling slots and runs trip each other's limits).

### MergePreparation / CleanupOutcome (FEAT-001 → all callers)

```rust
pub(crate) enum MergePreparation {
    Clean,
    Stashed { tag: String },
}

pub(crate) enum CleanupOutcome {
    Restored,
    PopConflict { tag: String, conflicted: Vec<String> },
    StashLimitExceeded { count: usize },
}
```

Variant set is EXACTLY this — no `Committed` variant (auto-commit forbidden), no `AutoResolved` variant (cleanup never destroys user data).

---

## Reference Code

Existing patterns to mirror, not reinvent:

### `format_git_failure` (worktree.rs:1564)

Use this for any `git` subprocess error message in new code:

```rust
fn format_git_failure(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout_str = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr_str = String::from_utf8_lossy(stderr).trim().to_string();
    match (stdout_str.is_empty(), stderr_str.is_empty()) {
        (false, false) => format!("{} | {}", stderr_str, stdout_str),
        (false, true) => stdout_str,
        (true, false) => stderr_str,
        (true, true) => "git failed without output".to_string(),
    }
}
```

### `hard_reset` (worktree.rs:1353) — error-returning git wrapper pattern

```rust
fn hard_reset(repo_path: &Path, commit: &str) -> Result<(), String> {
    let output = Command::new("git")
        .args(["reset", "--hard", commit])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("git reset --hard spawn: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "git reset --hard {} failed: {}",
            commit,
            format_git_failure(&output.stdout, &output.stderr)
        ));
    }
    Ok(())
}
```

Mirror this in `prepare_slot0_for_merge` and `cleanup_preparation`: spawn-map_err + status-check + `format_git_failure` for the failure case.

### `ensure_progress_union_merge` (worktree.rs:517) — historical context

`tasks/progress*.txt` files are configured as `merge=union` driver per-clone. With FEAT-001's gitignore, progress files are no longer tracked, so this union driver becomes vestigial for the progress-file case (still useful if a project tracks a progress-like file unrelated to task-mgr). FEAT-001 leaves `ensure_progress_union_merge` in place — removing it is out of scope.

### Marker-block helper pattern (worktree.rs:424-505)

`merged_attributes_contents` + `ensure_progress_union_merge` show the canonical idempotent marker-block rewrite shape. FEAT-001 mirrors this exact pattern for `.gitignore`:
- `ATTR_MARKER_BEGIN` / `ATTR_MARKER_END` constants delimit the managed body
- Rewrite only when body drifted; idempotent when matching
- User-authored lines outside markers preserved verbatim

REFACTOR-001 may extract a shared helper if both call sites have structurally identical rewrite logic.

### `MergeResolver` trait + `ClaudeMergeResolver` (merge_resolver.rs:220-294)

FEAT-004's auto-recovery reuses the existing `ClaudeMergeResolver`. Constructor signature:

```rust
ClaudeMergeResolver::new(
    model: &str,
    effort: &str,
    claude_timeout: Duration,
    signal_flag: Arc<AtomicBool>,
    db_dir: Option<&Path>,
)
```

Build this from `ProjectConfig.merge_resolver_timeout_secs` / `merge_resolver_effort` (defaults exist), the engine's loop-resolved default model, the loop's signal flag, and the current db_dir.

---

## Feature-Specific Checks

### After FEAT-003 lands, run an end-to-end behavioral smoke (REVIEW-001 task):

```bash
# 1. Create a 2-slot loop on a scratch PRD
# 2. After one wave, leave tasks/progress-<id>.txt dirty in slot 0
# 3. Trigger next wave
# 4. Assert no `chore(progress)` commit appears:
git log --grep '^chore(progress)' main..HEAD       # MUST be empty
# 5. Assert no stashes remain post-wave:
git stash list | grep '^stash@.*task-mgr-slot-'     # MUST be empty
# 6. Multi-worktree: run two loops on the same `.git/`, confirm distinct run_ids in stash tags
```

### After FEAT-004 lands, smoke the auto-recovery path:

```bash
# 1. Create a stranded ephemeral on a scratch repo (commit on feat/x-slot-1, no merge)
# 2. Start loop with auto_recovery=Some
# 3. Confirm reconcile auto-merges and deletes the branch
# 4. Confirm loop continues past startup
# 5. Repeat with auto_recovery=None — confirm today's abort behavior
```

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- Work on the correct branch: **feat/slot-merge-preflight**
