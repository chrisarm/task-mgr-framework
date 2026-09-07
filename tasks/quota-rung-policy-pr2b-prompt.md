# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Quota rung policy PRE-PR-3 (2b gate: extra-mark identity, wait-probe, Stop split)** for **task-mgr**.

## Problem Statement

PR-2 shipped generic `QuotaBucket` ingest, remaining-percent as the gate unit, factory `tierFallback` unavailable (not ask), proto-channel HashSet replace-on-evaluate, and `handle_rung_only_empty_selection`. Landed extra-mark still keys on `exact_model_for(mapped_rung)`: after `set-tier claude frontier <opus>`, a **Fable** HUD row extra-marks **standard** (the pin is harmful). Named `seven_day_opus` / `seven_day_sonnet` family-token-map onto standard / cost-efficient. Scoped 6h Wait lifts on week 45% left because `account_quota_preflight` builds `usage_suggests_lifted` **before** apply. Sequential `AccountReaction::Stop` maps to `RateLimit` + `operator_stopped: false` (exit 1); wave maps every Stop to exit **130**. `extra_usage` dollars 0 still Stops via `amount_exhausted` AccountLow. `has_review` uses `id.contains("REVIEW")`. Pin docs still say required for parallel/wave.

PRE-PR-3 is a **gate between PR-2 and PR-3, not a fourth product PR**. Stories: US-008–US-012 / FR-009–FR-011 / CONTRACT-002 in `tasks/prd-quota-rung-policy.md`. Sidecar `tasks/quota-rung-policy-PR-3-review.md` is **superseded**.

Pins (do not rewrite):

1. A low **frontier** (HUD: “Current week (Fable)”) bucket is **not** an account emergency — continue on **standard** (and cheaper rungs).
2. Engine language is **capability rungs** (`frontier` / `standard` / `cost-efficient` / `cheapest`), never model ids (`fable`, `opus`, `claude-fable-5`) except at the ingest adapter that maps an API label onto a rung.
3. Default action is a **horizon heuristic** (all config.json-overridable): **wait** if reset is within the next hour; **stop** if reset is more than 12 hours away *and* no other rung can run; **ask** if other rungs still work but no downgrade instruction exists. `--use-other-models-ttl <minutes>` (0 allowed) is how long `ask` waits for a human before continuing on working rungs — **that flag is PR-3, not this gate**.

---

## PRE-PR-3 scope lock (read every iteration)

**Implementation SSoT is worktree** `/home/chris/Documents/startat0/Projects/task-mgr-worktrees/feat-quota-rung-policy-pr2/` **/** `main` `dacccb3` — **not** a `feat/quota-rung-policy-pr1` checkout (that tree has no `quota.rs`). Relocate every consumer line number on that tree. Extend, do not rewrite: `evaluate_quota` / `apply_quota`, remaining-percent, factory `tierFallback`, proto-channel HashSet replace-on-evaluate, `HorizonStopped`, `handle_rung_only_empty_selection`, Fable 3600 skip of probe, CODE-FIX-003 includeForced not global, dual predicates.

In scope: HUD-family extra-mark identity **union**; unlabeled named `seven_day_*` → `rungs: None`; live-shaped evaluate+apply + snapshot/exclude; `Wait { secs, account_binding }` + `wait_probe_lifted` after apply; `AccountReaction::{OperatorStopped, StopSpend}`; `has_review = is_frontier_class`; extra_usage Ignore at `evaluate_one` before amount-exhausted AccountLow; remaining-min `> 100` at loop/batch preflight; post-output banner via `react_to_outputs` closure over `AccountReactionParams.models`; pin-optional docs.

**Out of scope (do not implement, do not spawn as “helpful” follow-ups):**

- `--use-other-models-ttl` clap on `loop run` / `batch run` (FR-005)
- TTL > 0 re-evaluate on stop-check cadence
- `resolve_execution_plan` down-only walker / post-resolve clamp / expiry map / `active_rungs` (FR-006)
- Family-match of explicit `tasks.model` at resolve time
- Inherit / `account_quota_stopped` / `batch --chain` discriminator
- `models set-usage-rule` / `set-tier-fallback` / `models show` live remaining (FR-008 / US-007)
- Automatic clamp of all-high / review onto standard (still PR-3)
- Dated month-name `parse_reset_from_output` as the **account** wait for a rung-scoped bucket
- Re-folding scoped windows into an account wait
- Live Anthropic / real `~/.claude` credentials
- Editing `tasks/quota-rung-policy-pr3.json` or `tasks/quota-rung-policy-PR-3-review.md`

Do **not** edit `tasks/quota-rung-policy.json`, `tasks/quota-rung-policy-pr1.json`, `tasks/quota-rung-policy-pr2.json`, `tasks/quota-rung-policy-pr3.json`, or `tasks/prd-quota-rung-policy.md`.

Landed code already shipped — do not reimplement: `evaluate_quota`/`apply_quota`, remaining-percent, factory `tierFallback`, proto-channel HashSet replace-on-evaluate, `HorizonStopped`, `handle_rung_only_empty_selection`, Fable 3600 skip of probe, CODE-FIX-003 includeForced not global, dual predicates.

HUD tokens (`fable` / `opus` / `sonnet` / `haiku` as API labels) live **only** in `usage.rs`. Split a task if it would exceed 10 files or 12 ACs.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). If a **Project Verification Skills** section applies to this task, follow that skill after the language gate. Don't add a separate self-critique step; the linters, type-checker, targeted tests, and (when present) the project verification skill catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Extra-mark via `exact_model_for(mapped_rung)` after an operator pin (Fable HUD extra-marks standard)
- Preferring `scope.model.id` over the family constant (kills Opus+pin extra-mark on snapshot ids)
- A single extra-mark `identity: &str` of “id or family constant” instead of an identity set union
- Family-token-mapping unlabeled `seven_day_opus` / `seven_day_sonnet` onto standard / cost-efficient
- Apply-only live-shaped fixture without snapshot/exclude tests (architect rev 5)
- Widening post-output `WaitFn` or `UsageGateFn` (must stay `Fn(u64)` and `(u8, &Path, u64)`)
- Building `usage_suggests_lifted` before apply as the scoped Wait probe (landed `account_quota_preflight` known-bad)
- Scoped-only Wait lifting on week 45% left / `UsageInfo.percentage`
- Dropping `extra_usage` from `is_spend_kind` without Ignore at `evaluate_one` before amount-exhausted AccountLow
- Sequential `OperatorStopped` mapped onto `RateLimit` + `operator_stopped` (does not set `was_stopped`)
- Wave `StopSpend` or `OperatorStopped` as exit 130
- `has_review` via `id.contains("REVIEW")` (false-positive `REFACTOR-REVIEW-FINAL`; false-negative `MILESTONE-FINAL`)
- `remainingMinPercent` / `LOOP_USAGE_REMAINING_MIN` > 100 silently accepted at loop/batch preflight
- Documenting pin as required for parallel/wave after this slice (optional, not required; all-high clamp is PR-3)
- HUD tokens (`fable`/`opus`/`sonnet`/`haiku`) outside `usage.rs`
- `quota.rs` or `engine.rs` proto-channel keys containing `fable` / `opus` / `claude-fable-5`
- Implementing `--use-other-models-ttl`, FR-006 expiry + down-only walker, inherit, `account_quota_stopped`, FR-008 `set-usage-rule` / `set-tier-fallback`, or `models show` live remaining
- Reusing `handle_quota_deferral` for rung-only exhaustion
- Collapsing dual Anthropic I/O predicates onto one flag
- Undoing Fable CLI 3600 skip of `usage_gate` / `probe_rate_limit_lifted`
- `quota.rs` importing runners, clap, or SQLite
- Tests that require live Anthropic network or real `~/.claude` credentials
- Driving `loop run` / `batch run` (verify-task-mgr refuses; `loop run --help` is the safe probe)
- Inventing a second verification harness
- Manual edits to `tasks/*.json` for status (use task-mgr CLI / task-status tags)
- `unwrap()` in production paths
- Catch-all error handlers that swallow context
- Editing `tasks/quota-rung-policy-pr3.json` or `tasks/quota-rung-policy-PR-3-review.md`

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy -- -D warnings` (per-iteration). `--all-targets` is REVIEW-001 only
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() in production code paths
- Sequential and wave paths stay parity-locked for account reactions (exhaustive destructure, shared coordinators)
- quota.rs and engine.rs proto-channel keys contain no model-id literals (frontier/standard/cost-efficient/cheapest only)
- HUD tokens (fable/opus/sonnet/haiku as API labels) live only in usage.rs
- Post-output WaitFn stays Fn(u64); UsageGateFn stays (u8, &Path, u64)
- No --use-other-models-ttl / FR-006 clamp / inherit / FR-008 CLI in this gate
- No live Anthropic / real ~/.claude credentials in tests

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Project Verification Skills, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

Do **not** edit `tasks/quota-rung-policy.json`, `tasks/quota-rung-policy-pr1.json`, `tasks/quota-rung-policy-pr2.json`, `tasks/quota-rung-policy-pr3.json`, `tasks/quota-rung-policy-PR-3-review.md`, or `tasks/prd-quota-rung-policy.md`.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/quota-rung-policy-pr2b.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Inspect this iteration's task          | `task-mgr show <TASK-ID>` using the task ID from `## Current Task`                                                                                                                 |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2b.json` — priority auto-computed; DB + PRD JSON updated atomically                                                   |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task (rare), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/quota-rung-policy-pr2b.json
jq '.globalAcceptanceCriteria' tasks/quota-rung-policy-pr2b.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/quota-rung-policy-pr2b-prompt.md`   | This prompt file (read-only)                                               |
| `tasks/progress-$PREFIX.txt` | Progress log — **tail** for recent context, **append** after each task     |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration. Two targeted patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on, or a dependsOn task)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist). Before appending, create it with a minimal header if missing; never crash on absent files.

---

## Your Task (every iteration)

Optimize for context economy: pull only what's needed, don't dump whole files.

1. **Work the task in `## Current Task`** — the loop engine already selected and claimed it at iteration start. Use `task-mgr show <TASK-ID>` only if you need to inspect the pinned task details again. If `## Current Task` says there is no eligible task or unmet cross-PRD `requires`, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `## Current Task` lists a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log (`grep -n -A 40 '## .* - <THAT-TASK-ID>' tasks/progress-$PREFIX.txt`). Skip entirely on the first iteration (file won't exist). CONTRACT dependents **must** grep `## CONTRACT-002` from the progress log and implement against that text.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, or the task touches a file that's likely documented there, `grep` for the relevant term and read only the surrounding lines:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```
   The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, Project Verification Skills, Key Context, and the CLAUDE.md excerpts that matter for this PRD) are already embedded in **this prompt file**. Prefer it over re-reading source docs. When a verification skill applies, Read that SKILL.md at verification time — do not paste it into the progress log.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed (`feat/quota-rung-policy-pr2b`). Switch if wrong. Base this work on the PR-2 tree (`feat-quota-rung-policy-pr2` / `main` `dacccb3`), not `feat/quota-rung-policy-pr1`.

5. **Think before coding** (in context, not on disk):
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section or grep 2-3 existing call sites. Never guess key types from variable names.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — and even then, one rejected alternative with a one-line reason is enough. For normal tasks: pick and go.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite). If a **Project Verification Skills** entry covers this task, Read that SKILL.md and follow it after the language gate; do not invent a second harness. A green compile/test run is not proof for covered user-facing changes. If the skill is blocked (can't launch, unmet precondition), emit `<promise>BLOCKED</promise>` rather than marking the task done. Fix failures before committing; never commit broken code.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:` as appropriate). Multiple tasks per iteration: `feat: ID1-completed, ID2-completed - [Title]`.

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>` — the loop engine flips `passes` and syncs the PRD JSON. Do NOT edit the JSON. (Legacy `<completed>TASK-ID</completed>` still works; prefer `<task-status>`.)

10. **Append progress** — ONE post-implementation block, using the format below, terminated with `---` so the next iteration's tail works.

11. For TEST-xxx tasks: target 80%+ coverage on new methods; use `assert_eq!` on string outputs.

---

## Task Selection (reference)

The loop engine owns selection and claim at iteration start. It injects the claimed task into `## Current Task`; work only that pinned task during this iteration.

To request a different pick on the **next** iteration, emit `<reorder>TASK-ID</reorder>`. The engine will claim that task on the next iteration. Never combine reorder with `next --claim`.

Two runtime checks you DO own:

- If `## Current Task` has `preflightChecks`, run them. If any fails: emit `<task-status><TASK-ID>:skipped</task-status>` with the preflight reason and stop this iteration; the engine will pick the next task on the next iteration.
- If the previous task had a `completionCheck`, run it before starting the new one. If it fails: `task-mgr fail <prev-task> --error "completionCheck failed"` and fix it first.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

1. **ANALYSIS gate**: FEAT-008, FEAT-009, FIX-010, FIX-011, and FIX-012 already carry `consumerAnalysis` in the task JSON / this prompt's Data Flow. Do not spawn a separate ANALYSIS task.
2. **Consumer Impact Table**:
   - `BREAKS` → implement the mitigation named on the task. Split only if two semantic contexts truly need different code paths.
   - `NEEDS_REVIEW` → verify the call site before changing it.
   - `OK` → proceed.
3. **Semantic distinctions**: Fable HUD extra-mark vs Opus HUD extra-mark; scoped Wait probe vs account-binding Wait probe; OperatorStopped vs StopSpend — do not shoehorn them into one path.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **milestones** run the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (implementation / test / fix tasks)

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
# Rust — scope tests to the touched crate/module (grep touchesFiles to pick)
cargo fmt --check
cargo check                                         # fast type check
cargo clippy -- -D warnings
cargo test --lib quota usage
cargo test --lib account
cargo test --lib project_config
cargo test --test reaction_parity <filter>          # only when tests/reaction_parity.rs is in touchesFiles
```

Scoping heuristic: start from `touchesFiles`. For each Rust file, run `cargo test` filtered to that module. If you can't determine the scope confidently, widen to the crate (still cheaper than the full workspace).

**Do NOT** run the entire workspace test suite (`cargo test` with no filter) during regular iterations — that's REVIEW-001's job.

**Project verification skill:** if this prompt has a **Project Verification Skills** section, run it after the language gate for covered tasks (see that section). Compile/unit tests alone are not proof for those changes.

### Final gate at REVIEW-001 (the milestone)

The single `REVIEW-001` task at the end runs the **full, unscoped** suite on a clean checkout and must finish green. There are no separate MILESTONE-1 or MILESTONE-2 tasks.

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

If ANY test fails — including pre-existing failures that predate this PRD — the milestone fixes them. Default: **attempt every failure**, even ones that look out-of-scope. They become scope the moment the milestone gates the phase on the full suite being green. Trunk-green is the invariant this mechanism exists to protect.

Pragmatic escape hatch: if there are **more than ~12 failures AND they're all clearly unrelated to this PRD**, don't try to do all of them inline. Triage:

1. Fix everything you can attribute to this PRD's changes, inline in the milestone commit.
2. For the remaining unrelated failures: spawn a single `FIX-xxx` or `CLARIFY-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2b.json` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID so a human can route ownership.

Below the ~12-failure threshold, just fix them.

If a **Project Verification Skills** section is present, this gate also includes that skill's mapped-feature drive (see that section).

---

## Project Verification Skills

This repo ships a project-level verification skill. Language-level gates (fmt, type-check, lint, scoped tests) are **necessary but not sufficient** for user-facing changes the skill covers. Follow the skill literally — do not invent a second harness, and do not paste the skill body into the progress log.

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** `models-show` (DOCS-001, FIX-012, REVIEW-001: text only, no remaining percents) and `loop-run-help` (every behavior-touching task + REVIEW-001: `$H capture loop-help -- loop run --help` must NOT list `--use-other-models-ttl`). Extra-mark, wait-probe, Stop split, extra_usage Ignore, remaining-min bound, and snapshot/exclude are user-facing but are not a clap subcommand — prove them with hermetic unit/preflight/coordinator tests (the skill refuses `loop run` / `batch run`). Do not drive `models list --remote`.

**Per-iteration:** if this task is listed in **This PRD maps to** (or is a FIX / WIRE-FIX spawned from a mapped task), Read that SKILL.md (and the matching `features/*.md` if listed) and drive that recipe after the scoped language gate. Capture evidence where the skill says. A green compile/test run is not proof.

**REVIEW-001 / milestone:** drive every listed feature this PRD touched. A skipped sub-feature is reported skipped, not verified via a sibling path.

**Blocked skill:** if you cannot launch or a precondition fails, emit `<promise>BLOCKED</promise>` with the unmet precondition. Do not skip the drive and mark the task done.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production — CODE-REVIEW-1 verifies. Most common misses:

- Extra-mark helper takes a single `identity: &str` instead of a set union
- `wait_probe_lifted` built in `account_quota_preflight` before apply (landed known-bad)
- Wrappers never pass `Wait.account_binding` into the probe
- `OperatorStopped` still mapped to `RateLimit` (orchestrator `_` arm exit 1)
- Wave `StopSpend` still exit 130
- `extra_usage` dropped from `is_spend_kind` without evaluate Ignore
- `AccountReactionParams.models` added but `react_to_outputs` still prints builtin `remaining_banner`
- `WaitFn` / `UsageGateFn` widened
- HUD tokens copied into `quota.rs` / `model.rs`
- Unused-import warning on new code → call sites missing

---

## Contract Tasks

`CONTRACT-xxx` tasks (`taskType: "contract"`) are **design-only**. Their job is to produce a stable, reviewable foundational contract (interface, data shape, error model, ownership) that 2+ downstream implementation tasks will depend on.

**When you are given a CONTRACT task**:
- Do not write production code or full test suites.
- Produce the precise definition + extreme details (edge cases, invariants, known-bad discriminators, failure modes, alternatives considered + rationale).
- Explicitly list every downstream story / task ID that will depend on this contract.
- Record the **full contract text** in the progress log under a clear `## CONTRACT-002` header so later agents can read it directly.
- Emit `<task-status>CONTRACT-002:done</task-status>` when the contract is recorded and the acceptance criteria are satisfied.

Downstream FEAT/FIX tasks that list a CONTRACT task in `dependsOn` are expected to implement against the recorded contract. If the contract needs revision, the revision must be done by re-opening the CONTRACT task (or spawning a follow-up CONTRACT-FIX via `task-mgr add`).

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REVIEW-001`) spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

### What each review looks for

| Review                  | Priority | Spawns (priority)                  | Before                  | Focus                                                                                                   |
| ----------------------- | -------- | ---------------------------------- | ----------------------- | ------------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | all FEAT/FIX + CONTRACT + DOCS | Language idioms, security, error handling, `qualityDimensions`, wiring, respect for CONTRACT-002        |
| REVIEW-001              | 99       | `FIX-xxx`                          | CODE-REVIEW-1           | Full unscoped gate + PRE-PR-3 ship table                                                                |

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
  "estimatedEffort": "high",
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2b.json
```

`--depended-on-by` wires the new task into the milestone's `dependsOn` AND syncs the PRD JSON atomically — don't edit the JSON yourself. When a **Project Verification Skills** entry covers the issue, set `verifyCommand` to that skill's drive (the helper or recipe the SKILL.md names), not a unit-test invocation. Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

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

Target: ~10 lines per block. If your entry is longer than ~25 lines, compress it — a future iteration has to read this.

CONTRACT-002: the contract text itself is the exception — write the full copy-pasteable definition under `## CONTRACT-002`.

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw, auto-appended). **Do not Read those files directly** during a loop iteration — they grow unboundedly. Instead:

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval of learnings scored for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries when recall is sparse

Record your own learnings with `task-mgr learn` so they're indexed for future recall. Don't append directly to those files.

**Write concise learnings** (1-2 lines each):
- GOOD: "`wait_probe_lifted` must run after apply using `Wait.account_binding`"
- BAD: "There is a function that checks whether the wait should lift and it needs to look at the right buckets depending on whether the wait is account-binding or scoped-only which is a boolean on the Wait variant."

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
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Add to JSON via `task-mgr add` and commit: `chore: Add blocker task CLARIFY-001`
4. Output:

```
<promise>BLOCKED</promise>
```

Do **not** interview. Blocking → PAUSE-NEEDED / `<promise>BLOCKED</promise>`.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

- **[5430] [5409]** Keep `evaluate_quota` pure; apply owns ask/wait/stop. extra_usage Ignore belongs at `evaluate_one`, not apply-only.
- **[5361]** OAuth usage fold uses only account-binding windows. Do not re-fold scoped windows into an account wait. Unlabeled `seven_day_*` stay ingested with `rungs: None`.
- **[5475]** Remaining banner / ingest must use run `ResolvedModelsConfig` (`buckets_for_run_models` / `remaining_banner_for_run_models`).
- **[5373]** Same `AccountReaction::Stop` from the shared coordinator, but post-reaction wiring diverges (wave exit 130 vs seq exit 1). This gate splits `OperatorStopped` vs `StopSpend`.
- **[5469]** `QuotaAccountAction::Stop` must not reuse `UsageCheckResult::StopSignaled`. HorizonStopped-shaped Empty + `operator_stopped: false`.
- **[5366]** Rung-scoped Fable Wait skips `usage_gate` and early-lift probe. Do not undo. Post-output `WaitFn` stays `Fn(u64)`.
- **[5297] [5298] [5301]** Dual predicates for Anthropic I/O. Do not collapse. Fable phrasing still skips both seams.
- **[5075] [4866] [4126] [4868] [5371]** Account-global reactions fire once per wave. Exhaustive param destructure (no `..`) is the parity lock.
- **[4955]** `preflight_validate_and_probe` is the shared loop/batch chokepoint — remaining-min `> 100` belongs there next to `LOOP_USAGE_THRESHOLD`.
- **[3927] [5088]** Do not reuse `handle_quota_deferral` for rung-only exhaustion (already shipped PR-2 sibling).
- **[5463]** Proto-channel replace-on-evaluate; keep snapshot on API fail.
- **[5457]** Do not narrow snapshot to different-provider-only.
- **[5376] [5395] [5369]** Do not call `load_usage_info` from tests or from paths with no RateLimit; hermetic seams only.
- **[5150]** Use loop-engine-parity-auditor for dual-path account/routing changes.
- **[4753]** After a parallel-slot loop, fixture-reading test binaries may point at a pruned `-slot-N` worktree. `touch tests/<binary>.rs` and rebuild — not a code regression.
- **[4481]** Never stamp model ids onto generated task JSON / engine state (`quota.rs` / `engine.rs`).
- **[5464]** REVIEW-001: unit proofs + unscoped FMT/CHECK/CLIPPY/TEST=0.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` / `src/loop_engine/CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md` to pull just that block.

- **CONTRACT-LOG-001:** `ui::*` for product UX / CLI data / byte-locked operator contracts (stderr, exact bytes, NEVER tracing). `tracing` for internal diagnostics only.
- **Account-global reactions** (`account_usage_gate`, `react_to_outputs`) fire **exactly once per wave**, never once per rate-limited slot.
- **Dual predicate:** pre-iteration `usage_params.enabled = LOOP_USAGE_CHECK_ENABLED && claude_provider_enabled`. Post-output `anthropic_account_io_allowed = claude_provider_enabled` (never from the env flag). Production wait then splits: `check_and_wait` requires both flags; `probe_rate_limit_lifted` requires Claude-only. FR-002 Fable/rung-scoped phrasing still skips **both** seams — do not undo that.
- **Blackout channel (FEAT-008):** `IterationContext::provider_blackouts` is separate from `runner_overrides`. The PR-2 proto-channel is a sibling `HashSet<(Provider, CapabilityTier)>` — do not record a **provider** blackout for rung-scoped unavailability. Spillover is **never** a working rung for rung-scoped decisions.
- **Single-home contract:** production entry + hermetic `_inner` + exhaustive param destructure (no `..`). Seq and wave share the inner. `tests/reaction_parity.rs` is the lock.
- **`LOOP_USAGE_CHECK_ENABLED` is not a Claude kill-switch.** Env=false still classifies post-output RateLimit; proto-channel still replaced on successful evaluate.
- **CapabilityTier / `tier_of`:** config exact-match. Substring tier classification is dead. The only allowed `contains` is ingest: API family token vs configured model **string** to pick a rung when `display_name` is absent (`limits[]` unlabeled ids). Extra-mark compares rungs to identity **set** I (always family constant **plus** `scope.model.id` when present). Named object siblings without `scope.model` must **not** family-token-map onto rungs. `has_review` calls `is_frontier_class` — do not reimplement prefix stripping.
- **`model_for` is bidirectional nearest-defined (down, then up).** Do not call it for blackout clamp (PR-3). Extra-mark uses `exact_model_for` against identity set I, **not** `exact_model_for(mapped_rung)`.
- **Grok-only recipe is untouched** by this PR. Do not invent `models set-primary`.
- **Skills:** tests that spawn the real binary with an init-family command MUST set `HOME` to a tempdir. verify-task-mgr already does this.
- **Landed pin-required text is stale** (`CLAUDE.md` ~156, `src/loop_engine/CLAUDE.md` ~240). DOCS-001 strikes it: after this slice pin is **optional, not required** for mixed standard/medium. All-high/review clamp is still PR-3. Fable CLI 3600 still fires if a Fable-routed task spawns.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments. Line numbers are from worktree `feat-quota-rung-policy-pr2` / `main` `dacccb3`.

**OAuth JSON object window** (`serde_json::Value` string keys → `QuotaBucket`):

```rust
json.get("five_hour")?.get("utilization")?.as_f64()
let remaining = (100.0 - util).clamp(0.0, 100.0);
// Walk EVERY object sibling with utilization/dollars — no allow-list of window names
```

**OAuth `limits[]` HUD extra-mark** (CONTRACT-002 / FEAT-008):

```rust
limit["scope"]["model"]["display_name"]  // HUD table maps this (usage.rs hud_tier_from_label only)
limit["scope"]["model"]["id"]            // snapshot id — ADD to identity set I, do not prefer over the constant

// After HUD maps to rung R:
//   I = always canonical_model_for_hud_tier(R)  // FABLE_MODEL / OPUS_MODEL / SONNET_MODEL / HAIKU_MODEL
//     plus scope.model.id when present
// Extra-mark every defined Claude rung whose exact_model_for(provider, t) equals any I.
// Always include HUD primary.
// NOT exact_model_for(mapped_rung). NOT prefer snapshot id.
// Fable HUD + set-tier claude frontier <opus> → frontier only.
// Opus HUD + same pin + id "claude-opus-5-SNAPSHOT" → standard AND frontier.
// Opus HUD, no pin → standard only.

// Helper: extra_mark_rungs_matching(models, provider, identities: impl IntoIterator<Item = &str>)
//   or call once per member of I and union. Not identity: &str.
```

**Named sibling without `scope.model`** (FEAT-009):

```rust
// key: "seven_day_opus" / "seven_day_sonnet"
// kind may still be weekly_scoped (kind_for_named_key / looks_rung_scoped_key)
// rungs: None — do NOT call map_unlabeled_token
// limits[] unlabeled ids (id, no display_name) STILL use map_unlabeled_token (FR-003 step 2)
evaluate_quota(ingest(live_shaped_oauth_json()), &UsagePolicy::default(), 8)
  .unavailable == {(Provider::Claude, CapabilityTier::Frontier)}  // only
```

**Wait-driving probe** (FIX-010):

```rust
// apply already computes:
let has_account_binding_wait = !account_wait_resets.is_empty(); // account.rs:1213
// mixed session+scoped ⇒ true

QuotaAccountAction::Wait { secs: u64, account_binding: bool }

// AFTER apply (not in account_quota_preflight before apply):
fn wait_probe_lifted(info: &UsageInfo, floor: u8, account_binding: bool, models: &ResolvedModelsConfig) -> bool
// account_binding: usage_suggests_lifted(info, floor, false)
// scoped-only: buckets_for_run_models; lift iff every nonempty-rungs bucket remaining > floor or missing
// NO evaluate_quota. NO extra GET.

// Post-output UNCHANGED:
pub type WaitFn<'f> = &'f dyn Fn(u64) -> bool;           // account.rs:181
pub type UsageGateFn<'f> = &'f dyn Fn(u8, &Path, u64) -> UsageCheckResult; // account.rs:58

// Thread models onto QuotaPreflightParams (RunAccountQuotaGateParams already has models at account.rs:1432)
```

**AccountReaction split** (FIX-011):

```rust
// DELETE AccountReaction::Stop
AccountReaction::OperatorStopped  // seq: Empty + operator_stopped true  (copy iteration.rs:149 StopSignaled)
                                  // wave: was_stopped true, exit 0, reason "stop signal during rate-limit wait"
AccountReaction::StopSpend        // seq: Empty + operator_stopped false + should_stop (copy iteration.rs:166 HorizonStopped)
                                  // wave: was_stopped false, exit 0, reason "usage/spend limit"
// NOT RateLimit + operator_stopped (orchestrator.rs:730 _ → exit 1)
// Spend scan BEFORE prefer-rung-scoped decide_item (account.rs:585)
```

**Hygiene** (FIX-012):

```rust
has_review = is_frontier_class(&id);  // model.rs:240 — NOT id.contains("REVIEW")
// MILESTONE-FINAL true; REFACTOR-REVIEW-FINAL false; 8d71d1f7-CODE-REVIEW-1 true
// do not change prompt/core.rs contains("REVIEW")

// evaluate_one: Ignore extra_usage / promotional / nimbus_quill BEFORE amount_exhausted AccountLow
// then drop extra_usage from is_spend_kind (account.rs:1344)
// spend dollars 0 still AccountLow + apply Stop

// preflight_validate_and_probe (project_config.rs:1062) only:
//   LOOP_USAGE_REMAINING_MIN > 100 OR usagePolicy.remainingMinPercent > 100 → error naming LOOP_USAGE_REMAINING_MIN
// u8 200 is the known-bad. Non-loop silent.

// AccountReactionParams.models: &ResolvedModelsConfig
// react_to_outputs closes over params.models → remaining_banner_for_run_models when oauth_json present
// do NOT widen WaitFn / UsageGateFn
```

**evaluate vs apply (shipped PR-2 — do not rewrite):**

```rust
evaluate_quota(buckets, policy, remaining_min)  // pure, no ask, no other_rungs_runnable
apply_quota(eval, buckets, policy, tier_fallback, work)  // resolves ask|wait|stop|unavailable
decision.unavailable.iter().any(|(p, t)| *p == Provider::Claude && *t == CapabilityTier::Frontier)
```

---

## Feature-Specific Checks

- Relocate every line number on worktree `feat-quota-rung-policy-pr2` / `main` `dacccb3` before editing. The `feat/quota-rung-policy-pr1` checkout has no `quota.rs`.
- Landed known-bads to invert:
  - `usage.rs:571` `extra_mark_rungs` keys on `exact_model_for(primary)` — Fable HUD + pin extra-marks standard
  - `usage.rs:369` `ingest_named_sibling` calls `map_unlabeled_token` — `seven_day_opus` → standard
  - `account.rs:1575` `account_quota_preflight` builds `usage_suggests_lifted` before apply
  - `iteration.rs:874` `== Stop` → `RateLimit` / `operator_stopped: false`
  - `wave_scheduler.rs:1101` every `Stop` → exit 130
  - `quota.rs:291` `amount_exhausted` AccountLow before extra_usage Ignore
  - `account.rs:1742` `id.contains("REVIEW")`
- Promote `live_shaped_oauth_json` to `pub(crate)` (cfg(test) OK) so account/pre_spawn tests share it — do not copy the fixture.
- `grep -n "claude-fable-5\\|fable" src/loop_engine/quota.rs src/loop_engine/engine.rs` → 0. HUD tokens only in `usage.rs`.
- Pin is harmful until FEAT-008 (code) is green, not until CONTRACT-002 is recorded in the progress log. After FEAT-008, pin is optional (DOCS-001). All-high clamp is still PR-3.

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - see CLAUDE.md excerpts above
- Do not spawn nested agents
- Do not interview; blocking → `<promise>BLOCKED</promise>`
