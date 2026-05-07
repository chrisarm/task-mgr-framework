# Bakes jina-reranker-v2-base-multilingual GGUF into the image so startup is
# instant. The model is downloaded from Hugging Face at build time.
#
# llama-box binary downloaded from pinned GitHub release — no container registry auth needed.
# llama-box docs: https://github.com/gpustack/llama-box
#
# Build args:
#   LLAMA_BOX_VERSION  — pinned release tag (default: v0.0.171)
#   LLAMA_BOX_VARIANT  — cpu (default) or cuda-12.8 for GPU profile

ARG LLAMA_BOX_VERSION=v0.0.171
ARG LLAMA_BOX_VARIANT=cpu

# nvidia/cuda:*-runtime-* ships libcudart.so + libcublas.so that libggml-cuda.so
# links against. The host's libcuda.so.1 (driver) comes in via the NVIDIA
# container runtime; the toolkit runtime libs do NOT, so the base image must
# provide them. CPU builds tolerate the larger image; the libcudart.so/libcublas.so
# additions are unused but harmless (~600 MB extra is acceptable for a single
# image that handles both --profile cpu and the GPU default).
FROM nvidia/cuda:12.8.0-cudnn-runtime-ubuntu22.04

ARG LLAMA_BOX_VERSION
ARG LLAMA_BOX_VARIANT

ENV MODEL_PATH=/models/jina-reranker-v2.gguf
ENV HF_REPO=gpustack/jina-reranker-v2-base-multilingual-GGUF
ENV HF_FILE=jina-reranker-v2-base-multilingual-FP16.gguf
ENV HF_REVISION=09a0e5b9f3d193a4f1e771ba6ceccdf1153d3a9a

RUN apt-get update && apt-get install -y --no-install-recommends \
    wget \
    unzip \
    libgomp1 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Download the pinned llama-box binary from the GitHub release, extract, and
# install. The release zip ships the binary alongside ~14 shared libraries
# (libllama.so, libmtmd.so, libggml-*.so, etc.). llama.cpp's runtime backend
# loader (`ggml_backend_load_all`) discovers `libggml-*.so` by scanning the
# directory next to the executable, so we install binary + libs together in
# /opt/llama-box and put a wrapper on PATH that runs the binary from that dir.
RUN wget --tries=3 --retry-connrefused --waitretry=5 \
        -O /tmp/llama-box.zip \
        "https://github.com/gpustack/llama-box/releases/download/${LLAMA_BOX_VERSION}/dl-llama-box-linux-amd64-${LLAMA_BOX_VARIANT}.zip" \
    && mkdir -p /opt/llama-box \
    && unzip -j /tmp/llama-box.zip -d /opt/llama-box \
    && chmod 755 /opt/llama-box/llama-box /opt/llama-box/*.so \
    && printf '#!/bin/sh\nexec /opt/llama-box/llama-box "$@"\n' > /usr/local/bin/llama-box \
    && chmod 755 /usr/local/bin/llama-box \
    && rm -rf /tmp/llama-box.zip

RUN mkdir -p /models

# Download the GGUF from Hugging Face. Retry up to 3 times on network failure.
RUN wget --tries=3 --retry-connrefused --waitretry=5 \
         -O "$MODEL_PATH" \
         "https://huggingface.co/$HF_REPO/resolve/$HF_REVISION/$HF_FILE"

EXPOSE 8080

ENTRYPOINT ["llama-box"]

# Run llama-box in rerank mode on the baked model.
#
# `--parallel 1` is critical: llama-box defaults to n_seq_max=8, which divides
# the 1024-token context window across 8 parallel sequences (=128 tokens each).
# That triggers HTTP 400 "sum of lengths of query and document exceeds the
# context size" on any non-trivial document. We never multiplex reranks per
# request, so collapsing to a single sequence gives each rerank call the full
# 1024-token budget.
#
# `--gpu-layers 999` offloads every model layer to the GPU when the CUDA
# backend is available. With CPU-only builds the flag is a no-op. Without it,
# llama-box keeps all layers on CPU even with CUDA loaded — first request
# easily exceeds the client's 30s read timeout.
CMD ["--rerank", "--model", "/models/jina-reranker-v2.gguf", "--port", "8080", "--host", "0.0.0.0", "--parallel", "1", "--gpu-layers", "999"]
