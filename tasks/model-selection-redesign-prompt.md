# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Model-Selection Redesign — Provider-First Config, Capability Tiers, Multi-Provider Orchestration** for **task-mgr**.

## Problem Statement

Model selection in task-mgr has accreted into **5 overlapping config surfaces** (`defaultModel`, `reviewModel`, `primaryRunner` with 3 sub-maps, `fallbackRunner`) plus hardcoded Claude assumptions: `difficulty=high → OPUS_MODEL` and substring-based `ModelTier` classification. With Claude Fable 5 sitting above Opus, substring tier matching is structurally dead (`claude-fable-5` contains no `"opus"`), and the operator wants codex + grok running alongside Claude as first-class providers with a coherent, single-surface routing policy.

This PRD replaces all 5 surfaces with one provider-first `models` + `routing` config block (hard break), introduces capability tiers (`frontier` / `standard` / `cost-efficient` / `cheapest`) with an **anchor tier** + difficulty offset window, decouples effort per provider (codex via `-c model_reasoning_effort=` BEFORE `exec`), adds role-split + difficulty-spillover + quota-aware failover routing, redefines escalation ladders in tier terms, and lands provider stamping (migration v20).

**Scope cuts you must respect**: the multi-provider review cascade is DEFERRED to `tasks/prd-review-cascade.md` (do not build it; a premature `routing.reviewCascade` config key gets a "not yet supported" validation note). Engine-side structural frontier forcing is REMOVED (FR-011 generator guidance already shipped — do NOT implement any engine code for it).

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — the tier abstraction makes every future model launch a config edit instead of a code change) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- `resolve_execution_plan` (or any resolution-path function) writing `tasks.model` — escalation/promotion paths are the ONLY writers (escape-valve contract)
- `BlackoutState` persisted to disk/DB, or read/written by `promote_once` / `runner_overrides` paths
- Substring-based model→tier or model→provider matching anywhere — provider inference stays token-equality, tier lookup is config exact-match
- A second difficulty/effort normalizer — reuse the one `normalize_difficulty` everywhere
- Legacy alias tier keys (opus/sonnet/haiku) accepted in the new tiers map
- Codex inferred from a model string — Codex routing is config-explicit only (byIdPrefix route or taskClasses)
- Model-ID literals outside `src/loop_engine/model.rs` (no_hardcoded_models discipline)
- Raw UPDATE on `tasks.status` — all status writes via TaskLifecycle verbs
- Tests that only assert 'no crash' or check type without verifying content
- Tests that hand-build config maps not matching the FR-001 JSON schema — use production-shaped fixtures
- Error messages that don't identify what went wrong (CONFIG ERRORs must name the offending key and the accepted set)
- Catch-all error handlers that swallow context

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: Scoped tests pass per iteration; full `cargo test` green at milestones
- Rust: `cargo fmt --check` passes
- `cargo run --bin gen-docs -- --check` passes after any `src/loop_engine/model.rs` constant/table change
- All `tasks.status` writes via TaskLifecycle verbs — no raw UPDATEs
- Model-ID literals live only in `src/loop_engine/model.rs`
- No breaking changes to existing APIs unless explicitly required by the PRD hard break

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/model-selection-redesign.json)
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

If you genuinely need a top-level PRD field that's not surfaced per-task (rare), pull it with `jq`, never a full Read:

```bash
jq '.globalAcceptanceCriteria' tasks/model-selection-redesign.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/model-selection-redesign-prompt.md` | This prompt file (read-only)                                         |
| `tasks/progress-$PREFIX.txt`         | Progress log — **tail** for recent context, **append** after each task     |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration. Two targeted patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a dependsOn task you're building on)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing; never crash on absent files.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/model-selection-redesign.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes` — everything you need for the task. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `task-mgr next` listed a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, Key Context, and the CLAUDE.md excerpts that matter for this PRD) are already embedded in **this prompt file**. If a task cites a section name not shown here:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed (`feat/model-selection-redesign`). Switch if wrong.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section or grep 2-3 existing call sites. Never guess key types from variable names.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — and even then, one rejected alternative with a one-line reason is enough. For normal tasks: pick and go.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). Fix failures before committing; never commit broken code.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:` as appropriate). Multiple tasks per iteration: `feat: ID1-completed, ID2-completed - [Title]`.

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON.

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---` so the next iteration's tail works.

11. For TEST-xxx tasks: target 80%+ coverage on new methods; use `assert_eq!` on string outputs (exact model IDs, never `contains()`).

---

## Task Selection (reference)

`task-mgr next --prefix $PREFIX --claim` already picks: eligible tasks (`passes: false`, deps complete, not `requiresHuman`), preferring file-overlap with the previous task's `touchesFiles`, then lowest priority. You don't pick — you claim what it returns.

Two runtime checks you DO own:

- If the returned task has `preflightChecks`, run them. If any fails: `task-mgr skip <TASK-ID> --reason "<preflight failure>"` and re-run `task-mgr next`.
- If the previous task had a `completionCheck`, run it before starting the new one. If it fails: `task-mgr fail <prev-task> --error "completionCheck failed"` and fix it first.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

The PRD already contains the full consumer analysis (it was reviewed by production-code-architect; the Consumers and Semantic Distinctions tables are baked into each task's `consumerAnalysis`). So for this PRD: no separate ANALYSIS-xxx gate exists — instead, **honor the task's embedded `consumerAnalysis`**:

- `BREAKS` consumers → the task's own scope includes rewiring them (listed in mitigation).
- `NEEDS_REVIEW` consumers → read that call site BEFORE implementing and verify the mitigation in the same change.
- The Semantic Distinctions on each task are load-bearing — e.g. `provider_for_model` (token-equality, untouched) vs `tier_of` (config exact-match, new) are DIFFERENT functions with different matching rules. Never merge them.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **milestones** run the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (implementation / test / fix tasks)

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
cargo fmt --check
cargo check 2>&1 | tee /tmp/check.txt | tail -3 && grep "^warning\|^error" /tmp/check.txt | head -10
cargo clippy -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -3 && grep "^error" /tmp/clippy.txt | head -10
# Scope tests by module/test-file name from touchesFiles, e.g.:
cargo test resolve_execution_plan 2>&1 | tee /tmp/test.txt | tail -3 && grep "FAILED\|error\[" /tmp/test.txt | head -10
cargo test --test capability_tier_matrix 2>&1 | tee /tmp/test2.txt | tail -3 && grep "FAILED\|error\[" /tmp/test2.txt | head -10
```

Scoping heuristic: single-crate repo — scope by test-name filter or `--test <file>` matching the touched module. If you can't determine the scope confidently, widen (still cheaper than full suite). After touching `src/loop_engine/model.rs` constants/tables, ALWAYS also run `cargo run --bin gen-docs -- --check`.

**Do NOT** run the entire unfiltered `cargo test` during regular iterations — that's the milestone's job. Exception: REFACTOR-005 (deletion sweep) runs the full suite as its own completion check because deletions can strand any test file.

### Milestone gate (MILESTONE-1 / -2 / -FINAL)

Milestones run the **full, unscoped** suite and must finish green:

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings
cargo test 2>&1 | tee /tmp/test-results.txt | tail -3 && grep "FAILED\|error\[" /tmp/test-results.txt | head -10
cargo run --bin gen-docs -- --check
```

If ANY test fails — including pre-existing failures that predate this PRD — the milestone fixes them. Default: **attempt every failure**. Pragmatic escape hatch: if there are **more than ~12 failures AND they're all clearly unrelated to this PRD**, fix what's attributable inline, then spawn a single `FIX-xxx` task via `task-mgr add --stdin --depended-on-by <THIS-MILESTONE>` listing the failures, and `<promise>BLOCKED</promise>` with that task ID.

**Known false-failure mode** (learning #4753): mass `cli_tests`/`concurrent` failures whose errors all name a removed `…-slot-N` worktree path are stale shared-target binaries, NOT a regression. Fix: `touch tests/<binary>.rs` and re-run before spawning anything.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production — CODE-REVIEW-1 verifies. Most common misses:

- Not registered in dispatcher/router → new `models` CLI verbs must reach `src/main.rs` clap dispatch
- Test mocks bypass real wiring → verify `resolve_execution_plan` is on the PRODUCTION path of both prompt builders, not just test seams
- Config field read but not passed through → `ExecutionPlan` must thread through `SlotPromptBundle` / `resolve_effective_runner` input / wave scheduler (learning #4913: sequential/wave parity requires matching parameter threading)
- New DB column defined but not threaded into `TryFrom<Row>` / export mapping (v20 columns)
- Wrong key type on map access — JSON tier keys are kebab-case strings (`"cost-efficient"`); provider keys lowercase → check Data Flow Contracts
- Struct field added but construction sites missed (production + test helpers + fixtures) — learning #4915

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REFACTOR-REVIEW-FINAL`) spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

| Review                  | Priority | Spawns (priority)                  | Before            | Focus                                                                                                   |
| ----------------------- | -------- | ---------------------------------- | ----------------- | ------------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | MILESTONE-1       | Escape-valve contract, validator purity, single-home stamping, wiring reachable, no `unwrap()`, `qualityDimensions` met |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)             | MILESTONE-FINAL   | DRY (ONE anchor/tier derivation fn), complexity, coupling, clarity — full-context final pass             |

Use the **rust-python-code-reviewer** agent when reviewing code. Document findings in the progress file.

### Spawning follow-up tasks

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

`--depended-on-by` wires the new task into the milestone's `dependsOn` AND syncs the PRD JSON atomically — don't edit the JSON yourself. Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

---

## Progress Report Format

APPEND a block to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it **tight** — future iterations tail this; verbosity here bloats every later context.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block. If your entry is longer than ~25 lines, compress it.

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw). **Do not Read those files directly** — use `task-mgr recall --for-task <TASK-ID>` / `--query` / `--tag`. Record your own with `task-mgr learn` (1-2 lines each; group related tasks when reporting).

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in the progress file
2. Create clarification task (e.g., `CLARIFY-001` with priority 0) via `task-mgr add --stdin`
3. Commit: `chore: Add blocker task CLARIFY-001`
4. Output:

```
<promise>BLOCKED</promise>
```

Note: FEAT-006's codex spike is ALREADY RESOLVED (2026-06-09, pre-loop, codex-cli 0.136.0) — confirmed values are embedded in the task. Do not re-run the spike or block on it.

---

## Milestones

Milestones (MILESTONE-1 / MILESTONE-2 / MILESTONE-FINAL) are **full-gate checkpoints**: they prove the trunk is green before the next phase begins. They are NOT a sweep to rewrite remaining tasks — stale tasks self-correct when their agent picks them up.

### Milestone Protocol

1. Check all `dependsOn` tasks have `passes: true`. If any don't, the milestone can't run yet.
2. **Run the full quality gate** (Quality Checks § Milestone gate). This is the ONE place in the loop where the entire test suite runs.
3. **Leave the repo green.** For every failure, including pre-existing ones:
   - Trivial fixes go in the milestone's own commit: `chore: MILESTONE-N - fix stale test <name>`.
   - Non-trivial failures → spawn a `FIX-xxx` task via `task-mgr add --stdin --depended-on-by <THIS-MILESTONE>` with the failure's `verifyCommand`.
   - If the failure reveals that a remaining task in this PRD is stale or needs splitting, spawn the correction now — only in response to a concrete test failure, never a speculative sweep.
4. Mark the milestone `<task-status>MILESTONE-N:done</task-status>` only when the full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` (use `task-mgr recall --query <text>` for anything not here).

- **[3019/3736/3065]** Escape valve: `check_override_invalidation` (engine.rs) detects operator edits of `tasks.model` by snapshot comparison vs `ctx.overflow_original_task_model` and clears six override channels with one stderr line. The NEW case this PRD adds: anchor-resolved tasks record NULL as original — the comparison must still fire.
- **[4921/4672/4689]** `promote_once` owns the SINGLE `ctx.runner_overrides.contains_key` idempotency guard across all four promotion sites — never add a fifth site or second guard; test already-promoted → None explicitly. The blackout channel must never read/write overrides.
- **[3927]** Wave-mode `handle_no_eligible_tasks` historically went straight to the stale tracker on empty selection → false stale-abort. The deferral-first branch must be ordered BEFORE auto-recovery/stale logic, in BOTH paths.
- **[4171]** Wave-mode rate-limit reaction fires ONCE per wave (post-wave aggregation folds N slot outcomes), not per slot — hook blackout recording there.
- **[4913]** Sequential/wave parity requires matching parameter threading — every input influencing `resolve_execution_plan` must reach both prompt builders; pin with parity tests.
- **[4915]** Adding struct fields (ExecutionPlan threading, SlotPromptBundle) requires systematic construction-site discovery: production + test helpers + fixtures.
- **[4959]** Task-ID prefix matching uses dash-boundary token matching (`id_body_matches_prefix`), never substring — `classify_task` builds on it.
- **[4729]** `no_hardcoded_models` keeps model literals in `model.rs` only — extend its regex for fable in CONTRACT-001, never weaken it.
- **[4552/4983]** Codex protected-state guard (snapshot/verify-restore) wraps all codex iterations at exactly the mutation boundary sites — leave untouched.
- **[503/348/1550]** Migrations: exactly three edits in `migrations/mod.rs`; down migration is version-only column convention; test via `run_migrations`, not bare `create_schema`.
- **[4753]** Mass fixture-path test failures naming a removed `-slot-N` worktree = stale shared-target binaries → `touch tests/<binary>.rs`, recompile, re-run. Not a regression.
- **Deliberate deviation** from #4670/#4633 (read-time config normalizers): this PRD is an operator-confirmed HARD BREAK — delete migration machinery, don't normalize.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file.

- **Model IDs**: all Claude model constants + difficulty→effort mapping live in `src/loop_engine/model.rs` (`OPUS_MODEL` = claude-opus-4-8, `SONNET_MODEL` = claude-sonnet-4-6, `HAIKU_MODEL` = claude-haiku-4-5-20251001; this PRD adds `FABLE_MODEL` = claude-fable-5). After bumping/adding constants: `cargo run --bin gen-docs` regenerates `.claude/commands/tasks.md`; CI runs `-- --check`.
- **Fixtures**: JSON fixtures use `{{OPUS_MODEL}}`-style placeholders in `tests/fixtures/*.json.tmpl`, rendered by `tests/common/mod.rs::render_fixture_tmpl`. `tests/no_hardcoded_models.rs` blocks literal model strings outside `model.rs`.
- **Provider routing (current)**: Codex is reachable EXCLUSIVELY via config (`provider: "codex"`), NEVER inferred from a model string. `preflight_validate_and_probe` runs from BOTH `loop run` AND `batch run`. `protected_state` snapshots orchestrator files around every codex iteration. `CodexAuthFailure` is excluded from `handle_task_failure` at both callers.
- **Grok**: provider inference is token-equality (Groq ≠ Grok). The user's grok CLI only exposes `grok-build`. Startup binary check: enabled provider binary must resolve or the loop exits before the first iteration.
- **Lifecycle**: status mutation SSoT in `src/lifecycle/` — six verbs, five hard invariants; all `tasks.status` writes via TaskLifecycle verbs, no raw UPDATEs.
- **Output discipline (CONTRACT-LOG-001)**: `ui::*` for product UX / CLI data / byte-locked operator contracts; `tracing` for internal diagnostics only. New stderr warnings (legacy keys, default_model) go through `ui::emit_prefixed`-style surfaces, not `tracing`.
- **Reactions single-home contract**: new loop behaviors go through reactions with `#[deprecated]` leaf locks, exhaustive param-struct destructure (no `..`), hermetic `_inner` + injected seams. See `src/loop_engine/CLAUDE.md` (module-level docs auto-load when you read files there).
- **Worktrees**: loop runs in `$HOME/projects/task-mgr-worktrees/<branch>/`; per-worktree `.task-mgr/tasks.db`.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names.

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
| --- | --- | --- |
| config.json → `ModelsConfig.providers` → tier map | JSON string keys → `HashMap<String, ProviderConfig>` (provider name, lowercase) → `HashMap<String, Option<String>>` (tier kebab-case string key → model id) | `cfg.providers.get("claude").and_then(\|p\| p.tiers.get("cost-efficient"))` — note `cost-efficient` is kebab-case in JSON; `CapabilityTier::as_str()` must emit exactly `"cost-efficient"`, and the resolved layer converts to typed keys: `resolved.model_for(Provider::Claude, CapabilityTier::CostEfficient)` |
| task → `ExecutionPlan` → `RunnerOpts` | `Task.difficulty: Option<String>` (DB, lowercase) → `ExecutionPlan.effort: Option<String>` → `RunnerOpts.effort: Option<String>` | effort precedence at spawn: `ctx.effort_overrides.get(task_id).copied().map(String::from).or(plan.effort.clone())` — ctx override (overflow downgrade) wins |
| `routing.taskClasses.*.byDifficulty` | JSON string key (`"high"`) → `HashMap<String, Vec<String>>` (difficulty → provider names) | difficulty is matched lowercase-trimmed via the same `normalize_difficulty` used by effort lookup — do NOT add a second normalizer |
| result structs → provider stamping | `IterationResult.effective_runner: RunnerKind` / `SlotResult.effective_runner` → `tasks.completed_by_provider: TEXT` (lowercase provider name via `Provider::as_str`) | stamp in the completion arm of `process_iteration_output`; store `"claude"`/`"grok"`/`"codex"`, never model strings |

---

## Feature-Specific Checks

- **Resolution precedence (FR-003, 6 rungs)**: explicit `tasks.model` → `routing.byIdPrefix` → task class route (review/planning force frontier) → quota-blackout reroute (derived per pass, never stored) → anchor window (low→anchor−1, medium→anchor, high→anchor+1, clamped) → tier→model via `model_for`, effort LAST from the final provider's table. Name rungs by constant, not ordinal — the deferred cascade PRD inserts between rungs 1 and 2.
- **Escape-valve contract**: `resolve_execution_plan` NEVER writes `tasks.model`. Escalation/promotion paths (recovery.rs) remain the only writers. NULL-original semantics for anchor-resolved tasks.
- **Channel discipline**: `BlackoutState` newtype on `IterationContext` only — (1) never persisted (in-memory, clears on restart by design), (2) never read/written by `promote_once`/`runner_overrides` paths, (3) set only by account-level rate-limit signals, never task failures.
- **Codex constraints**: effort values ∈ {low, medium, high} — xhigh rejected by validation as a deliberate POLICY cap (spike-confirmed 2026-06-09: codex-cli 0.136.0 itself accepts `none|minimal|low|medium|high|xhigh`; the validation error message must say "by policy"); `-c model_reasoning_effort=<level>` positioned BEFORE the `exec` subcommand (confirmed valid there); codex pinning is route-only (`models route <prefix> --provider codex`) — `tasks.model` cannot express Codex.
- **After touching `model.rs` constants/tables**: `cargo run --bin gen-docs -- --check` in the same iteration.
- **Default sanity (PRD success metric)**: default config + anchor=standard → low=claude-sonnet-4-6, medium=claude-opus-4-8, high=claude-fable-5; anchor=cost-efficient → haiku/sonnet/opus.

---

## Important Rules

- Work on **ONE story per iteration**
- **For high-effort tasks** (`estimatedEffort: "high"`): consider using `/ralph-loop` to iterate within the task until all acceptance criteria pass
- **Commit frequently** after each passing story
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- **Scope cuts are binding** — no review-cascade engine code (deferred PRD), no structural frontier forcing (FR-011 already delivered as generator guidance)
