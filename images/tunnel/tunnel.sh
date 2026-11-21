#!/bin/sh
# Home end of the gateway tunnels (SPEC §12.7). Allod writes the WireGuard
# configuration to $TUNNEL_DIR/wg0.conf; this applies it to wg0 (one address and one
# peer per gateway), forwards tcp/443 arriving through the tunnel to Caddy, and writes
# $TUNNEL_DIR/status.json (latest handshake per peer) every 10 seconds. It also serves
# $TUNNEL_DIR/routes/<n> (gateway n's domains) on port 8089 for shared gateways, and
# routes/<n>.lightning (the public Lightning port gateway n passes on, if any). While
# Allod writes $TUNNEL_DIR/lightning, Lightning connections arriving through the tunnel
# on port 9735 go on to ldk-server (SPEC §12.7.2).
# A gateway that goes silent gets a new UDP source port (see STALE below).
set -u
DIR=${TUNNEL_DIR:-/tunnel}
TARGET=${TUNNEL_TARGET:-caddy:443}
IF=wg0
LN_PORT=${TUNNEL_LIGHTNING_PORT:-9735}
LN_TARGET=${TUNNEL_LIGHTNING_TARGET:-ldk-server:9735}
ln_pid=""

# Allod (group 1000) writes the configuration here.
chgrp 1000 "$DIR" 2>/dev/null || true
chmod 0770 "$DIR" 2>/dev/null || true

socat TCP-LISTEN:443,fork,reuseaddr "TCP:$TARGET" &
# Each gateway's domain list (written by Allod), fetched by shared gateways over the
# tunnel to keep their routes.
mkdir -p "$DIR/routes"
httpd -f -p "${ROUTES_PORT:-8089}" -h "$DIR/routes" &

# A gateway that stops answering while this end keeps sending: a NAT on the way (rootless
# Docker, the home router) holds a dead mapping for this UDP source port, and nothing
# comes back until it expires, which can take hours. A new source port gets a new
# mapping, so after STALE seconds without a handshake wg0 moves to another port. The
# wait doubles (up to 30 minutes) while a gateway stays silent: one that is really down
# must not keep moving the port under the others.
STALE=${TUNNEL_STALE_SECS:-180}
rotate_wait=$STALE
rotated_at=0

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
  # Lightning through a gateway: on while Allod asks for it.
  if [ -s "$DIR/lightning" ]; then
    if [ -z "$ln_pid" ] || ! kill -0 "$ln_pid" 2>/dev/null; then
      socat "TCP-LISTEN:$LN_PORT,fork,reuseaddr" "TCP:$LN_TARGET" &
      ln_pid=$!
      echo "tunnel: Lightning connections on port $LN_PORT go to $LN_TARGET"
    fi
  elif [ -n "$ln_pid" ]; then
    kill "$ln_pid" 2>/dev/null || true
    ln_pid=""
    echo "tunnel: Lightning forwarding off"
  fi
  if ip link show "$IF" >/dev/null 2>&1; then
    now=$(date +%s)
    # Gateways that answered before and no longer do (never answered: not set up yet).
    stale=$(wg show "$IF" latest-handshakes |
      awk -v t="$now" -v s="$STALE" '$2 > 0 && t - $2 > s { n++ } END { print n + 0 }')
    if [ "$stale" -eq 0 ]; then
      rotate_wait=$STALE
    elif [ $((now - rotated_at)) -ge "$rotate_wait" ]; then
      # Port 0: the kernel picks a free one.
      if wg set "$IF" listen-port 0; then
        echo "tunnel: $stale gateway(s) silent for over ${STALE}s: new source port $(wg show "$IF" listen-port)"
      fi
      rotated_at=$now
      rotate_wait=$((rotate_wait * 2))
      [ "$rotate_wait" -le 1800 ] || rotate_wait=1800
    fi
    peers=$(wg show "$IF" dump | awk 'NR > 1 {
      printf "%s{\"public_key\":\"%s\",\"latest_handshake\":%s,\"rx\":%s,\"tx\":%s}", (n++ ? "," : ""), $1, $5, $6, $7
    }')
    printf '{"updated_at":%s,"peers":[%s]}\n' "$(date +%s)" "$peers" >"$DIR/.status.json" &&
      mv "$DIR/.status.json" "$DIR/status.json"
  fi
  sleep 10
done
