# Init and import

Init scaffolds a project database directory and imports a PRD JSON file into SQLite so later commands have tasks to list. Operators scaffold with `task-mgr init`, import with `task-mgr loop init <prd>`, and still have a deprecated `task-mgr init --from-json <prd>` shim.

## Sub-features

- `init-project` creates `.task-mgr/tasks.db` and `config.json` under `--dir` and stages bundled skills into `$HOME/.claude/commands/`.
- `loop-init` imports the seeded sample PRD with stable `--no-prefix` ids `TASK-001`…`TASK-007`.
- `init-shim` is the deprecated `init --from-json` path: same import, plus a stderr deprecation notice.
- `loop-init-dry-run` previews import without inserting task rows.
- `skills-isolated` writes skills only under the sandbox HOME, never the operator HOME.

## How to get to it (user POV)

- Run `task-mgr init` in a project (no PRD).
- Run `task-mgr loop init tasks/<prd>.json` (canonical import).
- Run `task-mgr init --from-json tasks/<prd>.json` (deprecated shim, still supported).
- Add `--no-prefix` to import ids exactly as they appear in the JSON.
- Add `--dry-run` to preview an import.
- Add `--append --update-existing` to refresh an already-imported PRD (not required for the baseline proof).

## Driving it with verify-task-mgr

Preconditions:

- `$H launch` and `$H sandbox-new` have succeeded.
- `$H doctor` reports isolation ok and `db: absent`.
- `PRD` is `VERIFY_TASK_MGR_PRD` from `$H env-print`.
- No `tasks.db` exists yet under `VERIFY_TASK_MGR_DIR`.

- **Project scaffold.** Create the database directory. Run `$H capture init-project -- --format json init`. Exit code `0`. Stderr contains `Initialized .task-mgr/`. `VERIFY_TASK_MGR_DIR/tasks.db` and `VERIFY_TASK_MGR_DIR/config.json` exist. Stderr may contain `Staged skills to ~/.claude/commands/`.
- **Skills isolation.** Confirm staging stayed in the sandbox. Run `ls "$VERIFY_TASK_MGR_HOME/.claude/commands"`. The directory contains bundled `*.md` skill files. Do not write to the operator `~/.claude/commands/`.
- **Dry-run import.** Preview without inserting tasks. Run `$H capture init-dry -- --format json loop init "$PRD" --no-prefix --dry-run`. Exit code `0`. Stdout JSON has `"dry_run":true` and `"tasks_imported":7`. Then run `$H sql "SELECT COUNT(*) FROM tasks"`. The count is `0`. A `tasks.db` file may exist because dry-run still opens/migrates the database.
- **Canonical import.** Import the sample PRD. Run `$H capture init-loop -- --format json loop init "$PRD" --no-prefix`. Exit code `0`. Stdout JSON has `"tasks_imported":7`, `"files_imported":14`, `"relationships_imported":6`, `"dry_run":false`. Stderr contains `deprecated relationship fields (synergyWith/batchWith/conflictsWith); these are ignored`. Text equivalent (if you omit `--format json`) contains `Initialized: 7 tasks, 14 files, 6 relationships`.
- **List after import.** Read the tasks back. Run `$H capture init-list -- --format json list`. Exit code `0`. Stdout JSON has `"count":7`. `TASK-001` has title `Create core module structure` and status `done`. `TASK-002` is `done`. `TASK-003`…`TASK-007` are `todo`.
- **DB side effect.** Dump rows. Run `$H snapshot-db after-init`. The dump lists seven ids `TASK-001`…`TASK-007` with `TASK-001|done`, `TASK-002|done`, and the rest `todo`.
- **Deprecated shim (fresh sandbox).** Rebuild isolation so this entry point is not skipped. Run `$H cleanup` then `$H sandbox-new --replace` (or a new id), then `$H capture init-shim -- --format json init --no-prefix --from-json "$PRD"`. Exit code `0`. Stderr contains `DEPRECATED:` and `canonical form is \`task-mgr loop init`. Stdout JSON still has `"tasks_imported":7`. `$H capture init-shim-list -- --format json list` returns `"count":7`.
- **Proof.** Keep `init-project.*`, `init-dry.*`, `init-loop.*`, `init-list.*`, `after-init.db.txt`, and (if driven) `init-shim.*` under `artifacts/<run-id>/`. After `$H cleanup`, those files still exist.

## Gotchas

- Omitting `--dir` (running raw `task-mgr init` in this checkout) writes the operator project `.task-mgr/` and stages skills into the real `$HOME`. Always use `$H`.
- `task-mgr init` treats `--dir` as the database directory and uses its *parent* as the project root (`init_project` creates `<parent>/.task-mgr`). The helper sets `--dir` to `<sandbox>/project/.task-mgr` so the parent is the throwaway project. Pointing `--dir` at a random temp folder would create `<temp's parent>/.task-mgr`.
- `loop init --dry-run` still creates/migrates `tasks.db`. Assert on task *rows*, not on the file's absence.
- The shim `init --from-json --dry-run` still runs project scaffold (`config.json` appears) and skips skill staging and task rows. Observe each.
- Without `--no-prefix`, this fixture prefixes ids as `3019e47c-TASK-001` (`taskPrefix` in the JSON). Recipes in this map use `--no-prefix`.
- `passes: true` on a story imports as `done`. This fixture marks TASK-001 and TASK-002 that way, so a fresh import is not seven todos and `next` returns `TASK-003`.
- Auto prefix mode can write `taskPrefix` back into the PRD file. That is why the sandbox copies the fixture instead of importing `tests/fixtures/sample_prd.json` in place.
- `--enhance` writes `CLAUDE.md` / `AGENTS.md` into the project root. Safe in the sandbox; never run it against this checkout as a verification step.
- `loop init` does not fire the model-anchor picker. Project-level `task-mgr init` may, but only when stdin and stderr are both TTYs — captured runs skip it.
