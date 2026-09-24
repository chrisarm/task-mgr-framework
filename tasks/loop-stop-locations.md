# Loop stop locations

**Type**: plan-tasks lean brief
**Branch**: feat/loop-stop-locations
**Task list**: tasks/loop-stop-locations.json
**Prompt**: tasks/loop-stop-locations-prompt.md

## Problem

A running `task-mgr loop run` notices `.stop-<prefix>` only in `ResolvedPaths.tasks_dir` (the PRD parent from `resolve_paths`). Startup later points `paths.prd_file` at the worktree copy and leaves `tasks_dir` alone. The session banner prints that directory relative to the process cwd, and the loop-operator skill tells agents to `touch tasks/.stop-<prefix>` from the repo root. The file lands in the main checkout. The worktree loop keeps iterating. A different prefix is correctly unaffected, so the miss looks like success.

Batch's between-PRD check is a third directory: `cli.dir.join("tasks")`, which is `.task-mgr/tasks/.stop`, not `tasks/.stop`.

## In scope

- `stop_requested` on a small `SignalLocations` value. Canonical directory keeps today's no-mtime rule. Extra dirs are launch `<cwd>/tasks` (or cwd when its final component is `tasks`), `<actual_worktree_path>/tasks`, and `git::main_repo_root_at(source_root)/tasks`.
- Inner loop: extra dirs honor only the prefix stop/pause file, and only when mtime is strictly after process start.
- Batch between PRDs: canonical is `.task-mgr/tasks` with no mtime gate; extra dirs honor a global `.stop` only when mtime is strictly after batch start.
- Call sites: `iteration.rs`, `wave_orchestration.rs`, and the batch between-PRD check. Account waits stay `check_stop_signal(tasks_dir, None)`.
- `handle_pause` deletes the pause file that matched, including an extra-dir copy. Wave preflight does not grow a pause check.
- At loop start, after the run record exists, delete this prefix's extra-dir `.stop-<prefix>` when its mtime is before or equal to process start. Warn with the removed path and how to stop now: `task-mgr loop stop --prefix <prefix>`, or create the file again at that path or at the canonical `.stop-<prefix>`. A failed delete still warns and does not abort startup. Leave canonical prefix files, global `.stop`, pause files, and other prefixes alone.
- `task-mgr loop stop --prefix` reads `{db_dir}/loop-runs/<prefix>.json` or `batch.json`, requires a live `loop run` / `batch run` argv, writes the one canonical file for that situation, and unlinks it if the pid dies before the write sticks.
- Banner stop line is the command, with absolute paths untruncated after the box.
- Gitignore for stop/pause/pid files, a short `src/loop_engine/CLAUDE.md` note, and the loop-operator / loop-monitor `STOP_FILE` instructions.
- A verify-task-mgr feature for `loop stop` that does not launch `loop run`.

## Out of scope

- Scanning every git worktree, slot worktrees, or `<root>/.task-mgr/tasks` merely because it exists.
- Aborting the in-flight iteration, a database stop flag, or prefix stops inside usage / Ask / transient waits.
- Hand-editing the fenced command reference in the root `CLAUDE.md`.
- Telling operators to delete stop files after the process exits.

## Success bar

An agent can stop the loop it is watching by running `task-mgr loop stop --prefix <prefix>` without choosing a directory. A prefix file created after start under the launch `tasks/`, the feature worktree `tasks/`, or the main checkout `tasks/` also stops that prefix at the next iteration or wave boundary, and does not stop a different prefix. A stale extra-dir file from before process start does not. Batch between PRDs still stops on `.task-mgr/tasks/.stop`, and also on a fresh `tasks/.stop` in those extra dirs.

## Key files / subsystems

- `src/loop_engine/signals.rs` — single-dir predicate stays; new helper owns the mtime rule.
- `src/loop_engine/env.rs` — `tasks_dir` is the PRD parent. Do not recompute it from the remapped PRD.
- `src/loop_engine/batch.rs` — between-PRD dir is `db_dir.join("tasks")`.
- `src/git/mod.rs` — `main_repo_root_at(dir)` uses `--path-format=absolute`.
- `src/db/prefix.rs` — `validate_prefix` before any filename join.
- `src/cli/commands.rs` / `src/main.rs` — nested `LoopCommand::Stop` only.
- `~/.grok/skills/loop-operator/` and `~/.grok/skills/loop-monitor/SKILL.md` — outside the repo; edit in place.

## Notes for review

Architect fold is already in the session plan. Do not reopen: threading `SignalLocations` through account waits, treating `.task-mgr/tasks` as an extra candidate, mtime "at or after", or `git rev-parse` without `main_repo_root_at`. Do not collapse the check back to one directory. Learnings to preserve: **1885** (exit 0 for operator stop, 130 for SIGINT), **5558 / 5469** (operator stop stays distinct from horizon stop), **623** (`tasks_dir` is the PRD parent), **2798** (the loop reads the worktree PRD copy).

## Pins

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

## Rules

1. `started_at` is one `SystemTime` captured at process entry. `run_batch` calls `engine::run_loop` in-process. Copy that same timestamp into every inner `SignalLocations`. Do not call `SystemTime::now()` at `run_loop` entry or next to the sweep.
2. Cmdline match: some argv element's final path component is exactly `task-mgr`, plus other elements `loop` and `run`, or `batch` and `run`. Not a substring. Re-check after the write; unlink only the path this command wrote.
3. After canonicalize, a path that is also canonical is removed from `extras` and keeps the no-mtime rule. A missing directory is omitted. `canonicalize` failure must not abort startup.
4. Exit cleanup of extras deletes `.stop-<prefix>` and `.pause-<prefix>` only. Do not call `cleanup_signal_files_for_prefix` on an extra.
5. Absolute paths are `ui::emit` lines after `print_session_banner` returns. Do not add them to `format_session_banner`.

## AA review (folded)

Verdict: APPROVED. Concerns: none open.

The four Low items in the architect report are ACCEPTED residuals (do not "fix" them):

- Prefix stop does not interrupt an account wait.
- Deprecated flat `task-mgr loop <prd>` has no `run` argv token so `loop stop` fails closed.
- Early `initialize_loop` Err paths may leave a prefix run record (overwrite it on the next successful start).
- The Pause row inside the banner box may stay truncated.
