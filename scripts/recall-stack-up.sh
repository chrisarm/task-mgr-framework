#!/usr/bin/env bash
# Bring up the recall stack (ollama embeddings + llama-box reranker) defined
# in docker/docker-compose.yml at the project root. Idempotent: safe to re-run.
#
# Usage:
#   scripts/recall-stack-up.sh                # GPU + default Jina profiles
#   scripts/recall-stack-up.sh --cpu          # CPU-only profile
#   scripts/recall-stack-up.sh --rebuild      # force `docker compose build`
#   scripts/recall-stack-up.sh --down         # stop the stack
#   scripts/recall-stack-up.sh --embed-profile nemotron-3-embed-q8
#   scripts/recall-stack-up.sh --rerank-profile nemotron-rerank-1b
#   scripts/recall-stack-up.sh --list-profiles
#
# Environment overrides:
#   OLLAMA_URL        (default: http://localhost:11435)
#   RERANKER_URL      (default: http://localhost:8181)
#   RERANKER_MODEL    (default: derived from --rerank-profile)
#   EMBED_MODEL_SUBSTR (default: derived from --embed-profile)
#   OLLAMA_MODEL / RERANK_HF_* / RERANK_MODEL_PATH  (compose build args)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$PROJECT_ROOT/docker/docker-compose.yml"

OLLAMA_URL="${OLLAMA_URL:-http://localhost:11435}"
RERANKER_URL="${RERANKER_URL:-http://localhost:8181}"

EMBED_PROFILE="${EMBED_PROFILE:-jina-small-q8}"
RERANK_PROFILE="${RERANK_PROFILE:-jina-v2}"

REBUILD=0
DOWN=0
LIST_PROFILES=0
PROFILE="gpu"   # "gpu" → no profile flag; "cpu" → --profile cpu

log()  { printf '[recall-stack] %s\n' "$*"; }
warn() { printf '[recall-stack] WARN: %s\n' "$*" >&2; }
die()  { printf '[recall-stack] ERROR: %s\n' "$*" >&2; exit 1; }

apply_embed_profile() {
  case "$1" in
    jina-small-q8|jina)
      EMBED_PROFILE=jina-small-q8
      export OLLAMA_MODEL="${OLLAMA_MODEL:-hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0}"
      EMBED_MODEL_SUBSTR="${EMBED_MODEL_SUBSTR:-jina-embeddings-v5}"
      export OLLAMA_START_PERIOD="${OLLAMA_START_PERIOD:-60s}"
      ;;
    nemotron-3-embed-q8|nemotron-embed|nemotron)
      EMBED_PROFILE=nemotron-3-embed-q8
      export OLLAMA_MODEL="${OLLAMA_MODEL:-hf.co/Aqua00/Nemotron-3-Embed-8B-GGUF:Q8_0}"
      EMBED_MODEL_SUBSTR="${EMBED_MODEL_SUBSTR:-Nemotron-3-Embed}"
      # 8B load + first request can exceed 60s on cold GPU.
      export OLLAMA_START_PERIOD="${OLLAMA_START_PERIOD:-180s}"
      ;;
    *)
      die "unknown --embed-profile '$1' (try --list-profiles)"
      ;;
  esac
}

apply_rerank_profile() {
  case "$1" in
    jina-v2|jina)
      RERANK_PROFILE=jina-v2
      export RERANK_HF_REPO="${RERANK_HF_REPO:-gpustack/jina-reranker-v2-base-multilingual-GGUF}"
      export RERANK_HF_FILE="${RERANK_HF_FILE:-jina-reranker-v2-base-multilingual-FP16.gguf}"
      export RERANK_HF_REVISION="${RERANK_HF_REVISION:-09a0e5b9f3d193a4f1e771ba6ceccdf1153d3a9a}"
      export RERANK_MODEL_PATH="${RERANK_MODEL_PATH:-/models/jina-reranker-v2.gguf}"
      # Config `rerankerModel` string historically used by task-mgr:
      RERANKER_MODEL="${RERANKER_MODEL:-jina-reranker-v2-base-multilingual}"
      ;;
    nemotron-rerank-1b|nemotron-rerank|nemotron)
      RERANK_PROFILE=nemotron-rerank-1b
      export RERANK_HF_REPO="${RERANK_HF_REPO:-kread/llama-nemotron-rerank-1b-v2-GGUF}"
      export RERANK_HF_FILE="${RERANK_HF_FILE:-llama-nemotron-rerank-1b-v2-q8_0.gguf}"
      export RERANK_HF_REVISION="${RERANK_HF_REVISION:-main}"
      export RERANK_MODEL_PATH="${RERANK_MODEL_PATH:-/models/llama-nemotron-rerank-1b-v2-q8_0.gguf}"
      # llama-box advertises the GGUF basename in /v1/models.
      RERANKER_MODEL="${RERANKER_MODEL:-llama-nemotron-rerank-1b-v2-q8_0.gguf}"
      ;;
    *)
      die "unknown --rerank-profile '$1' (try --list-profiles)"
      ;;
  esac
}

list_profiles() {
  cat <<'EOF'
Embedding profiles (--embed-profile):
  jina-small-q8          Default. ~0.6 GB, 1024-d. Fast.
  nemotron-3-embed-q8    Nemotron-3-Embed 8B Q8_0 (~8.5 GB, 4096-d). Needs ~9–11 GB VRAM.

Reranker profiles (--rerank-profile):
  jina-v2                Default. ~0.5 GB FP16, 1024-token ctx.
  nemotron-rerank-1b     Nemotron rerank 1B Q8_0 (~1.3 GB), 8192-token ctx.

After switching embed profile, set .task-mgr/config.json and re-embed:
  {
    "ollamaUrl": "http://localhost:11435",
    "embeddingProfile": "nemotron-3-embed-q8",
    "rerankerUrl": "http://localhost:8181",
    "rerankerProfile": "nemotron-rerank-1b"
  }
  task-mgr curate embed --force
EOF
}

while (( $# > 0 )); do
  case "$1" in
    --rebuild) REBUILD=1 ;;
    --down)    DOWN=1 ;;
    --cpu)     PROFILE="cpu" ;;
    --gpu)     PROFILE="gpu" ;;
    --embed-profile)
      shift
      [[ $# -gt 0 ]] || die "--embed-profile requires an argument"
      apply_embed_profile "$1"
      ;;
    --rerank-profile)
      shift
      [[ $# -gt 0 ]] || die "--rerank-profile requires an argument"
      apply_rerank_profile "$1"
      ;;
    --list-profiles) LIST_PROFILES=1 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) printf 'unknown arg: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

if (( LIST_PROFILES )); then
  list_profiles
  exit 0
fi

# Apply defaults if flags were not passed (still sets exports for compose).
apply_embed_profile "$EMBED_PROFILE"
apply_rerank_profile "$RERANK_PROFILE"

compose() {
  if [[ "$PROFILE" == "cpu" ]]; then
    docker compose -f "$COMPOSE_FILE" --profile cpu "$@"
  else
    docker compose -f "$COMPOSE_FILE" "$@"
  fi
}

wait_http_ok() {
  local label="$1" url="$2" timeout="${3:-120}"
  local deadline=$((SECONDS + timeout))
  log "waiting for $label at $url (timeout ${timeout}s)"
  while (( SECONDS < deadline )); do
    if curl -sf --max-time 3 "$url" >/dev/null 2>&1; then
      log "  $label OK"
      return 0
    fi
    sleep 2
  done
  return 1
}

verify_ollama_model() {
  local tags
  tags="$(curl -sf --max-time 5 "$OLLAMA_URL/api/tags" || true)"
  if grep -q "$EMBED_MODEL_SUBSTR" <<<"$tags"; then
    log "ollama model present (matches '$EMBED_MODEL_SUBSTR')"
  else
    warn "ollama responding but no '$EMBED_MODEL_SUBSTR' model loaded — recall will fail"
    grep -oE '"name":"[^"]+"' <<<"$tags" | sed 's/^/    /' >&2 || true
    return 1
  fi
}

verify_reranker() {
  local resp
  resp="$(curl -sf --max-time 30 -X POST "$RERANKER_URL/v1/rerank" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"$RERANKER_MODEL\",\"query\":\"test query\",\"documents\":[\"alpha\",\"beta\"],\"top_n\":2}" \
    2>/dev/null || true)"
  if grep -q '"relevance_score"' <<<"$resp"; then
    log "reranker scoring OK (model=$RERANKER_MODEL)"
  else
    # Fallback: try model id advertised by /v1/models (GGUF basename).
    local advertised
    advertised="$(curl -sf --max-time 5 "$RERANKER_URL/v1/models" 2>/dev/null \
      | grep -oE '"id"[[:space:]]*:[[:space:]]*"[^"]+"' | head -1 | sed 's/.*"\([^"]*\)"/\1/' || true)"
    if [[ -n "$advertised" && "$advertised" != "$RERANKER_MODEL" ]]; then
      resp="$(curl -sf --max-time 30 -X POST "$RERANKER_URL/v1/rerank" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$advertised\",\"query\":\"test query\",\"documents\":[\"alpha\",\"beta\"],\"top_n\":2}" \
        2>/dev/null || true)"
      if grep -q '"relevance_score"' <<<"$resp"; then
        log "reranker scoring OK (advertised model=$advertised)"
        warn "config rerankerModel should be '$advertised' to match llama-box"
        return 0
      fi
    fi
    warn "reranker reachable but /v1/rerank did not return scores"
    warn "  response: ${resp:0:300}"
    return 1
  fi
}

print_config_hint() {
  log "suggested .task-mgr/config.json keys:"
  cat <<EOF
  {
    "ollamaUrl": "$OLLAMA_URL",
    "embeddingProfile": "$EMBED_PROFILE",
    "rerankerUrl": "$RERANKER_URL",
    "rerankerProfile": "$RERANK_PROFILE",
    "rerankerOverFetchPercent": 200
  }
EOF
  log "  (rerankerOverFetchPercent = extra % beyond --limit; default 200, max 300)"
  if [[ "$EMBED_PROFILE" != "jina-small-q8" ]]; then
    warn "after changing embeddingProfile, run: task-mgr curate embed --force"
  fi
  if command -v nvidia-smi >/dev/null 2>&1; then
    log "GPU memory snapshot:"
    nvidia-smi --query-gpu=memory.used,memory.free --format=csv,noheader 2>/dev/null | sed 's/^/  /' || true
  fi
}

main() {
  command -v docker >/dev/null || die "docker not found in PATH"
  docker info >/dev/null 2>&1 || die "docker daemon not reachable (try: sudo systemctl start docker)"
  [[ -f "$COMPOSE_FILE" ]] || die "compose file missing: $COMPOSE_FILE"

  if (( DOWN )); then
    log "stopping stack"
    compose down
    exit 0
  fi

  log "profile: $PROFILE  embed=$EMBED_PROFILE  rerank=$RERANK_PROFILE"
  log "compose: $COMPOSE_FILE"
  log "OLLAMA_MODEL=$OLLAMA_MODEL"
  log "RERANK_MODEL_PATH=$RERANK_MODEL_PATH"

  if (( REBUILD )); then
    compose up -d --build
  else
    compose up -d
  fi

  local ollama_wait=120
  if [[ "$EMBED_PROFILE" == "nemotron-3-embed-q8" ]]; then
    ollama_wait=300
  fi

  wait_http_ok "ollama"   "$OLLAMA_URL/api/tags"     "$ollama_wait" || die "ollama failed to come up at $OLLAMA_URL"
  wait_http_ok "llama-box" "$RERANKER_URL/v1/models" 180 \
    || wait_http_ok "llama-box" "$RERANKER_URL/health" 5 \
    || die "llama-box failed to come up at $RERANKER_URL"

  verify_ollama_model || die "ollama model missing — rebuild image: $0 --rebuild --embed-profile $EMBED_PROFILE"
  verify_reranker     || die "reranker verification failed — rebuild: $0 --rebuild --rerank-profile $RERANK_PROFILE"

  log "stack ready: ollama=$OLLAMA_URL  reranker=$RERANKER_URL"
  print_config_hint
}

main "$@"
