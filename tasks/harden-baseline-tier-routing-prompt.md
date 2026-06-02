# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Harden baseline-tier runner routing + framework follow-ups** for **task-mgr**.

## Problem Statement

The baseline-tier runner routing change on `main` (`primaryRunner.baselineTierRoutes`,
`fallbackToClaude`→`runtimeErrorFallback` + on-disk config migration) is correct on
the happy path but carries: (1) a config read path that is correct only by serde-alias
coincidence; (2) an untested on-disk config rewrite; (3) a **confirmed** baseline-tier
derivation divergence — recovery derives the baseline from `claude_fallback_model` and
omits `user_default`, so a recovering Codex task can match a different `baselineTierRoutes`
route than it was originally routed by; (4) a pre-existing `..` path-traversal gap in
`sanitize_branch_name`. On top of that, two foundational abstractions
(`compute_baseline_model`, `promote_once`) are extracted and the two largest god-modules
(`orchestrator.rs`, `wave_scheduler.rs`) are split along clean seams. Code and task-mgr
live in the SAME repo (no external git repo).

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry, decide how it'll be handled before coding.
3. **Pick an approach** — Only for `estimatedEffort: "high"` or `modifiesBehavior: true`, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration).

---

## Priority Philosophy

In order: **PLAN** → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** → **CORRECTNESS** → **CODE QUALITY** → **POLISH**.

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- `promote_once` mutating `IterationContext` (reintroduces dirty-ctx-on-rollback)
- Inserting `RunnerKind::Codex` into `runner_overrides` on a Codex→Claude promotion (must always insert Claude)
- Changing happy-path routing-precedence behavior or editing existing routing-precedence test assertions (beyond mechanical `..Default::default()`)
- Bundling a WS-3 god-module split with a logic change in the same commit
- Catch-all error handlers that swallow context

---

## Global Acceptance Criteria

These apply to **every** implementation task — task-level `acceptanceCriteria` layer on top.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: Scoped tests pass with `cargo test` (full suite at milestones)
- Rust: `cargo fmt --check` passes
- No literal model-id strings outside `src/loop_engine/model.rs` (`tests/no_hardcoded_models.rs`)
- Status mutations go through `TaskLifecycle` verbs, never raw `UPDATE tasks SET status`
- No breaking changes to documented happy-path routing precedence

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Everything you need is returned by `task-mgr next`; everything PRD-wide is already in **this prompt file** (authoritative copy). If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/harden-baseline-tier-routing.json)
```

Use `$PREFIX` in every CLI call below.

### Commands you'll actually run

| Need | Command |
| ---- | ------- |
| Pick + claim the next eligible task | `task-mgr next --prefix $PREFIX --claim` |
| Inspect one task | `task-mgr show $PREFIX-TASK-ID` |
| List remaining tasks (debug) | `task-mgr list --prefix $PREFIX --status todo` |
| Recall learnings for a task | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up task (review spawns) | `echo '{...}' \| task-mgr add --stdin --depended-on-by MILESTONE-N` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`) |

### Files you DO touch

| File | Purpose |
| ---- | ------- |
| `tasks/harden-baseline-tier-routing-prompt.md` | This prompt (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — **tail** for recent context, **append** after each task |

**Reading progress** — never Read the whole log:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read on the first iteration (file won't exist).

---

## Your Task (every iteration)

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/harden-baseline-tier-routing.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   If it reports no eligible task or unmet `requires`, output `<promise>BLOCKED</promise>` with the reason and stop.
2. **Pull only the progress context you need** — usually just the most recent section.
3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do NOT Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly. Do NOT Read `CLAUDE.md` in full — the excerpts you need are below; otherwise `grep -n -A 10 '<keyword>' CLAUDE.md`.
4. **Verify branch** — `git branch --show-current` matches the printed `branchName` (`feat/harden-baseline-tier-routing`).
5. **Think before coding** — assumptions, edge cases, approach. Cross-module data → use the Data Flow Contracts section below.
6. **Implement** — single task, code + tests in one coherent change.
7. **Run the scoped quality gate** (below — scoped tests, NOT the full suite).
8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`). For the WS-3 refactors use `refactor:` and keep each its OWN commit with no logic edits.
9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`.
10. **Append progress** — one tight block (format below), terminated with `---`.

---

## Quality Checks

### Per-iteration scoped gate (implementation / test / fix tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
# scope tests to the touched module — this is a single-crate workspace, so filter by module/fn:
cargo test --lib project_config        # for FIX-002/003, TEST-INIT-002
cargo test --lib loop_engine::model    # for CONTRACT-BASE-001, TEST-INIT-001
cargo test --test codex_recovery       # for FIX-001, TEST-001
cargo test --lib worktree              # for FIX-004, TEST-INIT-003
cargo test --test primary_runner_routing   # routing-precedence regression guard
```

Pick the filter(s) matching `touchesFiles`. **Do NOT** run the full unscoped `cargo test` during regular iterations — that's the milestone's job.

### Milestone gate (MILESTONE-1 / -2 / -FINAL)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test && cargo run --bin gen-docs -- --check
```

Must finish green, including pre-existing failures. Below ~12 unrelated failures, fix them inline; above that AND clearly unrelated, spawn a single `FIX-xxx` via `task-mgr add --stdin --depended-on-by <THIS-MILESTONE>` and `<promise>BLOCKED</promise>`.

---

## Behavior Modification Protocol (FIX-001 — `modifiesBehavior: true`)

FIX-001 changes recovery behavior. Its `ANALYSIS-001` dependency (priority 0) carries the
Consumer Impact + Semantic Distinctions tables and must be `passes: true` first. The key
semantic distinction to honor:

- **baseline-tier derivation** (recovery.rs:331-336) — the thing being fixed (use real
  project/user defaults via `compute_baseline_model`).
- **target_model selection** (recovery.rs:363-376) — a DISTINCT concern; decides WHICH
  Claude model to promote to. **Leave it unchanged.**

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

- Config field read but not passed through → wire through (FIX-001: both callers must pass the engine-cached defaults).
- New public fn / module defined but not called from production → grep-verify reachability.
- Unused-import warning on new code → call sites missing.
- A signature change that compiles but a caller silently passes `None` where a real value exists (INT-001 guards this for FIX-001).

---

## Review Tasks

| Review | Priority | Spawns (priority) | Focus |
| ------ | -------- | ----------------- | ----- |
| CODE-REVIEW-1 | 13 | `CODE-FIX` / `WIRE-FIX` (14-16) | no-ctx-mutation in promote_once, insert-safe-provider, recovery↔primary parity, config key-preservation, branch-name containment, no leaked model literals, reachability |
| REFACTOR-REVIEW-FINAL | 70 | `REFACTOR-xxx` (71-85) | full-context: behavior-neutrality of the WS-3 moves (spot-diff vs git history), SSoT of the two abstractions, DRY/complexity |

Use the **rust-python-code-reviewer** agent. Spawn fixes with
`echo '{...}' | task-mgr add --stdin --depended-on-by <MILESTONE>`; commit
`chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`; emit `<task-status><REVIEW-ID>:done</task-status>`.
If no issues, emit the status with a one-line "No issues found".

---

## Progress Report Format

APPEND to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight (~10 lines):

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence]
Files: [comma-separated paths]
Learnings: [1-3 bullets]
---
```

---

## Stop and Blocked Conditions

**COMPLETE** — only after all stories `passes: true`, all milestones pass, no new tasks pending:
```
<promise>COMPLETE</promise>
```

**BLOCKED** — document the blocker in the progress file, spawn a `CLARIFY-xxx` (priority 0) if needed, then:
```
<promise>BLOCKED</promise>
```

---

## Milestones

Gates, not sweeps. Check `dependsOn` all `passes: true` → run the FULL gate → leave the trunk green (fix pre-existing failures or route them to `FIX-xxx`) → mark `<task-status>MILESTONE-N:done</task-status>` only when green. The WS-3 refactors land BEFORE MILESTONE-FINAL, each as its own `refactor:` commit.

---

## Key Learnings (from task-mgr recall)

Authoritative — do NOT Read `tasks/long-term-learnings.md` / `tasks/learnings.md`.

- **[4418]** Provider identity / baseline must thread from spawn THROUGH recovery, not be re-derived — the exact anti-pattern FIX-001 fixes (`EffectiveRunnerInput` carries model + provider_hint; recovery should consume threaded defaults, not re-source them).
- **[4049]** Routing precedence: explicit task model > primaryRunner match > difficulty=high > prd default > project default > user default. `compute_baseline_model` owns the last four (minus the explicit/primaryRunner rungs).
- **[3057]/[9]** Layered model resolution with explicit precedence — keep it total (never panics).
- **[4393]/[4396]/[4378]/[4561]** Key-preserving config writers MUST round-trip through `serde_json::Value` and mutate one key, never reserialize a typed struct (FIX-002/003, TEST-INIT-002). Atomic tempfile + `persist`.
- **[4553]** Insert-safe-provider: Codex→Claude promotion inserts `RunnerKind::Claude`, NEVER Codex (`promote_once` callers pass `target=Claude` for that path).
- **[4532]** `source_runner` on `PendingPromotion` disambiguates Grok→Claude vs Codex→Claude (both target Claude) for the direction-neutral banner — preserve it through `promote_once`.
- **[4537]** Codex→Claude fallback is opt-in via the per-route flag (now `runtimeErrorFallback`).
- **[4473]** `escalate_task_model_if_needed_inner` early-returns on `RunnerKind::Codex` (recovery.rs:150) into `maybe_codex_fallback_to_claude` — that branch is where FIX-001 lands.
- **[2954]** The overflow dump-filename sanitizer collapses `..` before allowlist filtering — mirror that for `sanitize_branch_name` (FIX-004); neutralize the `.`/`..` COMPONENT, don't strip all dots.
- **[1484]** Extract loop bodies with enum return types for clean orchestration — `WaveDecision` for REFACTOR-003.
- **[3914]** Byte-identical function extraction validates carving correctness — diff the moved block vs git history for REFACTOR-002/003.
- **[880]** Inline orchestrator blocks are the real refactoring targets — the run_loop startup phase (REFACTOR-002).

---

## CLAUDE.md Excerpts (only what applies to this PRD)

From `src/loop_engine/CLAUDE.md` — do NOT Read the full file.

- **Routing precedence** (`resolve_task_execution_target`): explicit task model → direct primaryRunner match (byTaskType > byIdPrefix) → compute baseline Claude model (difficulty=high → OPUS_MODEL, else prd → project → user default) → `baselineTierRoutes` remap for prefix + baseline tier → baseline model → None. **SSoT**: `model::primary_runner_match` is the single prefix-matching impl; do NOT re-implement.
- **baselineTierRoutes** keyed by task prefix, then provider-neutral tier (`low`/`standard`/`high`; legacy aliases `haiku`/`sonnet`/`opus` still deserialize). `parse_baseline_tier_key` is the ONLY place tier strings become `ModelTier`.
- **Symmetric Claude↔Grok fallback + idempotency**: a single `ctx.runner_overrides.contains_key(task_id)` snapshot taken BEFORE the promotion branch is the guard; a task crosses provider boundary ONCE per run. Footgun: do NOT gate idempotency on `provider_for_model(effective_model)` re-derivation (flapping). "When you add a THIRD cross-provider promotion site, replicate the guard" — `promote_once` (CONTRACT-PROMO-001) becomes that single guard.
- **Deferred-commit promotion**: inner helper does DB writes only + returns `Option<PendingPromotion>`; caller applies via `apply_pending_promotion` AFTER `tx.commit()?`. `promote_once` must preserve this — it constructs the PendingPromotion, it does NOT apply or mutate ctx.
- **Codex closed in v1**: `provider_for_model` never returns Codex; Codex reached only via explicit `primaryRunner` provider intent. `tests/codex_runner_overrides_invariant.rs` pins never-insert-Codex.
- **Drift sentinels are `assert!`, not `debug_assert!`** (release-build silent-mismatch guards).
- **Status mutations** go through `TaskLifecycle` verbs only.

From project `CLAUDE.md`:
- After bumping model.rs constants, regenerate docs via `cargo run --bin gen-docs`; CI runs `gen-docs -- --check`. (No constant bump in this PRD, but VERIFY-001 runs the check.)
- `src/output/ui.rs` + observability (CONTRACT-LOG-001): `ui::*` for operator UX/contracts; `tracing` for internal diagnostics only.

---

## Data Flow Contracts

Verified access patterns — use exactly; do NOT guess key types.

### Baseline defaults: engine cache → recovery → compute_baseline_model (FIX-001 / INT-001)

```rust
// engine cache (typed Option<&'a str>, src/loop_engine/engine.rs:159-161):
//   project_default_model: Option<&'a str>
//   user_default_model:    Option<&'a str>
// prd_default comes from the DB inside recovery:
let prd_default: Option<String> =
    conn.query_row("SELECT default_model FROM prd_metadata WHERE id = 1", [], |r| r.get(0)).ok().flatten();
// difficulty from the task row:
let difficulty: Option<String> =
    conn.query_row("SELECT difficulty FROM tasks WHERE id = ?", [task_id], |r| r.get(0)).ok().flatten();
// SAME four inputs the primary site uses — no key-type transition, but the VALUE SOURCE
// for project_default/user_default MUST be the engine-cached defaults (NOT claude_fallback_model):
let baseline = model::compute_baseline_model(
    difficulty.as_deref(),
    prd_default.as_deref(),
    project_default,   // threaded Option<&str> from engine cache
    user_default,      // threaded Option<&str> from engine cache
);
```

> The bug being fixed: today recovery passes `project_default = primary.claude_fallback_model`
> and omits `user_default`. The fix threads the engine-cached defaults so recovery and the
> primary site key the tier on the same baseline.

### Config migration: file → serde_json::Value → ProjectConfig (FIX-002/003, TEST-INIT-002)

```rust
// Tier keys are STRING map keys at every level ("opus" vs "high"), never enum variants.
// Mutate the Value in place; never reserialize a typed ProjectConfig (drops unknown keys).
let routes = value.get("primaryRunner")
    .and_then(|p| p.get("baselineTierRoutes"))
    .and_then(serde_json::Value::as_object);
// baseline_tier_routes: HashMap<String, HashMap<String, RunnerSpec>>  (prefix -> tier-string -> spec)
// parse_baseline_tier_key normalizes "opus"/"sonnet"/"haiku" + "high"/"standard"/"low" -> ModelTier.
```

---

## Feature-Specific Checks

- After FIX-001, run `cargo test --test primary_runner_routing` AND `cargo test --test codex_recovery` together — the first guards the happy path, the second the recovery parity.
- For REFACTOR-002/003: after the move, `git diff <prev-commit> -- <moved-region>` should show only relocation, no logic deltas. Run `tests/reaction_parity.rs`.
- VERIFY-001 must run `cargo run --bin gen-docs -- --check` even though no model constant changed (catches any doc drift from CLAUDE.md edits).

---

## Important Rules

- Work on **ONE story per iteration**.
- **Commit frequently**; keep CI green; never commit failing code.
- **Read before writing**; minimal changes; check existing patterns.
- WS-3 refactors are **behavior-neutral, separate commits** — never bundle with a fix or a logic edit.
