# Plan: Improve Task-Related Learning Recall

## Problem Statement

306 learnings imported but recall effectiveness is limited:
- `applies_to_files`: 110/306 (36%) populated
- `applies_to_task_types`: **0/306 (0%)** — task-type matching completely broken
- `applies_to_errors`: **0/306 (0%)** — error matching completely unused
- FTS5 indexes only `title` + `content` — tags not searchable via text query

When the loop runs `recall --for-task FEAT-003`, the PatternsBackend can only match 36% of learnings by file, 0% by task type, and 0% by error pattern. The remaining 64% are invisible to task-based recall.

## Two Tracks (Parallel)

### Track A: Data Backfill (immediate, no task-mgr changes)

**A1. Backfill `applies_to_task_types` from tags**

The imported learnings have branch-derived tags like `embedding-routing`, `pto-start-time`, `ses-email-enhancements`. These map to task type prefixes used in PRDs:

| Tag pattern | Task type prefix |
|---|---|
| `embedding-*` | `FEAT-` |
| `pto-*` | `FEAT-`, `FIX-` |
| `ses-*` | `FEAT-` |
| `workflow-*` | `FEAT-`, `FIX-` |
| `terminal-*` | `FEAT-` |

Write a SQL migration script that populates `applies_to_task_types` based on tag patterns and outcome:
- `failure` / `workaround` → also add `FIX-`
- `pattern` / `success` → add `FEAT-`

**A2. Backfill `applies_to_files` from content**

24 learnings mention modules like `engine`, `matcher`, `workflow_handler` in their content but have no `applies_to_files`. Write a SQL script that:
- Scans `content` for known module names → maps to file globs
- E.g., `content LIKE '%WorkflowEngine%'` → `applies_to_files = '["service/src/agent/workflow/engine.rs"]'`

Map table:
| Content keyword | File pattern |
|---|---|
| `WorkflowEngine`, `workflow engine` | `service/src/agent/workflow/engine.rs` |
| `WorkflowMatcher`, `matcher` | `service/src/agent/workflow/matcher.rs` |
| `workflow_handler` | `service/src/agent/consumer/workflow_handler.rs` |
| `OwnershipManager`, `ownership` | `common/src/redis/ownership.rs` |
| `rate_limit` | `service/src/tools/rate_limit.rs` |
| `SendEmailTool`, `SES`, `email` | `service/src/tools/definitions/ses/*` |
| `embedder`, `CachedEmbedder` | `service/src/agent/kb/cached_embedder.rs` |
| `pay_period` | `service/src/tools/definitions/date/pay_period.rs` |
| `retriever` | `service/src/agent/kb/retriever.rs` |

**A3. Enrich import parser for future imports**

Update `parse_learnings.py` to:
- Auto-derive `applies_to_task_types` from branch names (feat/ → FEAT-, fix/ → FIX-)
- Map long-term category headers to task types
- Ensure file extraction catches module-in-sentence patterns

### Track B: task-mgr Code Improvements (Rust changes in startat0/task-mgr)

**B1. Auto-populate applicability on `learn` command** (HIGH IMPACT)

When `task-mgr learn --task-id FEAT-003` is called, auto-populate:
- `applies_to_files` from the task's `touchesFiles` (already in DB)
- `applies_to_task_types` from the task ID prefix (extracted via `extract_task_prefix()`)

This is the highest-leverage change: every future learning automatically gets metadata without the agent needing to specify `--files` and `--task-types`.

**Changes:**
- `src/commands/learn.rs`: After recording the learning, if `task_id` is set and `files`/`task_types` are empty, auto-fill from task context
- Requires: `resolve_task_context()` from `retrieval/patterns.rs` (already exists)

**B2. Auto-populate applicability in LLM extraction** (HIGH IMPACT)

`extract_learnings_from_output()` already receives `task_id`. After parsing the LLM response, if `applies_to_files` or `applies_to_task_types` are empty on extracted learnings, fill from task context (same pattern as B1).

**Changes:**
- `src/learnings/ingestion/mod.rs`: After `parse_extraction_response()`, enrich each `RecordLearningParams` with task context
- Use `resolve_task_context()` to get files and prefix

**B3. Tag-aware retrieval in PatternsBackend** (MEDIUM IMPACT)

Add a new matching dimension: tags from the learning are compared against the task's file paths to find semantic matches.

Map tags to file path prefixes:
- Tags containing "workflow" → match tasks with files under `service/src/agent/workflow/`
- Tags containing "ses" or "email" → match tasks with `service/src/tools/definitions/ses/`
- Tags containing "pto" → match tasks with `service/src/tools/definitions/date/`

New scoring constant: `TAG_CONTEXT_MATCH_SCORE = 3` (lower than file match, higher than error match).

**Changes:**
- `src/learnings/retrieval/patterns.rs`:
  - Load tags for candidates (batch_get_learning_tags already exists)
  - Add tag-to-path mapping and scoring
  - New reason: "tag context match"

**B4. Include tags in FTS5 index** (LOW IMPACT, OPTIONAL)

Currently FTS5 only indexes `title` + `content`. Adding tags would let `recall --query "chrono"` also find learnings tagged with "chrono-date-handling" even if the word "chrono" doesn't appear in title/content.

**Changes:**
- New migration: `ALTER TABLE learnings_fts` to add `tags` column (FTS5 content sync)
- Update triggers to join tags into a space-separated string for FTS5
- This is a schema migration (v8) — must be backward compatible

## Implementation Order

1. **A1 + A2** — SQL backfill scripts (immediate, ~30 min)
2. **B1** — Auto-populate on `learn` (highest leverage for future learnings)
3. **B2** — Auto-populate in extraction (same pattern, quick follow-up)
4. **A3** — Parser improvements (for next bulk import)
5. **B3** — Tag-aware retrieval (medium impact, more complex)
6. **B4** — FTS5 tags (optional, schema migration)

## Expected Impact

| Metric | Before | After A1+A2 | After A1+A2+B1+B2 |
|---|---|---|---|
| `applies_to_files` populated | 36% | ~55% | 55% existing + 100% future |
| `applies_to_task_types` populated | 0% | ~60% | 60% existing + 100% future |
| `applies_to_errors` populated | 0% | 0% | 0% (not addressed) |
| Task-based recall effectiveness | ~36% | ~65% | ~65% existing, 100% future |

## Failure Conditions

- **SQL backfill could set wrong file patterns**: Use conservative keyword matching, review output before committing
- **Auto-populate could create overly broad patterns**: Limit to exact task files, not globs
- **Tag-to-path mapping could create false positives**: Use moderate score (3) so it doesn't override stronger signals
- **FTS5 migration could break existing DB**: Use IF NOT EXISTS, test with copy of production DB first

## Scope Boundaries

- NOT adding semantic/embedding search (too complex, separate effort)
- NOT changing UCB bandit algorithm (working correctly)
- NOT modifying the `extract_learnings_from_output` LLM prompt to request better metadata (fragile)
- NOT backfilling `applies_to_errors` (no error data available in imported learnings)
