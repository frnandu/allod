# syntax=docker/dockerfile:1.7
# Tor: onion service for the web UI and Lightning P2P, plus a SOCKS proxy for
# ldk-server's outbound connections to .onion peers (SPEC §2.3.1). /control is shared
# with Allod (uid/gid 1000, group-writable): the node's onion service on demand.
ARG DEBIAN_IMAGE=debian:bookworm-20261020-slim
FROM ${DEBIAN_IMAGE}
RUN apt-get update \
 && apt-get install -y --no-install-recommends tor bash tini \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /var/lib/tor/services && chown -R debian-tor:debian-tor /var/lib/tor \
 && chmod 0700 /var/lib/tor \
 && groupadd -g 1000 allod && usermod -aG allod debian-tor \
 && mkdir -p /control && chown 1000:1000 /control && chmod 0775 /control
COPY images/tor/tor-entrypoint.sh /usr/local/bin/
USER debian-tor
VOLUME ["/var/lib/tor"]
EXPOSE 9050
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/tor-entrypoint.sh"]
