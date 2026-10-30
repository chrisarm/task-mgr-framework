# PRD: CLI-Error-Gated Output Classification + Completion-Safe Reaction Ordering

**Type**: Bug Fix
**Priority**: P1 (High) — a loop whose PRD talks about HTTP 429s can never progress
**Author**: Claude Code
**Created**: 2026-10-06
**Status**: Draft

---

## 1. Overview

### Problem Statement

`detection::analyze_output` classifies an iteration by substring-matching the agent's **final result text** (`RunnerResult.output`). Three classifiers — `is_rate_limited`, `is_prompt_too_long`, `is_transient_backend` — fire on the agent's own prose, not only on CLI/API errors. A successful agent summary that merely *mentions* a rate limit, an oversized prompt, or a 503 is misclassified, and the misclassification is destructive:

1. **The task is reset** `in_progress → todo` by the account reaction (`reactions/account.rs:701 reset_in_progress_tasks`) **before** the pipeline records `<completed>` (`iteration_pipeline.rs:358`, reached from `orchestrator.rs:399`). The later `mark_task_done` calls `complete(..., force=false)`, which rejects `todo → done` at `lifecycle/complete.rs:70-72` (the matrix allow-list at `matrix.rs:79` is a later operator gate, not this error). The failure is only logged as a warning (`iteration_pipeline.rs:393`). The finished work is silently discarded.
2. **The loop parks for hours.** With the Claude provider enabled, the reaction loads the Usage API reset time (`api_secs`) and waits for it, independent of `LOOP_USAGE_CHECK_ENABLED` (deliberately so; see learning 5298).
3. **The loop can never progress.** Re-running the task produces a similar summary, which re-trips the classifier. This repeats every time.

### Background — incident (2026-10-06, consumer repo mw_support, loop prefix `17a0ade1`)

- PRD: "ADP rate-limit resilience". FEAT-001 = "Log ADP 429 response diagnostics".
- Haiku implemented it, the gate passed, and it committed `03f162f3 feat: 17a0ade1-FEAT-001-completed`. Its final summary said "One warn line per 429 with operation, headers..." and "`rate_limit_diagnostic_headers()`".
- `is_rate_limited` matched `contains("429") && (contains("rate") || contains("limit"))` (`detection.rs:154`) across the whole output, so the outcome was `RateLimit`.
- Log:
  ```
  Rate limit detected in output, running account reaction...
  Usage API: window resets in 4h 37m 41s.
  session 99% left (4h 37m 40s) · week 6% left (27m 40s) · frontier 1% left (27m 40s)
  Waiting 4h 37m 41s for usage reset; probing every 30s ...
  ```
- FEAT-001 went `in_progress → todo`. After the early-lift probe resumed the loop, FEAT-001 re-ran, re-verified, emitted a similar summary, and tripped again ("Waiting 4h 53m 41s"). The operator stopped the loop by hand.
- Real CLI limits look different. Operator logs (`operator-loop-7c003548.log:302`, `ff1b6d8e.log:142`) show "You've hit your session limit · resets 4pm" with the CLI exiting **code 1**.

### Root cause (one sentence)

Classification keys on *what the text says* instead of *who produced it*. Agent prose and CLI error strings travel through the same `output: String`, and no structural signal survives to tell them apart.

---

## 2. Goals

### Primary Goals

- [ ] An iteration whose CLI run did **not** report an error is never classified `RateLimit`, `Crash(PromptTooLong)`, or `TransientBackend`, regardless of output text.
- [ ] Every real CLI limit/overflow/backend message currently detected (all existing exit≠0 tests) is still detected.
- [ ] On both the sequential and wave paths, a task whose `<completed>ID</completed>` was emitted is recorded `done` before any state-resetting reaction. The account wait still runs when the **pre-pipeline** outcome was `RateLimit`, even if `record_completion` (`iteration_pipeline.rs:625`) rewrites that outcome to `Completed`. The wave path's current order is not sufficient for this: it already runs the ladder first (`wave_scheduler.rs:1061` then `:1115`), but the reaction then sees no `RateLimit` item and returns `None` (`account.rs:693-698`), so a slot that is both rate-limited and tagged never waits.

### Success Metrics

- Replaying the FEAT-001 incident (its exact summary, exit 0 or grace-killed 143 with `completion_killed=true`) gives an outcome ≠ `RateLimit`, and the task ends `done`.
- 0 regressions in the existing `detection.rs` exit≠0 classifier tests.
- The mw_support `17a0ade1` loop completes FEAT-001 → FEAT-006 without any `Rate limit detected` line while the account has >2% remaining.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **Provenance over text.** Text classifiers may only inspect output when there is a CLI-error signal (see FR-002). Under that gate, the existing pattern sets are **kept unchanged**. Recall matters more than precision for genuine error text (learning 5364 widened them on purpose for live Fable copy).
- **A grace-kill is not a CLI error.** A run that task-mgr itself terminated after `<completed>` (`completion_killed == true`, exit 143/137) must be treated as *no CLI error*. A bare `exit_code != 0` gate would leave this incident unfixed, because FEAT-001's run was grace-killed ("Post-completion grace expired, terminating Claude process group").
- **A completion is never lost to a reaction.** Every path that resets `in_progress → todo` (RateLimit account reaction; PromptTooLong `handle_overflow` rungs 1-3) must run **after** the `<completed>` ladder has recorded completion, or must skip the whole reaction for tasks whose IDs are in the completed tags. The account reaction reads a **snapshot** of the pre-pipeline outcome, not the outcome the ladder rewrote.
- **Not-RateLimit is not done.** A grace-kill that avoids `RateLimit` is still `Crash(RuntimeError)` at `detection.rs` step 4 (exit 143, locked by `test_exit_code_143_sigterm`). `done` comes only from the completion ladder (tag in the scanned text, or a `-completed` commit on the sequential path). The classifier gate does not record completion.
- **No regression for Grok/Codex.** Grok has no error events (`stream.rs` assistant arm), so `cli_error` stays false. The exit≠0 arm still scans assistant `output`, which is not the stderr diagnostic. Codex `error` / `turn.failed` sets `cli_error` and the gated scan uses that error string, which is **not** in `derive_output`. Existing stderr-based transient classification (`runner.rs:998`, `runner.rs:1273`) is unchanged. FR-007 records each provider's channels and does not retarget the scan. `tool_result.is_error` is never a CLI error.

### Performance Requirements

- Best effort. The gate is an O(1) boolean check ahead of the existing O(n) substring scans, so a clean run now skips those scans entirely.

### Style Requirements

- Follow the existing patterns in `loop_engine`: pure classifier functions in `detection.rs`, and I/O-free `decide_*` helpers.
- **Typed signal, not a stringly sentinel.** Carry the error signal as a typed field on `RunnerResult` (e.g. `cli_error: bool`, or a small `CliErrorSignal` enum). Never as a magic prefix in `output`.
- No `.unwrap()` on stream fields; missing `is_error` = `false`.
- A new `RunnerResult` field needs no `Default` derive. Update all 10 literal construction sites (listed in §6) explicitly; the compile errors are the audit.

### Known Edge Cases

| # | Edge Case | Why It Matters | Expected Behavior |
|---|---|---|---|
| E1 | Agent summary says "One warn line per 429 … rate_limit_diagnostic_headers", exit 0 | The incident | Not RateLimit; normal completion path |
| E2 | Same summary, **grace-killed**: exit 143, `completion_killed=true`, and either `<completed>X</completed>` in the scanned text or a `X-completed` commit | Incident shape. Not-RateLimit is not done: exit 143 is still `Crash(RuntimeError)` (`detection.rs` step 4). Claude `output` is only `result.result` (`claude.rs` result arm). The grace arms on `assistant_buf` (`stream.rs` `arm_completion_grace`), and `conversation` is head-capped at 50_000 bytes so the tag can be absent from the transcript | Not RateLimit. The DB row for X is `done` because the ladder saw the tag or the sequential `-completed` commit. Assert the row, not only the classifier |
| E3 | Grace-killed, `output` non-empty or empty, `parse_completed_tasks(output)` empty, `<completed>` only in the grace buffer (missing from the head-capped `conversation`) | Pipeline scans `output` only today; a conversation-only fallback misses the tag that armed the grace | No CLI error. FR-004 scans a tail-retained copy of the buffer `arm_completion_grace` already scans. The task is `done` |
| E4 | CLI result `is_error:true`, `result:"You've hit your session limit · resets 4pm"`, exit 1 | Real limit | RateLimit (unchanged) |
| E5 | Assistant message with an `error` field (`rate_limit` type) and exit 1 | Real API 429 path (`extract_error_text`, claude.rs:204) | RateLimit; the error text is scanned |
| E6 | Agent prose "Bad Gateway from upstream 502", exit 0 | `is_transient_backend` false positive | Not TransientBackend |
| E7 | Agent prose "the prompt is too long for the old API", exit 0 | Existing test `detection.rs:1539` asserts PromptTooLong at exit 0 | Not PromptTooLong. **The test is rewritten** to assert the new contract, with a comment citing this PRD |
| E8 | Real "Prompt is too long" from the CLI, exit 0 but `is_error:true` | The CLI may exit 0 on overflow | PromptTooLong, via `is_error` |
| E9 | Real limit with exit 0 **and** `is_error:false` (hypothetical CLI drift) | Risk 1: a missed real limit → crash/empty path | Not detected as RateLimit. Mitigated by a warn-level `detection.text_signal_without_cli_error` log (FR-005) so drift is visible |
| E10 | RateLimit run in which the agent ALSO emitted `<completed>X</completed>` before the CLI error (sequential and wave) | Ordering bug, real-error variant. `record_completion` rewrites the live outcome to `Completed`, so a post-pipeline `react_to_outputs` that reads the live outcome skips the wait | X recorded `done` first (ladder, then sequential wrapper commit). The reaction reads the **pre-pipeline** `RateLimit` snapshot, waits, and does not reset X. The iteration is given back (`consumes_budget` false / wave `iteration_consumed: false`) |
| E11 | PromptTooLong overflow on a run with `<completed>X</completed>` (sequential and wave) | `handle_overflow` inserts context and model snapshots (`post_output.rs` before the status update) and resets while the row is still `in_progress` (`iteration.rs` before `orchestrator.rs:399`; `slot.rs` before the pipeline) | The **entire** `handle_overflow` call is skipped for the resolved claimed id when it is in `parse_completed_tasks` (no context rows, no rungs 1–5). X is recorded `done` by the ladder. Git-only and output-scan completions are out of scope (see §5) |
| E12 | Probe (`probe_rate_limit_lifted`, `recovery.rs` starts at the function; the `is_rate_limited` return is the scan site) prints prose containing "limit" with exit 0 | Probe uses plain `--print` and ignores exit status today | Probe = "lifted" when exit 0; text is scanned only when exit≠0 |
| E13 | Codex `turn.failed` event with rate-limit text, exit≠0 | That text is `[Error: …]` in the conversation, not in `derive_output` | RateLimit because the retained `StreamEvent::Error` string is scanned, not because the agent summary is scanned |
| E14 | Grok assistant text mentions 429, exit 0 | Grok has no error events | Not RateLimit |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **Contract owner**: `src/loop_engine/runner.rs` owns `RunnerResult`; `src/loop_engine/detection.rs` owns `analyze_output`.
- **Consumers**: Claude, Grok and Codex runner constructors; `iteration.rs` sequential classify; `slot.rs` wave classify; `recovery.rs` `probe_rate_limit_lifted` (scan return, not the function start); FR-003 snapshot ordering in `orchestrator.rs` and `wave_scheduler.rs`; FR-004 grace-buffer fallback.
- **Recommended task**: `CONTRACT-001` is design-only. It records `OutputSignals`, the `has_cli_error` formula, which text is scanned, the grace-buffer tail, the pre-pipeline outcome snapshot, the stop-exit mapping, and the whole-call overflow skip. Downstream implementation tasks depend on that record: signal capture, the classifier gate, sequential ordering, the wave snapshot, the grace-buffer scan, and the probe gate.

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
|---|---|---|
| stream-json `result` line → Accumulator | `serde_json::Value` (string keys) → typed `Accumulator` field | `val.get("is_error").and_then(serde_json::Value::as_bool).unwrap_or(false)` inside the `Some("result")` arm (`claude.rs` result arm). `tool_result.is_error` is a different field and must not be read |
| assistant `error` → Accumulator | `Value` → `StreamEvent::Error(String)` | In `accumulate`, on `StreamEvent::Error(e)` only: set `acc.cli_error = true` and store `e` for the gated scan. Do not set `cli_error` from tool results |
| Accumulator → RunnerResult | `drive_stream` returns at `stream.rs` (the function signature is earlier in the same function) `(output, Some(conversation), denials)` plus the new signal | Extend the returned tuple/struct with `cli_error`, the retained error text, and a tail-retained copy of the buffer `arm_completion_grace` scans. Set `RunnerResult { cli_error, .. }` at the Claude constructor in `runner.rs` |
| RunnerResult → analyze_output | struct fields | `analyze_output(&r.output, r.exit_code, signals, dir)` at `iteration.rs` and `slot.rs`. `signals` carries `cli_error`, `completion_killed`, `error_text`, and optional `task_id` / `run_id` for the warn event. When `cli_error` is set, the scan uses the error text, not `output` |
| RunnerResult → pipeline | `IterationResult` then `ProcessingParams` | `process_iteration_output` never sees `RunnerResult`. Copy `grace_buffer_tail` and `completion_killed` onto `IterationResult` in `run_iteration` and `run_slot_iteration`, then into both `ProcessingParams` literals |
| Grok/Codex → RunnerResult | exit status / `StreamEvent::Error` | Grok constructor: `cli_error: false` (the exit≠0 gate covers it; Grok emits no error events). Codex constructor: `cli_error: saw_error_event`, and the retained text is the `turn.failed` / `error` string |

### Modularity & Coupling Targets

- **Public surface**: +1 field on `RunnerResult`; `analyze_output` gains 2 params, or 1 small struct `OutputSignals { cli_error, completion_killed }`, which is preferred so there are no same-typed bool pairs. No new CLI flags, config keys or DB columns.
- **Ownership**: the gate decision lives in exactly one place, `analyze_output`. Runners report facts and never classify.
- **Coupling budget**: `detection.rs` must not import runner types beyond the signal struct.

---

## 2.7. Production Readiness

| Dimension | Requirement for THIS PRD |
|---|---|
| Change scope | May touch: `loop_engine/{detection,claude,stream,runner,iteration,slot,orchestrator,recovery,iteration_pipeline,wave_scheduler}.rs`, `reactions/{account,post_output}.rs`. Must NOT change: classifier pattern sets, `decide_account_rate_limit` wait math, the usage-gate/probe split (learning 5298), the DB schema, CLI flags, OperatorStopped/StopSpend exit codes (both stay 0) |
| Example to copy | Pipeline-then-reaction order: `follow wave_scheduler.rs` (`process_slot_result`, then `react_to_outputs`) and `orchestrator.rs` `react_to_transient` (after the ladder). The wave call must be changed to pass the pre-pipeline snapshot; do not copy its post-pipeline outcome read |
| Boundary validation | Entry: stream-json `result` line `is_error`, or `StreamEvent::Error`. Validator: `as_bool()`. Bad/missing `is_error` → `false`. `tool_result.is_error` is ignored |
| Failure semantics | No retries added. Idempotency: the completion ladder runs once per iteration (unchanged); the reaction's `reset_in_progress_tasks` leaves `done` rows untouched (B1 `status='in_progress'` guard). A RateLimit snapshot still gives the iteration back after the ladder rewrites the live outcome to `Completed` |
| Observability | New `tracing::warn!` event `detection.text_signal_without_cli_error`. Correlation id is `active_prefix`, the same prefix on `task-mgr-<prefix>.log` (`observability.rs`). Fields: `{task_id, classifier, exit_code, completion_killed}` plus `run_id` when the call site has it. **No output text.** Tracing has no field redactor, so a later field that includes output would leak repository content. Existing `Rate limit detected in output` line unchanged |
| Rollout & rollback | Revert the commit (no migration, no persisted format change). Consumers pick it up via `cargo install` |
| Proof | FR-002 → `test_agent_summary_about_429_handling_is_not_rate_limit` (E1) and the grace-killed classifier assertion (E2) fail before, pass after. E2 also asserts the DB row is `done`. FR-003 → E10 on sequential and wave: row `done` and the wait still runs from the snapshot. FR-004 → E3 test against the grace-buffer tail. All existing exit≠0 classifier tests stay green |

---

## 3. User Stories

### US-001: Loops over rate-limit-themed PRDs progress

**As an** operator running a loop on a PRD about HTTP 429 handling
**I want** the agent's summary to never be mistaken for a CLI rate limit
**So that** finished tasks stay done and the loop doesn't park for hours

**Acceptance Criteria:**
- [ ] E1, E2, E6, E7, E12 and E14 classify as no-CLI-error outcomes (E2's `done` row is US-002, not this classifier check)
- [ ] E4, E5 and E8 still classify as before
- [ ] E13 is RateLimit because the retained Codex error string is scanned

### US-002: Completion survives a genuine reaction

**As an** operator
**I want** a task the agent completed to be recorded done even if the same run hit a real limit or overflow
**So that** a real usage limit at the end of a task doesn't throw away the work

**Acceptance Criteria:**
- [ ] E10 and E11 record the completed task `done` on both the sequential and wave paths
- [ ] E10's RateLimit wait still runs from the pre-pipeline snapshot, and that iteration does not consume budget
- [ ] E11 skips `handle_overflow` entirely for the tagged task; overflow rungs still run for a task that is not in `parse_completed_tasks`

---

## 4. Functional Requirements

### FR-001: Carry a CLI-error signal from runners to detection

`CONTRACT-001` records the types. The implementation task that depends on it does the capture below.

- `cli_error` is true only for a stream-json `result` line with `is_error: true`, or for `StreamEvent::Error` (assistant `error`, Codex `error` / `turn.failed`). `tool_result.is_error` does not count. Nothing reads `result.is_error` today.
- Retain the error text for the gated scan: the `result` string when `is_error`, otherwise the `StreamEvent::Error` string. Codex `turn.failed` text lives in the conversation as `[Error: …]`, not in `derive_output`, so the retained string is what E13 scans.
- Grok: `cli_error = false` (no error events; exit≠0 covers it).
- Also return a tail-retained copy of the buffer `arm_completion_grace` scans (FR-004). `drive_stream`'s return is the place to add it; the function signature is earlier in the same function.
- Update all 10 `RunnerResult` literal sites (`runner.rs` production constructors and the three runner test literals; `claude.rs` four `ClaudeResult` literals — `ClaudeResult` is a type alias). `runner.rs` doc `ignore` example is not a compile site. Extend `test_parse_stream_json_result_subtypes`.
- **First step**: capture a real stream-json usage-limit `result` line and an assistant-`error` line as fixtures (run `claude -p --output-format stream-json` against an exhausted rung, or pull them from a saved transcript). If `is_error` turns out not to be set on limit lines, the gate relies on exit≠0 alone, which the logs show is 1.

**Validation:** parser unit tests per fixture: `is_error:true` → `cli_error`; missing → `false`; a `tool_result` with `is_error:true` and no result/assistant error → `cli_error: false`.

### FR-002: Gate the text classifiers on the signal

`has_cli_error = cli_error || (exit_code != 0 && !completion_killed)`. In `analyze_output`, steps 3, 3.5 and 3.6 (`is_rate_limited`, `is_prompt_too_long`, `is_transient_backend`) run only when `has_cli_error`. When `cli_error` is set, scan the retained error text, not the agent summary. Otherwise scan `output` as today. Completed/Blocked/Reorder precedence is unchanged. Step 4 (exit 143 → `Crash(RuntimeError)`) is unchanged.

**Validation:** E1, E2 (classifier half), E6, E7 and E14 regression tests (fail before). Rewrite `test_rate_limit_429_in_middle_of_line` (`detection.rs:1050`) and `test_detects_prompt_too_long_regardless_of_exit_code` (`detection.rs:1539`) to the new contract, with a comment citing this PRD. All other exit≠0 tests stay green. Production `analyze_output` callers are only `iteration.rs` and `slot.rs` (about 70 test calls, all in `detection.rs`).

### FR-003: Run state-resetting reactions after the completion ladder, from a pre-pipeline snapshot

On both paths, snapshot the outcome **before** `process_iteration_output` / `process_slot_result`. Run the ladder first so `done` rows exist. Pass the snapshot into `react_to_outputs`. Do not read the post-pipeline outcome: `record_completion` sets `Completed` for every successful tag, and `react_to_outputs_inner` returns immediately when no item is still `RateLimit`.

Sequential order, in `orchestrator.rs` (the reaction leaves `iteration.rs`):

1. Snapshot `result.outcome` (and keep `task_id` and `output`; the current OperatorStopped/StopSpend return at `iteration.rs` drops both and must not run before the ladder).
2. `process_iteration_output`.
3. `react_to_completions` when the ladder completed the claimed task, **before** the multi-hour wait, so the wrapper commit is durable if the process dies during the wait. The wave path still defers merge (ephemeral branches persist; do not merge inside the reaction).
4. `react_to_outputs` on the snapshot.
5. Budget: `consumes_budget` and the wave's `iteration_consumed` follow the snapshot. A `RateLimit` snapshot gives the iteration back even when the live outcome is now `Completed`.

Stop mapping (do not restore exit 130 or 1; learning 5373's old "130 vs 1" note does not apply):

- OperatorStopped and StopSpend are applied only in the orchestrator, after the ladder. Both stay `IterationOutcome::Empty` + `should_stop` for the exit match.
- OperatorStopped: `operator_stopped = true` → exit 0, reason `"stop signal"` (`orchestrator.rs` Empty + `operator_stopped` arm).
- StopSpend: set `ctx.account_quota_stopped` (so `batch --chain` aborts) and `operator_stopped = false` → exit 0, reason `"quota soft-stop"`.
- Keep `task_id` and `output` through the ladder. After the ladder they may be cleared on the stop return; the `done` row is already written.

PromptTooLong, both paths: skip the **entire** `handle_overflow` call when the resolved claimed id (`resolve_tag_id_to_claimed`) is in `parse_completed_tasks(output)`. A skip only around `recover_in_progress` still inserts `overflow_recovered` and model snapshots. Git-only and output-scan completions are not covered by this skip (non-goal).

**Validation:** E10 sequential and wave: X is `done`, the wait still runs, and the iteration is not consumed. E11 sequential and wave: no overflow context row for X, and X is `done`. An existing OperatorStopped test still expects exit 0 `"stop signal"`. A StopSpend test still expects `account_quota_stopped` and exit 0 `"quota soft-stop"`.

### FR-004: Completion scan falls back to the grace buffer

When `completion_killed` is set and `parse_completed_tasks(output)` is empty, `process_iteration_output` scans a tail-retained copy of the buffer `arm_completion_grace` already scans (`stream.rs`). That copy is returned from `drive_stream` onto `RunnerResult`. `process_iteration_output` does not see `RunnerResult`. `run_iteration` and `run_slot_iteration` copy `grace_buffer_tail` and `completion_killed` onto `IterationResult`, and both `ProcessingParams` literals (sequential `orchestrator.rs`, wave `slot.rs`) pass them in. Early returns use an empty tail and `completion_killed: false`. This applies whether `output` is empty or is a non-empty summary that lacks the tag. Do not scan `conversation` alone: it is head-capped at 50_000 bytes (`append_capped`), so the tag that armed the grace can already have fallen off.

Wave sets `skip_git_completion_detection: true`, so a grace-kill whose tag is only in that buffer cannot be saved by a `-completed` commit. The buffer scan is the wave path's only recovery for E3.

**Validation:** E3 test: grace-killed, output lacks `<completed>`, the tag is only in the grace-buffer tail (and absent from a head-capped conversation) → the task is `done`.

### FR-005: Observability for gated-out text signals

When `!has_cli_error` and a classifier *would* have matched, emit `warn!(target: "task_mgr::detection", event = "text_signal_without_cli_error", classifier, task_id, exit_code, completion_killed)` without the output text. Include `run_id` when the call site has it. The correlation id is `active_prefix` on `task-mgr-<prefix>.log`. Tracing has no redactor; the event must keep excluding output text. This makes CLI drift (E9) visible.

**Validation:** a log-capture test for E1 asserts exactly one event with `classifier="rate_limit"` and no output-text field.

### FR-006: Probe gating

`probe_rate_limit_lifted` (function start in `recovery.rs`; the `!is_rate_limited(...)` return is the scan) returns lifted when the probe exits 0. Text is scanned only when exit≠0. The probe calls `is_rate_limited` directly; it does not call `analyze_output`.

**Validation:** E12 test using a fake `CLAUDE_BINARY` script.

### FR-007: Record the limit shape per provider

The three CLIs do not share a stream. Fixture tests lock how task-mgr parses a line it already has. They do not show which stream a live limit uses. Do not retarget `is_rate_limited` until a captured line shows which channel held the sentence.

Append one JSON object to `.task-mgr/logs/limit-shape-<prefix>.jsonl` when any of these is true: `exit_code != 0`, `cli_error` is set, or any captured channel matches `is_rate_limited`, `is_prompt_too_long`, or `is_transient_backend`. A clean exit 0 with no match writes nothing.

Each line carries `provider`, `exit_code`, `cli_error`, `completion_killed`, and only that provider's channels. Each text field is the trailing 4096 bytes. Do not put the text on a tracing event.

| Provider | Fields | Where that text is today |
| --- | --- | --- |
| Claude | `result_is_error`, `result_text`, `assistant_error`, `stderr` | `result.result` is the stdout result line. An assistant `error` is `StreamEvent::Error` and is not part of `output`. Stderr is inherited and discarded. Pipe it, tee it to the terminal, and store the tail. |
| Grok | `stderr`, `output_tail` | Stderr is already buffered and scanned only for auth and transient 5xx, before `analyze_output`. `output` is the assistant `text` chunks. |
| Codex | `error_text`, `stderr`, `output_tail` | `error` / `turn.failed` is the `[Error: …]` string, not `derive_output`. Stderr is buffered for the transient check and then dropped. |

Classification stays as FR-002 left it. A Grok or Codex stderr sentence that matches `is_rate_limited` is recorded and is still not classified as `RateLimit`. A 502 still returns `TransientBackend`.

**Validation:** one jsonl line per provider contains only that provider's fields. A `CLAUDE_BINARY` stub prints a session-limit sentence on stderr and a different sentence in `result.result`; the line stores each sentence in its own field, and the outcome still comes from FR-002's scan string, not from stderr.

---

## 5. Non-Goals (Out of Scope)

- **An operator kill-switch for the post-output usage reaction.** Reason: it treats the symptom and adds a permanent knob. Learning 5298 records that collapsing pre/post usage I/O onto one flag broke recovery.
- **Tightening the pattern sets** (`hit your`…`limit`, `reached your`…`limit` and others). Reason: once gated they scan only CLI error text, and the operator chose to keep recall.
- **Treating `<completed>` as `IterationOutcome::Completed` in `analyze_output`.** Reason: the pipeline owns completion. FR-003 fixes the ordering instead.
- **Changing Grok/Codex stderr transient classification.** Reason: it is already gated on exit≠0.
- **Retargeting the rate-limit scan onto stderr or another channel.** Reason: FR-007 stores the shape. Change the scan only after a real `limit-shape-<prefix>.jsonl` line shows which field held the sentence.
- **Protecting git-only or output-scan completions from `handle_overflow`.** Reason: the primary goal promises `<completed>` tags. A tag-skip of the whole call covers that. Moving the DB reset to after the ladder so it no-ops on `completed_task_ids` would also cover git-only overflow, and is not required for this bug.

## 5.5. Low-Value / High-Effort Areas

| Area | Why low value | Effort | Action |
|---|---|---|---|
| Per-rung structured error parsing (mapping `error.type` → rung) | The existing text patterns already cover the rungs once gated | Med | Cut |
| Scrubbing the agent prompt so agents avoid the words "429"/"limit" | Fragile, untestable, and treats the symptom | Low | Cut |

---

## 6. Technical Considerations

### Affected Components

- `claude.rs` result arm (`is_error` only; ignore `tool_result.is_error`), `stream.rs` Accumulator / `StreamEvent::Error` / `drive_stream` return (signature is earlier in the function): capture `cli_error`, error text, and the grace-buffer tail
- `runner.rs` `RunnerResult` plus the three production constructors (`runner.rs` Claude / Grok / Codex) and 7 test literals (`runner.rs` three, `claude.rs` four). The `ignore` doc example is not a compile site
- `detection.rs:43` `analyze_output`: the gate; FR-005 event
- `iteration.rs` classify and the RateLimit / PromptTooLong blocks that move; `orchestrator.rs` after `process_iteration_output`: snapshot, ladder, wrapper commit, then reaction
- `slot.rs` classify and overflow skip; `wave_scheduler.rs` snapshot into `react_to_outputs`
- `iteration_pipeline.rs` completion ladder: grace-buffer fallback when `completion_killed` and `output` has no tag
- `recovery.rs` `probe_rate_limit_lifted`: exit-status gate at the `is_rate_limited` return

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| A. Anchor the 429 clause to line-leading CLI shapes (`API Error: 429`) | Tiny diff | Fixes one clause only; prose like "Error: 429 …" at line start still trips; siblings stay unfixed | Rejected |
| B. Scan only the last N lines / short outputs | Small | Agent summaries end in exactly the prose that trips it; arbitrary threshold | Rejected |
| C. **Gate on a CLI-error provenance signal** (`is_error`, assistant `error`, exit≠0 not caused by a grace-kill) | Closes the whole class for all 3 classifiers; matches the project's "carry provenance in the type" pattern | Touches 10 struct literals; depends on the CLI exposing `is_error` or exiting non-zero | **Preferred** |

**Selected**: C + the FR-003 ordering fix. **Phase 2 Foundation Check**: C costs about 1 day more than A, but it removes a bug class that otherwise grows by one review cycle per new pattern (each new CLI phrasing widens the prose surface).

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| A real limit arrives with exit 0 and `is_error:false` → not detected → crash/empty path burns iterations | Med | Low (logs show exit 1) | FR-001 captures real fixtures first; FR-005 warn event makes drift visible |
| Moving the sequential reaction changes stop/spend exit behavior | Med | Med | Apply OperatorStopped and StopSpend only after the ladder. Both stay exit 0 (`"stop signal"` vs `"quota soft-stop"`). StopSpend still sets `account_quota_stopped`. Do not restore 130 or 1. Copy the existing exit-0 tests to the new call site |
| The grace-kill exception hides a real CLI error that coincides with the kill | Low | Low | `cli_error` (from `is_error`/assistant error) still wins over `completion_killed` |

### Security Considerations

- The FR-005 event excludes output text. Tracing has no field redactor, and the text may contain repository content or secrets. The correlation id is `active_prefix` on `task-mgr-<prefix>.log`.

### Public Contracts

#### Modified Interfaces

| Module | Current | Proposed | Breaking? | Migration |
|---|---|---|---|---|
| `detection::analyze_output` | `(output:&str, exit_code:i32, _dir:&Path)` at `detection.rs:43` | `(output:&str, exit_code:i32, signals:OutputSignals, _dir:&Path)` | Crate-internal | Update 2 prod callers (`iteration.rs`, `slot.rs`) + the `detection.rs` test calls (about 70). A test helper preserves exit≠0 semantics. The probe does not call `analyze_output` |
| `runner::RunnerResult` | no error field | `+ cli_error: bool` + retained error text + grace-buffer tail | Crate-internal | 10 literal sites |

### Data Flow Contracts

See §2.6.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|---|---|---|---|
| `iteration.rs` classify | sequential classify | NEEDS UPDATE | pass signals |
| `slot.rs` classify | wave classify | NEEDS UPDATE | pass signals |
| `recovery.rs` `probe_rate_limit_lifted` | probe lifted? | CHANGES (FR-006) | exit-status gate; function start, scan at the `is_rate_limited` return |
| `runner.rs` Grok/Codex stderr transient | already exit≠0 | OK | none |
| `.task-mgr/logs/limit-shape-<prefix>.jsonl` | which channel held the sentence | NEW (FR-007) | record only; the scan string is unchanged |
| `detection.rs:1050`, `:1539` | tests encoding prose-match | BREAKS by design | rewrite to new contract |
| `wave_scheduler.rs` post-slot `react_to_outputs` | reads post-pipeline outcome | NEEDS UPDATE | pass the pre-pipeline snapshot |
| `reaction_parity.rs`, `overflow_per_slot*.rs` | build outcomes directly | OK | none |

### Semantic Distinctions

| Code Path | Context | Current | Required |
|---|---|---|---|
| exit 143 + `completion_killed` | task-mgr's own grace kill after `<completed>` | scanned as an error; step 4 is still `Crash(RuntimeError)` | not a CLI error; `done` only if the ladder sees the tag or a sequential `-completed` commit |
| exit 143 without `completion_killed` | external SIGTERM | signal path (`iteration.rs` matches 130 or 143 only; exit 137 does not set the stop flag) | unchanged |
| Wave RateLimit reaction | after the pipeline, but the live outcome is already `Completed` when a tag was recorded | wait skipped | keep the order; feed the pre-pipeline snapshot |
| Sequential RateLimit reaction | before the pipeline | resets the completed task | move after the ladder and the wrapper commit; feed the snapshot |
| `consumes_budget` / `iteration_consumed` | follows the live outcome | a flipped `Completed` would consume the iteration | follow the pre-pipeline snapshot |

### Inversion Checklist

- [x] All callers identified (2 prod `analyze_output`, the probe's direct `is_rate_limited`, Grok/Codex stderr)
- [x] Routing that depends on the outcome reviewed (account reaction snapshot, overflow whole-call skip, transient, budget give-back)
- [x] Tests encoding current behavior identified (`detection.rs:1050`, `:1539`, `test_exit_code_143_sigterm`)
- [x] Semantic contexts documented (grace-kill vs external signal; wave vs sequential order; live outcome vs snapshot)

### Documentation

| Doc | Action | Description |
|---|---|---|
| `src/loop_engine/CLAUDE.md` | Update | "Classifiers are gated on CLI-error provenance; agent prose never classifies" and the reaction-after-pipeline ordering invariant |
| `docs/ARCHITECTURE.md` | Update | Iteration outcome flow: signal capture → gate → pipeline → reactions |

---

## 7. Open Questions

- [ ] Does the current Claude CLI set `is_error:true` on usage-limit `result` lines? The next live limit answers this from `limit-shape-<prefix>.jsonl` (`result_is_error` plus which field holds the sentence). The design holds either way (exit 1 observed). Do not exhaust an account to find out.
- [x] After FR-003, OperatorStopped and StopSpend keep `task_id` and `output` through the ladder, then return `Empty` + `should_stop` for the exit match. They do not return `task_id: None` before the ladder. Exit codes stay 0. StopSpend sets `account_quota_stopped`. The sequential wrapper commit runs before the wait; the wave merge stays deferred.

---

## Appendix

### Related

- Learnings: 5364 (widened patterns), 5298 (don't collapse usage flags), 5373 (stop exit codes), 5391 (spend banners park the wait)
- Incident: mw_support `tasks/operator-loop-17a0ade1.log` lines 163-166
