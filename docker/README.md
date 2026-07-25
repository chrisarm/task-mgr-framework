# Docker Services for task-mgr

Two services that back the optional recall features:

| Service | Port | Purpose |
|---------|------|---------|
| `ollama` / `ollama-cpu` | 11435 | Hosts the embedding model for semantic recall (default: Jina; optional Nemotron Q8) |
| `llama-box` / `llama-box-cpu` | 8181 | Hosts the cross-encoder reranker (default: Jina v2; optional Nemotron 1B) |

Default models are baked into the image at build time — container startup is instant
with no network dependency at runtime. Alternate profiles are selected via
`scripts/recall-stack-up.sh --embed-profile` / `--rerank-profile` (compose build-args).

## Prerequisites

- Docker 24+ with the Compose plugin (`docker compose version`)
- **GPU path**: Nvidia GPU + [nvidia-container-toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html) installed
- **CPU path**: no GPU required

## Quick Start

### GPU (default)

```bash
# Build images and start services in the background
docker compose -f docker/docker-compose.yml up -d --build

# Verify
curl localhost:11435/api/tags
curl -X POST localhost:8181/v1/rerank \
  -H 'Content-Type: application/json' \
  -d '{"model":"jina-reranker-v2-base-multilingual","query":"test","documents":["a","b"],"top_n":2}'
```

### CPU-only

```bash
docker compose -f docker/docker-compose.yml --profile cpu up -d --build

# Same verification commands as above
```

> **Note**: The default profile (`docker compose up`) starts the GPU services.
> You must pass `--profile cpu` explicitly to start the CPU-only variants instead.
> Running `docker compose up` without `--profile cpu` on a machine without Nvidia
> drivers will produce a clear error from Docker's device reservation check.

## task-mgr Configuration

Prefer catalog **profiles** (see `scripts/recall-stack-up.sh --list-profiles`):

```json
{
  "ollamaUrl": "http://localhost:11435",
  "embeddingProfile": "jina-small-q8",
  "rerankerUrl": "http://localhost:8181",
  "rerankerProfile": "jina-v2",
  "rerankerOverFetch": 3
}
```

Raw `embeddingModel` / `rerankerModel` strings remain supported as an escape hatch.
After changing `embeddingProfile`, run `task-mgr curate embed --force`.

## Service Details

### ollama (`docker/ollama/Dockerfile`)

- Base image: `ollama/ollama:latest`
  (TODO: pin to a specific version, e.g. `ollama/ollama:0.6.x`, for reproducibility)
- Baked model: `hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0`
- Host port: `11435` (mapped to container port `11434`, ollama's internal default)

### llama-box (`docker/llama-box/Dockerfile`)

- Base image: `ghcr.io/gpustack/llama-box:latest`
  (TODO: pin to a specific version, e.g. `ghcr.io/gpustack/llama-box:v0.0.x`, for reproducibility)
- Baked model: `gpustack/jina-reranker-v2-base-multilingual-GGUF` (f16 GGUF)
- Host port: `8181` (mapped to container port `8080`, llama-box's internal default — host-side 8181 avoids conflict with other projects on 8080)
- Runs in rerank mode: `--rerank --model /models/jina-reranker-v2.gguf --port 8080 --host 0.0.0.0`

## Failure Modes

| Failure | Behavior |
|---------|----------|
| GPU host without Nvidia driver | `docker compose up` exits with Docker device-reservation error. Use `--profile cpu`. |
| Build-time network failure on model download | The `RUN` layer retries (wget: up to 3 attempts with 5 s back-off; huggingface-cli: its own retry logic). A failed build leaves no image — re-run `docker compose up --build`. Large model files (jina-reranker f16 ≈ 500 MB) may take several minutes on slow connections. |
| Ollama unreachable at recall time | `task-mgr recall --query` hard-fails with a clear error. Pass `--allow-degraded` for offline use. |
| Reranker unreachable at recall time | recall soft-fails (skips reranking, uses raw merged results). No data loss. |

## Port Mapping Verification

```bash
# Confirms ollama is up and the baked model is listed
curl localhost:11435/api/tags | python3 -m json.tool

# Confirms llama-box rerank endpoint is alive
curl -s -X POST localhost:8181/v1/rerank \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "jina-reranker-v2-base-multilingual",
    "query": "semantic search",
    "documents": ["text about embeddings", "unrelated content"],
    "top_n": 2
  }' | python3 -m json.tool
```
