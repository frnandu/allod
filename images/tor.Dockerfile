# syntax=docker/dockerfile:1.7
# Tor: onion service for the web UI and Lightning P2P, plus a SOCKS proxy for
# ldk-server's outbound connections to .onion peers (SPEC §2.3.1).
ARG DEBIAN_IMAGE=debian:bookworm-20261020-slim
FROM ${DEBIAN_IMAGE}
RUN apt-get update \
 && apt-get install -y --no-install-recommends tor bash tini \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /var/lib/tor/services && chown -R debian-tor:debian-tor /var/lib/tor \
 && chmod 0700 /var/lib/tor
COPY images/tor/tor-entrypoint.sh /usr/local/bin/
USER debian-tor
VOLUME ["/var/lib/tor"]
EXPOSE 9050
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/tor-entrypoint.sh"]
