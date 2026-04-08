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
