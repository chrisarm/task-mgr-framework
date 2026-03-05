# PRD: Worktree Lifecycle Management (Phase 2)

**Type**: Feature
**Priority**: P2 (Medium)
**Author**: Claude Code
**Created**: 2026-02-28
**Status**: Draft

---

## 1. Overview

### Problem Statement
The scoped-sessions effort (Phase 1) adds per-PRD locks, prefix-scoped queries, and per-session signals. However, git worktree lifecycle is unmanaged:

1. **No worktree cleanup**: `ensure_worktree()` creates worktrees but nothing ever removes them. After many loop runs, `{repo}-worktrees/` accumulates stale directories.
2. **Early exit orphans**: If worktree creation fails mid-way (`engine.rs:741`), the parent directory may be left behind with no cleanup.
3. **No diagnostics**: No command shows active/stale worktrees. Lock files only store `PID@hostname` — no branch, worktree path, or PRD info.
4. **Shallow lock metadata**: Per-PRD locks from Phase 1 should record branch + worktree path for better error messages.

### Background
`ensure_worktree()` in `env.rs` creates worktrees in `{repo}-worktrees/{branch}` sibling directory. `LockGuard` in `lock.rs` writes `{pid}@{hostname}` to lock files. The `print_session_banner()` in `display.rs` shows PRD, branch, max iterations, and optional deadline — but no paths for DB, stop file, or worktree. The `batch.rs` module runs PRDs sequentially but has no post-loop cleanup. The `status` command (`loop_engine/status.rs`) shows a per-PRD dashboard but no worktree or multi-PRD aggregate view.

---

## 2. Goals

### Primary Goals
- [ ] Worktrees are cleaned up on loop exit (prompted or flag-driven)
- [ ] Partial worktree artifacts are cleaned up on early exit / setup failure
- [ ] New `task-mgr worktrees` command for listing, pruning, and removing worktrees
- [ ] Lock files include branch, worktree path, and prefix metadata
- [ ] Batch mode cleans up worktrees between PRD runs
- [ ] Session banner shows DB path, stop/pause hints, and worktree path
- [ ] `task-mgr list` and `task-mgr status` support multi-PRD grouping
- [ ] Progress files are scoped per-PRD prefix

### Success Metrics
- Zero stale worktree directories after normal loop/batch completion
- Lock error messages include branch + prefix info
- `task-mgr worktrees list` output matches `git worktree list` with lock status
- All existing `cargo test` tests continue to pass

---

## 2.5. Quality Dimensions

### Correctness Requirements
- `remove_worktree()` must never delete a worktree with uncommitted changes — warn and skip
- Enhanced lock format must be backwards-compatible: old `{pid}@{host}` files must still parse
- Early exit cleanup must not remove worktrees that were successfully created and in use
- `worktrees prune` must cross-reference lock files before removing anything

### Performance Requirements
- Best effort — no hard targets. `git worktree list/remove/prune` are fast operations.
- `parse_worktree_list()` is already O(n) and sufficient.

### Style Requirements
- Follow existing patterns: functions return `TaskMgrResult<T>`, output to stderr, use `eprintln!` for diagnostics
- New command modules follow the `commands/{name}.rs` pattern with `{Name}Result` struct + `format_text()` function
- No `.unwrap()` unless provably safe (e.g., after an existence check)
- Tests use `tempfile::TempDir` and the existing `setup_git_repo()` helper

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| Worktree with uncommitted changes | `git worktree remove` fails | Warn user, skip removal, continue |
| Lock file in old single-line format | Backwards compatibility | `read_holder_info()` falls back to parsing first line as `pid@host` |
| Worktree directory deleted out-of-band | User ran `rm -rf` manually | `git worktree prune` cleans up git metadata; `worktrees list` shows stale entry |
| Concurrent lock + worktree prune | Another process holds lock while prune runs | Cross-reference lock files; skip worktrees with active locks |
| No prefix in legacy DB | Old DBs have no `task_prefix` | Progress file falls back to `progress.txt`; list/status show ungrouped |
| Empty `{repo}-worktrees/` parent dir | All worktrees removed but parent remains | Remove parent dir if empty after last worktree removal |

---

## 3. User Stories

### US-P2-001: Worktree Cleanup on Loop Exit
**As a** developer finishing a loop session
**I want** the worktree to be optionally cleaned up on successful completion
**So that** stale worktrees don't accumulate on disk

**Acceptance Criteria:**
- [ ] On loop completion (all tasks done), prompt user to remove worktree (unless `--yes` mode, where it auto-keeps)
- [ ] Add `--cleanup-worktree` flag to force removal on exit
- [ ] Cleanup calls `git worktree remove {path}` then `git worktree prune`
- [ ] If removal fails (uncommitted changes), warn but don't error

### US-P2-002: Early Exit Worktree Cleanup
**As a** developer whose loop fails during setup
**I want** partial worktree artifacts cleaned up
**So that** I don't accumulate orphaned directories

**Acceptance Criteria:**
- [ ] If `ensure_worktree()` fails after creating parent dir, remove empty parent
- [ ] If `git worktree add` partially fails, run `git worktree prune`
- [ ] No change to successful worktree creation path

### US-P2-003: Worktree List/Prune Command
**As a** developer managing multiple PRD sessions
**I want** `task-mgr worktrees` to list and prune worktrees
**So that** I can see what's active and clean up stale ones

**Acceptance Criteria:**
- [ ] `task-mgr worktrees list` — shows all worktrees with branch, path, and whether a lock is held
- [ ] `task-mgr worktrees prune` — removes worktrees with no active lock, runs `git worktree prune`
- [ ] `task-mgr worktrees remove <path-or-branch>` — removes specific worktree
- [ ] Cross-references lock files (`loop-{prefix}.lock`) to determine active sessions

### US-P2-004: Enhanced Lock File Metadata
**As a** developer hitting a lock error
**I want** the error to tell me which branch/worktree/PRD holds the lock
**So that** I can diagnose contention without running `ps`

**Acceptance Criteria:**
- [ ] Lock file format: multi-line `{pid}@{host}\nbranch={branch}\nworktree={path}\nprefix={prefix}`
- [ ] `read_holder_info()` parses new format, falls back to old single-line format
- [ ] Lock error message includes branch + prefix when available
- [ ] Backwards compatible: old lock files still readable

### US-P2-005: Batch Worktree Cleanup Between PRDs
**As a** developer running `task-mgr batch`
**I want** each PRD's worktree cleaned up before the next PRD starts
**So that** batch runs don't leave N stale worktrees

**Acceptance Criteria:**
- [ ] After each PRD's loop completes in batch, offer to remove its worktree
- [ ] In `--yes` mode, auto-remove on success, keep on failure
- [ ] Add `--keep-worktrees` flag to batch to preserve all

### US-P2-006: Session Banner Hints
**As a** developer running a loop session
**I want** the startup banner to show me how to stop/pause and where key paths are
**So that** I can manage the session without guessing file paths

**Acceptance Criteria:**
- [ ] Banner includes: DB path, stop hint (`.stop` or `.stop-{prefix}`), pause hint, worktree path
- [ ] `task-mgr status` also shows these paths when a loop is active (via lock file detection)

### US-P2-007: Multi-PRD Status & List Awareness
**As a** developer with multiple PRDs imported
**I want** `task-mgr list` and `task-mgr status` to show per-PRD grouping
**So that** I can see progress across all PRDs in one view

**Acceptance Criteria:**
- [ ] `task-mgr list` groups tasks by prefix when multiple prefixes exist
- [ ] `task-mgr status` shows per-PRD summary: prefix, branch, task counts, active lock
- [ ] `task-mgr status --prefix P1` filters to single PRD
- [ ] Combined view with prefix headers when no prefix specified

### US-P2-008: Per-PRD Progress Files
**As a** developer running concurrent PRD loops
**I want** progress entries scoped to their PRD
**So that** progress from different PRDs doesn't interleave

**Acceptance Criteria:**
- [ ] Progress file becomes `progress-{prefix}.txt` when prefix is available
- [ ] Falls back to `progress.txt` for legacy/no-prefix mode
- [ ] `task-mgr status` reads the correct per-PRD progress file
- [ ] Existing `progress.txt` continues to work (no migration needed)

---

## 4. Functional Requirements

### FR-001: `remove_worktree()` Function
Core utility for worktree removal used by US-P2-001, US-P2-003, US-P2-005.

**Details:**
- Run `git worktree remove <path>` from the source_root
- If removal fails (uncommitted changes), return `Ok(false)` with warning printed to stderr
- On success, run `git worktree prune` to clean up stale metadata
- If the `{repo}-worktrees/` parent is empty after removal, remove the parent dir

**Validation:**
- Unit test with tempdir + git init + worktree add → remove succeeds
- Test with dirty worktree → warns, returns false

### FR-002: Enhanced Lock File Format
Multi-line lock file with backwards-compatible parsing.

**Details:**
- `write_holder_info()` writes: `{pid}@{host}\nbranch={branch}\nworktree={path}\nprefix={prefix}`
- `read_holder_info()` returns a struct `HolderInfo { identity: String, branch: Option<String>, worktree: Option<String>, prefix: Option<String> }`
- Falls back: if file contains single line with no `=`, treat entire line as identity
- Error formatting includes branch + prefix when available

**Validation:**
- Test: write new format → read back all fields
- Test: read old `pid@host` format → identity populated, optionals None

### FR-003: `Worktrees` Subcommand
New `task-mgr worktrees {list|prune|remove}` command.

**Details:**
- `list`: Run `git worktree list --porcelain`, parse via existing `parse_worktree_list()`, cross-reference lock files in `.task-mgr/`
- `prune`: Filter worktrees without active locks, run `git worktree remove` on each, then `git worktree prune`
- `remove <target>`: Find worktree by path or branch name, call `remove_worktree()`

**Validation:**
- Integration tests with real git worktrees in tempdir

### FR-004: Session Banner Enhancement
Add path hints to `print_session_banner()`.

**Details:**
- New parameters: `db_path`, `tasks_dir`, `worktree_path: Option<&Path>`, `task_prefix: Option<&str>`
- Display: Database path, Stop hint (`.stop` or `.stop-{prefix}`), Pause hint, Worktree path (when applicable)

**Validation:**
- Existing banner tests still pass (update signatures)
- New test for banner with all hints populated

### FR-005: Multi-PRD Grouping in List/Status
Group tasks and stats by prefix when multiple prefixes exist.

**Details:**
- `list`: Query `SELECT DISTINCT task_prefix FROM prd_metadata`, if multiple → group output by prefix with headers
- `status`: Add `--prefix` filter option, show per-PRD summary row when no prefix specified

**Validation:**
- Test with DB containing tasks from two different prefixes → grouped output

### FR-006: Per-PRD Progress Files
Scope progress files by prefix.

**Details:**
- `resolve_paths()` takes optional `prefix` parameter
- When prefix is present: `progress-{prefix}.txt` instead of `progress.txt`
- Fallback: if prefix file doesn't exist and legacy `progress.txt` does, read legacy

**Validation:**
- Test: resolve_paths with prefix → correct filename
- Test: resolve_paths without prefix → legacy filename

---

## 5. Non-Goals (Out of Scope)

- **Automatic worktree garbage collection on a schedule** — Reason: adds daemon complexity; explicit cleanup is sufficient
- **Cross-machine lock coordination** — Reason: advisory locks are kernel-level only; out of scope for Phase 2
- **Migration of existing progress.txt** — Reason: backwards-compatible fallback is sufficient
- **Signal file changes** — Reason: Phase 1's `SS-FEAT-010` already handles per-prefix signals

---

## 6. Technical Considerations

### Affected Components
- `src/loop_engine/env.rs` — `remove_worktree()`, `ensure_worktree()` error cleanup, `resolve_paths()` prefix param, make `parse_worktree_list()` and `compute_worktree_path()` public
- `src/loop_engine/engine.rs` — pass worktree/prefix to lock, call cleanup on exit, pass paths to banner
- `src/loop_engine/display.rs` — `print_session_banner()` new params for hints
- `src/loop_engine/batch.rs` — post-loop worktree cleanup, `--keep-worktrees` flag
- `src/loop_engine/config.rs` — `cleanup_worktree` field on `LoopConfig`
- `src/db/lock.rs` — `HolderInfo` struct, `write_holder_info_extended()`, `read_holder_info()` updated
- `src/commands/worktrees.rs` — new module (list, prune, remove)
- `src/commands/list.rs` — group-by-prefix display
- `src/commands/mod.rs` — register worktrees module
- `src/cli/commands.rs` — `Worktrees` subcommand variant
- `src/main.rs` — dispatch `Worktrees` command
- `src/loop_engine/status.rs` — per-PRD summary, `--prefix` filter
- `src/handlers.rs` — register `WorktreesResult` formatting

### Dependencies
- Phase 1 per-PRD locks (`loop-{prefix}.lock` files) — must be complete for lock cross-referencing
- Phase 1 prefix scoping (`task_prefix` in `prd_metadata`) — used by multi-PRD grouping

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| A: `remove_worktree()` as standalone fn in `env.rs` | Simple, follows existing pattern, testable | Slightly long file | **Preferred** |
| B: Separate `worktree.rs` module in `loop_engine/` | Better separation | More module wiring, env.rs already has all git helpers | Alternative |

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| A: Lock metadata as multi-line key=value | Human-readable, backwards-compatible via line count heuristic | Slightly more complex parsing | **Preferred** |
| B: Lock metadata as JSON | Structured, extensible | Overkill for 4 fields, harder to debug manually | Rejected |

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| A: `run_loop()` returns worktree path alongside exit code | Batch can use it for cleanup | Changes return type (breaking) | **Preferred** — wrap in `LoopResult` struct |
| B: Batch re-derives worktree path from PRD metadata | No engine changes | Duplicates path computation logic | Alternative |

**Selected Approach**: (A) for all three decisions. Add `remove_worktree()` to `env.rs`. Use multi-line key=value lock format. Return `LoopResult { exit_code, worktree_path: Option<PathBuf> }` from `run_loop()`.

### Risks & Mitigations
| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `git worktree remove` fails on dirty worktree during batch | Low — batch continues | Medium | Warn and skip; keep worktree for manual inspection |
| Lock format change breaks existing lock readers | Medium — stale locks unreadable | Low | First line is always `pid@host`; `read_holder_info()` falls back to single-line |
| `run_loop()` return type change breaks callers | Medium — compile error | Low | Only 2 callers (main.rs, batch.rs); update both in same PR |

### Security Considerations
- No secrets involved — lock files contain PID, hostname, branch name, paths
- `remove_worktree()` only operates on paths derived from `compute_worktree_path()` — no user-supplied arbitrary paths in the remove codepath
- `worktrees remove <target>` validates target is actually a git worktree before removal

### Public Contracts

#### New Interfaces
| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
|----------------|-----------|-------------------|-----------------|-------------|
| `env::remove_worktree` | `(source_root: &Path, worktree_path: &Path) -> TaskMgrResult<bool>` | `true` if removed, `false` if skipped (dirty) | `TaskMgrError` on git failure | Runs `git worktree remove` + `prune`, may remove empty parent dir |
| `lock::HolderInfo` | Struct: `{ identity: String, branch: Option<String>, worktree: Option<String>, prefix: Option<String> }` | — | — | — |
| `lock::LockGuard::write_holder_info_extended` | `(&mut self, branch: Option<&str>, worktree: Option<&str>, prefix: Option<&str>) -> TaskMgrResult<()>` | `()` | `TaskMgrError` on I/O | Writes multi-line lock file |
| `commands::worktrees::list` | `(dir: &Path, source_root: &Path) -> TaskMgrResult<WorktreesListResult>` | `WorktreesListResult` | `TaskMgrError` | Reads git worktree list + lock files |
| `commands::worktrees::prune` | `(dir: &Path, source_root: &Path) -> TaskMgrResult<WorktreesPruneResult>` | `WorktreesPruneResult` | `TaskMgrError` | Removes unlocked worktrees |
| `commands::worktrees::remove` | `(dir: &Path, source_root: &Path, target: &str) -> TaskMgrResult<WorktreesRemoveResult>` | `WorktreesRemoveResult` | `TaskMgrError` | Removes specific worktree |

#### Modified Interfaces
| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|----------------|-------------------|-------------------|-----------|-----------|
| `engine::run_loop` | `(LoopRunConfig) -> i32` | `(LoopRunConfig) -> LoopResult` | Yes | Update 2 callers (main.rs, batch.rs) to use `.exit_code` |
| `display::print_session_banner` | `(prd_file, branch, max_iterations, deadline_hours)` | `(SessionBannerParams)` — struct with all fields | Yes | Update single caller in engine.rs |
| `lock::LockGuard::write_holder_info` | `(&mut self) -> TaskMgrResult<()>` | Unchanged — new `write_holder_info_extended` variant | No | — |
| `lock::LockGuard::read_holder_info` | `(path: &Path) -> Option<String>` | `(path: &Path) -> Option<HolderInfo>` | Yes | Update callers to use `.identity` for old behavior |
| `env::resolve_paths` | `(prd_file, prompt_file, project_dir)` | `(prd_file, prompt_file, project_dir, prefix: Option<&str>)` | Yes | Update callers to pass prefix |

### Implementation Order

```
Group A — Standalone (parallel):
  US-P2-002 (Early exit cleanup)
  US-P2-004 (Enhanced lock metadata)
  US-P2-006 (Session banner hints)
  US-P2-008 (Per-PRD progress files)

Group B — Depends on Group A:
  US-P2-001 (Loop exit cleanup) — needs remove_worktree() from P2-002
  US-P2-007 (Multi-PRD status/list) — needs enhanced lock info from P2-004

Group C — Depends on Group B:
  US-P2-003 (Worktrees command) — needs remove_worktree(), enhanced lock info
  US-P2-005 (Batch cleanup) — needs US-P2-001
```

### Inversion Checklist
- [x] All callers of `run_loop()` identified (main.rs:606, batch.rs:226)
- [x] All callers of `print_session_banner()` identified (engine.rs:831)
- [x] All callers of `read_holder_info()` identified (lock.rs:76 — within `acquire_inner`)
- [x] All callers of `resolve_paths()` identified (engine.rs)
- [x] Tests that validate current lock format identified (lock.rs tests)
- [x] Different semantic contexts for `write_holder_info` documented (per-command vs per-loop locks)

---

## 7. Open Questions

- [ ] Should `--cleanup-worktree` be the default in `--yes` mode, or require explicit opt-in? (PRD says auto-keep in `--yes` mode)
- [ ] Should `worktrees prune` require `--yes` for non-interactive use, or is it safe by default since it only removes unlocked worktrees?

---

## Appendix

### Related Documents
- Phase 1 scoped-sessions PRD: `tasks/prd-scoped-sessions.md`
- Lock implementation: `src/db/lock.rs`
- Worktree management: `src/loop_engine/env.rs:268-461`

### Glossary
- **Worktree**: A git worktree — a separate working directory linked to the same repository, allowing concurrent work on different branches
- **Source root**: The original git repository root where PRD files, prompts, and `.task-mgr/` database live
- **Working root**: The directory where Claude runs — either source_root or a worktree path
- **Prefix**: A short identifier (e.g., `P1`, `abc12345`) prepended to task IDs to scope them per-PRD
