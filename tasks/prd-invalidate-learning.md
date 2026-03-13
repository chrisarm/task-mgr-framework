# PRD: Invalidate Learning Command

**Type**: Feature
**Priority**: P2 (Medium)
**Author**: Claude Code
**Created**: 2026-03-07
**Status**: Draft

---

## 1. Overview

### Problem Statement

When the Claude subprocess discovers a learning is wrong, it has no way to signal this. Bad learnings persist and get surfaced via UCB ranking, actively misleading future iterations. The subprocess needs a command to degrade and eventually retire incorrect learnings.

### Background

The learnings system uses a UCB bandit algorithm to surface relevant learnings during `recall`. A wrong learning that gets surfaced wastes iterations and can cause cascading failures. The `curate retire` command exists for bulk curation but requires manual thresholds. The subprocess needs a targeted, single-ID command it can call immediately when it detects a bad learning.

The `retired_at` column (migration v10) and confidence levels (High/Medium/Low) already exist. The two-step degradation (downgrade confidence first, retire on second call) prevents accidental data loss from a single false positive.

---

## 2. Goals

### Primary Goals

- [ ] Subprocess can signal that a specific learning is wrong via CLI
- [ ] Two-step degradation: first call downgrades confidence to Low, second call retires
- [ ] Already-retired learnings produce a clear error (no silent re-retirement)

### Success Metrics

- All 7 unit tests pass
- `cargo clippy -- -D warnings` clean
- Command is discoverable via `task-mgr --help`

---

## 2.5. Quality Dimensions

### Correctness Requirements

- First invalidation MUST set confidence to Low regardless of current level (High, Medium, or Low-but-not-yet-retired)
- Second invalidation (when confidence is already Low) MUST set `retired_at` to current timestamp
- Already-retired learnings MUST return an error, not silently succeed
- Non-existent learning IDs MUST return `TaskMgrError::learning_not_found`

### Performance Requirements

- Best effort — single-row operations on an indexed primary key

### Style Requirements

- Follow `apply_learning.rs` structural template (result struct, function, format_text, tests)
- Use existing `edit_learning()` for confidence downgrade (no raw SQL for fields covered by CRUD)
- Use raw SQL only for `retired_at` update (matching existing `curate retire` pattern)
- No `.unwrap()` outside tests

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
| --- | --- | --- |
| Learning already retired | Could silently succeed, hiding bugs | Return `InvalidState` error |
| Learning not found | ID typo or race condition | Return `NotFound` error |
| Confidence already Low (first call) | Downgrade is a no-op but should still set Low | Downgrade still writes Low via `edit_learning`, returns "downgraded" action |
| Two rapid calls | First sets Low, second retires | Correct sequencing — no race since single-threaded CLI |

---

## 3. User Stories

### US-001: Subprocess Invalidates a Wrong Learning

**As a** Claude subprocess running in the loop
**I want** to call `task-mgr invalidate-learning <id>` when I discover a learning is wrong
**So that** the learning is degraded and eventually excluded from future `recall` results

**Acceptance Criteria:**

- [ ] `task-mgr invalidate-learning 42` downgrades confidence from High/Medium to Low
- [ ] Second call to `task-mgr invalidate-learning 42` (already Low) sets `retired_at`
- [ ] Text output clearly indicates the action taken ("downgraded" vs "retired")
- [ ] JSON output includes `learning_id`, `title`, `previous_confidence`, `action`, `new_confidence`
- [ ] Non-existent ID returns non-zero exit code with error message
- [ ] Already-retired ID returns non-zero exit code with error message

---

## 4. Functional Requirements

### FR-001: `invalidate-learning` Command

Two-step degradation for a single learning by ID.

**Details:**

- Fetch learning via `get_learning(conn, id)` — error if not found
- Check if learning is already retired (query `retired_at IS NOT NULL`) — error if true
- If confidence != Low: downgrade to Low via `edit_learning(conn, id, EditLearningParams { confidence: Some(Confidence::Low), ..Default::default() })`
- If confidence == Low: retire via `UPDATE learnings SET retired_at = datetime('now') WHERE id = ?1`

**Result struct** (`InvalidateLearningResult`):
- `learning_id: i64`
- `title: String`
- `previous_confidence: String` (serialized form: "high", "medium", "low")
- `action: String` ("downgraded" | "retired")
- `new_confidence: Option<String>` (Some("low") when downgraded, None when retired)

**Text output format:**
- Downgrade: `Invalidated learning #42: "Title" (confidence: medium -> low)`
- Retire: `Retired learning #42: "Title" (was already low confidence)`

**Validation:**

- `cargo test --lib` passes all 7 unit tests
- `cargo clippy -- -D warnings` clean

---

## 5. Non-Goals (Out of Scope)

- **Batch invalidation** — Reason: `curate retire` covers bulk operations
- **Undo/unretire integration** — Reason: `curate unretire` already exists for this
- **UCB score adjustment** — Reason: confidence downgrade naturally reduces UCB ranking
- **Reason/explanation field** — Reason: the subprocess doesn't need to explain why; the action itself is the signal

---

## 6. Technical Considerations

### Affected Components

- `src/commands/invalidate_learning.rs` — NEW: core logic, result struct, format_text, tests
- `src/commands/mod.rs` — add `pub mod invalidate_learning;` + re-exports
- `src/cli/commands.rs` — add `InvalidateLearning` variant to `Commands` enum
- `src/main.rs` — add dispatch match arm
- `src/handlers.rs` — add `impl_text_formattable!` macro line

### Dependencies

- `crate::learnings::crud::read::get_learning` — fetch learning by ID
- `crate::learnings::crud::update::edit_learning` — downgrade confidence
- `crate::learnings::crud::types::EditLearningParams` — params struct (Default)
- `crate::models::Confidence` — enum for confidence levels
- `crate::TaskMgrError` — `learning_not_found()`, `invalid_state()`

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
| --- | --- | --- | --- |
| A: Single new file with inline retired_at check SQL | Follows apply_learning pattern exactly; self-contained; easy to test | Duplicates retired_at check logic (but it's one query) | **Preferred** |
| B: Add `invalidate` method to learnings CRUD module | Groups with other CRUD operations | Mixes command-level logic (two-step degradation) with pure CRUD; more files touched | Rejected |

**Selected Approach**: Approach A — single new command file (`src/commands/invalidate_learning.rs`) following the `apply_learning.rs` template. The retired_at check is a simple `SELECT retired_at FROM learnings WHERE id = ?1` query that doesn't warrant a new CRUD abstraction.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
| --- | --- | --- | --- |
| `get_learning` doesn't return `retired_at` field | Med — need separate query to check retirement status | High (confirmed: `Learning` struct lacks `retired_at`) | Use direct SQL query: `SELECT retired_at FROM learnings WHERE id = ?1` |
| Confidence already Low on first call still "downgrades" | Low — technically correct but potentially confusing | Low | `edit_learning` with `confidence: Some(Low)` is idempotent; action is "downgraded" regardless |
| Race between two processes calling invalidate | Low — CLI is single-threaded, lock guard protects | Very Low | LockGuard in main.rs dispatch ensures mutual exclusion |

### Security Considerations

- No user input beyond a validated i64 learning ID — no injection risk
- Lock guard prevents concurrent modification

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
| --- | --- | --- | --- | --- |
| `commands::invalidate_learning::invalidate_learning` | `(conn: &Connection, learning_id: i64)` | `InvalidateLearningResult { learning_id, title, previous_confidence, action, new_confidence }` | `TaskMgrError::NotFound` (missing ID), `TaskMgrError::InvalidState` (already retired) | DB: updates confidence or sets retired_at |
| `commands::invalidate_learning::format_text` | `(result: &InvalidateLearningResult)` | `String` | — | None |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
| --- | --- | --- | --- | --- |
| `cli::Commands` enum | N/A (new variant) | + `InvalidateLearning { learning_id: i64 }` | No | Additive enum variant |
| `commands::mod.rs` | N/A (new re-export) | + `pub use invalidate_learning::*` | No | Additive |

### Inversion Checklist

- [x] All callers identified and checked? (New command — no existing callers)
- [x] Routing/branching decisions that depend on output reviewed? (N/A)
- [x] Tests that validate current behavior identified? (N/A — new feature)
- [x] Different semantic contexts for same code discovered and documented? (N/A)

---

## 7. Open Questions

None — implementation spec is fully defined.

---

## Appendix

### Related Documents

- `docs/INTEGRATION.md` — updated with usage examples
- `src/commands/apply_learning.rs` — structural template
- `src/commands/curate/mod.rs` — retire SQL pattern reference

### Glossary

- **UCB**: Upper Confidence Bound — bandit algorithm used to rank learnings for recall
- **Soft-archive**: Setting `retired_at` timestamp to exclude from queries without deleting data
