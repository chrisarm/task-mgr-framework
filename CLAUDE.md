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
