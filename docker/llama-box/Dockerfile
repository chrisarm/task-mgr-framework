# Bakes a reranker GGUF into the image so startup is instant.
#
# llama-box binary downloaded from pinned GitHub release — no container registry auth needed.
# llama-box docs: https://github.com/gpustack/llama-box
#
# Build args:
#   LLAMA_BOX_VERSION  — pinned release tag (default: v0.0.171)
#   LLAMA_BOX_VARIANT  — cpu (default) or cuda-12.8 for GPU profile
#   HF_REPO / HF_FILE / HF_REVISION / MODEL_PATH — which GGUF to bake
#
# Catalog profiles (scripts/recall-stack-up.sh --rerank-profile):
#   jina-v2 (default):
#     HF_REPO=gpustack/jina-reranker-v2-base-multilingual-GGUF
#     HF_FILE=jina-reranker-v2-base-multilingual-FP16.gguf
#     MODEL_PATH=/models/jina-reranker-v2.gguf
#   nemotron-rerank-1b (~1.3 GB Q8_0):
#     HF_REPO=kread/llama-nemotron-rerank-1b-v2-GGUF
#     HF_FILE=llama-nemotron-rerank-1b-v2-q8_0.gguf
#     MODEL_PATH=/models/llama-nemotron-rerank-1b-v2-q8_0.gguf
#     HF_REVISION=main

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
ARG HF_REPO=gpustack/jina-reranker-v2-base-multilingual-GGUF
ARG HF_FILE=jina-reranker-v2-base-multilingual-FP16.gguf
ARG HF_REVISION=09a0e5b9f3d193a4f1e771ba6ceccdf1153d3a9a
ARG MODEL_PATH=/models/jina-reranker-v2.gguf

ENV MODEL_PATH=${MODEL_PATH}
ENV HF_REPO=${HF_REPO}
ENV HF_FILE=${HF_FILE}
ENV HF_REVISION=${HF_REVISION}

RUN apt-get update && apt-get install -y --no-install-recommends \
    wget \
    unzip \
    libgomp1 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Trust host-specific CAs (e.g. a TLS-inspecting proxy like Cloudflare WARP)
# BEFORE the downloads below, or they fail cert verification. The `certs`
# named context is docker/certs/ (see docker-compose.yml additional_contexts);
# an empty dir is valid — update-ca-certificates then installs nothing.
COPY --from=certs / /usr/local/share/ca-certificates/host-extra/
RUN update-ca-certificates

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

# No ENTRYPOINT binary — CMD expands $MODEL_PATH from ENV for profile-selected GGUFs.
# (ENTRYPOINT+shell CMD would become `llama-box sh -c ...` and break.)
#
# `--parallel 1` is critical: llama-box defaults to n_seq_max=8, which divides
# the context window across parallel sequences. We never multiplex reranks per
# request, so collapsing to a single sequence gives each call the full budget.
#
# `--gpu-layers 999` offloads every model layer to the GPU when the CUDA
# backend is available. With CPU-only builds the flag is a no-op.
CMD ["sh", "-c", "exec /opt/llama-box/llama-box --rerank --model \"$MODEL_PATH\" --port 8080 --host 0.0.0.0 --parallel 1 --gpu-layers 999"]
