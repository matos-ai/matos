ARG RUST_VERSION=1.97.1

FROM debian:bookworm-slim AS rust-toolchain
ARG RUST_VERSION
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "$RUST_VERSION" -c clippy -c rustfmt \
    && chmod -R a+rX /usr/local/rustup /usr/local/cargo

FROM node:22-slim AS base

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates build-essential pkg-config libssl-dev curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust-toolchain /usr/local/rustup /usr/local/rustup
COPY --from=rust-toolchain /usr/local/cargo /usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/home/executor/.cargo PATH=/usr/local/cargo/bin:$PATH

RUN userdel -r node \
    && useradd --uid 1000 --user-group --create-home --home-dir /home/executor --shell /bin/bash executor \
    && mkdir /workspace \
    && chown executor:executor /workspace

COPY --chmod=0755 entrypoint.sh /usr/local/bin/entrypoint.sh
COPY --chmod=0644 guard.sh /usr/local/bin/guard.sh
COPY --chmod=0644 companions.sh /usr/local/bin/companions.sh

ENV HOME=/home/executor
WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]

FROM base AS codex
RUN npm install -g @openai/codex@0.167.0 \
    && npm cache clean --force
USER 1000:1000

FROM base AS antigravity
ARG TARGETARCH
ARG AGY_VERSION=1.5.3
ARG AGY_SHA256_AMD64=1b4850599cce73a17f94ffb422df3bf9531531b77dbddc40c1a435e0c2c16807
ARG AGY_SHA256_ARM64=2d50060c7b1327995e5c81ec58829c91c2564ebb9fec165d3f9f1325ec188675
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) asset=agy_cli_linux_x64.tar.gz; sha="$AGY_SHA256_AMD64" ;; \
      arm64) asset=agy_cli_linux_arm64.tar.gz; sha="$AGY_SHA256_ARM64" ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o "/tmp/$asset" "https://github.com/google-antigravity/antigravity-cli/releases/download/${AGY_VERSION}/${asset}"; \
    echo "$sha  /tmp/$asset" | sha256sum -c -; \
    tar -xzf "/tmp/$asset" -C /usr/local/bin antigravity; \
    mv /usr/local/bin/antigravity /usr/local/bin/agy; \
    chmod 755 /usr/local/bin/agy; \
    rm -f "/tmp/$asset"
USER 1000:1000

FROM python:3.12-slim-bookworm AS mistral-vibe-build
ENV UV_TOOL_DIR=/opt/uv-tools UV_TOOL_BIN_DIR=/usr/local/bin UV_PYTHON_DOWNLOADS=never
RUN pip install --no-cache-dir uv==0.9.5 \
    && uv tool install --python /usr/local/bin/python3.12 mistral-vibe==3.1.4 \
    && chmod -R a+rX /opt/uv-tools

FROM python:3.12-slim-bookworm AS mistral-vibe
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates build-essential pkg-config libssl-dev curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=node:22-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=node:22-slim /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx
COPY --from=rust-toolchain /usr/local/rustup /usr/local/rustup
COPY --from=rust-toolchain /usr/local/cargo /usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/home/executor/.cargo PATH=/usr/local/cargo/bin:$PATH
RUN useradd --uid 1000 --user-group --create-home --home-dir /home/executor --shell /bin/bash executor \
    && mkdir /workspace \
    && chown executor:executor /workspace
COPY --from=mistral-vibe-build /opt/uv-tools /opt/uv-tools
COPY --from=mistral-vibe-build /usr/local/bin/vibe /usr/local/bin/vibe-app-server /usr/local/bin/
COPY --chmod=0755 entrypoint.sh /usr/local/bin/entrypoint.sh
COPY --chmod=0644 guard.sh /usr/local/bin/guard.sh
COPY --chmod=0644 companions.sh /usr/local/bin/companions.sh
ENV HOME=/home/executor
WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
USER 1000:1000

FROM base AS executor
RUN npm install -g @anthropic-ai/claude-code@2.1.302 \
    && npm cache clean --force
USER 1000:1000
