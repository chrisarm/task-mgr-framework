# PRD: Recall Score Output + Learning Supersession

**Type**: Enhancement
**Priority**: P2 (Medium)
**Author**: Claude Code
**Created**: 2026-04-21
**Status**: Draft

---

## 1. Overview

### Problem Statement

Two capabilities are missing from task-mgr's learnings system:

1. **Recall discards scores**: The recall pipeline computes rich numeric scores (FTS5 BM25, pattern-match points, vector cosine similarity, UCB bandit) but strips them before output. `--format json` only exposes categorical `confidence` (high/medium/low) and `times_shown`/`times_applied` counters. Loop prompt builders and consumers doing pattern-match confidence scoring have no parseable numeric signal.

2. **No supersession tracking**: When a learning is replaced by a better one, the only mechanism is `invalidate-learning` (two-step degradation to retired). This destroys the old learning without linking it to its replacement. There's no way to answer "what replaced this?" or to auto-filter superseded learnings from recall results.

### Background

The recall pipeline (`src/learnings/recall/mod.rs`) uses a `CompositeBackend` with three retrieval backends (FTS5, Patterns, Vector), each returning `ScoredLearning { learning, relevance_score, match_reason }`. For task-based recall, UCB bandit re-ranking computes a combined score (`relevance_score * 100.0 + ucb_score`). Line 115 discards all scores via `scored.into_iter().map(|s| s.learning).collect()`.

The learnings schema has `learning_tags` and `retired_at` for lifecycle management but no relationship tracking between learnings.

---

## 2. Goals

### Primary Goals

- [ ] Expose numeric relevance, UCB, and combined scores in `recall --format json` output
- [ ] Expose `match_reason` strings from retrieval backends in recall output
- [ ] Track supersession relationships between learnings in a dedicated join table
- [ ] Auto-filter superseded learnings from recall results
- [ ] Enable recording supersession at creation time (`learn --supersedes`) and retroactively (`edit-learning --supersedes`)

### Success Metrics

- `task-mgr --format json recall --query "X"` output contains `relevance_score`, `combined_score`, and `match_reason` fields
- `task-mgr --format json recall --for-task Y` output additionally contains `ucb_score`
- `task-mgr learn --supersedes <old-id>` creates a supersession relationship and downgrades the old learning
- Superseded learnings are excluded from recall by default, includable via `--include-superseded`

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Score fields must reflect the actual internal scores that determined ranking order. If learning A ranks above learning B, then `A.combined_score >= B.combined_score`.
- Supersession must be validated: both `old_learning_id` and `new_learning_id` must exist. Self-supersession (`old == new`) must be rejected.
- Supersession cascade: deleting either learning must clean up the relationship row (ON DELETE CASCADE).
- The `recall_learnings` / `recall_learnings_with_backend` functions used by `next` command and loop engine must remain unchanged — no regressions.

### Performance Requirements

- Supersession filter adds one subquery per retrieval SQL. The `learning_supersessions` table will be tiny (likely <100 rows). No index optimization beyond the UNIQUE constraint and `idx_supersessions_old` needed initially.
- Score computation already happens — we're just preserving values instead of discarding them. No additional DB queries for the scoring feature.

### Style Requirements

- Follow the existing `(score, reason)` tuple pattern from `compute_score()` in patterns.rs (learning #1467).
- New migration follows the established v1-v16 file pattern (learning #1397).
- Base schema files (`src/db/schema/*.rs`) are frozen — new table lives only in migration (learning #835).

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|---|---|---|
| Recall with no backends matching (empty query, no task) | All backends return empty, scores are 0 | Return learnings with `relevance_score: 0.5` (unfiltered FTS5 fallback) and `combined_score: 0.5`, no `ucb_score` |
| Self-supersession (`--supersedes <own-id>`) | Would create circular reference | Reject with error: "A learning cannot supersede itself" |
| Superseding an already-superseded learning | Transitive chains (A superseded by B, B superseded by C) | Allow — the filter is `NOT IN (SELECT old_learning_id ...)` which catches all superseded learnings regardless of chain depth |
| Superseding a retired learning | Learning is already archived | Allow — the relationship is still meaningful for history |
| UCB score for non-task recall | UCB only runs for `--for-task` queries | `ucb_score: null` in JSON (skip_serializing_if = None) |
| Vector backend unavailable (no Ollama) | Graceful degradation already handles this | Scores from other backends are unaffected; match_reason won't mention vector similarity |

---

## 3. User Stories

### US-001: Parseable Recall Scores

**As a** loop prompt builder
**I want** numeric relevance scores in recall JSON output
**So that** I can do confidence-weighted pattern matching on recalled learnings

**Acceptance Criteria:**

- [ ] `--format json` recall output includes `relevance_score` (f64), `combined_score` (f64), and `match_reason` (string, nullable) on each learning
- [ ] For `--for-task` queries, output additionally includes `ucb_score` (f64, nullable)
- [ ] Text format shows scores after the confidence line
- [ ] Scores reflect actual ranking order (combined_score is monotonically non-increasing in output order)

### US-002: Record Supersession

**As a** knowledge curator
**I want** to mark an old learning as superseded by a new one
**So that** recall surfaces the current knowledge, not the outdated version

**Acceptance Criteria:**

- [ ] `task-mgr learn --supersedes <old-id>` creates the new learning AND records the supersession relationship
- [ ] `task-mgr edit-learning <new-id> --supersedes <old-id>` records the relationship retroactively
- [ ] The old learning's confidence is auto-downgraded to `low`
- [ ] Self-supersession is rejected with a clear error

### US-003: Auto-Filter Superseded Learnings

**As a** recall consumer
**I want** superseded learnings excluded from recall results by default
**So that** I see current knowledge without noise from replaced learnings

**Acceptance Criteria:**

- [ ] Recall excludes superseded learnings by default (across FTS5, Patterns, and Vector backends)
- [ ] `--include-superseded` flag overrides the filter
- [ ] `learnings list` annotates superseded learnings with `(superseded by #N)`

---

## 4. Functional Requirements

### FR-001: Score Preservation in Recall Pipeline

Introduce `recall_learnings_scored()` that returns `ScoredRecallResult` carrying `relevance_score`, `ucb_score`, `combined_score`, and `match_reason` per learning. The existing `recall_learnings()` and `recall_learnings_with_backend()` remain unchanged — `next` command and loop engine are not affected.

**Details:**

- Fork the body of `recall_learnings_with_backend` (lines 72-128 in `src/learnings/recall/mod.rs`)
- Instead of discarding scores at line 115, preserve them in `ScoredLearningOutput` structs
- For task-based recall: extract UCB scores during re-ranking (refactor `rerank_with_ucb` to compute scores once, store, then sort)
- For non-task recall: `ucb_score = None`, `combined_score = relevance_score`

**Validation:**

- Output ordering matches `combined_score` descending
- Round-trip JSON serialization preserves all score fields

### FR-002: Score Fields in CLI Output

Add `relevance_score`, `ucb_score`, `combined_score`, and `match_reason` to `LearningSummary` in `src/commands/recall.rs`. Update `format_text()` to show scores. Replace hand-reconstructed match reasons in `format_verbose()` with actual backend `match_reason`.

**Details:**

- `relevance_score: f64` — always present
- `ucb_score: Option<f64>` — skip_serializing_if None
- `combined_score: f64` — always present
- `match_reason: Option<String>` — skip_serializing_if None

**Validation:**

- Adding fields to a Serialize struct is backward-compatible for JSON consumers (unknown fields are ignored by lenient parsers)

### FR-003: Supersession Table (Migration v17)

Create `learning_supersessions` table with `old_learning_id`, `new_learning_id`, `created_at`, and `reason` columns.

**Details:**

- Both FKs reference `learnings(id)` with `ON DELETE CASCADE`
- `UNIQUE(old_learning_id, new_learning_id)` prevents duplicate relationships
- Indexes on both FK columns for efficient joins
- Down migration drops the table entirely (not column-level, so no SQLite version concern)

**Validation:**

- Table exists after `run_migrations()`
- Down migration reverts to v16 cleanly

### FR-004: `--supersedes` Flag on learn and edit-learning

Add `--supersedes <old-id>` to both commands. When provided:

1. Validate: old_learning_id exists, is not the same as new_learning_id
2. Insert into `learning_supersessions`
3. Downgrade old learning's confidence to `low`

**Validation:**

- Self-supersession rejected
- Non-existent old ID rejected
- Old learning's confidence changes to `low`

### FR-005: Supersession Filter in Retrieval

Add `AND l.id NOT IN (SELECT old_learning_id FROM learning_supersessions)` to retrieval queries in FTS5 and Patterns backends. Add `include_superseded: bool` to `RetrievalQuery` to bypass the filter.

**Details:**

- FTS5: add to `execute_fts5_query`, `execute_like_query`, `execute_unfiltered_query` WHERE clauses
- Patterns: add to `load_learnings_with_applicability` WHERE clause
- Vector: filter at the Rust level after loading embeddings (the embedding query doesn't join learnings directly)
- Pass `include_superseded` through `RecallParams` → `RetrievalQuery`

**Validation:**

- Superseded learning does not appear in recall results
- Same learning appears with `--include-superseded`

---

## 5. Non-Goals (Out of Scope)

- **Score normalization to 0-1 range**: Backends intentionally use different scales. Normalizing loses information. Consumers can divide by known maxima using `match_reason` to identify the backend.
- **`learn-tag` subcommand**: The existing `edit-learning --add-tags` / `--remove-tags` already provides full tag management. A dedicated subcommand adds no new capability.
- **Transitive supersession resolution**: If A is superseded by B, and B by C, we don't need to resolve the chain. The `NOT IN` subquery catches all superseded learnings.
- **Supersession in `next` command output**: The `next` command uses the unscored `recall_learnings()` path. Adding scores or supersession annotations to `next` output is a separate enhancement.

---

## 6. Technical Considerations

### Affected Components

| File | Change |
|---|---|
| `src/learnings/recall/mod.rs` | New `ScoredRecallResult`, `ScoredLearningOutput` types; new `recall_learnings_scored()` function; refactor `rerank_with_ucb` to expose UCB scores |
| `src/commands/recall.rs` | Score fields on `LearningSummary`; `--include-superseded` flag; wire to scored function; update `format_text` and `format_verbose` |
| `src/learnings/mod.rs` | Re-export new types and function |
| `src/db/migrations/v17.rs` | New file: `learning_supersessions` table |
| `src/db/migrations/mod.rs` | Register v17, bump `CURRENT_SCHEMA_VERSION` to 17 |
| `src/cli/commands.rs` | `--supersedes` on `Learn` and `EditLearning` variants; `--include-superseded` on `Recall` variant |
| `src/main.rs` | Pass `supersedes` through in Learn and EditLearning dispatch |
| `src/commands/learn.rs` | `LearnParams.supersedes`; insert supersession row after learn |
| `src/learnings/crud/types.rs` | `supersedes: Option<i64>` on `EditLearningParams` |
| `src/learnings/crud/update.rs` | Insert supersession row + downgrade in edit path |
| `src/learnings/retrieval/fts5.rs` | Add supersession NOT IN subquery to 3 query functions |
| `src/learnings/retrieval/patterns.rs` | Add supersession NOT IN subquery to `load_learnings_with_applicability` |
| `src/learnings/retrieval/vector.rs` | Post-query Rust-level filter for superseded IDs |
| `src/learnings/retrieval/mod.rs` | `include_superseded: bool` on `RetrievalQuery` |
| `src/commands/learnings.rs` | Annotate superseded learnings in list output |

### Dependencies

- No new external dependencies
- SQLite built-in subquery support (available in all versions we target)

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| **A: New scored function alongside existing** | Zero risk to `next`/loop callers; clean separation | Small code duplication between `recall_learnings_with_backend` and `recall_learnings_scored` | Preferred |
| **B: Modify `RecallResult` to include scores** | Single function, no duplication | Ripples through `next` command, loop engine; breaks `RecallResult` serialization | Rejected |
| **C: Add scores as optional wrapper around existing** | Minimal duplication via delegation | More complex type layering | Alternative |

**Selected Approach**: A — new `recall_learnings_scored()` function. The duplication is minimal (the function body is ~50 lines) and the isolation from existing callers eliminates regression risk entirely.

**Phase 2 Foundation Check**: The scored function creates a clean extension point for future scoring enhancements (e.g., adding freshness decay, confidence weighting) without touching the stable `recall_learnings` path. N/A for supersession — the join table is already the maximally flexible design.

| Approach | Pros | Cons | Recommendation |
|---|---|---|---|
| **A: Separate `learning_supersessions` table** | Sparse data stays in its own table; clean JOIN semantics; no schema bloat on learnings | Extra table and query | Preferred |
| **B: `superseded_by` column on `learnings`** | Simpler queries; single-table | Adds a nullable column to every row (mostly NULL); can't represent many-to-many if needed later | Rejected |

**Selected Approach**: A — separate table. User explicitly requested this approach because the data is sparse.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| Supersession subquery slows retrieval on large learning sets | Low (query adds ~1ms for <1000 learnings) | Low | Index on `old_learning_id`; learning sets rarely exceed hundreds |
| Score field additions break strict JSON parsers | Medium (consumers crash on unknown fields) | Low | JSON field addition is backward-compatible for standard parsers; `task-mgr` consumers use `serde_json` which ignores unknowns |
| UCB score computation duplicated between `rerank_with_ucb` and `recall_learnings_scored` | Low (maintenance burden) | Medium | Extract a `compute_ucb_for_scored` helper that both call |

### Security Considerations

- No user-facing input reaches SQL without parameterization (existing pattern)
- Supersession IDs are validated against existing learnings (FK constraint + existence check)

### Public Contracts

#### New Interfaces

| Module/Function | Signature | Returns (success) | Returns (error) | Side Effects |
|---|---|---|---|---|
| `learnings::recall::recall_learnings_scored` | `(conn: &Connection, params: RecallParams, backend: &dyn RetrievalBackend) -> TaskMgrResult<ScoredRecallResult>` | `ScoredRecallResult { scored_learnings, count, query, for_task, ... }` | `TaskMgrError` | None (retrieval only) |

#### Modified Interfaces

| Module/Function | Current Signature | Proposed Signature | Breaking? | Migration |
|---|---|---|---|---|
| `commands::recall::LearningSummary` | `{ id, title, outcome, confidence, content, applies_to_files, applies_to_task_types, times_shown, times_applied }` | Adds `relevance_score: f64, ucb_score: Option<f64>, combined_score: f64, match_reason: Option<String>` | No (additive) | N/A |
| `commands::learn::LearnParams` | No `supersedes` field | Adds `supersedes: Option<i64>` | No (additive) | N/A |
| `learnings::crud::types::EditLearningParams` | No `supersedes` field | Adds `supersedes: Option<i64>` | No (additive) | N/A |
| `learnings::retrieval::RetrievalQuery` | No `include_superseded` field | Adds `include_superseded: bool` (default false) | No (Default impl provides false) | N/A |

### Data Flow Contracts

| Data Path | Key Types at Each Level | Copy-Pasteable Access Pattern |
|---|---|---|
| Backend → CompositeBackend → recall_learnings_scored → RecallCmdResult | `ScoredLearning { relevance_score: f64, match_reason: Option<String> }` → stays as `ScoredLearning` through composite merge → mapped to `ScoredLearningOutput { relevance_score, ucb_score, combined_score, match_reason }` → mapped to `LearningSummary { relevance_score, ucb_score, combined_score, match_reason }` | `let scored = backend.retrieve(conn, &query)?; // Vec<ScoredLearning>` → `result.scored_learnings[0].relevance_score` → `json_output.learnings[0].relevance_score` |
| Supersession check in retrieval SQL | `learning_supersessions.old_learning_id: INTEGER` joined against `learnings.id: INTEGER` | `WHERE l.id NOT IN (SELECT old_learning_id FROM learning_supersessions)` — added to each retrieval SQL WHERE clause |

### Consumers of Changed Behavior

| File:Line | Usage | Impact | Mitigation |
|---|---|---|---|
| `src/commands/next/mod.rs:320` | Calls `recall_learnings()` → `RecallResult` | OK — unchanged function | N/A |
| `src/loop_engine/prompt.rs` (various) | Calls `recall_learnings()` → formats learnings | OK — unchanged function | N/A |
| `src/commands/recall.rs:117` | Calls `recall_learnings_with_backend()` | CHANGES — switches to `recall_learnings_scored()` | This is the intended change point |

### Inversion Checklist

- [x] All callers of `recall_learnings` / `recall_learnings_with_backend` identified (next, loop engine, recall command)
- [x] Only the recall command switches to the scored variant; others remain unchanged
- [x] Supersession filter must be added to all 4 retrieval SQL locations (fts5: 3 functions, patterns: 1 function) + vector backend post-filter
- [x] Tests that validate current recall behavior exist in `src/learnings/recall/tests.rs` and `src/commands/recall.rs`

### Documentation

| Doc | Action | Description |
|---|---|---|
| `CLAUDE.md` | Update | Add note about supersession table and `--supersedes` flag |

---

## 7. Open Questions

- [ ] Should `learnings list --format json` also include supersession metadata (superseded_by / supersedes IDs)? (Leaning yes but may be phase 2)
- [ ] Should `--include-superseded` also be available on `learnings list`?

---

## Appendix

### Related Learnings from Institutional Memory

- **#833**: Don't add migration columns to base schema CREATE TABLE definitions
- **#835**: Base schema is frozen at v0 — new tables go in migrations only
- **#1397**: Migration file structure pattern for task-mgr
- **#348**: SQLite down migrations leave columns, only revert version number (N/A for new table — DROP TABLE is fine)
- **#1467**: Uniform (score, reason) tuples for multi-dimension scoring
- **#273**: Long scoring/matching functions should be decomposed into per-dimension helpers

### Glossary

- **Supersession**: A relationship where a newer learning replaces an older one
- **UCB**: Upper Confidence Bound — a bandit algorithm balancing exploitation (showing proven learnings) and exploration (trying less-shown ones)
- **BM25**: A probabilistic ranking function used by SQLite FTS5 for text relevance scoring
- **Combined score**: `relevance_score * 100.0 + ucb_score` — the final ranking key in task-based recall
