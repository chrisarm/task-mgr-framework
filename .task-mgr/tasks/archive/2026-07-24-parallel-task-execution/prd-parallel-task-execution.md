# PRD: Parallel Task Execution + Relationship Simplification

**Type**: Feature
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-04-21
**Status**: Draft

---

## 1. Overview

### Problem Statement

The task-mgr loop engine executes tasks sequentially: select one task, spawn Claude, wait for completion, repeat. For PRDs with many independent tasks (touching disjoint files), this leaves throughput on the table. Two tasks editing `src/foo.rs` and `src/bar.rs` respectively could safely run in parallel, completing a PRD in half the wall-clock time.

Separately, the `/tasks` and `/plan-tasks` skills have been updated to drop `synergyWith`/`batchWith`/`conflictsWith` relationship types. These were manually authored hints about task affinity that are better derived at runtime from `touchesFiles` overlap. The scoring algorithm should be simplified to use file-overlap data directly, and that same data becomes the conflict-detection mechanism for parallel execution.

### Background

The current selection algorithm in `src/commands/next/selection.rs` scores tasks on 4 dimensions: `priority_score` (1000 - priority), `file_score` (10 * files overlapping with `--after-files`), `synergy_score` (3 * synergy rels to recently completed), and `conflict_score` (-5 * conflict rels to recently completed). The synergy/batch/conflict relationships are stored in the `task_relationships` table and populated during PRD import from JSON fields.

The `task_files` table already stores every task's `touchesFiles` with indexes on both `task_id` and `file_path`, enabling efficient forward and reverse lookups. This is the foundation for file-based conflict detection: two tasks that share any file in `touchesFiles` must not execute simultaneously.

The loop engine (`src/loop_engine/engine.rs`) uses tokio only for signal handling; Claude subprocesses are spawned synchronously via `std::process::Command`. WAL mode and `busy_timeout=5000` are already enabled on all SQLite connections (`src/db/connection.rs:97-108`).

---

## 2. Goals

### Primary Goals

- [ ] Remove `synergyWith`, `batchWith`, `conflictsWith` from scoring, import, export, and prompt generation
- [ ] Add `select_parallel_group()` that selects up to N tasks with disjoint `touchesFiles`
- [ ] Add `--parallel N` flag to `task-mgr loop` (default 1, max 3) that runs non-conflicting tasks simultaneously in separate git worktrees
- [ ] Implement wave-based execution: select N tasks, spawn N Claude subprocesses, wait for all to complete, repeat
- [ ] Maintain full backward compatibility: `--parallel 1` (default) behaves identically to current sequential execution

### Success Metrics

- `task-mgr next --parallel 3` returns up to 3 tasks with zero file overlap
- `task-mgr loop --parallel 2` on a PRD with 2+ disjoint tasks completes both in one wave
- Two tasks sharing a file are NEVER placed in the same parallel group
- Old PRD JSON containing `synergyWith`/`batchWith`/`conflictsWith` parses without error
- All existing tests pass with zero regressions when running with `--parallel 1`

---

## 2.5. Quality Dimensions

### Correctness Requirements

- File conflict detection must be conservative: if task A's `touchesFiles` overlaps with task B's `touchesFiles` by even one path, they must NOT run in the same wave. False positives (refusing to parallelize safe tasks) are acceptable; false negatives (running conflicting tasks together) are not.
- Task status transitions must be atomic: claiming N tasks for a wave must either claim all or none. A partial claim that crashes leaves orphaned `in_progress` tasks (the existing recovery logic handles this, but it wastes an iteration).
- Git reconciliation after a wave must not lose commits. Each slot commits to the same branch in its own worktree; `git pull --rebase` syncs them. Since files are disjoint, rebase should always succeed — but if it fails, the error must be logged and the next wave must continue (not crash the loop).

### Performance Requirements

- Parallel group selection must remain sub-10ms for 200 tasks. The inverted file index is built in-memory from the existing `get_all_task_files()` HashMap — no additional SQL queries.
- Wave overhead (worktree setup, git sync) should be < 5 seconds per wave. Worktrees are created once at loop start and reused across waves.
- Inter-wave delay matches the existing `iteration_delay_secs` (default 2s). No per-slot delay.

### Style Requirements

- Follow existing `src/loop_engine/` patterns: functions return `TaskMgrResult`, errors logged to stderr with `eprintln!`, non-fatal failures don't crash the loop.
- Thread spawning uses `std::thread::spawn` (not tokio tasks) — consistent with the synchronous subprocess model.
- No `.unwrap()` on thread joins — handle `JoinHandle` panics gracefully.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|---|---|---|
| All eligible tasks share the same file | Parallel group selection degenerates to 1 task | Return group of 1; loop runs sequentially for that wave |
| Task has empty `touchesFiles` | No file data to conflict on | Can always parallelize — empty set has no overlap with any set |
| One slot crashes, others succeed | Partial wave failure | Crashed slot's task resets to `todo` (existing retry logic); successful slots' results are preserved; wave is still counted |
| Git merge fails after wave | Ephemeral branch can't merge back to main despite disjoint files | Log error, skip the failed slot's merge, exclude slot from next wave. Task is NOT marked failed — the code change was committed on the ephemeral branch, only the merge-back failed |
| PRD re-import mid-wave | Claude in one slot edits the PRD JSON | PRD hash check runs between waves (not mid-wave); re-import happens before next wave's selection |
| Signal (SIGINT) during wave | Need to terminate all N Claude subprocesses | Existing `SignalFlag` is `Arc<AtomicBool>` — shared across all slot threads. Each slot's watchdog kills its child on signal |
| `touchesFiles` is incomplete (task edits files not listed) | File conflict not detected | Accepted risk — `touchesFiles` is a best-effort declaration. Users should list all files. Git rebase catches actual conflicts as a safety net |
| Rate limit hit by one slot | API returns 429 for that slot's Claude call | Slot returns `IterationOutcome::RateLimit`. Wave completes normally for other slots. Next wave respects the existing usage-check-and-wait logic |

---

## 3. User Stories

### US-001: Parallel Loop Execution

**As a** developer running autonomous coding loops
**I want** multiple non-conflicting tasks to execute simultaneously
**So that** PRDs with many independent tasks complete faster

**Acceptance Criteria:**

- [ ] `task-mgr loop --parallel N` spawns up to N Claude subprocesses per wave (N=1,2,3)
- [ ] Tasks in the same wave touch disjoint sets of files
- [ ] Each parallel task runs in its own git worktree on the same branch
- [ ] Wave completes when ALL slots finish (wave model)
- [ ] Git commits from all slots are synced after each wave
- [ ] `--parallel 1` (default) is identical to current sequential behavior

### US-002: File-Based Conflict Detection in Task Selection

**As a** task-mgr user
**I want** the `next` command to identify tasks safe to run in parallel
**So that** I can see which tasks would conflict and which wouldn't

**Acceptance Criteria:**

- [ ] `task-mgr next --parallel N` returns up to N non-conflicting tasks as JSON
- [ ] Two tasks with any shared file path in `touchesFiles` are never in the same group
- [ ] Tasks with empty `touchesFiles` can always be included (no conflict possible)
- [ ] Group is ordered by score (highest-priority task first)

### US-003: Deprecate Relationship-Based Scoring

**As a** task-mgr maintainer
**I want** `synergyWith`/`batchWith`/`conflictsWith` relationships removed from scoring
**So that** the selection algorithm is simpler and file-overlap data drives all affinity decisions

**Acceptance Criteria:**

- [ ] Selection scoring uses only `priority_score + file_score` (no synergy/conflict components)
- [ ] Old PRD JSON with these fields parses without error (silently ignored on import)
- [ ] `task-mgr show` no longer displays deprecated relationship types
- [ ] `task-mgr export` no longer emits deprecated relationship fields
- [ ] Synergy prompt section returns empty (no DB queries)
- [ ] Calibration only tunes `file_overlap` and `priority_base` weights

---

## 4. Functional Requirements

### FR-001: Simplify Selection Scoring

Remove `synergyWith`, `batchWith`, `conflictsWith` from the selection algorithm. The scoring formula becomes `total = priority_score + file_score`.

**Details:**

- Remove `SYNERGY_BONUS` and `CONFLICT_PENALTY` constants from `selection.rs`
- Remove `synergy_score`, `conflict_score`, `synergy_from`, `conflict_from` from `ScoreBreakdown`
- Remove `batch_with` from `ScoredTask` and `batch_tasks` from `SelectionResult`
- Delete `get_eligible_batch_tasks()` function
- Keep `dependsOn` relationship handling unchanged (dependency filtering)

**Validation:**

- Existing selection tests updated to reflect 2-dimension scoring
- No regression in task ordering for PRDs that never used synergy/batch/conflict

### FR-002: Parallel Group Selection

Add `select_parallel_group()` to `selection.rs` that returns up to N non-conflicting tasks.

**Details:**

- Reuse existing scoring/filtering internals to get sorted eligible tasks
- Build inverted file index: `HashMap<&str, HashSet<usize>>` mapping file_path to indices of already-selected tasks
- Greedy algorithm: for each candidate in score order, check if ANY of its files appear in the inverted index. If no overlap, add to group and update the index.
- Tasks with empty `touchesFiles` have no entries in the index, so they never conflict
- Return up to `max_slots` tasks

**Validation:**

- Unit tests: conflicting tasks excluded, disjoint tasks included, empty files always parallel, max_slots respected, priority ordering preserved

### FR-003: `--parallel N` CLI Flag

Add `--parallel` option to both `loop` and `next` subcommands.

**Details:**

- `loop` subcommand: `--parallel <N>` (default 1, max 3). Also readable from `LOOP_PARALLEL` env var.
- `next` subcommand: `--parallel <N>` (default 1). When >1, returns array of non-conflicting tasks.
- Config: `parallel_slots: usize` field on `LoopConfig` with env var support.

**Validation:**

- `--parallel 0` rejected with error
- `--parallel 4` rejected with error ("max 3")
- `--parallel 1` produces identical output to current behavior

### FR-004: Per-Slot Worktree Management

Each parallel slot gets its own git worktree on an ephemeral branch.

**CONSTRAINT:** Git worktrees cannot share a branch — `git worktree add` refuses to check out a branch already checked out in another worktree. The solution is per-slot ephemeral branches.

**Details:**

- Slot 0 reuses the existing branch worktree on the main PRD branch (from `ensure_worktree()`)
- Slots 1-2 get worktrees at `{repo-parent}/{repo-name}-worktrees/{sanitized-branch}-slot-{N}/` on ephemeral branches `{branch}-slot-{N}` forked from the main branch
- Ephemeral branches are created at loop start and reused across waves
- After each wave: ephemeral branches merge back into the main branch via `git merge --no-edit` in slot 0's worktree. Merge is conflict-free because `touchesFiles` disjointness guarantees disjoint diffs. Then slot 1+ worktrees fast-forward to the updated main branch.
- Cleanup on loop exit: removes slot worktrees and deletes ephemeral branches

**Validation:**

- Worktrees created with correct paths on separate branches
- Merge after wave succeeds with disjoint file changes
- Cleanup removes slot worktrees and ephemeral branches

### FR-005: Wave-Based Parallel Execution

The loop dispatches N tasks per wave using `std::thread::spawn`.

**Details:**

- Before spawning: select parallel group, claim all tasks sequentially (one DB transaction)
- Spawn: one `std::thread` per slot, each running a simplified `run_slot_iteration()` in its worktree
- Each thread opens its own `rusqlite::Connection` (WAL mode already enabled — supports concurrent readers + serialized writers)
- `run_slot_iteration()` does NOT share `IterationContext` — each slot has its own minimal state (activity epoch for watchdog, timeout config). Crash tracking, session guidance, reorder hints, and stale tracking are managed by the main thread between waves.
- Wait: `JoinHandle::join()` on all threads. Wave completes when all slots finish.
- After wave (sequentially on main thread): merge slot branches back to main branch, log progress for each slot, update iteration context from merged results
- CrashTracker policy: if ALL slots in a wave crash, increment crash counter (eventual abort). If ANY slot succeeds, reset the counter.
- Progress logging happens sequentially on the main thread after the wave — avoids concurrent file append concerns.
- One wave = one iteration toward the `max_iterations` budget
- `run_loop()` is `async fn` but uses tokio only for signal setup — `thread::spawn` + `join()` blocking is acceptable since the existing sequential path already blocks on `child.wait()`

**Validation:**

- Two non-conflicting tasks complete in one wave
- Signal terminates all slots
- Crash in one slot doesn't affect others
- Sequential path (`--parallel 1`) unchanged

### FR-006: Stop Importing Deprecated Relationships

During PRD import (`init`/`add`), silently ignore `synergyWith`, `batchWith`, `conflictsWith`.

**Details:**

- `insert_task_relationships()` in `import.rs`: skip the three deprecated loops
- Log a one-time deprecation warning when deprecated fields are non-empty
- Keep fields on `PrdUserStory` with `#[serde(default)]` for backward compat
- `dependsOn` continues importing normally

**Validation:**

- Old PRD JSON with all 4 relationship types: `dependsOn` imported, others silently ignored
- New PRD JSON without deprecated fields: parses normally

### FR-007: DB Migration v18

Add `slot` column to `run_tasks` for tracking which parallel slot executed a task.

**Details:**

- `ALTER TABLE run_tasks ADD COLUMN slot INTEGER NOT NULL DEFAULT 0;`
- v17 is reserved for the recall-scores-and-supersession PRD (`learning_supersessions` table)
- Leave `task_relationships` CHECK constraint unchanged — permits deprecated types, new code simply won't insert them
- Down migration: SQLite can't drop columns; revert version number only

**Validation:**

- Migration up: `slot` column exists with default 0
- Migration down: version reverts to 17
- Existing `run_tasks` rows get `slot = 0` (backward compat)

### FR-008: Fix Archive Progress File Bug

`src/loop_engine/archive.rs:256` hard-codes `tasks_dir.join("progress.txt")` but progress files are per-prefix (`progress-{PREFIX}.txt`). Note: the archive code at lines 126-131 already correctly skips all progress files from being moved to the archive directory — only the learning-extraction path at line 256 is wrong.

**Details:**

- Change the learning-extraction path to glob for progress files matching the archived PRD's prefix: `progress-{prefix}.txt`
- If no prefix is available, fall back to `progress.txt` (unprefixed)
- Do NOT glob all `progress-*.txt` files indiscriminately — only extract learnings from the prefix matching the PRD being archived

**Validation:**

- Archive with prefix `P1` extracts learnings from `progress-P1.txt`
- Archive without prefix extracts from `progress.txt`
- Progress files from other prefixes are not touched

---

## 5. Non-Goals (Out of Scope)

- **Eager slot refill**: When one slot finishes before others, it waits. A work-stealing pool would increase throughput but adds mid-wave conflict recalculation complexity. Future enhancement.
- **Per-slot branches**: Each slot works on the same branch. Per-slot branches with merge-back add isolation but require merge conflict resolution that the disjoint-file guarantee already avoids.
- **Auto-detect optimal parallelism**: `--parallel 0` for auto-detection is deferred. Users explicitly choose 1, 2, or 3.
- **Context-economy placeholders**: `{{PROHIBITED_OUTCOMES}}`, `{{GLOBAL_ACCEPTANCE_CRITERIA}}` etc. are prompt template changes unrelated to parallel execution. Separate PRD.
- **Full-suite quality gates for REVIEW/MILESTONE tasks**: Convention-based test suite triggering is orthogonal. Separate PRD.
- **Removing `RelationshipType` enum variants**: The enum keeps all 4 variants for DB backward compat. Code paths that handle deprecated types become no-ops, but the variants remain parseable.

---

## 6. Technical Considerations

### Affected Components

| File | Change |
|---|---|
| `src/commands/next/selection.rs` | Remove relationship scoring; add `select_parallel_group()` |
| `src/commands/next/output.rs` | Remove batch/synergy/conflict from output structs |
| `src/commands/next/mod.rs` | Add `--parallel` support; wire to parallel group selection |
| `src/cli/commands.rs` | Add `--parallel` flag to `loop` and `next` subcommands |
| `src/loop_engine/config.rs` | Add `parallel_slots` to `LoopConfig`; `LOOP_PARALLEL` env var |
| `src/loop_engine/engine.rs` | Wave-based dispatch in main loop; `run_slot_iteration()`; `run_parallel_wave()` |
| `src/loop_engine/worktree.rs` | Per-slot worktree creation/cleanup functions |
| `src/loop_engine/progress.rs` | Slot-aware iteration logging |
| `src/loop_engine/prompt_sections/synergy.rs` | Gut to no-ops (return empty string / primary values only) |
| `src/loop_engine/calibrate.rs` | Remove synergy/conflict from `SelectionWeights` and calibration |
| `src/loop_engine/archive.rs` | Fix hard-coded `progress.txt` to use prefix-aware paths |
| `src/commands/init/import.rs` | Skip deprecated relationship insertion |
| `src/commands/init/parse.rs` | Keep deprecated fields for backward compat serde |
| `src/commands/export/prd.rs` | Remove deprecated relationship export |
| `src/commands/show.rs` | Remove deprecated relationship display |
| `src/models/relationships.rs` | Keep enum variants but mark deprecated paths as no-ops |
| `src/db/migrations/v18.rs` | New: `slot` column on `run_tasks` |
| `src/db/migrations/mod.rs` | Register v18 |

### Dependencies

- No new external crate dependencies. `std::thread` for parallelism, existing `rusqlite` for per-thread connections, existing `fs2` for locking.
- SQLite WAL mode already enabled (`src/db/connection.rs:99`) — supports concurrent readers + serialized writers across threads. Each slot thread calls `open_connection()` which inherits these pragmas automatically.
- `busy_timeout=5000` already set (`src/db/connection.rs:108`) — writers wait instead of failing on contention.

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| **A: std::thread per slot (wave model)** | Simple, debuggable, no async runtime changes. Wave model avoids mid-wave conflict recalculation. Each thread gets own DB connection (WAL-safe). | Slots idle while waiting for slowest task in wave. Thread overhead per wave (~1ms). | **Preferred** |
| **B: tokio::spawn per slot** | Could reuse existing tokio runtime. Lighter weight than OS threads. | Current Claude spawning is synchronous (`Command::new`). Would need to wrap in `spawn_blocking`. Adds async complexity to a fundamentally sync workflow. | Rejected |
| **C: Work-stealing pool** | Maximum throughput — free slot immediately picks next task. | Mid-wave conflict detection is complex (selected tasks change as slots complete). Race conditions on task claiming. Hard to reason about git state. | Rejected (future enhancement) |

**Selected Approach**: A — `std::thread` with wave model. The wave boundary is a clean synchronization point: all slots finish → git sync → select next group → repeat. This matches the existing loop's iteration-boundary model and avoids concurrency complexity.

**Phase 2 Foundation Check**: The wave model is the right foundation. Work-stealing (option C) is a pure optimization on top — the parallel group selection algorithm, per-slot worktrees, and git sync all transfer directly. The 1-wave-per-iteration accounting in `run_tasks` also extends naturally to work-stealing by adding a `wave_id` column later.

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| **A: Same-branch worktrees, git pull after wave** | Simple. File-disjointness guarantees no merge conflicts. No branch management overhead. | If `touchesFiles` is incomplete, rebase can fail (but this is logged and recovered). | **Preferred** |
| **B: Per-slot branches, merge to main after wave** | Full isolation even with incomplete `touchesFiles`. | Branch creation/deletion per wave. Merge conflicts possible if branches diverge. More git operations. | Rejected |

**Selected Approach**: A — same-branch worktrees. The `touchesFiles` disjointness guarantee makes merge conflicts impossible for correctly-declared tasks. Git rebase serves as a safety net for incomplete declarations.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| `touchesFiles` incomplete — tasks edit undeclared files causing git conflicts | Medium (one wave's git sync fails; task not lost but needs retry) | Medium | Git rebase failure is caught and logged. Failed slot excluded from next wave. `/tasks` skill instructs thorough file listing. |
| API rate limits hit faster with parallel Claude calls | Medium (429 errors waste iterations) | Medium | Max 3 slots caps concurrent API usage. Existing usage-check-and-wait runs between waves. Rate-limited slot returns `RateLimit` outcome; next wave pauses if threshold exceeded. |
| SQLite write contention between slot threads | Low (brief delays) | Low | WAL mode + `busy_timeout=5000` already configured. Each slot writes only its own task status (small, fast transactions). |

### Security Considerations

- No new user-facing input reaches SQL — parallel group selection uses the same parameterized queries as existing selection.
- Per-slot worktrees are created in the existing `{repo}-worktrees/` directory with the same permission model.
- Signal handling unchanged — `SIGINT`/`SIGTERM` propagate to all slot threads via shared `Arc<AtomicBool>`.

### Public Contracts

#### New Interfaces

| Module/Function | Signature | Returns (success) | Returns (error) | Side Effects |
|---|---|---|---|---|
| `commands::next::selection::select_parallel_group` | `(conn: &Connection, after_files: &[String], recently_completed: &[String], task_prefix: Option<&str>, max_slots: usize) -> TaskMgrResult<Vec<ScoredTask>>` | `Vec<ScoredTask>` (1..=max_slots tasks with disjoint files) | `TaskMgrError` | None (read-only) |
| `loop_engine::worktree::ensure_slot_worktrees` | `(project_root: &Path, branch_name: &str, num_slots: usize, yes_mode: bool) -> TaskMgrResult<Vec<PathBuf>>` | `Vec<PathBuf>` (one path per slot; slot 0 = main worktree) | `TaskMgrError` | Creates git worktrees on ephemeral branches `{branch}-slot-{N}` |
| `loop_engine::worktree::merge_slot_branches` | `(main_worktree: &Path, branch_name: &str, slot_worktrees: &[(usize, PathBuf)]) -> TaskMgrResult<Vec<MergeOutcome>>` | `Vec<MergeOutcome>` (success/failure per slot) | `TaskMgrError` | Merges ephemeral branches into main, fast-forwards slots |
| `loop_engine::worktree::cleanup_slot_worktrees` | `(project_root: &Path, branch_name: &str, num_slots: usize) -> TaskMgrResult<()>` | `()` | `TaskMgrError` | Removes slot worktrees and ephemeral branches |
| `loop_engine::engine::run_parallel_wave` | `(slots: Vec<SlotContext>, params: &WaveParams) -> TaskMgrResult<WaveResult>` | `WaveResult { outcomes: Vec<(usize, IterationResult)>, wave_duration: Duration }` | `TaskMgrError` | Spawns threads, claims tasks, modifies DB |

#### Modified Interfaces

| Module/Function | Current Signature | Proposed Signature | Breaking? | Migration |
|---|---|---|---|---|
| `commands::next::selection::ScoredTask` | Has `batch_with: Vec<String>` | Field removed | Yes (struct change) | Internal only — no external consumers |
| `commands::next::selection::ScoreBreakdown` | Has `synergy_score`, `conflict_score`, `synergy_from`, `conflict_from` | Fields removed | Yes (struct change) | Internal only — no external consumers |
| `commands::next::selection::SelectionResult` | Has `batch_tasks: Vec<String>` | Field removed | Yes (struct change) | Internal only — no external consumers |
| `commands::next::output::ScoreOutput` | Has `synergy`, `conflict`, `synergy_from`, `conflict_from` | Fields removed | Yes (JSON output change) | Additive removal — JSON consumers using `serde_json` ignore missing fields |
| `commands::next::output::NextTaskOutput` | Has `batch_with: Vec<String>` | Field removed | Yes (JSON output change) | Same as above |
| `loop_engine::calibrate::SelectionWeights` | Has `synergy: i32`, `conflict: i32` | Fields removed | Yes (internal struct) | Internal only |
| `loop_engine::config::LoopConfig` | No `parallel_slots` | Adds `parallel_slots: usize` (default 1) | No (additive) | N/A |
| `loop_engine::progress::log_iteration` | No slot parameter | Adds `slot: Option<usize>` | Yes (signature change) | All callers updated in same PR |

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
|---|---|---|
| task_files → inverted index → conflict check | `HashMap<String, Vec<String>>` (task_id→files) from `get_all_task_files()` → inverted to `HashMap<&str, HashSet<usize>>` (file→selected_indices) | `let task_files = get_all_task_files(conn, prefix)?; let mut file_to_selected: HashMap<&str, HashSet<usize>> = HashMap::new(); for (i, task) in group.iter().enumerate() { for f in task_files.get(&task.id).unwrap_or(&vec![]) { file_to_selected.entry(f.as_str()).or_default().insert(i); } }` |
| parallel group → wave → slot threads | `Vec<ScoredTask>` from `select_parallel_group()` → zipped with `Vec<PathBuf>` worktree paths → `Vec<SlotContext> { slot_index, working_root, task }` | `let group = select_parallel_group(conn, &after_files, &completed, prefix, slots)?; let slot_ctxs: Vec<SlotContext> = group.into_iter().zip(worktree_paths.iter()).enumerate().map(\|(i, (task, wt))\| SlotContext { slot_index: i, working_root: wt.clone(), task }).collect();` |
| wave result → iteration context | `WaveResult { outcomes: Vec<(usize, IterationResult)> }` → merged into `IterationContext` (last_files, crash_tracker, etc.) | `for (slot, result) in wave_result.outcomes { ctx.last_files.extend(result.files_modified); if matches!(result.outcome, IterationOutcome::Completed) { tasks_completed += 1; } }` |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|---|---|---|---|
| `src/loop_engine/engine.rs:122-124` | Queries synergies/conflicts/batches in `select_next_task` | CHANGES — these queries removed | Selection simplified to 2 dimensions |
| `src/loop_engine/engine.rs:504-509` | Uses `cluster_effort` from synergy cluster resolution | CHANGES — cluster effort = primary task effort | `resolve_synergy_cluster()` returns primary values directly |
| `src/loop_engine/prompt.rs` (synergy section call) | Calls `build_synergy_section()` | OK — returns empty string | No prompt change visible to Claude |
| `src/loop_engine/calibrate.rs:178-180` | Computes synergy/conflict correlations | CHANGES — removed | Calibration uses 2 dimensions only |
| `src/commands/next/output.rs:162-182` | Maps `ScoreBreakdown` to `ScoreOutput` | CHANGES — fewer fields | JSON output loses deprecated score fields |
| `src/commands/show.rs:71-83` | Groups relationships by type | CHANGES — only `DependsOn` displayed | Show output simpler |
| `src/commands/export/prd.rs:274-281` | Exports all 4 relationship types | CHANGES — only `dependsOn` exported | Exported JSON cleaner |
| `src/loop_engine/archive.rs:256` | Hard-codes `progress.txt` | CHANGES — uses prefix-aware glob | Archive extracts learnings from all progress files |

### Inversion Checklist

- [x] All callers of synergy/batch/conflict scoring identified (selection.rs, calibrate.rs, output.rs)
- [x] All callers of `build_synergy_section` / `resolve_synergy_cluster` identified (prompt.rs, engine.rs)
- [x] All importers of deprecated relationships identified (init/import.rs, add.rs)
- [x] All exporters identified (export/prd.rs)
- [x] All displayers identified (show.rs, next/output.rs)
- [x] Tests that validate synergy/batch/conflict behavior identified (selection tests, calibrate tests, relationship model tests)
- [x] Thread safety of shared state verified (SignalFlag is Arc<AtomicBool>, DB connections are per-thread, worktrees are per-slot)

### Documentation

| Doc | Action | Description |
|---|---|---|
| `CLAUDE.md` | Update | Add `--parallel` flag to Loop CLI Cheat Sheet; note v18 migration; document slot worktree paths |

---

## 7. Open Questions

- [ ] Should `task-mgr status` show per-slot progress when a parallel loop is running? (Leaning yes but may be phase 2)
- [ ] Should the wave iteration count toward `max_iterations` as 1 (current plan) or as N (one per task completed)? Current plan: 1 wave = 1 iteration, which means `max_iterations` needs to be lower for parallel runs. Alternative: count completed tasks, not waves.

---

## Appendix

### Related Learnings from Institutional Memory

- **#1448**: Stub migration pattern with `#[ignore]` tests enables TDD database changes — use for v18
- **#15**: Comprehensive test coverage for migrations: up, down, defaults, writes
- **#1027**: Migration tests should use >= assertions for schema version
- **#1251**: Multiple modules may have ignored tests gated on the same migration — check when adding v18
- **#1549**: Worktree DB needs migrate before smoke tests — relevant for per-slot worktrees
- **#1444**: Soft-archive queries need `archived_at IS NULL` filters — ensure new parallel queries include this
- **#178**: Single SQL query to avoid N+1 when resolving synergy partners — this optimization goes away with synergy removal
- **#418**: `TimeoutConfig` re-export needed when extracting to watchdog module — relevant if `run_slot_iteration` needs timeout config

### Glossary

- **Wave**: A set of N tasks dispatched simultaneously, one per slot. The loop waits for all slots to complete before starting the next wave.
- **Slot**: One parallel execution lane. Slot 0 reuses the main worktree; slots 1-2 get dedicated worktrees.
- **File conflict**: Two tasks whose `touchesFiles` arrays share at least one file path. Conflicting tasks must not run in the same wave.
- **Inverted file index**: A `HashMap<file_path, task_indices>` built from `task_files` data, used for O(1) conflict checking during parallel group selection.
- **WAL mode**: SQLite Write-Ahead Logging — enables concurrent readers with serialized writers. Already enabled in `open_connection()`.
