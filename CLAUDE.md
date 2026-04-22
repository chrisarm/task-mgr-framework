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
