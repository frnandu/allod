#!/usr/bin/env bash
# Tests the fast sync download against a flaky local server: the first connection stalls
# after 1 MiB (like a network that went down), the second is cut mid-way, the rest work.
# The download must resume each time and end with an identical file.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'kill $server 2>/dev/null || true; rm -rf "$tmp"' EXIT
mkdir -p "$tmp/control" "$tmp/data" "$tmp/www"
head -c $((5 * 1024 * 1024)) /dev/urandom >"$tmp/www/snap.dat"
cat >"$tmp/server.py" <<'PY'
import http.server, os, sys, time
path, port = sys.argv[1], int(sys.argv[2])
data = open(path, "rb").read()
state = {"n": 0}
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_HEAD(self):
        size = 9387990000 if self.path == "/big" else len(data)  # > 2^31: no 32-bit overflow
        self.send_response(200); self.send_header("Content-Length", str(size)); self.end_headers()
    def do_GET(self):
        state["n"] += 1
        start = 0
        r = self.headers.get("Range")
        if r and r.startswith("bytes="):
            start = int(r[6:].split("-")[0])
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{len(data)-1}/{len(data)}")
        else:
            self.send_response(200)
        self.send_header("Content-Length", str(len(data) - start)); self.end_headers()
        body = data[start:]
        if state["n"] == 1:      # stall after 1 MiB: connection stays open, no data
            self.wfile.write(body[:1 << 20]); self.wfile.flush(); time.sleep(3600)
        elif state["n"] == 2:    # cut the connection half way
            self.wfile.write(body[: len(body) // 2]); self.wfile.flush(); self.connection.close()
        else:
            self.wfile.write(body)
http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
PY
port=$((20000 + RANDOM % 20000))
python3 "$tmp/server.py" "$tmp/www/snap.dat" "$port" &
server=$!
sleep 1
export FASTSYNC_LIB=1 FASTSYNC_CTRL="$tmp/control" FASTSYNC_DATA="$tmp/data" \
  FASTSYNC_STALL_SECS=3 FASTSYNC_STALL_BYTES=1000 FASTSYNC_WATCHDOG_SECS=6 FASTSYNC_TICK=1
# shellcheck source=assumeutxo.sh
source "$here/assumeutxo.sh"
sleep() { command sleep 1; } # skip the retry backoff in the test
big=$(remote_size "http://127.0.0.1:$port/big")
[ "$big" = 9387990000 ] || { echo "FAIL: remote_size gave $big for a 9.4 GB file"; exit 1; }
echo '{"url":"x"}' >"$REQ"
url="http://127.0.0.1:$port/snap.dat"
for attempt in 1 2 3 4 5 6; do
  if download "$url"; then break; fi
  echo "attempt $attempt failed: $(cat "$ST")"
done
cmp "$tmp/www/snap.dat" "$SNAP" && echo "PASS: resumed after a stall and a dropped connection"

# Loading progress from bitcoind's log lines (only lines after the load started count).
log="$tmp/debug.log"
printf '%s\n' '2026-10-28T17:00:00Z [snapshot] 9 coins loaded (99.00%, 1 MB)' >"$log"
from=2
printf '%s\n' '2026-10-28T17:35:46Z [snapshot] 61000000 coins loaded (37.14%, 490 MB)' >>"$log"
[ "$(snapshot_progress "$log" "$from")" = '"phase":"coins","pct":37.14,"coins":61000000' ] || { echo "FAIL: coins progress"; exit 1; }
printf '%s\n' '2026-10-28T17:57:26Z [snapshot] loaded 164241311 (2828 MB) coins from snapshot 00ab' >>"$log"
[ "$(snapshot_progress "$log" "$from")" = '"phase":"writing"' ] || { echo "FAIL: writing phase"; exit 1; }
printf '%s\n' '2026-10-28T17:57:26Z FlushSnapshotToDisk: saving snapshot chainstate (2828 MB) started' \
  '2026-10-28T17:57:26Z [warning] Flushing large (20241311 entries) UTXO set to disk, it may take several minutes' >>"$log"
[ "$(snapshot_progress "$log" "$from")" = '"phase":"writing"' ] || { echo "FAIL: flush phase"; exit 1; }
printf '%s\n' '2026-10-28T17:57:38Z FlushSnapshotToDisk: completed (11558.97ms)' >>"$log"
[ "$(snapshot_progress "$log" "$from")" = '"phase":"checking"' ] || { echo "FAIL: checking phase"; exit 1; }
[ "$(snapshot_progress "$tmp/none" 1)" = '"phase":"starting"' ] || { echo "FAIL: no log"; exit 1; }
echo "PASS: snapshot loading progress"

# Only errors about the snapshot itself delete it.
snapshot_is_bad "Unable to load UTXO snapshot: Population failed: Aborting after an interrupt was requested." \
  && { echo "FAIL: an interrupt deleted the snapshot"; exit 1; }
snapshot_is_bad "error: Could not connect to the server 127.0.0.1:8332" && { echo "FAIL: lost RPC"; exit 1; }
snapshot_is_bad "Unable to load UTXO snapshot: assumeutxo block hash in snapshot metadata not recognized (00ab)" \
  || { echo "FAIL: unknown snapshot kept"; exit 1; }
snapshot_is_bad "Unable to load UTXO snapshot: Bad snapshot content hash: expected 1, got 2" || { echo "FAIL: bad hash kept"; exit 1; }
echo "PASS: an interrupted load keeps the snapshot"
