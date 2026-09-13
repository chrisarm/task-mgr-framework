# Task lifecycle

Task lifecycle is how an operator sees work move: list what exists, take the next eligible task, mark it done, or skip/fail it, then confirm with `show`.

## Sub-features

- `list-all` lists every imported task with id, title, status, priority.
- `list-filter` lists only `--status todo` (and other status values).
- `next-read` returns the highest-priority eligible task without claiming it.
- `next-claim` claims that task (`in_progress`) inside a run.
- `complete-claimed` marks the claimed task `done`.
- `complete-todo-rejected` refuses `todo → done` without `--force`.
- `skip-task` defers a task with a required `--reason`.
- `show-task` is the second view of stored status, files, and `dependsOn`.

## How to get to it (user POV)

- Run `task-mgr list` or `task-mgr list --status todo`.
- Run `task-mgr next` (read-only) or `task-mgr next --claim --run-id <id>`.
- Run `task-mgr run begin` to obtain a `run_id` before claiming.
- Run `task-mgr complete <id> --run-id <id>`.
- Run `task-mgr skip <id> --reason <text>`.
- Run `task-mgr fail <id> --error <text>` (default status `blocked`).
- Run `task-mgr show <id>`.

## Driving it with verify-task-mgr

Preconditions:

- Fresh sandbox. `$H doctor` isolation ok.
- Canonical import already done: `$H capture life-init -- --format json loop init "$PRD" --no-prefix` with `"tasks_imported":7`.
- `$H capture life-list -- --format json list` shows `TASK-001` and `TASK-002` status `done` (`passes: true` in the fixture) and `TASK-003` status `todo`.

- **List all.** Run `$H capture life-list -- --format json list`. Exit code `0`. `"count":7`. Every element has `id`, `title`, `status`, `priority`.
- **Filter todo.** Run `$H capture life-list-todo -- --format json list --status todo`. Exit code `0`. Every `status` is `todo`. Count is `5` (`TASK-003`…`TASK-007`).
- **Read-only next.** Run `$H capture life-next -- --format json next`. Exit code `0`. `task.id` is `TASK-003` (priority 3; TASK-001/002 already `done`). `task.status` is still `todo`. `$H sql "SELECT status FROM tasks WHERE id='TASK-003'"` is `todo`.
- **Complete from todo is rejected.** Run `$H capture life-complete-todo -- --format json complete TASK-003`. Exit code is non-zero. Stderr names `task-mgr complete TASK-003 --commit <sha> --force` and does **not** contain `next --claim TASK-003`. `$H sql "SELECT status FROM tasks WHERE id='TASK-003'"` is still `todo`.
- **Begin a run.** Run `$H capture life-begin -- --format json run begin`. Exit code `0`. Stdout JSON has `run_id` (UUID) and `"status":"active"`. Save `run_id`.
- **Claim next.** Run `$H capture life-claim -- --format json next --claim --run-id "$RUN_ID"`. Exit code `0`. `task.id` is `TASK-003`. `claim.claimed` is `true`. `$H capture life-show-claimed -- --format json show TASK-003` has `task.status` of `in_progress`.
- **Complete claimed.** Run `$H capture life-complete -- --format json complete TASK-003 --run-id "$RUN_ID"`. Exit code `0`. Stdout JSON `tasks[0].previous_status` is `in_progress` and the task is completed. `$H capture life-show-done -- --format json show TASK-003` has `task.status` of `done`. `show` also lists files `src/feature/storage.rs`, `src/feature/mod.rs` and `depends_on` including `TASK-002`.
- **Skip another todo.** Run `$H capture life-skip -- --format json skip TASK-007 --reason "deferred for verification"`. Exit code `0`. `$H capture life-list-skipped -- --format json list --status skipped` contains `TASK-007`.
- **Next after complete.** Run `$H capture life-next-2 -- --format json next`. Exit code `0`. `task.id` is `TASK-004` (depends on already-done `TASK-002`; priority 4).
- **Proof.** `$H snapshot-db life-final` shows `TASK-001|done`, `TASK-002|done`, `TASK-003|done`, `TASK-007|skipped`, and remaining todos. Keep the `life-*` captures. After cleanup, those artifacts still exist.

## Gotchas

- `complete` from `todo` is an invalid transition unless `--force`. Recovery is `task-mgr complete TASK-003 --commit <sha> --force`. Do not run `next --claim TASK-003` — `--claim` is a boolean and would claim the highest-priority ready task across all prefixes.
- Completing `TASK-001` on this fixture does nothing useful: it imported `done`. Drive `TASK-003`.
- `next` without `--claim` must not change status. Always re-read with `show` or `sql`.
- `skip` and `irrelevant` require `--reason`. Omitting it is a clap error, not a lifecycle change.
- Completing `TASK-005` before `TASK-003` is `done` fails the dependency check unless `--force`.
- `list` text output truncates titles; use `--format json` for assertions.
- `--format json` is global: `task-mgr --format json list` and `task-mgr list --format json` both work. The helper accepts either after `cli --`.
