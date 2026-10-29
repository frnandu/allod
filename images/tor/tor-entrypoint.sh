#!/usr/bin/env bash
# Writes torrc. Onion targets must be IP addresses, so service names are resolved here.
set -euo pipefail
log() { echo "[tor-entrypoint] $*" >&2; }

resolve() { # host:port -> ip:port
  local host=${1%:*} port=${1##*:} ip
  for _ in $(seq 1 60); do
    ip=$(getent hosts "$host" | awk '{print $1; exit}') && [ -n "$ip" ] && { echo "$ip:$port"; return; }
    sleep 2
  done
  log "cannot resolve $host"; return 1
}

TORRC=/tmp/torrc
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
} >"$TORRC"
mkdir -p /var/lib/tor/data /var/lib/tor/services && chmod 0700 /var/lib/tor/data /var/lib/tor/services

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

exec tor -f "$TORRC"
