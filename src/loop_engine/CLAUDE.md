# src/loop_engine — design notes

Cross-file narrative for the iterative loop subsystem. Module-level invariants
that touch multiple files; per-file/per-fn contracts live in rustdoc next to
the code. Several specific don't-do-this rules have been migrated to
`task-mgr learn` so they surface via `recall --for-task` — see
[Touchpoints](#touchpoints) for pointers.

## Auto-launch /review-loop after loop end

After a clean loop exit (all tasks complete), `task-mgr` can spawn an interactive
`claude "/review-loop tasks/<prd>.md"` session automatically. The user lands directly
in the review without a manual hand-off step.

**Default behavior**: fires when `autoReview: true` (default) AND `tasks_completed >= autoReviewMinTasks`
(default 3). Both live in `.task-mgr/config.json`. An empty config means both defaults apply.

**CLI overrides** (clap-enforced mutual exclusion):
- `--auto-review` — force on; treats the task-count threshold as 1
- `--no-auto-review` — force off unconditionally

**Batch mode**: ONE review fires at end-of-batch for the LAST successful PRD that met the
threshold — never per-PRD. Earlier PRDs in the batch are skipped even if they individually
qualified.

**Suppression cases** (prints a recovery hint, continues, exit code unchanged):
- Non-TTY stdout (CI, pipes) — hint: re-run interactively to get the review
- `tasks/<prd>.md` not found AND `tasks/prd-<stem>.md` not found — hint: name the markdown file to match
- Worktree path missing or cleaned up — hint: re-run `claude "/review-loop tasks/<prd>.md"` manually

**Process model**: `Command::status()` — blocking spawn, stdin/stdout/stderr inherit so the
review session is fully interactive. `ANTHROPIC_API_KEY` and other env vars inherit automatically.

**Module**: `src/loop_engine/auto_review.rs` — `Decision`, `resolve_decision`, `should_fire`,
`ReviewLauncher` trait, `maybe_fire`.

**Invariant**: auto-review failure NEVER changes the loop or batch exit code.

**Known footgun — paths with whitespace**: `ProcessLauncher::launch`
(`src/loop_engine/auto_review.rs:130`) interpolates the PRD path into a single
slash-command argv element: `format!("/review-loop {}", md.display())`. Claude
re-tokenizes the slash-command body on whitespace, so a PRD path containing
spaces (e.g. `tasks/My PRD.md`) splits into multiple tokens and the review
launch fails to find the file. Not a security issue (no shell, `Command::arg`
is safe), but project convention is space-free `tasks/<feature>.md` paths for
exactly this reason — keep it that way. If the Claude CLI grows a structured
args form, prefer that over in-band quoting.

`maybe_fire` enforces this convention with a launch-boundary guard: if the
resolved markdown path contains any `char::is_whitespace` character, the
launch is suppressed and a stderr hint tells the operator to rename the file
and re-run `/review-loop` manually. The guard sits AFTER `prd_md_path` (so it
sees the actual file we'd hand to Claude) and BEFORE `launcher.launch` (so
no fragmented argv ever reaches `claude`). It deliberately does not attempt
to quote or escape — quoting Claude's slash-command body is brittle, and
suppression with a clear hint is the simpler, more honest contract.

**Outer/inner split for test reachability**: `maybe_fire` is a thin
wrapper that performs the TTY pre-check and delegates to
`maybe_fire_inner` (`pub(crate)`), which contains every launch-decision
gate (decision, worktree existence, markdown path resolution, whitespace
guard, launcher dispatch). `cargo test` runs in a non-TTY env, so a unit
test that goes through the public `maybe_fire` would short-circuit at
the TTY gate before reaching any inner gate — meaning a test asserting
"this guard suppresses launch" via `CapturingLauncher` would pass even
if the guard were deleted. Tests for inner-side gates
(`maybe_fire_inner_*`) call the inner function directly to bypass the
TTY gate and exercise the real guard logic; a single
`maybe_fire_outer_suppresses_in_non_tty` test exercises the outer
wrapper to prove the TTY gate still fires. When adding a new
launch-boundary guard, add it inside `maybe_fire_inner` and test it via
the inner — never via the outer.

## Overflow recovery and diagnostics

When the Claude CLI subprocess returns "Prompt is too long", the loop engine
walks a **four-rung recovery ladder** and writes a diagnostics bundle. Entry
point: `overflow::handle_prompt_too_long` in `src/loop_engine/overflow.rs`,
called from the `PromptTooLong` arm of `run_iteration` in
`src/loop_engine/engine.rs`.

**The ladder** (in order; first rung whose precondition is met wins):

1. **Downgrade effort** — `model::downgrade_effort` (`xhigh → high`). Effort
   never drops below `high` (see `escalate_below_opus` rustdoc on the high-effort
   floor invariant).
2. **Escalate model below Opus** — `model::escalate_below_opus`
   (`haiku → sonnet`, `sonnet → opus`). Closes the Sonnet-default gap that
   used to immediately block the loop on iteration 1.
3. **Escalate to 1M-context Opus** — `model::to_1m_model` (`opus → opus[1m]`).
4. **Block** — task status set to `blocked`; no further recovery attempts.

Rungs 1-3 reset the task status to `todo` (and clear `started_at`) so the next
iteration retries with the override applied; rung 4 sets `blocked`.

**Diagnostics bundle (best-effort; failures log via `eprintln!` and never
propagate)**:

- **Prompt dump**: written to
  `.task-mgr/overflow-dumps/<sanitized-task-id>-iter<n>-<unix-ts>.txt`. Contains
  metadata + per-section byte breakdown + dropped sections + the raw assembled
  prompt. Task IDs are sanitized via `overflow::sanitize_id_for_filename`
  (path-traversal defense; `..` collapsed before allowlist filtering).
- **JSONL event log**: appended one-line-per-event to
  `.task-mgr/overflow-events.jsonl`. Each line is a serialized
  `OverflowEvent` (`ts`, `task_id`, `run_id`, `iteration`, `model`, `effort`,
  `prompt_bytes`, `sections`, `dropped_sections`, `recovery`, `dump_path`).
  `sections` is an ordered JSON array of `[name, size]` pairs (NOT a map).
  `recovery` is a tagged object with discriminator field `action` and
  variant-specific siblings (e.g. `{"action": "escalate_model", "new_model": "..."}`).
- **Rotation**: keeps newest 3 dumps per task ID via
  `overflow::rotate_dumps_keep_n`. Each entry (unreadable dir entry, missing
  metadata, failed deletion) is logged and skipped independently so a single
  IO error never aborts the rest of the rotation pass.

**Banner annotation**: when a task is mid-recovery, the iteration banner emits
`(overflow recovery from <original-model>)` next to the model line. The banner
gates on `IterationContext::overflow_recovered` (a `HashSet<String>` of task
IDs that have hit the overflow handler at least once), NOT on `model_overrides`
— see learning #893: crash escalation and consecutive-failure escalation must
stay in their own channels. The original model is captured first-overflow only
via `IterationContext::overflow_original_model.entry().or_insert_with(...)`.

**Order of operations is contractual** (do not reorder):
ctx update → DB UPDATE → stderr → dump → JSONL → rotate. Recovery state must
be durable before any best-effort observability writes.

## Iteration pipeline (shared)

Sequential (`run_iteration`) and parallel-wave (`run_slot_iteration` +
`process_slot_result`) execution paths share a single post-Claude pipeline:
`process_iteration_output` in `src/loop_engine/iteration_pipeline.rs`. The
module-level rustdoc lists the steps in order (progress logging,
`<key-decision>` extraction, `<task-status>` dispatch, completion ladder
including the `is_task_reported_already_complete` fallback, learning
extraction, bandit feedback, per-task crash tracking) and the two engine.rs
call sites (sequential at ~3204 in `run_loop`, wave at ~1166 in
`process_slot_result`).

**Why a shared pipeline**: before this unification, wave mode silently
skipped behaviors the sequential path treated as core — slot output was
never extracted for new learnings, bandit feedback never updated, and the
completion fallback didn't fire. The single-pipeline contract makes
parity-divergence a compile-time concern (any new step is added in one
place; both call sites pick it up).

**Prompt-builder companion**: `src/loop_engine/prompt/mod.rs` documents the
three-builder layout (`core` / `sequential` / `slot`) plus the main-thread
bundle rule — slot prompts must be built on the main thread before
`thread::spawn` because `rusqlite::Connection` is `!Send`. A compile-time
`Send` assertion on `SlotPromptBundle` enforces this; sections added to the
sequential prompt MUST also be wired through the wave builder so the two
paths cannot drift again.

**Out of scope for the pipeline** (kept at the call sites): wrapper-commit,
external-git reconciliation, human-review trigger, rate-limit waits,
pause-signal handling, slot merge resolution (see "Slot merge-back conflict
resolution" below).

## Slot merge-back conflict resolution

When parallel-slot waves finish, `merge_slot_branches_with_resolver` (in
`src/loop_engine/worktree.rs`) runs `git merge --no-edit` from slot 0 for each ephemeral
slot branch. On a non-zero exit it lists the conflicted files and invokes a `MergeResolver`
(callback seam, `pub(crate) trait`); the engine wires `ClaudeMergeResolver` from
`src/loop_engine/merge_resolver.rs`, which spawns Claude in slot 0's already-conflicted
worktree (`PermissionMode::Auto`, `working_dir = slot0_path`, 600s timeout) with a prompt
that explicitly prohibits push, branch deletion, hard reset outside the merge, and history
rewrites. The resolver's `Resolved` claim is **never trusted**: the caller re-inspects
MERGE_HEAD and HEAD post-spawn and downgrades a lying resolver to `failed_slots` with a
forced `git reset --hard pre_merge_head`. `SlotFailureKind::ResolverAttempted` vs
`PreResolver` lets engine.rs pick the right warning text without string-sniffing.

Note: merge resolution is intentionally NOT part of the shared
`iteration_pipeline` (see "Iteration pipeline (shared)" above) — it requires
working-tree state owned by `run_wave_iteration`, not the per-slot
post-Claude processing block.

### Gitignored progress files (FEAT-001, slot-merge-preflight PRD)

The per-PRD progress file `tasks/progress-<prefix>.txt` is the most common
source of slot-0 dirtiness — slot 1 commits to it on every wave iteration —
and git's merge precondition aborts when slot 0 has uncommitted local
changes to a file the incoming merge would touch (`"Your local changes to
the following files would be overwritten by merge"`, non-zero exit with **no
conflict markers**). The `ClaudeMergeResolver` then correctly short-circuits
because there's nothing to act on, and the slot's commits get stranded.
`task-mgr init` writes/refreshes a managed marker-block in `.gitignore`
covering `tasks/progress-*.txt` and runs a one-time `git rm --cached`
migration so existing repos drop the tracked file from the index without
losing its on-disk content. See `src/commands/init/mod.rs::ensure_progress_gitignore`
and `untrack_progress_files`. **The `git rm --cached` (NOT bare `git rm`)
distinction is load-bearing** — bare `git rm` would delete the file on disk
and lose the operator's loop history.

### Stash-based preflight (FEAT-003 / FEAT-004, slot-merge-preflight PRD)

For residual non-progress dirtiness (log files, build artifacts the project
hasn't gitignored, stray test fixtures), `merge_slot_branches_with_resolver`
runs a stash-based preflight before every per-slot `git merge --no-edit`.
`prepare_slot0_for_merge` stashes everything dirty (tracked + untracked)
under a deterministic tag `task-mgr-slot-{slot}-{run_id}-{epoch_ms}`;
`cleanup_preparation` pops after the merge attempt — successful or not.
Pop conflicts are warned-and-continued (stash retained on stack for operator
inspection), and once `count_stashes_with_prefix` exceeds
`ProjectConfig.slot_stash_limit` (default 5) on the same slot, the slot is
demoted to `failed_slots(PreResolver)` and the FEAT-002 consecutive-merge-fail
halt threshold trips. **Cleanup is structurally guaranteed to run exactly
once per slot** — `run_slot_merge_attempt` was extracted as a helper so
every exit path (rev-parse failure, spawn failure, clean success, any
conflict-handling branch) goes through the same `cleanup_preparation` call.
No auto-commit — that would pollute base-branch history with `chore(progress)`
commits. Stash tags include `run_id` so concurrent loops don't poach each
other's stashes. See `src/loop_engine/worktree.rs::prepare_slot0_for_merge`
and `cleanup_preparation`. `merge_resolver.rs:278` annotates the
"no conflicts reported, refusing to spawn" diagnostic with a preflight
pointer so the next operator who hits a regression knows where to look.

### Reconcile auto-recovery (FEAT-005, slot-merge-preflight PRD)

`reconcile_stale_ephemeral_slots` now accepts an optional
`AutoRecoveryConfig` (model / effort / claude_timeout / signal_flag /
db_dir / run_id / stash_limit). When `Some`, the function attempts an
automatic merge-back of each `CleanUnmerged` stale ephemeral at loop
startup using the same preflight + `ClaudeMergeResolver` path live waves
take — `prepare_slot0_for_merge` → `git merge --no-edit` →
`ClaudeMergeResolver` on conflict → `cleanup_preparation` → `git worktree
remove` + `git branch -D` on success. `slot0_path` is `project_root`
because reconcile runs **before** `ensure_slot_worktrees` — slot 0 IS the
loop's main worktree at startup. Per-branch failures keep the branch in
`unmerged` and fall through to the existing `halt_threshold` abort, with
the message annotated `(auto-recovery attempted and failed for:
<branches>)` so the operator sees which branches the resolver attempted
vs. didn't. When `None`, behavior is byte-for-byte identical to the
pre-FEAT-005 abort path. **Out of scope: case-4 dirty stale worktrees**
still always abort regardless of `auto_recovery` — auto-recovery never
runs on a worktree that has uncommitted work, by design.
Test-injection seam: `reconcile_stale_ephemeral_slots_inner` (pub(crate))
accepts an explicit `&dyn MergeResolver` so unit tests exercise the
resolver-Failed branch without spawning Claude. Engine wiring lives in
`src/loop_engine/engine.rs` at the FEAT-005 reconcile call site (Step 9.5)
— it builds a one-off `AutoRecoveryConfig` from `project_default_model` /
`project_config.merge_resolver_effort` / `merge_resolver_timeout_secs` /
`slot_stash_limit` with a fresh `SignalFlag` and a synthetic
`"startup-reconcile"` run-id (real run-id allocation happens later in
Step 12 `run_cmd::begin`).

## Parallel-slot scheduling

Five layered defenses harden parallel-slot execution against the cascade
that produced the mw-datalake incident (a 2-slot loop whose slot-1
merge-back failed on iteration 1 with a recomputed-slot-path ENOENT,
silently kept launching new waves, and eventually diverged 22-vs-18
commits with un-merged `Cargo.lock` modifications on each side).

### 1. Slot path threading (cause-fix)

`merge_slot_branches_with_resolver` (`src/loop_engine/worktree.rs`) takes
`slot_paths: &[PathBuf]` and uses `slot_paths[0]` as slot 0's path, never
recomputing it via `compute_slot_worktree_path(project_root, branch, 0)`.
The recomputation diverges when the loop runs from inside the matching
worktree — `compute_slot_worktree_path` re-derives a path under
`{parent(project_root)}/{slot0_name}-worktrees/...` while the actual slot 0
worktree IS the project root. Engine threads the paths returned by
`ensure_slot_worktrees` through `WaveParams::slot_worktree_paths`.

`compute_slot_worktree_path` is still correct for slots 1+ inside
`merge_slot_branches_with_resolver` and for `cleanup_slot_worktrees` — only
the slot 0 lookup was wrong.

### 2. Consecutive-merge-fail halt threshold

`ProjectConfig::merge_fail_halt_threshold` (default `2`) caps consecutive
parallel-slot merge-back failure waves before the engine halts. Single
failures are recoverable (next wave gets a clean slate from the
resolver); two-in-a-row indicate a cascading state. The reset/halt
contract is implemented once in
`apply_merge_fail_reset_and_halt_check` (`src/loop_engine/engine.rs`)
and called from the wave-loop boundary — sequential-loop and wave-loop
paths must not re-implement it.

Threshold semantics:
- `0` — never halt (legacy "log and continue" behavior, preserved
  bit-for-bit on the same forced-fail input)
- `1` — halt on any merge-back failure
- `2` (default) — halt after two consecutive merge-back failure waves

### 3. Implicit-overlap baseline + buildy heuristic

`select_parallel_group` in `src/commands/next/selection.rs` serializes
shared-infra contention through a single synthetic `__shared_infra__`
slot per wave. A candidate "claims" the synthetic slot when ANY of:

- (a) some `touchesFiles` entry's basename matches the union of
  `IMPLICIT_OVERLAP_FILES` (Cargo.lock, uv.lock, package-lock.json,
  go.sum, etc. — Rust/Python/JS/Go ecosystems out-of-the-box) ∪
  `ProjectConfig::implicit_overlap_files` ∪
  `PrdFile::implicit_overlap_files` (project + PRD lists EXTEND, do not
  replace, the baseline);
- (b) the task id matches `BUILDY_TASK_PREFIXES` (`FEAT`, `REFACTOR`,
  `REFACTOR-N`, `CODE-FIX`, `WIRE-FIX`, `IMPL-FIX` — superset of
  `SPAWNED_FIXUP_PREFIXES`) via the same token-aware
  `id_body_matches_prefix` matcher used by the soft-dep guard (no
  parallel matcher);
- (c) the task's `claims_shared_infra` field (Option<bool>, migration
  v19) is `Some(true)` — explicit override.

`Some(false)` overrides BOTH (a) and (b); `None` falls through to (a) ∨
(b). This deliberately changes the empty-`touchesFiles` parallelism
baseline — buildy-prefix tasks claim infra even with no listed files.

### 4. Cross-wave file affinity (un-merged ephemeral branches)

`select_parallel_group` accepts `ephemeral_overlay: &[(branch, files)]`
listing files claimed by un-merged ephemeral slot branches from prior
waves. A candidate is deferred when its `touchesFiles` overlap with any
ephemeral branch's claimed set — preventing the same file from being
modified on two divergent branches across waves.

Engine builds the overlay via `worktree::list_unmerged_branch_files`
(`git diff --name-only {base}...{ephemeral}`) for each `{branch}-slot-N`
ephemeral that hasn't merged back. Empty overlay → identical results to
the pre-FEAT-004 implementation (strict superset filter).

**Deadlock guard**: when the greedy pass yields an empty group AND every
candidate's only overlap was ephemeral, `ParallelGroupResult::ephemeral_block_diagnostics`
is populated with named blocking branches. Engine treats this as
equivalent to `failed_merges` non-empty so the FEAT-002 reset/halt
contract fires and the loop halts cleanly with named branches instead
of spinning until stale-iteration abort.

### 5. Stale ephemeral branch hygiene at startup

`reconcile_stale_ephemeral_slots` (`src/loop_engine/worktree.rs`) runs
once at loop startup BEFORE `ensure_slot_worktrees`. For each
`{branch}-slot-N` left over from a prior crash:
- Clean (worktree dir gone, no un-merged commits) → branch deleted, no
  abort.
- Un-merged commits exist AND `halt_threshold > 0` → abort startup
  (the operator must reconcile before the new loop can run).
- Dirty working tree (uncommitted changes) → abort regardless of
  `halt_threshold` (no automated cleanup of unsaved work).

Branch-name shape uses `ephemeral_slot_branch(branch, slot)` (slot 0 is
the loop's base branch; slots 1+ are `{branch}-slot-{N}`). Idempotent —
running twice produces identical state on the second pass.

**Slot-0 SAFETY GUARD (load-bearing)**: `classify_ephemeral_branch`
returns `Err` when the parsed slot suffix is `0`, and
`list_ephemeral_slot_branches` filters `slot > 0`. Production code never
creates a `{branch}-slot-0` ref (slot 0 reuses the base branch directly
in `ensure_slot_worktrees`), but a stray ref from a buggy past version,
manual operator action, or recovery artifact would otherwise classify
as `CleanMerged` with `worktree_path` pointing at the **loop's main
worktree** — `compute_slot_worktree_path(_, branch, 0)` short-circuits
to `compute_worktree_path(_, branch)`. The downstream
`delete_merged_ephemeral` would then `git worktree remove` the loop's
running worktree. Guard MUST hold; never broaden the glob without
adding the slot==0 rejection at the same boundary.

### 6. Run-level config caching (restart required for mid-loop edits)

`ProjectConfig` and the PRD-side `implicit_overlap_files` override are
loaded ONCE at `run_loop` startup and threaded through
`WaveIterationParams` (`prd_implicit_overlap_files`, `project_config`).
`run_wave_iteration`, `apply_merge_fail_reset_and_halt_check`, and the
merge-back resolver setup all read from the cached references — never
call `read_project_config` or `read_prd_implicit_overlap_files` from
inside a wave hot path.

**Mid-loop edits to `.task-mgr/config.json` or the PRD JSON do NOT take
effect** — operators must restart the loop to apply config changes.
Same restart-required semantics every other run-scoped knob already
has (`parallel_slots`, `default_model`, `merge_resolver_*`).
Documenting this here so the next "I changed config and nothing
happened" question has a quick answer.

### 7. Failed-merge accounting: `Vec<FailedMerge>`, not parallel arrays

`WaveOutcome.failed_merges: Vec<FailedMerge>` carries `(slot, task_id)`
as a single struct so the slot/task pairing is a type-level invariant.
The earlier shape (parallel `Vec<usize>` + `Vec<Option<String>>` held
lockstep by rustdoc) was correct but implicit; mismatched lengths would
have silently truncated under `zip`. Don't reintroduce parallel arrays
here, and apply the same shape preference for any future
"slot + per-slot data" aggregation.

**Synthetic-deadlock sentinel (`SYNTHETIC_DEADLOCK_SLOT = usize::MAX`)**:
`handle_ephemeral_deadlock` inserts one entry with this slot index when
every blocking ephemeral branch had a malformed suffix
(`synth_slots.is_empty() && !diagnostics.is_empty()`). Without it,
`failed_merges` would be empty, `apply_merge_fail_reset_and_halt_check`
would reset `consecutive_merge_fail_waves` to 0, and the deadlock
guard would silently spin until the stale-iteration tracker aborted —
defeating the FEAT-002 cascade halt. The diagnostic step special-cases
the sentinel to print `<malformed deadlock blocker>` instead of
synthesizing `{branch}-slot-18446744073709551615`.

General pattern: **any synthesis that translates "we observed a
problem" into "produce a failure record" must always emit at least
one record, even if the upstream parsers all rejected the input** —
otherwise downstream "is_empty" checks invert the safety guarantee.

## Touchpoints

| Concern | File | Symbol |
| --- | --- | --- |
| Slot path threading | `src/loop_engine/worktree.rs` | `merge_slot_branches_with_resolver` |
| Halt threshold contract | `src/loop_engine/engine.rs` | `apply_merge_fail_reset_and_halt_check` |
| Failed-merge struct | `src/loop_engine/engine.rs` | `FailedMerge`, `SYNTHETIC_DEADLOCK_SLOT` |
| Implicit overlap baseline | `src/commands/next/selection.rs` | `IMPLICIT_OVERLAP_FILES`, `BUILDY_TASK_PREFIXES` |
| Cross-wave overlay | `src/loop_engine/worktree.rs` + `src/commands/next/selection.rs` | `list_unmerged_branch_files`, `ephemeral_overlay` parameter |
| Startup hygiene + slot-0 guard | `src/loop_engine/worktree.rs` | `reconcile_stale_ephemeral_slots`, `classify_ephemeral_branch` |
| Run-level config caching | `src/loop_engine/engine.rs` | `WaveIterationParams::project_config`, `prd_implicit_overlap_files` |
| Overflow recovery ladder | `src/loop_engine/overflow.rs` | `handle_prompt_too_long`, `sanitize_id_for_filename`, `rotate_dumps_keep_n` |
| Auto-review launch boundary | `src/loop_engine/auto_review.rs` | `maybe_fire`, `maybe_fire_inner`, `ProcessLauncher` |
| Shared post-Claude pipeline | `src/loop_engine/iteration_pipeline.rs` | `process_iteration_output` |
| Merge resolver | `src/loop_engine/merge_resolver.rs` | `ClaudeMergeResolver`, `MergeResolver` trait |
| Stash preflight | `src/loop_engine/worktree.rs` | `prepare_slot0_for_merge`, `cleanup_preparation`, `run_slot_merge_attempt` |
