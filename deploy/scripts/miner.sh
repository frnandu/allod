#!/usr/bin/env bash
# Mines one regtest block every MINE_INTERVAL seconds (0 = off).
set -euo pipefail
# shellcheck source-path=SCRIPTDIR
source "$(dirname "$0")/lib.sh"
interval=${MINE_INTERVAL:-10}
if [ "$interval" = 0 ]; then log "background mining off"; exec sleep infinity; fi
log "mining one block every ${interval}s"
while true; do
  mine 1 || log "mining failed (bitcoind restarting?)"
  sleep "$interval"
done
