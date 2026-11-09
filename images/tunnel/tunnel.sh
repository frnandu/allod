#!/bin/sh
# Home end of the gateway tunnels (SPEC §12.7). Allod writes the WireGuard
# configuration to $TUNNEL_DIR/wg0.conf; this applies it to wg0 (one address and one
# peer per gateway), forwards tcp/443 arriving through the tunnel to Caddy, and writes
# $TUNNEL_DIR/status.json (latest handshake per peer) every 10 seconds. It also serves
# $TUNNEL_DIR/routes/<n> (gateway n's domains) on port 8089 for shared gateways.
set -u
DIR=${TUNNEL_DIR:-/tunnel}
TARGET=${TUNNEL_TARGET:-caddy:443}
IF=wg0

# Allod (group 1000) writes the configuration here.
chgrp 1000 "$DIR" 2>/dev/null || true
chmod 0770 "$DIR" 2>/dev/null || true

socat TCP-LISTEN:443,fork,reuseaddr "TCP:$TARGET" &
# Each gateway's domain list (written by Allod), fetched by shared gateways over the
# tunnel to keep their routes.
mkdir -p "$DIR/routes"
httpd -f -p "${ROUTES_PORT:-8089}" -h "$DIR/routes" &

applied=""
while :; do
  conf="$DIR/wg0.conf"
  if [ -s "$conf" ]; then
    sum=$(md5sum "$conf" | cut -d' ' -f1)
    if [ "$sum" != "$applied" ]; then
      ip link show "$IF" >/dev/null 2>&1 || ip link add "$IF" type wireguard
      # `wg` takes only keys and peers; addresses are set with ip(8).
      grep -v '^Address' "$conf" >/tmp/wg.conf
      if wg syncconf "$IF" /tmp/wg.conf; then
        ip -4 addr flush dev "$IF"
        for a in $(sed -n 's/^Address *= *//p' "$conf" | tr ',' ' '); do
          ip addr add "$a" dev "$IF"
        done
        ip link set "$IF" up
        applied=$sum
        echo "tunnel: configuration applied ($(grep -c '^\[Peer\]' "$conf") gateways)"
      else
        echo "tunnel: wg rejected the configuration" >&2
      fi
    fi
  fi
  if ip link show "$IF" >/dev/null 2>&1; then
    peers=$(wg show "$IF" dump | awk 'NR > 1 {
      printf "%s{\"public_key\":\"%s\",\"latest_handshake\":%s,\"rx\":%s,\"tx\":%s}", (n++ ? "," : ""), $1, $5, $6, $7
    }')
    printf '{"updated_at":%s,"peers":[%s]}\n' "$(date +%s)" "$peers" >"$DIR/.status.json" &&
      mv "$DIR/.status.json" "$DIR/status.json"
  fi
  sleep 10
done
