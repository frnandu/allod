#!/usr/bin/env bash
# sync-status.sh against a sample debug.log (fast sync: snapshot and background chains).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*"; exit 1; }
export SYNC_LOG="$tmp/debug.log" SYNC_CONTROL="$tmp"
tip() { echo "2026-11-05T10:18:59Z UpdateTip: new best=00ab height=$1 version=0x2e000000 log2_work=96.07 tx=1306363663 date='2026-02-07T00:50:39Z' progress=$2 cache=232.3MiB(1717632txo)"; }
out() { cat "$tmp/sync-status.json"; }

{ tip 269635 0.020; tip 269636 0.021; echo "2026-11-05T10:19:00Z Flushing 2 GB"; } >"$SYNC_LOG"
"$here/sync-status.sh" once
[[ $(out) == '{"height":269636,"progress":0.021,"background_height":0,"at":'* ]] || fail "single chain: $(out)"

# Snapshot activated: its tip takes over; background validation goes on far below it.
{ tip 935001 0.9101; tip 269637 0.021; tip 935002 0.9102; tip 269638 0.021; } >>"$SYNC_LOG"
"$here/sync-status.sh" once
[[ $(out) == '{"height":935002,"progress":0.9102,"background_height":269638,"at":'* ]] || fail "snapshot: $(out)"

# A short reorg is still the main chain.
tip 935001 0.9101 >>"$SYNC_LOG"
"$here/sync-status.sh" once
[[ $(out) == '{"height":935001,'* ]] || fail "reorg: $(out)"

# Follow mode: picks up new tips as bitcoind logs them.
rm -f "$tmp/sync-status.json"
"$here/sync-status.sh" follow & pid=$!
trap 'kill $pid 2>/dev/null; rm -rf "$tmp"' EXIT
sleep 1
tip 935050 0.9150 >>"$SYNC_LOG"
for _ in $(seq 1 50); do grep -q '"height":935050' "$tmp/sync-status.json" 2>/dev/null && break; sleep 0.2; done
[[ $(out) == '{"height":935050,"progress":0.9150,"background_height":269638,"at":'* ]] || fail "follow: $(out)"
echo "PASS: sync status from the log"
