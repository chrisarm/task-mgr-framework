# JSON review: loop-stop-locations (pass 2)

PRD: `tasks/loop-stop-locations.md`
JSON: `tasks/loop-stop-locations.json`
Prompt: `tasks/loop-stop-locations-prompt.md`

Pass 1 was APPLY. Both findings are fixed. They are not open.

- FEAT-004 `touchesFiles` is `.gitignore` and `src/loop_engine/CLAUDE.md` only. The skill paths stay in the description and notes, which `validate_safe_path` does not scan. `loop init` will not reject this list for absolute paths.
- FEAT-001 criteria now require `pause_requested` on the same strict-after helper: canonical pause has no mtime gate, extra `.pause-<prefix>` must be strictly after `started_at`, global `.pause` in an extra does not pause, and the known-bad sets mtime equal to `started_at` and expects false for both the prefix stop file and the prefix pause file.

No task threads `SignalLocations` into account waits, treats flat `task-mgr loop <prd>` as a live stop target, or conflates operator stop with `HorizonStopped`. The four accepted Low residuals stay out of scope. The verify-task-mgr recipe still proves help, missing-record refusal, and invalid-prefix refusal, and reports the live loop skipped.

## Summary

- Total tasks: 6
- Tasks needing revision: 0
- Critical issues: 0
- Warnings: 0

## Critical Issues (Must Fix)

None. The loop can import and execute this list without a structural failure.

## Warnings (Should Fix)

None. Pass-1 pause coverage is in the acceptance criteria the agent sees.

## Suggestions (Nice to Have)

None to apply. Pass-1 notes that were left unchanged on purpose are not findings: FEAT-002 and FEAT-003 stay one task each; FEAT-002's `SystemTime::now()` wording is narrowed by the description ("at `run_loop` entry or next to the sweep") and by REVIEW-001 ("to set `started_at`"); the prompt data-flow diagram still omits `pause_requested`, and the FEAT-001 criteria now carry that contract.

## Missing Tasks

None. Do not add tasks for the four accepted Lows, for account-wait wiring, or for a TEST-INIT split.

## Task-by-Task Review

No task needs changes.

NOTHING_TO_APPLY
