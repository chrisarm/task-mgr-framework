# Fix double in_progress from competing task selectors

**Status:** Implemented (REVIEW-001 full gates + checklist + simulated path + invariants verified 2026-06-16) 
**Date:** 2026-06-15  
**Chosen approach:** Drop `next --claim` from prompt templates + engine-injected guard  
**Test baseline:** `cargo test --test prompt_assembler_parity` green (25/25) pre-implementation

## Problem

Loop iterations sometimes leave **two tasks in `in_progress`** because two independent selectors both claim work:

1. **Engine pin** — `build_prompt` selects and claims a task, then injects it as `## Current Task` in the iteration prompt.
2. **Prompt step 1** — Generated `*-prompt.md` files (from `/tasks` or `/plan-tasks`) instruct the agent to run `task-mgr next --prefix $PREFIX --claim` as its first action.

The agent log states the mismatch plainly: *"task-mgr next is currently pointing at FEAT-007, while your prompt explicitly scoped this turn to FEAT-006."*

## Root cause (confirmed in code)

Two independent claim paths run every loop iteration:

```mermaid
sequenceDiagram
    participant Engine as build_prompt
    participant DB as tasks.db
    participant Agent as Loop agent

    Engine->>DB: next_excluding(claim=true)
    Note over DB: Task A → in_progress
    Engine->>Agent: Prompt with ## Current Task (A) + base prompt
    Agent->>DB: next --claim (from base prompt step 1)
    Note over DB: A skipped (not todo); Task B → in_progress
    Note over Agent: Two tasks in_progress
```

### Engine path

[`src/loop_engine/prompt/sequential.rs`](../../src/loop_engine/prompt/sequential.rs) (line ~409) calls `next::next_excluding(..., claim=true)` before assembling `## Current Task`.

Parallel/wave mode uses the same pattern: the wave scheduler selects tasks and `claim_slot_task` transitions them to `in_progress` before the slot prompt is built.

### Agent path

Generated `*-prompt.md` files from:

- [`.claude/commands/tasks.md`](../../.claude/commands/tasks.md)
- [`.claude/commands/plan-tasks.md`](../../.claude/commands/plan-tasks.md)

…instruct step 1:

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/<prd>.json)
task-mgr next --prefix $PREFIX --claim
```

### Why they diverge (deterministic, not intermittent)

[`src/commands/next/selection.rs`](../../src/commands/next/selection.rs) only considers `status='todo'` tasks. After the engine claims task A:

- Agent `next --claim` cannot re-select A (already `in_progress`).
- Selection picks the next eligible todo task B and claims it.

Result: A and B both `in_progress`.

### Why existing `task_ops` doesn't prevent this

[`src/loop_engine/prompt_sections/task_ops.rs`](../../src/loop_engine/prompt_sections/task_ops.rs) mentions `task-mgr next` for reads but does not forbid `--claim`. The conflicting step-by-step workflow lives in the **base prompt** (`*-prompt.md`), which is appended **last** in both assembly rosters ([`sequential.rs`](../../src/loop_engine/prompt/sequential.rs), [`slot.rs`](../../src/loop_engine/prompt/slot.rs)).

## Assumption validation (2026-06-15, subagent audit)

Four read-only subagents validated the plan against the codebase. Summary:

### Core bug mechanism — all CONFIRMED

| Assumption | Verdict | Evidence |
|------------|---------|----------|
| Sequential `build_prompt` calls `next_excluding(..., claim=true)` before `## Current Task` | **CONFIRMED** | `sequential.rs:409-417` |
| `select_next_task_excluding` only considers `status='todo'` | **CONFIRMED** | `selection.rs:303, 497` |
| CLI `next --claim` only transitions `todo → in_progress` | **CONFIRMED** | `next/mod.rs:272-274`, `lifecycle/claim.rs:4-5` |
| Skills + generated `*-prompt.md` instruct `next --prefix $PREFIX --claim` as step 1 | **CONFIRMED** | `tasks.md:530,569-573`; `plan-tasks.md:655,694-698`; examples in `tasks/data-driven-prompt-construction-prompt.md`, `tasks/cost-efficient-auxiliary-llm-prompt.md` |
| `task_ops.rs` does not forbid `--claim` | **CONFIRMED** | No guard text in `task_ops_section()` |
| `base_prompt` assembled **after** `## Current Task` in both rosters | **CONFIRMED** | `sequential.rs:667-671`; `slot.rs:540-541` |
| Read-only `next` still returns a different task after engine claim | **CONFIRMED** | Pinned task no longer `todo`; `--claim` adds second `in_progress` row |
| Wave mode claims on main thread before slot workers | **CONFIRMED** | `wave_scheduler.rs:91-147` (`claim_slot_task` before `thread::spawn`) |

**Wave ordering nuance (does not change fix):** slot prompts are built first (`build_slot_contexts`), then claimed in `run_parallel_wave`. In-memory task status is set to `in_progress` during prompt build (`wave_scheduler.rs:225-231`). Claim and prompt bundle always share the same `task_id` today.

### Learning #4251 (wave claim vs assign divergence) — FIXED

Subagent audit: the priority-claim / overlap-assign split described in learning **#4251** is **no longer present**. Current wave flow uses a single selector:

1. `select_parallel_group_excluding` (same `build_scored_candidates` kernel as `next`)
2. `prompt::slot::build_prompt(conn, &scored.task, …)` — no internal `next`
3. `claim_slot_task(conn, &slot.prompt_bundle.task_id)` — same ID

Tests: `test_wave_parallel_slots_one_runs_a_single_task`, `test_build_slot_contexts_populates_bundle_on_main_thread`, `test_run_parallel_wave_claims_all_tasks_before_spawning`.

**Implication:** The user's observed bug (agent `next --claim` vs engine pin) is the **primary** remaining dual-authority failure mode. #4251 is historical context only — remove from follow-up audit scope.

### Test coverage — gaps confirmed

| Layer | Exists | Missing |
|-------|--------|---------|
| Prompt forbids agent `next --claim` | — | All assertions |
| `pin_authority` section | Design doc only | Module + tests |
| `build_prompt` leaves exactly one `in_progress` (multi-task PRD) | — | Integration test |
| Simulated agent `next --claim` → two `in_progress` | — | `tests/prompt_pin_authority.rs` |
| `try_claim` race / no orphan soak | `lifecycle_performance.rs` | — (different layer) |
| Snapshot | `prompt_sequential_v1.txt` | Needs regen after `pin_authority` + `task_ops` edits |

**Recommended test home:** `tests/prompt_pin_authority.rs` — copy pattern from `tests/prompt_sequential_snapshot.rs`; use ≥2-task PRD; stale base prompt fixture with legacy `next --claim` step; parse task ID from `## Current Task` block only (per learning #5219).

### Implementation status check

`pin_authority.rs` **does not exist yet** — plan is validated but not implemented. Rosters still end at `base_prompt`.

## Recall insights (2026-06-15)

Multiple targeted `task-mgr recall --query …` runs (OR syntax not supported by CLI — used repeated queries for "double claim", "task claim race condition", "slot claim", `try_claim`, `in_progress`, claim authority/selection). **No contradictory prior decisions surfaced.** No prior learning specifically named "double `in_progress` from competing selectors" — this root cause is newly diagnosed; recalls reinforce existing claim invariants.

### Hard invariants — do not touch during this fix

| ID | Title | Takeaway |
|----|-------|----------|
| **3386** + **3766** | Different claim predicates are intentional | CLI `next --claim` (`next/mod.rs`) is strict: `todo` only. Slot `claim_slot_task` (`engine.rs` / `slot.rs`) uses `todo \| in_progress` for idempotent re-claim (refreshes `started_at` on recovery). **Do not unify.** |
| **3439** + **3766** | Race-safe pre-claim predicates stay explicit | `TaskLifecycle::try_claim` (`lifecycle/claim.rs`) takes `&[TaskStatus]` for conditional-WHERE optimistic locking (FR-005). `Ok(false)` = lost race / skip — not retry or error. No hiding behind a generic claim helper. |
| **3008** | `SlotResult.claim_succeeded` | Distinguishes engine claim failure from worker panic. Claim failures are expected in parallel waves. |
| **1895** | Main-thread wave claiming | `run_parallel_wave` + `claim_slot_task` before `thread::spawn` — load-bearing for worktree/slot accounting. |

### Directly supports this fix

| ID | Title | Takeaway for plan |
|----|-------|-------------------|
| **737** | Mirror documentation across prompt template and skill file | Template edits in `.claude/commands/tasks.md` must be mirrored in generated `tasks/*-prompt.md`. Validates the two-layer approach: engine guard for **existing** prompts + template rewrite for **new** ones. |
| **5225** | Mock script robustness requires respecting prompt structure boundaries | `## Current Task` is the canonical task-identity boundary in assembled prompts. Reinforces making it the sole work authority (not `next` output). |
| **4224** | Use task-mgr CLI exclusively for task operations | After fix, agent should use `task-mgr show <id>` / `<task-status>` — not `next --claim` — consistent with CLI-only workflow. |

### Reorder semantics (not a second in-iteration claim)

Reorder-related recalls (multiple hits): `<reorder>TASK-ID</reorder>` is detected in `detection.rs`, turned into a **deferred hint for the next iteration's** selection/claim (sequential: `ctx.reorder_hint`; wave: `pending_reorder_hints` queue). `MAX_CONSECUTIVE_REORDERS=2` forces algorithmic pick. Waves buffer hints; `select_parallel_group` is score-driven and does not honor hints yet. **Reorder is not an in-iteration second claim** — the current iteration's already-claimed task is left for recovery sweeps, not simultaneously worked alongside a `next --claim` pick.

### Related failure class (same symptom, different mechanism)

| ID | Title | Takeaway for plan |
|----|-------|-------------------|
| **4251** | Loop wave can orphan a task in_progress when claim-by-priority diverges from assign-by-overlap-score | **Historical — FIXED** in current code (subagent audit 2026-06-15). Wave now uses single `select_parallel_group_excluding` → `build_prompt` → `claim_slot_task` on same ID. Detection/workaround playbook still useful for **this** bug's stranded tasks. |

### Recovery infrastructure (symptom handling, not root-cause fix)

| ID | Title | Takeaway for plan |
|----|-------|-------------------|
| **3927** | Wave-mode empty-group path lacked sequential's all-complete + recovery | Stranded `in_progress` tasks can cause false "no eligible tasks after 3 stale iterations" aborts. `recover_in_progress_for_prefix` + `classify_drained_queue` already exist. Our fix reduces how often recovery is needed; does not replace it. |
| **3008** | Slot claim-succeeded boolean distinguishes claim failures from worker panics | Claim failures ≠ post-claim crashes. Relevant if we add detection metrics for orphan `in_progress` tasks. |
| **4358** | resurrect_for_iteration intentionally omits the in_progress guard | Do not "fix" recovery verbs to paper over dual-claim; fix the authority collision instead. |

### Tangential (same subsystem, different issue)

| ID | Title | Note |
|----|-------|------|
| **967** | requiresHuman:true does not prevent in_progress re-selection | Selection guard gap — separate from prompt `next --claim` but shows selection authority bugs have happened before in this area. |
| **5219** | Mock script task-ID parsing fragile to prompt format changes | Tests parsing prompts should anchor on `## Current Task`, not first `"id"` in file — relevant for any new integration test fixture. |

### Operator detection playbook (from #4251, applies here too)

When `in_progress` count exceeds active slots (or agent reports `next` ≠ pinned task):

```bash
# Find tasks stuck in_progress with no loop activity
task-mgr list --prefix $PREFIX --status in_progress
task-mgr show <SUSPECT-ID>   # Started set, no progress

# Confirm never dispatched (no iteration header in loop log)
grep '═══ Iteration.*Task: <SUSPECT-ID>' .task-mgr/logs/task-mgr-<prefix>.log

# Unblock
task-mgr reset <SUSPECT-ID>
```

## Rejected alternative: read-only `next`

Having the prompt call `task-mgr next` **without** `--claim` does not fix the mismatch:

- The pinned task is already `in_progress` after engine claim.
- Read-only `next` still skips it and surfaces a **different** todo task.
- `## Current Task` already contains full task JSON; learnings are pre-injected. Read-only `next` adds no value.

## Chosen fix

**Single selection authority: the loop engine** for all autonomous `loop run` / `batch run` iterations.

Three mitigations, ordered by salience (see **Placement note** below):

1. **Template rewrite (primary)** — Remove `next --claim` from `/tasks` and `/plan-tasks` generators so new `*-prompt.md` files never instruct claim-as-step-1.
2. **Early `task_ops` rule (primary)** — Strong prohibition immediately after `## Current Task`, before the large base prompt.
3. **Late `pin_authority` guard (safety net)** — Critical section after `base_prompt` overriding stale template text still present in in-flight prompts.

## Implementation considerations (peer review)

### 1. Reading order / instruction salience — highest implementation risk

**Sequential** final stitch (`sequential.rs` ~667): `{task_section}{task_ops}…{reorder_instr}{key_decision}{base_prompt}{pin_authority}`. The large base prompt ("## Your Task (every iteration)" / step-1 shell snippet / "## Task Selection (reference)") appears **late**.

**Slot** (`slot.rs` ~540): `{task_section}{task_ops}…{completion}{base_prompt}{pin_authority}` — `## Current Task` is first (good), but the same late base-prompt problem applies.

**LLM risk:** A prominent "step 1 of your workflow" inside the authoritative-looking base prompt can outweigh a later addendum even if it says "DO NOT."

**Mitigation priority:**

| Layer | Role |
|-------|------|
| Template removal of step-1 `next --claim` | **Primary** — eliminates the conflicting instruction |
| Strong `task_ops` bullet right after task envelope | **Primary** — early, hard language: "NEVER run `next --claim` in a loop iteration; the engine already pinned and claimed the task in `## Current Task`" |
| `pin_authority` after `base_prompt` | **Safety net** for stale in-flight `*-prompt.md` — not the sole preventive |

**Implementation note:** `pin_authority` stays after `base_prompt` (to override legacy text), but integration tests must assert the **combined** early + late prohibitions are hard to miss when a full stale base prompt is present. Consider asserting `task_ops` prohibition appears **before** the stale `next --claim` snippet in rendered output.

### 2. Reorder interaction — explicit template language

Reorder is sequential-only in prompt (slot drops reorder sections; wave queues `pending_reorder_hints`). It affects the **subsequent** iteration's `build_prompt` / `next_excluding` / claim — not an in-iteration second claim.

Template + guard text must state:

- (a) `<reorder>TASK-ID</reorder>` is the **only** sanctioned way to influence the next pick.
- (b) It affects the **next** iteration (engine will claim); do not combine with `next --claim`.
- (c) The current iteration's claimed task is left for recovery sweeps — reorder does **not** produce two simultaneous `in_progress` tasks.

Optional test note: reorder outcome should not leave a second concurrent `in_progress` from agent `next --claim` (orthogonal paths).

### 3. Legacy external drivers — explicit boundary

[`scripts/claude-loop.sh`](../../scripts/claude-loop.sh) does its own `next --claim` (~line 397), manually builds `## Current Task`, then concatenates `$PROMPT_FILE`. It **bypasses** `build_prompt`, rosters, and `pin_authority`.

**Out of scope for engine guard.** Users of `claude-loop.sh` (or similar external drivers) must update their `prompt.md` / script if it still contains claim-as-first-step. No code change required in this PR; boundary must be explicit in rollout.

### 4. Roster wiring mechanics

- Add `pin_authority.rs` + `pin_authority_spec()` + static `render` (pure critical, no trimmable budget — matches `task_ops` pattern).
- Wire into **both** `sequential_roster()` and `slot_roster()` after `base_prompt`.
- Ensure the section is included in the **criticals sub-roster** used by `assemble_criticals` (not only the display roster).
- Update sequential final `format!` stitch and slot prompt construction to append guard after `base_prompt`.
- `prompt_assembler_parity.rs` roster-completeness + symmetry checks drive wiring — add module early so parity test (25/25 baseline) catches drift.

### 5. Historical example prompts in repo

Several `tasks/*-prompt.md` and `.task-mgr/tasks/*-prompt.md` files still contain `next --prefix $PREFIX --claim` + "You don't pick — you claim what it returns." These are generator examples / historical fixtures. **Not bulk-updated.** Harmless at runtime once guard + new generators ship. Repo greps may still show the old pattern.

### 6. Defensive observability (optional, defer)

- Post-claim `in_progress` count vs expected concurrency log/assertion (debug/test only).
- `doctor` / `stats` surfacing "N `in_progress` for prefix (possible prior double-claim)".

Reasonably out of scope; recovery sweeps (`recover_in_progress_for_prefix`) already exist. Add one-line warning if cheap during PR; otherwise defer.

### 7. Subsystems confirmed covered

Wave/parallel (main-thread claim, `pending_reorder_hints`), Codex/Grok/Claude runners (shared prompt builders), recovery/rate-limit/overflow/human CLARIFY/batch — all inherit guard via `build_prompt` / slot roster. No impact on DB lock (`tests/concurrent.rs`), `try_claim` contract, or claim predicates.

Skill embedding: `.claude/commands/tasks.md` + `plan-tasks.md` changes flow via `include_str!` in `src/skills.rs` + manifest-guarded staging on `init` / `loop init` / `batch init`. In-flight loops do **not** need re-init for the engine guard.

## Implementation plan

### 1. Engine guard — `pin_authority` critical section after `base_prompt`

Add `src/loop_engine/prompt_sections/pin_authority.rs` with short static critical text, e.g.:

> This iteration's task is in `## Current Task` above. The loop engine already selected and claimed it. **Do NOT** run `task-mgr next --claim` (or any `next` to pick work) — that claims a different task and leaves two `in_progress` rows. Use `task-mgr show <id>` only if you need to re-read fields. To influence the **next** iteration's pick, emit `<reorder>TASK-ID</reorder>` — never combine reorder with `next --claim`.

**Placement note:** `base_prompt` is late in both rosters and in the final stitched prompt. The post-`base_prompt` guard is **late reinforcement** for stale templates. **`task_ops` + template removal are the primary preventives.**

Wire into **both** rosters **after** `base_prompt`:

| File | Change |
|------|--------|
| [`src/loop_engine/prompt/sequential.rs`](../../src/loop_engine/prompt/sequential.rs) | Append `pin_authority_spec()` after `base_prompt` in `sequential_roster()`; extend final stitch `format!` |
| [`src/loop_engine/prompt/slot.rs`](../../src/loop_engine/prompt/slot.rs) | Same in `slot_roster()` + slot prompt construction |
| [`src/loop_engine/prompt_sections/mod.rs`](../../src/loop_engine/prompt_sections/mod.rs) | Export new module |

Update [`tests/snapshots/prompt_sequential_v1.txt`](../../tests/snapshots/prompt_sequential_v1.txt) via `INSTA_UPDATE=1`.

Unit test: section forbids `--claim`, references `## Current Task`, mentions reorder semantics.

### 2. Strengthen `task_ops` (primary early rule)

In [`src/loop_engine/prompt_sections/task_ops.rs`](../../src/loop_engine/prompt_sections/task_ops.rs), add a prominent bullet (not a single soft line):

- **Loop iterations:** Work only the task in `## Current Task`. The engine already claimed it. **NEVER** run `task-mgr next --claim` (or `next` to pick work) inside a loop iteration — that claims a different task. Use `task-mgr show <id>` to re-read fields.

Appears immediately after the task envelope, **before** base prompt — this is the highest-salience preventive.

### 3. Update prompt templates

In [`.claude/commands/tasks.md`](../../.claude/commands/tasks.md) and [`.claude/commands/plan-tasks.md`](../../.claude/commands/plan-tasks.md):

| Location | Change |
|----------|--------|
| Commands table row "Pick + claim…" | → "Inspect this iteration's task" → `task-mgr show <TASK-ID>` (ID from `## Current Task`) |
| `## Your Task (every iteration)` step 1 | Remove `next --claim`; replace with: work `## Current Task` — engine already claimed it |
| `## Task Selection (reference)` | Engine owns selection+claim at iteration start. Agent may emit `<reorder>TASK-ID</reorder>` to request a different pick on the **next** iteration (engine will claim). Do not use `next --claim`. |
| Antipatterns row (plan-tasks) | "Trust `## Current Task`; never `next --claim` in loop iterations; reorder affects next iteration only" |

**Leave unchanged** (non-loop / manual contexts):

- [`CLAUDE.md`](../../CLAUDE.md) cheat sheet
- [`.claude/commands/tm-next.md`](../../.claude/commands/tm-next.md)
- [`docs/INTEGRATION.md`](../../docs/INTEGRATION.md)
- [`src/cli/commands.rs`](../../src/cli/commands.rs) help examples
- [`src/commands/cheatsheet.rs`](../../src/commands/cheatsheet.rs)

**Out of scope — legacy external driver:** [`scripts/claude-loop.sh`](../../scripts/claude-loop.sh) bypasses engine guard; operators must update `prompt.md` / script separately (optional follow-up PR).

### 4. Tests

| Test | Purpose |
|------|---------|
| `pin_authority` unit + snapshot update | Guard text in assembled prompt; reorder semantics mentioned |
| New `tests/prompt_pin_authority.rs` | ≥2-task PRD; **stale base prompt** fixture with full step-1 `next --claim` workflow; assert (1) `task_ops` prohibition appears **before** stale snippet, (2) `pin_authority` after base prompt, (3) exactly one `in_progress` after `build_prompt`; optional: simulate agent `next --claim` → two `in_progress` documents bug class |
| [`tests/prompt_assembler_parity.rs`](../../tests/prompt_assembler_parity.rs) | Add `pin_authority` to known sections; assert after `base_prompt` in both rosters; keep 25/25 green |
| [`tests/prompt_sequential_snapshot.rs`](../../tests/prompt_sequential_snapshot.rs) | Extend section-order test: `task_ops` before base; `pin_authority` after `# Agent Instructions` |

No change to `next` scoring or claim predicates (per learnings **#3386**, **#3439**, **#3766**).

### 5. Rollout for in-flight PRDs

| Audience | Action |
|----------|--------|
| All active `loop run` / `batch run` | Engine guard applies on binary upgrade + next run — no `*-prompt.md` edit or re-init required |
| New PRDs | Regenerate via `/tasks` or `/plan-tasks` for cleaner base prompts (primary salience fix) |
| Archived / example `tasks/*-prompt.md` | Not bulk-updated; engine guard covers runtime |
| `scripts/claude-loop.sh` users | **Not protected** by engine guard — must update external `prompt.md` / script manually |

## Files touched (summary)

- **New:** `src/loop_engine/prompt_sections/pin_authority.rs`
- **Edit:** `task_ops.rs`, `sequential.rs`, `slot.rs`, `prompt_sections/mod.rs`
- **Edit:** `.claude/commands/tasks.md`, `.claude/commands/plan-tasks.md`
- **Test:** snapshot + integration test

## Out of scope

- Changing `next` scoring or unifying CLI/slot claim predicates (learnings **#3386**, **#3439**, **#3766**)
- Modifying `try_claim` expected-status contract or FR-005 conditional-WHERE semantics
- Doctor auto-recovery for stranded dual-`in_progress` (symptom-only; root cause addressed above)
- Wave scheduler claim-by-priority vs assign-by-overlap divergence (learning **#4251** — fixed in current code)
- `scripts/claude-loop.sh` code changes (boundary documented; optional follow-up)
- Bulk update of historical example `tasks/*-prompt.md` fixtures
- `in_progress` count diagnostic in doctor/stats (optional hardening; defer)

## Review checklist

- [ ] Agree engine is sole claim authority for autonomous loop iterations
- [ ] Agree three-layer mitigation priority: template removal + early `task_ops` (primary), late `pin_authority` (safety net)
- [ ] Agree post-`base_prompt` guard is late in document order — salience risk acknowledged
- [ ] Agree template wording for step 1 + reorder ("next iteration only; engine claims")
- [ ] Confirm no bulk-regenerate of archived/example `*-prompt.md` files
- [ ] Confirm `scripts/claude-loop.sh` / external drivers are out of scope for engine guard
- [ ] Confirm both rosters + criticals sub-roster + final stitch + parity test all wire `pin_authority`
- [ ] Integration test asserts prohibition visible with full stale base prompt present
- [x] Learning **#4251** wave claim/assign divergence — fixed in current code
- [x] Claim predicate / `try_claim` invariants — recalls confirm no changes needed
- [x] `prompt_assembler_parity` baseline green (25/25)