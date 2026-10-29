# syntax=docker/dockerfile:1.7
# Allod image (target "allod") and the dev/test tools image (target "tools").
# Build context: repository root.

ARG NODE_IMAGE=node:22.23.4-bookworm-slim
ARG RUST_IMAGE=rust:1.95.0-bookworm
ARG DEBIAN_IMAGE=debian:bookworm-20261020-slim
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot

FROM ${NODE_IMAGE} AS web
WORKDIR /src/web
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0
COPY web/package.json web/pnpm-lock.yaml ./
RUN corepack pnpm install --frozen-lockfile
COPY web/ ./
RUN corepack pnpm build

FROM ${RUST_IMAGE} AS build
WORKDIR /src

# Dependency layer: only the manifests plus one stub source per crate, so a change to Allod's
# own code reuses the compiled third-party crates (the expensive part) instead of recompiling
# everything. Adding a crate means adding its manifest COPY below.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/allod/Cargo.toml crates/allod/Cargo.toml
COPY crates/allod-api/Cargo.toml crates/allod-api/Cargo.toml
COPY crates/allod-core/Cargo.toml crates/allod-core/Cargo.toml
COPY crates/allod-db/Cargo.toml crates/allod-db/Cargo.toml
COPY crates/allod-devtools/Cargo.toml crates/allod-devtools/Cargo.toml
COPY crates/allod-e2e/Cargo.toml crates/allod-e2e/Cargo.toml
COPY crates/allod-keys/Cargo.toml crates/allod-keys/Cargo.toml
COPY crates/allod-ln/Cargo.toml crates/allod-ln/Cargo.toml
COPY crates/allod-lnurl/Cargo.toml crates/allod-lnurl/Cargo.toml
COPY crates/allod-nwc/Cargo.toml crates/allod-nwc/Cargo.toml
COPY crates/allod-types/Cargo.toml crates/allod-types/Cargo.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    set -eu; \
    for d in crates/*/; do mkdir -p "$d/src"; : > "$d/src/lib.rs"; done; \
    for p in allod allod-devtools; do printf 'fn main() {}\n' > "crates/$p/src/main.rs"; done; \
    cargo build --release --locked -p allod -p allod-devtools

# Real sources replace the stubs; only the workspace crates are recompiled.
# `touch` makes them newer than the stub build: copied files keep their checkout mtimes,
# which are older, and cargo would otherwise keep using the stub artifacts.
# No cache mount on target: the compiled dependencies have to stay in the layer so the
# GitHub Actions cache (deploy/compose.ci.yaml) can restore them.
RUN rm -rf crates
COPY crates crates
COPY --from=web /src/web/dist web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    find crates -type f -exec touch {} +; \
    cargo build --release --locked -p allod -p allod-devtools \
 && install -m 0755 target/release/allod target/release/allod-devtools /usr/local/bin/ \
 && mkdir -p /out/data /out/handoff /out/control

FROM ${RUNTIME_IMAGE} AS allod
COPY --from=build /usr/local/bin/allod /usr/local/bin/allod
COPY --from=build --chown=1000:1000 /out/data /data
COPY --from=build --chown=1000:1000 /out/handoff /handoff
COPY --from=build --chown=1000:1000 /out/control /control
USER 1000:1000
ENV ALLOD_LISTEN=0.0.0.0:8029 \
    ALLOD_DATA_DIR=/data \
    ALLOD_HANDOFF_DIR=/handoff \
    ALLOD_CONTROL_DIR=/control
EXPOSE 8029
VOLUME ["/data"]
HEALTHCHECK --interval=15s --timeout=10s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/allod", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/allod"]
CMD ["serve"]

# Tools for the regtest overlay and CI: bitcoin-cli, ldk-server-cli, allod-devtools.
# The `ldk` and `btc` named contexts are the ldk-server and bitcoind images
# (compose: additional_contexts).
FROM ${DEBIAN_IMAGE} AS tools
RUN apt-get update \
 && apt-get install -y --no-install-recommends bash ca-certificates curl jq \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/allod-devtools /usr/local/bin/allod-devtools
COPY --from=ldk /usr/local/bin/ldk-server-cli /usr/local/bin/ldk-server-cli
COPY --from=btc /usr/local/bin/bitcoin-cli /usr/local/bin/bitcoin-cli
COPY deploy/scripts/ /usr/local/share/allod/
ENTRYPOINT []
CMD ["bash"]
