# PRD: Fast Dedup via Embedding Pre-Filter + Parallel Batching

**Type**: Enhancement
**Priority**: P1 (High)
**Author**: Claude Code
**Created**: 2026-04-03
**Status**: Draft

---

## 1. Overview

### Problem Statement

`curate dedup` with ~1,200 active learnings creates ~60 sequential `spawn_claude` subprocess calls (20 learnings/batch). Each call takes 10-30s, making the full command take 10-30 minutes. Most LLM calls are wasted on learnings with no near-duplicates.

### Background

- The `curate dedup` flow loads all active learnings, chunks them into batches, and sends each batch to Claude for semantic duplicate detection. Batches are processed sequentially.
- A local Ollama instance with Jina embeddings can compute vector similarity cheaply, pre-filtering candidates before calling the LLM.

---

## 2. Goals

### Primary Goals

- [ ] Reduce `curate dedup` wall-clock time from ~15-30 minutes to ~1-2 minutes
- [ ] Add vector similarity to learnings recall via VectorBackend

### Success Metrics

- `curate dedup` with ~1,200 learnings completes in < 2 minutes (embedding path)
- `curate dedup` without embeddings completes in < 5 minutes (parallel + larger batches)
- `recall --query "..."` returns semantically similar results when embeddings exist
- Zero degradation when Ollama is unavailable — existing functionality unchanged

---

## 2.5. Quality Dimensions

### Correctness Requirements

- Embedding pre-filter must not produce false negatives — use a similarity threshold slightly below the LLM threshold
- Parallel batch processing must produce identical merge results to sequential processing
- Vector storage must round-trip f32 values exactly (little-endian BLOB encoding)
- `cosine_similarity` must handle zero vectors, identical vectors, opposite vectors

### Performance Requirements

- Pairwise similarity for ~1,200 learnings (720K dot products of 1024-dim vectors) < 1 second
- Ollama health check timeout: 2-3 seconds
- Embedding API timeout: 30 seconds
- Worker threads must not hold DB connections

### Style Requirements

- `TaskMgrResult<T>`, `TaskMgrError` variants, `rusqlite` for DB
- `ureq` for HTTP (already in deps)
- No `.unwrap()` on embedding API responses — graceful degradation
- Config via `ProjectConfig` with `#[serde(default)]`

### Known Edge Cases

| Edge Case | Why It Matters | Expected Behavior |
|---|---|---|
| Ollama not running | Most common failure mode | Fall back to LLM-only dedup |
| Jina model not pulled | User has Ollama but not the model | Clear error: "Run: ollama pull ..." |
| Zero-length learning content | Degenerate embeddings | Skip that learning, log warning |
| Learning with only title | Minimal content | Embed title alone |
| Embedding model changed | Old vectors incompatible | `model` column tracks provenance; `curate embed --force` re-embeds |
| All embeddings missing | Fresh DB | Falls back to LLM-only path |
| spawn_claude fails for one batch | Intermittent failure | Other parallel batches continue |

---

## 3. User Stories

### US-001: Fast Dedup via Embedding Pre-Filter

**As a** task-mgr user
**I want** `curate dedup` to use vector similarity to pre-filter candidates
**So that** only likely duplicates are sent to the LLM

**Acceptance Criteria:**
- [ ] When embeddings exist, dedup clusters by cosine similarity first
- [ ] Only clusters above threshold are sent to LLM
- [ ] Learnings with no near-duplicates are skipped (no LLM call)
- [ ] Learnings without embeddings go to a fallback LLM batch
- [ ] Falls back entirely to LLM-only when no embeddings exist

### US-002: Parallel LLM Batch Processing

**As a** task-mgr user
**I want** dedup LLM calls to run concurrently
**So that** wall-clock time scales with concurrency

**Acceptance Criteria:**
- [ ] `--concurrency N` flag (default: 2)
- [ ] Merge results identical regardless of concurrency level
- [ ] Errors in one batch don't block others

### US-003: Vector-Based Learnings Recall

**As a** task-mgr user (or loop agent)
**I want** learnings recall to use vector similarity
**So that** semantically related learnings surface even without keyword overlap

**Acceptance Criteria:**
- [ ] `VectorBackend` integrates into `CompositeBackend` alongside FTS5 and Patterns
- [ ] Vector results score-normalized for fair comparison with other backends
- [ ] Degrades gracefully when Ollama unavailable or no embeddings
- [ ] `recall --query "..."` returns vector-matched results when embeddings exist

### US-004: Embedding Computation

**As a** task-mgr user
**I want** to compute embeddings for my learnings
**So that** dedup pre-filtering works

**Acceptance Criteria:**
- [ ] `curate embed` computes embeddings for unembedded active learnings
- [ ] `curate embed --force` re-embeds all
- [ ] `curate embed --status` shows coverage stats
- [ ] Works zero-config with Ollama at localhost:11434

---

## 4. Functional Requirements

### FR-001: Embedding Storage (Migration v15)

`learning_embeddings` table: `learning_id` PK, `model`, `dimensions`, `embedding` BLOB, `created_at`.

### FR-002: OllamaEmbedder

Concrete struct (no trait). Uses `ureq`:
- `embed(text) -> TaskMgrResult<Vec<f32>>` — POST `/api/embed`
- `embed_batch(texts) -> TaskMgrResult<Vec<Vec<f32>>>` — batch via array input
- `is_available() -> bool` — GET `/api/tags`, check model
- Timeouts: 3s health check, 30s embed

### FR-003: Configuration

Extend `ProjectConfig`: `ollamaUrl` (default localhost:11434), `embeddingModel` (default Jina v5). Both `#[serde(default)]`.

### FR-004: Cosine Similarity + Storage

Pure Rust `cosine_similarity(a, b) -> f32`. Returns 0.0 for zero vectors. Storage as little-endian f32 BLOBs.

### FR-005: Embedding-Based Clustering

Union-find: load embeddings, pairwise cosine similarity, cluster connected components above threshold.

### FR-006: Parallel Batch Processing

`std::thread` + `mpsc::channel`. Workers call `spawn_claude()` only. Main thread does DB merges. Default concurrency: 2.

### FR-007: Larger Default Batch Sizes

Fallback path: target ~200K chars/batch, clamped 20-100.

---

## 5. Non-Goals

- **EmbeddingProvider trait** — one provider, one struct. Extract trait if/when a second provider is needed.
- **Auto-embed on learning creation** — `curate embed` is sufficient. Keep learning creation fast.
- **ANN / approximate search** — O(n^2) is fine at n <= 5K.

---

## 6. Technical Considerations

### Affected Components

- `src/db/migrations/v15.rs` (new) — learning_embeddings table
- `src/db/migrations/mod.rs` — register v15, bump version to 15
- `src/learnings/embeddings/mod.rs` (new) — OllamaEmbedder, storage, cosine_similarity
- `src/learnings/mod.rs` — add `pub mod embeddings`
- `src/loop_engine/project_config.rs` — ollamaUrl, embeddingModel
- `src/learnings/retrieval/vector.rs` (new) — VectorBackend
- `src/learnings/retrieval/mod.rs` — add vector module
- `src/learnings/retrieval/composite.rs` — add VectorBackend to default_backends()
- `src/commands/curate/mod.rs` — embedding pre-filter, parallel processing
- `src/commands/curate/dedup.rs` — cluster_by_embedding_similarity
- `src/commands/curate/types.rs` — concurrency in DedupParams
- `src/cli/commands.rs` — Embed subcommand, --concurrency flag
- `src/main.rs` — CurateAction::Embed handler

### Data Flow Contracts

| Data Path | Access Pattern |
|---|---|
| Ollama response -> Vec<f32> | `body["embeddings"][0].as_array()` -> map `as_f64() as f32` |
| Vec<f32> -> BLOB | `iter().flat_map(\|f\| f.to_le_bytes()).collect::<Vec<u8>>()` |
| BLOB -> Vec<f32> | `chunks_exact(4).map(\|c\| f32::from_le_bytes(c.try_into().unwrap()))` |

### Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Ollama unavailable | Loses embedding benefit | Graceful fallback to LLM-only |
| API rate limits with concurrent Claude calls | Throttled/failed batches | Default concurrency 2; errors don't block other batches |
| O(n^2) slow above 5K learnings | Pre-filter bottleneck | Not an issue now (n=1200); switch to ANN later if needed |

### Documentation

| Doc | Action |
|---|---|
| `CLAUDE.md` | Add note about Ollama for embeddings, config fields |
