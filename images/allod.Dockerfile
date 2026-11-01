# syntax=docker/dockerfile:1.10
# Allod image (target "allod") and the dev/test tools image (target "tools").
# Build context: repository root.

ARG NODE_IMAGE=node:22.23.4-bookworm-slim
ARG RUST_IMAGE=rust:1.95.0-bookworm
ARG DEBIAN_IMAGE=debian:bookworm-20261020-slim
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot

# web/dist is the same on every architecture: build it once, on the builder's own platform.
FROM --platform=$BUILDPLATFORM ${NODE_IMAGE} AS web
WORKDIR /src/web
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0
COPY web/package.json web/pnpm-lock.yaml ./
RUN corepack pnpm install --frozen-lockfile
COPY web/ ./
RUN corepack pnpm build

FROM ${RUST_IMAGE} AS build
WORKDIR /src
ARG TARGETARCH
COPY images/install-sccache.sh images/cargo-sccache.sh /usr/local/bin/
# Optional: offline builds carry on without sccache (a bad checksum installs nothing).
RUN install-sccache.sh "$TARGETARCH" || echo "sccache not installed" >&2
# Cargo profile: "release" for published images, "fast-release" for regtest and nightly
# images (deploy/compose.regtest.yaml, .github/workflows/nightly.yml; no LTO, so a
# rebuild relinks in seconds).
ARG CARGO_PROFILE=release
# rust-lightning is a pinned git dependency (allod-gossip); Cargo's built-in Git fetch
# cannot resolve that commit from GitHub's mirror, the Git CLI fetches it by SHA.
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

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
COPY crates/allod-gossip/Cargo.toml crates/allod-gossip/Cargo.toml
COPY crates/allod-keys/Cargo.toml crates/allod-keys/Cargo.toml
COPY crates/allod-ln/Cargo.toml crates/allod-ln/Cargo.toml
COPY crates/allod-lnurl/Cargo.toml crates/allod-lnurl/Cargo.toml
COPY crates/allod-nwc/Cargo.toml crates/allod-nwc/Cargo.toml
COPY crates/allod-types/Cargo.toml crates/allod-types/Cargo.toml
# The optional ACTIONS_* secrets turn on sccache (images/cargo-sccache.sh); secrets never
# reach the image or the layer cache key.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=secret,id=ACTIONS_RESULTS_URL,env=ACTIONS_RESULTS_URL \
    --mount=type=secret,id=ACTIONS_RUNTIME_TOKEN,env=ACTIONS_RUNTIME_TOKEN \
    set -eu; \
    for d in crates/*/; do mkdir -p "$d/src"; : > "$d/src/lib.rs"; done; \
    for p in allod allod-devtools allod-gossip; do printf 'fn main() {}\n' > "crates/$p/src/main.rs"; done; \
    cargo-sccache.sh build --profile "$CARGO_PROFILE" --locked -p allod -p allod-devtools -p allod-gossip \
 # Drop the workspace members' fingerprints, so their stub artifacts can never satisfy a
 # later build of the real sources.
 && rm -rf "target/$CARGO_PROFILE/.fingerprint/"allod* \
 # Park the compiled dependencies outside /src/target but still inside this layer: the
 # layer rides the GitHub Actions cache (deploy/compose.ci.yaml), and it seeds a cold
 # target cache mount in the build step below.
 && mv target /opt/target-deps

# Real sources replace the stubs. target/ is a BuildKit cache mount: it persists on the
# builder across builds, so cargo recompiles only the crates whose files changed (and
# their dependents), like a local incremental build. A cold mount (fresh builder, CI
# runner) is seeded from the dependency layer instead of recompiling third-party crates.
# Each profile builds into its own target/<profile>/, so both share the mount safely.
RUN rm -rf crates
COPY crates crates
COPY --from=web /src/web/dist web/dist
RUN --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=secret,id=ACTIONS_RESULTS_URL,env=ACTIONS_RESULTS_URL \
    --mount=type=secret,id=ACTIONS_RUNTIME_TOKEN,env=ACTIONS_RUNTIME_TOKEN \
    set -eu; \
    [ -d "target/$CARGO_PROFILE/.fingerprint" ] || { mkdir -p target; cp -a /opt/target-deps/. target/; }; \
    cargo-sccache.sh build --profile "$CARGO_PROFILE" --locked -p allod -p allod-devtools -p allod-gossip \
 && install -m 0755 "target/$CARGO_PROFILE/allod" "target/$CARGO_PROFILE/allod-devtools" "target/$CARGO_PROFILE/allod-gossip" /usr/local/bin/ \
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

FROM ${DEBIAN_IMAGE} AS gossip
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd -g 1000 allod && useradd -u 1000 -g 1000 -M -s /usr/sbin/nologin allod \
 && mkdir -p /data && chown 1000:1000 /data
COPY --from=build /usr/local/bin/allod-gossip /usr/local/bin/
USER 1000:1000
ENV ALLOD_GOSSIP_DATA_DIR=/data
EXPOSE 8090
VOLUME ["/data"]
HEALTHCHECK --interval=15s --timeout=10s --start-period=20s --retries=5 \
  CMD ["curl", "-fsS", "http://127.0.0.1:8090/health"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/allod-gossip"]

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
