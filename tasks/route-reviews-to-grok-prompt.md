# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Route code reviews to Grok (configurable reviewModel)** for **task-mgr**.

## Problem Statement

Today the loop runs every task type on Claude. We want a **second model's perspective** on overall-implementation review by routing the review-class gates — `CODE-REVIEW-*`, `MILESTONE-FINAL`, and `REVIEW-001` (the lean combined gate) — to **Grok**, while everything else (implementation, tests, `VERIFY-*`, `REFACTOR-*`, intermediate `MILESTONE-1/-2`, `REFACTOR-REVIEW-FINAL`, and all spawned `*-FIX-*`) stays on Claude.

Grok is already a first-class primary runner (`provider_for_model("grok-…")` → `RunnerKind::Grok` → `dispatch()`); the grok CLI is installed and on PATH. The only gap is **selection**: nothing routes review-class tasks to a Grok model, and there is no config knob. This adds a `reviewModel` field to `.task-mgr/config.json` that the engine applies (authoritatively, at the dispatch sites) to review-class tasks, plus generator-skill stamping so the JSON is self-describing.

---

## Non-Negotiable Process (Read Every Iteration)

1. **Internalize quality targets** — read the task's `qualityDimensions`; that's what "done well" means.
2. **Plan edge-case handling** — for each `edgeCases` / `failureModes` entry, decide handling before coding.
3. **Pick an approach** — only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, run the scoped quality gate (below) — the linters/type-checker/targeted tests are your critic.

---

## Priority Philosophy

**PLAN** → **FOUNDATION (single-source-of-truth seams)** → **FUNCTIONING CODE** → **CORRECTNESS** → **CODE QUALITY** → **POLISH**.

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` on fallible paths). The override must keep runner selection, the `--model` flag, and the prompt-baked model **consistent** — a drift `assert!` will panic otherwise.

**Prohibited outcomes:**

- A predicate that matches unprefixed ids in tests but never matches the prefixed ids seen at dispatch (silent no-op).
- Overriding runner selection without also changing the `--model` string the runner receives (sends a Claude model id to the Grok runner).
- Putting the routing override in `resolve_task_model` / `ModelResolutionContext` (wrong seam — runs in `build_prompt`, has no `task_id`).
- Duplicating the review-class routing logic across the two dispatch sites instead of a shared predicate.
- A startup probe that aborts Claude-only loops (must only fire when `reviewModel` resolves to a Grok provider).
- Editing the auto-generated `MODELS:BEGIN/END` block by hand (gen-docs --check would clobber/fail).
- Tests that only assert "no panic" without verifying the resolved model/runner value.

---

## Global Acceptance Criteria

Apply to **every** implementation task, layered on top of the per-task `acceptanceCriteria`:

- Rust: no warnings in `cargo check`.
- Rust: no warnings in `cargo clippy -- -D warnings`.
- Rust: `cargo fmt --check` passes.
- Rust: scoped `cargo test` for touched modules passes (full suite at REVIEW-001).
- No `unwrap()`/`expect()` on fallible paths in production code — propagate with the crate error type.
- No breaking changes to existing config or CLI behavior when `reviewModel` is absent.

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Everything per-task is returned by `task-mgr next`; everything global is in **this prompt file** (authoritative).

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/route-reviews-to-grok.json)
```

### Commands

| Need | Command |
| --- | --- |
| Pick + claim next task | `task-mgr next --prefix $PREFIX --claim` |
| Inspect one task | `task-mgr show $PREFIX-TASK-ID` |
| Recall learnings for a task | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add a follow-up task | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (also: `failed`, `skipped`, `irrelevant`, `blocked`) |

### Files you DO touch

| File | Purpose |
| --- | --- |
| `tasks/route-reviews-to-grok-prompt.md` | This prompt (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — tail recent, append after each task |

Read recent progress with `tac tasks/progress-$PREFIX.txt | awk '/^---$/{exit} {print}' | tac` (skip on first iteration).

---

## Your Task (every iteration)

1. `PREFIX=$(jq -r '.taskPrefix' tasks/route-reviews-to-grok.json)` then `task-mgr next --prefix $PREFIX --claim`. If no eligible task / unmet `requires`, emit `<promise>BLOCKED</promise>` with the reason and stop.
2. Pull only the progress context you need (most-recent section).
3. `task-mgr recall --for-task <TASK-ID>` for focused learnings. **Never** Read `tasks/long-term-learnings.md` / `learnings.md` in full. Don't Read `CLAUDE.md` in full — grep for the section you need.
4. `git branch --show-current` matches the printed `branchName` (switch if wrong).
5. Think before coding: assumptions, edge cases, cross-module access (consult Data Flow Contracts below).
6. Implement — ONE task, code + tests in one coherent change.
7. Run the **scoped** quality gate (below). Fix failures before committing.
8. Commit: `feat: <TASK-ID>-completed - [Title]` (`refactor:`/`fix:`/`docs:` as appropriate).
9. Emit `<task-status><TASK-ID>:done</task-status>`.
10. Append ONE progress block (format below), terminated with `---`.

---

## Quality Checks

### Per-iteration scoped gate (FEAT / FIX tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test <module_or_fn_name>      # scope: e.g. `model::`, `project_config`, the engine test names you added
```

`FEAT-004` is **markdown-only** — no cargo needed; instead run `cargo run --bin gen-docs -- --check` to prove the MODELS block is untouched, and `git diff` the four files.

**Do NOT** run the full unscoped suite during normal iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test && cargo run --bin gen-docs -- --check
```

Fix every failure, including pre-existing ones. Above ~12 clearly-unrelated failures: fix what this diff caused, spawn a single `FIX-xxx` for the rest, and `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (REVIEW-001 reference)

- Override defined but applied at only one of the two dispatch sites → routing inconsistent between sequential and wave runs.
- Wave override mutates the transient `effective_model` local instead of `slot.prompt_bundle.resolved_model` → Grok runner gets a Claude `--model` and/or the prompt shows the wrong model; the drift `assert!` fires.
- Override applied before the crash/overflow escalation block (sequential) → escalation overwrites it.
- `params.project_config` not threaded / re-reading the config file instead of using the in-scope reference.
- Startup probe not provider-gated → breaks Claude-only loop starts.
- Skill edits land in the repo copies but not the global `~/.claude/commands/` copies (or vice-versa).

---

## Key Learnings (from task-mgr recall — authoritative; don't re-Read learnings files)

- **[3055] / [3089]** `resolve_effective_runner` (engine.rs:346) is the **single source of truth** for runner selection, called from BOTH the wave and sequential sites. Route through it; don't fork.
- **[2985]** Runner resolution precedence: (1) `runner_overrides[task]` wins, (2) else `provider_for_model(model)`, (3) else default. Your override changes the **model string**, not `runner_overrides`; leave that map's precedence intact.
- **[2983]** Wave dispatch must resolve the effective runner/model on the **main thread** in `run_wave_iteration` before the slot spawns (workers can't see it otherwise).
- **[2997]** When a computed value is needed for multiple downstream uses, compute/rewrite it early (here: rewrite `prompt_bundle.resolved_model` so selection + flag + prompt all read the same value).
- **[3110]** Single-source-of-truth **drift sentinels are `assert!`** (not `debug_assert!`): a guard compares `slot_result.effective_runner` against a re-derivation. If your wave override leaves selection and model inconsistent, this **panics**. Keep them consistent — don't rely on the panic as the contract.
- **[2982] / [2800] / [2366]** Add the new config field as an **optional `#[serde(default)]`** field on `ProjectConfig`, mirroring `fallback_runner`; test via the `tests/fallback_config.rs` pattern.
- **[2961] / [2958]** `provider_for_model` uses **token-equality** (`lower.split('-').any(|t| t == "grok")`), not substring. `is_review_class` lives next to it.
- **[2959]** Provider guard-rail pattern: escalation fns early-return when `provider_for_model(...)` isn't the expected provider — mirror this for the startup probe's provider gate.
- **[2986]** If you ever add a field to `SlotContext`/the bundle, update ALL struct literals incl. `tests/prompt_slot.rs`. (Mutating the existing `resolved_model` field needs no new field.)
- **[1947]** Keep the config key spelled **`reviewModel`** consistently across code, tests, and docs (a prior rerankerTopN/rerankerOverFetch drift caused a doc bug).

---

## CLAUDE.md Excerpts (src/loop_engine/CLAUDE.md — only what applies)

- `IterationContext` carries `runner_overrides`, `model_overrides`, `overflow_recovered`, `overflow_original_model`; per-iteration mutations happen **before `resolve_effective_runner`**. Do NOT add `review_model` here — read `params.project_config.review_model`.
- **Provider routing — `model::provider_for_model`** classifies a model id; the result is consumed by `resolve_effective_runner` (engine.rs) for the spawn-site dispatch.
- **Drift sentinels are `assert!`, not `debug_assert!`**: they compare `slot_result.effective_runner` vs a `resolve_effective_runner(...)` re-derivation to catch a silent wrong-runner/wrong-model spawn. The two dispatch call sites are sequential (`run_iteration`) and parallel-wave (`run_slot_iteration` driven from `run_wave_iteration`).
- Grep, don't full-Read: `grep -n -A 10 '<header>' src/loop_engine/CLAUDE.md`.

---

## Data Flow Contracts

**Config → engine override (the load-bearing path):**

```
.task-mgr/config.json  ── serde ─▶  ProjectConfig.review_model: Option<String>   (project_config.rs)
   key "reviewModel"                       │
                                           ▼
   loop start ── &ProjectConfig ─▶  IterationParams.project_config / WaveIterationParams.project_config
                                           │   (already passed by reference — confirmed in scope at both sites)
                                           ▼
   dispatch  ──▶  if model::is_review_class(&task_id)
                     && let Some(rm) = normalize(params.project_config.review_model.as_deref())
                  ── sequential: effective_model = Some(rm)            (feeds resolve_effective_runner + --model)
                  ── wave:       slot.prompt_bundle.resolved_model = Some(rm)   (feeds runner select + --model + format_task_json prompt)
                                           ▼
   resolve_effective_runner(ctx, &task_id, effective_model)  ──▶  RunnerKind::Grok  (via provider_for_model)
```

**Task ID prefix contract:** ids are persisted/dispatched as `<8-hex>-<TYPE>-<n>` (`prefix_id`, `src/commands/init/mod.rs:82`; `generate_prefix` = `md5(branch:filename)[..8]`). `is_review_class` MUST strip a leading `^[0-9a-f]{8}-` group before matching — `bundle.task_id` is the prefixed form.

**Grok binary resolution (for the startup probe):** `GROK_BINARY` env → `fallbackRunner.cliBinary` → bare `grok` on PATH (`resolve_grok_binary`, `src/loop_engine/runner.rs:~614`). The probe must use this same order.

---

## Code Anchors (verify with grep; line numbers drift)

- `ProjectConfig` struct `project_config.rs:58`; `impl Default` `:177`; `check_fallback_runner_binary` `:306`.
- `provider_for_model` `model.rs:81`; `normalize` `model.rs:225`.
- `resolve_effective_runner` `engine.rs:346`; `run_slot_iteration` `:557`; `run_wave_iteration` `:1835`; `run_iteration` `:2230`.
- `resolve_grok_binary` `runner.rs:~614`.
- gen-docs targets `.claude/commands/{tasks.md, plan-tasks.md}` (`src/bin/gen-docs.rs:32`).

---

## Progress Report Format

Append to `tasks/progress-$PREFIX.txt` (create with a header if missing). Keep tight (~10 lines).

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 one-line bullets]
---
```

Record durable learnings with `task-mgr learn` (don't append to learnings files directly).

---

## Stop and Blocked Conditions

**COMPLETE** — before emitting `<promise>COMPLETE</promise>`: all tasks `passes: true`; no new tasks pending from final review; REVIEW-001 passed with the full suite + `gen-docs --check` green.

**BLOCKED** — document the blocker, spawn a clarification task via `task-mgr add --stdin --depended-on-by <blocked-task>` (priority 0), then emit `<promise>BLOCKED</promise>`.

---

## Important Rules

- ONE task per iteration. Commit after each passing task. Keep CI green. Read before writing. Minimal changes.
- Work on branch **feat/route-reviews-to-grok**.
- Deferred (manual, out of this repo — do NOT attempt here): the DeskMaiT `.task-mgr/config.json` `reviewModel` line and the global `~/.claude/CLAUDE.md` §3a note. `FEAT-004`'s edits to the two `~/.claude/commands/` files are a side effect not captured by this repo's commit — commit them in `~/.claude` separately.
