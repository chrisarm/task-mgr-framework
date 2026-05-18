# Changelog — 2026-05-17

## Post-merge slot completion reconciliation

**Branch**: `feat/post-merge-slot-reconcile`
**PRD**: `tasks/post-merge-slot-reconcile.json` (no PRD markdown — task list authored directly)

### What shipped

Closes a wave/parallel-slot silent-completion gap. When a slot agent
committed work with the `<TASK-ID>-COMPLETED` marker but its subprocess
exited before flushing the `<completed>` tag (buffer drop, watchdog
kill, deadline, signal), the task previously stayed `in_progress` until
loop exit and was reset to `todo` by step 17.6 — forcing the next
iteration to rediscover from `git log` that the work was already merged.

The fix surfaces slot 0's pre-merge HEAD on `MergeOutcomes`, adds
`git_reconcile::reconcile_merged_slot_completions` to scan
`{pre_merge_head}..HEAD` (with `--no-merges`) for completion markers,
and wires the call into `run_wave_iteration` before the four terminal
returns. A `pending_slot_tasks.retain` drain prevents loop-exit cleanup
from undoing the DB write. A private `query_incomplete_task_ids`
helper now backs both this reconcile and the pre-existing
`reconcile_external_git_completions`.

### Why it matters

Wave-mode loops no longer waste an iteration re-discovering finished
work after a subprocess flake. The reconcile is range-scoped
(`{pre..HEAD}`), defended against resolver-merge-commit poisoning via
`--no-merges`, and respects dependency gating (`force=false` on
`complete_cmd::complete`). End-to-end test exercises the
`pending_slot_tasks` drain path.

### Breaking changes

None. `MergeOutcomes.pre_merge_head: Option<String>` is additive via
`#[derive(Default)]`. No public API changes; reuses existing helpers
(`contains_task_id`, `prefix_and`, `complete_cmd::complete`,
`update_prd_task_passes`, `rev_parse_head`).

---
