# Learnings

Learnings are operator-recorded notes (failure, success, workaround, pattern) stored in the same database as tasks, listed with `learnings`, and recalled for a task by file and type match.

## Sub-features

- `learn-record` inserts a learning and returns a numeric `learning_id`.
- `learnings-list` lists active learnings including the new title.
- `recall-for-task` returns that learning for `TASK-001` via `--for-task` (file-pattern match; no network).
- `recall-query-skipped` documents `recall --query` as Ollama-backed and not part of the default proof.

## How to get to it (user POV)

- Run `task-mgr learn --outcome <failure|success|workaround|pattern> --title ... --content ...` with optional `--files`, `--tags`, `--task-id`.
- Run `task-mgr learnings` or `task-mgr learnings --recent N`.
- Run `task-mgr recall --for-task <id>`.
- Run `task-mgr recall --query <text>` (vector backend; needs Ollama unless `--allow-degraded`).

## Driving it with verify-task-mgr

Preconditions:

- Fresh sandbox with `$H capture learn-init -- --format json loop init "$PRD" --no-prefix` (`tasks_imported` 7).
- `$H doctor` shows `db: present`.

- **Record a pattern.** Run `$H capture learn-record -- --format json learn --outcome pattern --title "Foundation module layout" --content "Put types next to the module root" --files src/feature/mod.rs --tags rust,layout --confidence high`. Exit code `0`. Stdout JSON has `"title":"Foundation module layout"`, `"outcome":"pattern"`, and a positive integer `learning_id`. Save that id.
- **List learnings.** Run `$H capture learn-list -- --format json learnings`. Exit code `0`. `"count"` is at least `1`. Some element has `id` equal to the saved `learning_id` and the same title. `"total"` counts active (non-retired) rows.
- **Recall for TASK-001.** Run `$H capture learn-recall -- --format json recall --for-task TASK-001`. Exit code `0`. `for_task` is `TASK-001`. `learnings` contains the recorded title (TASK-001 touches `src/feature/mod.rs`, which matches `--files`).
- **DB side effect.** Run `$H sql "SELECT id, title, outcome FROM learnings WHERE retired_at IS NULL"`. The recorded title is present.
- **Query path (do not drive live).** Do not run `recall --query` unless Ollama is the object of the test. Without `--allow-degraded`, an unreachable embedder is a non-zero exit, not an empty list. If you must touch that flag, assert the failure or the degraded empty vector results explicitly — do not call it equivalent to `--for-task`.
- **Proof.** Keep `learn-record.*`, `learn-list.*`, `learn-recall.*`. After cleanup they still exist.

## Gotchas

- `--for-task` does not require Ollama. `--query` does, by default.
- `--task-id` on `learn` is a foreign key: the task must exist. Recording against `TASK-001` after import is fine; recording before import fails.
- `learnings` JSON uses `id`; `learn` JSON uses `learning_id`. Assert the correct field per command.
- Recall ranking can include other rows if the sandbox was reused. Fresh `sandbox-new` keeps the list to what this recipe inserted.
- Do not `delete-learning` during the proof; cleanup removes the whole sandbox DB.
