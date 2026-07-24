# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Data-Driven Prompt Construction (PromptSection + Assembler)** for **task-mgr**.

## Problem Statement

The prompt module (`src/loop_engine/prompt/`) has two composition paths — `sequential.rs` (single-task, used by `run_iteration`) and `slot.rs` (parallel-wave, produces a `Send`-safe `SlotPromptBundle`). They share `core` helpers, but each path hand-assembles its own ordered list of sections. A doc comment in `prompt/mod.rs` enforces the only thing keeping them in sync by discipline: *"any new section added to the sequential prompt must also be wired into `slot` — there is no second source of truth."* This is exactly the parity-drift hazard that bit the drained-queue seq/wave divergence.

Phase 2 / Item 3 of the coherence refactoring replaces that manual discipline with a **data-driven assembler**: a single `PromptContext` + an ordered `Vec<SectionSpec>` (fn-pointer spec table, approach C) consumed by one `assemble()` function that both paths call. The slot roster is a subset of the sequential roster. A roster-completeness test recovers exhaustiveness in CI. The spike (2026-05-23) validated this against real section signatures and proved the `!Send` `Connection` fear unfounded (the assembler runs main-thread and returns owned Strings).

The authoritative design is **`docs/designs/prompt-assembler-contract.md`** (§CONTRACT-001 + Approaches & Tradeoffs). Read it via the CONTRACT-001 task.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). The linters, type-checker, and targeted tests catch more than a re-read does.

**This effort's overriding invariant: byte-identical migration.** Every section is migrated one at a time behind a parity test asserting `assemble()` output == the live legacy builder output. Never compare against a frozen expected string.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying the rendered prompt bytes
- Tests that mirror implementation internals (break when refactoring)
- A section whose text is still inlined in `sequential.rs`/`slot.rs` after it has been migrated (two sources of truth)
- Emitting sections in render-phase order (criticals last) instead of roster/display order — changes byte layout silently
- Forgetting to clear `shown_learning_ids` when the learnings section is dropped — silently skews the UCB bandit
- Threading per-section budget through `PromptContext` instead of `SectionKind` — collapses per-section budgets into one shared field

---

## Global Acceptance Criteria

These apply to **every** implementation task — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: All scoped tests pass with `cargo test -p task-mgr <scope>`
- Rust: `cargo fmt --check` passes
- No breaking changes to the public re-exports `loop_engine::prompt::{build_prompt, BuildPromptParams, PromptResult}` or `slot::SlotPromptBundle`'s Send contract unless explicitly required by the task
- `SlotPromptBundle` compile-time Send assertion still compiles after every change

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** Loading the JSON wastes context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything global is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your task prefix

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/data-driven-prompt-construction.json)
```

Use `$PREFIX` in every CLI call below so you stay scoped to this task list.

### Commands you'll actually run

| Need                                    | Command                                                                                                                                                                           |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pick + claim the next eligible task     | `task-mgr next --prefix $PREFIX --claim`                                                                                                                                          |
| Inspect one task (full acceptance etc.) | `task-mgr show $PREFIX-TASK-ID`                                                                                                                                                   |
| List remaining tasks (debug only)       | `task-mgr list --prefix $PREFIX --status todo`                                                                                                                                    |
| Recall learnings relevant to a task     | `task-mgr recall --for-task $PREFIX-TASK-ID`                                                                                                                                       |
| Add a follow-up task (review spawns)    | `echo '{...}' \| task-mgr add --stdin --depended-on-by REVIEW-001`                                                                                                               |
| Mark status                             | Emit `<task-status>$PREFIX-TASK-ID:done</task-status>` (statuses: `done`, `failed`, `skipped`, `irrelevant`, `blocked`)                                                          |

### Files you DO touch

| File                                                | Purpose                                                                |
| --------------------------------------------------- | ---------------------------------------------------------------------- |
| `tasks/data-driven-prompt-construction-prompt.md`   | This prompt file (read-only)                                           |
| `tasks/progress-$PREFIX.txt`                        | Progress log — **tail** for recent context, **append** after each task |
| `docs/designs/prompt-assembler-contract.md`         | The authoritative contract (read for CONTRACT-001 and any FEAT)        |

**Reading progress** — never Read the whole log:

```bash
# Most recent section only
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac
# Specific prior task
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration.

---

## Your Task (every iteration)

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/data-driven-prompt-construction.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   If it reports no eligible task, output `<promise>BLOCKED</promise>` with the reason and stop.

2. **Pull only the progress context you need** (the `tac | awk | tac` command). Skip on the first iteration.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>`. Do NOT Read `tasks/long-term-learnings.md` / `tasks/learnings.md` directly. Do NOT Read `CLAUDE.md` in full — the relevant excerpts are below.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed. Switch if wrong.

5. **Think before coding**: state assumptions; plan each edge case/failure mode; for cross-module data access consult the Data Flow Contracts section or grep 2-3 existing call sites; pick an approach (survey alternatives only when `high` effort or `modifiesBehavior: true`).

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (below — scoped tests only). Fix failures before committing.

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:`).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`. Do NOT edit the JSON.

10. **Append progress** — one tight block, terminated with `---`.

---

## Quality Checks

### Per-iteration scoped gate (CONTRACT / FEAT / FIX / REFACTOR-FIX tasks)

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test -p task-mgr prompt          # the prompt module + its tests
cargo test -p task-mgr assembler       # narrower, once assembler.rs exists
cargo test -p task-mgr --test prompt_assembler_parity   # the parity test added by FEAT-002+
```

Scope from `touchesFiles`. Do NOT run the entire workspace suite during regular iterations — that's REVIEW-001's job.

### Full gate (REFACTOR-001 / REVIEW-001)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test
```

REVIEW-001 fixes every failure, including pre-existing ones (default: attempt all). **Known pre-existing flake**: `test_wave_crash_tracker_any_completed_resets` fails ~50% under concurrency, unrelated to this work — re-run to confirm rather than attributing it to the assembler. Escape hatch only if >~12 clearly-unrelated failures: fix what's attributable, spawn a single `FIX-xxx` for the rest, `<promise>BLOCKED</promise>`.

---

## Common Wiring Failures (REVIEW-001 reference)

- A section migrated to a SectionSpec but the spec never added to the roster → text silently disappears from the prompt. Parity test catches it.
- Roster built but `assemble()` never called from one of the two paths → that path still uses legacy inlined code.
- `&Connection` captured into a slot worker closure → `!Send` compile error (good — that's the assertion doing its job; build the bundle on the main thread before `thread::spawn`).
- `shown_learning_ids` cleared in a builder instead of `assemble()` → duplicate invariant; grep test catches it.

---

## Review Tasks

| Review         | Priority | Spawns (priority)                  | Focus                                                                                          |
| -------------- | -------- | ---------------------------------- | ---------------------------------------------------------------------------------------------- |
| REFACTOR-001   | 98       | `REFACTOR-FIX-xxx` (50-97)         | DRY across render fns + roster builders, assemble() complexity, pattern adherence              |
| REVIEW-001     | 99       | `FIX-xxx` / `WIRE-FIX-xxx` (50-97) | Wiring (every section via roster), Send/main-thread Connection invariants, full-suite green    |

Use the **rust-python-code-reviewer** agent when reviewing. Spawn follow-ups via `echo '{...}' | task-mgr add --stdin --depended-on-by REVIEW-001`.

---

## Progress Report Format

APPEND to `tasks/progress-$PREFIX.txt` (create with a one-line header if missing). Keep it tight (~10 lines):

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

---

## Learnings Guidelines

Use `task-mgr recall --for-task <TASK-ID>` / `--query "<keywords>"`. Record with `task-mgr learn`. Do NOT Read the learnings markdown files directly.

---

## Stop and Blocked Conditions

### Stop Condition

Before `<promise>COMPLETE</promise>`: verify ALL tasks `passes: true`, no new tasks pending from final review, REVIEW-001 passed full suite green.

### Blocked Condition

If blocked: document in progress file, optionally create a clarification task via `task-mgr add --stdin --depended-on-by <blocked-task>` (priority 0), output `<promise>BLOCKED</promise>`.

---

## Reference Code

The CONTRACT-001 interface (from `docs/designs/prompt-assembler-contract.md` §CONTRACT-001 — the authoritative copy; reproduced here so you don't have to open the file every iteration):

```rust
pub struct PromptContext<'a> {
    pub conn: &'a Connection,
    pub task: &'a Task,
    pub task_files: &'a [String],
    pub project_root: &'a Path,
    pub base_prompt_path: &'a Path,
    pub permission_mode: &'a PermissionMode,
    pub steering_path: Option<&'a Path>,
    pub session_guidance: &'a str,
    pub run_id: Option<&'a str>,
    pub task_prefix: Option<&'a str>,
    // sequential-only inputs as Option<…>; slot leaves them None:
    pub reorder_hint: Option<&'a str>,
    pub batch_sibling_prds: Option<&'a [PathBuf]>,   // real input to build_sibling_prd_section
}
// NOTE: there is NO `SiblingPrd` or `SynergyCluster` type — earlier drafts named
// them by mistake. Real signatures: build_sibling_prd_section(conn, task_id,
// task_prefix, batch_sibling_prds: &[PathBuf]); build_synergy_section(conn,
// task_id, run_id) which is a permanent no-op returning String::new(). Grep
// before wiring; the contract doc was corrected on 2026-05-23.

pub enum SectionKind { Critical, Trimmable { budget: usize } }

#[derive(Default)]
pub struct Rendered { pub text: String, pub shown_learning_ids: Vec<i64> }

pub struct SectionSpec {
    pub name: &'static str,                                   // stable id; matches section_sizes keys
    pub kind: SectionKind,
    pub render: fn(&PromptContext, SectionKind) -> Rendered,  // kind carries the budget
}

pub struct Assembled {
    pub prompt: String,
    pub section_sizes: Vec<(&'static str, usize)>,
    pub dropped_sections: Vec<String>,
    pub shown_learning_ids: Vec<i64>,
}

pub fn assemble(ctx: &PromptContext, roster: &[SectionSpec], total_budget: usize) -> Assembled;
```

**Invariants** (every impl + caller MUST maintain): single render site per section; **roster = display order, PER PATH** — each path supplies its OWN ordered `Vec<SectionSpec>`; the slot roster is a set-subset of sequential's but *independently ordered* (slot emits `task` first, sequential mid-list — they do NOT share a relative order). Parity unit = per-section rendered text AND whole-prompt bytes **per path** (each path's `assemble()` output == that path's own legacy whole-prompt output, because its roster preserves its own legacy order). Do NOT try to make one roster reproduce both paths' byte layouts. Criticals render first for the budget gate but emit in roster position. **Critical-overflow translation**: `assemble` signals overflow uniformly via `dropped_sections == ["CRITICAL"]`; the **sequential** caller MUST translate that back into `Err(TaskMgrError::PromptOverflow{..})` so `overflow::handle_prompt_too_long`'s five-rung ladder is unchanged, while the slot caller keeps its sentinel-in-bundle behavior. learnings side-output centralized in `assemble()` (clear `shown_learning_ids` when learnings dropped); `TOTAL_PROMPT_BUDGET` (80_000) parity across both paths; `SectionSpec: Send`; no `&Connection` stored in `Assembled`/`SlotPromptBundle` (the load-bearing assertion is `assert_impl_all!(SlotPromptBundle: Send)` in `tests/prompt_slot.rs` — preserve it).

**Real section signatures to wrap** (grep to confirm before use): `build_dependency_section(conn, &task.id)` in `prompt_sections/dependencies.rs`; `core::build_learnings_block(conn, task, budget)` in `prompt/core.rs`; `task_ops_section()` in `prompt_sections/task_ops.rs`.

---

## Key Learnings (from task-mgr recall)

Treat these as authoritative — do NOT Read the learnings markdown files unless a task needs one not listed here.

- **[3940]** Prompt assembler spike: the `!Send` fear is unfounded — the assembler is a main-thread fn that returns owned Strings; `Send` is a property of the OUTPUT bundle. The real design work is display-order-vs-render-phase and the learnings side-output (`shown_learning_ids`) invariant — both centralizable wins.
- **[2226 / 2134 / 2195]** Three-builder prompt architecture: `core` (shared testable helpers), `sequential`, `slot`. The core helpers become the `render`-fn bodies in the assembler — do not invent new section logic.
- **[2664]** When sequential and slot need identical section rendering, the logic belongs in `core` (now: behind a SectionSpec render fn), never duplicated.
- **[2663]** `try_fit_section()` is the existing budget-degradation primitive — reuse it verbatim inside the assembler's trimmable loop; do not reimplement budget accounting.
- **[2705]** Slot historically drifted from sequential (omitted steering/session_guidance), causing degraded parallel-mode prompts. The byte-parity test per section is the guard against re-drift — this whole effort exists to make that drift impossible.
- **[1590]** Each `prompt_sections/*.rs` module exposes a single `section() -> &'static str` (or a small builder fn). Wrap that existing fn in a SectionSpec; don't rewrite it.
- **[2852 / 2031]** PromptTooLong recovery walks a shared ladder keyed off `TOTAL_PROMPT_BUDGET` and the dropped-section accounting — keep `total_budget` identical (80_000) across both paths so the overflow ladder behaves the same.

---

## CLAUDE.md Excerpts (only what applies to this change)

From `src/loop_engine/prompt/mod.rs` (the rule being retired by FEAT-007) and `src/loop_engine/CLAUDE.md`:

- **Three-builder layout**: `core` (bedrock section helpers shared by every builder), `sequential` (canonical single-task builder; re-exports `build_prompt`/`BuildPromptParams`/`PromptResult` — these must keep resolving), `slot` (parallel-wave builder producing a `Send`-safe `SlotPromptBundle`).
- **Main-thread bundle rule** (the hard constraint the assembler must respect): wave mode MUST build `SlotPromptBundle` on the main thread before each `thread::spawn`. Slot worker threads NEVER read from `&Connection` — `rusqlite::Connection` is `!Send`; every learnings/source/synergy lookup feeding the prompt runs on the main thread before the spawn (learnings #1893/#1852/#1871). A compile-time `Send` assertion on `SlotPromptBundle` backstops this; adding a non-`Send` field breaks the build by design. **Do not weaken or remove this assertion.**
- **The rule FEAT-007 deletes** (`prompt/mod.rs` rustdoc ~lines 17-19, AND a duplicate copy in `src/loop_engine/CLAUDE.md`'s "Prompt-builder companion" note): *"any new section added to the sequential prompt must also be wired into `slot` — there is no second source of truth."* The assembler + roster-completeness test replaces it; FEAT-007 must remove BOTH copies.
- `TOTAL_PROMPT_BUDGET = 80_000`; `CRITICAL_OVERFLOW_SENTINEL` behavior (criticals over budget → empty prompt) must be preserved exactly.

---

## Important Rules

- Work on **ONE task per iteration**
- **Commit frequently** after each passing task
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Migrate one section at a time, behind a byte-parity test** — never bulk-rip multiple sections without per-section parity
- Work on the correct branch: **feat/data-driven-prompt-construction**
