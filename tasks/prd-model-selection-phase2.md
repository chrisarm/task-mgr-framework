# PRD: Model Selection & Escalation — Phase 2: Loop Engine Integration

**Type**: Feature
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-02-19
**Status**: Draft
**Depends On**: `prd-model-selection.md` (Phase 1 — data layer + pure model logic)

---

## 1. Overview

### Problem Statement

Phase 1 established the data model (model/difficulty/escalation_note fields in DB and JSON), the pure model resolution logic (`loop_engine::model`), and the NextTaskOutput fields. Phase 2 wires everything into the live loop engine so that:

1. Each iteration resolves the correct model via synergy cluster analysis
2. The `claude` subprocess receives `--model <model>`
3. Non-opus iterations get an escalation policy loaded from a template file
4. Crash recovery auto-escalates the model one tier
5. The iteration header displays the active model

### Background

Phase 1 deliverables (assumed complete):
- `src/loop_engine/model.rs` — pure functions: `model_tier`, `resolve_task_model`, `resolve_iteration_model`, `escalate_model`
- `src/db/migrations/v6.rs` — `tasks.{model, difficulty, escalation_note}`, `prd_metadata.default_model`
- Parse/import/export round-trip for all new fields
- `NextTaskOutput` carries model/difficulty/escalation_note
- `build_task_json()` includes these fields in the prompt's task JSON block

---

## 2. Goals

### Primary Goals
- [ ] Loop engine resolves the iteration model before spawning Claude
- [ ] `claude` subprocess receives `--model` flag when a model is resolved
- [ ] Escalation policy template loaded from file and injected for non-opus models
- [ ] Crash recovery escalates model one tier automatically
- [ ] Iteration header displays the active model name

### Success Metrics
- `cargo test` passes (zero regressions)
- Integration test: PRD with haiku default + one high-difficulty task → iteration uses opus
- Integration test: crash on haiku task → retry uses sonnet
- Escalation section appears in prompt for haiku/sonnet, absent for opus

---

## 3. User Stories

#### US-008: Prompt Builder Model Resolution
**As a** loop engine operator
**I want** the prompt builder to resolve the correct model for each iteration
**So that** the iteration uses the highest-tier model needed for the synergy cluster

**Acceptance Criteria:**
- [ ] `build_prompt()` loads `default_model` from `prd_metadata` table (new helper: `load_prd_default_model`)
- [ ] `build_prompt()` loads all pending tasks' `(id, files, model, difficulty)` (new helper: `load_pending_task_models`)
- [ ] Calls `model::resolve_iteration_model()` with the selected task and pending task data
- [ ] `PromptResult` includes new field `resolved_model: Option<String>`
- [ ] When selected task has empty `touchesFiles`, synergy cluster is just the selected task (no panic)
- [ ] When `prd_metadata` has no `default_model`, falls back to None gracefully

**touchesFiles:** `src/loop_engine/prompt.rs`

---

#### US-009: Claude Subprocess `--model` Flag
**As a** loop engine
**I want** to pass `--model <model>` to the Claude subprocess
**So that** the correct model runs for each iteration

**Acceptance Criteria:**
- [ ] `spawn_claude()` signature adds `model: Option<&str>` parameter
- [ ] When `model` is `Some(m)`, args include `--model` and `m` before `-p`
- [ ] When `model` is `None`, no `--model` flag is passed (CLI default)
- [ ] All existing call sites updated to pass `None` (backward compat)
- [ ] Test: `CLAUDE_BINARY=echo` verifies `--model` appears in echoed args when specified
- [ ] Test: `CLAUDE_BINARY=echo` verifies no `--model` when None

**touchesFiles:** `src/loop_engine/claude.rs`, `src/loop_engine/engine.rs`

---

#### US-010: Escalation Policy Template Injection
**As a** non-opus AI agent
**I want** to receive escalation instructions in my prompt
**So that** I know to stop, revert, and escalate difficulty when I'm struggling

**Acceptance Criteria:**
- [ ] Template file lives at `scripts/escalation-policy.md` (co-located with `scripts/prompt.md`)
- [ ] Template content instructs the agent to: stop implementation, revert changes, set difficulty to "high", add escalationNote, end iteration
- [ ] `build_prompt()` loads the template when `resolved_model` is non-opus (tier < 3)
- [ ] Escalation section is injected BEFORE the reorder instruction section in prompt ordering
- [ ] When `resolved_model` is opus or None, escalation section is NOT injected
- [ ] When template file is missing, no section is injected (warning printed to stderr)
- [ ] Template loading uses a new helper function `load_escalation_template(scripts_dir)` for testability

**touchesFiles:** `src/loop_engine/prompt.rs`, `scripts/escalation-policy.md` (new)

---

#### US-011: Crash Recovery Model Escalation
**As a** loop engine recovering from a crash
**I want** the model to auto-escalate one tier on retry
**So that** a more capable model handles the task that caused the crash

**Acceptance Criteria:**
- [ ] `IterationContext` gets two new fields: `last_task_id: Option<String>`, `last_was_crash: bool`
- [ ] At the end of each iteration, update `last_task_id` and `last_was_crash` based on outcome
- [ ] Before spawning Claude, check: if same task as last iteration AND last was a crash → call `model::escalate_model()`
- [ ] Escalation is logged: "Model escalated for crash recovery: haiku → sonnet"
- [ ] If already opus, stays opus (no escalation beyond max tier)
- [ ] If resolved_model is None (CLI default), no escalation (can't escalate unknown baseline)
- [ ] Integration with existing CrashTracker — escalation happens independently of backoff

**touchesFiles:** `src/loop_engine/engine.rs`

---

#### US-012: Iteration Header Model Display
**As a** user watching the loop output
**I want** to see which model each iteration is using
**So that** I can verify the model selection is working correctly

**Acceptance Criteria:**
- [ ] `print_iteration_header()` accepts optional model parameter
- [ ] When model is Some, prints: `Model: claude-sonnet-4-6`
- [ ] When model is None, prints: `Model: (default)`
- [ ] When crash escalation occurred, the header shows the escalated model (not the original)

**touchesFiles:** `src/loop_engine/display.rs`, `src/loop_engine/engine.rs`

---

#### US-013: Engine Orchestration — Wire It All Together
**As a** loop engine
**I want** `run_iteration()` to orchestrate model resolution, escalation, and spawning in the correct order
**So that** all model selection features work together seamlessly

**Acceptance Criteria:**
- [ ] After `build_prompt()`, read `prompt_result.resolved_model`
- [ ] Apply crash escalation if applicable (US-011)
- [ ] Pass effective model to `spawn_claude()` (US-009)
- [ ] Pass effective model to `print_iteration_header()` (US-012)
- [ ] Model logged to progress file on each iteration
- [ ] No change to IterationOutcome enum or return types — model is an internal concern

**touchesFiles:** `src/loop_engine/engine.rs`

---

## 4. Functional Requirements

### FR-006: Prompt Builder Model Resolution Flow
In `build_prompt()`, after selecting the task:
1. Query `SELECT default_model FROM prd_metadata WHERE id = 1`
2. Query all pending tasks with files, model, difficulty
3. For each pending task: call `resolve_task_model(task.model, task.difficulty, prd_default)`
4. Call `resolve_iteration_model(selected_id, selected_files, resolved_task_models)`
5. Store result in `PromptResult.resolved_model`

### FR-007: Subprocess Model Flag
`spawn_claude()` builds args as:
```
claude --print --dangerously-skip-permissions [--model <model>] -p <prompt>
```
The `--model` flag is inserted before `-p` only when `model` is `Some`.

### FR-008: Escalation Template Loading
The template is loaded once per prompt build (not cached across iterations) to allow hot-editing:
1. Resolve path: `{scripts_dir}/escalation-policy.md` where `scripts_dir` is derived from the prompt file's parent directory
2. Read file contents
3. Inject as `## Model Escalation Policy\n\n{contents}\n\n---\n\n` section

### FR-009: Crash Escalation Timing
Escalation happens AFTER `build_prompt()` resolves the base model but BEFORE `spawn_claude()`:
```
build_prompt() → resolved_model
if crash_retry: effective_model = escalate_model(resolved_model)
else: effective_model = resolved_model
spawn_claude(..., effective_model)
```

---

## 5. Non-Goals (Out of Scope)

- **Session resume with `--resume`**: The bash reference uses `--resume` with session IDs. task-mgr doesn't use sessions — each iteration is a fresh `--print -p` call. Not in scope.
- **Heartbeat-based timeout**: Already handled by task-mgr's existing monitor system.
- **Usage API model-aware throttling**: Usage thresholds don't vary by model.
- **Per-model iteration counting**: All iterations count equally regardless of model.

---

## 6. Technical Considerations

### Affected Components

| File | Change |
|------|--------|
| `src/loop_engine/prompt.rs` | Load prd_default_model, load pending task models, resolve iteration model, inject escalation template, add resolved_model to PromptResult |
| `src/loop_engine/claude.rs` | Add `model: Option<&str>` param, conditionally add `--model` to args |
| `src/loop_engine/engine.rs` | Add last_task_id/last_was_crash to IterationContext, crash escalation logic, wire model through spawn_claude and display |
| `src/loop_engine/display.rs` | Add model param to print_iteration_header |
| `scripts/escalation-policy.md` | **NEW** — escalation policy template |

### Dependencies
- **Phase 1 complete**: model.rs module, v6 migration, parse/import/export fields, NextTaskOutput fields
- **`claude --model` flag**: Assumed functional (bash reference uses it)

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `--model` flag position matters in claude CLI | Medium | Low | Test with CLAUDE_BINARY=echo to verify arg ordering; document expected flag position |
| Escalation template missing causes silent no-op | Low | Medium | Print stderr warning when file not found; document required file in README |
| Synergy cluster query is slow with many tasks | Low | Low | O(n*m) scan; typical PRDs <100 tasks. Could add index later if needed |
| Crash escalation + synergy escalation double-escalate | Low | Low | Escalation is tier-based (max = opus), not additive — double-escalation just stays at opus |

### Public Contracts

#### Modified Interfaces

| Module | Current Signature | Proposed Signature | Breaking? | Migration |
|--------|-------------------|-------------------|-----------|-----------|
| `loop_engine::claude::spawn_claude` | `(prompt: &str, signal_flag: Option<&SignalFlag>, working_dir: Option<&Path>)` | `(prompt: &str, signal_flag: Option<&SignalFlag>, working_dir: Option<&Path>, model: Option<&str>)` | Yes (internal) | Add `None` at sole call site in engine.rs |
| `loop_engine::display::print_iteration_header` | `(iteration, max, task_id, elapsed)` | `(iteration, max, task_id, elapsed, model: Option<&str>)` | Yes (internal) | Add `None` at sole call site in engine.rs |

#### New Interfaces

| Module | Signature | Returns | Side Effects |
|--------|-----------|---------|-------------|
| `loop_engine::prompt::load_prd_default_model` | `(conn: &Connection) -> Option<String>` | PRD default model string or None | Read from DB |
| `loop_engine::prompt::load_pending_task_models` | `(conn: &Connection) -> Vec<(String, Vec<String>, Option<String>, Option<String>)>` | `(task_id, files, model, difficulty)` tuples | Read from DB |
| `loop_engine::prompt::load_escalation_template` | `(scripts_dir: &Path) -> Option<String>` | Template content or None | File I/O |

### Inversion Checklist
- [x] All callers of `spawn_claude` identified? — 1 call in engine.rs + tests (all updated)
- [x] All callers of `print_iteration_header` identified? — 1 call in engine.rs
- [x] What if template file has incorrect content? — Pass-through to Claude; user's responsibility
- [x] What if model string is empty string `""`? — `model_tier("")` returns Default; `Some("")` still passes `--model ""` which may error — guard against empty in resolve functions
- [x] Race condition: PRD JSON modified between model resolution and Claude reading it? — Acceptable: Claude reads JSON independently, model was resolved at claim time

---

## 7. Open Questions

- [x] Where should escalation-policy.md live? → `scripts/` directory (next to `scripts/prompt.md`)
- [x] Should escalation template be cached? → No, reload each iteration to allow hot-editing
- [ ] Should `PromptResult.resolved_model` be logged to the `runs` table? → Defer to Phase 2 implementation; can always add a `model` column to `run_tasks` later

---

## Appendix

### Escalation Policy Template (initial content)

```markdown
## Model Escalation Policy

You are running as a **cost-optimized model** for this iteration. If you encounter
significant difficulty meeting the task's acceptance criteria — for example, repeated
test failures, architectural complexity beyond your confidence level, or you find
yourself going in circles — follow this escalation procedure:

1. **Stop** your current implementation effort immediately.
2. **Revert** only the files you changed during this iteration:
   run `git diff --name-only | xargs git checkout --`
3. **Update the task** in the PRD JSON file:
   - Set `"difficulty": "high"` on the task object.
   - Add an `"escalationNote"` field with a brief explanation of what went wrong
     and what approach you attempted (this helps the next iteration).
4. **End this iteration** — do not attempt the task again.

The next iteration will automatically use a more capable model for high-difficulty tasks.
Do NOT set difficulty to high preemptively — only escalate after a genuine failed attempt.
```

### Prompt Section Ordering (updated)

1. Steering (from steering.md)
2. Session Guidance (from .pause interactions)
3. Reorder Hint (from previous iteration)
4. Source Context (from touchesFiles)
5. Completed Dependencies
6. Synergy Tasks
7. Current Task (JSON block — now includes model/difficulty/escalationNote)
8. Relevant Learnings
9. Non-code task completion instruction (if applicable)
10. **Escalation Policy (NEW — only for non-opus models)**
11. Reorder instruction
12. Base Prompt (from prompt.md)

### Related Documents
- `tasks/prd-model-selection.md` — Phase 1 PRD (prerequisite)
- `tasks/backlog.md` — Original design discussion
- `linked_projects/external-ref/claude-loop.sh` lines 900-1404 — Bash reference
