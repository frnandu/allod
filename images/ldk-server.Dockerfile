# syntax=docker/dockerfile:1.7
# ldk-server built from a pinned upstream revision.
# Named context "ldk-src" = ldk-server source tree (compose: additional_contexts;
# default is the pinned git URL, override with LDK_SERVER_SRC=/path/to/checkout).
# Build context: repository root (for the entrypoint scripts).

ARG RUST_IMAGE=rust:1.95.0-bookworm
ARG DEBIAN_IMAGE=debian:bookworm-20261020-slim

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY --from=ldk-src . .
# No cache mount on target: the compiled artifacts must live in the layer, otherwise the
# GitHub Actions cache (deploy/compose.ci.yaml) restores an empty target/ and rebuilds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked -p ldk-server -p ldk-server-cli \
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
