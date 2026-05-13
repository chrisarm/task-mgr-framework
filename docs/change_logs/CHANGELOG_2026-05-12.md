# Changelog — 2026-05-12

## Auto-launch `/review-loop` after loop end

**Branch**: `feat/auto-review-after-loop`
**PRD**: `tasks/auto-review-after-loop.json`

### What shipped

`task-mgr loop` and `task-mgr batch` now auto-spawn an interactive
`claude "/review-loop tasks/<prd>.md"` session after a clean run, defaulting on
for runs that completed >=3 tasks. Behavior is configurable via
`.task-mgr/config.json` (`autoReview`, `autoReviewMinTasks`) and overridable
per invocation with mutually exclusive `--auto-review` / `--no-auto-review`
CLI flags on both `loop` and `batch` (parent + `run` subcommand). In batch
mode exactly one review fires for the LAST successful PRD that met the
threshold.

### Why it matters

Removes the manual hand-off step between a loop run and the code review pass.
The user lands directly in an interactive review session with full context
preserved (env vars inherit, TTY inherits). Auto-review failure NEVER alters
the loop/batch exit code, so existing scripts and CI behave identically.

Suppression cases (non-TTY, missing PRD markdown, missing worktree) each
print a one-line recovery hint instead of erroring.

### Breaking changes

None. The defaults match what most users would have done manually, and the
opt-out path (`--no-auto-review` or `autoReview: false`) is a one-flag flip.

---
