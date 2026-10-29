# syntax=docker/dockerfile:1.7
# Bitcoin Core from the official release tarball, verified against pinned SHA-256 sums.
ARG DEBIAN_IMAGE=debian:bookworm-20261020-slim

FROM ${DEBIAN_IMAGE} AS download
ARG BITCOIN_VERSION=31.1
ARG BITCOIN_SHA256_AMD64=b80d9c3e04da78fb6f0569685673418cf686fadba9042d926d13fb87ff503f9e
ARG BITCOIN_SHA256_ARM64=dcf1873f2208ba4f962f3398d47e154c39c0084be8f4553e05c940d0ace3d004
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) arch=x86_64-linux-gnu; sum=$BITCOIN_SHA256_AMD64 ;; \
      arm64) arch=aarch64-linux-gnu; sum=$BITCOIN_SHA256_ARM64 ;; \
      *) echo "unsupported arch $TARGETARCH"; exit 1 ;; \
    esac; \
    file="bitcoin-${BITCOIN_VERSION}-${arch}.tar.gz"; \
    curl -fsSLo "/tmp/$file" "https://bitcoincore.org/bin/bitcoin-core-${BITCOIN_VERSION}/$file"; \
    echo "$sum  /tmp/$file" | sha256sum -c -; \
    tar -xzf "/tmp/$file" -C /tmp; \
    install -m 0755 /tmp/bitcoin-${BITCOIN_VERSION}/bin/bitcoind /tmp/bitcoin-${BITCOIN_VERSION}/bin/bitcoin-cli /usr/local/bin/

FROM ${DEBIAN_IMAGE}
RUN apt-get update \
 && apt-get install -y --no-install-recommends bash openssl tini curl ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd -g 1000 allod && useradd -u 1000 -g 1000 -M -d /data -s /usr/sbin/nologin allod \
 && mkdir -p /data /control && chown 1000:1000 /data /control
COPY --from=download /usr/local/bin/bitcoind /usr/local/bin/bitcoin-cli /usr/local/bin/
COPY images/bitcoind/bitcoind-entrypoint.sh images/bitcoind/assumeutxo.sh images/bitcoind/sync-tuning.sh /usr/local/bin/
USER 1000:1000
VOLUME ["/data"]
HEALTHCHECK --interval=15s --timeout=10s --start-period=30s --retries=5 \
  CMD ["bitcoin-cli", "-conf=/data/bitcoin.conf", "-datadir=/data", "getblockchaininfo"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/bitcoind-entrypoint.sh"]
