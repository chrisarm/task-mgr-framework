# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Quota rung policy PR-2 (generic buckets + remaining unit + horizon apply)** for **task-mgr**.

## Problem Statement

`parse_oauth_usage_json` still folds only account-binding windows into one used-percent `UsageInfo` (PR-1). Operators still see `Usage: 55.0% (threshold: 92%)`. A 95%-used frontier weekly bucket is omitted from the account wait (good) but is not yet a rung-unavailable signal, so frontier-routed work keeps dispatching until a Fable CLI RateLimit sleeps the whole wave 3600s.

PR-2 makes remaining percent the unit, ingests every API bucket as a generic `QuotaBucket`, and applies a horizon heuristic: wait if reset is within an hour; wait capped at `MAX_WAIT_SECS` between 1h and 12h; stop if reset is more than 12 hours away and no other rung can run. Factory default `routing.tierFallback.maxDifficulty: high` + `includeReview: true` **is** the downgrade instruction — frontier-low marks frontier **unavailable** (continue on standard), it does not ask. `evaluate_quota` does **not** emit `ask`; apply in `account.rs` resolves ask/wait/stop. Unavailable rungs are excluded from the next selection (do not account-wait). Proto-channel `HashSet<(Provider, CapabilityTier)>` on `IterationContext` is replaced on each successful evaluate.

**This list ships PR-2 only** (CONTRACT-001 + FEAT-003 + FEAT-004 + FEAT-005 / US-003 + US-004 / FR-003 + FR-004). `--use-other-models-ttl`, FR-006 expiry + down-only walker + family-match of explicit `tasks.model`, and FR-008 `set-usage-rule` / `set-tier-fallback` CLI stay PR-3.

Pins (do not rewrite):

1. A low **frontier** (HUD: “Current week (Fable)”) bucket is **not** an account emergency — continue on **standard** (and cheaper rungs).
2. Engine language is **capability rungs** (`frontier` / `standard` / `cost-efficient` / `cheapest`), never model ids (`fable`, `opus`, `claude-fable-5`) except at the ingest adapter that maps an API label onto a rung.
3. Default action is a **horizon heuristic** (all config.json-overridable): **wait** if reset is within the next hour; **stop** if reset is more than 12 hours away *and* no other rung can run; **ask** if other rungs still work but no downgrade instruction exists. Factory default `tierFallback` **is** that instruction (ask is the opt-out). `--use-other-models-ttl` is PR-3.

---

## PR-2 scope lock (read every iteration)

In scope: `quota.rs` types + `evaluate_quota`; `usage.rs` `ingest_oauth_value` (HUD table + extra-mark by configured model string equality; do not add a models param to `parse_oauth_usage_json`); remaining rename / `% left` banners / `LOOP_USAGE_REMAINING_MIN` / `LOOP_USAGE_THRESHOLD` preflight error; `format_duration` days band; apply layer in `account.rs`; proto-channel HashSet replace-on-evaluate; exclude unavailable rungs from next selection even when `provider_blackouts` is empty; absent `tierFallback` key → factory Some.

**Out of scope (do not implement, do not spawn as “helpful” follow-ups):**

- `--use-other-models-ttl` clap on `loop run` / `batch run` (FR-005)
- TTL > 0 re-evaluate on stop-check cadence
- `resolve_execution_plan` down-only walker / post-resolve clamp (FR-006)
- Family-match of explicit `tasks.model` at resolve time (PR-3)
- `models set-usage-rule` / `set-tier-fallback` / `models show` live remaining (FR-008 / US-007)
- Dated month-name `parse_reset_from_output` as the **account** wait for a rung-scoped bucket
- Re-folding scoped windows into an account wait
- Live Anthropic / real `~/.claude` credentials

Do **not** edit `tasks/quota-rung-policy.json` (full-vision list), `tasks/quota-rung-policy-pr1.json`, or `tasks/prd-quota-rung-policy.md`.

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

- evaluate_quota emitting ask or taking other_rungs_runnable: bool
- Account-waiting (`check_and_wait` / `Wait { 3600 }`) for a scoped-rung unavailable (parks standard work)
- Skip-wait hot-loop of the same frontier task when unavailable (must exclude from next selection)
- Reusing handle_quota_deferral for rung-only exhaustion (stale-abort, learning 3927 / 5088)
- Storing fable / opus / claude-fable-5 in quota.rs or IterationContext proto-channel keys
- HUD-only mapping after the PR-1 pin (Opus HUD must extra-mark rungs sharing that configured model string)
- Substring tier_of for extra-mark (string equality of configured model strings only)
- Treating severity / is_active as the default low predicate
- Spend stop at remaining-percent floor (must be remainingAmount lte 0)
- Soonest-reset among multiple low wait buckets (must be latest)
- Remaining as a 0.08 ratio instead of 0–100 percent
- Silently ignoring LOOP_USAGE_THRESHOLD (must preflight error)
- Keeping used>=92 compare after the remaining rename
- Factory default with no tierFallback (ask path is the opt-out, not the default)
- Deserializing absent routing.tierFallback as None (must be factory Some; explicit JSON null is the ask opt-out)
- Documenting a wait loop as accepted for explicit off-ladder tasks.model (family-match + defer if unavailable and includeForced=false is PR-3; do not document wait-loop as accepted)
- Implementing --use-other-models-ttl, FR-006 expiry + down-only walker, FR-008 set-usage-rule / set-tier-fallback CLI, or models show live remaining
- Dated month-name parse_reset_from_output becoming the account wait for a rung-scoped bucket
- Spillover counting as a working rung for rung-scoped decisions
- Collapsing dual Anthropic I/O predicates onto one flag
- quota.rs importing runners, clap, or SQLite
- Tests that require live Anthropic network or real ~/.claude credentials
- Driving `loop run` / `batch run` (verify-task-mgr refuses; `loop run --help` is the safe probe)
- Inventing a second verification harness
- Manual edits to tasks/*.json for status (use task-mgr CLI / task-status tags)
- unwrap() in production paths
- Catch-all error handlers that swallow context

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy -- -D warnings` (per-iteration). `--all-targets` is REVIEW-001 only — do not run it on FEAT-003 / FEAT-004 / FEAT-005
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() in production code paths
- Sequential and wave paths stay parity-locked for account reactions (exhaustive destructure, shared coordinators)
- quota.rs and engine.rs proto-channel keys contain no model-id literals (frontier/standard/cost-efficient/cheapest only)
- No --use-other-models-ttl / FR-006 clamp / FR-008 CLI in this PR
- No live Anthropic / real ~/.claude credentials in tests

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Project Verification Skills, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

Do **not** edit `tasks/quota-rung-policy.json`, `tasks/quota-rung-policy-pr1.json`, or `tasks/prd-quota-rung-policy.md`.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/quota-rung-policy-pr2.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Inspect this iteration's task          | `task-mgr show <TASK-ID>` using the task ID from `## Current Task`                                                                                                                 |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2.json` — priority auto-computed; DB + PRD JSON updated atomically                                                   |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task (rare), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/quota-rung-policy-pr2.json
jq '.globalAcceptanceCriteria' tasks/quota-rung-policy-pr2.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/quota-rung-policy-pr2-prompt.md`   | This prompt file (read-only)                                               |
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

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `## Current Task` lists a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log (`grep -n -A 40 '## .* - <THAT-TASK-ID>' tasks/progress-$PREFIX.txt`). Skip entirely on the first iteration (file won't exist). CONTRACT dependents **must** grep `## CONTRACT-001` from the progress log and implement against that text.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, or the task touches a file that's likely documented there, `grep` for the relevant term and read only the surrounding lines:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```
   The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, Project Verification Skills, Key Context, and the CLAUDE.md excerpts that matter for this PRD) are already embedded in **this prompt file**. Prefer it over re-reading source docs. When a verification skill applies, Read that SKILL.md at verification time — do not paste it into the progress log.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed (`feat/quota-rung-policy-pr2`). Switch if wrong.

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

1. **ANALYSIS gate**: FEAT-004 and FEAT-005 already carry `consumerAnalysis` in this prompt's Data Flow / Feature-Specific Checks. Do not spawn a separate ANALYSIS task.
2. **Consumer Impact Table**:
   - `BREAKS` → implement the mitigation named on the task (remaining compare flip; exclude-not-account-wait). Split only if two semantic contexts truly need different code paths.
   - `NEEDS_REVIEW` → verify the call site before changing it.
   - `OK` → proceed.
3. **Semantic distinctions**: account-binding remaining-low vs rung-scoped remaining-low vs operator-forbade-downgrade are different contexts — do not shoehorn them into one `Wait { 3600 }`.

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
2. For the remaining unrelated failures: spawn a single `FIX-xxx` or `CLARIFY-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2.json` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID so a human can route ownership.

Below the ~12-failure threshold, just fix them.

If a **Project Verification Skills** section is present, this gate also includes that skill's mapped-feature drive (see that section).

---

## Project Verification Skills

This repo ships a project-level verification skill. Language-level gates (fmt, type-check, lint, scoped tests) are **necessary but not sufficient** for user-facing changes the skill covers. Follow the skill literally — do not invent a second harness, and do not paste the skill body into the progress log.

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** `loop-run-help` (FEAT-004 negative: help must not list `--use-other-models-ttl`; REVIEW-001). Remaining `% left` banners and `LOOP_USAGE_REMAINING_MIN` / `LOOP_USAGE_THRESHOLD` are user-facing but are not a clap subcommand — prove them with hermetic unit/preflight tests (the skill refuses `loop run` / `batch run`). Do not drive `models list --remote`.

**Per-iteration:** if this task is listed in **This PRD maps to** (or is a FIX / WIRE-FIX spawned from a mapped task), Read that SKILL.md (and the matching `features/*.md` if listed) and drive that recipe after the scoped language gate. Capture evidence where the skill says. A green compile/test run is not proof.

**REVIEW-001 / milestone:** drive every listed feature this PRD touched. A skipped sub-feature is reported skipped, not verified via a sibling path.

**Blocked skill:** if you cannot launch or a precondition fails, emit `<promise>BLOCKED</promise>` with the unmet precondition. Do not skip the drive and mark the task done.

The helper **refuses** `loop run`, `batch run`, and the deprecated flat `loop <prd>` / `batch <glob>` forms. `loop run --help` is the safe probe.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production — CODE-REVIEW-1 verifies. Most common misses:

- Not registered in dispatcher/router → add to registration (`mod.rs` `pub mod quota`)
- Test mocks bypass real wiring → verify production path separately
- Config field read but not passed through → wire `remaining_min` / `tierFallback` through `UsageParams` / apply params
- Unused-import warning on new code → call sites missing
- Wrong key type on map access (atom vs string) — struct keys ≠ JSONB keys → check Data Flow Contracts
- New CLI subcommand / DB column / JSON field defined but not threaded into the dispatcher / `TryFrom<Row>` / parse-to-task mapping
- Proto-channel written but `compute_quota_excluded_ids` still keys only on `provider_blackouts`, or still early-returns empty when `provider_blackouts` is empty (empty is the production case — do not gate proto-channel exclusion on that set being non-empty)
- `evaluate_quota` result dropped on the floor of `check_and_wait`
- Extra-mark stuffed into `parse_oauth_usage_json` (new `ingest_oauth_value` takes the models config; parse stays threshold-only)
- Absent `routing.tierFallback` key deserialized as `None` (must be factory `Some`; explicit JSON `null` is the ask opt-out)

---

## Contract Tasks

`CONTRACT-xxx` tasks (`taskType: "contract"`) are **design-only**. Their job is to produce a stable, reviewable foundational contract (interface, data shape, error model, ownership) that 2+ downstream implementation tasks will depend on.

**When you are given a CONTRACT task**:
- Do not write production code or full test suites.
- Produce the precise definition + extreme details (edge cases, invariants, known-bad discriminators, failure modes, alternatives considered + rationale).
- Explicitly list every downstream story / task ID that will depend on this contract.
- Record the **full contract text** in the progress log under a clear `## CONTRACT-001` header so later agents can read it directly.
- Emit `<task-status>CONTRACT-001:done</task-status>` when the contract is recorded and the acceptance criteria are satisfied.

Downstream FEAT/FIX tasks that list a CONTRACT task in `dependsOn` are expected to implement against the recorded contract. If the contract needs revision, the revision must be done by re-opening the CONTRACT task (or spawning a follow-up CONTRACT-FIX via `task-mgr add`).

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REVIEW-001`) spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

### What each review looks for

| Review                  | Priority | Spawns (priority)                  | Before                  | Focus                                                                                                   |
| ----------------------- | -------- | ---------------------------------- | ----------------------- | ------------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | CONTRACT + FEAT-003/004/005 | Language idioms, security, error handling, `qualityDimensions`, wiring, respect for CONTRACT-001        |
| REVIEW-001              | 99       | `FIX-xxx`                          | all prior               | Full unscoped suite + PR-2 ship check                                                                   |

Use the **rust-python-code-reviewer** / **loop-engine-parity-auditor** when reviewing code. Document findings in the progress file.

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
}' | task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2.json
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

CONTRACT-001 is the exception: the full contract text belongs in that progress block under `## CONTRACT-001` (dependents grep it). Keep the contract copy-pasteable; do not bury it in prose.

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw, auto-appended). **Do not Read those files directly** during a loop iteration — they grow unboundedly. Instead:

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval of learnings scored for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries when recall is sparse

Record your own learnings with `task-mgr learn` so they're indexed for future recall. Don't append directly to those files.

**Write concise learnings** (1-2 lines each):
- GOOD: "`evaluate_quota` returns per-bucket facts; apply in `account.rs` owns ask"
- BAD: "There is a function called evaluate_quota that takes buckets and a policy and it is important that it does not return ask because the apply layer needs the remaining task list."

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
3. Add via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2.json` and commit
4. Output:

```
<promise>BLOCKED</promise>
```

---

## Milestones

`REVIEW-001` is the full-gate checkpoint: it proves the trunk is green before merge. It is NOT a sweep to rewrite remaining tasks.

### Milestone Protocol

1. Check all `dependsOn` tasks have `passes: true`. If any don't, the milestone can't run yet.
2. **Run the full quality gate** (unscoped format, type-check, lint, complete test suite). Drive verify-task-mgr `loop-run-help`.
3. **Leave the repo green.** For every failure, including pre-existing ones that predate this PRD:
   - Trivial fixes go in the milestone's own commit: `chore: REVIEW-001 - fix stale test <name>`.
   - Non-trivial failures → spawn a `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr2.json` with the failure's `verifyCommand`. The loop picks it up; the milestone re-runs when the FIX passes.
4. Mark `<task-status>REVIEW-001:done</task-status>` only when the full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

- **[5361]** OAuth usage fold uses only account-binding windows (`five_hour` / `seven_day` + `limits[]` session/weekly_all). PR-2 ingest adds buckets alongside; do not re-fold scoped windows into an account wait.
- **[5362] → invert** Gate-relevant was used ≥ 92. PR-2 remaining unit: proceed when remaining > floor (default 8). Same bar, inverted.
- **[5398]** `reset_at` / wait duration must use the **live** remaining-min, not a hardcoded 8/92.
- **[5297] [5298] [5301]** Dual predicates for Anthropic I/O: pre = `LOOP_USAGE_CHECK_ENABLED ∧ Claude enabled`; post allow-flag = Claude only; post usage-API load ANDs env; probe is Claude-only. Do not collapse. Proto-channel still replaced on successful evaluate when env=false.
- **[5075] [4866] [4138] [4171] [5371]** Account-global reactions fire exactly once per wave. Seq and wave share the coordinator.
- **[4126] [4868]** Exhaustive param destructure (no `..`) is the parity lock.
- **[3927] [5088]** Quota deferral must not trip stale-abort. Do **not** reuse `handle_quota_deferral` for rung-only exhaustion — new sibling.
- **[5087] [5089]** `BlackoutState` / `provider_blackouts` is a separate ephemeral channel from `runner_overrides`. The PR-2 proto-channel is a **sibling** HashSet, not that map.
- **[1810] [4373]** `IterationContext` is main-thread-only (not Send-safe). Slot workers must not mutate the proto-channel.
- **[5376] [5385] [5386] [5395]** Do not call `load_usage_info` from tests or from paths with no RateLimit; ureq timeouts already exist. Hermetic seams only.
- **[4955]** `preflight_validate_and_probe` is the shared loop/batch chokepoint — put the `LOOP_USAGE_THRESHOLD` hard-break there.
- **[5150]** Use loop-engine-parity-auditor for dual-path account/routing changes.
- **[4753]** After a parallel-slot loop, fixture-reading test binaries may point at a pruned `-slot-N` worktree. `touch tests/<binary>.rs` and rebuild — not a code regression.
- **[4481]** Never stamp model ids onto generated task JSON / engine state.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` / `src/loop_engine/CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md` to pull just that block.

- **CONTRACT-LOG-001:** `ui::*` for product UX / CLI data / byte-locked operator contracts (stderr, exact bytes, NEVER tracing). `tracing` for internal diagnostics only.
- **Account-global reactions** (`account_usage_gate`, `react_to_outputs`) fire **exactly once per wave**, never once per rate-limited slot.
- **Dual predicate:** pre-iteration `usage_params.enabled = LOOP_USAGE_CHECK_ENABLED && claude_provider_enabled`. Post-output `anthropic_account_io_allowed = claude_provider_enabled` (never from the env flag). Production wait then splits: `check_and_wait` requires both flags; `probe_rate_limit_lifted` requires Claude-only. FR-002 Fable/rung-scoped phrasing still skips **both** seams — do not undo that.
- **Blackout channel (FEAT-008):** `IterationContext::provider_blackouts` is separate from `runner_overrides`. The PR-2 proto-channel is a sibling `HashSet<(Provider, CapabilityTier)>` — do not record a **provider** blackout for rung-scoped unavailability. Spillover is **never** a working rung for rung-scoped decisions.
- **Single-home contract:** production entry + hermetic `_inner` + exhaustive param destructure (no `..`). Seq and wave share the inner. `tests/reaction_parity.rs` is the lock.
- **`LOOP_USAGE_CHECK_ENABLED` is not a Claude kill-switch.** Env=false still classifies post-output RateLimit; proto-channel still replaced on successful evaluate.
- **CapabilityTier / `tier_of`:** config exact-match. Substring tier classification is dead. The only allowed `contains` is ingest: API family token vs configured model **string** to pick a rung when `display_name` is absent. Extra-mark is string equality of configured models.
- **`model_for` is bidirectional nearest-defined (down, then up).** Do not call it for blackout clamp (PR-3). Ingest may call it to read a rung's configured model string for extra-mark equality.
- **Grok-only recipe is untouched** by this PR. Do not invent `models set-primary`.
- **Skills:** tests that spawn the real binary with an init-family command MUST set `HOME` to a tempdir. verify-task-mgr already does this.
- **PR-1 pin** (`models set-tier claude frontier <standard-model>`) remains required for parallel/wave until PR-3 automatic clamp. Extra-mark exists so an Opus HUD row after that pin marks **both** standard and frontier.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments.

**OAuth JSON object window** (`serde_json::Value` string keys → `QuotaBucket`):

```rust
json.get("five_hour")?.get("utilization")?.as_f64()  // used 0–100 as-is; 1.0 is 1%, not exhausted
let remaining = (100.0 - util).clamp(0.0, 100.0);
json.get("five_hour")?.get("resets_at")?.as_str()
// Walk EVERY object sibling with utilization/dollars — no allow-list of window names
```

**OAuth `limits[]`** (array of objects):

```rust
let kind = limit.get("kind").and_then(|v| v.as_str()); // "session" | "weekly_all" | "weekly_scoped" | ...
let percent = limit.get("percent").and_then(|v| v.as_f64())
    .or_else(|| limit.get("percent").and_then(|v| v.as_u64()).map(|u| u as f64));
limit.get("severity")  // display hint — MUST NOT set default-low
limit.get("is_active") // display hint — MUST NOT set default-low
limit["scope"]["model"]["display_name"]  // HUD table maps this
```

**HUD map then extra-mark** (`ingest_oauth_value` in `usage.rs` only — do **not** add a models param to `parse_oauth_usage_json`):

```rust
// ingest_oauth_value(json: &Value, models: &ResolvedModelsConfig) -> Vec<QuotaBucket>
// parse_oauth_usage_json stays threshold-only (PR-1 signature). Extra-mark needs config;
// that is why ingest is a new function, not a new param on the fold parser.

// 1. HUD label table (case-insensitive prefix/token):
//    Fable → Frontier, Opus → Standard, Sonnet → CostEfficient, Haiku → Cheapest
// 2. Else unlabeled ids: family token as substring of configured model_for string
// 3. Extra-mark: after mapping display_name to rung R,
//    for each defined rung T:
//      if model_for(provider, T) == model_for(provider, R) { mark T }
//    string equality, NOT substring tier_of.
// After set-tier claude frontier <standard-model>, an Opus HUD row
// maps to Standard AND marks Frontier.
// Output remains Vec<(Provider, CapabilityTier)>
```

**Project config** (camelCase JSON → structs; sparse `serde_json::Value` round-trip):

```rust
config["usagePolicy"]["remainingMinPercent"]          // u8, default 8
config["usagePolicy"]["waitIfResetWithinMinutes"]     // default 60
config["usagePolicy"]["stopIfResetBeyondHours"]       // default 12
config["usagePolicy"]["askTtlMinutes"]                // default 0 (TTL CLI is PR-3)
config["usagePolicy"]["rules"][i]["onLow"]            // wait|unavailable|stop|ask|ignore
config["routing"]["tierFallback"]["maxDifficulty"]    // "low"|"medium"|"high"; default "high"
config["routing"]["tierFallback"]["includeReview"]    // default true
config["routing"]["tierFallback"]["includeForced"]    // default false

// RoutingConfig.tierFallback: Option<TierFallback>
// #[serde(default = "default_tier_fallback")]
//   absent key → Some(TierFallback { maxDifficulty: high, includeReview: true, includeForced: false })
//   explicit JSON null → None → ask opt-out
// Do NOT use #[serde(default)] on Option (that yields None for a missing key).
```

**evaluate vs apply:**

```rust
// quota.rs — pure, no ask, no other_rungs_runnable
evaluate_quota(buckets: &[QuotaBucket], policy: &UsagePolicy, remaining_min: u8)
  -> per-bucket ignore | unavailable
     + account wait/stop INPUTS { remaining, reset_secs, kind, low }

// account.rs apply — remaining work + tierFallback
// resolves ask | wait | stop | unavailable
// stop beats ask
// factory / omitted-key tierFallback → unavailable (not ask) for eligible tasks
// explicit JSON null / narrower maxDifficulty / includeReview false → ask
// PR-2 TTL 0 + forbade → defer (no sleep)
decision.unavailable.iter().any(|(p, t)| *p == Provider::Claude && *t == CapabilityTier::Frontier)
```

**Proto-channel:**

```rust
// IterationContext — main-thread only
unavailable_rungs: HashSet<(Provider, CapabilityTier)>
// replace the set on each successful evaluate
// keep snapshot on API fail
// compute_quota_excluded_ids also consults this set
// Empty provider_blackouts is the PRODUCTION case — do not gate proto-channel
// exclusion on that set being non-empty. Drop/bypass
//   if active_blackouts.is_empty() { return HashSet::new(); }
// do NOT reuse handle_quota_deferral / provider_blackouts
```

**Live fixture shape** (production-shaped; use in unit tests):

```json
{
  "five_hour": { "utilization": 24.0, "resets_at": "<session RFC3339>" },
  "seven_day": { "utilization": 55.0, "resets_at": "<weekly RFC3339>" },
  "seven_day_opus": { "utilization": 100.0, "resets_at": "<later RFC3339>" },
  "seven_day_sonnet": { "utilization": 100.0, "resets_at": "<later RFC3339>" },
  "limits": [
    { "kind": "session", "percent": 24, "severity": "normal", "is_active": true, "resets_at": "<session RFC3339>" },
    { "kind": "weekly_all", "percent": 55, "severity": "normal", "is_active": true, "resets_at": "<weekly RFC3339>" },
    {
      "kind": "weekly_scoped",
      "percent": 95,
      "severity": "critical",
      "is_active": true,
      "resets_at": "<weekly RFC3339>",
      "scope": { "model": { "display_name": "Fable" } }
    }
  ]
}
```

PR-2 expect: session remaining 76, week remaining 45, frontier remaining 5; account remaining 45 > floor 8 proceeds; frontier unavailable under factory default. Banner contains `76% left` / `5% left` and does not print used `95%`. Parse/ingest tests are hermetic — do not call `load_usage_info`.

**Existing coordinators (do not break PR-1 FR-002):**

```rust
// Narrow Fable/rung-scoped CLI still: Wait { blackout_fallback_secs } ignoring api/output;
// no Blackout; no usage_gate / probe. Leave that path intact.
AccountUsageGateParams { /* exhaustive destructure, no `..` */ }
MAX_WAIT_SECS // already 5 * 3600 in account.rs — reuse for the 1h–12h cap
```

---

## Feature-Specific Checks

- **evaluate vs apply.** `evaluate_quota` emits per-bucket ignore/unavailable + account wait/stop **inputs**. It does **not** emit `ask` and does **not** take `other_rungs_runnable`. Apply in `account.rs` owns remaining-work + `tierFallback`.
- **Factory default is auto-downgrade.** `tierFallback.maxDifficulty: high`, `includeReview: true`, `includeForced: false`. Absent `routing.tierFallback` key → factory `Some` (unavailable, not ask). Explicit JSON `null` → `None` → ask opt-out. Ask is also the opt-out for narrower `maxDifficulty` / `includeReview: false`. PR-2 TTL default 0 + forbade → **defer** (no sleep, no continue). Do not implement the clap flag.
- **Horizon middle band.** Wait ≤1h; 1h–12h wait capped at `MAX_WAIT_SECS` (3h session waits; 5h–12h is cap-and-repark); stop >12h and nothing else can run.
- **Exclude, do not account-wait.** Unavailable rungs drop out of the next selection. Empty `provider_blackouts` is the production case — do not gate proto-channel exclusion on that set being non-empty. `Wait { 3600 }` only when the remaining queue cannot run. Skip-wait hot-loops the same frontier task; account-wait parks standard.
- **Proto-channel replace-on-evaluate.** `HashSet<(Provider, CapabilityTier)>` on `IterationContext`. Replace the set on each successful evaluate; keep snapshot on API fail. No expiry (PR-3). Main-thread only.
- **Extra-mark.** HUD table maps `display_name`; then mark every rung whose configured model string **equals** that mapped rung's model. After the PR-1 pin, an Opus HUD row marks standard **and** frontier. Not substring `tier_of`.
- **Remaining unit.** 0–100 percent, never 0.08. `usage_remaining_min` default 8. `LOOP_USAGE_REMAINING_MIN` > config > 8. `LOOP_USAGE_THRESHOLD` preflight **error**. Old used≥92 ≡ remaining≤8.
- **Spillover is never a working rung** for rung-scoped decisions.
- **Do not add** `--use-other-models-ttl`, down-only walker, family-match of explicit `tasks.model`, `set-usage-rule`, or month-name account wait. Do **not** document a wait loop as accepted for off-ladder `tasks.model`.
- **Ingest entry** is `ingest_oauth_value`. Do not add a models param to `parse_oauth_usage_json`.
- **Banner durations ≥ 24h** use a days band in `format_duration` (`display.rs`). Do not byte-lock the exact substring `5d 13h`.

---

## Important Rules

- Work on **ONE story per iteration**
- **For high-effort tasks** (`estimatedEffort: "high"` or 10+ acceptance criteria): consider using `/ralph-loop` to iterate within the task until all acceptance criteria pass, e.g.:
  `/ralph-loop "Implement [TASK-ID]: [title]. Criteria: [list]. Output <promise>DONE</promise> when all pass." --max-iterations 10`
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - reaction coordinators, WaitSpy / IoSeamSpy, exhaustive destructure
