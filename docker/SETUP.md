# Recall stack — new-machine setup

`task-mgr recall` (the semantic-recall path on `feat/recall-strict-rerank`) needs
two HTTP services running locally:

| Service     | Port  | What it does                                                                 |
|-------------|-------|------------------------------------------------------------------------------|
| `ollama`    | 11434 | Generates query embeddings via `jina-embeddings-v5-text-small-retrieval`     |
| `llama-box` | 8080  | Cross-encoder reranker (`jina-reranker-v2-base-multilingual`) over top-K hits |

Both ship as Docker images in this repo; both bake the model weights at build
time so first-call latency is ~ms, not "download a model" minutes.

---

## Prerequisites

- **Docker 24+ with the Compose plugin** — verify `docker compose version`.
- **GPU path (recommended)**: NVIDIA GPU + driver + the
  [nvidia-container-toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/install-guide.html).
  Verify with `docker info | grep -i runtime` (should list `nvidia`) and
  `nvidia-smi -L`.
- **CPU path**: works on any machine; reranks are noticeably slower (5–30 s
  for a typical batch on a laptop CPU) but functionally identical.
- **~3 GB free disk** for the two images (model weights dominate).
- **Free TCP ports 11434 and 8080** — if anything else binds them, stop it
  first or override via the compose file.

## First-time bring-up

```sh
# from the repo root, on the feat/recall-strict-rerank branch:
git checkout feat/recall-strict-rerank          # required — main does not yet ship the recall code
scripts/recall-stack-up.sh --rebuild            # build + start; ~5–15 min on first run (model downloads)
# CPU-only machine:
scripts/recall-stack-up.sh --cpu --rebuild
```

The script:
1. Verifies `docker` is installed and the daemon is reachable.
2. Runs `docker compose -f docker/docker-compose.yml up -d` (with `--build` if requested).
3. Polls `http://localhost:11434/api/tags` and `http://localhost:8080/v1/models`
   until both respond (timeouts: 120 s and 180 s).
4. Confirms the embedding model is loaded inside ollama.
5. Sends a real `/v1/rerank` request and confirms it returns `relevance_score`
   values.

If anything fails, the script exits non-zero with a clear message.

## Day-to-day

```sh
scripts/recall-stack-up.sh         # idempotent — fast no-op if already healthy
scripts/recall-stack-up.sh --down  # stop both services
scripts/recall-stack-up.sh --rebuild   # after editing a Dockerfile
```

After reboot just re-run the script — `restart: unless-stopped` in the compose
file means Docker usually has them up before you log in, but the script
verifies it.

## Configure task-mgr to use the stack

Add to the project's `.task-mgr/config.json` (already set on this machine):

```json
{
  "ollamaUrl": "http://localhost:11434",
  "embeddingModel": "hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0",
  "rerankerUrl": "http://localhost:8080",
  "rerankerModel": "jina-reranker-v2-base-multilingual",
  "rerankerOverFetch": 3
}
```

Then:

```sh
task-mgr recall --query 'overflow recovery ladder' --limit 5
```

Top results will be tagged `match_reason: "cross-encoder rerank"` when the
reranker is healthy, and `vector similarity` when it soft-fails.

## Failure modes & escape hatches

| Symptom                                                | Cause / fix                                                                                                                |
|--------------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------|
| `docker compose up` errors on device reservation        | No NVIDIA runtime — re-run with `--cpu`.                                                                                   |
| Build fails downloading model from Hugging Face         | Transient HF outage. The Dockerfile retries 3× with back-off; if it still fails, re-run `--rebuild` later.                 |
| `task-mgr recall` errors "Ollama embedding service unreachable" | Ollama container is down. Run the script. Or pass `--allow-degraded` to fall back to FTS5/pattern recall.            |
| `task-mgr recall` warns "reranker: ... using un-reranked order" | Reranker container is down — recall still returns results, just without cross-encoder rerank. Run the script.       |
| Port 11434 or 8080 already in use                       | Another service (e.g. a host-installed `ollama`) owns the port. Stop it (`systemctl stop ollama`) or change the compose port mapping. |
| Slow first rerank after reboot                          | llama-box does a one-time JIT warmup on the first request; subsequent calls are fast.                                      |

## What's pinned and where

- **llama-box**: `v0.0.171` (`docker/llama-box/Dockerfile` `LLAMA_BOX_VERSION`)
- **CUDA base**: `nvidia/cuda:12.8.0-cudnn-runtime-ubuntu22.04`
- **ollama base**: `ollama/ollama:0.22.0`
- **Reranker model revision**: `09a0e5b9f3d193a4f1e771ba6ceccdf1153d3a9a` (`HF_REVISION` in `docker/llama-box/Dockerfile`)
- **Embedding model**: `hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0` (`OLLAMA_MODEL` in `docker/ollama/Dockerfile`)

Bumping any of these requires a `--rebuild`.
