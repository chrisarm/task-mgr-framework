# PRD: Model Selection — Phase 1 Review Fixes + Phase 2 Engine Integration

**Type**: Enhancement + Feature
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-02-20
**Status**: Draft
**Depends On**: `prd-model-selection.md` (Phase 1 — data layer + pure model logic, **complete**)
**Supersedes**: `prd-model-selection-phase2.md` (original Phase 2 draft — updated with architect review decisions)

---

## 1. Overview

### Problem Statement

Phase 1 established the data model and pure model resolution logic. A thorough code review found 2 P1 bugs and 4 P2 improvements that must be fixed before Phase 2 can safely build on them. Phase 2 itself — wiring model selection into the live loop engine — is unimplemented: the `spawn_claude` subprocess receives no `--model` flag, the iteration header doesn't show the active model, and crash recovery doesn't escalate models.

This PRD covers both scopes as a single body of work:
- **Part A**: Fix 6 code review findings in the Phase 1 foundation
- **Part B**: Wire model resolution, subprocess flags, escalation policy, crash recovery, and observability into the engine

### Background

Phase 1 deliverables (complete and merged):
- `src/loop_engine/model.rs` — pure functions: `model_tier`, `resolve_task_model`, `resolve_iteration_model`, `escalate_model`
- `src/db/migrations/v7.rs` — `tasks.{model, difficulty, escalation_note}`, `prd_metadata.default_model`
- Parse/import/export round-trip for all new fields
- `NextTaskOutput` carries model/difficulty/escalation_note
- `build_task_json()` includes these fields in the prompt's task JSON block

---

## 2. Goals

### Primary Goals
- [ ] Fix all P1/P2 issues from Phase 1 code review before building on them
- [ ] Loop engine resolves the iteration model before spawning Claude
- [ ] `claude` subprocess receives `--model` flag when a model is resolved
- [ ] Escalation policy template loaded from file and injected for non-opus models
- [ ] Crash recovery escalates model one tier automatically (sonnet baseline when None)
- [ ] Iteration header and progress log display the active model name
- [ ] `run_iteration` parameter count reduced via `IterationParams` struct

### Success Metrics
- `cargo test` passes (zero regressions)
- `cargo clippy` clean (zero new warnings)
- Empty-string model `""` is normalized to `None` (not passed through as `--model ""`)
- Case-insensitive `"High"` triggers opus escalation same as `"high"`
- Integration: PRD with sonnet default + one high-difficulty task -> iteration uses opus
- Integration: crash on task with None model -> retry uses opus (via sonnet baseline)
- Escalation policy appears in prompt for haiku/sonnet/None, absent for opus

---

## 2.5. Quality Dimensions

### Correctness Requirements
- Empty/whitespace model strings must normalize to `None`, not short-circuit the resolution chain
- Difficulty comparison must be case-insensitive (`"High"` = `"high"` = `"HIGH"`)
- `truncate_to_budget` message must accurately describe what's being measured (bytes, not chars)
- Crash escalation with `None` model must not silently no-op — assume sonnet baseline
- Escalation policy must be included for `Default`/`None` tier (CLI default could be sonnet)
- Export SQL must use named columns, not positional indices, to prevent breakage on schema changes

### Performance Requirements
- Synergy cluster query is O(task_count) per iteration — acceptable for PRDs < 100 tasks
- Escalation template loaded from disk each iteration (enables hot-editing, no cache)
- No additional subprocess spawns — model flag is just an arg to existing `spawn_claude`

### Style Requirements
- Follow existing patterns: `Option<&str>` for new parameters, `#[serde(skip_serializing_if)]` for JSON
- Doc comments on all public functions and non-obvious behavior
- No `.unwrap()` on DB queries in the prompt builder — use `unwrap_or_default` or `?`
- `run_iteration` must use an `IterationParams` struct, not 18+ positional parameters

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| `model: ""` (empty string) in PRD JSON | Blocks difficulty escalation and PRD default fallback | Normalize to `None`, fall through resolution chain |
| `model: "  "` (whitespace only) | Same as empty string but less obvious | Normalize to `None` via `.trim().is_empty()` |
| `difficulty: "High"` (capitalized) | Users may capitalize in JSON | Case-insensitive compare, triggers opus |
| `difficulty: "HIGH"` (all caps) | Same as above | Case-insensitive compare, triggers opus |
| Crash with `None` resolved model | `escalate_model(None)` returns `None` — crash recovery is a no-op | Assume `SONNET_MODEL` as baseline, escalate to opus |
| Crash on same task twice (already at opus) | `escalate_model(opus)` returns opus | Stay at opus ceiling, no infinite escalation |
| Escalation template missing from disk | First-time users won't have the file | Print stderr warning, continue without injection |
| `max_by_key` tie-breaking in iteration model | Two `Default`-tier models in synergy cluster | Returns last element (document this behavior) |
| PRD with no `default_model` and no task models | All resolution returns `None` | `--model` flag omitted entirely, CLI default used |
| All synergy tasks are `done`/`irrelevant` | Cluster query returns empty | Only primary task participates in resolution |

---

## 3. User Stories

### Part A: Review Fixes

#### US-A01: Normalize Empty Model Strings
**As a** PRD author who accidentally writes `"model": ""`
**I want** the system to treat it as "no model specified"
**So that** it doesn't block difficulty escalation and PRD defaults

**Acceptance Criteria:**
- [ ] `resolve_task_model(Some(""), ...)` normalizes to fall-through (not `Some("")`)
- [ ] `resolve_task_model(Some("  "), ...)` normalizes whitespace-only to fall-through
- [ ] Existing test `test_resolve_task_model_empty_string_is_valid_override` updated to expect fall-through
- [ ] Precedence table test gains 3 new rows for empty-string cases
- [ ] `resolve_task_model(Some(""), Some("high"), None)` returns `Some(OPUS_MODEL)` (not `Some("")`)

**touchesFiles:** `src/loop_engine/model.rs`

---

#### US-A02: Case-Insensitive Difficulty Comparison
**As a** PRD author who writes `"difficulty": "High"`
**I want** it to trigger opus escalation same as `"high"`
**So that** case doesn't silently break model selection

**Acceptance Criteria:**
- [ ] `difficulty == Some("high")` changed to case-insensitive comparison
- [ ] `"High"`, `"HIGH"`, `"hIgH"` all trigger opus escalation
- [ ] Doc comment updated to note case-insensitivity
- [ ] Precedence table test gains rows for case variants

**touchesFiles:** `src/loop_engine/model.rs`

---

#### US-A03: Fix Truncation Budget Message
**As a** developer reading truncation messages in prompts
**I want** the message to accurately describe the unit of measurement
**So that** I'm not confused about byte vs char budgets

**Acceptance Criteria:**
- [ ] `"[truncated to {} chars]"` changed to `"[truncated to {} bytes]"`
- [ ] All 9 test assertions updated to match new message
- [ ] No functional change to truncation logic itself

**touchesFiles:** `src/loop_engine/prompt.rs`

---

#### US-A04: Named Columns in Export SQL
**As a** maintainer adding columns to the tasks table
**I want** export queries to use named columns instead of positional indices
**So that** column reordering doesn't silently corrupt data

**Acceptance Criteria:**
- [ ] `load_tasks` query changes from `row.get(0)?` to `row.get("id")?` etc. (13 replacements)
- [ ] `load_prd_metadata` query in export already uses positions — also convert to named
- [ ] All existing export tests pass without changes

**touchesFiles:** `src/commands/export/prd.rs`

---

#### US-A05: Documentation Fixes
**As a** developer reading model selection code
**I want** doc comments that explain non-obvious behavior
**So that** I don't write incorrect code based on wrong assumptions

**Acceptance Criteria:**
- [ ] `resolve_iteration_model` doc comment notes that `max_by_key` returns the last tied element
- [ ] `PrdFile.model` doc comment explains it maps to `prd_metadata.default_model` in the DB

**touchesFiles:** `src/loop_engine/model.rs`, `src/commands/init/parse.rs`

---

### Part B: Phase 2 Engine Integration

#### US-B00: Extract IterationParams Struct
**As a** maintainer of the loop engine
**I want** `run_iteration`'s 17 parameters grouped into a struct
**So that** adding new parameters doesn't make the function signature worse

**Acceptance Criteria:**
- [ ] New `IterationParams` struct contains all current parameters except `ctx: &mut IterationContext`
- [ ] `run_iteration` signature becomes `(ctx: &mut IterationContext, params: &IterationParams) -> TaskMgrResult<IterationResult>`
- [ ] `run_loop` call site updated to construct `IterationParams`
- [ ] Purely mechanical — no logic changes, all existing tests pass
- [ ] `#[allow(clippy::too_many_arguments)]` removed

**touchesFiles:** `src/loop_engine/engine.rs`

---

#### US-B01: Claude Subprocess `--model` Flag
**As a** loop engine
**I want** to pass `--model <model>` to the Claude subprocess
**So that** the correct model runs for each iteration

**Acceptance Criteria:**
- [ ] `spawn_claude()` signature adds `model: Option<&str>` parameter
- [ ] When `model` is `Some(m)`, args include `--model` and `m` before `-p`
- [ ] When `model` is `None`, no `--model` flag is passed (CLI default)
- [ ] All existing call sites updated to pass `None` (engine.rs, learnings/ingestion/mod.rs, tests)
- [ ] Test: `CLAUDE_BINARY=echo` verifies `--model` appears in echoed args when specified
- [ ] Test: `CLAUDE_BINARY=echo` verifies no `--model` when None

**touchesFiles:** `src/loop_engine/claude.rs`, `src/loop_engine/engine.rs`, `src/learnings/ingestion/mod.rs`

---

#### US-B02: Iteration Header Model Display
**As a** user watching the loop output
**I want** to see which model each iteration is using
**So that** I can verify the model selection is working correctly

**Acceptance Criteria:**
- [ ] `print_iteration_header()` accepts `model: Option<&str>` parameter
- [ ] When model is `Some`, prints: `Model: claude-sonnet-4-6`
- [ ] When model is `None`, prints: `Model: (default)`
- [ ] When crash escalation occurred, the header shows the escalated model (not the original)

**touchesFiles:** `src/loop_engine/display.rs`, `src/loop_engine/engine.rs`

---

#### US-B03: Progress Log Model Field
**As a** user reviewing progress.txt after a loop session
**I want** to see which model was used for each iteration
**So that** I can correlate model tier with iteration outcomes

**Acceptance Criteria:**
- [ ] `log_iteration()` accepts `model: Option<&str>` parameter
- [ ] Progress entry includes `- Model: <value>` or `- Model: (default)` line
- [ ] All existing tests pass with `None` as the new parameter
- [ ] New tests verify model appears in log output

**touchesFiles:** `src/loop_engine/progress.rs`, `src/loop_engine/engine.rs`

---

#### US-B04: Prompt Builder Model Resolution
**As a** loop engine operator
**I want** the prompt builder to resolve the correct model for each iteration
**So that** the iteration uses the highest-tier model needed for the synergy cluster

**Acceptance Criteria:**
- [ ] `BuildPromptParams` gains `default_model: Option<&'a str>` field
- [ ] `PromptResult` gains `resolved_model: Option<String>` field
- [ ] New `resolve_synergy_cluster_model(conn, task_id, task_model, task_difficulty, default_model) -> Option<String>`:
  - Resolves primary task via `model::resolve_task_model()`
  - Queries pending (not done/irrelevant) `synergyWith` tasks' model/difficulty
  - Resolves each synergy task via `model::resolve_task_model()`
  - Combines all into `model::resolve_iteration_model()`
  - Normalizes `Some("")` to `None`
- [ ] When selected task has empty `touchesFiles`, synergy cluster is just the selected task
- [ ] When `prd_metadata` has no `default_model`, falls back to `None` gracefully
- [ ] `default_model` is threaded via `BuildPromptParams`, not queried inside prompt builder

**touchesFiles:** `src/loop_engine/prompt.rs`

---

#### US-B05: Escalation Policy Template Injection
**As a** non-opus AI agent
**I want** to receive escalation instructions in my prompt
**So that** I know to stop, revert, and escalate difficulty when I'm struggling

**Acceptance Criteria:**
- [ ] Template file lives at `tasks/scripts/escalation-policy.md` (resolved via `base_prompt_path.parent()`)
- [ ] Template instructs the agent to: stop, revert changes, set difficulty to "high", add escalationNote, end iteration
- [ ] `build_prompt()` loads the template when `resolved_model` tier is **not Opus**
- [ ] **Includes injection for `Default`/`None` tier** (CLI default could be sonnet, policy is safe for opus)
- [ ] Escalation section injected BEFORE the reorder instruction section
- [ ] When template file is missing, warning printed to stderr, no injection (graceful degradation)
- [ ] When `resolved_model` tier is Opus, escalation section NOT injected

**touchesFiles:** `src/loop_engine/prompt.rs`, `tasks/scripts/escalation-policy.md` (new)

---

#### US-B06: Crash Recovery Model Escalation Fields
**As a** loop engine recovering from a crash
**I want** the model to auto-escalate one tier on retry
**So that** a more capable model handles the task that caused the crash

**Acceptance Criteria:**
- [ ] `IterationContext` gains `last_task_id: Option<String>` and `last_was_crash: bool`
- [ ] Both initialized to `None`/`false` in `IterationContext::new()`
- [ ] Fields are loop-thread-local (doc comment stating no concurrency concern)

**touchesFiles:** `src/loop_engine/engine.rs`

---

#### US-B07: Engine Orchestration — Wire It All Together
**As a** loop engine
**I want** `run_iteration()` to orchestrate model resolution, escalation, and spawning in the correct order
**So that** all model selection features work together seamlessly

**Acceptance Criteria:**
- [ ] `PrdMetadata` (engine-private struct) gains `default_model: Option<String>`
- [ ] `read_prd_metadata()` query updated to SELECT `default_model`
- [ ] `default_model` threaded from `run_loop()` through `IterationParams` into `BuildPromptParams`
- [ ] After `build_prompt()`: read `prompt_result.resolved_model`
- [ ] Crash escalation: if `ctx.last_was_crash && ctx.last_task_id == task_id`:
  - If model is `None` → treat as `SONNET_MODEL`, escalate to `OPUS_MODEL`
  - Otherwise → call `escalate_model()` normally
  - Log to stderr: `"Crash escalation: {old} -> {new}"`
- [ ] Pass `effective_model` to `spawn_claude()`, `print_iteration_header()`, `log_iteration()`
- [ ] `IterationResult` gains `effective_model: Option<String>` (set in all return points)
- [ ] `ctx.last_task_id` and `ctx.last_was_crash` updated at end of each iteration
- [ ] Model logged to progress file via `result.effective_model`

**touchesFiles:** `src/loop_engine/engine.rs`

---

## 4. Functional Requirements

### FR-001: Empty Model Normalization
`resolve_task_model` must treat `Some("")` and `Some("  ")` as `None` (no model preference). The check is `task_model.filter(|m| !m.trim().is_empty())`.

### FR-002: Case-Insensitive Difficulty
Difficulty `"high"` check uses `eq_ignore_ascii_case`. No other difficulty values trigger escalation.

### FR-003: Prompt Builder Model Resolution Flow
In `build_prompt()`, after selecting the task:
1. Resolve primary task: `resolve_task_model(task.model, task.difficulty, params.default_model)`
2. Query pending synergyWith tasks' model/difficulty from `task_relationships` + `tasks`
3. Resolve each synergy task via `resolve_task_model`
4. Combine all into `resolve_iteration_model([primary, ...synergy])`
5. Normalize `Some("")` → `None`
6. Store in `PromptResult.resolved_model`

### FR-004: Subprocess Model Flag
`spawn_claude()` builds args as:
```
claude --print --dangerously-skip-permissions [--model <model>] -p <prompt>
```
The `--model` flag is inserted before `-p` only when `model` is `Some`.

### FR-005: Escalation Template Loading
Loaded once per prompt build (not cached, enables hot-editing):
1. Resolve path: `base_prompt_path.parent()/scripts/escalation-policy.md`
2. Skip if `model_tier(resolved_model) == ModelTier::Opus`
3. Read file, inject content into prompt
4. Missing file → stderr warning, no injection

### FR-006: Crash Escalation Timing
Escalation happens AFTER `build_prompt()` resolves the base model but BEFORE `spawn_claude()`:
```
build_prompt() → resolved_model
if crash_retry && same_task:
    if resolved_model is None:
        effective_model = OPUS_MODEL  (sonnet baseline → opus)
    else:
        effective_model = escalate_model(resolved_model)
else:
    effective_model = resolved_model
spawn_claude(..., effective_model)
```

### FR-007: IterationParams Struct
Replace `run_iteration`'s 17 positional parameters with:
```rust
pub struct IterationParams<'a> {
    pub conn: &'a Connection,
    pub db_dir: &'a Path,
    pub project_root: &'a Path,
    pub tasks_dir: &'a Path,
    pub iteration: u32,
    pub max_iterations: u32,
    pub run_id: &'a str,
    pub base_prompt_path: &'a Path,
    pub steering_path: Option<&'a Path>,
    pub inter_iteration_delay: Duration,
    pub signal_flag: &'a SignalFlag,
    pub elapsed_secs: u64,
    pub verbose: bool,
    pub usage_params: &'a UsageParams,
    pub prd_path: Option<&'a Path>,
    pub task_prefix: Option<&'a str>,
    pub default_model: Option<&'a str>,  // NEW
}
```

---

## 5. Non-Goals (Out of Scope)

- **`--resume` session management**: Each iteration is a fresh `--print -p` call, not session-based
- **Per-model usage throttling**: Usage thresholds don't vary by model
- **Per-model iteration counting**: All iterations count equally regardless of model
- **`TASK_MGR_DEFAULT_MODEL` env var**: Could set an implicit baseline for unconfigured users — deferred to future phase
- **Logging model to `runs`/`run_tasks` table**: Could add `model` column later — deferred

---

## 6. Technical Considerations

### Affected Components

| File | Change |
|------|--------|
| `src/loop_engine/model.rs` | A1: normalize empty strings, A2: case-insensitive difficulty, A5: docs |
| `src/loop_engine/prompt.rs` | A3: truncation msg, B4: model resolution + synergy query, B5: escalation injection |
| `src/loop_engine/claude.rs` | B1: `--model` flag parameter |
| `src/loop_engine/display.rs` | B2: model in iteration header |
| `src/loop_engine/progress.rs` | B3: model in progress log |
| `src/loop_engine/engine.rs` | B0: IterationParams, B6: crash fields, B7: full orchestration |
| `src/commands/export/prd.rs` | A4: named columns |
| `src/commands/init/parse.rs` | A5: doc comment |
| `src/learnings/ingestion/mod.rs` | B1: spawn_claude call site update |
| `tasks/scripts/escalation-policy.md` | B5: new template file |

### Dependencies
- **Phase 1 complete**: model.rs module, v7 migration, parse/import/export fields
- **`claude --model` flag**: Assumed functional in Claude CLI
- **SQLite 3.35+**: Not required — migration v7 uses ADD COLUMN (supported in all versions)

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| **A: Thread `default_model` via BuildPromptParams** | Single source of truth, caller controls query timing, testable | One more field on the params struct | **Preferred** |
| B: Query `default_model` inside `build_prompt()` | Self-contained, fewer params | Hidden DB access, harder to test, violates DI | Rejected |
| **A: Sonnet baseline for crash escalation with None** | Crash recovery always works, pragmatic default | Assumes CLI default is sonnet-equivalent | **Preferred** |
| B: No-op for crash escalation with None | No assumptions about CLI default | Crash recovery silently useless for unconfigured users | Rejected |
| **A: Escalation policy for all non-Opus tiers** | Safe for sonnet/haiku/unknown, informational for opus | Slightly longer prompt for opus-on-None users | **Preferred** |
| B: Skip for both Opus and None | Shorter prompt for unconfigured users | Misses sonnet/haiku users running without explicit config | Rejected |

**Selected Approach**: A for all three — pragmatic correctness over theoretical purity.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `--model ""` passed to Claude CLI causes error | Medium | Low | A1 normalizes empty strings to None before they reach spawn_claude |
| Escalation template missing on first use | Low | Medium | Stderr warning + graceful degradation; template created as part of this work |
| IterationParams refactor introduces subtle regression | Medium | Low | Purely mechanical (compiler catches field mismatches); run full test suite |
| Synergy cluster query slow with many tasks | Low | Low | O(n) for typical PRDs; index on task_relationships if needed later |
| Sonnet baseline assumption wrong (CLI default is haiku) | Low | Low | Escalating to opus is still better than no-op; user can set explicit default_model |

### Security Considerations
- Model string is passed directly as CLI arg — validate no shell injection possible (Rust `Command` is safe by default, args are not shell-interpreted)
- No new user input surfaces — model comes from PRD JSON or DB, both controlled by the user

### Public Contracts

#### Modified Interfaces

| Module | Current Signature | Proposed Signature | Breaking? | Migration |
|--------|-------------------|-------------------|-----------|-----------|
| `loop_engine::claude::spawn_claude` | `(prompt, signal_flag, working_dir)` | `(prompt, signal_flag, working_dir, model: Option<&str>)` | Yes (internal) | Add `None` at 3 call sites |
| `loop_engine::display::print_iteration_header` | `(iteration, max, task_id, elapsed)` | `(iteration, max, task_id, elapsed, model: Option<&str>)` | Yes (internal) | Add `None` at 1 call site |
| `loop_engine::progress::log_iteration` | `(path, iteration, task_id, outcome, files)` | `(path, iteration, task_id, outcome, files, model: Option<&str>)` | Yes (internal) | Add `None` at 1 call site |
| `loop_engine::engine::run_iteration` | `(ctx, conn, db_dir, ... 17 params)` | `(ctx: &mut IterationContext, params: &IterationParams)` | Yes (internal) | Construct IterationParams at 1 call site |
| `loop_engine::prompt::BuildPromptParams` | 11 fields | 12 fields (+`default_model`) | Yes (internal) | Add `default_model: None` at all construction sites |
| `loop_engine::prompt::PromptResult` | 4 fields | 5 fields (+`resolved_model`) | Yes (internal) | Read `resolved_model` at 1 call site in engine.rs |

#### New Interfaces

| Module | Signature | Returns | Side Effects |
|--------|-----------|---------|-------------|
| `prompt::resolve_synergy_cluster_model` | `(conn, task_id, task_model, task_difficulty, default_model)` | `Option<String>` — highest-tier model for cluster | DB read (synergy tasks) |
| `prompt::get_synergy_task_models` | `(conn, task_id, default_model)` | `Vec<Option<String>>` — resolved models per synergy task | DB read |
| `prompt::append_escalation_policy` | `(prompt, base_prompt_path, resolved_model)` | `()` — mutates prompt string | File I/O (template read) |
| `engine::IterationParams` | Struct with 17 fields | N/A — parameter container | None |

### Inversion Checklist
- [x] All callers of `spawn_claude` identified? — engine.rs, learnings/ingestion/mod.rs, tests
- [x] All callers of `print_iteration_header` identified? — 1 call in engine.rs
- [x] All callers of `log_iteration` identified? — 1 call in engine.rs
- [x] What if model string is empty? — A1 normalizes to None before resolution
- [x] What if difficulty is capitalized? — A2 uses case-insensitive comparison
- [x] What if crash + None model? — Assume sonnet baseline, escalate to opus
- [x] What if escalation template has wrong content? — Pass-through to Claude; user's responsibility
- [x] What if two `PrdMetadata` structs conflict? — engine.rs struct is private; export struct is separate
- [x] Thread safety of new IterationContext fields? — Loop-thread-local, doc commented

---

## 7. Open Questions

- [x] Where should escalation-policy.md live? → `tasks/scripts/` (resolved via `base_prompt_path.parent()`)
- [x] Should escalation template be cached? → No, reload each iteration for hot-editing
- [x] Crash escalation with None model? → Assume sonnet baseline, escalate to opus
- [x] Escalation policy for Default/None tier? → Include (skip only for Opus)
- [x] IterationParams refactor? → Yes, as prerequisite step B0
- [ ] Should `resolved_model` be logged to `run_tasks` table? → Defer to future phase

---

## 8. Implementation Order

```
A1-A5 (review fixes)    <- independent, do first
   |
   v
B0 (IterationParams)    <- mechanical refactor, prerequisite for B7
   |
   v
B1 (spawn_claude)  --+
B2 (display)        --+
B3 (progress)       --+-- independent leaf changes
B4 (prompt builder) --+
B5 (escalation)     --+
B6 (engine fields)  --+
   |
   v
B7 (engine wiring)      <- depends on all of B0-B6
```

---

## Appendix

### Escalation Policy Template

```markdown
## Model Escalation Policy

You are running as a **cost-optimized model** for this iteration. If you encounter
significant difficulty meeting the task's acceptance criteria -- for example, repeated
test failures, architectural complexity beyond your confidence level, or you find
yourself going in circles -- follow this escalation procedure:

1. **Stop** your current implementation effort immediately.
2. **Revert** only the files you changed during this iteration:
   run `git diff --name-only | xargs git checkout --`
3. **Update the task** in the PRD JSON file:
   - Set `"difficulty": "high"` on the task object.
   - Add an `"escalationNote"` field with a brief explanation of what went wrong
     and what approach you attempted (this helps the next iteration).
4. **End this iteration** -- do not attempt the task again.

The next iteration will automatically use a more capable model for high-difficulty tasks.
Do NOT set difficulty to high preemptively -- only escalate after a genuine failed attempt.
```

### Prompt Section Ordering (updated)

1. Steering (from steering.md)
2. Session Guidance (from .pause interactions)
3. Reorder Hint (from previous iteration)
4. Source Context (from touchesFiles)
5. Completed Dependencies
6. Synergy Tasks
7. Current Task (JSON block — includes model/difficulty/escalationNote)
8. Relevant Learnings
9. Non-code task completion instruction (if applicable)
10. **Escalation Policy (NEW — skip only for Opus tier)**
11. Reorder instruction
12. Base Prompt (from prompt.md)

### Related Documents
- `tasks/prd-model-selection.md` — Phase 1 PRD (prerequisite, complete)
- `tasks/prd-model-selection-phase2.md` — Original Phase 2 draft (superseded by this PRD)
