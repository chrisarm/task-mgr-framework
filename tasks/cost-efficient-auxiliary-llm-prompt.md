# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Cost-efficient auxiliary LLM passes** for **task-mgr**.

## Problem Statement

After each loop iteration, learning extraction and milestone progress summarization always spawn Claude Haiku via hardcoded `HAIKU_MODEL` + `spawn_claude`, ignoring the operator's `models` + `routing` config. The loop already builds `ResolvedModelsConfig` once per run (`ctx.resolved_models` in `IterationContext`), but auxiliary paths never read it.

**Goal:** use the **primary provider's configured `cost-efficient` tier** (Sonnet on default Claude — intentional upgrade from today's Haiku/cheapest tier) via `runner::dispatch`, with effort omitted for parity with today's auxiliary spawns.

**Out of scope:** curate dedup/enrich (`"haiku"` / `dedup_model`), merge resolver, PRD reconcile, extraction prompt provider-neutral wording.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (shared helpers before call sites) → **FUNCTIONING CODE** (CLI + loop agree) → **CORRECTNESS** (compiles, scoped tests pass) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, naming).

Non-negotiables: cost-efficient tier is intentional (not cheapest-tier parity); primary_provider only (not effective_runner); effort None on auxiliary spawns; extraction failures are best-effort (warn + empty, never abort loop).

**Prohibited outcomes:**

- Hardcoding `HAIKU_MODEL` or any literal model string outside `model.rs` (`tests/no_hardcoded_models.rs`)
- Using `spawn_claude` directly in auxiliary paths after this change — must go through `runner::dispatch`
- Passing `effort_for(provider, Some("low"))` on auxiliary spawns — effort must stay `None`
- Using `effective_runner` or per-iteration model for auxiliary resolution — `primary_provider` only
- Breaking `ExtractionResult` shape or best-effort empty-return contract on extraction failures
- Tests that spawn a real LLM subprocess during unit tests — use pure plan-resolution tests instead

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: Scoped tests pass for touched modules
- Rust: `cargo fmt --check` passes
- No literal Claude model strings introduced outside `model.rs`
- No per-task or top-level PRD `model` fields in this JSON

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global is already embedded in **this prompt file**.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/cost-efficient-auxiliary-llm.json)
```

Use `$PREFIX` in every CLI call below.

### Commands you'll actually run

| Need | Command |
| --- | --- |
| Pick + claim next task | `task-mgr next --prefix $PREFIX --claim` |
| Inspect one task | `task-mgr show $PREFIX-TASK-ID` |
| Recall learnings | `task-mgr recall --for-task $PREFIX-TASK-ID` |
| Add follow-up (review spawns) | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001` |
| Mark status | `<task-status>$PREFIX-TASK-ID:done</task-status>` |

### Files you DO touch

| File | Purpose |
| --- | --- |
| `tasks/cost-efficient-auxiliary-llm-prompt.md` | This prompt (read-only) |
| `tasks/progress-$PREFIX.txt` | Progress log — tail + append |

```bash
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

---

## Your Task (every iteration)

1. `PREFIX=$(jq -r '.taskPrefix' tasks/cost-efficient-auxiliary-llm.json)` then `task-mgr next --prefix $PREFIX --claim`
2. Tail progress if not first iteration
3. `task-mgr recall --for-task <TASK-ID>` for focused learnings
4. Verify branch: `feat/cost-efficient-auxiliary-llm`
5. Implement one task (code + tests together)
6. Run scoped quality gate (below)
7. Commit: `feat: <TASK-ID>-completed - [Title]`
8. `<task-status><TASK-ID>:done</task-status>`
9. Append progress block ending with `---`

---

## Behavior Modification Protocol (FEAT-002, FEAT-003)

These tasks change public function signatures and spawn behavior:

1. Grep all callers before changing signatures: `extract_learnings_from_output`, `summarize_milestone`
2. Update **every** caller in the same task (iteration_pipeline + main; engine + progress tests)
3. Preserve best-effort contracts — extraction returns empty on failure; milestone falls back to heuristic

---

## Quality Checks

### Per-iteration scoped gate (FEAT tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test cost_efficient_auxiliary   # FEAT-001
cargo test summarize_milestone        # FEAT-003
cargo test record_extracted_learnings # if touching ingestion recording
```

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

---

## Key Learnings (from task-mgr recall)

- **[2975]** `spawn_claude` is already `dispatch(RunnerKind::Claude, ...)` — switching auxiliary paths to `runner::dispatch` is the meaningful change; `claude.rs` type aliases stay valid.
- **[5134]** Thread `ResolvedModelsConfig` through context objects for production paths; use `builtin_resolved_models()` only as test/fallback default.
- **[5081]** `resolve_models_config` is pure and IO-free — build once per run, pass references to hot paths.
- **[5120]** Tier membership uses `ResolvedModelsConfig::model_for` / `tier_of` exact config match — never substring classification.
- **[2111]** Learning extraction runs through unified `iteration_pipeline` — single wiring point covers sequential and wave mode.
- **[2174]** `LearningWriter::flush(conn)` runs after the recording loop, never inside a transaction.
- **[656]** Non-loop spawns use `PermissionMode::text_only()` for classification-style tasks (keep current extraction behavior).

---

## CLAUDE.md Excerpts (only what applies)

- **Model IDs**: All model constants live in `src/loop_engine/model.rs` only; `tests/no_hardcoded_models.rs` enforces this.
- **Models config (FR-001)**: Runtime resolution uses `models` + `routing` in `.task-mgr/config.json`; explicit per-task `model` bypasses config (never set in generated tasks).
- **Capability tiers**: `cheapest` < `cost-efficient` < `standard` < `frontier`. Default Claude: Haiku / Sonnet / Opus / Fable. This change targets **cost-efficient**, not cheapest.
- **Iteration pipeline**: `process_iteration_output` in `iteration_pipeline.rs` is the sole post-Claude pipeline for sequential and wave paths.
- **Learning extraction opt-out**: `TASK_MGR_NO_EXTRACT_LEARNINGS=1` keeps tests hermetic — do not break this contract.
- **Parallel-slot note**: Stale slot-worktree test binaries can fail after parallel loops — touch `tests/<binary>.rs` to force recompile if fixture paths point at removed `-slot-N` worktrees.

---

## Data Flow Contracts

### ResolvedModelsConfig → learning extraction

```rust
// Built once per loop run (orchestrator.rs):
ctx.resolved_models = resolve_models_config(&project_config.models, &project_config.routing);

// Iteration pipeline (iteration_pipeline.rs ~L513):
extract_learnings_from_output(
    conn,
    learning_source,
    task_id,
    Some(run_id),
    Some(db_dir),
    Some(signal_flag),
    &params.ctx.resolved_models,  // &ResolvedModelsConfig
)?;

// CLI (main.rs ExtractLearnings):
let proj_cfg = read_project_config(&cli.dir);
let resolved = resolve_models_config(&proj_cfg.models, &proj_cfg.routing);
extract_learnings_from_output(..., &resolved, ...)?;
```

### ResolvedModelsConfig → milestone summary

```rust
// engine.rs apply_status_updates milestone hook (~L1062):
let resolved = ctx
    .as_ref()
    .map(|c| &c.resolved_models)
    .unwrap_or(model::builtin_resolved_models());
progress::summarize_milestone(pp, &update.task_id, db_dir, resolved);

// progress.rs:
pub fn summarize_milestone(
    progress_path: &Path,
    milestone_task_id: &str,
    db_dir: Option<&Path>,
    resolved: &ResolvedModelsConfig,
);
```

### Auxiliary plan resolution (model.rs)

```rust
let plan = cost_efficient_auxiliary_plan(resolved);
// plan.provider: Provider (enum — primary_provider)
// plan.model: Option<&str> from model_for(provider, CapabilityTier::CostEfficient)

let kind = runner_kind_for(plan.provider);  // RunnerKind enum
dispatch_auxiliary(plan, prompt, EXTRACTION_TIMEOUT, db_dir, signal_flag)?;
// RunnerOpts { model: plan.model, effort: None, ... }
```

**Type transitions:** `Provider` (enum) → `RunnerKind` (enum) at dispatch; `model` stays `Option<&str>` from config tier table (may be `None` for null Codex rung).

---

## Feature-Specific Checks

- `grep -r HAIKU_MODEL src/learnings/ingestion/mod.rs src/loop_engine/progress.rs` → no matches after FEAT-002/003
- `grep -r spawn_claude src/learnings/ingestion/mod.rs src/loop_engine/progress.rs` → no matches in auxiliary paths
- `grep -r extract_learnings_from_output` → all callers pass `&ResolvedModelsConfig`
- `grep -r summarize_milestone` → all callers pass `&ResolvedModelsConfig`
- Default config: cost-efficient on Claude resolves to `SONNET_MODEL` (not `HAIKU_MODEL`)

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Minimal changes** — curate dedup/enrich out of scope
- Branch: **feat/cost-efficient-auxiliary-llm**

---

## Stop / Blocked

**Complete** when all tasks `passes: true` and REVIEW-001 full suite is green → `<promise>COMPLETE</promise>`

**Blocked** → document in progress, spawn clarification task, `<promise>BLOCKED</promise>`