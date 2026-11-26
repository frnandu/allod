#!/usr/bin/env bash
# Runs Allod on the host (cargo, debug build) and the web UI with live reload (Vite),
# against the running deploy/compose.yaml stack (ldk-server, bitcoind, gossip stay in
# Docker). The Allod container is stopped meanwhile: two Allods must never share one
# database and node.
#
#   scripts/host-dev.sh        (or: just host-dev)
#   open http://127.0.0.1:5173, edit web/src, the page updates; Ctrl-C to stop.
#
# Ctrl-C (or any exit) undoes everything: the volumes get their container owner back,
# the port proxy is removed and the Allod container starts again.
#
# Rootless Docker maps the containers' uid 1000 to a subordinate uid on the host, so the
# host user cannot write Allod's data volume. While this runs, the volumes belong to the
# host user (uid 0 inside rootless Docker) with group 1000 read-write, so the containers
# that share handoff/control keep their access.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE=(docker compose -f "$ROOT/deploy/compose.yaml" --project-directory "$ROOT/deploy")
PROJECT=allod
PROXY="$PROJECT-hostdev-proxy"
# Host ports for the proxy (chosen not to clash with the stack's own published ports).
LDK_PORT=13536 BTC_PORT=18332 GOSSIP_PORT=18090 API_PORT=18029 BRIDGE_PORT=18030 TOR_PORT=19050
# Caddy reaches Allod as `allod:8029` (HTTPS and its certificate check). Rootless
# containers cannot reach the host's loopback, so a forwarder listens on a host-only
# address they can reach: libvirt's bridge by default (nothing outside this machine
# reaches it). Set HOST_DEV_BRIDGE_IP to use another one.
BRIDGE_IP=${HOST_DEV_BRIDGE_IP:-$(ip -4 -o addr show virbr0 2>/dev/null | awk '{print $4}' | cut -d/ -f1)}

vol() { docker volume inspect -f '{{.Mountpoint}}' "${PROJECT}_$1"; }

container=$("${COMPOSE[@]}" ps -a -q allod)
[ -n "$container" ] || { echo "no allod container in the $PROJECT stack: start the stack first" >&2; exit 1; }
if (ss -ltn 2>/dev/null || netstat -ltn) | grep -q "127\.0\.0\.1:$API_PORT "; then
  echo "127.0.0.1:$API_PORT is in use: is another host-dev still running?" >&2
  exit 1
fi

# The container's environment, minus what changes on the host.
mapfile -t ENV < <(docker inspect "$container" --format '{{range .Config.Env}}{{println .}}{{end}}' |
  grep -E '^ALLOD_' | grep -vE '^ALLOD_(LISTEN|DATA_DIR|HANDOFF_DIR|CONTROL_DIR|LDK_SERVER_ADDRESS|BITCOIND_RPC_URL|GOSSIP_SIDECAR_URL|TLS_CHECK_ADDR|TUNNEL_DIR|TOR_DIR|TOR_PROXY)=')
network=$(docker inspect "$container" --format '{{range $k, $v := .NetworkSettings.Networks}}{{$k}}{{end}}')

# chown/chmod the volumes inside rootless Docker (root there = the host user).
fix_volumes() { # $1 = owner (0: the host user, 1000: the containers)
  local mode=g+rwX
  [ "$1" = 1000 ] && mode=g-w
  docker volume create "${PROJECT}_tunnel" >/dev/null
  docker volume create "${PROJECT}_tor-control" >/dev/null
  # tor-control stays group-writable either way: tor-proxy (group 1000) writes the node's
  # onion host name there.
  docker run --rm -v "${PROJECT}_allod-data:/d" -v "${PROJECT}_handoff:/h" -v "${PROJECT}_bitcoind-control:/c" \
    -v "${PROJECT}_tunnel:/t" -v "${PROJECT}_tor-control:/o" \
    alpine:3.20 sh -c "chown -R $1:1000 /d /h /c /t /o && chmod -R $mode /h /c /t && chmod -R g+rwX /o \
      && chmod g+s /h && find /h /c /t /o -type f -exec chmod g-x {} + 2>/dev/null; true"
}

# Stops a background job started with setsid, with everything it started.
stop_group() {
  [ -n "${1:-}" ] || return 0
  kill -TERM -- "-$1" 2>/dev/null || return 0
  for _ in $(seq 1 20); do kill -0 -- "-$1" 2>/dev/null || return 0; sleep 0.5; done
  kill -KILL -- "-$1" 2>/dev/null
}

cleanup() {
  set +e
  echo
  echo "host-dev: restoring the stack…"
  # Allod first, then the volumes back to the containers: if this is cut short (a
  # SIGKILL soon after the TERM), the container can still read its files.
  stop_group "${ALLOD_PID:-}"
  fix_volumes 1000
  # The proxy holds the `allod` alias: gone before the container takes it back.
  docker rm -f "$PROXY" >/dev/null 2>&1
  "${COMPOSE[@]}" start allod >/dev/null && echo "host-dev: Allod container running again."
  stop_group "${VITE_PID:-}"
  stop_group "${BRIDGE_PID:-}"
}
trap cleanup EXIT INT TERM

echo "host-dev: stopping the Allod container…"
"${COMPOSE[@]}" stop allod >/dev/null
fix_volumes 0

echo "host-dev: forwarding ldk-server, bitcoind, gossip and tor-proxy to 127.0.0.1…"
docker rm -f "$PROXY" >/dev/null 2>&1 || true
if [ -n "$BRIDGE_IP" ]; then
  echo "host-dev: Caddy reaches this Allod as allod:8029 through $BRIDGE_IP:$BRIDGE_PORT"
  setsid socat "TCP-LISTEN:$BRIDGE_PORT,bind=$BRIDGE_IP,fork,reuseaddr" "TCP:127.0.0.1:$API_PORT" &
  BRIDGE_PID=$!
else
  echo "host-dev: no host-only address for Caddy (set HOST_DEV_BRIDGE_IP): public HTTPS will not reach this Allod" >&2
fi
# The proxy also stands in for the Allod container on the stack network (alias allod).
docker run -d --name "$PROXY" --network "$network" --network-alias allod \
  -p "127.0.0.1:$LDK_PORT:3536" -p "127.0.0.1:$BTC_PORT:8332" -p "127.0.0.1:$GOSSIP_PORT:8090" \
  -p "127.0.0.1:$TOR_PORT:9050" \
  -e "BRIDGE=${BRIDGE_IP:+$BRIDGE_IP:$BRIDGE_PORT}" \
  alpine:3.20 sh -c 'apk add --no-cache socat >/dev/null || exit 1
    socat TCP-LISTEN:3536,fork,reuseaddr TCP:ldk-server:3536 &
    socat TCP-LISTEN:8332,fork,reuseaddr TCP:bitcoind:8332 &
    socat TCP-LISTEN:8090,fork,reuseaddr TCP:gossip:8090 &
    socat TCP-LISTEN:9050,fork,reuseaddr TCP:tor-proxy:9050 &
    if [ -n "$BRIDGE" ]; then socat TCP-LISTEN:8029,fork,reuseaddr "TCP:$BRIDGE" & fi
    wait' >/dev/null
for _ in $(seq 1 30); do
  docker exec "$PROXY" sh -c 'pgrep socat >/dev/null' 2>/dev/null && break
  sleep 1
done

echo "host-dev: building and starting Allod (debug)…"
# shellcheck disable=SC2016 # expanded by the inner bash
setsid bash -c 'cd "$1" && shift && exec "$@"' _ "$ROOT" \
  env "${ENV[@]}" \
    ALLOD_LISTEN="127.0.0.1:$API_PORT" \
    ALLOD_DATA_DIR="$(vol allod-data)" \
    ALLOD_HANDOFF_DIR="$(vol handoff)" \
    ALLOD_CONTROL_DIR="$(vol bitcoind-control)" \
    ALLOD_TUNNEL_DIR="$(vol tunnel)" \
    ALLOD_TOR_DIR="$(vol tor-control)" \
    ALLOD_TOR_PROXY="127.0.0.1:$TOR_PORT" \
    ALLOD_LDK_SERVER_ADDRESS="127.0.0.1:$LDK_PORT" \
    ALLOD_BITCOIND_RPC_URL="http://127.0.0.1:$BTC_PORT" \
    ALLOD_GOSSIP_SIDECAR_URL="http://127.0.0.1:$GOSSIP_PORT/status" \
    ALLOD_TLS_CHECK_ADDR= \
    ALLOD_LOG="${ALLOD_LOG:-info,allod=debug}" \
    cargo run -p allod -- serve &
ALLOD_PID=$!

echo "host-dev: starting the web UI with live reload on http://127.0.0.1:5173 …"
# shellcheck disable=SC2016 # expanded by the inner bash
setsid bash -c 'cd "$1/web" && corepack pnpm install --frozen-lockfile >/dev/null && ALLOD_API_URL="$2" exec corepack pnpm dev' \
  _ "$ROOT" "http://127.0.0.1:$API_PORT" &
VITE_PID=$!

wait -n "$ALLOD_PID" "$VITE_PID"
