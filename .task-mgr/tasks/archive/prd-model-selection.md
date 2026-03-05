# PRD: Model Selection & Escalation for task-mgr

**Type**: Feature
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-02-19
**Status**: Draft
**Phases**: 2 (this document covers Phase 1: Data Layer + Pure Logic; Phase 2: Loop Engine Integration)

---

## 1. Overview

### Problem Statement

task-mgr's loop engine always spawns Claude with the CLI default model. Users who want cost-optimized runs (haiku for easy tasks, opus for hard ones) have no mechanism to control model selection. The bash reference (`claude-loop.sh`) already supports this via PRD-level defaults, per-task overrides, difficulty-based escalation, synergy cluster analysis, and crash-recovery model upgrades — but task-mgr's Rust implementation has none of it.

### Background

The design was discussed and settled in `tasks/backlog.md`:

- **PRD-level default model**: One model for the whole project (e.g., haiku for cheap runs)
- **Per-task override**: Individual tasks can specify a different model
- **Difficulty escalation**: Tasks with `difficulty: "high"` auto-use Opus unless explicitly overridden
- **Synergy cluster resolution**: When tasks overlap in files, the iteration uses the highest-tier model in the cluster
- **Agent self-escalation**: Non-opus agents that struggle can set `difficulty: "high"` and bail, so the next iteration uses opus
- **Crash recovery escalation**: On crash/retry, bump the model one tier (haiku→sonnet→opus)
- **Escalation prompt**: Non-opus iterations get an escalation policy section loaded from a template file

The bash reference implementation is at `linked_projects/external-ref/claude-loop.sh` lines 900-1404.

---

## 2. Goals

### Primary Goals
- [ ] PRD JSON supports `model` at top level and per-task, plus `difficulty` and `escalationNote` per-task
- [ ] Data round-trips faithfully: JSON → SQLite → JSON preserves all model/difficulty fields
- [ ] Pure model resolution logic exists with full test coverage (no I/O dependencies)
- [ ] Loop engine passes `--model` to Claude subprocess based on resolved model
- [ ] Non-opus iterations get escalation policy injected from a template file
- [ ] Crash recovery escalates model one tier automatically

### Success Metrics
- All existing tests pass (zero regressions)
- Model resolution logic has >95% branch coverage via unit tests
- PRD round-trip test: JSON with model/difficulty fields survives init→export unchanged

---

## 3. User Stories

### Phase 1: Data Layer + Pure Model Logic

#### US-001: PRD Default Model Field
**As a** PRD author
**I want** to set a default model at the PRD top level
**So that** all tasks in the project use a cost-optimized model unless overridden

**Acceptance Criteria:**
- [ ] `PrdFile` struct accepts optional `"model"` field (camelCase in JSON)
- [ ] Value is stored in `prd_metadata.default_model` column (TEXT, nullable)
- [ ] Exported JSON includes `"model"` at top level when present
- [ ] Missing field defaults to `None` (backward compatible)

**touchesFiles:** `src/commands/init/parse.rs`, `src/commands/init/import.rs`, `src/commands/export/prd.rs`, `src/db/migrations/v6.rs`

---

#### US-002: Per-Task Model Override
**As a** PRD author
**I want** to override the model on specific tasks
**So that** tricky tasks use a more capable model without changing the whole project default

**Acceptance Criteria:**
- [ ] `PrdUserStory` struct accepts optional `"model"` field
- [ ] Value stored in `tasks.model` column (TEXT, nullable)
- [ ] `Task` struct has `model: Option<String>` field
- [ ] `TryFrom<&Row>` reads model with `.ok().flatten()` pattern (handles pre-v6 DBs)
- [ ] Export includes `"model"` per-task when present
- [ ] Round-trip: JSON with per-task model → init → export → field preserved

**touchesFiles:** `src/commands/init/parse.rs`, `src/commands/init/import.rs`, `src/commands/export/prd.rs`, `src/models/task.rs`, `src/db/migrations/v6.rs`

---

#### US-003: Task Difficulty Field
**As a** PRD author or AI agent
**I want** to set a difficulty level on tasks
**So that** high-difficulty tasks automatically use Opus

**Acceptance Criteria:**
- [ ] `PrdUserStory` struct accepts optional `"difficulty"` field (string: "low", "medium", "high")
- [ ] Value stored in `tasks.difficulty` column (TEXT, nullable)
- [ ] `Task` struct has `difficulty: Option<String>` field
- [ ] Export includes `"difficulty"` per-task when present
- [ ] Round-trip preserves the field
- [ ] Agent can write `difficulty: "high"` to the PRD JSON and it survives re-import

**touchesFiles:** `src/commands/init/parse.rs`, `src/commands/init/import.rs`, `src/commands/export/prd.rs`, `src/models/task.rs`, `src/db/migrations/v6.rs`

---

#### US-004: Escalation Note Field
**As an** AI agent that struggled with a task
**I want** to leave an escalation note explaining what went wrong
**So that** the next iteration (with a more capable model) has context on the failed approach

**Acceptance Criteria:**
- [ ] `PrdUserStory` struct accepts optional `"escalationNote"` field
- [ ] Value stored in `tasks.escalation_note` column (TEXT, nullable)
- [ ] `Task` struct has `escalation_note: Option<String>` field
- [ ] Export includes `"escalationNote"` per-task when present
- [ ] Round-trip preserves the field

**touchesFiles:** `src/commands/init/parse.rs`, `src/commands/init/import.rs`, `src/commands/export/prd.rs`, `src/models/task.rs`, `src/db/migrations/v6.rs`

---

#### US-005: Database Migration v6
**As a** user with an existing task-mgr database
**I want** the schema to auto-upgrade when I update the binary
**So that** model/difficulty columns are available without manual intervention

**Acceptance Criteria:**
- [ ] Migration v6 adds `model TEXT`, `difficulty TEXT`, `escalation_note TEXT` to `tasks` table
- [ ] Migration v6 adds `default_model TEXT` to `prd_metadata` table
- [ ] `CURRENT_SCHEMA_VERSION` bumped to 6
- [ ] Migration runs automatically on DB open (existing pattern)
- [ ] Down migration reverts version number (columns left as NULL — SQLite compat)
- [ ] Existing v5 databases upgrade cleanly with no data loss

**touchesFiles:** `src/db/migrations/v6.rs` (new), `src/db/migrations/mod.rs`

---

#### US-006: Pure Model Resolution Logic
**As a** developer
**I want** all model selection logic in a pure, testable module
**So that** model resolution can be tested without databases or subprocesses

**Acceptance Criteria:**
- [ ] New module `src/loop_engine/model.rs` with `pub mod model;` in `mod.rs`
- [ ] `ModelTier` enum: `Default(0)`, `Haiku(1)`, `Sonnet(2)`, `Opus(3)` — derives Ord
- [ ] `model_tier(Option<&str>) -> ModelTier` — substring match on "opus"/"sonnet"/"haiku"
- [ ] `resolve_task_model(task_model, task_difficulty, prd_default) -> Option<String>`
  - Precedence: task.model > (difficulty=="high" → OPUS_MODEL) > prd_default > None
- [ ] `resolve_iteration_model(selected_id, selected_files, pending_tasks_with_models) -> Option<String>`
  - Builds synergy cluster (selected + pending tasks with overlapping touchesFiles)
  - Returns highest-tier model in the cluster
- [ ] `escalate_model(current: Option<&str>) -> Option<String>` — haiku→sonnet→opus→opus, None→None
- [ ] Constants: `OPUS_MODEL`, `SONNET_MODEL`, `HAIKU_MODEL`
- [ ] Unit tests for all functions covering: precedence rules, tier ordering, synergy clustering, escalation, edge cases (None inputs, unknown model strings, empty touchesFiles)

**touchesFiles:** `src/loop_engine/model.rs` (new), `src/loop_engine/mod.rs`

---

#### US-007: NextTaskOutput Model Fields
**As a** consumer of the `next` command output
**I want** model, difficulty, and escalation_note in the task output
**So that** the loop engine and CLI users can see model-relevant task metadata

**Acceptance Criteria:**
- [ ] `NextTaskOutput` struct has `model: Option<String>`, `difficulty: Option<String>`, `escalation_note: Option<String>`
- [ ] Fields populated from task data in `build_task_output()`
- [ ] JSON output of `task-mgr next` includes these fields when present
- [ ] Prompt builder's `build_task_json()` includes model/difficulty/escalationNote in the task JSON block

**touchesFiles:** `src/commands/next/output.rs`, `src/loop_engine/prompt.rs`

---

### Phase 2: Loop Engine Integration (Separate PRD)

The following will be covered in a Phase 2 PRD after Phase 1 lands:

- **US-P2-001**: Prompt builder integrates model resolution (loads prd_default_model, builds synergy cluster, calls resolve_iteration_model)
- **US-P2-002**: `spawn_claude()` accepts `--model` flag
- **US-P2-003**: Escalation policy template file (`scripts/escalation-policy.md`) loaded and injected into prompt for non-opus iterations
- **US-P2-004**: `IterationContext` tracks `last_task_id` and `last_was_crash` for crash-recovery escalation
- **US-P2-005**: Display shows model name in iteration header
- **US-P2-006**: End-to-end integration test (PRD with mixed models → loop selects correct model per iteration)

---

## 4. Functional Requirements

### FR-001: JSON Field Parsing (camelCase)
All new JSON fields use camelCase naming per existing convention (`#[serde(rename_all = "camelCase")]`):
- Top-level: `"model"` → `PrdFile.model`
- Per-task: `"model"`, `"difficulty"`, `"escalationNote"` → `PrdUserStory.{model, difficulty, escalation_note}`

All fields are optional with `#[serde(default)]` — existing PRDs without them parse unchanged.

### FR-002: Model Resolution Precedence
For a single task, the effective model is:
1. `task.model` (explicit override) — if present, used as-is
2. `task.difficulty == "high"` → `"claude-opus-4-6"` — auto-escalation
3. `prd.model` (project default) — fallback
4. `None` — CLI default (no `--model` flag passed)

Only `"high"` difficulty triggers auto-escalation. Other values (`"low"`, `"medium"`) are informational.

### FR-003: Synergy Cluster Model Selection
For an iteration, the model is the **highest tier** among:
- The selected task's resolved model
- Resolved models of other **pending** tasks whose `touchesFiles` overlap with the selected task's `touchesFiles`

Tier ordering: Opus(3) > Sonnet(2) > Haiku(1) > Default(0).

### FR-004: Model Escalation on Crash Recovery
When the loop retries a task after a crash:
- `haiku` → escalate to `sonnet`
- `sonnet` → escalate to `opus`
- `opus` → stays `opus`
- `None` → stays `None` (can't escalate unknown baseline)

### FR-005: Escalation Template File
A template file at `scripts/escalation-policy.md` (co-located with existing `scripts/prompt.md`) contains the escalation policy text. The loop engine loads this file and injects it into the prompt when running a non-opus model. If the file is missing, no escalation section is injected (graceful degradation, with a warning).

---

## 5. Non-Goals (Out of Scope)

- **Automatic difficulty estimation**: No ML/heuristic to auto-set difficulty — it's manual or agent-set
- **Model cost tracking**: No token usage or cost reporting per model
- **Per-model timeout adjustment**: Timeouts remain per-task `timeoutSecs`, not model-based
- **Model validation**: No validation that model strings are real Claude model IDs — pass-through to CLI
- **Batch mode model selection**: The `--batch` mode in `claude-loop.sh` is out of scope for task-mgr

---

## 6. Technical Considerations

### Affected Components

| File | Change |
|------|--------|
| `src/loop_engine/model.rs` | **NEW** — pure model resolution + escalation logic |
| `src/loop_engine/mod.rs` | Add `pub mod model;` |
| `src/db/migrations/v6.rs` | **NEW** — add 3 columns to tasks, 1 to prd_metadata |
| `src/db/migrations/mod.rs` | Bump `CURRENT_SCHEMA_VERSION` to 6, add v6 to array |
| `src/commands/init/parse.rs` | Add model/difficulty/escalation_note to parse structs |
| `src/models/task.rs` | Add 3 fields to Task struct + TryFrom |
| `src/commands/init/import.rs` | Add columns to INSERT/UPDATE queries |
| `src/commands/export/prd.rs` | Add fields to export structs + SELECT queries |
| `src/commands/next/output.rs` | Add fields to NextTaskOutput |
| `src/loop_engine/prompt.rs` | Include model/difficulty/escalationNote in task JSON block |

### Dependencies
- SQLite `ALTER TABLE ADD COLUMN` — supported in all SQLite versions (nullable columns only)
- `claude --model <model>` CLI flag — assumed to work (verified in bash reference)

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Migration breaks existing databases | High | Low | Nullable columns only; `.ok().flatten()` in TryFrom handles missing columns; migration test on v5 DB |
| Agent writes invalid model string to PRD | Low | Medium | Pass-through design — invalid model causes Claude CLI error, not task-mgr crash. Loop continues to next iteration. |
| Synergy cluster resolution is expensive with many tasks | Low | Low | O(n*m) where n=pending tasks, m=avg files per task. Typical PRDs have <100 tasks. |

### Public Contracts

#### New Interfaces

| Module | Signature | Returns | Side Effects |
|--------|-----------|---------|-------------|
| `loop_engine::model::model_tier` | `(model: Option<&str>) -> ModelTier` | `ModelTier` enum variant | None |
| `loop_engine::model::resolve_task_model` | `(task_model: Option<&str>, difficulty: Option<&str>, prd_default: Option<&str>) -> Option<String>` | Resolved model string or None | None |
| `loop_engine::model::resolve_iteration_model` | `(selected_id: &str, selected_files: &[String], task_models: &[(String, Vec<String>, Option<String>)]) -> Option<String>` | Highest-tier model in synergy cluster | None |
| `loop_engine::model::escalate_model` | `(current: Option<&str>) -> Option<String>` | Next tier model string | None |

#### Modified Interfaces

| Module | Current | Proposed | Breaking? | Migration |
|--------|---------|----------|-----------|-----------|
| `init::import::insert_task` | `(conn, story)` — 10 columns | Same signature — 13 columns | No | New columns read from PrdUserStory.{model, difficulty, escalation_note} which default to None |
| `init::import::update_task` | `(conn, story)` — 9 SET columns | Same signature — 12 SET columns | No | Same as above |
| `init::import::insert_prd_metadata` | `(conn, prd, raw_json)` — 10 columns | Same signature — 11 columns | No | New column from PrdFile.model |

### Inversion Checklist
- [x] All callers of insert_task/update_task identified? — Yes: `init/mod.rs` only
- [x] All callers of insert_prd_metadata identified? — Yes: `init/mod.rs` only
- [x] TryFrom handles pre-v6 databases? — Yes: `.ok().flatten()` pattern
- [x] Export handles NULL columns? — Yes: `skip_serializing_if = "Option::is_none"`
- [x] Migration is backward compatible? — Yes: nullable ALTERs only

---

## 7. Open Questions

- [x] Should difficulty accept any string or only known values? → **Any string** — only "high" has special meaning, others are informational
- [x] Hardcoded or template escalation prompt? → **Template file** (`scripts/escalation-policy.md`)
- [x] Can agent self-escalate? → **Yes** — agent writes difficulty + escalationNote to PRD JSON

---

## Appendix

### Model Tier Reference

| Model Pattern | Tier | Constant |
|--------------|------|----------|
| Contains "opus" | 3 (Opus) | `OPUS_MODEL = "claude-opus-4-6"` |
| Contains "sonnet" | 2 (Sonnet) | `SONNET_MODEL = "claude-sonnet-4-6"` |
| Contains "haiku" | 1 (Haiku) | `HAIKU_MODEL = "claude-haiku-4-5-20251001"` |
| None / unknown | 0 (Default) | — |

### Precedence Examples

| PRD model | Task model | Task difficulty | Resolved |
|-----------|-----------|-----------------|----------|
| haiku | — | — | haiku |
| haiku | sonnet | — | sonnet |
| haiku | — | high | opus |
| haiku | sonnet | high | sonnet (explicit override wins) |
| — | — | high | opus |
| — | — | — | None (CLI default) |
| — | — | medium | None (only "high" escalates) |

### Related Documents
- `tasks/backlog.md` — Original design discussion
- `linked_projects/external-ref/claude-loop.sh` lines 900-1404 — Bash reference implementation
- `$HOME/.claude/plans/glimmering-stirring-storm.md` — Implementation plan
