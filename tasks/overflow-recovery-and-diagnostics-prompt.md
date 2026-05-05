# Claude Code Agent Instructions

You are an autonomous coding agent implementing **Prompt-Overflow Recovery Escalation + Diagnostics** for **task-mgr**.

## Problem Statement

The loop engine's `PromptTooLong` recovery handler in `src/loop_engine/engine.rs:2044-2113` has two distinct gaps. (1) **Recovery gap**: a Sonnet-default loop that overflows on iteration 1 is immediately blocked because `downgrade_effort` (only `xhigh→high`) and `to_1m_model` (only Opus→Opus[1M]) both no-op for Sonnet at `high` effort — `escalate_model` exists but is never called from this branch. (2) **Diagnostics gap**: when overflow happens the user sees one stderr line and the task is blocked; there is no record of what the prompt looked like, how big it was, or which section dominated. The error message also conflates "1M was tried and failed" with "1M was never available", sending users hunting in the wrong place.

This PR adds a third recovery rung (Sonnet→Opus before Opus→Opus[1M]) and a diagnostics bundle: prompt dumps to `.task-mgr/overflow-dumps/`, structured JSONL event log at `.task-mgr/overflow-events.jsonl`, per-section byte breakdown in `PromptResult`, banner annotation when a task is mid-recovery, and corrected stderr messages.

---

## Non-Negotiable Process (Read Every Iteration)

Before writing code:

1. **Internalize quality targets** — Read `qualityDimensions`; that's what "done well" means for THIS task.
2. **Plan edge-case handling** — For each `edgeCases` / `invariants` / `failureModes` entry on the task, decide how it'll be handled before coding.
3. **Pick an approach** — State assumptions in your head. Only for `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, name the one alternative you rejected and why.

After writing code, the scoped quality gate is your critic — run it (Quality Checks § Per-iteration). Don't add a separate self-critique step; the linters, type-checker, and targeted tests catch more than a re-read does.

---

## Priority Philosophy

In order: **PLAN** (anticipate edge cases) → **PHASE 2 FOUNDATION** (~1 day now to save ~2+ weeks later — take it, we're pre-launch) → **FUNCTIONING CODE** (pragmatic, reliable) → **CORRECTNESS** (compiles, type-checks, scoped tests pass deterministically) → **CODE QUALITY** (clean, no warnings) → **POLISH** (docs, formatting).

Non-negotiables: tests drive implementation; satisfy every `qualityDimensions` entry; handle `Option`/`Result` explicitly (no `unwrap()` in production). For `estimatedEffort: "high"` or `modifiesBehavior: true` tasks, note the one alternative you rejected and why. For everything else, pick and go.

**Prohibited outcomes:**

- Tests that only assert 'no crash' or check type without verifying content
- Tests that mirror implementation internals (break when refactoring)
- Abstractions with only one concrete use
- Error messages that don't identify what went wrong
- Catch-all error handlers that swallow context
- Banner annotation inferred from `model_overrides` (must use `overflow_recovered` HashSet — see learning #893)
- Unsanitized `task_id` flowing into dump filenames (path traversal risk)
- DB UPDATE that runs AFTER dump/JSONL writes (recovery state must be durable before observability)
- `.unwrap()` on filesystem operations in the overflow module (warnings via `eprintln!`, never propagate)
- Effort downgrade below `high` (the ladder floor; preserves the `model.rs:42-48` invariant)

---

## Global Acceptance Criteria

These apply to **every** implementation task in this PRD — the task-level `acceptanceCriteria` returned by `task-mgr next` are layered on top. If any of these fails, the task is not done.

- Rust: No warnings in `cargo check` output
- Rust: No warnings in `cargo clippy -- -D warnings` output
- Rust: All scoped tests pass (per-iteration); full `cargo test` passes at milestones
- Rust: `cargo fmt --check` passes
- No breaking changes to existing public APIs (`PromptResult` and `IterationContext` extensions are additive only)
- Filesystem failures (disk full, permission errors) in the overflow module log warnings via `eprintln!` and do NOT propagate — observability is best-effort, recovery is not
- Order of operations in `PromptTooLong` arm: ctx update → DB UPDATE → stderr → dump (best-effort) → JSONL (best-effort) → rotate (best-effort)

---

## Task Files + CLI (IMPORTANT — context economy)

**Never read or edit `tasks/*.json` directly.** PRDs are thousands of lines; loading one wastes a huge amount of context and editing corrupts loop-engine state. Everything the agent needs about a task is returned by `task-mgr next`; everything PRD-wide that matters for implementation (Priority Philosophy, Prohibited Outcomes, Global Acceptance Criteria, Key Learnings, CLAUDE.md Excerpts, Data Flow Contracts, Key Context) is already embedded in **this prompt file** — that is the authoritative copy. If something here looks inconsistent with the JSON, trust this file and surface the discrepancy.

### Getting your PRD's task prefix

The `taskPrefix` is auto-generated by `task-mgr init` and written into the JSON. Fetch it once at the start of an iteration (don't hardcode it):

```bash
PREFIX=$(jq -r '.taskPrefix' tasks/overflow-recovery-and-diagnostics.json)
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

### Files you DO touch

| File                                 | Purpose                                                                    |
| ------------------------------------ | -------------------------------------------------------------------------- |
| `tasks/overflow-recovery-and-diagnostics-prompt.md` | This prompt file (read-only) |
| `tasks/progress-{{TASK_PREFIX}}.txt` | Progress log — **tail** for recent context, **append** after each task     |

**Reading progress** — sections are separated by `---` lines and each starts with `## <Date> - <TASK-ID>`. Never Read the whole log; it grows every iteration. Two targeted patterns cover every case:

```bash
# Most recent section only (default recency check)
tac tasks/progress-$PREFIX.txt 2>/dev/null | awk '/^---$/{exit} {print}' | tac

# Specific prior task (e.g. a synergy task you're building on, or a dependsOn task)
grep -n -A 40 '## .* - <TASK-ID>' tasks/progress-$PREFIX.txt
```

Skip the read entirely on the first iteration (file won't exist).

---

## Your Task (every iteration)

1. **Resolve prefix and claim the next task**:
   ```bash
   PREFIX=$(jq -r '.taskPrefix' tasks/overflow-recovery-and-diagnostics.json)
   task-mgr next --prefix $PREFIX --claim
   ```
   The output includes `id`, `title`, `description`, `acceptanceCriteria`, `qualityDimensions`, `edgeCases`, `touchesFiles`, `dependsOn`, `branchName`, and `notes`. If it reports no eligible task, output `<promise>BLOCKED</promise>` with the printed reason and stop.

2. **Pull only the progress context you need** — most iterations want just the most recent section. If `task-mgr next` listed a `dependsOn` task whose rationale you need, grep that specific task's block instead of reading the whole log.

3. **Recall focused learnings** — `task-mgr recall --for-task <TASK-ID>` returns the learnings scored highest for this specific task. The most relevant learnings for THIS PRD are already pre-distilled below ("Key Learnings" section) — start there.

4. **Verify branch** — `git branch --show-current` matches the `branchName` task-mgr printed. Switch if wrong.

5. **Think before coding**:
   - State assumptions to yourself.
   - For each `edgeCases` / `invariants` / `failureModes` entry, note how it'll be handled.
   - Cross-module data access → consult the **Data Flow Contracts** section below or grep 2-3 existing call sites.
   - Pick an approach. Only survey alternatives when `estimatedEffort: "high"` OR `modifiesBehavior: true` — even then, one rejected alternative with a one-line reason is enough.

6. **Implement** — single task, code and tests in one coherent change.

7. **Run the scoped quality gate** (see Quality Checks below — scoped tests only, NOT the full suite).

8. **Commit**: `feat: <TASK-ID>-completed - [Title]` (or `refactor:`/`fix:`/`test:` as appropriate).

9. **Emit status**: `<task-status><TASK-ID>:done</task-status>`.

10. **Append progress** — ONE post-implementation block, terminated with `---`.

---

## Behavior Modification Protocol (only when `modifiesBehavior: true`)

Only **FEAT-005** in this PRD has `modifiesBehavior: true`. Its `consumerAnalysis` is already populated in the JSON (you'll see it in `task-mgr show FEAT-005`). The dependency on **ANALYSIS-001** ensures consumer analysis runs first and refreshes any drift since the PRD was drafted.

For FEAT-005 specifically:
1. Confirm ANALYSIS-001 has `passes: true` (it should be the priority-0 first task).
2. Read the Consumer Impact Table from the PRD (already embedded in the task's `consumerAnalysis` field) — no surprises if ANALYSIS-001 found drift, since it appended to progress.
3. Implement against the documented contract; do NOT introduce new readers/writers of `model_overrides` outside the documented seams.

---

## Quality Checks

The full test suite is expensive. Per-iteration tasks run a **scoped** gate; **milestones** run the full gate.

### Per-iteration scoped gate (implementation / test / fix tasks)

Format → type-check → lint → **scoped tests for touched files** → pre-commit hooks. Fix every failure before committing.

```bash
# Always pipe through tee + grep — never run twice
cargo fmt --check
cargo check 2>&1 | tee /tmp/check.txt | tail -5 && grep "error\|warning" /tmp/check.txt | head -10
cargo clippy -- -D warnings 2>&1 | tee /tmp/clippy.txt | tail -3 && grep "^error\|^warning" /tmp/clippy.txt | head -10

# Scoped test runs:
# Most tasks here touch src/loop_engine/* — run the loop_engine module tests
cargo test loop_engine 2>&1 | tee /tmp/test.txt | tail -10 && grep "FAILED\|error\[" /tmp/test.txt | head -10

# Tests in tests/overflow_recovery.rs (TEST-INIT-004, TEST-002):
cargo test --test overflow_recovery 2>&1 | tee /tmp/test.txt | tail -10 && grep "FAILED\|error\[" /tmp/test.txt | head -10

# Tests in tests/overflow_filesystem.rs (TEST-001):
cargo test --test overflow_filesystem 2>&1 | tee /tmp/test.txt | tail -10 && grep "FAILED\|error\[" /tmp/test.txt | head -10
```

**Do NOT** run the entire workspace test suite during regular iterations.

### Milestone gate (MILESTONE-1 / -2 / -FINAL)

```bash
cargo fmt --check && cargo check && cargo clippy -- -D warnings && cargo test 2>&1 | tee /tmp/milestone.txt | tail -15 && grep "FAILED\|error\[" /tmp/milestone.txt | head -20
cargo run --bin gen-docs -- --check 2>&1 | tee /tmp/gendocs.txt | tail -5
```

If ANY test fails — including pre-existing failures — the milestone fixes them. **Default: attempt every failure inline** unless there are >12 unrelated failures (then spawn a single FIX-xxx and `<promise>BLOCKED</promise>`).

---

## Common Wiring Failures (CODE-REVIEW-1 reference)

New code must be reachable from production. Most common misses for THIS PRD:

- `pub mod overflow;` missing from `src/loop_engine/mod.rs` → overflow.rs unreachable
- `OverflowEvent` defined but `append_event_log` never called from `handle_prompt_too_long` → JSONL never written
- `format_iteration_banner_with_recovery` defined but the iteration banner site (engine.rs ~line 1793) still uses the old inline format → annotation never appears
- `escalate_below_opus` defined but the `PromptTooLong` arm in engine.rs never calls it → recovery gap not actually closed
- `section_sizes` populated in `build_prompt` but `handle_prompt_too_long` reads `prompt_result.dropped_sections` and forgets the new field → dump header empty

Use grep on `escalate_below_opus`, `OverflowEvent`, `handle_prompt_too_long`, `format_iteration_banner_with_recovery`, `sanitize_id_for_filename`, `section_sizes` to verify each new symbol has at least one production call site.

---

## Review Tasks

Review-type tasks (`CODE-REVIEW-1`, `REFACTOR-REVIEW-FINAL`) spawn follow-up tasks for each issue found. The loop re-reads state every iteration, so spawned tasks are picked up automatically.

| Review                  | Priority | Spawns (priority)                  | Before            | Focus                                                                                                   |
| ----------------------- | -------- | ---------------------------------- | ----------------- | ------------------------------------------------------------------------------------------------------- |
| CODE-REVIEW-1           | 13       | `CODE-FIX` / `WIRE-FIX` (14-16)    | MILESTONE-1       | No `unwrap()`, error propagation, sanitization invariant, banner gating on `overflow_recovered` not `model_overrides`, wiring reachable |
| REFACTOR-REVIEW-FINAL   | 70       | `REFACTOR-xxx` (71-85)             | MILESTONE-FINAL   | All code + tests: DRY, complexity, coupling, clarity, pattern adherence — full-context final pass        |

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

`--depended-on-by` wires the new task into the milestone's `dependsOn` AND syncs the PRD JSON atomically. Commit with `chore: <REVIEW-ID> - Add <FIX|REFACTOR> tasks`, then emit `<task-status><REVIEW-ID>:done</task-status>`.

---

## Progress Report Format

APPEND a block to `tasks/progress-{{TASK_PREFIX}}.txt` (create with a one-line header if missing). Keep it **tight**.

```
## [YYYY-MM-DD HH:MM] - [TASK-ID]
Approach: [one sentence — what you chose and why]
Files: [comma-separated paths touched]
Learnings: [1-3 bullets, one line each]
---
```

Target: ~10 lines per block.

---

## Stop and Blocked Conditions

### Stop Condition

Before outputting `<promise>COMPLETE</promise>`:

1. Verify ALL stories have `passes: true`
2. Verify no new tasks were created in final review
3. Verify all milestones pass

### Blocked Condition

If blocked: document blocker in progress file, create `CLARIFY-xxx` task with priority 0, commit, output `<promise>BLOCKED</promise>`.

---

## Milestones

Milestones (MILESTONE-1, MILESTONE-2, MILESTONE-FINAL) are **full-gate checkpoints** — they prove the trunk is green before the next phase begins. They are NOT a sweep to rewrite remaining tasks.

1. Check all `dependsOn` tasks have `passes: true`. If any don't, the milestone can't run yet.
2. **Run the full quality gate** (see Quality Checks § Milestone gate).
3. **Leave the repo green.** Trivial fixes inline; non-trivial → spawn `FIX-xxx` via `task-mgr add --stdin --depended-on-by <THIS-MILESTONE>`.
4. Mark `<task-status>MILESTONE-N:done</task-status>` only when full gate is green.

---

## Key Learnings (from task-mgr recall)

These are pre-distilled learnings relevant to this PRD. Treat them as authoritative — do NOT Read `tasks/long-term-learnings.md` or `tasks/learnings.md` unless a task explicitly needs a learning that isn't here.

- **#1856** — `IterationContext.model_overrides: HashMap<task_id, model_id>` is the established seam for per-task model recovery on `PromptTooLong`. Effort downgrade preserves model; model escalation preserves effort. This PRD extends the same pattern with rung 2 (Sonnet→Opus before Opus→Opus[1M]).
- **#1861** — `IterationContext` is per-slot, NOT thread-safe; each parallel slot gets its own. Justifies the "no parallel-slot rotation contention" simplification — same task can't be in two slots concurrently because `next::next()` claims via `status=in_progress, run_id=current`.
- **#893** — Separate crash escalation from retry escalation in loop engines. Crash escalation (transient, per-iteration via `check_crash_escalation`) and consecutive-failure escalation (DB-level via `escalate_task_model_if_needed`) MUST stay in their own channels — neither writes to `ctx.model_overrides`. This is why the banner gates on a dedicated `overflow_recovered: HashSet<String>` rather than inferring overflow from `model_overrides`.
- **#851** — Follow existing escalation patterns for consistency. `escalate_below_opus` should mirror the style of `to_1m_model` and `downgrade_effort`: `Option<&str> → Option<&'static str>`, no allocations, match expression.
- **#854** — FEAT-004 (prior model escalation) was implemented in a single pass. Precedent for adding `escalate_below_opus` and `handle_prompt_too_long` without scope creep.
- **#522** — Incremental field addition to `IterationResult` / `PromptResult` / `IterationContext` is a known, safe pattern. Add the field with a default initializer at all construction sites; keep the change additive.
- **#209** — `IterationResult.effective_model` is `None` for early exits, `Some(...)` on the main path. The new stderr messages MUST use `effective_model.as_deref().unwrap_or("(default)")` — `"None"` should never leak into user-visible strings.
- **#165** — `resolve_task_model` follows a multi-level fallback chain: `task_model > difficulty='high' > prd_default > project_default > user_default`. Recovery overrides (this PRD's `model_overrides`) sit ABOVE this chain at the iteration-level computation in `engine.rs:1761`.
- **#1832** — `IterationContext` carries mutable state between iterations. Pattern precedent for adding `overflow_recovered` and `overflow_original_model` as new cross-iteration fields.
- **#1853** — Worktree paths sanitize branch names via an allowlist. Direct precedent for `sanitize_id_for_filename` — same allowlist style (`[A-Za-z0-9._-]`, replace others with `-`).
- **#1370** — Linear pipeline orchestrators are acceptable beyond the 30-line guideline. `handle_prompt_too_long` is a 6-step linear pipeline (ctx → DB → stderr → dump → JSONL → rotate); ~80 lines is fine if it reads top-to-bottom.

---

## CLAUDE.md Excerpts (only what applies to this PRD)

These bullets are extracted from `CLAUDE.md` for the subsystems this PRD touches. Do NOT Read the full file.

**Database / worktrees:**
- The Ralph loop database is at `.task-mgr/tasks.db` (relative to the project/worktree root). Each worktree has its own copy.
- Main worktree: `$HOME/projects/task-mgr`. Feature worktrees: `$HOME/projects/task-mgr-worktrees/<branch-name>/`.

**Model IDs and effort mapping:**
- All Claude model IDs and the difficulty→effort mapping live in `src/loop_engine/model.rs` (`OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL` constants and the `EFFORT_FOR_DIFFICULTY` table).
- After bumping a value there, run `cargo run --bin gen-docs` to regenerate the MODELS block in `.claude/commands/tasks.md`. CI runs `--check` and fails on stale doc.
- Tests import the constants; a regression test (`tests/no_hardcoded_models.rs`) ensures literal model strings don't creep back in outside `model.rs`.
- For this PRD: `escalate_below_opus` MUST use `SONNET_MODEL` and `OPUS_MODEL` constants, not literal strings.

**Loop CLI essentials:**
- Add a task: `echo '{...}' | task-mgr add --stdin`
- Permission guard: loop iterations deny Edit/Write on `tasks/*.json` via `--disallowedTools`. Never edit those files directly.
- Mark status via `<task-status>TASK-ID:status</task-status>` tag.

**Slot merge-back conflict resolution** (for context — this PRD doesn't touch the merge resolver, but it lives in the same `src/loop_engine/` tree):
- `merge_slot_branches_with_resolver` in `src/loop_engine/worktree.rs` runs `git merge --no-edit` from slot 0 for each ephemeral slot branch.
- `ClaudeMergeResolver` in `src/loop_engine/merge_resolver.rs` is invoked on conflict.

**Test & build output convention:**
- ALWAYS pipe through `tee` + `grep` in the SAME command. Never run twice. Example:
  ```bash
  cargo test 2>&1 | tee /tmp/test-results.txt | tail -10 && grep "FAILED\|error\[" /tmp/test-results.txt | head -10
  ```

**task-mgr loop integration with this PRD:**
- The recovery code we're modifying (`src/loop_engine/engine.rs:2044-2113`) runs after each iteration's outcome classification. The Claude CLI subprocess returns "Prompt is too long" detected by `detection.rs::classify_crash`.
- Loop iterations are scoped per-task; each iteration may select a new task (or the same task on retry). `IterationContext` is the per-slot state carrier.

---

## Data Flow Contracts

These are **verified access patterns** for cross-module data structures. Use these exactly — do NOT guess key types.

### Path 1: `PromptResult.section_sizes` → dump header

```rust
// Producer side — in src/loop_engine/prompt.rs::build_prompt:
//   section_sizes: Vec<(&'static str, usize)>  (positional, order-preserving)
let mut section_sizes: Vec<(&'static str, usize)> = Vec::new();
section_sizes.push(("task", task_section.len()));
section_sizes.push(("base_prompt", base_prompt_section.len()));
// ... (one push per named section, in assembly order)

// Consumer side — in src/loop_engine/engine.rs::handle_prompt_too_long:
//   Iterate IN ORDER, do NOT convert to HashMap (loses meaning)
for (name, size) in &prompt_result.section_sizes {
    eprintln!("  {}: {}", name, size);
}

// Dump header construction in overflow.rs:
let header = DumpHeader {
    sections: prompt_result.section_sizes.as_slice(),  // borrow, not clone
    ...
};
```

**Type transition flag:** `section_sizes` stays as `Vec<(&'static str, usize)>` end-to-end on the Rust side. When serialized to JSONL via `serde`, it becomes a JSON **array** of `[name, size]` pairs (NOT an object). This preserves order across the boundary. The consumer-side `jq '.sections[] | .[0]'` extracts names; `jq '.sections | map(.[1]) | add'` sums sizes.

### Path 2: `IterationContext.model_overrides` and `overflow_recovered`

```rust
// Producer side — in handle_prompt_too_long (rung 2):
ctx.model_overrides.insert(task_id.clone(), OPUS_MODEL.to_string());
ctx.overflow_recovered.insert(task_id.clone());
// Capture original ONLY on first overflow (resolved Q6):
ctx.overflow_original_model
    .entry(task_id.clone())
    .or_insert_with(|| effective_model.as_deref().unwrap_or("(default)").to_string());

// Consumer side — in next iteration's effective_model derivation (engine.rs:1761):
//   Reader is unchanged; rung 2 just inserts into the existing map
let effective_model = ctx.model_overrides.get(&task_id).cloned()
    .or_else(|| /* existing fallback chain */);

// Consumer side — banner annotation (engine.rs ~1793):
let recovery_suffix = if ctx.overflow_recovered.contains(&task_id) {
    match ctx.overflow_original_model.get(&task_id) {
        Some(orig) => format!(" (overflow recovery from {})", orig),
        None => " (overflow recovery)".to_string(),  // defensive fallback
    }
} else {
    String::new()
};
```

**Type transition flag:** `model_overrides` is `HashMap<String, String>` (task_id → model_id). `overflow_recovered` is `HashSet<String>` (task_id only). `overflow_original_model` is `HashMap<String, String>` (task_id → ORIGINAL model captured on first overflow). The banner MUST gate on `overflow_recovered.contains()`, NEVER on `model_overrides.contains_key()` — see learning #893.

### Path 3: `OverflowEvent` → JSONL line

```rust
// Producer side — in handle_prompt_too_long, after dump_prompt:
let event = OverflowEvent {
    ts: chrono::Utc::now().to_rfc3339(),
    task_id: task_id.to_string(),  // RAW (unsanitized) for JSON; sanitized form is in dump_path
    run_id: run_id.map(String::from),
    iteration,
    model: effective_model.as_deref().map(String::from),
    effort: effort.map(String::from),
    prompt_bytes: prompt_result.prompt.len(),
    sections: prompt_result.section_sizes.clone(),
    dropped_sections: prompt_result.dropped_sections.clone(),
    recovery: action,  // RecoveryAction enum
    dump_path: dump_path.to_string_lossy().into_owned(),
};

// Atomic write (resolved Q5 - single write_all):
let mut line = serde_json::to_vec(&event)?;
line.push(b'\n');
let mut file = OpenOptions::new().append(true).create(true).open(&jsonl_path)?;
file.write_all(&line)?;

// Consumer side — out of scope for this PRD, documented for completeness:
// for line in BufReader::new(File::open(jsonl_path)?).lines() {
//     let event: OverflowEvent = serde_json::from_str(&line?)?;
//     match event.recovery {
//         RecoveryAction::Blocked => { /* ... */ },
//         RecoveryAction::EscalateModel { new_model } => { /* ... */ },
//         _ => {},
//     }
// }
```

**Type transition flag:** `RecoveryAction` is a Rust enum that becomes a tagged JSON object via `#[serde(tag = "action", rename_all = "snake_case")]`. The `action` field discriminates variants; payload fields (`new_model`, `new_effort`) are siblings of `action` in the JSON, NOT nested under a "data" key.

---

## Key Context for handle_prompt_too_long (FEAT-005)

The function being extracted from the existing `PromptTooLong` arm. Approximate signature:

```rust
pub(crate) fn handle_prompt_too_long(
    ctx: &mut IterationContext,
    conn: &Connection,
    task_id: &str,
    effort: Option<&str>,
    effective_model: Option<&str>,
    prompt_result: &PromptResult,
    iteration: u32,
    run_id: Option<&str>,
    base_dir: &Path,  // .task-mgr/ root; tests pass tempdir
) -> RecoveryAction
```

**Body (linear pipeline):**

```rust
// Step 1: pick recovery rung
let action = if let Some(next_effort) = model::downgrade_effort(effort) {
    ctx.effort_overrides.insert(task_id.to_string(), next_effort);
    RecoveryAction::DowngradeEffort { new_effort: next_effort.to_string() }
} else if let Some(next_model) = model::escalate_below_opus(effective_model) {
    ctx.model_overrides.insert(task_id.to_string(), next_model.to_string());
    RecoveryAction::EscalateModel { new_model: next_model.to_string() }
} else if let Some(m1m) = model::to_1m_model(effective_model) {
    ctx.model_overrides.insert(task_id.to_string(), m1m.to_string());
    RecoveryAction::To1mModel { new_model: m1m.to_string() }
} else {
    RecoveryAction::Blocked
};

// Step 2: ALL rungs (incl. blocked) capture overflow markers — first time only
ctx.overflow_recovered.insert(task_id.to_string());
ctx.overflow_original_model
    .entry(task_id.to_string())
    .or_insert_with(|| effective_model.as_deref().unwrap_or("(default)").to_string());

// Step 3: DB UPDATE (status='todo' on rungs 1-3, 'blocked' on rung 4)
let new_status = if matches!(action, RecoveryAction::Blocked) { "blocked" } else { "todo" };
let _ = conn.execute(
    if new_status == "todo" {
        "UPDATE tasks SET status = ?1, started_at = NULL WHERE id = ?2 AND status = 'in_progress'"
    } else {
        "UPDATE tasks SET status = ?1 WHERE id = ?2 AND status = 'in_progress'"
    },
    rusqlite::params![new_status, task_id],
);

// Step 4: stderr message (one of four phrasings — see Recovery Messages section)
match &action { /* eprintln! per rung */ }

// Step 5-7: best-effort observability (dump → JSONL → rotate)
//   Each step uses match Err(e) => eprintln!("warning: ...", e), do NOT propagate
let dumps_dir = base_dir.join("overflow-dumps");
let header = DumpHeader { /* ... */ };
let dump_path = match overflow::dump_prompt(&dumps_dir, task_id, iteration, &header, &prompt_result.prompt) {
    Ok(p) => p,
    Err(e) => { eprintln!("warning: overflow dump write failed: {}", e); /* fallback path */ }
};
let event = OverflowEvent { /* ... */ };
if let Err(e) = overflow::append_event_log(base_dir, &event) {
    eprintln!("warning: overflow event log append failed: {}", e);
}
let sanitized = overflow::sanitize_id_for_filename(task_id);
if let Err(e) = overflow::rotate_dumps_keep_n(&dumps_dir, &sanitized, 3) {
    eprintln!("warning: overflow dump rotation failed: {}", e);
}

action
```

### Recovery Messages (FEAT-005 must emit these EXACTLY)

```rust
RecoveryAction::DowngradeEffort { new_effort } =>
    eprintln!("Prompt is too long for {} at effort {} — downgrading effort to {}",
        task_id, effort.unwrap_or("(default)"), new_effort),

RecoveryAction::EscalateModel { new_model } =>
    eprintln!("Prompt is too long for {} at effort {}, model {} — escalating model to {} (effort floor reached)",
        task_id, effort.unwrap_or("(default)"), effective_model.as_deref().unwrap_or("(default)"), new_model),

RecoveryAction::To1mModel { new_model } =>
    eprintln!("Prompt is too long for {} at effort {}, model {} — escalating to 1M-context variant {} (already at Opus)",
        task_id, effort.unwrap_or("(default)"), effective_model.as_deref().unwrap_or("(default)"), new_model),

RecoveryAction::Blocked =>
    eprintln!("Prompt is too long for {} at effort {}, model {} — no recovery available (already at Opus[1M] with effort=high)",
        task_id, effort.unwrap_or("(default)"), effective_model.as_deref().unwrap_or("(default)")),
```

---

## Important Rules

- Work on **ONE story per iteration**
- **Commit frequently** after each passing story
- **Keep CI green** — never commit failing code
- **Read before writing** — always read files first
- **Minimal changes** — only implement what's required
- **No parallel-slot contention** — same task can't be in two slots (next::next claims via run_id+status), so dump rotation is best-effort by design
- **Override persistence is for the loop's lifetime** — once a task escalates, it stays escalated; never clear `model_overrides` / `overflow_recovered` after a successful iteration
- **First-overflow capture only** — `overflow_original_model` uses `entry().or_insert()`, NEVER `insert()`
