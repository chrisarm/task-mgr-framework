# Recall stack — new-machine setup

`task-mgr recall` (the semantic-recall path) needs two HTTP services running
locally:

| Service     | Port  | What it does                                                                 |
|-------------|-------|------------------------------------------------------------------------------|
| `ollama`    | 11435 | Generates query/document embeddings (profile-selected model)                 |
| `llama-box` | 8181  | Cross-encoder reranker over top-K hits (profile-selected GGUF)               |

Both ship as Docker images in this repo; default profiles bake Jina weights at
build time so first-call latency is ~ms. Alternate profiles (Nemotron) are
opt-in and download larger weights.

---

## Model profiles

| Kind | Profile id | Weights (approx) | Notes |
|------|------------|------------------|-------|
| Embed | `jina-small-q8` **(default)** | ~0.6 GB | 1024-d, no text prefixes |
| Embed | `nemotron-3-embed-q8` | ~8.5 GB | 4096-d; uses `query:` / `passage:` prefixes; re-embed after switch |
| Rerank | `jina-v2` **(default)** | ~0.5 GB | 1024-token ctx |
| Rerank | `nemotron-rerank-1b` | ~1.3 GB Q8_0 | **Blocked on llama-box v0.0.171** (`llama-embed` arch unknown). Use `jina-v2` until llama-box upgrades. |

List profiles: `scripts/recall-stack-up.sh --list-profiles`

**VRAM (laptop RTX A4500 15 GB example):** Jina+Jina is easy; Nemotron embed alone
~9–11 GB GPU; Nemotron embed + Nemotron rerank is tight (~12–14 GB). GPU offload
requires nvidia-container-toolkit + current CDI (see `scripts/recall-stack-up.sh`
boot notes / host `nvidia-cdi-refresh` if GPU processes do not appear in
`nvidia-smi`).

---

## Prerequisites

- **Docker 24+ with the Compose plugin** — verify `docker compose version`.
- **GPU path (recommended)**: NVIDIA GPU + driver + the
  [nvidia-container-toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html).
  Verify with `docker info | grep -i runtime` (should list `nvidia`) and
  `nvidia-smi -L`.
- **CPU path**: works on any machine; reranks are slower but functionally identical.
- **Disk**: ~3 GB for default Jina images; +~9 GB if baking Nemotron embed.
- **Free TCP ports 11435 and 8181**.

## First-time bring-up

```sh
# from the repo root:
scripts/recall-stack-up.sh --rebuild            # default Jina + Jina; ~5–15 min first run
# CPU-only machine:
scripts/recall-stack-up.sh --cpu --rebuild
# High-quality embed (long download / rebuild):
scripts/recall-stack-up.sh --rebuild \
  --embed-profile nemotron-3-embed-q8 \
  --rerank-profile nemotron-rerank-1b
```

The script:
1. Verifies `docker` is installed and the daemon is reachable.
2. Runs `docker compose -f docker/docker-compose.yml up -d` (with `--build` if requested).
3. Polls Ollama + llama-box until healthy.
4. Confirms the selected embedding model is listed in Ollama.
5. Sends a real `/v1/rerank` request and confirms `relevance_score` values.
6. Prints suggested `.task-mgr/config.json` keys for the selected profiles.

## Day-to-day

```sh
scripts/recall-stack-up.sh         # idempotent — fast no-op if already healthy
scripts/recall-stack-up.sh --down  # stop both services
scripts/recall-stack-up.sh --rebuild   # after editing a Dockerfile or changing profile
scripts/recall-stack-up.sh --list-profiles
```

## Configure task-mgr to use the stack

**Preferred (catalog profiles):**

```json
{
  "ollamaUrl": "http://localhost:11435",
  "embeddingProfile": "jina-small-q8",
  "rerankerUrl": "http://localhost:8181",
  "rerankerProfile": "jina-v2",
  "rerankerOverFetchPercent": 200
}
```

**Nemotron quality stack:**

```json
{
  "ollamaUrl": "http://localhost:11435",
  "embeddingProfile": "nemotron-3-embed-q8",
  "rerankerUrl": "http://localhost:8181",
  "rerankerProfile": "nemotron-rerank-1b",
  "rerankerOverFetchPercent": 200
}
```

`rerankerOverFetchPercent` is **extra** percent beyond `--limit` (not a multiplier).
Example: `50` with limit 10 fetches 15 candidates before rerank. Default **200**
(old multiplier of 3). Max **300** (clamped). Absolute slate hard-cap remains 30.
The legacy key `rerankerOverFetch` is ignored with a warning.

Raw escape hatches still work: `embeddingModel` / `rerankerModel` strings without
a profile. When `embeddingModel` matches a catalog Ollama id, prefixes are
applied automatically.

### After changing the embedding profile

Vectors are keyed by model string with a composite PK `(learning_id, model)`
(migration v21). Switching profiles **keeps** other models' rows; only the
active model is used for recall. Gap-fill embeddings for the newly active model
(does not re-embed docs that already have that model):

```sh
task-mgr curate embed            # only learnings missing the active model
# task-mgr curate embed --force  # re-embed ALL active learnings for active model only
# task-mgr curate embed --prune-stale --status  # reclaim rows left by prior models (DB-only)
task-mgr curate embed --status   # shows profile, model, dims, coverage, per-model rows
task-mgr recall --query 'overflow recovery ladder' --limit 5
```

Top results will be tagged `match_reason: "cross-encoder rerank"` when the
reranker is healthy, and `vector similarity` when it soft-fails.

## Failure modes & escape hatches

| Symptom | Cause / fix |
|---------|-------------|
| `docker compose up` errors on device reservation | No NVIDIA runtime — re-run with `--cpu`. |
| Build fails downloading model from Hugging Face | Transient HF outage / TLS MITM without `docker/certs/`. Dockerfile retries 3×; re-run `--rebuild`. |
| `task-mgr recall` errors "Ollama embedding service unreachable" | Ollama down. Run the script, or `--allow-degraded` for FTS5/pattern only. |
| `task-mgr recall` warns "reranker: ... using un-reranked order" | Reranker down — results still return without cross-encoder order. |
| Empty vector hits after profile switch | Run `task-mgr curate embed` (gap-fill; only learnings missing the new model are embedded). |
| Port 11435 or 8181 already in use | Stop the other service or remap compose ports + config URLs. |
| GPU free but models on CPU | Host CDI/runtime issue; ensure nvidia-container-toolkit + CDI refresh. |

## What's pinned and where

- **llama-box**: `v0.0.171` (`docker/llama-box/Dockerfile` `LLAMA_BOX_VERSION`)
- **CUDA base**: `nvidia/cuda:12.8.0-cudnn-runtime-ubuntu22.04`
- **ollama base**: `ollama/ollama:0.22.0`
- **Default embed**: `OLLAMA_MODEL` build-arg (Jina small Q8_0)
- **Default rerank**: `HF_REPO` / `HF_FILE` / `MODEL_PATH` build-args (Jina v2 FP16)
- **Catalog SSoT (Rust)**: `src/learnings/embeddings/profiles.rs`, `src/learnings/reranker/profiles.rs`

Bumping models or profiles requires a `--rebuild` for the affected service.
