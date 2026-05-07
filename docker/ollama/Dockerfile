# Bakes the jina-embeddings model into the image so the first `ollama run` is
# instant. Change OLLAMA_MODEL to swap the embedding model without rebuilding
# the rest of the image.
FROM ollama/ollama:0.22.0

ENV OLLAMA_MODEL=hf.co/jinaai/jina-embeddings-v5-text-small-retrieval-GGUF:Q8_0

# Start the daemon, pull the model, then shut down cleanly.
# The pulled blobs are stored in /root/.ollama/models inside the image layer.
RUN ollama serve & sleep 5 && ollama pull $OLLAMA_MODEL && pkill ollama && sleep 1

EXPOSE 11434

# Inherited from base image; runs `ollama serve`.
CMD ["serve"]
