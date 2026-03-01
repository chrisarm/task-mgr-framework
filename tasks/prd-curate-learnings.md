# PRD: Learning Curation System

**Type**: Feature
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-02-26
**Status**: Draft

---

## 1. Overview

### Problem Statement

task-mgr's institutional memory system accumulates learnings over time but has no way to maintain quality. After ~306 learnings, the corpus suffers from:

- **Duplicates**: Semantically identical learnings recorded by different runs (insert-time dedup only catches exact outcome+title matches)
- **Stale entries**: Old, low-confidence, or never-applied learnings dilute recall quality and waste context window budget
- **Sparse metadata**: Only 36% of learnings have `applies_to_files`; 0% have `applies_to_task_types` or `applies_to_errors` — crippling pattern-based retrieval

Without curation, recall degrades as the corpus grows: more noise, less signal.

### Background

The learning system has a sophisticated retrieval pipeline (FTS5 + pattern matching + UCB bandit ranking) but its effectiveness is bottlenecked by data quality. The existing design doc `docs/designs/P1-improve-learning-recall-metadata.md` acknowledges the metadata sparsity problem but proposes only forward-looking fixes. This PRD addresses retroactive curation of the existing corpus and ongoing maintenance.

---

## 2. Goals

### Primary Goals

- [ ] Soft-archive stale/low-value learnings so they stop appearing in recall
- [ ] Backfill missing metadata (`applies_to_files`, `applies_to_task_types`, `applies_to_errors`, tags) on existing learnings using LLM analysis
- [ ] Identify and merge semantically duplicate learnings, preserving the best information from each

### Success Metrics

- Retirement: reduce active learning count by removing provably stale entries (never applied after sufficient exposure)
- Enrichment: increase `applies_to_files` population from 36% to >80%, populate `applies_to_task_types` and `applies_to_errors` from 0% to >50%
- Dedup: eliminate duplicate clusters, reducing total active count while preserving information

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Soft-archived learnings must be excluded from ALL recall/retrieval paths (12+ queries)
- Dedup merges must preserve the union of metadata and tags from all source learnings
- Bandit stats (times_shown, times_applied) must be summed when merging, not lost
- Enrichment must never overwrite existing metadata — only fill NULL fields

### Performance Requirements

- Best effort for LLM operations — no hard latency targets
- Print batch progress (e.g., "Processing batch 2/15...")
- Per-batch transaction commits so completed work survives interruption
- Re-running after interruption must naturally resume where it left off (idempotent queries)

### Style Requirements

- Follow existing CLI patterns exactly (clap derive, Params/Result structs, TextFormattable, output_result)
- Follow existing LLM integration patterns (random delimiter injection protection, best-effort JSON parsing, graceful degradation)
- Reuse existing CRUD functions (`record_learning`, `edit_learning`, `delete_learning`) rather than raw SQL

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|-----------|---------------|-------------------|
| 0 learnings need curation | Wasted LLM call on empty input | Short-circuit, return empty result, no LLM invocation |
| LLM returns non-JSON / garbage | Best-effort parsing must not crash | Log warning, increment `llm_errors` counter, continue to next batch |
| LLM references non-existent learning IDs | Hallucinated IDs in dedup clusters | Validate all IDs against DB before acting; skip invalid clusters |
| Same learning in multiple dedup clusters | LLM assigns one learning to two merge groups | Process clusters in order; skip learnings already merged |
| Learning with `retired_at` set appears in recall | Filter missed in one of 12+ queries | Comprehensive test that creates retired learning and verifies exclusion from every retrieval path |
| Process killed between batches | User hits Ctrl+C or crash | Per-batch transactions ensure completed batches are durable; re-run processes remaining |
| Process killed mid-batch | Kill during LLM call or DB writes | Transaction rolls back; no partial state. LLM work lost but DB consistent. |
| Enrichment on already-enriched learning | Re-run after partial completion | Query only learnings with NULL metadata fields; already-enriched are skipped naturally |
| Dedup threshold too aggressive | Merges unrelated learnings | Originals are soft-archived (not deleted), recoverable via `unretire`. Default threshold conservative (0.7). |
| `claude` binary not found | No Claude CLI installed | Propagate `IoError` with context; user sees clear error message |

---

## 3. User Stories

### US-001: Retire stale learnings
**As a** task-mgr user
**I want** to identify and soft-archive learnings that are old, low-confidence, or never useful
**So that** recall quality improves by removing noise from the active corpus

**Acceptance Criteria:**
- [ ] `curate retire --dry-run` shows candidates with reasons without modifying DB
- [ ] `curate retire` soft-archives candidates (sets `retired_at` timestamp)
- [ ] Retired learnings are excluded from all recall, retrieval, list, and bandit queries
- [ ] `curate unretire <id>` restores a learning to active status
- [ ] `learnings` list shows active count vs total count
- [ ] No LLM calls required — purely SQL-based

### US-002: Enrich learning metadata
**As a** task-mgr user
**I want** to backfill missing `applies_to_files`, `applies_to_task_types`, `applies_to_errors`, and tags on existing learnings
**So that** pattern-based retrieval becomes effective for the existing corpus

**Acceptance Criteria:**
- [ ] `curate enrich --dry-run` shows proposed metadata additions
- [ ] `curate enrich` updates learnings via LLM-inferred metadata
- [ ] Only NULL/empty fields are populated — existing metadata is never overwritten
- [ ] `--field <name>` filters to learnings missing a specific field
- [ ] Batch progress is printed (e.g., "Processing batch 2/15...")
- [ ] Interrupted runs can be re-run to process remaining learnings

### US-003: Deduplicate and merge learnings
**As a** task-mgr user
**I want** to find semantically similar learnings and merge them into consolidated entries
**So that** the corpus is concise and doesn't waste context window budget on redundant information

**Acceptance Criteria:**
- [ ] `curate dedup --dry-run` shows merge clusters with proposed consolidated content
- [ ] `curate dedup` creates merged learnings and soft-archives originals
- [ ] Merged learning inherits union of metadata, tags from all sources
- [ ] Bandit stats are summed (times_shown, times_applied), window stats reset
- [ ] Highest confidence from cluster is used for merged learning
- [ ] Interrupted runs can be re-run safely

---

## 4. Functional Requirements

### FR-001: Schema migration — `retired_at` column
Add `retired_at TEXT` column to the `learnings` table via migration v8. NULL means active; non-NULL datetime means soft-archived.

**Details:**
- Migration adds the column with `DEFAULT NULL`
- All 12+ retrieval queries add `AND retired_at IS NULL` filter
- `get_learning()` by ID does NOT filter by retired_at (needed for `unretire`, `show` commands)
- Count queries (`SELECT COUNT(*)`) and aggregate queries (`SUM(window_shown)`) must also filter

**Validation:**
- Integration test creates retired learning, verifies it's excluded from every retrieval path

### FR-002: `curate retire` subcommand
Identify and soft-archive stale learnings based on configurable thresholds.

**Details:**
- Default criteria (any match triggers retirement candidacy):
  1. Age >= 90 days AND confidence = 'low' AND times_applied = 0
  2. times_shown >= 10 AND times_applied = 0 (shown enough, never useful)
  3. times_shown >= 20 AND (times_applied / times_shown) < 0.05 (very low application rate)
- All thresholds configurable via CLI flags
- `--dry-run` shows candidates with reasons; no DB changes
- Without `--dry-run`: sets `retired_at = datetime('now')` in a single transaction

### FR-003: `curate unretire` subcommand
Restore a soft-archived learning to active status.

**Details:**
- Takes one or more learning IDs
- Sets `retired_at = NULL`
- Validates learning exists and is currently retired

### FR-004: `curate enrich` subcommand
Backfill missing metadata on active learnings using LLM analysis.

**Details:**
- Query active learnings where any applicability field is NULL (or filtered by `--field`)
- Batch into groups of N (default 20)
- For each batch: build prompt with learning content, call Claude, parse JSON response
- Apply proposed metadata via `edit_learning()` (only for NULL fields)
- Each batch committed in its own transaction for resume safety
- Print progress per batch

### FR-005: `curate dedup` subcommand
Find and merge semantically duplicate learnings using LLM analysis.

**Details:**
- Load all active learnings
- If corpus fits in context (~150K chars), send all at once; otherwise batch
- LLM identifies clusters of duplicates with proposed merged content
- For each cluster:
  - Create new merged learning (union metadata, summed stats, highest confidence)
  - Soft-archive all originals (set `retired_at`)
  - Single transaction per cluster
- `--threshold` (default 0.7) controls merge aggressiveness

### FR-006: Extend `EditLearningParams`
Add `add_task_types`, `remove_task_types`, `add_errors`, `remove_errors` fields to `EditLearningParams`. This is a prerequisite for enrich to update these fields through the existing CRUD layer.

---

## 5. Non-Goals (Out of Scope)

- **Embedding-based semantic search** — LLM prompt comparison is sufficient for curation batch sizes; embeddings are a future optimization
- **Automatic scheduled curation** — this is a manual CLI command, not a background job
- **Learning versioning/history** — soft archive is the only recovery mechanism; no full audit trail
- **Cross-database learning sharing** — curation operates on a single local SQLite database

---

## 6. Technical Considerations

### Affected Components

| Component | What Changes |
|-----------|-------------|
| `src/db/migrations/v8.rs` | New migration: add `retired_at` column |
| `src/db/migrations/mod.rs` | Register v8, bump `CURRENT_SCHEMA_VERSION` to 8 |
| `src/learnings/retrieval/fts5.rs` | Add `retired_at IS NULL` to 3 queries |
| `src/learnings/retrieval/patterns.rs` | Add `retired_at IS NULL` to 1 query |
| `src/learnings/recall/mod.rs` | Add `retired_at IS NULL` to 2 queries |
| `src/learnings/bandit.rs` | Add `retired_at IS NULL` to 2 queries |
| `src/learnings/ingestion/mod.rs` | Add `retired_at IS NULL` to dedup check |
| `src/commands/learnings.rs` | Add `retired_at IS NULL` to list/count queries |
| `src/learnings/crud/types.rs` | Extend `EditLearningParams` with task_types/errors fields |
| `src/learnings/crud/update.rs` | Handle new fields (copy files pattern) |
| `src/cli/commands.rs` | Add `CurateAction` enum, `Curate` variant, extend `EditLearning` flags |
| `src/commands/curate/` | New module: mod, types, retire, enrich, dedup, prompts, output, tests |
| `src/commands/mod.rs` | Add curate module + re-exports |
| `src/handlers.rs` | Add `impl_text_formattable!` for result types |
| `src/main.rs` | Add `Commands::Curate` dispatch, extend `EditLearning` dispatch |

### Dependencies

- **Claude CLI** — required for `enrich` and `dedup` subcommands (via `spawn_claude()`)
- **rusqlite** — already a dependency; no new crates needed

### Approaches & Tradeoffs

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| **Hard delete retired** | Simpler (no query changes), no migration | Irreversible; dedup merges destructive; no recovery | Rejected |
| **Soft archive with `retired_at` column** | Recoverable; dedup originals preserved; `unretire` possible | 12+ queries need filtering; migration required | **Preferred** |
| **Separate archive table** | Clean separation of active/archived | More complex migration; CRUD needs duplication; harder to unretire | Rejected |

**Selected Approach**: Soft archive with `retired_at` column. The query filter changes are mechanical and testable. Recovery from bad dedup merges justifies the extra effort.

### Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Retired learning leaks into recall (missed filter in 1 of 12+ queries) | Medium — stale content in recall | Medium — many touch points | Comprehensive integration test that verifies exclusion from every retrieval function |
| LLM produces bad dedup merge (combines unrelated learnings) | Medium — information muddled, but recoverable | Low — conservative default threshold | Soft-archive originals (not delete); `--dry-run` first; `unretire` for recovery |
| Interrupted LLM call loses expensive API work | Low — costs money/time but no data corruption | Medium — long-running LLM calls | Per-batch commits; re-run processes remaining items naturally |

### Security Considerations

- LLM prompts use random UUID delimiters around untrusted learning content (existing injection protection pattern from `extraction.rs`)
- No secrets or credentials in learning content (learnings are code patterns, not credentials)
- `spawn_claude` runs with `--dangerously-skip-permissions` (existing pattern for non-interactive use)

### Public Contracts

#### New Interfaces

| Module/Endpoint | Signature | Returns (success) | Returns (error) | Side Effects |
|----------------|-----------|-------------------|-----------------|-------------|
| `curate::retire` | `curate_retire(conn, dry_run, min_age_days, min_shows, max_rate)` | `RetireResult` | `TaskMgrError` | Sets `retired_at` on matching learnings |
| `curate::unretire` | `curate_unretire(conn, learning_ids)` | `UnretireResult` | `TaskMgrError` | Clears `retired_at` on specified learnings |
| `curate::enrich` | `curate_enrich(conn, dry_run, batch_size, field_filter)` | `EnrichResult` | `TaskMgrError` | Updates metadata fields via `edit_learning()` |
| `curate::dedup` | `curate_dedup(conn, dry_run, batch_size, threshold)` | `DedupResult` | `TaskMgrError` | Creates merged learnings, soft-archives originals |

#### Modified Interfaces

| Module/Endpoint | Current Signature | Proposed Signature | Breaking? | Migration |
|----------------|-------------------|-------------------|-----------|-----------|
| `crud::types::EditLearningParams` | 9 optional fields | 13 optional fields (+task_types, +errors) | No | Additive — `Default` still works |
| `crud::update::edit_learning` | Handles 9 fields | Handles 13 fields | No | New fields are `Option`, ignored if `None` |
| All retrieval queries | No `retired_at` filter | Add `AND retired_at IS NULL` | No | Behavioral — retired learnings now hidden |

### Inversion Checklist

- [x] All callers of retrieval queries identified (12+ locations documented)
- [x] Routing/branching decisions that depend on learning count reviewed (UCB total_window_shows, list count)
- [x] Tests that validate current behavior identified (retrieval tests, CRUD tests, learnings list tests)
- [x] Different semantic contexts for same code discovered (get_learning by ID should NOT filter — needed for unretire/show)

---

## 7. Open Questions

- [ ] Should `learnings` list command show retired learnings with a flag (`--include-retired`) or completely hide them?
- [ ] Should the dedup threshold be semantic (passed to LLM as guidance) or should we compute actual similarity scores?
- [ ] For the `curate enrich` LLM prompt: should we provide examples of good metadata from well-tagged learnings as few-shot examples?

---

## 8. Phasing

This feature is implemented in three phases, each independently shippable:

| Phase | Subcommand | LLM Required | Depends On |
|-------|-----------|-------------|-----------|
| 1 | `curate retire` + `curate unretire` + migration v8 | No | Nothing |
| 2 | `curate enrich` | Yes | Phase 1 (migration), `EditLearningParams` extension |
| 3 | `curate dedup` | Yes | Phase 1 (soft archive for originals) |

---

## Appendix

### Related Documents

- `docs/designs/P1-improve-learning-recall-metadata.md` — existing design for metadata improvement
- `src/learnings/ingestion/extraction.rs` — LLM prompt/parsing patterns to follow
- `src/loop_engine/claude.rs` — `spawn_claude()` interface

### Glossary

- **Soft archive**: Setting `retired_at` timestamp on a learning to exclude it from recall without permanent deletion
- **UCB bandit**: Upper Confidence Bound algorithm used to rank learnings for recall, balancing exploitation (proven useful) with exploration (new/untested)
- **Applicability metadata**: `applies_to_files`, `applies_to_task_types`, `applies_to_errors` — JSON arrays that enable context-aware retrieval
