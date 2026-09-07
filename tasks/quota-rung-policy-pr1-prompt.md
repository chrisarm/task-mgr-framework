# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Quota rung policy PR-1 (account-binding parse + Fable CLI RateLimit coordinator)** for **task-mgr**.

## Problem Statement

`parse_oauth_usage_json` (`src/loop_engine/usage.rs`) folds **every** utilization window — including named `seven_day_opus` / `seven_day_sonnet` and `limits[].kind = weekly_scoped` (HUD: Current week (Fable)) with `severity: critical` at 95% used — into one `UsageInfo.percentage = max(used)` and one `reset_at`. The pre-iteration gate then waits whenever that max is ≥ `usage_threshold` (92). Live HUD: session 24% used, weekly-all 55% used, Fable weekly 95% used — the loop parks until Sep 12 while standard/session still have headroom.

A second bug: `"You've reached your Fable limit. … switch models with /model."` matches neither `hit your` nor `usage limit`, so `is_rate_limited` misses it and the iteration classifies as a **crash**, burning auto-block budget. Even after classification, `decide_account_rate_limit` prefers `api_secs` (session ~5h or weekly ~6d), spillover records a provider Blackout, unknown-reset falls through to `usage_fallback_wait` (300s), and `react_to_outputs` always wires `usage_gate` + `probe_rate_limit_lifted` (30s CLI probe with no `-m` lifts the wait).

**This list ships PR-1 only** (FR-001 + FR-002 / US-001 + US-002). After PR-1: the false account park is gone (gate uses account-binding **used** 55% < 92); Fable/rung-scoped CLI is `RateLimit` with a **narrow** 3600s Wait that ignores API/output secs, does not Blackout the provider, and does not early-lift. Remaining `% left` banners, horizon heuristic, ask TTL, and rung blackout stay PR-2 / PR-3. No auto-downgrade in this slice.

**PR-1 operator recipe (required for parallel/wave):** after merge, `task-mgr models set-tier claude frontier <standard-model>` until PR-3. One Fable RateLimit in `react_to_outputs` sleeps the **whole wave** 3600s, so the pin is **required** for parallel/wave, not recommended. Sequential without the pin: that task waits 3600s (accepted).

Pins (do not rewrite):

1. A low **frontier** (HUD: “Current week (Fable)”) bucket is **not** an account emergency — continue on **standard** (and cheaper rungs). PR-1 does this by omitting scoped windows from the account fold + the operator pin; automatic clamp is PR-3.
2. Engine language is **capability rungs** (`frontier` / `standard` / `cost-efficient` / `cheapest`), never model ids (`fable`, `opus`, `claude-fable-5`) except at the ingest adapter that maps an API label onto a rung. PR-1 may match `fable` only in `detection.rs` ingest of CLI phrasing.
3. Default action is a **horizon heuristic** — **PR-2 / PR-3**. Do not implement wait/ask/stop, `--use-other-models-ttl`, or `tierFallback` here.

---

## PR-1 scope lock (read every iteration)

In scope: `usage.rs` account-binding fold; `detection.rs` RateLimit phrasing; `reactions/account.rs` coordinator contract; `tests/reaction_parity.rs` seq/wave + IoSeamSpy; CLAUDE.md pin recipe.

**Out of scope (do not implement, do not spawn as “helpful” follow-ups):**

- `src/loop_engine/quota.rs`, `evaluate_quota`, `QuotaBucket`, `UsagePolicy`
- Remaining-percent rename / `% left` banners / `LOOP_USAGE_REMAINING_MIN`
- Horizon heuristic, `ask`, `--use-other-models-ttl`, `routing.tierFallback`
- Rung blackout channel / down-only walker / `set-usage-rule`
- Dated month-name `parse_reset_from_output` (`Sep 12`)
- Automatic frontier→standard
- New CLI flags
- Live Anthropic / real `~/.claude` credentials

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

- Folding weekly_scoped / seven_day_opus / seven_day_sonnet / severity=critical / is_active into UsageInfo.percentage, reset_at, or exhausted
- Storing remaining percent in UsageInfo.percentage (must stay used 0–100; remaining rename is PR-2)
- Treating a frontier weekly bucket as an account wait
- Classifying Fable/rung-scoped CLI as Crash or incrementing consecutive-failure / auto-block
- Keying the 3600 Wait on every RateLimit or on `/model` alone
- Keying the 3600 Wait on plain `reached your` ∧ `limit` (no model token, no “switch models”)
- Using api_secs or output_secs for the narrow Fable/rung-scoped Wait (must ignore both)
- RateLimitAction::Blackout or provider_blackouts.record on Fable/rung-scoped phrasing even when spillover_enabled
- Running usage_gate / probe_rate_limit_lifted for that phrasing (undoes the 3600s Wait in ~30s)
- Adding dated Sep 12 / month-name parse_reset_from_output in this PR (keep None for `sep` tokens)
- Falling through the unknown-reset else to usage_fallback_wait (300s) for Fable phrasing
- Automatic frontier→standard, new quota.rs, evaluate_quota, --use-other-models-ttl, remaining % left banners, or a rung-blackout channel
- Collapsing dual Anthropic I/O predicates onto one flag
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
- No warnings in `cargo clippy -- -D warnings` (per-iteration). `--all-targets` is REVIEW-001 only — do not run it on FIX-001 / FIX-002
- `cargo fmt --check` passes
- Scoped tests for touched modules pass
- No unwrap() in production code paths
- Sequential and wave paths stay parity-locked for account reactions (exhaustive destructure, shared coordinators)
- UsageInfo.percentage remains used 0–100; compare stays `percentage >= usage_threshold` (default 92)
- No new CLI flags in this PR
- No live Anthropic / real ~/.claude credentials in tests

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about this iteration's task is embedded in `## Current Task`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Project Verification Skills, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

Do **not** edit `tasks/quota-rung-policy.json` (full-vision list) or `tasks/prd-quota-rung-policy.md`.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/quota-rung-policy-pr1.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this PRD.

### Commands you'll actually run

| Need                                   | Command                                                                                                                                                                           |
| -------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Inspect this iteration's task          | `task-mgr show <TASK-ID>` using the task ID from `## Current Task`                                                                                                                 |
| List remaining tasks (debug only)      | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task    | `task-mgr recall --for-task $PREFIX-TASK-ID` (also: `--query <text>`, `--tag <tag>`)                                                                                              |
| Add a follow-up task (review spawns)   | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr1.json` — priority auto-computed; DB + PRD JSON updated atomically                                                   |
| Mark status                            | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) — loop engine routes through `task-mgr` and syncs the JSON |

If you genuinely need a top-level PRD field that's not surfaced per-task (rare), pull it with `jq`, never a full Read:

```bash
jq '.requires' tasks/quota-rung-policy-pr1.json
jq '.globalAcceptanceCriteria' tasks/quota-rung-policy-pr1.json
```

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/quota-rung-policy-pr1-prompt.md`   | This prompt file (read-only)                                               |
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

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed (`feat/quota-rung-policy-pr1`). Switch if wrong.

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

FIX-001 and FIX-002 are localized to `src/loop_engine/` (plus `tests/reaction_parity.rs` for FIX-002). There is **no ANALYSIS-xxx** in this list — callers are documented on each task's `consumerAnalysis`. Do not spawn ANALYSIS. Do not split unless CODE-REVIEW-1 finds a BREAKS context that cannot share one implementation.

1. **ANALYSIS gate**: skip — not required for this slice.
2. Honor the semantic distinctions already on the task (account-binding vs rung-scoped; Fable phrasing vs 4pm account copy vs plain session limit).
3. If you discover a second semantic context that would BREAK a caller, split via `task-mgr add` rather than shoehorn.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **milestones** run the full gate and must leave the repo fully green (including pre-existing failures).

### Per-iteration scoped gate (implementation / test / fix tasks)

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
# Rust — scope tests to the touched crate/module (grep touchesFiles to pick)
cargo fmt --check
cargo check                                         # fast type check
cargo clippy -- -D warnings                 # per-iteration; --all-targets is REVIEW-001 only
# FIX-001
cargo test --lib loop_engine::usage
# FIX-002
cargo test --lib loop_engine::detection
cargo test --lib loop_engine::reactions::account
cargo test --test reaction_parity
```

Scoping heuristic: start from `touchesFiles`. Do **NOT** run the entire workspace test suite (`cargo test` with no filter) during regular iterations — that's REVIEW-001's job.

**Project verification skill:** PR-1 has **no new CLI flags**. Proof of parse + detection + wait is unit / coordinator tests. After the language gate on REVIEW-001, Read `.claude/skills/verify-task-mgr/SKILL.md` and drive **`loop run --help` only**. The skill **refuses** `loop run` / `batch run`. Do not invent a second harness. No live Anthropic.

### Final gate at REVIEW-001 (the milestone)

The single `REVIEW-001` task at the end of the lean path runs the **full, unscoped** suite on a clean checkout and must finish green. There are no separate MILESTONE-1 or MILESTONE-2 tasks.

```bash
cargo fmt --check && cargo check && cargo clippy --all-targets -- -D warnings && cargo test
```

If ANY test fails — including pre-existing failures that predate this PRD — the milestone fixes them. Default: **attempt every failure**, even ones that look out-of-scope. They become scope the moment the milestone gates the phase on the full suite being green. Trunk-green is the invariant this mechanism exists to protect.

Pragmatic escape hatch: if there are **more than ~12 failures AND they're all clearly unrelated to this PRD**, don't try to do all of them inline. Triage:

1. Fix everything you can attribute to this PRD's changes, inline in the milestone commit.
2. For the remaining unrelated failures: spawn a single `FIX-xxx` or `CLARIFY-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr1.json` listing the failing test names + error summaries, and `<promise>BLOCKED</promise>` with that task ID so a human can route ownership.

Below the ~12-failure threshold, just fix them.

If `cargo test` fails with paths to a removed `-slot-N` worktree, `touch tests/<binary>.rs` and rebuild — that is a stale-binary cache, not a code regression (learning #4753).

---

## Project Verification Skills

This repo ships a project-level verification skill. Language-level gates (fmt, type-check, lint, scoped tests) are **necessary but not sufficient** for user-facing changes the skill covers. Follow the skill literally — do not invent a second harness, and do not paste the skill body into the progress log.

- **`verify-task-mgr`** — `.claude/skills/verify-task-mgr/SKILL.md`
  Drive the task-mgr CLI the way an operator would — isolated --dir + HOME sandbox, no PATH binary, no checkout .task-mgr. Use to prove init/import, task lifecycle, learnings, models routing, or status/doctor after a user-facing CLI change.
  Feature map: `.claude/skills/verify-task-mgr/features/README.md`
  **This PRD maps to:** no new CLI surface. Safe probe is `loop run --help` only (the skill refuses `loop run` / `batch run`). Proof of parse / detection / wait is unit + coordinator tests. Do not drive `models list --remote`. No live Anthropic.

**Per-iteration:** FIX-001 / FIX-002 are not user-facing CLI changes — scoped unit/coordinator tests are the proof. Do not spawn a live loop to “see usage wait.”

**REVIEW-001 / milestone:** Read the skill, `$H launch`, `$H sandbox-new`, `$H doctor`, then `$H capture loop-run-help -- loop run --help`. Confirm the binary from this checkout prints help and that the helper would refuse an actual `loop run`. Capture evidence. A skipped sub-feature (models-routing remaining numbers, live loop) is reported skipped, not verified via a sibling path.

**Blocked skill:** if you cannot launch or a precondition fails, emit `<promise>BLOCKED</promise>` with the unmet precondition. Do not skip the drive and mark REVIEW-001 done.

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production — CODE-REVIEW-1 verifies. Most common misses for this slice:

- Parse fold updated but `UsageInfo` rustdoc still says “max across all windows including limits[]”
- Narrow 3600 predicate implemented only in `decide_account_rate_limit` while `react_to_outputs_with_io_seams` still wires `usage_gate` + probe (test d fails in production even if inner tests pass)
- Fable phrasing still takes the spillover `Blackout` branch (`if spillover_enabled` currently returns before any phrasing check)
- Unknown-reset `else` still uses `fallback_wait` 300 instead of `blackout_fallback_secs` 3600
- Test (b) written with `api_secs=None` (passes for the wrong reason)
- `AccountReactionParams` destructure uses `..` (breaks the parity lock)
- Dated Sep 12 parser added “to be helpful”
- Remaining invert: `percentage = 100.0 - util` (live fixture luckily proceeds, inverse fails)
- New code not called from `analyze_output` / `react_to_outputs` production entry

---

## Contract Tasks

No `CONTRACT-xxx` in this PR-1 list (CONTRACT-001 is a PR-2 predecessor). Do not spawn one.

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REVIEW-001`) spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

### What each review looks for

| Review                  | Priority | Spawns (priority)                  | Before                  | Focus                                                                                                   |
| ----------------------- | -------- | ---------------------------------- | ----------------------- | ------------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | FIX-001 + FIX-002       | Language idioms, dual predicate, used-percent, narrow 3600 predicate, wiring, seq/wave parity           |
| REVIEW-001              | 99       | `FIX-xxx`                          | CODE-REVIEW-1 + spawned | Full unscoped suite + PR-1 independent-ship check + verify-task-mgr `loop run --help`                    |

Use the **rust-python-code-reviewer** / **loop-engine-parity-auditor** when reviewing code. Document findings in the progress file. Do not implement PR-2/PR-3 under the guise of review.

### Spawning follow-up tasks

```sh
echo '{
  "id": "CODE-FIX-001",
  "title": "Fix: <specific issue>",
  "description": "From CODE-REVIEW-1: <details>",
  "rootCause": "<file:line + issue>",
  "exactFix": "<specific change>",
  "verifyCommand": "<shell command that proves the fix — unit/coordinator test, not loop run>",
  "acceptanceCriteria": ["Issue resolved", "No new warnings"],
  "priority": 14,
  "estimatedEffort": "high",
  "touchesFiles": ["affected/file.rs"]
}' | task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr1.json
```

`--depended-on-by` wires the new task into REVIEW-001's `dependsOn` AND syncs the PRD JSON atomically — don't edit the JSON yourself. Commit with `chore: <REVIEW-ID> - Add <FIX> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`. If no issues found, emit the status with a one-line "No issues found" in the progress file.

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

- GOOD: "`decide_account_rate_limit` Fable branch must run before the spillover `if` or Blackout wins"
- BAD: "When implementing the Fable coordinator it is important to remember that the spillover enabled path currently returns Blackout before any phrasing check so you have to reorder..."

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify REVIEW-001 passes

If verified:

```
<promise>COMPLETE</promise>
```

### Blocked Condition

If blocked (missing dependencies, unclear requirements):

1. Document blocker in the progress file
2. Create clarification task (e.g., `CLARIFY-001` with priority 0)
3. Add via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr1.json` and commit: `chore: Add blocker task CLARIFY-001`
4. Output:

```
<promise>BLOCKED</promise>
```

Do **not** interview the operator. If a requirement is genuinely missing, PAUSE via BLOCKED.

---

## Milestones

REVIEW-001 is the only milestone. It is a **full-gate checkpoint**: prove the trunk is green. It is NOT a sweep to rewrite remaining tasks or to start PR-2.

### Milestone Protocol

1. Check all `dependsOn` tasks have `passes: true`. If any don't, the milestone can't run yet.
2. **Run the full quality gate** (unscoped format, type-check, lint, complete test suite). Drive verify-task-mgr `loop run --help` as specified above.
3. **Leave the repo green.** For every failure, including pre-existing ones that predate this PRD:
   - Trivial fixes go in the milestone's own commit: `chore: REVIEW-001 - fix stale test <name>`.
   - Non-trivial failures → spawn a `FIX-xxx` task via `task-mgr add --stdin --depended-on-by REVIEW-001 --from-json tasks/quota-rung-policy-pr1.json` with the failure's `verifyCommand`. The loop picks it up; the milestone re-runs when the FIX passes.
4. Mark `<task-status>REVIEW-001:done</task-status>` only when the full gate is green.

---

## Reference Code

Current fold (`src/loop_engine/usage.rs` ~218–267) — **change this**:

```rust
for key in ["five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet"] { /* ... */ }
// limits[]: every row, exhausted = percent >= 100.0 || severity == "critical"
let percentage = windows.iter().map(|w| w.util).fold(0.0_f64, f64::max);
let reset_at = soonest_reset(windows.iter().filter(|w| w.exhausted) ...)
    .or_else(|| five_hour)
    .or_else(|| soonest any);
```

Target fold: named keys `five_hour`, `seven_day` only; `limits[]` only `kind` `session` / `weekly_all`; `percentage` = max **used**; `reset_at` = **latest** among account-binding windows with used ≥ 92, else prefer session. Band discriminator: weekly-all **95** + session **50** → `percentage = 95`, `reset_at` = weekly (old `exhausted=≥100` keeps session). Update `UsageInfo` / `parse_oauth_usage_json` rustdoc to match. Tests call `parse_oauth_usage_json` only — never `check_and_wait` / `load_usage_info`.

Current `is_rate_limited` (`detection.rs` ~151–166) misses `You've reached your Fable limit` (no `hit your`, no adjacent `usage limit`).

Current `decide_account_rate_limit` (`account.rs` ~211–230): spend-stop, then `if spillover_enabled { Blackout { resolve_wait_secs(api, output, blackout_fallback_secs) } }`, else `Wait { resolve_wait_secs(api, output, fallback_wait) }` with `fallback_wait` default 300. Fable branch must win **before** spillover Blackout and must use `blackout_fallback_secs` even when spillover is off.

Current production wait (`account.rs` ~370–386): if Claude+env → `usage_gate`; then `reset_wait` with probe wired whenever Claude is allowed. Narrow Fable phrasing must skip **both**.

Hermetic test seams already exist: `decide_account_rate_limit` unit tests in `account.rs`; `WaitSpy` + `react_to_outputs_inner` and `IoSeamSpy` + `react_to_outputs_with_io_seams` in `tests/reaction_parity.rs`. Reuse them. Test (g) shape: one Fable `RateLimit` item + two `Completed` siblings, `WaitSpy.calls == 1`, `last_secs == 3600`, `blackout.active` empty even with `spillover_enabled = true`. Test (h): output containing only `/model` must not take the 3600 override. Non-Fable fixture: `You've reached your Opus limit` → Wait 3600.

`check_and_wait` (`account.rs` ~1068): `if usage.percentage < f64::from(threshold) { return BelowThreshold; }`. Live fixture 55 < 92 proceeds by construction once parse is fixed. FIX-001 tests must not call `check_and_wait` / `load_usage_info` (network).

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here (then use `task-mgr recall --query <text>`, not a full Read).

- **[5297]** Dual predicates for Anthropic I/O: pre = `LOOP_USAGE_CHECK_ENABLED ∧ Claude enabled`; post allow-flag = Claude only. Do not collapse.
- **[5298]** Collapsing dual predicates onto one flag either kills RateLimit recovery or leaves Grok-only loops hanging on OAuth.
- **[5301]** Post RateLimit: usage-API load ANDs env; early-lift probe is Claude-only (env does not apply). FR-002 additionally skips **both** for Fable/rung-scoped phrasing even when Claude is enabled.
- **[5075] [4866] [4171] [4151]** Account-global reactions fire exactly once per wave, not per slot. Wave coordinators fold N slot outcomes into one `react_to_outputs`.
- **[4126] [4182]** Sequential and wave must share the same coordinator (`react_to_outputs` / `_inner`); exhaustive param destructure (no `..`) is the parity lock.
- **[5090]** `parse_reset_from_output` fallback handles malformed quota messages — keep `None` for unparseable tokens. Do not add dated Sep 12 parsing in PR-1.
- **[5088]** Deferral-first ordering prevents false stale-abort on quota exhaustion. PR-1 Fable CLI must **not** record `provider_blackouts` (would trip this path / learning 3927).
- **[5087] [5076]** `BlackoutState` is a separate ephemeral channel from `runner_overrides`. Fable phrasing must leave it untouched.
- **[4753]** After a parallel-slot loop, fixture-reading test binaries may point at a pruned `-slot-N` worktree. `touch tests/<binary>.rs` and rebuild — not a code regression.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets were extracted from `CLAUDE.md` / `src/loop_engine/CLAUDE.md` for the subsystems this PRD touches. They're the only CLAUDE.md content you need for iteration work — do NOT Read the full file. If a task description cites a section name not shown here, `grep -n -A 10 '<section header>' CLAUDE.md` to pull just that block.

- **CONTRACT-LOG-001:** `ui::*` for product UX / CLI data / byte-locked operator contracts (stderr, exact bytes, NEVER tracing). `tracing` for internal diagnostics only.
- **Account-global reactions** (`account_usage_gate`, `react_to_outputs`) fire **exactly once per wave**, never once per rate-limited slot.
- **Dual predicate:** pre-iteration `usage_params.enabled = LOOP_USAGE_CHECK_ENABLED && claude_provider_enabled`. Post-output `anthropic_account_io_allowed = claude_provider_enabled` (never from the env flag). Production wait then splits: `check_and_wait` requires both flags; `probe_rate_limit_lifted` requires Claude-only. FR-002 adds a third exception: Fable/rung-scoped phrasing skips **both** seams.
- **Blackout channel (FEAT-008):** `IterationContext::provider_blackouts` is separate from `runner_overrides`. Written only by the account rate-limit reaction. Rung-scoped CLI phrasing must **not** record a provider blackout even when spillover is on.
- **Single-home contract:** production entry + hermetic `_inner` + exhaustive param destructure (no `..`). Seq and wave share the inner. `tests/reaction_parity.rs` is the lock.
- **`LOOP_USAGE_CHECK_ENABLED` is not a Claude kill-switch** and is not a Fable-phrasing kill-switch. Env=false still classifies post-output RateLimit.
- **Grok-only recipe is untouched** by this PR. Do not invent `models set-primary`.
- **Skills:** tests that spawn the real binary with an init-family command MUST set `HOME` to a tempdir. verify-task-mgr already does this.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types from variable names or comments.

**OAuth JSON object window** (`serde_json::Value` string keys → `UsageInfo`):

```rust
json.get("five_hour")?.get("utilization")?.as_f64()  // used 0–100 as-is; 1.0 is 1%, not exhausted
json.get("five_hour")?.get("resets_at")?.as_str()
json.get("seven_day")  // weekly-all named key — account-binding
// Do NOT read seven_day_opus / seven_day_sonnet into the fold
```

**OAuth `limits[]`** (array of objects):

```rust
let kind = limit.get("kind").and_then(|v| v.as_str()); // account-binding: "session" | "weekly_all"
let percent = limit.get("percent").and_then(|v| v.as_f64())
    .or_else(|| limit.get("percent").and_then(|v| v.as_u64()).map(|u| u as f64));
limit.get("severity")  // display hint — MUST NOT set exhausted in PR-1
limit.get("is_active") // display hint — MUST NOT set exhausted in PR-1
limit["scope"]["model"]["display_name"]  // present on weekly_scoped; ignore for the PR-1 account fold
// Skip kind=weekly_scoped, extra_usage, promotional, unknown
```

**`UsageInfo` (PR-1):**

```rust
pub struct UsageInfo {
    pub percentage: f64,          // used 0–100; max of account-binding windows only
    pub reset_at: Option<String>, // latest among account-binding windows with used >= 92; else session
}
// check_and_wait: if usage.percentage < f64::from(threshold) { BelowThreshold }
// stderr may still print: Usage: 55.0% (threshold: 92%)
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

Expect `percentage ≈ 55`, `reset_at` = session. Inverse: set `seven_day` / `weekly_all` to 100, expect `percentage = 100`, `reset_at` = weekly. Band: session 50 + weekly-all 95 → `percentage = 95`, `reset_at` = weekly (not session). Parse-only — do not call `check_and_wait` / `load_usage_info`.

**Coordinator types:**

```rust
decide_account_rate_limit(
    api_secs: Option<u64>,      // IGNORE on narrow Fable/rung-scoped predicate
    output_secs: Option<u64>,   // IGNORE on narrow predicate
    output: &str,
    spillover_enabled: bool,    // must NOT Blackout on narrow predicate
    fallback_wait: u64,         // 300 — must NOT use on narrow predicate
    blackout_fallback_secs: u64 // 3600 — THIS is the Wait secs for the narrow predicate
) -> RateLimitAction::Wait { secs: blackout_fallback_secs }

// Narrow predicate (3600 override), not classification:
//   model token (fable|opus|sonnet|haiku) followed by "limit"
//   OR co-occurrence with "switch models"
// `/model` alone is NOT sufficient — test (h)
// `You've reached your Opus limit` → Wait 3600 (not contains("fable") only)
// Plain "reached your … limit" keeps api_secs (ordinary RateLimit)

AccountReactionParams { /* exhaustive destructure, no `..` */ }
IterationOutcome::RateLimit  // already excluded from handle_task_failure at both callers
```

**Wave test (g) items:** one `OutputReactionItem { outcome: RateLimit, output: Fable sentence }` + two `Completed`. `WaitSpy.calls == 1`, `last_secs == Some(3600)`, `!blackout.active(now).contains(&Provider::Claude)` even with `spillover_enabled = true`.

---

## Feature-Specific Checks

- **Used-percent, not remaining.** `percentage ≈ 55` on the live fixture. Storing remaining 45 luckily passes the live case and **fails** the weekly-all 100 inverse. Keep the existing stderr `Usage: 55.0% (threshold: 92%)`. Parse-only: do not call `check_and_wait` / `load_usage_info`.
- **95/50 band.** weekly-all used 95 + session 50 → `percentage = 95`, `reset_at` = weekly. Old `exhausted=≥100` keeps session reset (5h-cap on the wrong window). Live 55 and inverse 100 do not catch this.
- **Two predicates for FIX-002.** Classification (`is_rate_limited`) may widen to catch the live Fable sentence (and may classify plain session-limit copy as RateLimit). The **3600 Wait override** is narrower: model token + `limit`, or `switch models`. Not `/model` alone (test h). Not every RateLimit. Not plain `reached your … limit`. Non-Fable: `You've reached your Opus limit` → Wait 3600.
- **Tests (a)–(h) are mandatory**, including (b) with `api_secs = 6 days` populated, (g) the wave shape, and (h) `/model` alone. Do not treat “3600 not ~6 days” with `api_secs=None` as sufficient.
- **Pin recipe** must say **required for parallel/wave**, not recommended. Sequential without the pin: 3600s wait, accepted. No auto-downgrade in this slice.
- **`fable` substring** is allowed only in `detection.rs` CLI ingest. Do not store model ids on `IterationContext` / blackout keys.
- **Do not add** month-name parsing, `quota.rs`, CLI flags, remaining banners, or `tierFallback`.

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
