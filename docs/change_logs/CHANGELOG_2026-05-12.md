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

## Auto-review follow-ups

**Branch**: `feat/auto-review-after-loop`
**PRD**: `tasks/auto-review-followups.json`

### What shipped

Four follow-up tasks addressing review findings on the auto-review feature:
(1) whitespace guard in `maybe_fire` — paths containing any Unicode whitespace
now suppress the launch with a rename hint instead of producing a fragmented
`/review-loop` argv; (2) rustdoc clarifying the operator-controlled trust
boundary on `prd_md_path` and the TOCTOU race between the worktree existence
check and the launcher's `current_dir`; (3) a zero-threshold regression test
asserting `should_fire(min_tasks=0, ...)` behaves correctly at the boundary;
(4) hoisting the `chain_base` snapshot out of the `make_result` closure in
`batch.rs` to collapse a duplicate `if chain { chain_base.clone() } else { None }`.

### Why it matters

Hardens the auto-review feature against a known footgun (whitespace paths)
before it can bite an operator. Documentation additions make trust-boundary
and concurrency assumptions explicit for future maintainers. The
`chain_base` hoist removes a small DRY violation without behavior change.

### Breaking changes

None.

---
