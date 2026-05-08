#!/usr/bin/env bash
# Bring up the recall stack (ollama embeddings + llama-box reranker) defined
# in docker/docker-compose.yml at the project root. Idempotent: safe to re-run.
#
# Usage:
#   scripts/recall-stack-up.sh                # GPU profile (default)
#   scripts/recall-stack-up.sh --cpu          # CPU-only profile
#   scripts/recall-stack-up.sh --rebuild      # force `docker compose build`
#   scripts/recall-stack-up.sh --down         # stop the stack
#
# Environment overrides:
#   OLLAMA_URL        (default: http://localhost:11435)
#   RERANKER_URL      (default: http://localhost:8080)
#   RERANKER_MODEL    (default: jina-reranker-v2-base-multilingual)
#   EMBED_MODEL_SUBSTR (default: jina-embeddings-v5)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$PROJECT_ROOT/docker/docker-compose.yml"

OLLAMA_URL="${OLLAMA_URL:-http://localhost:11435}"
RERANKER_URL="${RERANKER_URL:-http://localhost:8080}"
RERANKER_MODEL="${RERANKER_MODEL:-jina-reranker-v2-base-multilingual}"
EMBED_MODEL_SUBSTR="${EMBED_MODEL_SUBSTR:-jina-embeddings-v5}"

REBUILD=0
DOWN=0
PROFILE="gpu"   # "gpu" → no profile flag; "cpu" → --profile cpu

while (( $# > 0 )); do
  case "$1" in
    --rebuild) REBUILD=1 ;;
    --down)    DOWN=1 ;;
    --cpu)     PROFILE="cpu" ;;
    --gpu)     PROFILE="gpu" ;;
    -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
    *) printf 'unknown arg: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

log()  { printf '[recall-stack] %s\n' "$*"; }
warn() { printf '[recall-stack] WARN: %s\n' "$*" >&2; }
die()  { printf '[recall-stack] ERROR: %s\n' "$*" >&2; exit 1; }

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
    log "reranker scoring OK"
  else
    warn "reranker reachable but /v1/rerank did not return scores"
    warn "  response: ${resp:0:300}"
    return 1
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

  log "profile: $PROFILE  compose: $COMPOSE_FILE"
  if (( REBUILD )); then
    compose up -d --build
  else
    compose up -d
  fi

  wait_http_ok "ollama"   "$OLLAMA_URL/api/tags"     120 || die "ollama failed to come up at $OLLAMA_URL"
  wait_http_ok "llama-box" "$RERANKER_URL/v1/models" 180 \
    || wait_http_ok "llama-box" "$RERANKER_URL/health" 5 \
    || die "llama-box failed to come up at $RERANKER_URL"

  verify_ollama_model || die "ollama model missing — rebuild image: $0 --rebuild"
  verify_reranker     || die "reranker verification failed"

  log "stack ready: ollama=$OLLAMA_URL  reranker=$RERANKER_URL"
}

main "$@"
