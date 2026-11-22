#!/usr/bin/env bash
# Writes torrc and runs tor. Onion targets must be IP addresses, so service names are
# resolved here.
#
# With TOR_CONTROL_DIR (the tor-proxy service, shared with Allod), the node's onion
# service follows Allod's Settings → Lightning → Connectivity: while
# $TOR_CONTROL_DIR/lightning exists, tor publishes an onion service for
# TOR_NODE_TARGET, and its host name goes to $TOR_CONTROL_DIR/lightning.onion. The key
# stays when the service is turned off, so the address is the same next time. torrc is
# re-rendered every few seconds (the target's IP can change when ldk-server is
# recreated), and tor reloads it on SIGHUP.
set -euo pipefail
log() { echo "[tor-entrypoint] $*" >&2; }

resolve() { # host:port [tries] -> ip:port
  local host=${1%:*} port=${1##*:} tries=${2:-60} ip
  for _ in $(seq 1 "$tries"); do
    ip=$(getent hosts "$host" | awk '{print $1; exit}') && [ -n "$ip" ] && { echo "$ip:$port"; return; }
    [ "$tries" -gt 1 ] && sleep 2
  done
  [ "$tries" -gt 1 ] && log "cannot resolve $host"
  return 1
}

CONTROL=${TOR_CONTROL_DIR:-}
NODE_TARGET=${TOR_NODE_TARGET:-ldk-server:9735}
NODE_DIR=/var/lib/tor/services/lightning-node
TORRC=/tmp/torrc

# The fixed part, resolved once.
STATIC=/tmp/torrc.static
{
  echo "DataDirectory /var/lib/tor/data"
  echo "SocksPort 0.0.0.0:9050"
  echo "SocksPolicy accept 10.0.0.0/8"
  echo "SocksPolicy accept 172.16.0.0/12"
  echo "SocksPolicy accept 192.168.0.0/16"
  echo "SocksPolicy reject *"
  echo "Log notice stdout"
  if [ "${TOR_WEB_TARGET:-allod:8029}" != none ]; then
    echo "HiddenServiceDir /var/lib/tor/services/allod"
    echo "HiddenServicePort 80 $(resolve "${TOR_WEB_TARGET:-allod:8029}")"
  fi
  if [ "${TOR_LIGHTNING_TARGET:-ldk-server:9735}" != none ]; then
    echo "HiddenServiceDir /var/lib/tor/services/lightning"
    echo "HiddenServicePort ${TOR_LIGHTNING_PORT:-9735} $(resolve "${TOR_LIGHTNING_TARGET:-ldk-server:9735}")"
  fi
} >"$STATIC"

node_ip="" # the last address NODE_TARGET resolved to, kept through a failed lookup
render() { # prints the full torrc (runs in this shell, so node_ip sticks)
  cat "$STATIC"
  if [ -n "$CONTROL" ] && [ -e "$CONTROL/lightning" ]; then
    local ip
    if ip=$(resolve "$NODE_TARGET" 1); then node_ip=$ip; fi
    if [ -n "$node_ip" ]; then
      echo "HiddenServiceDir $NODE_DIR"
      echo "HiddenServicePort ${NODE_TARGET##*:} $node_ip"
    fi
  fi
}

# Copies the node onion's host name for Allod, when it changed.
publish_failed=""
publish_onion() {
  [ -n "$CONTROL" ] && [ -s "$NODE_DIR/hostname" ] || return 0
  if ! cmp -s "$NODE_DIR/hostname" "$CONTROL/lightning.onion" 2>/dev/null; then
    # install, not cp: the key directory's files are 0600, Allod must read this one.
    if install -m 0644 "$NODE_DIR/hostname" "$CONTROL/lightning.onion.tmp" 2>/dev/null \
      && mv -f "$CONTROL/lightning.onion.tmp" "$CONTROL/lightning.onion"; then
      log "Lightning node onion: $(cat "$NODE_DIR/hostname"):${NODE_TARGET##*:}"
      publish_failed=""
    elif [ -z "$publish_failed" ]; then
      log "cannot write $CONTROL/lightning.onion ($(stat -c '%U:%G %a' "$CONTROL" 2>/dev/null)); retrying quietly"
      publish_failed=1
    fi
  fi
}

mkdir -p /var/lib/tor/data /var/lib/tor/services && chmod 0700 /var/lib/tor/data /var/lib/tor/services
render >"$TORRC"

if [ "${TOR_WEB_TARGET:-allod:8029}" != none ]; then
  ( for _ in $(seq 1 120); do
      if [ -s /var/lib/tor/services/allod/hostname ]; then
        log "web UI onion: http://$(cat /var/lib/tor/services/allod/hostname)"
        [ -s /var/lib/tor/services/lightning/hostname ] && \
          log "lightning onion: $(cat /var/lib/tor/services/lightning/hostname):${TOR_LIGHTNING_PORT:-9735}"
        break
      fi
      sleep 2
    done ) &
fi

[ -n "$CONTROL" ] || exec tor -f "$TORRC"

tor -f "$TORRC" &
tor_pid=$!
trap 'kill -TERM "$tor_pid" 2>/dev/null; wait "$tor_pid"; exit 0' TERM INT
while sleep 5; do
  kill -0 "$tor_pid" 2>/dev/null || { wait "$tor_pid"; exit $?; }
  render >"$TORRC.new"
  if ! cmp -s "$TORRC.new" "$TORRC"; then
    mv -f "$TORRC.new" "$TORRC"
    if [ -e "$CONTROL/lightning" ]; then log "Lightning node onion service on"; else log "Lightning node onion service off"; fi
    kill -HUP "$tor_pid"
  fi
  publish_onion
done
