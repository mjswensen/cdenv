FROM debian:13-slim

RUN apt-get update && \
    apt-get install -y build-essential ca-certificates curl git tmux && \
    rm -rf /var/lib/apt/lists/*

SHELL ["/bin/bash", "-o", "pipefail", "-c"]
ENV MISE_DATA_DIR="/mise"
ENV MISE_CONFIG_DIR="/mise"
ENV MISE_CACHE_DIR="/mise/cache"
ENV MISE_INSTALL_PATH="/usr/local/bin/mise"
ENV PATH="/mise/shims:$PATH"

RUN curl https://mise.run | sh
RUN mise install --system node@24 rust@1.97.1
RUN mise exec rust@1.97.1 -- \
    cargo install cargo-deny --version 0.20.2 --locked --root /usr/local
