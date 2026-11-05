#!/usr/bin/env bash
# Sync status for Allod without RPC (SPEC §2.3.4). While bitcoind connects blocks it
# holds its chain lock nearly all the time, so on a small machine every RPC that reports
# the height (getblockchaininfo, getblockcount, even waitfornewblock) can wait minutes.
# bitcoind logs each new tip without that lock: this follows the log and writes
# $CONTROL/sync-status.json (height, progress, when) at most every 2 s.
#
# With fast sync, two chainstates log tips: the snapshot chain (the one that matters)
# and background validation from genesis, far below it. Heights more than 100 under the
# highest tip seen are reported as background_height.
#
# Usage: sync-status.sh [follow|once]   (once: read the log as it is and exit; tests)
set -uo pipefail
LOG=${SYNC_LOG:-/data/debug.log}
CONTROL=${SYNC_CONTROL:-/control}
OUT=$CONTROL/sync-status.json
MODE=${1:-follow}

tip=0 progress=0 background=0 pending=0 last_write=0

write_status() {
  local now=$1
  printf '{"height":%s,"progress":%s,"background_height":%s,"at":%s}\n' \
    "$tip" "$progress" "$background" "$now" >"$OUT.tmp" && mv -f "$OUT.tmp" "$OUT"
  last_write=$now
  pending=0
}

# One log line; updates tip/progress/background.
take() {
  [[ $1 =~ UpdateTip:.*\ height=([0-9]+)\ .*\ progress=([0-9.]+) ]] || return 1
  local h=${BASH_REMATCH[1]} p=${BASH_REMATCH[2]}
  if ((h + 100 < tip)); then
    background=$h
  else
    tip=$h
    progress=$p
  fi
  pending=1
}

if [ "$MODE" = once ]; then
  while IFS= read -r line; do take "$line"; done < <(grep 'UpdateTip:' "$LOG" 2>/dev/null)
  ((pending)) && write_status "$(printf '%(%s)T' -1)"
  exit 0
fi

# Recent lines first, so a restart reports at once; then everything new. -F follows the
# file across bitcoind's restarts (it may shrink debug.log at startup).
while true; do
  if IFS= read -r -t 3 line; then
    take "$line" || continue
  elif (($? <= 128)); then
    break # the log follower ended
  fi
  now=$(printf '%(%s)T' -1)
  if ((pending && now - last_write >= 2)); then write_status "$now"; fi
done < <(tail -n 500 -F "$LOG" 2>/dev/null | grep --line-buffered 'UpdateTip:')
