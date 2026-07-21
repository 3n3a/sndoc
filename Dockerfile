# syntax=docker/dockerfile:1

# --- builder -----------------------------------------------------------
# git2's vendored-libgit2/vendored-openssl features compile from source, so
# the builder needs cmake + a C compiler (gcc/g++ ship with the rust image).
FROM rust:1-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# Bake the embedding model into the image instead of pulling it from
# HuggingFace at runtime. model2vec-rs's StaticModel::from_pretrained loads
# from a local folder when the path exists, given just these three files.
RUN mkdir -p /model \
    && for f in config.json tokenizer.json model.safetensors; do \
         curl -fSL "https://huggingface.co/minishlab/potion-retrieval-32M/resolve/main/$f" \
           -o "/model/$f"; \
       done

# --- runtime -------------------------------------------------------------
FROM debian:bookworm-slim

# ca-certificates: HTTPS to GitHub for the docs clone/refresh/live fetch.
# curl: used by the HEALTHCHECK below.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/sndoc /usr/local/bin/sndoc
COPY --from=builder /model /opt/sndoc/model

RUN mkdir -p /data

ENV SNDOC_HTTP_ADDR=0.0.0.0:8080 \
    SNDOC_DATA_DIR=/data \
    SNDOC_EMBED_MODEL=/opt/sndoc/model \
    HF_HOME=/data/hf
# SNDOC_HTTP_TOKEN is intentionally left unset here — `sndoc serve --http`
# refuses to start without it; set it at deploy time (see docker-compose.yml
# or the Coolify env var UI).

EXPOSE 8080

# The bearer-token middleware wraps every route, so any request returns 401
# once the listener is bound — a non-"000" curl status means "up and
# serving". The long start-period covers the first-run docs clone + index
# build, which happens before the HTTP listener binds.
HEALTHCHECK --start-period=600s --interval=30s --timeout=5s --retries=3 \
    CMD code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/mcp); \
        [ "$code" != "000" ]

CMD ["sndoc", "serve"]
