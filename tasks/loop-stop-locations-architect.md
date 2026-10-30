# Architect review — loop stop locations

**Status**: APPROVED

The lean brief plus the pins are enough to implement without a product decision. The task seed agrees with the pins. Nothing below is an open High or an unresolved Medium.

## Strengths

- The bug is real and the split matches the code. `resolve_paths` sets `tasks_dir` from the PRD parent (`src/loop_engine/env.rs` around the `prd_absolute.parent()` assignment) and canonicalizes the PRD first. Step 8.5 then does `paths.prd_file = live_prd_file.clone()` (`src/loop_engine/startup.rs`) and does not touch `tasks_dir`. A worktree loop therefore never watches the directory an agent reaches with `touch tasks/.stop-<prefix>` from the main checkout.
- Batch is a third directory on purpose. `run_batch` sets `let tasks_dir = dir.join("tasks")` and passes that `dir` through as `LoopRunConfig.db_dir` (`src/loop_engine/batch.rs`). `dir` is the resolved DB directory from `main` (`resolve_db_dir` once, before the command match). That is `.task-mgr/tasks`, not project `tasks/`. `ensure_directories` creates `.task-mgr/tasks` on every loop start, so "watch it because it exists" would always fire. The pin that refuses it as an extra is the right cut.
- `check_stop_signal` stays a single-directory `exists` check (`src/loop_engine/signals.rs`). Prefix file, else global `.stop`. No mtime. Existing tests in that file and `tests/multi_prd_lifecycle.rs` keep compiling. The mtime rule lives only on `SignalLocations`.
- Call-site set is the three boundaries that already decide "stop before new work": `run_iteration` step 1 (`src/loop_engine/iteration.rs`), `wave_preflight_check` (`src/loop_engine/wave_orchestration.rs`), and the batch loop before each PRD (`src/loop_engine/batch.rs`). Account waits stay `check_stop_signal(tasks_dir, None)` at the four sites in `transient_backoff_wait`, `wait_for_ask_ttl_inner`, and `wait_for_usage_reset_inner` (`src/loop_engine/reactions/account.rs`). No new field on those param structs.
- Operator stop is already a different exit from quota horizon. File stop returns `operator_stopped: true` / wave `was_stopped: true`, exit reason `"stop signal"`, exit 0. `HorizonStopped` on the wave path sets `was_stopped: false` and reason `"quota horizon stop"`. The orchestrator sets `was_stopped` only in the `operator_stopped` arm and leaves horizon on `"quota soft-stop"` (`src/loop_engine/orchestrator.rs`). Batch skips remaining PRDs only when `loop_result.was_stopped` is set. Copy those flags; do not invent a new outcome.
- `loop stop` fits the CLI shape that already exists. `LoopCommand` is an exhaustive match in `src/main.rs`; a new `Stop` variant that is not matched fails the build. `resolve_loop_command` returns `Nested(child)` when `cmd` is `Some`, so a nested variant does not need a new argument on that function. `cli.dir` is already the `resolve_db_dir` result. `validate_prefix` is `[a-zA-Z0-9.-]`, non-empty (`src/db/prefix.rs`), which rejects `../x` before any join.
- Banner truncation is the current footgun, and the plan avoids it. `format_session_banner` clamps to 110 columns and runs the stop hint through `shorten_path_for_display` then `truncate_display` (`src/loop_engine/display.rs`). Absolute paths go out through `ui::emit`, which writes the caller's bytes to stderr with no decoration (`src/output/ui.rs`).
- `git::main_repo_root_at` already uses `rev-parse --path-format=absolute --git-common-dir` and canonicalizes (`src/git/mod.rs`). `None` means drop that one candidate. Slot paths are a different vec (`slot_worktree_paths`); `actual_worktree_path` is the feature worktree only.
- The helper is a pure value (`canonical`, `extras`, `started_at`) plus a cmdline check that takes raw argv bytes. Both are unit-testable without a live loop or `/proc`. Fail-closed on a missing record, a bad prefix, or a non-loop cmdline is the right security posture for a file that canonical checks will honor with no mtime gate.

## Concerns

No open High. No unresolved Medium. Lows are residuals the pins already accept.

- **Low — prefix stop does not interrupt an account wait.** After step 1, `run_iteration` enters `run_account_quota_gate`, which polls only a global file in `params.tasks_dir`. A `.stop-<prefix>` written by `loop stop` during that wait is invisible until the next iteration or wave preflight, so one agent body can still start. Pin 2 and the out-of-scope list forbid threading `SignalLocations` into `wait_for_usage_reset`, `wait_for_ask_ttl_inner`, or `transient_backoff_wait`. Do not "fix" this.
- **Low — deprecated flat invocations are invisible to `loop stop`.** `task-mgr loop <prd>` is synthesized into `LoopCommand::Run` but the process argv has no separate `run` token (`resolve_loop_command`). Pin 8 requires `loop` and `run`, or `batch` and `run`. Those processes fail closed. Nested `loop run` / `batch run` is the contract.
- **Low — startup `Err` paths skip orchestrator step 21.** `initialize_loop` returns `Err(LoopResult)` from many arms (path resolve, lock, worktree, `run_cmd::begin`) before `run_loop` reaches `cleanup_signal_files_for_prefix`. A prefix record written before the sweep and not removed there still names the batch pid, whose argv is `batch` + `run`, so pin 8 will trust it. Overwrite the same `{prefix}.json` on the next successful start. Deleting the record on those early returns is hygiene, not a new rule.
- **Low — the Pause row inside the box can stay truncated.** Pin 9 binds only the Stop row. `shorten_path_for_display` remains correct for DB and worktree lines.

## Questions for User

None. The pins answer the product choices (account waits, `.task-mgr/tasks` as an extra, equal mtime, `main_repo_root_at`, no fenced command-reference edit).

## Suggested Revisions

Do not rewrite the brief. If the seed is touched at all, add these sentences so a later edit cannot "simplify" past a pin:

1. `started_at` is one `SystemTime` captured at process entry. `run_batch` calls `engine::run_loop` in-process (`src/loop_engine/batch.rs`). Copy that same timestamp into every inner `SignalLocations`. Do not call `SystemTime::now()` at `run_loop` entry or next to the sweep. The sweep still runs at each loop start, after that prefix's `loop-runs/<prefix>.json` exists, and deletes only extra-dir `.stop-<prefix>` with mtime before or equal to that process-start clock.
2. Cmdline match: some argv element's final path component is exactly `task-mgr` (so `/usr/bin/task-mgr` counts and `not-task-mgr` does not), plus other elements `loop` and `run`, or `batch` and `run`. Not a substring. Not `kill -0` alone. Re-check that same predicate after the write; unlink only the path this command wrote.
3. After canonicalize, a path that is also canonical is removed from `extras` and keeps the no-mtime rule. A missing directory is omitted. `canonicalize` failure must not abort startup.
4. Exit cleanup of extras deletes `.stop-<prefix>` and `.pause-<prefix>` only. Do not call `cleanup_signal_files_for_prefix` on an extra: that helper always also removes global `.stop` and `.pause` in the directory it is given (`src/loop_engine/signals.rs`).
5. Absolute paths are `ui::emit` lines after `print_session_banner` returns. Do not add them to `format_session_banner`.

## How a "simpler" design fails

`check_stop_signal` is `exists` on `.stop-<prefix>` or else `.stop` in one directory. Pointing that function at a merged list, or calling `exists` on the three extra directories with no mtime, stops this process because of yesterday's file in the main checkout. Treating mtime equal to the clock as fresh does the same for a file stamped in the start second. The known-bad test (mtime set equal to `started_at`, expect false) is the only thing that keeps that simplification from shipping.

Rebuilding canonical from `paths.prd_file.parent()` after step 8.5 watches the worktree copy. That directory is not the one the loop watches today, and it would drop the mtime gate on a tree the operator already treats as live. `tasks_dir` from the pre-remap `resolve_paths` result is the only canonical for an inner loop.

Capturing `SystemTime::now()` inside each `run_loop` looks like "process start" and is wrong for batch. PRD 1 can run for a long time in the same process. A `.stop-<prd2>` created in main `tasks/` during PRD 1 is after process start, so pin 6 says PRD 2 must honor it. A clock taken when PRD 2's `initialize_loop` reaches the sweep is later than that file, so the sweep deletes it and the predicate ignores it. The operator did the right thing and the loop removed the evidence.

Deduping by dropping the directory entirely, instead of dropping it only from `extras`, applies the extra rule to today's signal directory whenever the PRD parent, the launch `tasks/`, and `main_repo_root_at(source_root)/tasks` are the same path. Global `.stop` in that directory would stop counting. Canonical wins on collision.

Adding `<db_dir>/tasks` as an extra because `ensure_directories` created it makes `.task-mgr/tasks/.stop-<prefix>` an inner-loop signal. The pin refuses that. Batch already uses that directory as its canonical global check; it is not a fourth watch root.

Reusing `cleanup_signal_files_for_prefix` on extras deletes a fresh global `.stop` in the main checkout at inner-loop exit. Batch's between-PRD check would then miss the file the operator placed to skip the remaining PRDs. Prefix-only removal is the whole extra-dir cleanup.

Putting the absolute paths into the 110-column box runs them through `shorten_path_for_display` (cwd-relative or `~/`) and `truncate_display` (`...`). That is the bug the banner has today. The Stop row inside the box is the short command `task-mgr loop stop --prefix <prefix>`. Paths are stderr lines after the box.

Folding operator stop into `HorizonStopped`, or setting `was_stopped` on the horizon arm, changes batch `--chain`. Horizon must keep `was_stopped: false`. File stop keeps exit 0, reason `"stop signal"`, `operator_stopped` / `was_stopped` true. SIGINT stays exit 130 via `signal_flag`, not via this file.

Teaching `wait_for_usage_reset` / `wait_for_ask_ttl_inner` / `transient_backoff_wait` about extras requires a new argument on `UsageGateFn` and `ResetWaitFn` and retargets the four `check_stop_signal(tasks_dir, None)` sites. Those sites only see a global file in the canonical directory. A prefix file is observed at the next iteration or wave boundary. That delay is accepted.

`LockGuard::acquire` is the short `tasks.db.lock` taken in `initialize_loop` and dropped before the named loop lock. The lock held for the whole run is `LockGuard::acquire_named(..., "loop-{prefix}.lock")` (`startup.rs`, then held as `loop_lock` until `run_loop` returns). `loop stop` that takes that named guard blocks until the loop exits, then the pin-8 recheck sees a dead pid and unlinks the canonical file it just wrote. Canonical files have no mtime gate, so the unlink is the only thing standing between a dead pid and a surprise stop of the next run. Taking the DB lock is also forbidden: the command does not need the database, and agent subprocesses hold `tasks.db.lock` during the iteration.

Trusting a record because `kill -0` succeeds, or because the cmdline contains the substring `task-mgr`, accepts `task-mgr list` and `not-task-mgr loop run`. A canonical file written for that pid is sticky. The post-write unlink exists because the process can die between the check and the write.

Scanning `slot_worktree_paths`, every `git worktree list` entry, or calling `git rev-parse` from an implicit cwd instead of `main_repo_root_at(source_root)` widens the watch set the pins closed. `None` from `main_repo_root_at` drops only the main-checkout candidate; the loop still runs.

## Verdict

APPROVED. Implement `SignalLocations` beside the unchanged single-directory predicate, consult it only at the iteration, wave, and batch-between-PRD boundaries, and keep account waits on `check_stop_signal(tasks_dir, None)`. Canonical for a loop is the pre-remap `ResolvedPaths.tasks_dir`; canonical for the batch check is `db_dir.join("tasks")`; extras are the launch `tasks/` directory, the feature worktree `tasks/`, and `main_repo_root_at(source_root)/tasks`, deduped after canonicalize, with prefix files only and a single process-start mtime clock. Stale extra-dir prefix stop files are deleted at loop start with a `ui::emit` recreate warning after the run record exists. `task-mgr loop stop --prefix` is a nested command with no `LockGuard`, a real argv check, and an unlink if the pid dies before the write sticks. The in-flight iteration finishes and the loop exits 0, distinct from `HorizonStopped`.
