# syntax=docker/dockerfile:1.10
# ldk-server built from a pinned upstream revision.
# The pin is the LDK_SERVER_REV default below. Every build reads it from here —
# Compose, the release workflow, and a plain `docker build` — so nothing else needs
# to repeat it (Cargo.toml carries the same commit for ldk-server-client).
# Build context: repository root (for the entrypoint scripts).

ARG RUST_IMAGE=rust:1.95.0-bookworm
ARG DEBIAN_IMAGE=debian:bookworm-20261020-slim
# Where to fetch the pinned revision from. Git accepts a local path as well as a URL, so
# LDK_SERVER_REPO=/path/to/ldk-server builds from a checkout when offline.
ARG LDK_SERVER_REPO=https://github.com/lightningdevkit/ldk-server.git
ARG LDK_SERVER_REV=590cdd9923f566a9b8dbe1cd1f34f2b0476cb2ba

FROM ${RUST_IMAGE} AS build
ARG LDK_SERVER_REPO
ARG LDK_SERVER_REV
WORKDIR /src
RUN git init -q . \
 && git remote add origin "${LDK_SERVER_REPO}" \
 && { git fetch -q --depth 1 origin "${LDK_SERVER_REV}" || git fetch -q origin; } \
 && git checkout -q --detach "${LDK_SERVER_REV}" \
 && rm -rf .git
# .git is gone, so ldk-server's build.rs cannot read HEAD. Give it the pin explicitly so
# `--version` and GetNodeInfo report "<cargo version> (<commit>)" instead of "(unknown)".
ENV GIT_HASH=${LDK_SERVER_REV}
# Cargo's built-in Git fetch fails to resolve the pinned rust-lightning commit from
# GitHub's mirror. Git CLI fetches that exact commit by SHA.
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ARG TARGETARCH
COPY images/install-sccache.sh images/cargo-sccache.sh /usr/local/bin/
# Optional: offline builds carry on without sccache (a bad checksum installs nothing).
RUN install-sccache.sh "$TARGETARCH" || echo "sccache not installed" >&2
# "release" (upstream's profile: fat LTO) for published images; "fast-release" for nightly
# images keeps the release profile but without LTO and with parallel codegen.
ARG CARGO_PROFILE=release
# No cache mount on target: the compiled artifacts must live in the layer, otherwise the
# GitHub Actions cache (deploy/compose.ci.yaml) restores an empty target/ and rebuilds.
# The optional ACTIONS_* secrets turn on sccache (images/cargo-sccache.sh).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=secret,id=ACTIONS_RESULTS_URL,env=ACTIONS_RESULTS_URL \
    --mount=type=secret,id=ACTIONS_RUNTIME_TOKEN,env=ACTIONS_RUNTIME_TOKEN \
    set -eu; \
    case "$CARGO_PROFILE" in \
      release) set -- ;; \
      fast-release) set -- --config profile.release.lto=false --config profile.release.codegen-units=16 ;; \
      *) echo "unsupported CARGO_PROFILE $CARGO_PROFILE" >&2; exit 1 ;; \
    esac; \
    cargo-sccache.sh build --release --locked "$@" -p ldk-server -p ldk-server-cli \
 && install -m 0755 target/release/ldk-server target/release/ldk-server-cli /usr/local/bin/

FROM ${DEBIAN_IMAGE}
RUN apt-get update \
 && apt-get install -y --no-install-recommends bash ca-certificates curl jq tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd -g 1000 allod && useradd -u 1000 -g 1000 -M -d /data -s /usr/sbin/nologin allod \
 && mkdir -p /data /handoff /export && chown 1000:1000 /data /handoff /export
COPY --from=build /usr/local/bin/ldk-server /usr/local/bin/ldk-server-cli /usr/local/bin/
COPY images/ldk-server/ldk-entrypoint.sh images/ldk-server/ldk-healthcheck.sh /usr/local/bin/
USER 1000:1000
VOLUME ["/data"]
EXPOSE 9735 3536
HEALTHCHECK --interval=15s --timeout=10s --start-period=30s --retries=5 \
  CMD ["/usr/local/bin/ldk-healthcheck.sh"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/ldk-entrypoint.sh"]
