# src/commands/curate — design notes

Subsystem narrative for the `task-mgr curate` command family (dedup, enrich,
retire). Cross-cutting config and persistence contracts live here; per-fn
contracts live in rustdoc. File-scoped don't-do-this rules (e.g. the
`normalize_pair` CHECK constraint, narrow ON CONFLICT) are migrated to
`task-mgr learn` so they surface via `recall --for-task` when working in
this directory.

## Embedding / Ollama Configuration

`curate embed` generates local embeddings via Ollama for the dedup pre-filter. Configure in `.task-mgr/config.json`:

```json
{
  "ollamaUrl": "http://localhost:11435",
  "embeddingModel": "hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0"
}
```

- **Default URL**: `http://localhost:11435` (the bundled docker-compose stack
  remaps to 11435 to avoid clashing with a host-installed `ollama serve` on the
  upstream-default 11434)
- **Default model**: `hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0` (1024 dimensions)
- **Schema**: Migration v15 adds `learning_embeddings` table (BLOB storage, little-endian f32)

### Graceful Degradation

- `curate dedup` works without Ollama — falls back to standard batch sizing when no embeddings exist
- `curate embed --status` only queries the DB (no Ollama connection needed)
- `curate embed` returns a clear error if Ollama is unreachable or the model is missing
- `recall --query <text>` HARD-FAILS by default if Ollama is unreachable. Pass
  `--allow-degraded` to fall back to silently-empty vector results (useful for
  offline runs). `recall --for-task <id>` (no `--query`) does not need Ollama.

### Reranker (optional)

The recall pipeline can layer a cross-encoder reranker on top of the per-backend
union slate. Reranking only fires for `recall --query <text>` (with or without
`--for-task`); `--for-task` alone runs the today's UCB-only pipeline.

Configure in `.task-mgr/config.json`:

```json
{
  "rerankerUrl": "http://localhost:8181",
  "rerankerModel": "jina-reranker-v2-base-multilingual",
  "rerankerOverFetch": 3
}
```

- **`rerankerUrl`** — base URL of a [gpustack/llama-box](https://github.com/gpustack/llama-box)
  server exposing OpenAI-compatible `/v1/rerank`. Reranker is disabled when unset.
  Project default is host port **8181** (one off from llama-box's internal 8080
  to avoid clashing with other projects that commonly publish on 8080); the
  bundled docker-compose stack remaps `8181:8080` for this reason.
- **`rerankerModel`** — model name passed in the `model` field of the rerank
  request. Required alongside `rerankerUrl`; either-or disables rerank.
- **`rerankerOverFetch`** — per-backend over-fetch factor. Slate size is
  `min(limit * over_fetch, 30)`. Default `3`. Higher = better recall headroom,
  longer rerank latency.
- **Example llama-box invocation** (CPU, host-native — bind to 8181 to match
  the project default; if you run the bundled docker-compose stack instead,
  the container's internal 8080 is remapped to host 8181 automatically):

  ```sh
  llama-box --rerank-only --port 8181 \
      --model /models/jina-reranker-v2-base-multilingual.gguf
  ```

  See `docker/docker-compose.yml` for a full Docker setup that bundles Ollama
  embeddings + llama-box rerank, GPU-by-default with a `--profile cpu` fallback.

#### Soft-fail asymmetry

The reranker is a quality booster, not a correctness primitive: when the
server is unreachable, recall emits a `[warn]` line to stderr and returns the
un-reranked candidates with their original BM25/cosine/pattern scores. Recall
still exits `0`. Contrast with Ollama, which by default hard-fails because the
vector backend is part of the recall result, not just an ordering heuristic.

#### `--query "X" --for-task Y` interaction

When both are set:
1. Per-backend top-N union slate is fetched (FEAT-003's `retrieve_for_rerank`).
2. Cross-encoder reranks the slate by `(query, candidate)` similarity.
3. UCB tiebreaks within ±0.05 rerank-score bands (same band → higher UCB wins).
4. Slate is truncated to `--limit`.

`--for-task` alone (no `--query`) skips steps 1-3 entirely; the reranker is
NOT consulted.

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
