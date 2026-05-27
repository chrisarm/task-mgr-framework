# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Grok Fallback Runner** for **task-mgr**.

## Problem Statement

The task-mgr loop currently dead-ends when the Claude CLI fails on a task in
two scenarios the existing recovery system cannot escape:

1. **`PromptTooLong`**: after the 4-rung overflow ladder
   (`downgrade_effort → escalate_below_opus → to_1m_model → blocked`) reaches
   Opus[1M] at high effort. Task is marked `blocked` with no further options.
2. **`RuntimeError`** (generic unknown crash): after consecutive-failure
   model escalation reaches the Opus ceiling. Task retries on Opus until
   human intervention or `auto_block_task` fires at `max_retries`.

This PRD adds an `LlmRunner` trait abstraction with `ClaudeRunner` + `GrokRunner`
impls, plus a 5th rung in the overflow ladder and a `RuntimeError` fallback
hook that promote tasks to Grok when the Claude ladder is exhausted. Disabled
by default; opt-in via `.task-mgr/config.json`. The architect review surfaced
~10 edge cases (override precedence vs operator intent, `tasks.model` DB
column writes paired with override inserts, wave-mode wiring, single-source
idempotency guards, Grok auth-failure short-circuits, etc.) — all incorporated
into the PRD before /tasks ran.

A tag-emission spike confirmed grok auto-loads `CLAUDE.md` + `.claude/skills/`
natively and emits the loop's control tags (`<promise>`, `<task-status>`,
`<reorder>`) correctly on first try with no system-prompt override.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (the runner trait extraction now pays back 10x when adding OpenAI/Gemini later) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Substring matching `.contains("grok")` for provider detection — must use token-equality on `-` splits to avoid mis-routing `groq-llama` (Groq Inc.) to xAI Grok
- Re-deriving `effective_runner` in multiple places — compute ONCE per iteration and pass the value through
- Passing `prompt_result.resolved_model` (pre-override) to `handle_prompt_too_long` — must pass `effective_model` (post-override) or idempotency guard breaks silently
- `Box<dyn LlmRunner>` dynamic dispatch — use `enum RunnerKind` + static match dispatch (allocation-free; exhaustive-match guards against missing variant handlers)
- OR-style idempotency guard (`runner_overrides.get(task)` OR `provider_for_model(model)`) — pin to the single computed `effective_runner` value; OR masks future drift
- Skipping the `tasks.model` DB UPDATE when overflow rung 4 or RuntimeError hook sets `runner_overrides` — without it, `resolve_task_model` on next iteration silently shadows the override
- Running RuntimeError fallback hook inside a slot worker — IterationContext is not thread-safe; wire it into the post-wave aggregation step on the main thread
- Persisting `runner_overrides` / `model_overrides` / `effort_overrides` to DB — design decision is in-memory only (matches existing pattern); restart clears state
- Counting `TaskMgrError::GrokAuthFailure` toward `consecutive_failures` — auth lapses must not push a healthy task into `auto_block_task` with a misleading reason
- Parsing `grok --help` output to check flag availability — brittle; minimum version goes in user-facing docs only
- Tests that hand-build HashMaps to mock IterationContext state — use the real struct so wrong field names fail at compile time, not silently
- Tests that only assert 'no crash' or check type without verifying content
- Migrating background `spawn_claude` callers (curate, learnings, milestone, prd_reconcile, watchdog) to direct LlmRunner — out of v1 scope; they keep using the spawn_claude wrapper

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: Scoped tests for touched files pass with `cargo test`
- Rust: `cargo fmt --check` passes
- No breaking changes to existing public `spawn_claude` signature (preserved via type aliases `SpawnOpts = RunnerOpts` and `ClaudeResult = RunnerResult`)
- No new `.unwrap()` or `.expect()` in production code paths (existing `#[cfg(test)]` allow continues)
- All `eprintln!` warnings include enough context to identify the failed operation (task ID, operation name, path if applicable)
- Behavior unchanged when `fallbackRunner` config is absent or `enabled:false` — byte-identical to today's 4-rung ladder ending in `Blocked`

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/grok-fallback-runner.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task    | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by MILESTONE-N` — priority auto-computed; DB + PRD JSON updated atomically                                                   |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

### Files you DO touch

| File                                          | Purpose                                                                |
| --------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/grok-fallback-runner-prompt.md`        | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`                  | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing; never crash on absent files.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/grok-fallback-runner.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes`. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `task-mgr next` listed a `dependsOn` task whose rationale you need, grep that specific task's block.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. **Do not** Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** The CLAUDE.md content that matters for THIS PRD is embedded in the **CLAUDE.md Excerpts** section below. If a task description cites a section name not shown here, grep for it (`grep -n -A 10 '<keyword>' CLAUDE.md`).

4. **Verify branch** — `git branch --show-current` should match `feat/grok-fallback-runner`. Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — and even then, one rejected alternative with a one-line reason is enough.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). Fix failures before committing.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`/`chore:` as appropriate).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON.

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---`.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` already picks: eligible tasks (`passes: false`, deps complete, not `requiresHuman`), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority. You don't pick — you claim what it returns.

Two runtime checks you DO own:

- If the returned task has `preflightChecks`, run them. If any fails: `task-mgr skip <TASK-ID> --reason "<preflight failure>"` and re-run `task-mgr next`.
- If the previous task had a `completionCheck`, run it before starting the new one. If it fails: `task-mgr fail <prev-task> --error "completionCheck failed"` and fix it first.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

For this PRD: **FEAT-007** (RuntimeError fallback hook in `escalate_task_model_if_needed`) is the only `modifiesBehavior: true` task. **ANALYSIS-001** is its prerequisite.

1. ANALYSIS-001 must have `passes: true` and produce a Consumer Impact Table in the progress file before FEAT-007 runs.
2. FEAT-007 reads that table BEFORE implementing and confirms: with `fallbackRunner.enabled = false`, the modification is byte-identical to today's behavior.
3. Semantic distinction (also documented in ANALYSIS-001): "Claude-tier escalation" (within-provider, existing) vs "Grok promotion" (cross-provider, new) — same function, different exit paths. They MUST NOT share counter semantics in a way that makes Grok promotion consume a Claude escalation step.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **milestones** run the full gate and must leave the repo fully green.

### Per-iteration scoped gate

```bash
# Scope tests to touched files
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test --lib loop_engine                            # most tasks
cargo test --lib loop_engine::<module> 2>&1 | tee /tmp/t.txt | tail -3 && grep -E 'FAILED|error\[' /tmp/t.txt | head -10
```

For tests in `tests/`, run the specific test file:
```bash
cargo test --test <test_file_basename>
```

**Do NOT** run the entire workspace test suite (bare `cargo test` with no filter) during regular iterations — that's the milestone's job.

### Milestone gate (MILESTONE-1 / -2 / -FINAL)

Milestones run the **full, unscoped** suite on a clean checkout and must finish green:

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
cargo run --bin gen-docs -- --check    # MILESTONE-1 onward — model constants doc sync
```

If ANY test fails (including pre-existing failures), the milestone fixes them. Default: attempt every failure. Escape hatch: >12 unrelated failures → spawn `FIX-xxx` task via `task-mgr add --stdin --depended-on-by <THIS-MILESTONE>` listing the failing tests, and `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production. Most common misses for THIS PRD:

- `dispatch` function not exported from `runner` module → call sites can't reach it
- `RunnerKind::Grok` arm panics with `unimplemented!` because FEAT-003 didn't replace it
- `provider_for_model` defined but never called from the iteration paths → effective_runner always defaults to Claude
- `fallback_runner` field on `ProjectConfig` not deserialized correctly (missing `#[serde(rename_all = "camelCase")]` or `#[serde(default)]`)
- `IterationContext::runner_overrides` defined but not initialized in `IterationContext::new` / `Default` impl
- `handle_prompt_too_long` signature gains new params but call sites at engine.rs:2532 and engine.rs:~1352 not updated
- Startup binary check defined but never called from `task-mgr loop start`
- `TaskMgrError::GrokAuthFailure` defined but the overflow + RuntimeError handlers don't pattern-match on it (cascades into generic IoError)
- Test fixtures hand-build `HashMap`s for IterationContext mock state — silently mask wrong field types
- `tasks.model` DB UPDATE in FEAT-006 not paired with override insert → next iteration's `resolve_task_model` reads stale Claude model and shadows the in-memory override

---

## Review Tasks

Review-type tasks spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

| Review                  | Priority | Spawns (priority)                  | Before            | Focus                                                                                              |
| ----------------------- | -------- | ---------------------------------- | ----------------- | -------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | MILESTONE-1       | Trait dispatch, provider routing, idempotency guards, `tasks.model` DB pairing, wave-mode wiring |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)             | MILESTONE-FINAL   | DRY between ClaudeRunner/GrokRunner, complexity, pattern adherence                                |

Use the **rust-python-code-reviewer** agent. Document findings in the progress file. Spawning follow-ups:

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-1: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 14,
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by MILESTONE-1
```

If no issues, emit `<task-status><REVIEW-ID>:done</task-status>` with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight — future iterations tail this.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block.

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw). **Do not Read those files directly.** Use:

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries

Record learnings with `task-mgr learn` (don't append directly to the files).

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

If verified: `<promise>COMPLETE</promise>`

### Blocked Condition

If blocked: document in progress file, create CLARIFY-xxx task with priority 0, commit, output `<promise>BLOCKED</promise>`.

---

## Milestones

Milestones are **full-gate checkpoints**: they prove the trunk is green before the next phase begins. NOT sweeping rewrites — stale tasks self-correct when their agent picks them up.

1. Check all `dependsOn` tasks have `passes: true`. If any don't, milestone can't run yet.
2. **Run the full quality gate** (Quality Checks § Milestone gate — complete test suite). This is the ONE place in the loop where the entire suite runs.
3. **Leave the repo green** — for every failure (including pre-existing): trivial → fix in milestone commit; non-trivial → spawn `FIX-xxx` via `task-mgr add --depended-on-by <THIS-MILESTONE>`.
4. Mark milestone done only when full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here.

- **[2031]** PromptTooLong recovery four-rung ladder contract: order of operations is contractual — ctx update → DB UPDATE → stderr → dump → JSONL → rotate. Rung 4 inserts at action-selection only; durability ordering unchanged.
- **[2852]** Wave + sequential share `handle_prompt_too_long` via shared call sites; any new rung works in both paths automatically because the function is shared.
- **[1856]** Per-task model escalation precedent: `IterationContext.model_overrides` HashMap keyed by task_id; written by overflow handler, read at runner-dispatch site. Mirror this exact shape for `runner_overrides`.
- **[1832]** `IterationContext` pattern for state tracking across iterations: contains `last_commit`, `last_files`, `crash_tracker`. Add new fields here.
- **[1810]** `IterationContext` is NOT thread-safe; wave-mode hooks MUST run on the main thread (post-aggregation), not inside `run_slot_iteration`. This is the #1 footgun for FEAT-007.
- **[2728]** Per-task `HashMap<String, T>` pattern for tracking state across iterations (e.g. `crashed_last_iteration`). `runner_overrides` is the same shape.
- **[1860]** Reorder hints in parallel context should buffer, not immediate-service. Same principle for RuntimeError hook: fire after slots merge back, not during.
- **[2286]** `iteration_pipeline` consolidates sequential + slot post-Claude processing. Reduces wave/sequential drift risk — but the RuntimeError hook is OUTSIDE that pipeline (lives in escalate_task_model_if_needed in engine.rs's main control flow).
- **[869]** Three-tier default resolution pattern (per-task → PRD → hardcoded). Runner selection mirrors: override → provider-from-model → Claude default.
- **[2800]** Named serde defaults enable multiple `ProjectConfig` fields — use this pattern for `FallbackRunnerConfig` defaults (provider/model/runtime_error_threshold).
- **[2366]** Default function pattern with serde: `#[serde(default = "default_*")]` paired with private `fn default_*() -> T`. Apply to `FallbackRunnerConfig`.
- **[2485]** camelCase JSON fields map to snake_case Rust via serde. `fallback_runner` (Rust) ↔ `fallbackRunner` (JSON); test the round-trip explicitly.
- **[2256]** `skip_serializing_if` for backward-compatible optional fields. Apply to `OverflowEvent.runner: Option<String>` so legacy event consumers don't see a new field on existing events.
- **[1989]** Trait-based resolver with configurable timeout and signal handling (`ClaudeMergeResolver`). Direct prior art for the `LlmRunner` trait — same shape (trait + impl + dispatch helper).
- **[2699]** Method extraction with responsibility-driven naming improves intent clarity. Apply when factoring shared code between ClaudeRunner and GrokRunner (signal handling, stdin write, env wiring).
- **[1992]** Short-circuit spawn before expensive subprocess invocation. Apply to GrokRunner binary check: probe binary at runner init, fail before subprocess spawn.
- **[656]** Non-loop `spawn_claude` callers (curate, learnings, watchdog) use `PermissionMode::Scoped { allowed_tools: None }` — they're explicitly OUT OF SCOPE for migration to `LlmRunner` per the PRD (FEAT-001 keeps them on the `spawn_claude` wrapper).
- **[1626]** Opt-in cleanup flag (`cleanup_title_artifact: bool`) threaded through spawn_claude signature. `GrokRunner` silently ignores this flag (no `--session-id` equivalent in grok).

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets are extracted from `src/loop_engine/CLAUDE.md` for the subsystems this PRD touches. Do NOT Read the full file.

**Overflow recovery and diagnostics** (this PRD extends to 5 rungs):

- The 4-rung ladder lives in `overflow::handle_prompt_too_long` in `src/loop_engine/overflow.rs`, called from the `PromptTooLong` arm in `engine.rs`.
- Rungs: (1) downgrade effort, (2) escalate sub-Opus, (3) escalate to 1M-context Opus, (4) block. This PRD inserts FallbackToProvider between 3 and 4.
- Rungs 1-3 reset task status to `todo` and clear `started_at` so the next iteration retries with the override applied; rung 4 (now rung 5 after this PRD) sets `blocked`.
- Order of operations is contractual (do not reorder): ctx update → DB UPDATE → stderr → dump → JSONL → rotate. Recovery state must be durable before any best-effort observability writes.
- Banner annotation: `(overflow recovery from <original-model>)` next to the model line. Gates on `IterationContext::overflow_recovered` (HashSet of task IDs), NOT on `model_overrides`. The original model is captured first-overflow only via `entry().or_insert_with(...)`.
- Crash escalation and consecutive-failure escalation must stay in their own channels (per learning #893).

**Iteration pipeline (shared)** — relevant for wave-mode wiring:

- Sequential (`run_iteration`) and parallel-wave (`run_slot_iteration` + `process_slot_result`) execution paths share a single post-Claude pipeline: `process_iteration_output` in `src/loop_engine/iteration_pipeline.rs`.
- Out of scope for the pipeline (kept at the call sites): wrapper-commit, external-git reconciliation, human-review trigger, rate-limit waits, pause-signal handling, slot merge resolution — AND the new RuntimeError fallback hook (FEAT-007 lives in the engine's main control flow, not the pipeline).

**Auto-launch /review-loop after loop end** — not directly modified by this PRD but worth knowing the loop completion flow.

**Parallel-slot scheduling** — relevant for wave-mode RuntimeError wiring:

- `IterationContext` is NOT thread-safe. Slot workers may only read context fields snapshotted into their own state (via `SlotPromptBundle`); they MUST NOT mutate context maps.
- Failed-merge accounting uses `Vec<FailedMerge>` (struct, not parallel arrays) to keep slot/task pairing as a type-level invariant.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures in this PRD. Use these exactly — do NOT guess key types from variable names.

### 1. ProjectConfig fallbackRunner — JSON to Rust round-trip

| Layer | Type | Field name |
|---|---|---|
| JSON on disk | string key | `"fallbackRunner"` (camelCase) |
| Rust struct | `Option<FallbackRunnerConfig>` | `fallback_runner` (snake_case) |
| Inner field | `bool` | `enabled` |
| Inner field | `String` | `provider` (default `"grok"`) |
| Inner field | `String` | `model` (default `"grok-4-fast"`) |
| Inner field | `Option<String>` | `cli_binary` (None = use `grok` on PATH) |
| Inner field | `u32` | `runtime_error_threshold` (default 2) |

Copy-pasteable access:
```rust
let cfg = read_project_config(db_dir)?;
if let Some(fr) = cfg.fallback_runner.as_ref() {
    if fr.enabled {
        // use fr.model, fr.runtime_error_threshold, fr.cli_binary
    }
}
```

JSON shape:
```json
{
  "fallbackRunner": {
    "enabled": true,
    "provider": "grok",
    "model": "grok-4-fast",
    "runtimeErrorThreshold": 2
  }
}
```

Absent key → `None`. Explicit `"fallbackRunner": null` → `None`. Both resolve to "fallback disabled" — never enabled-with-defaults.

### 2. Per-task runner resolution (SINGLE source of truth — compute ONCE per iteration)

| Layer | Type | Key |
|---|---|---|
| Source | `Option<String>` | `effective_model` (post-override) |
| Override map | `HashMap<String, RunnerKind>` | `task_id` → `RunnerKind` |
| Fallback | `enum Provider { Claude, Grok }` | computed by `provider_for_model` |
| Result | `enum RunnerKind { Claude, Grok }` | `effective_runner` |

Copy-pasteable access (call ONCE per iteration, use the result for spawn AND for idempotency guards):
```rust
let effective_runner = ctx.runner_overrides
    .get(&task_id)
    .copied()
    .unwrap_or_else(|| match provider_for_model(effective_model.as_deref()) {
        Provider::Grok => RunnerKind::Grok,
        Provider::Claude => RunnerKind::Claude,
    });

// Pass to spawn:
let result = dispatch(effective_runner, prompt, mode, opts)?;

// Pass to handle_prompt_too_long for idempotency guard (don't re-derive!):
overflow::handle_prompt_too_long(
    &mut ctx, conn, task_id, effort, effective_model.as_deref(),
    effective_runner,  // <-- pass the same value
    &prompt_result, iteration, run_id, base_dir, slot_index, &project_config,
);
```

### 3. Override-invalidation check (operator escape valve)

| Layer | Type | Key |
|---|---|---|
| DB column | `tasks.model` (nullable TEXT) | `task_id` |
| Snapshot | `HashMap<String, Option<String>>` | `overflow_original_task_model` |

Copy-pasteable check:
```rust
fn check_override_invalidation(
    ctx: &mut IterationContext,
    conn: &Connection,
    task_id: &str,
) -> rusqlite::Result<()> {
    if let Some(prev) = ctx.overflow_original_task_model.get(task_id).cloned() {
        let current: Option<String> = conn.query_row(
            "SELECT model FROM tasks WHERE id = ?1",
            rusqlite::params![task_id],
            |row| row.get(0),
        )?;
        if current != prev {
            ctx.runner_overrides.remove(task_id);
            ctx.model_overrides.remove(task_id);
            ctx.effort_overrides.remove(task_id);
            ctx.overflow_recovered.remove(task_id);
            ctx.overflow_original_model.remove(task_id);
            ctx.overflow_original_task_model.remove(task_id);
            eprintln!(
                "Operator changed task model for {task_id} — clearing auto-recovery overrides; resolving fresh."
            );
        }
    }
    Ok(())
}
```

### 4. OverflowEvent.runner serialization

| Layer | Type | Key |
|---|---|---|
| Rust struct | `Option<String>` | `runner` (skip_serializing_if = "Option::is_none") |
| JSON output | string | `"runner"` (snake_case) — OMITTED when None |
| Reader contract | `Option<&str>` | absence → treat as `"claude"` (default) |

Copy-pasteable reader pattern:
```rust
let runner = event.get("runner").and_then(Value::as_str).unwrap_or("claude");
```

This preserves legacy JSONL events (which never had this field) without forcing a schema migration.

### 5. tasks.model DB column + in-memory override pairing (CONTRACTUAL)

When either overflow rung 4 (FEAT-006) OR RuntimeError hook (FEAT-007) sets `runner_overrides[task] = Grok`, the SAME logical operation MUST also:

```rust
// 1. Snapshot the pre-fallback task model (FIRST time only)
ctx.overflow_original_task_model
    .entry(task_id.to_string())
    .or_insert_with(|| read_task_model_from_db(conn, task_id).unwrap_or(None));

// 2. Update in-memory overrides
ctx.runner_overrides.insert(task_id.to_string(), RunnerKind::Grok);
ctx.model_overrides.insert(task_id.to_string(), cfg.model.clone());

// 3. UPDATE the DB column so resolve_task_model agrees on next iteration
conn.execute(
    "UPDATE tasks SET model = ?1 WHERE id = ?2",
    rusqlite::params![&cfg.model, task_id],
)?;
```

**Why this matters**: `resolve_task_model` (model.rs:149) reads `task_row.model` at HIGHEST precedence. If steps 2 + 3 are not paired, on the next iteration the DB row still says `claude-opus-4-7`, that wins precedence, the in-memory override is silently shadowed, and the spawn dispatch routes back to Claude. This was the architect's #1 silent-bug class for this PRD.

---

## Important Rules

- Work on **ONE story per iteration**.
- **Commit frequently** after each passing story.
- **Keep CI green** — never commit failing code.
- **Read before writing** — always read files first.
- **Minimal changes** — only implement what's required by the task's acceptance criteria.
- **Check existing patterns** — `ClaudeMergeResolver` in `src/loop_engine/merge_resolver.rs` is the prior art for trait extraction.
- **FEAT-001 is byte-identical to today's spawn_claude behavior** — type aliases preserve all 10 call sites; do NOT refactor anything else in Phase 1.
- **FEAT-006 and FEAT-007 MUST pair the `tasks.model` DB UPDATE with the in-memory override insert** — see Data Flow Contracts §5. This is the most critical contract in the PRD.
- **FEAT-007 RuntimeError hook lives in post-wave aggregation (main thread), NEVER in `run_slot_iteration`** — IterationContext is not thread-safe (learning #1810).
- **`provider_for_model` uses token-equality on `-` splits, NOT substring matching** — `groq-llama-70b` (Groq Inc.) must classify as Claude, not Grok.
