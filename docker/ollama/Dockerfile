# Bakes an embedding model into the image so the first request is instant.
# Override OLLAMA_MODEL at build time to select a catalog profile:
#   jina-small-q8 (default):
#     hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0
#   nemotron-3-embed-q8 (~8.5 GB download; longer build):
#     hf.co/Aqua00/Nemotron-3-Embed-8B-GGUF:Q8_0
#
# Example:
#   docker compose -f docker/docker-compose.yml build \
#     --build-arg OLLAMA_MODEL=hf.co/Aqua00/Nemotron-3-Embed-8B-GGUF:Q8_0 ollama
FROM ollama/ollama:0.22.0

ARG OLLAMA_MODEL=hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0
ENV OLLAMA_MODEL=${OLLAMA_MODEL}

# Trust host-specific CAs (e.g. a TLS-inspecting proxy like Cloudflare WARP)
# BEFORE the model pull, or the download fails cert verification. The `certs`
# named context is docker/certs/ (see docker-compose.yml additional_contexts);
# an empty dir is valid — update-ca-certificates then installs nothing.
COPY --from=certs / /usr/local/share/ca-certificates/host-extra/
RUN update-ca-certificates

# Start the daemon, pull the model, then shut down cleanly.
# The pulled blobs are stored in /root/.ollama/models inside the image layer.
RUN ollama serve & sleep 5 && ollama pull $OLLAMA_MODEL && pkill ollama && sleep 1

EXPOSE 11434

# Inherited from base image; runs `ollama serve`.
CMD ["serve"]
