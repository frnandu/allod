#!/usr/bin/env bash
set -euo pipefail

ROOT=$(mktemp -d)
declare -a pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill -TERM "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  rm -rf "$ROOT"
}
trap cleanup EXIT

mkdir -p "$ROOT/replica" "$ROOT/a-data" "$ROOT/a-handoff" "$ROOT/b-data" "$ROOT/b-handoff"
cat >"$ROOT/child.sh" <<'EOF'
#!/usr/bin/env bash
trap 'printf stopped >"$STOPPED"; exit 0' TERM
printf started >"$STARTED"
while :; do sleep 1; done
EOF
chmod +x "$ROOT/child.sh"

run_fence() {
  local instance=$1 data=$2 handoff=$3 started=$4 stopped=$5
  LITESTREAM_REPLICA_URL="file://$ROOT/replica" ALLOD_INSTANCE_ID="$instance" \
    LDK_DATA_DIR="$data" LDK_HANDOFF_DIR="$handoff" LDK_RESTORE_FENCE_POLL_SECS=1 \
    STARTED="$started" STOPPED="$stopped" \
    images/ldk-server/ldk-restore-fence.sh "$ROOT/child.sh" &
  LAST_PID=$!
  pids+=("$LAST_PID")
}

wait_file() {
  local file=$1
  for _ in $(seq 1 100); do [ -e "$file" ] && return; sleep 0.05; done
  echo "timed out waiting for $file" >&2; exit 1
}

# First owner starts and records epoch zero.
run_fence host-a "$ROOT/a-data" "$ROOT/a-handoff" "$ROOT/a.started" "$ROOT/a.stopped"
owner=$LAST_PID
wait_file "$ROOT/a.started"
[ "$(jq -r .epoch "$ROOT/replica/restore-fence.json")" = 0 ]

# Stale clone cannot start while another replica owner exists.
if LITESTREAM_REPLICA_URL="file://$ROOT/replica" ALLOD_INSTANCE_ID=host-b \
  LDK_DATA_DIR="$ROOT/b-data" LDK_HANDOFF_DIR="$ROOT/b-handoff" \
  images/ldk-server/ldk-restore-fence.sh "$ROOT/child.sh" 2>"$ROOT/stale.err"; then
  echo "stale replica unexpectedly started" >&2; exit 1
fi
grep -q 'replica has an active owner' "$ROOT/stale.err"
kill -0 "$owner"

# Verified latest restore atomically advances epoch. Old owner stops before new child starts.
printf '%s\n' latest-replica-confirmed >"$ROOT/b-handoff/restore-request"
run_fence host-b "$ROOT/b-data" "$ROOT/b-handoff" "$ROOT/b.started" "$ROOT/b.stopped"
restored=$LAST_PID
wait_file "$ROOT/a.stopped"
wait_file "$ROOT/b.started"
[ "$(jq -r .epoch "$ROOT/replica/restore-fence.json")" = 1 ]
[ ! -e "$ROOT/b-handoff/restore-request" ]
kill -0 "$restored"

# Clean shutdown releases ownership; same host and epoch can restart without takeover.
kill -TERM "$restored"
wait "$restored" || true
run_fence host-b "$ROOT/b-data" "$ROOT/b-handoff" "$ROOT/restart.started" "$ROOT/restart.stopped"
restarted=$LAST_PID
wait_file "$ROOT/restart.started"
kill -0 "$restarted"

echo "restore fence tests passed"
