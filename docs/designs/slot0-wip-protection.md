# Slot 0 WIP protection during parallel merge-back

Design note for fixing the "WIP vanished from my checkout" failure mode.
Review separately from implementation.

## Summary

The user's throwaway-worktree workflow already isolates merge-back stash from
their main checkout in most cases. The scar likely came from running on the
live main checkout (or a PRD branch checked out there), not from `git_reconcile`.
Code hardening remains valuable as a secondary safeguard for live-checkout runs.

---

## Does this line up with the throwaway-worktree workflow?

**Mostly yes** — with three code-level caveats worth knowing.

### What the worktree workflow gets right

When you:

```sh
git worktree add ../mw_dependencies-worktrees/<name> -b <branch> origin/main
cd ../mw_dependencies-worktrees/<name>
task-mgr loop run -y ...
```

- **`source_root`** = `git rev-parse --show-toplevel` from cwd → the **throwaway
  worktree**, not main (`get_project_root` in `src/main.rs`)
- **Slot 0** = that worktree (`ensure_worktree` returns `project_root` when
  already inside the correct branch worktree — `src/loop_engine/worktree.rs`
  lines 203–207)
- **Merge-back stash** (`prepare_slot0_for_merge`) runs with
  `current_dir = slot0_path` → only the throwaway tree's files are stashed/popped
- **Main checkout's uncommitted WIP** (different path on disk) is **not** touched
  by FEAT-004 preflight

So the real answer for day-to-day use: **run from a throwaway worktree** and
the stash failure mode does not reach main.

`git_reconcile` was never the culprit — it only reads commit history and updates
DB/PRD status. The dangerous auto-stash is **FEAT-004 preflight** in
`src/loop_engine/worktree.rs`.

### Caveats (code vs operator expectation)

| Operator expectation | What the code actually does |
|----------------------|----------------------------|
| "Its own `.task-mgr` DB" | **Partially wrong.** Default `.task-mgr` is **worktree-anchored to the main repo** when cwd is a linked worktree (`resolve_db_dir` in `src/db/path.rs`, rule 3). You get an isolated **working tree**, but the DB is shared unless you pass explicit `--dir`. |
| "Reconcile only touches the throwaway tree" | **Working-tree ops yes; git refs are shared.** `git branch -D`, `git worktree prune`, and stale-ephemeral cleanup mutate the **shared object store** — they can delete unpushed branches repo-wide, but they do **not** stash/modify files in main's working directory. |
| Parallel (`--parallel 2` default) from throwaway worktree | **Parallel slot 1+ likely fails.** `ensure_worktree` errors when already inside a linked worktree whose branch ≠ `{branch}-slot-N` (`worktree.rs` lines 209–218). Startup falls back to **sequential** (`startup.rs` line ~803). For the throwaway workflow that's actually safer — no merge-back stash runs at all. |

### Secondary safeguards (if you must run from a live checkout)

- Commit or push WIP first (stash is recoverable but a clean commit/PR is safer)
- Before launching, confirm no local-only branch you care about is unpushed
  (reconcile prunes branches it thinks are merged)

---

## When main checkout was vulnerable (likely scar scenario)

Main's WIP gets stashed only when **main (or your live checkout) IS slot 0**:

```
Safe: throwaway worktree
  cwd = ../repo-worktrees/name
  slot 0 = throwaway path
  main checkout → not slot 0 → files untouched

Risky: live checkout
  cwd = main repo, branch = main or PRD branch
  slot 0 = this checkout
  → prepare_slot0_for_merge stashes ALL dirty files
```

Concrete risky cases:

1. `task-mgr loop run` from main repo with PRD branch = `main` (or
   `--no-worktree` — parallel disabled anyway)
2. Already inside the PRD branch worktree where you keep personal WIP
   (slot 0 = your cwd)
3. `--yes` + dirty startup — warns and continues (`env.rs` lines 150–155), so
   dirty WIP reaches wave merge-back

The FEAT-004 blanket stash (`git stash push --include-untracked` on **all**
dirty paths) sweeps operator edits. Pop conflicts leave WIP on the stash stack —
looks deleted from the working tree.

### Failure flow (today)

```
Wave ends
  → merge_slot_branches_with_resolver
  → prepare_slot0_for_merge(slot0)
      → dirty WT → git stash push --include-untracked (ALL files)
  → git merge ephemeral branch
  → cleanup_preparation → git stash pop
      → clean pop: WIP back in WT
      → pop conflict: WIP on stash stack — WT looks empty
  → merge failure → git reset --hard pre_merge_head
```

Startup (`--yes`): `check_uncommitted_changes` warns and continues — dirty WIP
reaches merge-back.

---

## Fix strategy

**Primary mitigation (operator workflow):** throwaway worktree — already
correct; document prominently.

**Secondary mitigation (code):** harden `prepare_slot0_for_merge` for operators
who run from a live checkout anyway.

### 1. Dirty-path classifier

In `src/loop_engine/worktree.rs`, classify `git status --porcelain -uall`
paths:

```rust
enum Slot0DirtyClass {
    Clean,
    StashableOnly { paths: Vec<String> },  // safe to partial-stash
    OperatorWip { paths: Vec<String> },    // must NOT stash
}
```

**Stashable allowlist** (extend existing `is_progress_path`):

- `tasks/progress*.txt` and `.task-mgr/tasks/progress*.txt` (already defined)
- `.task-mgr/logs/**` (matches `GITIGNORE_BODY` in `src/commands/init/mod.rs`)

**Everything else** (tracked source edits, untracked new files like
`scratch.log`, staged changes) → `OperatorWip`.

Mirror the philosophy in `recover_progress_only_slot`: "keeps real uncommitted
work from being silently committed."

### 2. Harden `prepare_slot0_for_merge`

Replace blanket stash with:

- `Clean` → `MergePreparation::Clean` (unchanged)
- `StashableOnly` → partial stash:
  `git stash push --include-untracked -m <tag> -- <path1> <path2> ...`
- `OperatorWip` → `Err(...)` listing blocked paths + guidance:
  - commit or stash manually before the wave
  - or run from an isolated worktree

Callers already treat prep `Err` as `failed_slots(PreResolver)` — slot commits
stay on the ephemeral branch; loop continues. No data loss.

Emit via `ui::emit_err` when blocking (paths + stash tag when stashing).

### 3. Startup gate (parallel mode only)

After `ensure_slot_worktrees` in `src/loop_engine/startup.rs`:

- If slot 0 has `OperatorWip` → **abort loop** (exit 1), even in `--yes`
- Message: which paths are dirty, that parallel merge-back will not touch
  operator WIP, and how to proceed

**Do not gate sequential runs from throwaway worktrees** (noisy and unnecessary
when slot 0 is already isolated).

Optional: when cwd is primary repo (`!is_inside_worktree`) + dirty tracked
files → one-line hint to use a throwaway worktree.

### 4. Fix `cleanup_preparation` false-positive

When `resolve_stash_ref_by_tag` returns `None` or errors, **do not** return
`Restored`. Add `CleanupOutcome::StashNotFound { tag }` (or equivalent) and
surface in merge-back diagnostics.

On `PopConflict`, extend `ui::emit_err` with:

- exact stash tag
- `git stash list` grep hint (`task-mgr-slot-`)
- `git stash show -p stash@{N}` recovery command

### 5. Tests

| Test | Change |
|------|--------|
| `test_prepare_dirty_tracked_file` | Expect `Err`, WT unchanged |
| `test_prepare_dirty_and_untracked_creates_one_stash` | Expect `Err` when `file.txt` dirty |
| `test_prepare_untracked_file` / `test_merge_back_stashes_dirty_unrelated_file` | Use allowlisted path or expect `Err` for `scratch.log` |
| `test_merge_back_pop_conflict_keeps_stash_and_warns` | Use allowlisted stashable path |
| **New** `test_prepare_stashable_progress_only` | Progress dirtied → partial stash, merge proceeds |
| **New** `test_prepare_blocks_operator_tracked_wip` | Modified `src/foo.rs` → Err, no stash |

Keep FEAT-004 pop-conflict / stash-limit / run-id-isolation tests — adapt
fixtures only.

### 6. Documentation

Update `src/loop_engine/CLAUDE.md` stash-preflight section:

- throwaway-worktree recommendation (primary)
- allowlist vs abort contract (secondary)
- DB worktree-anchoring vs WT isolation note

---

## Out of scope

- **`wrapper_commit`** (`git_reconcile.rs`): auto-commits all dirty files on the
  **sequential** path when a task completes without a commit. Not triggered
  during parallel waves (`wrapper_commit = false` in `wave_scheduler.rs`).
- Fixing parallel slot 1+ from inside a secondary worktree (separate ergonomics
  task).
- Config knob to restore legacy "stash everything" behavior.

---

## Verification

```sh
cargo test -p task-mgr worktree::tests::test_prepare_
cargo test -p task-mgr worktree::tests::test_merge_back_
cargo test -p task-mgr reconcile_stale_ephemeral
```

Manual:

- From **main repo checkout** with dirty `src/foo.rs`:
  `task-mgr loop run --parallel 2 --yes` → startup abort, main WT unchanged.
- From **throwaway worktree** with same dirty file → only that worktree checked;
  main still untouched.

---

## Implementation checklist

- [ ] Add `Slot0DirtyClass` + `is_stashable_path()` in `worktree.rs`
- [ ] Rewrite `prepare_slot0_for_merge` (partial stash / operator-WIP abort)
- [ ] Parallel-mode startup gate in `startup.rs`
- [ ] Fix `cleanup_preparation` false `Restored` + PopConflict messaging
- [ ] Update FEAT-004 tests + add operator-WIP block tests
- [ ] Update `CLAUDE.md`