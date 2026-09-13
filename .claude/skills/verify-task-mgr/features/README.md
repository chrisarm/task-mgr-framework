# task-mgr verification map

This directory is the maintained source for verifying the user-facing behavior of the task-mgr CLI. Read the index before driving the app, then use the matching feature file as the recipe.

## Baseline preconditions

- Run `$H launch` so the binary is this checkout's build, not `PATH`.
- Run `$H sandbox-new` so `--dir` is `/tmp/task-mgr-verify-<id>/project/.task-mgr` and `HOME` is `/tmp/task-mgr-verify-<id>/home`.
- Seeded PRD is the sandbox copy of `tests/fixtures/sample_prd.json` (`VERIFY_TASK_MGR_PRD`).
- `$H doctor` reports isolation ok. After import, `db: present`.
- Never drive the checkout `.task-mgr/` or the operator's real `$HOME`.
- Never invoke `loop run` or `batch run` through this harness.

`H=.claude/skills/verify-task-mgr/scripts/verify-task-mgr`

## Driving conventions

- Start every recipe from a fresh `sandbox-new` unless its preconditions say otherwise.
- Pass every product command through `$H cli` or `$H capture`. The helper injects `--dir`.
- Prefer `--format json` for assertions. `models` is text-only even with that flag.
- Treat every command as literal. Keep task ids (`TASK-001`) and flag names unchanged.
- Restore a fresh sandbox after a mutation that would poison a later recipe. Do not remove proof artifacts during cleanup.

## Proof and skip reporting

- Capture the user action and the resulting state, not only the last stdout.
- CLI proof includes the command, stdout, stderr, and exit code (`capture` writes all four).
- Mutation proof includes a second read (`show`, `list`, `models show`, or `snapshot-db`).
- Record the feature id and entry point used with every artifact.
- Report an unreachable path with the attempted command and the unmet precondition.
- Do not report a skipped entry point as verified through a different path.

## Feature entry contract

Each feature file starts with an H1 title and one paragraph describing the user-visible behavior. It then uses exactly four H2 sections in this order.

1. `Sub-features` lists short IDs with one line for each behavior.
2. `How to get to it (user POV)` lists every user entry point.
3. `Driving it with verify-task-mgr` starts with `Preconditions:` and uses labeled bullets that pair each user action with an exact command and observable result.
4. `Gotchas` lists traps that can waste or invalidate a verification run.

Keep implementation details out of the map. Name only user paths, stable handles, required state, commands, and observable proof.

## Features

- [Init and import](./init-and-import.md) covers project scaffold, canonical `loop init`, the deprecated `--from-json` shim, dry-run, and skill staging isolation.
- [Task lifecycle](./task-lifecycle.md) covers list, next, claim, complete, skip, fail, and show.
- [Learnings](./learnings.md) covers record, list, and `--for-task` recall.
- [Models routing](./models-routing.md) covers `models init`, `show`, offline `list`, and `set-anchor`.
- [Status and doctor](./status-and-doctor.md) covers the status dashboard, health check, and the skipped live-loop path.
