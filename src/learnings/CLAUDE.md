# src/learnings — design notes

Subsystem narrative for the learnings system: creation chokepoint, supersession
join table, recall scoring, and the per-backend retrieval pipeline (FTS5,
patterns, vector). File-scoped invariants (e.g. `apply_supersession` ordering,
`SUPERSESSION_SUBQUERY` as single-source filter, vector-backend filters-in-Rust)
are migrated to `task-mgr learn` so they surface via `recall --for-task` when
working in this directory.

## Learning Creation Chokepoint

All production code paths that create learnings must go through `LearningWriter` in
`src/learnings/crud/writer.rs`. This ensures every new learning automatically gets an
Ollama embedding scheduled (best-effort, graceful degradation when Ollama is down).

**Pattern:**
1. Construct `LearningWriter::new(db_dir)` — pass `Some(path)` for embedding, `None` in tests.
2. Call `writer.record(conn, params)` (or `writer.push_existing(id, title, content)` for
   callers like `merge_cluster` that do their own `record_learning` inside a transaction).
3. Call `writer.flush(conn)` **after** any enclosing transaction has committed — this is
   where the Ollama HTTP call happens. Never flush inside a `rusqlite::Transaction`.

**Production paths using LearningWriter:**
- `learn()` in `src/commands/learn.rs`
- `import_learnings()` in `src/commands/import_learnings/mod.rs`
- `curate_dedup()` in `src/commands/curate/mod.rs` (via `push_existing` after `merge_cluster`)
- `extract_learnings_from_output()` in `src/learnings/ingestion/mod.rs` (loop engine path)

The low-level `record_learning()` primitive in `src/learnings/crud/create.rs` is still
public for tests and `curate enrich`, but new production creation paths should use
`LearningWriter` to get automatic embedding scheduling.

## Learning Supersession

When a newer learning replaces an older one, the link is tracked in the
`learning_supersessions` join table (migration v17). The old row is retained (for
audit / history) but auto-filtered from recall by default.

- **Create a supersession**: `task-mgr learn --supersedes <old-id> ...` or
  `task-mgr edit-learning <new-id> --supersedes <old-id>`. The old learning's
  confidence is downgraded to `low` and a row is inserted into
  `learning_supersessions(old_id, new_id, superseded_at)`.
- **Recall behavior**: `task-mgr recall` excludes superseded learnings by default.
  Pass `--include-superseded` to see them. Filtering happens in
  `retrieval/mod.rs::passes_query_filters()` via a shared SQL helper — all three
  backends (fts5, patterns, vector) honor the flag.
- **Listing**: `task-mgr learnings` annotates rows with `(superseded by #N)` and
  `(supersedes #M)`.
- See `task-mgr learn --help`, `task-mgr edit-learning --help`,
  `task-mgr recall --help` for flag details.

**Invariants for future maintainers:**

- **`apply_supersession` runs AFTER `LearningWriter::flush`** in `learn()` — the
  new learning's `id` is only known post-insert. In `edit_learning()` the id is
  known upfront so `apply_supersession` can run before/after other field edits;
  it runs after so typo'd `--supersedes` values don't roll back unrelated edits.
- **Single source for the filter SQL**: `pub(crate) const SUPERSESSION_SUBQUERY`
  in `src/learnings/retrieval/mod.rs` is the canonical `NOT IN (SELECT
  old_learning_id FROM learning_supersessions)` fragment. All retrieval call
  sites (`fts5::execute_fts5_query`, `fts5::execute_like_query`,
  `fts5::execute_unfiltered_query`, `patterns::load_learnings_with_applicability`,
  `recall::load_ucb_fallback`) must format this const into their WHERE clauses
  alongside — never replacing — the existing `retired_at IS NULL` filter.
- **Vector backend filters in Rust, not SQL**: `vector.rs` loads embeddings
  directly, so supersession is enforced via `load_superseded_ids()` +
  `HashSet::contains` after the retrieval. Keep the two paths in sync when
  changing filter semantics.
- **Tests that touch `learning_supersessions` need `setup_db_with_migrations()`**
  — the plain `setup_db()` calls `create_schema()` only, which stops at v0.

## Recall Score Output

`task-mgr --format json recall` returns numeric scores alongside the categorical
`confidence` field so consumers can parse signal strength:

- `relevance_score` — raw retrieval score (FTS5 BM25, pattern-match points, or
  vector cosine similarity, depending on backend)
- `ucb_score` — UCB1 bandit score (present on `--for-task` queries)
- `combined_score` — aggregated ranking score used for ordering
- `match_reason` — human-readable explanation (e.g. `"FTS5 text match"`,
  `"file pattern match, task type match"`)

The underlying `recall_learnings()` / `recall_learnings_with_backend()` signatures
are unchanged; scored output flows through `recall_learnings_scored()` and the
existing CLI formatters.
