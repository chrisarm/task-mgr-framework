# Changelog — 2026-08-16

## Keep Honest Terminal Closes from Being Reset to `todo`

**Branch**: `feat/honest-terminal-closes`
**PRD**: `tasks/prd-honest-terminal-closes.md`

### What shipped

Loop-exit cleanup (orchestrator 17.5 / 17.6) no longer force-writes honest
terminal closes (`blocked` / `skipped` / `irrelevant` / `done`) back to `todo`.
Orphan reclaim and overflow rungs 1–3 use `TaskLifecycle::recover_in_progress`
(atomic `in_progress → todo` only). Failed slot merge uses
`reopen_after_merge_fail` (`in_progress|done → todo`) so a premature `:done`
that never landed on slot 0 is still retried, while an honest `:blocked` stays
classified. `handle_task_failure_with_runner` skips the consecutive-failure
ladder when the row is already terminal. `resurrect_for_iteration` stays the
unguarded escape hatch.

### Why it matters

A VERIFY gate that honestly emitted `:blocked` was being undone at process
exit (`Reset uncompleted slot task … to todo`), then re-claimed next wave.
Overflow-rung-5 and auto-block closes had the same hole because they are not
status tags. Operators now keep classified work classified.

### Breaking changes

None. True orphans (`in_progress`, no status tag) still reset. Merge-fail of
`:done` still reopens. Overflow rungs 1–3 still flip the claimed row to `todo`.

---
