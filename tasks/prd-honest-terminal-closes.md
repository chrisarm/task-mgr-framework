# PRD: Keep Honest Terminal Closes from Being Reset to `todo`

**Type**: Bug Fix
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-08-15
**Status**: Draft
**Plan**: session plan “Keep honest terminal closes from being reset to `todo`” (architect-reviewed)
**Related**: `src/lifecycle/CLAUDE.md` Recovery-verb families; learnings **#4358**, **#4810**, **#5151**, **#2304**, **#3727**, **#3093**

---

## 1. Overview

### Problem Statement

A slot agent honestly classified a VERIFY gate:

```
<task-status>805856aa-VERIFY-PACER-OBS:blocked</task-status>
<promise>COMPLETE</promise>
```

The shared pipeline applied that tag (`:blocked` → `TaskStatusChange::Failed` → `FailStatus::Blocked`), so the DB row was `blocked`. Loop-exit cleanup then printed:

```
Reset uncompleted slot task 805856aa-VERIFY-PACER-OBS to todo
```

That is orchestrator step 17.6. The honest block was undone. The next wave re-claimed the same VERIFY gate as `todo`.

This is not a one-off. Any honest `:blocked` / `:skipped` / `:irrelevant` (and any overflow-rung-5 or `auto_block_after_failures` close) that is still listed in `pending_slot_tasks` or `last_claimed_task` is force-written back to `todo` on loop exit. Parallel mode is the largest blast radius: `pending_slot_tasks` accumulates **every un-done claim for the whole run**.

### Background

`TaskStatus::is_terminal()` already defines the closed set: **`done | blocked | skipped | irrelevant`**. There is no `failed` task-row status (`failed` lives only on `run_tasks`). Selection, `count_remaining_active_tasks`, crash-map prune, and `try_claim` already use that set.

Two loop-exit trackers do **not**. They clear only on `:done` (`completed_task_ids` / `slot_marked_done`) and then feed `reset_task_to_todo` → `TaskLifecycle::resurrect_for_iteration`, which is **status-agnostic by documented Recovery contract** (learning **#4358**). A correctly-applied terminal row is force-written to `todo`.

Three production callers share that unguarded helper today:

| Path | Site | Intent |
|---|---|---|
| Sequential last-claimed | orchestrator 17.5 | orphan reclaim |
| Wave pending | orchestrator 17.6 | orphan reclaim |
| Merge-fail FEAT-002 | `apply_merge_fail_reset_and_halt_check` | work did not land on slot 0 |

Tag-history (`closed_task_ids` from the pipeline) is **not** a sufficient authority: overflow rung 5 and `auto_block_after_failures` land on `blocked` after / outside `apply_status_updates`; multi-tag last-write-wins (`:blocked` then `:unblock`) makes “any Failed in this batch” wrong.

An architect + explore review of the first-cut plan found a second product fork the original write-up missed: **merge-fail of a premature `:done` must still reopen**. Guarding the shared helper with `in_progress`-only would leave completed-but-unmerged work stranded on the ephemeral.

---

## 2. Goals

### Primary Goals

- [ ] An honest terminal close (`blocked` / `skipped` / `irrelevant` / `done`) survives loop-exit cleanup (17.5 / 17.6). True orphans (`in_progress`, no status tag) still reset to `todo`.
- [ ] Overflow-rung-5 and `auto_block_after_failures` closes survive loop-exit (they are not status tags; tag-history would miss them).
- [ ] Merge-fail of a `:done` slot still reopens to `todo` (work never landed on slot 0). Merge-fail of an honest `blocked` / `skipped` / `irrelevant` does **not** reopen.
- [ ] The two predicates live as Category C Recovery verbs in `TaskLifecycle`. Loop-engine wrappers do not invent `SELECT` + `from_str` policy.
- [ ] `resurrect_for_iteration` SQL and its `blocked → todo` recovery tests stay unchanged (learning **#4358**).
- [ ] `handle_task_failure_with_runner` does not increment `consecutive_failures` when the claimed row is already terminal — skip is **inside** the function, not copied at two call sites.

### Success Metrics

- DB-state unit tests: `recover_in_progress` no-ops on every non-`in_progress` status; `reopen_after_merge_fail` reopens `in_progress` and `done` only.
- Wave claimed `:blocked` (and table-driven `:failed` / `:skipped` / `:irrelevant`) remains that status after the 17.6 helper.
- Overflow rung 5 then helper: row stays `blocked`.
- Auto-block then helper: row stays `blocked`.
- True orphan: still `in_progress`, no tag → helper **does** reset and emits the existing “Reset … to todo” line.
- Merge-fail × `blocked`: status unchanged, pending drained, halt counter increments.
- Merge-fail × `done`: status becomes `todo`.
- `handle_task_failure_with_runner` on an already-terminal row: `consecutive_failures` unchanged.
- `cargo test` scoped to `lifecycle` recovery tests + `loop_engine::wave_scheduler` + any extended `tests/iteration_pipeline.rs` is green.
- `loop-engine-parity-auditor` on `orchestrator.rs`, `wave_scheduler.rs`, `slot.rs`, `recovery.rs` reports no new sequential/wave divergence.

---

## 2.5. Quality Dimensions

### Correctness Requirements

- **SSoT is current DB `TaskStatus`**, not pipeline tag-history. Overflow rung 5 and `auto_block_after_failures` land after / outside `apply_status_updates`.
- **Orphan reclaim may only perform `in_progress → todo`.** `todo` and every terminal are no-ops. Use `== InProgress`, not `!is_terminal()` (`todo` is also a no-op).
- **Merge-fail may perform `in_progress → todo` and `done → todo`.** Honest `blocked` / `skipped` / `irrelevant` stay closed. Halt counter and pending drain still run.
- **Atomic conditional UPDATE**, not SELECT-then-write. Sibling Recovery verbs already use `WHERE status = 'in_progress'` (learning **#4810**). `try_claim` FR-005 forbids hiding the predicate. A concurrent `task-mgr fail` / decay / other process is a real writer.
- **Do not change `resurrect_for_iteration` SQL.** Learning **#4358** and `src/lifecycle/tests/recovery_tests.rs` pin `blocked → todo` for that verb.
- **Do not count a terminal close as `tasks_completed`.** Wrapper-commit / `IterationOutcome::Completed` stay **done-only**. A human-observe VERIFY block must not look like PRD progress or fire auto-review.
- **`<promise>BLOCKED</promise>` without a status tag is not a DB close.** Leftover `in_progress` must still orphan-reset.
- **`handle_task_failure` skip uses `is_terminal()`, not `== InProgress`.** Overflow rungs 1–3 have already flipped the row to `todo` and still need the consecutive-failure counter.
- **`reconcile_ambiguous_exit` / `prd_complete` stay before 17.5** (learning **#5151**). After this fix a last-iter honest block stays `blocked`, so the pre-17.6 snapshot stays true; do not redesign stuck-vs-complete messaging.

### Performance Requirements

- Best effort. No new SELECT-before-UPDATE on the orphan or merge-fail write path. One conditional UPDATE per id, matching `auto_block_after_failures` / `resurrect_with_model_override`.
- `read_status` is allowed **only** inside `handle_task_failure_with_runner` (a read that decides whether to skip a write, not a SELECT that then calls an unguarded UPDATE).
- No new transaction boundaries.

### Style Requirements

- Every `tasks.status` write goes through a `TaskLifecycle` verb (`src/lifecycle/CLAUDE.md`).
- Reuse `lifecycle::read_status` — do not add a third `SELECT status` + `from_str` in the loop engine.
- Sequential and wave share one write (the verb) and one failure-skip (inside `handle_task_failure_with_runner`). Dual-site copies are the parity class this repo has already burned on.
- Log the existing “Reset {kind} {id} to todo” line **only** when the verb returns `Ok(true)` / `Ok(1)`.
- Failures of the reset itself never propagate (FEAT-002 failure-mode AC).
- Comments / rustdoc must stop saying “closed = `<completed>`”.

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Claimed `:blocked` / `:failed` / `:skipped` / `:irrelevant` then loop exit | The reported bug | Stay terminal |
| Overflow rung 5 `Blocked` then 17.5/17.6 | Not a status tag; tag-history would miss this | Stay `blocked` |
| `auto_block_after_failures` then exit | Post-pipeline; tag-history would miss this | Stay `blocked` |
| Drain-classifier exit 1 after sibling `:done` + this `:blocked` | `prd_complete` / remaining-active count | B stays `blocked`; `count_remaining_active_tasks == 0` remains true |
| `:blocked` + `ID-completed` commit, merge OK | Reconcile already refuses (`force=false`) | Reset must not reopen |
| Peer `:blocked` on another slot’s claimed id | Peer was in pending | Helper no-ops on `blocked` |
| Merge-fail of already `blocked` / `skipped` / `irrelevant` | Honest classification, work classification is independent of merge | Status unchanged; halt counter increments; pending drained |
| Merge-fail of already `done` | Work never landed on slot 0; original shared-guard plan would strand it | Reopen to `todo` |
| Multi-tag `:blocked` then `:unblock` | Last-write-wins | Final DB `todo`; orphan helper no-ops (not `in_progress`) |
| Multi-tag `:blocked` then `:done` | Matrix rejects `Blocked → Done` | Final `blocked`; helper no-ops |
| Rate-limit / transient already reset to `todo` | `recover_in_progress_for_prefix` already ran | Helper no-ops |
| True orphan (crash, no tag, still `in_progress`) | The reason 17.5/17.6 exist | Helper **does** reset |
| `<promise>BLOCKED</promise>` only, no status tag | Promise is not a DB close | Still `in_progress`; helper **does** reset |
| Claim-fail (`claim_succeeded=false`) | Never claimed | Never pushed; never reset |
| Unblock / Reset tags | Not terminals | `todo` → helper no-op |
| Unknown / unparsable `status` text | SELECT+`from_str` would have to invent a fallback | Conditional UPDATE: 0 rows, no write |
| Missing row | Same | `Ok(false)`, no log line |

---

## 2.6. Boundary Contracts & Modularity Targets

### New or Changed Public Boundaries

- **Contract owner**: `src/lifecycle/recovery.rs` (`TaskLifecycle`).
- **Consumers**:
  - **CONTRACT-001** defines the two verbs and the predicate table.
  - **US-002** (17.5 / 17.6 orphan reclaim) calls `recover_in_progress`.
  - **US-003** (merge-fail) calls `reopen_after_merge_fail`.
  - **US-004** (overflow rungs 1–3) calls `recover_in_progress`.
  - **US-005** (`handle_task_failure` skip) uses `lifecycle::read_status` + `TaskStatus::is_terminal()` inside the function.
- **Recommended predecessor**: **`CONTRACT-001`** — the Recovery predicate split. Two+ stories implement against it.

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| Orphan / merge-fail write | `&mut Connection` → `TaskLifecycle` → `tasks.status` (`TEXT`, CHECK of `TaskStatus`) | `TaskLifecycle::new(conn).recover_in_progress(task_id)?` / `.reopen_after_merge_fail(task_id)?` — **no** pre-read of status |
| Failure-skip read | `&Connection` → `Option<TaskStatus>` | `crate::lifecycle::read_status(conn, task_id).is_some_and(\|s\| s.is_terminal())` |
| Loop-exit trackers (unchanged authority) | `IterationContext.pending_slot_tasks: Vec<String>` (all-run claimed ids); `last_claimed_task: Option<String>` (last sequential claim) | Still drained only on `:done` / explicit merge-fail retain. **Not** reset authority. |
| Done-only completion list (must stay done-only) | `ProcessingOutcome.completed_task_ids: Vec<String>` | Membership means `:done` / `<completed>` / git-reconcile done. Do **not** stuff terminals in. |
| Consecutive-failure counter | `tasks.consecutive_failures: i32` | `increment_consecutive_failures` has **no** status predicate today. After US-005 the increment is skipped when `read_status` is terminal. Do not skip on `todo`. |

### Modularity & Coupling Targets

- **Target public surface**: two new `TaskLifecycle` methods (`recover_in_progress`, `reopen_after_merge_fail`). No new CLI, no new DB columns, no new config keys.
- **Ownership**: Recovery predicates live in `src/lifecycle/`. Loop-engine wrappers (`reset_orphan_to_todo` / merge-fail arm) are logging + halt-counter only.
- **Coupling budget**: do not put status-write policy in `wave_scheduler.rs` or `orchestrator.rs`. Do not add `ProcessingOutcome.closed_task_ids`. Do not unify `last_claimed_task` + `pending_slot_tasks`.
- **Cohesion**: `recover_in_progress` sits next to `recover_in_progress_for_prefix` (bulk twin) and `resurrect_with_model_override` (per-id + model twin). `reopen_after_merge_fail` sits next to them as the third Recovery predicate, not as a flag on the first.

### When to Emit a CONTRACT-xxx Task

**Emit `CONTRACT-001`.** The two verbs + the predicate table are implemented against by orphan reclaim, merge-fail, and overflow rungs 1–3. Landing the verbs and their unit tests before any call-site swap is the synergistic prerequisite the plan review identified.

---

## 3. User Stories

### CONTRACT-001: Recovery predicate verbs

**As a** lifecycle maintainer
**I want** a per-id `in_progress → todo` verb and a merge-fail `in_progress|done → todo` verb
**So that** every reclaim / reopen site shares one atomic predicate and `resurrect_for_iteration` stays the documented force-any-status escape hatch

**Acceptance Criteria:**

- [ ] `TaskLifecycle::recover_in_progress(task_id) -> Result<bool>` with `WHERE id = ? AND status = 'in_progress'`, clears `started_at`.
- [ ] `TaskLifecycle::reopen_after_merge_fail(task_id) -> Result<bool>` with `WHERE id = ? AND status IN ('in_progress', 'done')`, clears `started_at`.
- [ ] Table-driven `recovery_tests`: `recover_in_progress` returns `false` and leaves status unchanged for `blocked` / `skipped` / `irrelevant` / `done` / `todo` / missing; `true` + `todo` for `in_progress`.
- [ ] Table-driven `recovery_tests`: `reopen_after_merge_fail` returns `true` for `in_progress` and `done`; `false` for `blocked` / `skipped` / `irrelevant` / `todo` / missing.
- [ ] `resurrect_for_iteration` SQL and its `blocked → todo` test are **unchanged**.
- [ ] `src/lifecycle/CLAUDE.md` Recovery-family note and FR-006 table list both new verbs. The stale “six verbs” / “three Recovery verbs” wording is updated.
- [ ] No loop-engine call-site swap in this story (verbs + tests + docs only).

---

### US-002: Loop-exit orphan reclaim uses `recover_in_progress`

**As a** loop operator
**I want** 17.5 / 17.6 to reclaim only stranded `in_progress` rows
**So that** an honest VERIFY `:blocked` is not undone at process exit

**Acceptance Criteria:**

- [ ] Steps 17.5 and 17.6 call `recover_in_progress` (via a wrapper that logs only on `Ok(true)`). They do **not** call `resurrect_for_iteration`.
- [ ] `reset_task_to_todo` is no longer the policy SSoT. Split or replace so merge-fail cannot silently pick up the orphan predicate.
- [ ] Wave claimed `:blocked` remains `blocked` after the 17.6 helper. Table-drive `:failed` / `:skipped` / `:irrelevant`.
- [ ] True orphan (`in_progress`, no tag) still resets and emits `Reset uncompleted [slot] task {id} to todo`.
- [ ] Overflow rung 5 then helper: `blocked` survives (fails if anyone “simplifies” to tag-history).
- [ ] Auto-block then helper: `blocked` survives.
- [ ] Sequential last-iter fixture (should-have if cheap): max-iter 1, output only `:blocked` → DB `blocked` after 17.5.
- [ ] Rustdoc on the orphan wrapper and orchestrator 17.5 / 17.6 comments no longer say “closed = `<completed>`”.

---

### US-003: Merge-fail uses `reopen_after_merge_fail`

**As a** wave operator
**I want** a failed slot merge to reopen work that never landed, without undoing an honest terminal
**So that** `:done` + merge-conflict is retried and `:blocked` + merge-conflict stays classified

**Acceptance Criteria:**

- [ ] `apply_merge_fail_reset_and_halt_check` calls `reopen_after_merge_fail`, not `resurrect_for_iteration` / not `recover_in_progress`.
- [ ] Merge-fail × `in_progress`: status becomes `todo` (existing FEAT-002 tests keep passing).
- [ ] Merge-fail × `done`: status becomes `todo` (new test — pins the product fork).
- [ ] Merge-fail × `blocked` / `skipped` / `irrelevant`: status unchanged; pending still drained; halt counter still increments.
- [ ] Reset failure is logged and never fatal (existing FEAT-002 AC).

---

### US-004: Overflow rungs 1–3 use `recover_in_progress`

**As a** recovery maintainer
**I want** retry-in-place overflow rungs on the same guarded verb as orphan reclaim
**So that** `resurrect_for_iteration` has no accidental orphan-reclaim callers left

**Acceptance Criteria:**

- [ ] `reactions/post_output.rs` rungs 1–3 call `recover_in_progress` instead of `resurrect_for_iteration`.
- [ ] Rung 4 stays on `resurrect_with_model_override`. Rung 5 stays on `auto_block_after_failures`.
- [ ] Existing overflow tests stay green (rungs 1–3 only fire on a claimed `in_progress` row; overflow runs before the pipeline).
- [ ] `resurrect_for_iteration` itself is not deleted and its unguarded contract tests remain.

---

### US-005: Skip `handle_task_failure` when the row is already terminal

**As a** loop operator
**I want** an honest `:blocked` (or overflow/auto-block) to stop the retry ladder
**So that** `consecutive_failures` is not dirtied on a classified row

**Acceptance Criteria:**

- [ ] Skip lives at the top of `handle_task_failure_with_runner` via `read_status` + `is_terminal()`. Sequential and wave call sites are **not** duplicated.
- [ ] CodexAuthFailure / GrokAuthFailure call-site exclusions are untouched.
- [ ] `todo` is **not** skipped (overflow 1–3 still need the counter).
- [ ] One unit/integration test: already-terminal row → `consecutive_failures` unchanged.
- [ ] Do **not** add `IterationOutcome::Blocked` to both exclusion lists in this PRD.
- [ ] Do **not** fix the adjacent wave-missing-`TransientBackend` exclusion in this PRD.
- [ ] After this story, run `loop-engine-parity-auditor` on `orchestrator.rs`, `wave_scheduler.rs`, `slot.rs`, `recovery.rs`.

---

### US-006: Hygiene only — comments match the new policy

**As a** future auditor
**I want** rustdoc to describe orphan reclaim as “still `in_progress`”, not “not `<completed>`”
**So that** the next lifecycle audit does not re-teach the bug

**Acceptance Criteria:**

- [ ] `IterationContext::pending_slot_tasks` rustdoc.
- [ ] Orchestrator steps 17.5 / 17.6 comments.
- [ ] Orphan / merge-fail wrapper rustdoc names the verb and the predicate.
- [ ] No tracker-drain call-site churn. No `closed_task_ids`.

---

## 4. Functional Requirements

### FR-001: Orphan reclaim is `in_progress → todo` only

Loop-exit cleanup (17.5, 17.6) may flip a row to `todo` only when the current DB status is `in_progress`. Missing row, `todo`, and every terminal are silent no-ops (no “Reset … to todo” line).

**Validation:** CONTRACT-001 unit table + US-002 wave/orphan/overflow-5/auto-block tests.

### FR-002: Merge-fail reopens `in_progress` and `done` only

A failed slot merge must not leave work pinned `in_progress`, and must reopen a premature `:done` so the next wave can retry work that never reached slot 0. Honest `blocked` / `skipped` / `irrelevant` stay closed. Halt accounting is independent of the status write.

**Validation:** US-003 tests, including the new merge-fail × `done` pin.

### FR-003: Recovery predicates live in `TaskLifecycle`

No `SELECT status` + `TaskStatus::from_str` + maybe-`resurrect_for_iteration` in `wave_scheduler` / `orchestrator`. Writes are one conditional UPDATE per verb.

**Validation:** code review of call sites; `lifecycle/CLAUDE.md` FR-006 row added; no new raw `UPDATE tasks SET status` outside `src/lifecycle/`.

### FR-004: `resurrect_for_iteration` contract is frozen

The unguarded `* → todo` verb stays. Overflow rungs 1–3 migrate **off** it (FR-005). Do not add `WHERE status = 'in_progress'` to it.

**Validation:** existing `recovery_tests::resurrect_for_iteration_flips_listed_ids_to_todo` still asserts `FEAT-2` (`blocked`) → `todo`.

### FR-005: Overflow retry-in-place is `in_progress`-only

Rungs 1–3 call `recover_in_progress`. Matches the policy table and rung 4’s existing guard.

**Validation:** US-004; existing overflow suites.

### FR-006: Terminal rows do not enter the consecutive-failure ladder

`handle_task_failure_with_runner` returns `Ok(())` before increment when `read_status` is terminal. One site, both paths.

**Validation:** US-005 test + parity auditor.

### FR-007: Done-only surfaces stay done-only

`completed_task_ids`, wrapper-commit attribution, `tasks_completed`, `IterationOutcome::Completed`, and git-reconcile done detection do not grow to include `blocked` / `skipped` / `irrelevant`.

**Validation:** no new field on `ProcessingOutcome`; existing pipeline parity tests on `completed_task_ids` set equality keep passing.

---

## 5. Non-Goals (Out of Scope)

- **`TaskStatusChange::Blocked`.** `:failed` / `:fail` / `:blocked` → `Failed` → `FailStatus::Blocked` is the intended CLI/lifecycle split.
- **Changing `resurrect_for_iteration` SQL.** Learning #4358.
- **Decay / doctor / CLI `reset` / `unblock` / `unskip`.** Operator or age policy.
- **Reconcile `force=true`.** A `*-completed` commit must not override an honest block.
- **Treating `<promise>BLOCKED</promise>` as a DB close.**
- **`prd_complete` / `classify_drained_queue` / exit-code redesign.** Stuck-vs-complete messaging is a separate product question.
- **`ProcessingOutcome.closed_task_ids` / tracker unification / drain-on-terminal as the fix.** Tag-history cannot see overflow-block or auto-block. Trackers have different accumulation (last-iter vs all-run).
- **Flipping pipeline outcome to `IterationOutcome::Blocked` on `:blocked`.** Would `should_stop` the whole loop on one VERIFY gate.
- **Wave `TransientBackend` exclusion-list parity** with sequential. Adjacent hole; not this PRD.
- **Operator repair of `805856aa-VERIFY-PACER-OBS`** in the restaurant_agent_ex DB. After this ships there, re-block it (or let the next honest VERIFY iteration block it again). The code change is in **task-mgr**.

---

## 5.5. Low-Value / High-Effort Areas (Explicit Cuts or Deferrals)

| Area / Capability | Why the value is low relative to cost | Rough effort cost | Recommended action |
| --- | --- | --- | --- |
| Unify `last_claimed_task` + `pending_slot_tasks` | Different accumulation; `last_claimed` also drives wrapper-commit / external-git; tag-history drain still misses overflow/auto-block | High (seq/wave desync) | **Cut** |
| `ProcessingOutcome.closed_task_ids` / drain on `went_terminal_via_status` | Insufficient authority; crash-map already has the right local predicate | Medium | **Cut** as load-bearing; optional log hygiene only if it stays zero-churn |
| `TaskStatusChange::is_terminal()` extract | Only pays off if tracker drain lands; crash-map already matches | Low–medium | **Defer** |
| SELECT+`from_str` guard inside `reset_task_to_todo` | Wrong module, wrong shared predicate, TOCTOU, teaches the next person to “fix” `resurrect_for_iteration` | Looks cheap, high regret | **Rejected** (see §6) |
| Promise-BLOCKED as a non-failure like COMPLETE | Leftover `in_progress` must orphan-reset; promise is not a DB close | Small but wrong | **Cut** |

---

## 6. Technical Considerations

### Affected Components

- `src/lifecycle/recovery.rs` — new verbs
- `src/lifecycle/tests/recovery_tests.rs` — verb unit tests
- `src/lifecycle/mod.rs` — module rustdoc list
- `src/lifecycle/CLAUDE.md` — Recovery-family note + FR-006 table
- `src/loop_engine/wave_scheduler.rs` — split `reset_task_to_todo`; merge-fail arm; rustdoc
- `src/loop_engine/orchestrator.rs` — 17.5 / 17.6 comments (call-site swap is through the wrapper)
- `src/loop_engine/engine.rs` — `pending_slot_tasks` rustdoc
- `src/loop_engine/reactions/post_output.rs` — overflow rungs 1–3
- `src/loop_engine/recovery.rs` — terminal skip inside `handle_task_failure_with_runner`
- `src/loop_engine/slot.rs` — no write change; rustdoc only if it still says “remove on `done`” as if that were orphan policy
- Tests: `src/lifecycle/tests/recovery_tests.rs`, `wave_scheduler` merge-fail module tests, optional `tests/iteration_pipeline.rs` / overflow suites / `tests/retry_tracking.rs`

### Dependencies

- Internal: `TaskLifecycle`, `lifecycle::read_status`, `TaskStatus::is_terminal`, existing FEAT-002 merge-fail halt contract.
- External: none.
- Must-not-depend-on: pipeline tag-history, `completed_task_ids` membership, `<promise>` text.

### Approaches & Tradeoffs

Plan review (architect + explore) compared these. No separate `/spike` was required — the Recovery-verb family and the merge-fail × `:done` fork were the residual design risks, and both are resolved below.

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| A. `SELECT` + `from_str` guard inside `reset_task_to_todo` | Smallest-looking diff; one funnel for 17.5/17.6/merge-fail | Wrong module (lifecycle SSoT); TOCTOU; one predicate for two policies; merge-fail × `:done` silently strands work; third copy of `read_status` | **Rejected** |
| B. Add `WHERE status = 'in_progress'` to `resurrect_for_iteration` | One verb, no API growth | Breaks #4358 + pinned `blocked → todo` test; still the wrong merge-fail predicate | **Rejected** |
| C. New `recover_in_progress` + separate `reopen_after_merge_fail`; 17.5/17.6 and overflow 1–3 on the first; merge-fail on the second; failure-skip inside `handle_task_failure_with_runner` | Right module; atomic WHERE; two predicates; `resurrect` stays the escape hatch; seq/wave cannot drift | One extra Recovery verb | **Preferred** |
| D. Tracker drain on any terminal, no write guard | Looks like it would keep 17.6 from seeing honest closes | Misses overflow-block and auto-block; seq/wave drain sites can drift; next author reintroduces the write bug | **Rejected** as the fix |

**Selected Approach**: **C**.

**Phase 2 Foundation Check**: Approach C costs ~half a day extra versus A (one verb + one merge-fail predicate + tests) and avoids (1) a later “fix `resurrect_for_iteration`” incident, (2) silent lost-work on `:done` + merge-conflict, (3) a third status-policy site in the loop engine. Well above the 1:10 bar.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| Shared `in_progress`-only guard on merge-fail leaves `:done` stranded on the ephemeral | High (silent lost work) | High if Approach A is used; Low under C | Separate `reopen_after_merge_fail`; test merge-fail × `done` → `todo`. **Empirical test required.** |
| Someone “fixes” `resurrect_for_iteration` while implementing | High (breaks overflow contract + recovery_tests) | Medium (the rustdoc currently invites it) | CONTRACT-001 forbids touching that SQL; #4358 cited in the story; overflow 1–3 migrate off it so it has no orphan-reclaim callers |
| Dual-site `handle_task_failure` skip drifts | Medium (seq/wave consecutive_failures diverge) | High if copied at ~597 and ~1216 | Skip inside `handle_task_failure_with_runner` only; parity auditor |
| Tag-history-only implementation | High (overflow-5 / auto-block still undone) | Medium if someone “simplifies” | Overflow-5-then-helper and auto-block-then-helper tests are must-haves |

The first risk is High × High **only for Approach A**, which is rejected. Under C it is High × Low and has an empirical test. Not a blocker.

### Security Considerations

- No new auth, secrets, or user input. Task ids already flow from claim / `FailedMerge`.
- Conditional WHERE is the race-safety mechanism against a concurrent operator `fail` / decay writing a terminal while the loop exits.

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `TaskLifecycle::recover_in_progress` | `(&self, task_id: &str)` | `Ok(true)` iff one row updated | `Err(TaskMgrError)` on DB failure | `tasks.status='todo'`, `started_at=NULL`, `updated_at=now` **only if** current status is `in_progress` |
| `TaskLifecycle::reopen_after_merge_fail` | `(&self, task_id: &str)` | `Ok(true)` iff one row updated | `Err(TaskMgrError)` on DB failure | same columns **only if** current status is `in_progress` or `done` |

No new CLI, HTTP, or config surface. Loop-engine wrappers stay `pub(super)`.

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `reset_task_to_todo` | `(conn, task_id, kind_label)` → unguarded resurrect | Split / retarget: orphan wrapper → `recover_in_progress`; merge-fail → `reopen_after_merge_fail` | No (crate-internal `pub(super)`) | Call-site swap only |
| `handle_task_failure_with_runner` | same signature | same signature; early `Ok(())` when row is terminal | No (narrower side effect) | Callers unchanged |
| Overflow rungs 1–3 in `post_output.rs` | `resurrect_for_iteration(None, &[id])` | `recover_in_progress(id)` | No | Behavior-neutral on the current `in_progress` row |
| `resurrect_for_iteration` | unchanged | unchanged | No | Frozen |

### Data Flow Contracts

See §2.6. Additional notes:

- **Type transition to avoid**: `completed_task_ids: Vec<String>` is done-only. Do not treat membership as “classified.” Crash-map already has `went_terminal_via_status` for prune; that local predicate must **not** become reset authority.
- **`tasks.status` CHECK**: `'todo' \| 'in_progress' \| 'done' \| 'blocked' \| 'skipped' \| 'irrelevant'`. There is no `'failed'` row status. `:failed` / `:fail` / `:blocked` all land as `blocked`.

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
| --- | --- | --- | --- |
| `orchestrator.rs` ~734–754 | 17.5 / 17.6 call `reset_task_to_todo` | **BREAKS** today’s “force any listed id to todo” | Point at `recover_in_progress`; that **is** the fix |
| `wave_scheduler.rs` ~586–589 | merge-fail reset | **NEEDS REVIEW** | Must use `reopen_after_merge_fail`, not the orphan verb |
| `reactions/post_output.rs` ~308 | overflow rungs 1–3 | OK | Row is `in_progress` when the rung fires |
| `lifecycle/tests/recovery_tests.rs` `resurrect_for_iteration_flips_listed_ids_to_todo` | pins `blocked → todo` | OK | Do not change that verb |
| `wave_scheduler` FEAT-002 tests (~3300+) | insert `in_progress`, assert `todo` | OK | Still true under the new merge-fail predicate |
| `tests/overflow_*.rs` | rung 1–5 status | OK | Rungs 1–3 stay `todo`; rung 5 stays `blocked` and must survive 17.6 |
| `tests/retry_tracking.rs` / `handle_task_failure` suites | increment + auto-block | **NEEDS REVIEW** | Skip only when already terminal; `in_progress` / `todo` paths unchanged |
| `recover_in_progress_for_prefix` / rate-limit `reset_in_progress_tasks` | bulk B1 guard | OK | Unchanged |
| CLI `reset` / `unblock` / decay | operator / age | OK | Out of scope |
| `git_reconcile` / `get_completed_task_ids` | passed = `done`/`irrelevant` | OK | Do not unify with `is_terminal()` |

### Semantic Distinctions

| Code Path | Context | Current Behavior | Required After Change |
| --- | --- | --- | --- |
| 17.5 / 17.6 `reset_task_to_todo` | Loop-exit orphan reclaim | `* → todo` | `in_progress → todo` only |
| Merge-fail `reset_task_to_todo` | Work did not land on slot 0 | `* → todo` | `in_progress\|done → todo` |
| `resurrect_for_iteration` | Documented force-any-status | `* → todo` | **Unchanged** |
| Overflow rungs 1–3 | Retry-in-place after PromptTooLong | unguarded resurrect | `recover_in_progress` |
| Overflow rung 4 | Cross-provider pivot | `resurrect_with_model_override` (`in_progress`) | Unchanged |
| Overflow rung 5 / auto-block | Honest/policy terminal | `blocked` | Must survive 17.5/17.6 |
| `recover_in_progress_for_prefix` | Bulk startup / rate-limit / transient | `in_progress → todo` | Unchanged (already correct) |
| `handle_task_failure` | Consecutive-failure ladder | Increments regardless of status | Skip if `is_terminal()`; still run on `todo` / `in_progress` |
| `completed_task_ids` / wrapper-commit | PRD progress | done-only | Stay done-only |
| `<promise>BLOCKED</promise>` | Agent promise, no status tag | leftover `in_progress` | Still orphan-reset |

### Inversion Checklist

- [x] All callers of `reset_task_to_todo` / `resurrect_for_iteration` identified (17.5, 17.6, merge-fail, overflow 1–3, tests).
- [x] Routing that depends on `completed_task_ids` / `prd_complete` / drain-classifier reviewed — stay done-only / terminal-count as today.
- [x] Tests that pin unguarded `resurrect_for_iteration` identified and left intact.
- [x] Merge-fail × `:done` vs orphan × `:done` documented as different predicates.
- [x] Learning #4358 / #4810 / #5151 / #3727 cited so the loop does not “fix” the wrong verb or move `reconcile_ambiguous_exit`.

### Documentation

| Doc | Action | Description |
| --- | --- | --- |
| `src/lifecycle/CLAUDE.md` | Update | Recovery-family table: three policies (bulk/per-id `in_progress` reclaim, merge-fail `in_progress\|done`, unguarded `resurrect`). FR-006 row for the new verbs. Fix stale “six verbs” / “three Recovery verbs” counts. |
| `src/loop_engine/CLAUDE.md` | Update | Status-mutation table / orphan-reset paragraph: 17.5/17.6 call `recover_in_progress`; merge-fail calls `reopen_after_merge_fail`. |
| `IterationContext::pending_slot_tasks` + orchestrator 17.5/17.6 rustdoc | Update | Closed ≠ `<completed>`; orphan reset is DB-status gated. |
| `docs/designs/coherence-refactoring.md` | No change required | Historical note about the unguarded relaxation stays accurate; the new verbs are the response, not a rewrite of that retrospective. |

### Institutional Memory (embedded)

| ID | Takeaway for this PRD |
| --- | --- |
| **#4358** | Do not add an `in_progress` guard to `resurrect_for_iteration` to paper over another bug. |
| **#4810** | Recovery/mutation resets gate via `WHERE status = …`, not SELECT-then-write. |
| **#5151** | `reconcile_ambiguous_exit` must run **before** 17.5. Do not reorder. |
| **#2304** | Crash-map must treat Failed/Skipped/Irrelevant as terminal (already does). Do not confuse that prune with reset authority. |
| **#3727** | Do not increment `consecutive_failures` for non-task-logic outcomes. An already-terminal honest close is that class. |
| **#3093** | Reset to `todo` clears `started_at` in the same UPDATE. |
| **#3101** | Wave retry tracking is main-thread and must stay in parity with sequential — hence skip **inside** `handle_task_failure_with_runner`. |

---

## 7. Open Questions

Resolved during plan review; locked here so `/prd-tasks` does not re-litigate them:

- [x] **Merge-fail + `:done`:** reopen to `todo` (work never reached slot 0).
- [x] **Overflow rungs 1–3 this PRD?** Yes — migrate onto `recover_in_progress`.
- [x] **`<promise>BLOCKED</promise>` without a status tag:** still increment `handle_task_failure` if the row is `in_progress`, and 17.5/17.6 still orphan-reset. Promise is not a DB close.

No remaining open questions.

---

## Appendix

### Related Documents

- Session plan: Keep honest terminal closes from being reset to `todo` (architect-reviewed; Approach C).
- `src/lifecycle/CLAUDE.md` — Recovery verb families, FR-006.
- `docs/designs/coherence-refactoring.md` § Phase 1 retrospective — documents the unguarded `resurrect_for_iteration` relaxation.
- `docs/designs/fix-double-task-claim.md` — learning #4358.
- `tasks/prd-tasklifecycle-extraction.md` — original Category C verb contracts.

### Glossary

- **Orphan reclaim**: loop-exit (17.5 / 17.6) flipping a stranded `in_progress` claim back to `todo` so the next process does not wait on startup recover. Must not reclassify a row that is already `todo` or terminal.
- **Merge-fail reopen**: FEAT-002 reset after a slot’s merge-back failed. Work on the ephemeral did not land on slot 0. May undo `done` (premature completion); must not undo `blocked` / `skipped` / `irrelevant`.
- **Retry-in-place**: overflow rungs 1–4 resetting a still-claimed overflowed task to `todo` (rung 4 also writes `tasks.model`). Only legal while `in_progress`.
- **Honest / policy terminal**: `:blocked` / `:skipped` / `:irrelevant` / `:done`, plus `auto_block_after_failures` and overflow rung 5. Stay terminal under orphan reclaim.
- **`resurrect_for_iteration`**: unguarded per-id `* → todo` Recovery verb. Frozen. Not an orphan-reclaim API.
- **`recover_in_progress`**: new per-id sibling of `recover_in_progress_for_prefix`. Atomic `in_progress → todo`.
- **`reopen_after_merge_fail`**: new per-id Recovery verb. Atomic `in_progress|done → todo`.
