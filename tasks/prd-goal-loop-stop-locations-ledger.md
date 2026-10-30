# Goal ledger: loop-stop-locations

Started: 2026-09-23 Source: `/prd-goal tasks/loop-stop-locations.json` plus the folded plan and `tasks/loop-stop-locations.md`
Worktree root: /home/chris/projects/task-mgr-worktrees/
Pipeline overrides: phase 1: treat tasks/loop-stop-locations.md as the PRD — start at architect
Author-ahead: no
Verification skill: .claude/skills/verify-task-mgr/SKILL.md

## Now

Phase: 1 loop-stop-locations
State: ticked
Next: goal closed
PRD / JSON / prompt: tasks/loop-stop-locations.md / tasks/loop-stop-locations.json / tasks/loop-stop-locations-prompt.md
Architect / JSON-review: tasks/loop-stop-locations-architect.md / tasks/loop-stop-locations-json-review.md
Worktree / prefix / branch: /home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-loop-stop-locations / 0400e2ee / feat/loop-stop-locations
Live writer id: 01a0d09f-7165-7f00-af5f-136a36552ed7
Fix cycle: 1
Last event: 2026-09-23 merged locally as c61f4ca. Gate green. Not pushed. Full maintain-verification-skill not re-run.

## Pins (verbatim — law for every phase)

1. `check_stop_signal(tasks_dir, prefix)` stays the single-directory predicate so existing tests stay valid. The mtime rule lives only in the new helper.
2. Call `stop_requested` only from `iteration.rs`, `wave_orchestration.rs`, and the batch between-PRD check. Do not add a field to account param structs, `UsageGateFn`, or `ResetWaitFn`. Do not change `wait_for_usage_reset`, `wait_for_ask_ttl_inner`, or `transient_backoff_wait`.
3. Loop canonical is `ResolvedPaths.tasks_dir` from `resolve_paths` (the pre-remap parent). Prefix file and global `.stop` / `.pause` count whenever they exist.
4. For the batch between-PRD check only: canonical is `db_dir.join("tasks")` (`.task-mgr/tasks`).
5. Extra directories, deduped after canonicalize: launch (`<cwd>/tasks`, or cwd when its final component is `tasks`); `<actual_worktree_path>/tasks` when that path is `Some` (not slot worktrees); `git::main_repo_root_at(source_root)/tasks` (`None` drops this candidate only). Do not add `<root>/.task-mgr/tasks` just because the directory exists.
6. Extra directories on an inner loop honor only `.stop-<prefix>` and `.pause-<prefix>`, and only when mtime is strictly after the `SystemTime` captured once at process start. Equal timestamps are stale.
7. At loop start, after `started_at` is captured and the prefix run record is written, delete each extra-dir `.stop-<prefix>` whose mtime is before or equal to `started_at`. `ui::emit` (not tracing-only) names the absolute path, says it was removed because it predates this process and would have been ignored, and tells the operator to run `task-mgr loop stop --prefix <prefix>` or create the file again at that same absolute path or at `<canonical>/.stop-<prefix>`. If the delete fails, still emit that warning and continue startup. Do not delete a canonical prefix file, a global `.stop`, a `.pause-<prefix>`, or another prefix's stop file. Do not sweep stale extra-dir global files at batch start.
8. `task-mgr loop stop --prefix <prefix>` is a nested `LoopCommand::Stop` only. Do not take `LockGuard`. `validate_prefix` before any path join. Trust a record only when that pid is alive and `/proc/<pid>/cmdline` argv contains the `task-mgr` binary plus the separate tokens `loop` and `run`, or `batch` and `run`. After the write, re-check. If the pid died, unlink the file just created.
9. Stop line inside the box: `task-mgr loop stop --prefix <prefix>`. Print canonical and extra absolute paths as untruncated stderr lines after the box.
10. The in-progress iteration still finishes, then the loop exits 0. Do not conflate operator stop with quota `HorizonStopped`.
11. Do not hand-edit the fenced command reference in the root `CLAUDE.md`.

## Simplified shape (do not re-expand)

Watch the launch `tasks/` directory, the feature worktree `tasks/`, and the main checkout `tasks/` for a prefix stop file newer than process start. Canonical PRD-parent files still stop with no mtime gate. Stale extra-dir prefix files are deleted at start with a recreate warning. `task-mgr loop stop --prefix` writes the canonical file from the run record.

## Goal exit gates

- [x] An operator can stop the running loop with `task-mgr loop stop --prefix <prefix>` without choosing a directory.
- [x] A prefix file created after start under the launch `tasks/`, the feature worktree `tasks/`, or the main checkout `tasks/` stops that prefix at the next iteration or wave boundary and does not stop a different prefix.
- [x] A stale extra-dir prefix file from before process start is deleted with a warning that tells the operator how to create a fresh one; a canonical prefix file from before start still stops the run.
- [x] Batch between PRDs stops on `.task-mgr/tasks/.stop`, and on a fresh global `.stop` in the extra dirs. The consumed canonical file is unlinked (`b84ca79`).
- [x] Phase merged locally onto the default branch, independent gate passed, `/compound` ran, and this ledger is updated.

## Phases

| #   | Phase               | PRD                              | JSON                              | State   | Land | Notes |
| --- | ------------------- | -------------------------------- | --------------------------------- | ------- | ---- | ----- |
| 1   | loop-stop-locations | tasks/loop-stop-locations.md | tasks/loop-stop-locations.json    | ticked | c61f4ca | Local merge. Not pushed. |

States: pending → authoring → authored → looping → reviewing →
fixing(n) → compounding → merging → merged → ticked
Also: paused | skipped (STATUS already Done)

## Residuals (human gates — not merge-blocking unless the goal says so)

| ID | What | Owner |
| VERIFY-001 | Full `/maintain-verification-skill` pass was not re-run. The phase added `features/loop-stop.md` and REVIEW drove help, missing-record, and bad-prefix. Live `loop run` stays skipped because the harness refuses it. | operator |
| LOW-001 | Operator skill text still says a stop leaves other prefixes alone. The batch-only fallback writes a global `.stop` and skips every remaining PRD. | operator |
| LOW-002 | `loop stop` follows a symlink in the run record's canonical path. Prefix traversal is rejected. | operator |

## Log

- 2026-09-23 Goal opened from `tasks/loop-stop-locations.json`. One open phase. Verification skill already in the repo. Awaiting confirmation of this phase list before the PRD author.
- 2026-09-23 User: skip the PRD; start at the architect. `tasks/loop-stop-locations.md` is the PRD. Spawning architect.
- 2026-09-23 Architect APPROVED. Report `tasks/loop-stop-locations-architect.md`. Lows are accepted residuals. Fold next. No second architect pass.
- 2026-09-23 Fold complete. `AA review (folded)` and verbatim pins are in `tasks/loop-stop-locations.md`. Tasks author next.
- 2026-09-23 Tasks author updated JSON and prompt.
- 2026-09-23 JSON review APPLY. Critical: FEAT-004 absolute paths in touchesFiles. Warning: FEAT-001 pause criterion. Apply next.
- 2026-09-23 JSON review pass 2 NOTHING_TO_APPLY. Handoff commit 8ba44eb on feat/loop-stop-locations. 11/11 pins. loop init imported 6 tasks, prefix 0400e2ee.
- 2026-09-23 Loop pid 1551546 in worktree feat-loop-stop-locations. Iteration 1 is FEAT-001. Operator log tasks/operator-loop-0400e2ee.log.
- 2026-09-23 Loop exit 0: all tasks complete, 9 iterations, 8 tasks done, tip 1a88c2c. Headless auto-review pid 1826127. Not merging until that findings file is read.
- 2026-09-23 Auto-review NEEDS WORK. Medium: `.task-mgr/tasks/.stop` stays after a batch honors it. Fix cycle 1: 0400e2ee-CODE-FIX-001. Loop pid 1851926.
- 2026-09-23 CODE-FIX-001 landed `b84ca79`. Delta-verify: consume unlinks canonical `.stop`, leaves extra `.stop` and `.pause`, failed unlink still stops. `cargo test --lib stop` 74 passed. Compound `522f0ad`. Full `cargo test --offline` green (no FAILED). Merged locally `c61f4ca`. Not pushed.
