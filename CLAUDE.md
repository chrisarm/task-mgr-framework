# task-mgr Project Notes

## Database Location

The Ralph loop database is at `.task-mgr/tasks.db` (relative to the project/worktree root). Each worktree has its own copy.

## Worktrees

- Main: `$HOME/projects/task-mgr`
- Feature worktrees: `$HOME/projects/task-mgr-worktrees/<branch-name>/`

## Task Files

- PRD task lists: `.task-mgr/tasks/<prd-name>.json`
- Loop prompts: `.task-mgr/tasks/<prd-name>-prompt.md`
- Progress log: `.task-mgr/tasks/progress.txt`

## Model IDs and Effort Mapping

All Claude model IDs and the difficulty→effort mapping live in a single file:
`src/loop_engine/model.rs` (`OPUS_MODEL` / `SONNET_MODEL` / `HAIKU_MODEL` constants
and the `EFFORT_FOR_DIFFICULTY` table). After bumping a value there:

```sh
cargo run --bin gen-docs   # regenerates the MODELS block in .claude/commands/tasks.md
```

CI runs `cargo run --bin gen-docs -- --check` which fails if the doc is stale.
Tests import the constants; JSON fixtures use `{{OPUS_MODEL}}` placeholders in
`tests/fixtures/*.json.tmpl` rendered at load time by
`tests/common/mod.rs::render_fixture_tmpl`. A regression test
(`tests/no_hardcoded_models.rs`) ensures literal model strings don't creep back
in outside `model.rs`.

## `task-mgr models` subcommand

List and pin Claude models:

```sh
task-mgr models list                     # offline — built-in model IDs + effort table
task-mgr models list --remote            # live /v1/models (requires both env vars below)
task-mgr models list --refresh           # busts cache before fetch; implies --remote
task-mgr models set-default [<model>]    # prompts interactively when model omitted
task-mgr models set-default <id> --project   # writes .task-mgr/config.json instead
task-mgr models unset-default [--project]
task-mgr models show                     # resolved default + source label
```

**Remote opt-in** (prevents surprise HTTP calls on a globally-exported SDK key):

- `ANTHROPIC_API_KEY` — your Anthropic API key
- `TASK_MGR_USE_API=1` — explicit opt-in; both must be set or we silently fall
  back to the built-in list

Cache: `$XDG_CACHE_HOME/task-mgr/models-cache.json` (24h TTL, stale treated as miss).

**Config locations & precedence** (highest to lowest): explicit task `model` →
`difficulty==high` → PRD `defaultModel` → `.task-mgr/config.json defaultModel`
→ `$XDG_CONFIG_HOME/task-mgr/config.json defaultModel` → none.
`difficulty==high` always escalates to `OPUS_MODEL`, independent of any
default.

The interactive picker fires from `task-mgr init` when nothing resolves and
stdin+stderr are both TTYs. Non-TTY / auto-mode runs print a one-line stderr
hint and skip — no hang.

## Loop CLI Cheat Sheet

- **Add a task**: `echo '{"id":"X-FIX-001","title":"...","difficulty":"medium","touchesFiles":[]}' | task-mgr add --stdin`
- **Link into milestone**: append `--depended-on-by MILESTONE-ID`
- **Mark status**: emit `<task-status>TASK-ID:done</task-status>` (also: `failed`, `skipped`, `irrelevant`, `blocked`)
- **Permission guard**: loop iterations deny Edit/Write on `tasks/*.json` via `--disallowedTools`
- **Never edit** `.task-mgr/tasks/*.json` directly — use the CLI and tags above

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

## Embedding / Ollama Configuration

`curate embed` generates local embeddings via Ollama for the dedup pre-filter. Configure in `.task-mgr/config.json`:

```json
{
  "ollamaUrl": "http://localhost:11434",
  "embeddingModel": "hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0"
}
```

- **Default URL**: `http://localhost:11434`
- **Default model**: `hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0` (1024 dimensions)
- **Schema**: Migration v15 adds `learning_embeddings` table (BLOB storage, little-endian f32)

### Graceful Degradation

- `curate dedup` works without Ollama — falls back to standard batch sizing when no embeddings exist
- `curate embed --status` only queries the DB (no Ollama connection needed)
- `curate embed` returns a clear error if Ollama is unreachable or the model is missing

## Dedup Dismissal Memory

`curate dedup` persists pairs the LLM has already examined and found distinct in the
`dedup_dismissals` table (migration v18: composite PK `(id_lo, id_hi)` **plus
`CHECK (id_lo < id_hi)`** for defense-in-depth, plus `idx_dedup_dismissals_hi`).
Subsequent runs skip batches whose every C(N,2) pair is already dismissed, so
users don't re-pay LLM calls for the same "no duplicates" output.

- **Pair normalization**: `normalize_pair()` canonicalizes `(a, b)` to `(min, max)`;
  all writes go through `record_dismissals()`. The v18 CHECK constraint backstops
  this at the schema level — a self-pair or reversed pair that slipped past Rust
  normalization fails at INSERT time rather than silently corrupting the table.
- **Narrow conflict suppression**: `record_dismissals` uses
  `ON CONFLICT (id_lo, id_hi) DO NOTHING`, **not** `INSERT OR IGNORE`. This keeps
  duplicates idempotent while letting CHECK (or any future NOT NULL / FK) failures
  propagate as real errors instead of being swallowed.
- **Multi-row INSERT**: `record_dismissals` emits a single
  `INSERT ... VALUES (?,?),(?,?),...` per chunk of 256 pairs (512 params,
  well under `SQLITE_MAX_VARIABLE_NUMBER`). One round-trip per chunk, not per pair.
- **When dismissals are recorded**: after a successful LLM batch, every C(N,2) pair
  from the batch minus (a) pairs the LLM grouped as duplicates and (b) pairs whose
  IDs were retired by a strictly earlier batch.
- **Merge-map rewrite**: when the batch itself merges sources `{A,B}→N`, recorded
  pairs are rewritten via a per-batch `merge_map` so retired source IDs become the
  surviving merged ID. `(A,C)+(B,C)` collapse to `(N,C)`; two clusters in one batch
  `{A,B}→N1, {C,D}→N2` collapse the four cross-pairs to a single `(N1,N2)`. Without
  this rewrite the dismissals would point at retired (inert) rows and the next run
  would re-call the LLM on `(N, survivor)` pairs the LLM has effectively already
  judged. Logic lives in `compute_dismissal_pairs()` in `src/commands/curate/mod.rs`.
- **When they are NOT recorded**: `dry_run=true` (read-only convention) OR the batch
  raised an LLM error (can't trust a batch whose result we never got). The
  `continue` in the LLM error arm short-circuits before any dismissal accounting.
- **Forcing re-examination**: `task-mgr curate dedup --reset-dismissals` clears the
  table (`clear_dismissals()`) before the run; applies even with `--dry-run` because
  a reset is an administrative action, not an LLM pass.
- **`DedupResult.clusters_skipped`**: serde `default = 0` so JSON consumers parsing
  older output still work; new runs populate it with the count of batches skipped.
- Table has no foreign keys to `learnings` — rows for retired learnings are inert
  and harmless (they just never match an active cluster).

Helpers live in `src/commands/curate/mod.rs` as `pub(crate)` (not exported outside
the crate): `load_dismissals`, `record_dismissals`, `clear_dismissals`,
`is_fully_dismissed`, `compute_dismissal_pairs`, plus the private `normalize_pair`
/ `unordered_pairs`.

## Curate session cleanup workaround

Claude Code 2.1.110 writes an `ai-title` jsonl to `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl`
even with `--no-session-persistence`. To avoid polluting the user's projects dir, `curate dedup`
and `curate enrich` opt into `spawn_claude`'s `cleanup_title_artifact` arg: a fixed UUID is
passed via `--session-id` (before `-p`, required — Claude parses flags only left of the prompt)
and, after `child.wait()` returns, that exact file is removed synchronously. An earlier detached
30s-delay thread design was replaced because threads die when the parent `task-mgr` process
exits; synchronous post-wait cleanup is both simpler and guaranteed to run. Scope is narrow —
loops and learning ingestion do NOT opt in; only the curate call sites do. See `spawn_claude`
and `cleanup_title_artifact_sync` in `src/loop_engine/claude.rs`.
