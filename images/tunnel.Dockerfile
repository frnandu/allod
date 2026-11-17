# syntax=docker/dockerfile:1.7
# Home end of the gateway tunnels (SPEC §12.7): WireGuard to each gateway, and tcp/443
# from the tunnel to Caddy. Needs NET_ADMIN; the kernel's WireGuard module does the rest.
ARG ALPINE_IMAGE=alpine:3.20
FROM ${ALPINE_IMAGE}
RUN apk add --no-cache wireguard-tools-wg iproute2 socat tini busybox-extras
COPY images/tunnel/tunnel.sh /usr/local/bin/allod-tunnel
VOLUME ["/tunnel"]
ENTRYPOINT ["/sbin/tini", "--", "/usr/local/bin/allod-tunnel"]
