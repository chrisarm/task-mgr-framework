# Claude Code Agent Instructions

You are an autonomous coding agent implementing **CLI-Error-Gated Output Classification + Completion-Safe Reaction Ordering** for **task-mgr**.

## Problem Statement

`detection::analyze_output` classifies an iteration by substring-matching the agent's final result text. `is_rate_limited`, `is_prompt_too_long`, and `is_transient_backend` fire on the agent's own prose. A successful summary that mentions a 429 resets the task from `in_progress` to `todo` before the pipeline records `<completed>`. `complete(..., force=false)` then rejects `todo → done` (`lifecycle/complete.rs`). The loop also parks for hours on the Usage API reset. Re-running the task writes a similar summary and trips again.

The fix is provenance. Text classifiers run only when there is a CLI-error signal: `result.is_error`, `StreamEvent::Error`, or exit ≠ 0 that task-mgr did not cause with a post-completion grace kill. Reactions that reset state run after the completion ladder and read a snapshot of the pre-pipeline outcome, so a real limit still waits and a tagged task stays `done`.

Source PRD: `tasks/prd-cli-error-gated-detection.md`. Do not re-read it in full unless a task names a section you do not have below.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Satisfy this task** — meet the acceptance criteria and prohibited outcomes in this prompt.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it will be handled before coding.
3. **Pick an approach** — State assumptions. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, run the scoped quality gate. This PRD does not map onto a verify-task-mgr feature. Do not invent a second harness and do not launch `loop run`.

---

## Priority Philosophy

In order: **PLAN** → **PHASE 2 FOUNDATION** (typed provenance, one gate, one snapshot) → **FUNCTIONING CODE** → **CORRECTNESS** → **CODE QUALITY** → **POLISH**.

**Prohibited outcomes:**

- Classifying agent prose as RateLimit, PromptTooLong, or TransientBackend when has_cli_error is false
- Scanning the agent summary when cli_error is set (scan the retained error text)
- Treating tool_result.is_error as a CLI error
- Treating a grace-kill (completion_killed, exit 143 or 137) as a CLI error
- A magic prefix or sentinel inside output instead of a typed RunnerResult field
- Reading the post-pipeline outcome inside react_to_outputs (record_completion rewrites it to Completed and the wait is skipped)
- Letting a flipped Completed consume the iteration budget
- Restoring OperatorStopped or StopSpend exit 130 or exit 1
- Returning task_id None and empty output before the completion ladder on a stop
- StopSpend that does not set account_quota_stopped
- A skip that wraps only recover_in_progress inside handle_overflow (context rows and rungs 1-5 still run)
- Moving an untagged handle_overflow call to after process_iteration_output
- Scanning the head-capped conversation alone for the grace-kill `<completed>` tag
- Putting output text on detection.text_signal_without_cli_error (tracing has no redactor)
- Changing classifier pattern sets or decide_account_rate_limit wait math
- Collapsing pre-iteration and post-output usage I/O onto one flag
- Tests that only assert no crash, or that call the live Usage API
- Hand-editing tasks/*.json

---

## Global Acceptance Criteria

These apply to every implementation task. The task-level criteria in `## Current Task` are layered on top.

- Rust: cargo fmt --check passes
- Rust: cargo check has no warnings
- Rust: cargo clippy --all-targets -- -D warnings passes for the touched crate
- Scoped tests for the touched modules pass
- No unwrap() on stream-json fields; missing is_error is false
- No per-task model field and no top-level PRD model field

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Everything about this iteration's task is in `## Current Task`. PRD-wide rules are in this prompt.

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/cli-error-gated-detection.json)
```

| Need | Command |
| --- | --- |
| Inspect this iteration's task | `task-mgr show <TASK-ID>` |
| List remaining tasks | `task-mgr list --prefix $PREFIX --status todo` |
| Recall | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up | `echo '{...}' \| task-mgr add --stdin --from-json tasks/cli-error-gated-detection.json --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| --- | --- |
| `tasks/cli-error-gated-detection-prompt.md` | This prompt (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — tail, then append |

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read on the first iteration. The contract text lives under `## CONTRACT-001` once that task is done. Dependents read that section, not the whole log.

---

## Your Task (every iteration)

1. Work the task in `## Current Task`.
2. Pull only the progress context you need.
3. Recall with `task-mgr recall --for-task`. Do not Read `tasks/long-term-learnings.md` or `CLAUDE.md` in full. The excerpts below are the CLAUDE.md content for this PRD.
4. `git branch --show-current` matches the branch task-mgr printed.
5. Think before coding. For high-effort or behavior-modifying tasks, name one rejected alternative.
6. Implement the one task.
7. Run the scoped gate, then commit.
8. Commit: `fix: <TASK-ID>-completed - [Title]` (or `test:` / `docs:` / `chore:` as appropriate).
9. Emit `<task-status><TASK-ID>:done</task-status>`.
10. Append one progress block terminated with `---`.

---

## Quality Checks

### Per-iteration scoped gate

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test -p task-mgr <module_or_fn_name>
```

Do not run the unscoped suite during a FEAT. That is REVIEW-001.

### Final gate at REVIEW-001

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

Fix pre-existing failures too. Under about 12 unrelated failures, fix them here. Above that, spawn one FIX via `task-mgr add --stdin --from-json tasks/cli-error-gated-detection.json --depended-on-by REVIEW-001` and emit `<promise>BLOCKED</promise>`.

---

## Project Verification Skills

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** none of the feature ids. The change is loop-engine classification and reaction order. The harness refuses `loop run` and `batch run`. Do not invent a loop-run drive. Unit and integration tests are the proof.

---

## Contract Tasks

`CONTRACT-001` is design-only. Record the full contract in the progress log under `## CONTRACT-001`. Do not write production code. Downstream FEAT tasks implement that record. If the contract is wrong, reopen it or spawn a follow-up. Do not silently diverge.

---

## Review Tasks

| Review | Priority | Spawns | Focus |
| --- | --- | --- | --- |
| CODE-REVIEW-1 | 13 | CODE-FIX / WIRE-FIX at 14-20 | unwrap, wiring, §2.7 evidence, contract fidelity |
| PRUNE-001 | 60 | PRUNE-FIX at 61-69 | Existence on the branch diff only. Do not edit production code. Criteria say "spawn PRUNE-FIX tasks" and do not contain the token `PRUNE-FIX-` |
| REFACTOR-REVIEW-FINAL | 70 | REFACTOR-xxx at 71-85 | Shape code that stays. AC includes bare `PRUNE-FIX-xxx` |
| REVIEW-001 | 99 | FIX if the full suite needs it | Full suite. AC includes bare `PRUNE-FIX-xxx`, `CODE-FIX-xxx`, and `WIRE-FIX-xxx` |

Spawn shape:

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-1: <details>",
  "acceptanceCriteria": ["Issue resolved", "Class query or named test covers every site", "Scoped tests pass"],
  "priority": 14,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --from-json tasks/cli-error-gated-detection.json --depended-on-by REVIEW-001
```

PRUNE-FIX uses the same command with `--depended-on-by REVIEW-001` and priority 61-69. One fix per finding class.

---

## Progress Report Format

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence]
Files: [paths]
Learnings: [1-3 one-line bullets]
---
```

---

## Key Learnings (from task-mgr recall)

- **[5364]** Live CLI copy uses "reached your … limit". Keep the widened patterns. Gate them. Do not narrow them.
- **[5298]** Do not collapse pre-iteration and post-output Anthropic I/O onto one flag. The probe is not silenced by `LOOP_USAGE_CHECK_ENABLED` while Claude is enabled.
- **[5373]** OperatorStopped and StopSpend are exit 0. Do not restore 130 vs 1.
- **[5391]** Spend-limit banners park the wait. StopSpend still sets `account_quota_stopped` so `batch --chain` aborts.
- **[5347]** Untagged `handle_overflow` runs before `process_iteration_output` on both paths. Do not move that call.
- **[5350]** Overflow rungs 1–3 recover, 4 overrides the model, 5 auto-blocks. A tag skip skips the whole call, so none of those run.
- **[4126]** Sequential and wave share the account-reaction inner. One wait per wave, not per slot.
- **[5386]** RateLimit tests must not call the live Usage API. Inject the wait.
- **[2188]** `<completed>Y</completed>` while claiming X marks Y done and leaves X pending.
- **[4863]** When relocating a reaction, move the body. Do not leave a second copy.
- **[3870]** The DB flips to done only from an explicit completion signal. A classifier result is not that signal.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

- Account reactions (`react_to_outputs`, `react_to_transient`) fire once per wave. Coordinators destructure param structs exhaustively (no `..`). `iteration.rs`, `slot.rs`, and `wave_scheduler.rs` deny deprecated leaves.
- `handle_overflow` for an untagged PromptTooLong fires before `process_iteration_output` on both paths, so crash-tracking does not treat a retry as a terminal failure. This PRD adds one exception: the resolved claimed id is in `parse_completed_tasks(output)` → skip the entire call.
- `consumes_budget` / `iteration_consumed == false` gives the loop iteration back for RateLimit. After this PRD, that decision follows the pre-pipeline snapshot, not the outcome `record_completion` rewrote to Completed.
- The rate-limit reset filters `status = 'in_progress'`. That guard does not by itself keep the wait. The snapshot does.
- Wave `WaitedAndRetry` returns before merge-back. Do not merge inside the reaction. Sequential `react_to_completions` (wrapper commit) runs after the ladder and before the wait.
- OperatorStopped → exit 0 `"stop signal"`. StopSpend → exit 0, `was_stopped` false, and `account_quota_stopped` so batch `--chain` aborts.
- Dual predicate: pre-iteration usage check is `LOOP_USAGE_CHECK_ENABLED && Claude enabled`. Post-output early-lift probe is Claude enabled only. Do not collapse them.
- Claim-scoped tag rewrite is identity, then prefix strip, then 8-lowercase-hex body. Never `ends_with`.
- Status writes go through `TaskLifecycle`. `recover_in_progress` is `in_progress` only. `reopen_after_merge_fail` is a different verb. Do not add `WHERE status = 'in_progress'` to `resurrect_for_iteration`.
- Codex `turn.failed` becomes `StreamEvent::Error`. Auth scans match `[Error: ` lines only. The rate-limit scan for E13 uses the retained error string, not `derive_output`.
- Product UX uses `ui::*`. Diagnostics use `tracing`. The new warn goes to `task-mgr-<prefix>.log`. `active_prefix` is the correlation id. Tracing has no redactor, so the event must not include output text.
- `complete(..., force=false)` rejects `todo → done`. That is the incident's lost completion. The matrix allow-list is a later operator gate.

---

## Data Flow Contracts

Use these access patterns. Do not guess key types.

- stream-json `result` line → accumulator: `val.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false)` inside the `Some("result")` arm only. `tool_result.is_error` is a different field. Do not read it for `cli_error`.
- Assistant or Codex error → `StreamEvent::Error(String)`. On that event, set `cli_error` and store the string for the gated scan.
- `drive_stream` returns output, conversation, denials, plus `cli_error`, the retained error text, and a tail of the buffer `arm_completion_grace` already scans. `conversation` is head-capped (50_000 bytes) and is not the grace fallback.
- `RunnerResult` → `analyze_output(&output, exit_code, signals, dir)` at the sequential and wave classify sites. `signals` carries `cli_error`, `completion_killed`, and `error_text`. When `cli_error` is set, scan `error_text`, not `output`.
- Grok: `cli_error: false`. Codex: `cli_error` from `error` / `turn.failed`, and the retained text is that error string (it is not in `derive_output`).
- Pre-pipeline snapshot: copy `IterationOutcome` before `process_iteration_output` / `process_slot_result`. `react_to_outputs` items hold that copy. `record_completion` sets the live outcome to `Completed` and must not be the reaction input.
- `process_iteration_output` never sees `RunnerResult`. Copy `grace_buffer_tail` and `completion_killed` onto `IterationResult` in `run_iteration` and `run_slot_iteration`, then into both `ProcessingParams` literals (`orchestrator.rs` and `slot.rs`). Early returns use an empty tail and `completion_killed: false`. The pipeline scans `ProcessingParams`, not `RunnerResult`.

`has_cli_error = cli_error || (exit_code != 0 && !completion_killed)`.

- Limit-shape record (FR-007): append one JSON object to `.task-mgr/logs/limit-shape-<prefix>.jsonl` when exit ≠ 0, `cli_error` is set, or any captured channel matches a text classifier. A clean exit 0 with no match writes nothing. Store only that provider's channels, each text field the trailing 4096 bytes. Claude: `result_is_error`, `result_text`, `assistant_error`, and `stderr` (pipe stderr and tee it; it is inherited and discarded today). Grok: `stderr` and `output_tail`. Codex: `error_text` (the `turn.failed` / `error` string), `stderr`, and `output_tail`. Do not put this text on a tracing event. Do not retarget `is_rate_limited` onto stderr. A 502 stays `TransientBackend`. The jsonl line is the fixture for a later scan fix.

---

## Feature-Specific Checks

- `cli_error` wins over `completion_killed`. A grace-kill that also has `is_error` or `StreamEvent::Error` is still a CLI error.
- Exit 143 without `completion_killed` still takes the external-signal path. Exit 137 does not set that stop flag.
- Probe: exit 0 is lifted. Text is scanned only when exit ≠ 0. The probe does not call `analyze_output`.
- Git-only and output-scan completions are not protected from `handle_overflow`. Do not add that protection.

---

## Important Rules

- Work on one story per iteration.
- Commit after each passing story.
- Read before writing.
- Minimal changes.
- Follow a `follow <path>` note on the task. That is the pattern to copy.
- Mark status with `<task-status>`. Do not edit the JSON.

## Stop and Blocked Conditions

When every story has `passes: true` and the final review spawned nothing new:

```
<promise>COMPLETE</promise>
```

If blocked, append the reason to the progress log and:

```
<promise>BLOCKED</promise>
```
