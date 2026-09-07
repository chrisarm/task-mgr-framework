# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Quota rung policy PR-3 (ask TTL CLI + rung blackout clamp + policy CLI)** for **task-mgr**.

## Problem Statement

PR-2 (landed on local main `dacccb3`) ingests generic `QuotaBucket`s, speaks remaining percent, and applies a horizon heuristic. Factory `routing.tierFallback.maxDifficulty: high` + `includeReview: true` marks frontier **unavailable** and excludes it from the next selection. `askTtlMinutes` already sleeps via `QuotaAccountAction::Ask` (CODE-FIX-010). `HorizonStopped` is distinct from operator `.stop`. Rung-only empty selection uses `handle_rung_only_empty_selection`.

What is still missing: `--use-other-models-ttl` on `loop run` / `batch run`; rung blackout **expiry**; a **down-only** walker in `resolve_execution_plan` (must not call `model_for`); family-match of explicit off-ladder `tasks.model` at resolve time; next-PRD inherit of rung-unavailable; `models set-usage-rule` / `set-tier-fallback` / `models show` policy (remaining numbers only behind the live-fetch gate).

Until this PR, parallel/wave still needs the PR-1 pin (`models set-tier claude frontier <standard-model>`). After FEAT-007 clamp, that pin is no longer required.

**This list ships PR-3 only** (FEAT-006 + FEAT-007 + FEAT-008 / US-005 + US-006 + US-007 / FR-005 + FR-006 + FR-008). Do **not** reimplement PR-2 evaluate/apply, remaining-percent, proto-channel replace-on-evaluate, factory `tierFallback` serde, `askTtlMinutes` sleep, `HorizonStopped`, or `handle_rung_only_empty_selection` — build on them.

Pins (do not rewrite):

1. A low **frontier** (HUD: “Current week (Fable)”) bucket is **not** an account emergency — continue on **standard** (and cheaper rungs).
2. Engine language is **capability rungs** (`frontier` / `standard` / `cost-efficient` / `cheapest`), never model ids (`fable`, `opus`, `claude-fable-5`) except at the ingest adapter that maps an API label onto a rung.
3. Default action is a **horizon heuristic** (all config.json-overridable): **wait** if reset is within the next hour; **stop** if reset is more than 12 hours away *and* no other rung can run; **ask** if other rungs still work but no downgrade instruction exists. `--use-other-models-ttl <minutes>` (0 allowed) is how long `ask` waits for a human before continuing on working rungs.

Factory default (does **not** rewrite pin 3): `routing.tierFallback.maxDifficulty: high` and `includeReview: true` **are** the downgrade instruction. Pin 3’s “no instruction → ask” path is the **opt-out**. Default `includeForced` is **false**. `ask`-continue uses the **same eligibility** as `tierFallback`; if the operator forbade downgrade, TTL expiry **defers**, it does not continue.

---

## PR-3 scope lock (read every iteration)

In scope: `--use-other-models-ttl` on `loop run` / `batch run` (nested + deprecated flat; 0 allowed; overrides `askTtlMinutes` for this run); re-evaluate `usagePolicy` + `routing.tierFallback` on the stop-check cadence when TTL > 0; rung blackout expiry map; down-only walker in `resolve_execution_plan` (never `model_for`); family-match explicit `tasks.model` at resolve; `includeForced` per-task; long-horizon stop-this-PRD + next PRD inherits unavailable (batch/process-local); overflow skip of blacked rungs; `models set-usage-rule` / `set-tier-fallback` / `unset-tier-fallback` (JSON **null**); `models show` policy; remaining numbers only with the `list --remote` live-fetch gate.

**Already shipped on main — do not reimplement, do not spawn as “helpful” follow-ups:**

- `evaluate_quota` / `apply_quota` split (evaluate does not emit `ask`)
- Remaining-percent unit, `% left` banners, `LOOP_USAGE_REMAINING_MIN`, `LOOP_USAGE_THRESHOLD` preflight error
- Proto-channel replace-on-successful-evaluate / keep snapshot on API fail (upgrade HashSet → expiry map; do not replace the replace-rule)
- Factory `tierFallback` serde (absent key → Some; explicit null → None)
- `askTtlMinutes` sleep via `QuotaAccountAction::Ask` (CODE-FIX-010) — **override** it from clap, do not rewrite the sleep
- `HorizonStopped` vs operator `StopSignaled` (CODE-FIX-008)
- `handle_rung_only_empty_selection` (CODE-FIX-009)
- CODE-FIX-002 weekly-all >12h Stop; CODE-FIX-003 includeForced not a global forbid; CODE-FIX-005 pre-gate honors `LOOP_USAGE_CHECK_ENABLED`; CODE-FIX-006 explicit `onLow` on scoped buckets

**Out of scope:**

- Re-folding scoped windows into an account wait
- Dated month-name `parse_reset_from_output` as the **account** wait for a rung-scoped bucket
- New DB columns / persisting buckets or rung blackouts
- Interactive TTY prompt during `ask`
- Grok/Codex usage adapters
- Live Anthropic / real `~/.claude` credentials (verify-task-mgr must not pass `--remote` or set `TASK_MGR_USE_API`)

Do **not** edit `tasks/quota-rung-policy.json`, `tasks/quota-rung-policy-pr1.json`, `tasks/quota-rung-policy-pr2.json`, or `tasks/prd-quota-rung-policy.md`.

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

- Reimplementing PR-2 evaluate_quota/apply_quota, remaining-percent rename, proto-channel HashSet exclude, factory tierFallback serde, askTtlMinutes sleep (CODE-FIX-010), HorizonStopped vs operator_stopped (CODE-FIX-008), or handle_rung_only_empty_selection (CODE-FIX-009)
- Calling ResolvedModelsConfig::model_for for blackout clamp (bidirectional; can walk UP onto a blacked frontier)
- Storing fable / opus / claude-fable-5 in quota.rs, IterationContext, walker keys, or LoopResult inherit
- Reusing handle_quota_deferral for rung-only exhaustion (stale-abort, learning 3927 / 5088 / 5474)
- Documenting a wait loop as accepted for explicit off-ladder tasks.model (family-match at resolve; defer if unavailable and includeForced=false)
- Treating includeForced:false as a global forbid of factory unavailable (CODE-FIX-003 already forbids that; PR-3 is per-task family-match defer)
- TTL 0 sleeping; TTL > 0 as a deaf fixed sleep that ignores a mid-wait config write
- Computing effective_ttl only inside execute_quota_account_action so config askTtlMinutes 0 + --use-other-models-ttl 15 still Defer
- Converting factory/allowing unavailable+Proceed into Ask because a CLI TTL is set
- Treating expired expiry-map keys as active HashSet members (must use active_rungs(&map, now))
- Ask-continue on TTL expiry when the operator forbade downgrade (unset/null tierFallback, narrower maxDifficulty, includeReview false) — must defer
- Ask timeout setting was_stopped / stopping batch --chain; operator .stop during ask must still stop the chain
- Rung-scoped long-horizon stop not inheriting unavailable into the next PRD (next PRD re-stops instead of clamping to standard)
- Account-binding stop failing to stop the chain
- Printing remaining numbers from models show without the same live-fetch gate as models list --remote
- unset-tier-fallback deleting the key (serde then yields factory Some). Unset MUST write JSON null (ask opt-out)
- Spillover counting as a working rung for rung-scoped decisions
- quota.rs importing runners, clap, or SQLite; model.rs parsing OAuth JSON
- New DB columns; persisting last-fetched buckets or rung blackouts
- Collapsing dual Anthropic I/O predicates onto one flag
- Tests that require live Anthropic network or real ~/.claude credentials
- Driving `loop run` / `batch run` (verify-task-mgr refuses; `loop run --help` / `batch run --help` / `models` text are the safe probes)
- Inventing a second verification harness
- Manual edits to tasks/*.json for status (use task-mgr CLI / task-status tags)
- unwrap() in production paths
- Catch-all error handlers that swallow context

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` embedded in `## Current Task` are layered on top. If any of these fails, the task is not done.

- No warnings in `cargo check` output
- No warnings in `cargo clippy -- -D warnings` (per-iteration). `--all-targets` is REVIEW-001 only — do not run it on FEAT-006 / FEAT-007 / FEAT-008
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() in production code paths
- Sequential and wave paths stay parity-locked for account reactions (exhaustive destructure, shared coordinators)
- Engine keys are (Provider, CapabilityTier) only — no fable / opus / claude-fable-5 in quota.rs, engine.rs proto-channel, or walker state
- Do not reimplement PR-2 evaluate/apply, remaining-percent, proto-channel replace-on-evaluate, factory tierFallback serde, askTtlMinutes sleep, HorizonStopped, or handle_rung_only_empty_selection — extend them
- No live Anthropic / real ~/.claude credentials in unit tests; verify-task-mgr must not pass --remote or set TASK_MGR_USE_API

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Project Verification Skills, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

Do **not** edit `tasks/quota-rung-policy.json`, `tasks/quota-rung-policy-pr1.json`, `tasks/quota-rung-policy-pr2.json`, or `tasks/prd-quota-rung-policy.md`.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/quota-rung-policy-pr3.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Inspect this iteration's task          | `task-mgr show <TASK-ID>` using the task ID from `## Current Task`                                                                                                                 |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr3.json` — priority auto-computed; DB + PRD JSON updated atomically                                                   |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task (rare), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/quota-rung-policy-pr3.json
jq '.globalAcceptanceCriteria' tasks/quota-rung-policy-pr3.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/quota-rung-policy-pr3-prompt.md`   | This prompt file (read-only)                                               |
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

2. **Pull only the progress context you need** — most iterations want just the most recent section (the `tac | awk | tac` command above). If `## Current Task` lists a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log (`grep -n -A 40 '## .* - <THAT-TASK-ID>' tasks/progress-$PREFIX.txt`). Skip entirely on the first iteration (file won't exist).

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. That's the ONLY way to reach `tasks/long-term-learnings.md` / `tasks/learnings.md` content — **do not** Read those files directly; they grow unboundedly.

   **Never Read `CLAUDE.md` in full.** If the task description references a specific section, or the task touches a file that's likely documented there, `grep` for the relevant term and read only the surrounding lines:
   ```bash
   grep -n -A 10 '<keyword or header>' CLAUDE.md
   ```
   The authoritative per-task rules (Priority Philosophy, Prohibited Outcomes, Data Flow Contracts, Project Verification Skills, Key Context, and the CLAUDE.md excerpts that matter for this PRD) are already embedded in **this prompt file**. Prefer it over re-reading source docs. When a verification skill applies, Read that SKILL.md at verification time — do not paste it into the progress log.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed (`feat/quota-rung-policy-pr3`). Switch if wrong. Implement against **main + this branch** (PR-2 is already on local main `dacccb3`). Do not re-derive PR-2.

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

1. **ANALYSIS gate**: FEAT-006 and FEAT-007 already carry `consumerAnalysis` in this prompt's Data Flow / Feature-Specific Checks. Do not spawn a separate ANALYSIS task.
2. **Consumer Impact Table**:
   - `BREAKS` → implement the mitigation named on the task (CLI override of Ask execute; down-only clamp after all six rungs; inherit into next PRD). Split only if two semantic contexts truly need different code paths.
   - `NEEDS_REVIEW` → verify the call site before changing it.
   - `OK` → proceed.
3. **Semantic distinctions**: Ask-timeout vs operator `.stop` vs horizon Stop are different; factory clamp vs operator-forbade defer are different; account-binding chain-stop vs rung-scoped inherit are different — do not shoehorn them into one `was_stopped`.

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
cargo test --lib model
cargo test --lib account
cargo test --lib config
cargo test --test models_command
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
2. For the remaining unrelated failures: spawn a single `FIX-xxx` or `CLARIFY-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr3.json` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID so a human can route ownership.

Below the ~12-failure threshold, just fix them.

If a **Project Verification Skills** section is present, this gate also includes that skill's mapped-feature drive (see that section).

---

## Project Verification Skills

This repo ships a project-level verification skill. Language-level gates (fmt, type-check, lint, scoped tests) are **necessary but not sufficient** for user-facing changes the skill covers. Follow the skill literally — do not invent a second harness, and do not paste the skill body into the progress log.

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** `models-routing` (FEAT-008: extend `.claude/skills/verify-task-mgr/features/models-routing.md`; drive `models --help`, `models show`, `models set-usage-rule`, `models set-tier-fallback`, `models unset-tier-fallback`, `models show` again).
  **Help probes (capture names, not feature files — do not add `features/loop-run-help.md`):** FEAT-006 `$H capture loop-run-help -- loop run --help` and `$H capture batch-run-help -- batch run --help` must list `--use-other-models-ttl` (0 allowed). REVIEW-001 drives the mapped feature plus those two captures. The skill **refuses** `loop run` / `batch run`. Do not drive `models list --remote`. Remaining numbers on `models show` are gated like list `--remote` — prove they are **absent** offline.

**Per-iteration:** if this task is listed in **This PRD maps to** (or is a FIX / WIRE-FIX spawned from a mapped task), Read that SKILL.md (and the matching `features/*.md` if listed) and drive that recipe after the scoped language gate. Capture evidence where the skill says. A green compile/test run is not proof.

**REVIEW-001 / milestone:** drive every listed feature this PRD touched. A skipped sub-feature is reported skipped, not verified via a sibling path.

**Blocked skill:** if you cannot launch or a precondition fails, emit `<promise>BLOCKED</promise>` with the unmet precondition. Do not skip the drive and mark the task done.

The helper **refuses** `loop run`, `batch run`, and the deprecated flat `loop <prd>` / `batch <glob>` forms. `loop run --help` / `batch run --help` / `models` text are the safe probes.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production — CODE-REVIEW-1 verifies. Most common misses:

- Not registered in dispatcher/router → add `--use-other-models-ttl` to nested **and** flat parent clap bags **and** `resolve_loop_command` / `resolve_batch_command` **and** `main.rs` **and** `run_batch`
- Test mocks bypass real wiring → verify production path separately
- Config field read but not passed through → CLI `Some(0)` must not collapse to `None`; exhaustive `LoopConfig` destructure in `config.rs` tests must add the field
- `effective_ttl` only swapped in `execute_quota_account_action` after `ask_or_defer(policy.ask_ttl_minutes)` already chose Defer (config 0 + CLI 15 stays Defer)
- Expired expiry-map keys passed to HashSet callers without `active_rungs(&map, now)`
- Unused-import warning on new code → call sites missing
- `PlanContext` new field filled in `sequential.rs` but not `slot.rs` (seq/wave split)
- Down-only walker defined but `resolve_execution_plan` still returns before the post-resolve clamp (especially the `EXPLICIT_MODEL` early `return`)
- Family-match HUD tokens copied into `model.rs` / `quota.rs` instead of `pub(crate)` reuse of `usage.rs` ingest helpers
- `unset-tier-fallback` deletes the key (factory Some) instead of writing JSON `null`
- `models show` remaining percents without `check_opt_in`
- Overflow escalate still uses `model_for` onto a blacked rung
- Next PRD inherit not threaded through `LoopResult` → `run_batch` → next `IterationContext`
- Rung-only empty selection falls through `handle_quota_deferral`
- New CLI subcommand defined but not exported from `commands/models/mod.rs` / matched in `main.rs`

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REVIEW-001`) spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

### What each review looks for

| Review                  | Priority | Spawns (priority)                  | Before                  | Focus                                                                                                   |
| ----------------------- | -------- | ---------------------------------- | ----------------------- | ------------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | FEAT-006/007/008        | Language idioms, security, error handling, `qualityDimensions`, wiring, down-only walker, TTL re-eval, live-fetch gate |
| REVIEW-001              | 99       | `FIX-xxx`                          | all prior               | Full unscoped suite + PR-3 ship check                                                                   |

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
}' | task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr3.json
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

---

## Learnings Guidelines

Learnings live in `tasks/long-term-learnings.md` (curated) and `tasks/learnings.md` (raw, auto-appended). **Do not Read those files directly** during a loop iteration — they grow unboundedly. Instead:

- `task-mgr recall --for-task <TASK-ID>` — indexed retrieval of learnings scored for this task
- `task-mgr recall --query "<keywords>"` / `--tag <tag>` — targeted queries when recall is sparse

Record your own learnings with `task-mgr learn` so they're indexed for future recall. Don't append directly to those files.

**Write concise learnings** (1-2 lines each):
- GOOD: "`down_only_walker` uses `exact_model_for`; never `model_for`"
- BAD: "There is a function that walks capability tiers in descending order and it is important not to call model_for because that function can walk up."

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
3. Add via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr3.json` and commit
4. Output:

```
<promise>BLOCKED</promise>
```

---

## Milestones

`REVIEW-001` is the full-gate checkpoint: it proves the trunk is green before merge. It is NOT a sweep to rewrite remaining tasks.

### Milestone Protocol

1. Check all `dependsOn` tasks have `passes: true`. If any don't, the milestone can't run yet.
2. **Run the full quality gate** (unscoped format, type-check, lint, complete test suite). Drive verify-task-mgr feature `models-routing`, plus capture names `$H capture loop-run-help -- loop run --help` and `$H capture batch-run-help -- batch run --help` (not feature files).
3. **Leave the repo green.** For every failure, including pre-existing ones that predate this PRD:
   - Trivial fixes go in the milestone's own commit: `chore: REVIEW-001 - fix stale test <name>`.
   - Non-trivial failures → spawn a `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr3.json` with the failure's `verifyCommand`. The loop picks it up; the milestone re-runs when the FIX passes.
4. Mark `<task-status>REVIEW-001:done</task-status>` only when the full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

- **[5467] [5468]** `askTtlMinutes` already sleeps via `QuotaAccountAction::Ask` (stop-aware WaitFn). TTL 0: no sleep. TTL 15 → 900s. Stop mid-wait. PR-3 **overrides** this from clap and re-evals config on stop-check — do not rewrite the sleep.
- **[5427]** Ask/defer quota path surfaces `UsageCheckResult::Deferred`, not `StopSignaled`.
- **[4998] [5010] [5414]** Two independent clamps already exist (sparse-ladder nearest-defined `model_for`, extra-mark `exact_model_for`). Blackout clamp is a **third**, down-only walker. **Never** call `model_for` for it.
- **[5027] [5120]** `resolve_execution_plan` six-rung precedence; `tier_of` stays exact-match. Off-ladder `tasks.model` is `tier_of None` — family-match at resolve (human review item 4), do not wait-loop.
- **[5431] [5411]** Proto-channel is a sibling of `provider_blackouts` / `runner_overrides`. Key `(Provider, CapabilityTier)`. Replace on successful evaluate; keep snapshot on API fail. PR-3 adds expiry.
- **[5472] [5474] [3927] [5088]** Rung-only empty selection uses `handle_rung_only_empty_selection`. Misrouting it to `handle_quota_deferral` bumps stale-abort.
- **[5456] [CODE-FIX-002]** Same-provider rungs share `weekly_all`. Account-binding >12h Stop even if other rungs look runnable. Inherit is for **rung-scoped** stop-this-PRD only.
- **[CODE-FIX-003]** Factory `includeForced: false` must not globally forbid unavailable. PR-3 is per-task family-match defer.
- **[5297] [5298] [5301] [CODE-FIX-005]** Dual predicates unchanged. Pre-gate off when `LOOP_USAGE_CHECK_ENABLED=false` (keep snapshot).
- **[5075] [4866] [4138] [4126] [4868] [5371]** Account-global reactions once per wave; exhaustive destructure (no `..`); seq and wave share the inner.
- **[1810] [4373]** `IterationContext` is main-thread-only.
- **[4481] [5389]** No model-id literals in engine state / generated artifacts. HUD tokens stay in `usage.rs`.
- **[4955]** `preflight_validate_and_probe` is the shared loop/batch chokepoint.
- **[5150]** Use loop-engine-parity-auditor for dual-path account/routing changes.
- **[4753]** After a parallel-slot loop, fixture-reading test binaries may point at a pruned `-slot-N` worktree. `touch tests/<binary>.rs` and rebuild — not a code regression.
- **[5375]** PR-1 pin (frontier off Fable) is required for parallel/wave **until this PR's clamp**. Strike that recipe in docs once FEAT-007 lands.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` / `src/loop_engine/CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md` to pull just that block.

- **CONTRACT-LOG-001:** `ui::*` for product UX / CLI data / byte-locked operator contracts (stderr, exact bytes, NEVER tracing). `tracing` for internal diagnostics only.
- **Account-global reactions** fire **exactly once per wave**, never once per rate-limited slot. Production pre-iteration path is `run_account_quota_gate` (not `account_usage_gate`, which remains a parity-test helper).
- **Dual predicate:** pre-iteration `usage_params.enabled = LOOP_USAGE_CHECK_ENABLED && claude_provider_enabled`. Post-output `anthropic_account_io_allowed = claude_provider_enabled`. FR-002 Fable/rung-scoped phrasing still skips usage_gate **and** probe — do not undo that.
- **Blackout channel (FEAT-008 provider):** `IterationContext::provider_blackouts` is separate from `runner_overrides`. The PR-2/PR-3 proto-channel is a **sibling** keyed on `(Provider, CapabilityTier)` — do not record a **provider** blackout for rung-scoped unavailability. Spillover is **never** a working rung for rung-scoped decisions.
- **Single-home contract:** production entry + hermetic `_inner` + exhaustive param destructure (no `..`). Seq and wave share the inner. `tests/reaction_parity.rs` is the lock.
- **CapabilityTier / `tier_of`:** config exact-match. Substring tier classification is dead. The only allowed `contains` is ingest: API family token vs configured model **string**. Extra-mark is string equality of configured models (`exact_model_for`).
- **`model_for` is bidirectional nearest-defined (down, then up).** Do not call it for blackout clamp. Ingest may call `exact_model_for` to read a rung's configured model string.
- **`models` verbs** round-trip `serde_json::Value` (unrelated keys survive), reject legacy keys on mutating verbs, and print text (ignore `--format json`). `list --remote` requires `ANTHROPIC_API_KEY` + `TASK_MGR_USE_API=1`; verify-task-mgr unsets that env. There is no `models set-primary`.
- **Skills:** tests that spawn the real binary with an init-family command MUST set `HOME` to a tempdir. verify-task-mgr already does this.
- **Grok-only recipe is untouched** by this PR. Do not invent `models set-primary`.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments.

**Loop CLI TTL** (clap `Option<u64>` minutes → `LoopConfig` / run params):

```rust
// --use-other-models-ttl 15  → Some(15)
// --use-other-models-ttl 0   → Some(0)   // NOT None
// flag omitted               → None      // use config askTtlMinutes (default 0)
config.use_other_models_ttl: Option<u64>
// BEFORE apply / ask_or_defer — not only inside execute_quota_account_action:
effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)
// config askTtlMinutes 0 + CLI 15 → Ask { 15 }, not Defer
// Wire on LoopCommand::Run, BatchCommand::Run, Commands::Loop, Commands::Batch,
// resolve_loop_command, resolve_batch_command, main.rs, run_batch.
// Exhaustive LoopConfig destructure in config.rs tests must add the field.
```

**Ask execute (already shipped — override, do not rewrite):**

```rust
QuotaAccountAction::Ask { ttl_minutes }  // Ask path only, when effective_ttl > 0
QuotaAccountAction::Defer                // Ask path, effective_ttl 0
// Factory/allowing never emits Ask — continue-via-unavailable (PR-2 apply).
// Ask/Defer is opt-out / explicit onLow ask only.
// CODE-FIX-010 sleeps ttl_minutes*60 via stop-aware wait then WaitedAndReset — keep it.
// PR-3: pass effective_ttl into ask_or_defer at apply time (config 0 + CLI 15 → Ask { 15 }).
// Do not only swap ttl inside execute_quota_account_action.
// On each WaitTiming.stop_check_secs tick during Ask, re-read usagePolicy +
// routing.tierFallback only (not a whole-run ProjectConfig reload).
// Timeout: continue iff tier_fallback_allows, else Deferred.
// wait() false (.stop) → StopSignaled (chain stops). Timeout → not was_stopped.
```

**tierFallback (already shipped serde):**

```rust
config["routing"]["tierFallback"]["maxDifficulty"]  // "low"|"medium"|"high"; default "high"
config["routing"]["tierFallback"]["includeReview"]  // default true
config["routing"]["tierFallback"]["includeForced"]  // default false
// absent key → factory Some; explicit JSON null → None → ask opt-out
// unset-tier-fallback MUST write JSON null, not delete the key
tier_fallback_allows(fb, work)  // includeForced false is NOT a global forbid (CODE-FIX-003)
```

**Rung blackout expiry (upgrade PR-2 HashSet):**

```rust
// IterationContext — main-thread only
unavailable_rungs: HashMap<(Provider, CapabilityTier), u64 /* unix expiry */>
// replace the map on each successful evaluate (same rule as PR-2 HashSet)
// keep snapshot on API fail
fn active_rungs(map: &HashMap<(Provider, CapabilityTier), u64>, now: u64)
    -> HashSet<(Provider, CapabilityTier)>  // expiry > now only
// HashSet callers (compute_quota_excluded_ids, handle_rung_only_empty_selection,
// PlanContext) stay HashSet via the adapter — do not retouch iteration.rs /
// orchestrator.rs / wave_orchestration.rs / wave_scheduler.rs /
// reaction_parity.rs / model_selection_engine_edges.rs
// synthetic CLI rung-scoped RateLimit → expiry = now + 3600 even when spillover is off
// never provider_blackouts.record for rungs
decision.unavailable.iter().any(|(p, t)| *p == Provider::Claude && *t == CapabilityTier::Frontier)
```

**Down-only walker (new, model.rs — must not call `model_for`):**

```rust
// blacked = active_rungs(&map, now)  // HashSet of unexpired keys
// (provider, start_tier, blacked: &HashSet<(Provider, CapabilityTier)>) -> Option<CapabilityTier>
// Walk CapabilityTier::ALL descending. Skip >= start. Skip exact_model_for None.
// Skip blacked. First remaining defined non-blacked LOWER rung, or None.
// Post-resolve clamp AFTER all six rungs including EXPLICIT_MODEL.
// Discriminator: grok standard-only + standard blacked → None, never up.
```

**Family-match explicit `tasks.model` at resolve (reuse ingest adapter):**

```rust
// usage.rs (ingest-only HUD tokens — pub(crate) reuse, do not copy into model.rs):
hud_tier_from_label("Fable") → Some(Frontier)   // case-insensitive prefix/token
family_token / map_unlabeled_token against configured exact_model_for strings
// resolve_execution_plan EXPLICIT_MODEL: if tier_of is None, family-match the
// explicit tasks.model STRING. If that family maps to an unavailable rung and
// includeForced=false → defer (exclude id). Do not wait-loop. Do not substring tier_of.
```

**Next-PRD inherit (batch/process-local):**

```rust
// LoopResult carries the expiry map (not SQLite, not runner_overrides)
// run_batch seeds the next PRD's IterationContext from it
// Receiver: active_rungs(&map, now) so expired keys are not active
// Do not restate HorizonStopped / account-binding chain-stop here (PR-2 + FEAT-006)
```

**models CLI (sparse Value round-trip):**

```rust
// set_json_path + validate_and_write (copy handle_set_tier)
config["usagePolicy"]["rules"][i]["kind"]
config["usagePolicy"]["rules"][i]["id"]
config["usagePolicy"]["rules"][i]["onLow"]  // wait|unavailable|stop|ask|ignore
config["routing"]["tierFallback"] = { maxDifficulty, includeReview, includeForced }
config["routing"]["tierFallback"] = null    // unset-tier-fallback — ask opt-out
// models show: policy from config always
// remaining numbers: only if check_opt_in() succeeds (TASK_MGR_USE_API=1 + key),
// same gate as models list --remote. Offline show: no `% left`.
```

**Existing coordinators (do not break PR-1 FR-002 or PR-2 apply):**

```rust
// Narrow Fable/rung-scoped CLI still: Wait { blackout_fallback_secs } ignoring api/output;
// no Blackout; no usage_gate / probe. PR-3 additionally writes synthetic frontier
// unavailable with 3600 expiry.
handle_rung_only_empty_selection(...)  // keep; never handle_quota_deferral
UsageCheckResult::HorizonStopped       // quota stop-this-PRD; not was_stopped
WaitTiming { stop_check_secs: 10, probe_secs: 30, status_secs: 12*60 }
MAX_WAIT_SECS // 5 * 3600 — reuse
```

---

## Feature-Specific Checks

- **Do not reimplement PR-2.** Evaluate/apply, remaining unit, proto-channel replace-rule, factory serde, Ask sleep, HorizonStopped, handle_rung_only_empty_selection are done. Extend them.
- **CLI `Some(0)` ≠ omitted.** Exhaustive `LoopConfig` destructure in `config.rs` must add `use_other_models_ttl`. Nested + flat + batch + `resolve_*` + `main.rs` + `run_batch`.
- **`effective_ttl = cli.unwrap_or(policy.ask_ttl_minutes)` before apply / `ask_or_defer`.** Config 0 + CLI 15 must emit `Ask { 15 }`, not Defer. Do not only swap ttl in `execute_quota_account_action`. Keep CODE-FIX-010 sleep.
- **Factory/allowing is continue-via-unavailable** (PR-2 apply). CLI TTL must not convert it into Ask. Ask/Defer is opt-out / explicit onLow ask only.
- **Ask-continue eligibility ≡ `tier_fallback_allows`.** Forbade → defer on TTL expiry. Ask-path TTL 0 → Defer, no sleep. Ask-path TTL > 0 re-reads `usagePolicy` + `tierFallback` on stop-check only — not a whole-run config reload (batch still caches the rest).
- **Down-only walker, not `model_for`.** Post-resolve after all six rungs including `EXPLICIT_MODEL`. Both prompt builders construct `PlanContext`. HashSet callers go through `active_rungs(&map, now)`.
- **Family-match at resolve.** Off-ladder `tasks.model` (`tier_of` None) maps via the ingest adapter. Unavailable + `includeForced=false` → defer. No accepted wait loop.
- **`includeForced` is per-task.** Do not regress CODE-FIX-003 (factory + `has_forced` still Proceeds).
- **Next PRD inherits the expiry map** (process-local). Receiver uses `active_rungs`. Do not restate HorizonStopped on FEAT-007.
- **`unset-tier-fallback` writes JSON `null`**, not key deletion.
- **`models show` remaining numbers** only behind `list --remote` live-fetch gate. Offline show = policy only.
- **Never `loop run` / `batch run` / `models list --remote`** through verify-task-mgr. Help + models text + evidence captures are the proof of the operator surface.
- **PR-1 pin** is no longer required for parallel/wave once FEAT-007 clamp exists — reword docs at REVIEW-001.

---

## Important Rules

- Work on **ONE story per iteration**
- **For high-effort tasks** (`estimatedEffort: "high"` or 10+ acceptance criteria): consider using `/ralph-loop` to iterate within the task until all acceptance criteria pass, e.g.:
  `/ralph-loop "Implement [TASK-ID]: [title]. Criteria: [list]. Output <promise>DONE</promise> when all pass." --max-iterations 10`
- **Commit frequently** after each passing story
- **Keep CI green** - never commit failing code
- **Read before writing** - always read files first
- **Minimal changes** - only implement what's required
- **Check existing patterns** - reaction coordinators, WaitSpy / IoSeamSpy, exhaustive destructure, models handler `set_json_path`
