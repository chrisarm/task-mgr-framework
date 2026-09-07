# Status and doctor

Status is the progress dashboard for imported PRDs. Doctor is the read-only (or `--auto-fix`) health check operators run after a crash. Live `loop run` is a user entry point this harness refuses to spawn.

## Sub-features

- `status-dashboard` shows task counts and completion after import.
- `stats-summary` shows the same counts plus learnings and run info.
- `doctor-healthy` reports no issues on a fresh import.
- `doctor-stale` (optional) reports a stuck `in_progress` task when no active run tracks it.
- `loop-run-help` is the safe probe of the autonomous-loop entry point.
- `loop-run-live` is skipped: the helper refuses `loop run` / `batch run`.

## How to get to it (user POV)

- Run `task-mgr status` (optional PRD path or `--prefix`).
- Run `task-mgr stats`.
- Run `task-mgr doctor` (add `--auto-fix` to repair; add `--dry-run` to preview repairs).
- Run `task-mgr loop run tasks/<prd>.json --yes` to start the autonomous loop (not driven here).
- Run `task-mgr loop run --help` to inspect that entry point without spawning.

## Driving it with verify-task-mgr

Preconditions:

- Fresh sandbox with `$H capture dash-init -- --format json loop init "$PRD" --no-prefix`.
- `$H doctor` (the harness) reports `db: present`.

- **Status dashboard.** Run `$H capture dash-status -- --format json status`. Exit code `0`. JSON `tasks.total` is `7`, `tasks.done` is `2`, `tasks.todo` is `5`, `completion_percentage` is about `28.57`. `project.name` is `sample-test-project`. Text equivalent contains `=== Status Dashboard ===` and `Progress: 2/7 tasks`.
- **Stats.** Run `$H capture dash-stats -- --format json stats`. Exit code `0`. `tasks.total` is `7`. `learnings` counts are present.
- **Healthy doctor.** Run `$H capture dash-doctor -- --format json doctor`. Exit code `0`. `summary.total_issues` is `0`. Text equivalent contains `✓ No issues found. Database is healthy.`
- **Stale in_progress (optional second view).** Claim without a run: `$H capture dash-claim -- --format json next --claim`. Then `$H capture dash-doctor-stale -- --format json doctor`. `issues` includes `stale_in_progress_task` for `TASK-003` (claimed, no active run). Do not pass `--auto-fix` unless the recipe is specifically proving repair; if you do, follow with another `doctor` that returns `total_issues` 0 and `snapshot-db` showing `TASK-003` back at `todo`.
- **Loop help (safe).** Run `$H capture dash-loop-help -- loop run --help`. Exit code `0`. Stdout describes `loop run` flags including `--yes` and `--hours`.
- **Live loop (unreachable here).** `$H cli -- loop run "$PRD" --yes` is refused by the helper (exit 2, message `refusing 'loop run'`). Record that as `verified-unreachable` with precondition “harness isolation: no agent spawn, no git worktrees on the operator clone.” Do not bypass the helper to force a spawn.
- **Proof.** Keep `dash-status.*`, `dash-stats.*`, `dash-doctor.*`, `dash-loop-help.*`. After cleanup they still exist.

## Gotchas

- `status` with no import looks like “no project initialized”; that is not a failed binary, it is empty state. Import first.
- A fresh sample-PRD import is already 2/7 done. Do not assert 0% completion.
- `doctor --auto-fix` mutates tasks (stale `in_progress` → `todo`, abandoned runs). Prefer JSON `dry_run` / `--dry-run` before repair.
- `doctor --setup` audits Claude Code config under `$HOME`. In this harness that is the sandbox home, not the operator config — do not treat a missing `~/.claude/settings.json` in the sandbox as an operator machine problem.
- `loop run` without `--yes` prompts on a TTY. Captured stdin is not a TTY; do not start it.
- Parallel-slot loops write worktrees named `<branch>-slot-N`. That is why live loop is out of default verification: leftover worktrees and cached test binaries are checkout-wide, not sandbox-local.
