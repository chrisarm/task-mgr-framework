# Claude Code Agent Instructions

You are an autonomous coding agent implementing **LlmRunner Trait Hygiene — Phase 2: RunnerCapability + Dispatch Enforcement** for **task-mgr**.

## Problem Statement

The `LlmRunner` trait has two production implementations (`ClaudeRunner` at `src/loop_engine/runner.rs:240`, `GrokRunner` at `runner.rs:469`). Each runner's `spawn` method **silently destructures fields it does not support** with the `_` pattern — for example, `GrokRunner::spawn` at `runner.rs:492` reads:

```rust
let RunnerOpts {
    // ...
    use_pty: _,           // Claude-only PTY workaround; silently ignored on Grok
    // ...
} = opts;
```

This is a known footgun. Phase 1 (`cleanup_session` + `FakeRunner`) merged and removed the worst offender (`cleanup_title_artifact: _`). One silent destructure remains (`use_pty: _`), and any future capability-asymmetric field will leak the same way.

The engine has the inverse footgun: `engine.rs:5044` hard-codes `if effective_runner != RunnerKind::Claude` for what is sometimes a capability check dressed as identity. As soon as a third runner is added with similar semantics, branches like this silently break.

This PRD adds a typed `RunnerCapability` enum, an `LlmRunner::supports` trait method (default `false`), and dispatch enforcement that refuses any `RunnerOpts` field encoding an unsupported capability with a new `TaskMgrError::UnsupportedRunnerCapability` error variant. Both production runners use **exhaustive** `supports` matches (no wildcard arms) so adding a new variant forces a per-runner decision at compile time. The PRD also audits every `RunnerKind` match in `src/` and migrates at least one `CAPABILITY-MISLABELED` branch if any qualify. The capability surface is the foundation Phases 3-5 (error taxonomy, args builder, RAII session tracking) build on.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate the registry table shape) → **PHASE 2 FOUNDATION** (exhaustive `supports()` matches are the foundational architectural choice — a wildcard arm here defeats the entire PRD) → **FUNCTIONING CODE** (dispatch enforcement is fail-closed) → **CORRECTNESS** (compiles, type-checks, per-capability test matrix passes) → **CODE QUALITY** (qualityDimensions satisfied, error messages identify all three fields) → **POLISH** (CLAUDE.md, design doc retrospective).

Non-negotiables: capability is a typed surface, never an ad-hoc string. `supports()` default returns `false` (safe direction). Production runners use **EXHAUSTIVE** matches without wildcard arms. Capability enforcement is field-presence, not value-validity. KIND-CORRECT vs CAPABILITY-MISLABELED is a deliberate distinction — provider-identity branches stay as `RunnerKind` matches.

**Prohibited outcomes:**

- Adding a wildcard arm (`_ => ...`) in any production `LlmRunner::supports` impl — exhaustive matches are the entire forcing function for future-variant safety
- Allowing a capability-driven `RunnerOpts` field to remain destructured as `<field>: _,` in any production runner — the grep lint must reject this
- Returning `UnsupportedRunnerCapability` for a `RunnerOpts::default()` call on any runner — only NON-DEFAULT capability-driven field values trigger enforcement
- Introducing `Box<dyn LlmRunner>` on the hot path; static-dispatch `RunnerKind` stays the spawn boundary
- Removing or altering the `WORKAROUND()` markers established by Phase 1
- Adding `RunnerCapability` variants without a corresponding entry in the `checks` enforcement table (or a documented reason why the variant has no field to check)
- Migrating a KIND-CORRECT branch in `engine.rs` to `runner.supports(cap)` — the distinction matters; provider-identity branches stay
- Using `String` or `Box<str>` for the `capability_name` / `field_name` fields in `UnsupportedRunnerCapability` — both are static; allocation here would be a smell
- Tests that only assert `dispatch` returns `Err` — must verify the specific error variant AND the `capability_name` + `field_name` strings
- Tests that hand-build `RunnerOpts` via `HashMap` or similar — must construct the real `RunnerOpts` struct so a wrong field name fails to compile

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: `cargo fmt --check` passes
- Rust: `cargo check --all-targets --all-features` passes with no new warnings
- Rust: `cargo clippy -- -D warnings` passes
- Rust: Scoped tests for touched files pass with `cargo test`
- No new `.unwrap()` / `.expect()` in production paths
- Error messages include enough context to identify the offending call site (`runner_kind`, `capability_name`, `field_name`)
- Static-dispatch `RunnerKind` path preserved on every spawn boundary (no `Box<dyn LlmRunner>` on hot path)
- Static `checks` registry table is the SINGLE place that maps `RunnerOpts` fields to `RunnerCapability` variants
- Comments explain WHY (capability surface contract, fail-closed rationale), never narrate WHAT or restate the enum

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation is already embedded in **this prompt file** — that is the authoritative copy.

### Getting your PRD's task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/01-runner-capability-enforcement.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task    | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` — priority auto-computed; DB + PRD JSON updated atomically                                                     |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task:

```bash
jq '.requires' tasks/01-runner-capability-enforcement.json
jq '.globalAcceptanceCriteria' tasks/01-runner-capability-enforcement.json
```

### Files you DO touch

| File                                              | Purpose                                                                |
| ------------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/runner-capability-enforcement-prompt.md`   | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`                      | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log. Two patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. ANALYSIS-001's inventory that FEAT-006 consumes)
grep -n -A 40 '## .* - ANALYSIS-001' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist).

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/01-runner-capability-enforcement.json)
   task-mgr next --prefix $PREFIX --claim
   ```

2. **Pull only the progress context you need** — most iterations want just the most recent section. FEAT-006 specifically needs ANALYSIS-001's inventory — grep for that section by name.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns learnings scored highest for this specific task. **Do not** Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly.

   **Never Read `CLAUDE.md` in full.** Grep for specific subsystems:
   ```bash
   grep -n -A 10 'LLM runner dispatch' src/loop_engine/CLAUDE.md
   grep -n -A 10 'Provider routing' src/loop_engine/CLAUDE.md
   ```

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed. Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - For FEAT-003 (`modifiesBehavior: true`): one rejected alternative + one-line reason.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below). Fix failures before committing.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`test:` as appropriate).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON.

10. **Append progress** — ONE post-implementation block, terminated with `---` so the next iteration's tail works.

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` picks: eligible tasks (`passes: false`, deps complete, not `requiresHuman`), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

**FEAT-003 is the only `modifiesBehavior: true` task in this PRD.** It changes `dispatch()`'s return shape by adding a new error variant. The ANALYSIS-001 gate is the safety net: it confirms ZERO production call sites today set a capability-driven field on a runner that doesn't support it. If ANALYSIS-001 found exceptions, FEAT-003 cannot ship without addressing them first.

Per-context handling (per the PRD §3.5):
- If ANALYSIS-001 found a production call site that would be rejected: **BLOCKED** until the call site is fixed (spawn a CLARIFY-xxx or FIX-xxx task)
- Otherwise: proceed with FEAT-003 as designed

---

## Quality Checks

### Per-iteration scoped gate

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
# Most FEAT tasks here touch src/loop_engine/runner.rs — scope accordingly
cargo fmt --check
cargo check                                                  # fast type check
cargo clippy -- -D warnings
cargo test -p task-mgr loop_engine::runner                   # scoped to runner module
cargo test -p task-mgr <specific_test_fn>                    # narrower if needed
```

Scoping heuristic: this PRD touches `src/loop_engine/runner.rs`, `src/errors.rs`, and minor parts of `src/loop_engine/engine.rs`. `cargo test -p task-mgr` filtered to the relevant module is usually right. The grep lint test (FEAT-005) lives in `tests/` and runs as part of `cargo test`.

**Do NOT** run the entire workspace test suite during regular iterations — that's REVIEW-001's job.

### Final gate at REVIEW-001 (the milestone)

REVIEW-001 runs the **full, unscoped** suite on a clean checkout AND verifies the capability contract end-to-end (every (runner × capability) pair via dispatch + FakeRunner). The full suite must finish green, including pre-existing failures (trunk-green is the invariant).

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

If more than ~12 pre-existing failures are clearly unrelated to this PRD, spawn one `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001` and BLOCKED until resolved. Below that threshold, fix inline.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

- New `RunnerCapability` variant added without a row in the `checks` table → field silently no-op at dispatch
- New `RunnerCapability` variant added without updating BOTH `ClaudeRunner::supports` and `GrokRunner::supports` → compile error (this is intentional and the forcing function)
- Wildcard arm `_ => ...` added to a production `supports()` impl → defeats Phase 2's foundational guarantee; CODE-REVIEW-1 finding
- `String` / `Box<str>` used for `capability_name` / `field_name` in `UnsupportedRunnerCapability` → unnecessary allocation; should be `&'static str`
- `Box<dyn LlmRunner>` introduced on the hot path → static-dispatch `RunnerKind` is the path; only `enforce_capabilities` uses a brief `&dyn LlmRunner`
- `<field>: _,` destructure pattern reintroduced for a capability-driven field → FEAT-005's grep lint catches this
- KIND-CORRECT branch in `engine.rs` migrated to `runner.supports(cap)` → semantic error (provider-identity branches must stay as `RunnerKind` matches)

---

## Review Tasks

This PRD uses the lean review path:

| Review                  | Priority | Spawns (priority)                  | Focus                                                                                                |
| ----------------------- | -------- | ---------------------------------- | ---------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | Exhaustive `supports()` matches, static lifetimes, no `Box<dyn LlmRunner>`, audit honesty             |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)             | DRY between checks table and supports() impls, contract fidelity, Phase 4 readiness                   |
| REVIEW-001              | 99       | `FIX-xxx` (only if escape-hatch fires) | Full unscoped quality suite + capability matrix end-to-end test + exhaustive-match scratch verification |

Use the `rust-python-code-reviewer` agent for substantive review. Spawning template:

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
  "touchesFiles": ["src/loop_engine/runner.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001
```

`--depended-on-by` wires the new task into the milestone's `dependsOn` AND syncs the PRD JSON atomically. If no issues found, emit the status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt`. Keep it tight.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

**ANALYSIS-001 special format:** dump the full audit inventory under a clear `## ANALYSIS-001 — dispatch audit + RunnerKind classification` header so FEAT-006 can grep + consume it. Format each row as `file:line | KIND-CORRECT | <one-line rationale>` (or `CAPABILITY-MISLABELED | <which capability + why>`).

Target: ~10 lines per non-ANALYSIS block.

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify REVIEW-001 passes (full suite + capability matrix end-to-end)
3. Verify no new tasks were created in final review

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked:

1. Document blocker in the progress file
2. Create clarification task via `task-mgr add --stdin`
3. Output:

```
<promise>BLOCKED</promise>
```

---

## Milestones

This PRD has one milestone: **REVIEW-001** (priority 99). It runs the full unscoped quality suite AND verifies the capability contract end-to-end. Trunk-green is the invariant.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md`.

- **[Phase 1 — 8cc50ff5-FEAT-008]** — FakeRunner test seam introduced under `#[cfg(test)]`; extended by this PRD with `supports_fn: fn(RunnerCapability) -> bool`. Reuse the seam; don't introduce a parallel test runner.
- **[Phase 1 — cleanup_title_artifact removal]** — silent destructure patterns leak provider asymmetry through the wrapper's API surface. The structural fix is at the trait, not at the call site. This PRD's `RunnerCapability` is the next layer of the same lesson.
- **[2956]** — `RunnerKind` enum static dispatch keeps allocation-free; no `Box<dyn LlmRunner>` on the hot path. The brief `&dyn LlmRunner` used inside `enforce_capabilities` is zero-allocation and one indirect call — acceptable; allocation is not.
- **[2891]** — extract common subprocess scaffolding immediately when adding the second agent implementation. Applies one level up: extract capability checks immediately rather than per-runner `_` destructures.
- **[grok auth-failure detection]** — provider-specific stderr-sniffing (`GROK_AUTH_FAILURE_SUBSTRINGS`, `stderr_contains_auth_failure`) is KIND-CORRECT (provider identity), not capability-mislabeled. Stays as `RunnerKind` match.
- **[Empty/whitespace env var fall-through]** — `$GROK_BINARY = ""` is treated as unset. Same principle applies to `effort: Some("")` and `disallowed_tools: Some("")` — they are "no opinion" values, not capability-driven, and must NOT trigger enforcement.
- **[Transactional ctx writes are deferred]** — `RuntimeError` fallback hook runs DB updates + ctx mutations as a pair, with ctx mutation deferred to after `tx.commit()?`. Not directly relevant to capability surface but a useful pattern reference for any future enforcement that combines DB + ctx work.
- **[Single-source-of-truth drift sentinels are `assert!` not `debug_assert!`]** — applies to the `checks` table here: if the table ever drifts from the `supports()` declarations, that's a silent capability mis-categorization. Phase 4's compile-time assertion forecloses this; for Phase 2, the test matrix catches it.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets are extracted from `src/loop_engine/CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work.

**LLM runner dispatch (current state — this PRD extends this):**

> The loop engine dispatches every LLM subprocess through `runner::dispatch(kind, prompt, permission_mode, opts)` at `src/loop_engine/runner.rs`. `kind: RunnerKind` is a static-dispatch enum match (no `Box<dyn LlmRunner>` on the hot path). `ClaudeRunner` is the default; `GrokRunner` is the fallback when `fallbackRunner` is configured. The trait `LlmRunner: Send + Sync` exists for testability and clean separation; production dispatch is the free function.

**Provider routing — `model::provider_for_model`** (KIND-CORRECT example):

> Classifies a model id as `Provider::Claude` or `Provider::Grok` via **token equality on `-` splits of the lowercased id**. Substring matching is explicitly prohibited (would mis-route Groq Inc. models). This is the SINGLE source of truth for spawn-site dispatch discriminant — provider identity, not capability. Stays as `RunnerKind` match in any consuming code.

**Grok auth-failure detection** (KIND-CORRECT example):

> `runner.rs::GROK_AUTH_FAILURE_SUBSTRINGS` + `stderr_contains_auth_failure` rely on a small set of case-insensitive substrings matched against captured stderr. This is Grok-specific stderr-sniffing — provider identity, not capability. Stays as a provider-specific code path.

**Operator escape valve — `check_override_invalidation`** (out of scope but adjacent):

> Compares current `tasks.model` against `overflow_original_task_model[task_id]` snapshot. Provider-independent; not a capability concern.

**Engine.rs:5044 (the Grok-fallback hook gate):**

> `if effective_runner != RunnerKind::Claude { return Ok((escalated, None)); }` in `escalate_task_model_if_needed_inner`. PRD §6 analysis: KIND-CORRECT — the Grok runtime-error fallback hook fires when current runner is Claude because Grok IS the fallback target. Promoting Grok to "fallback for itself" is nonsense. Stays as `RunnerKind` match. ANALYSIS-001 verifies this conclusion.

**Touchpoints (`src/loop_engine/CLAUDE.md`):** Phase 2 adds a "Capability surface" subsection; the row for `LLM runner dispatch` is updated to mention `RunnerCapability` + `supports` + `enforce_capabilities`.

---

## Data Flow Contracts

N/A for this PRD. `RunnerCapability` and `RunnerOpts` are consumed within `src/loop_engine/runner.rs` exclusively (plus tests). The new error variant `UnsupportedRunnerCapability` flows through `TaskMgrError` like any other variant — no cross-module key-typing concerns.

---

## Important Rules

- Work on **ONE task per iteration**
- **No `/ralph-loop` needed** — every task in this PRD is `low` or `medium` effort
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- **Check existing patterns** — see CLAUDE.md excerpts above; reuse Phase 1's FakeRunner seam
- **Exhaustive matches are the architectural choice** — wildcard arms in production `supports()` impls defeat the entire PRD. If clippy or a linter suggests adding one, refuse.
- **Boundary contract with coherence-refactoring engine-orchestration PRD**: that PRD will carve `engine.rs`. If you see comment annotations from this PRD's FEAT-006 in a hunk that PRD wants to move, preserve them. Coordinate via reviewer overlap.
