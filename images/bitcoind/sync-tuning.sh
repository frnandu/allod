#!/usr/bin/env bash
# Initial sync tuning (SPEC §2.3.2). While bitcoind is in its first initial block
# download, bitcoin.conf gets a sync profile sized to the machine: a large dbcache (the
# setting that matters most for IBD) and blocksonly (no transaction relay). Both are read
# only at startup, so once IBD ends this watcher marks the sync as done and stops
# bitcoind; the container restarts and the entrypoint writes the normal profile
# (BITCOIN_DBCACHE, transaction relay on: ldk-server's fee estimates need the mempool).
# Sourced by the entrypoint and the test for its functions; run with "watch" as the
# background watcher.
set -uo pipefail
DATA=${SYNC_DATA:-/data}
SYNC_DONE=$DATA/.initial-sync-done
MEMINFO=${SYNC_MEMINFO:-/proc/meminfo}
CGROUP=${SYNC_CGROUP:-/sys/fs/cgroup}

# Usable memory in MiB: the machine's, or the container's limit if lower (cgroup v2 / v1).
mem_mib() {
  local total limit
  total=$(awk '/^MemTotal:/ {printf "%d", $2 / 1024}' "$MEMINFO" 2>/dev/null)
  total=${total:-0}
  limit=""
  if [ -r "$CGROUP/memory.max" ]; then
    limit=$(cat "$CGROUP/memory.max")
  elif [ -r "$CGROUP/memory/memory.limit_in_bytes" ]; then
    limit=$(cat "$CGROUP/memory/memory.limit_in_bytes")
  fi
  if [[ "$limit" =~ ^[0-9]+$ ]]; then
    limit=$((limit / 1024 / 1024))
    [ "$total" -eq 0 ] || [ "$limit" -lt "$total" ] && total=$limit
  fi
  echo "$total"
}

# CPUs the container may use, when a cgroup quota limits it below the visible cores;
# empty otherwise (bitcoind's own default fits the machine).
cpu_quota() {
  local q p n
  [ -r "$CGROUP/cpu.max" ] || return 0
  read -r q p <"$CGROUP/cpu.max"
  [[ "$q" =~ ^[0-9]+$ ]] || return 0
  n=$(( (q + p - 1) / p ))
  [ "$n" -lt "$(nproc)" ] && echo "$n"
  return 0
}

# dbcache (MiB) for the sync: 60% of memory above 2 GiB (left for the OS, ldk-server and
# Allod), between 450 MiB and 16 GiB, and never below the configured steady value.
sync_dbcache() {
  local mem=$1 steady=${2:-450} v
  v=$(( (mem - 2048) * 6 / 10 ))
  [ "$v" -lt 450 ] && v=450
  [ "$v" -gt 16384 ] && v=16384
  [ "$v" -lt "$steady" ] && v=$steady
  echo "$v"
}

# Sync profile lines for bitcoin.conf, or nothing when tuning is off or done.
# Settings the operator put in BITCOIN_EXTRA_CONF are left alone.
sync_profile() {
  local chain=$1 steady=$2 extra=${3:-} mem par
  [ "${BITCOIN_SYNC_TUNING:-auto}" = auto ] || return 0
  [ "$chain" = regtest ] && return 0
  [ -f "$SYNC_DONE" ] && return 0
  mem=$(mem_mib)
  printf '%s' "$extra" | grep -q '^dbcache=' || echo "dbcache=$(sync_dbcache "$mem" "$steady")"
  printf '%s' "$extra" | grep -q '^blocksonly=' || echo "blocksonly=1"
  par=$(cpu_quota)
  [ -n "$par" ] && ! printf '%s' "$extra" | grep -q '^par=' && echo "par=$par"
  return 0
}

watch_sync() {
  local cli=(bitcoin-cli -conf="$DATA/bitcoin.conf" -datadir="$DATA")
  while true; do
    sleep 60
    [ -f "$SYNC_DONE" ] && return 0
    if "${cli[@]}" getblockchaininfo 2>/dev/null | grep -q '"initialblockdownload": false'; then
      touch "$SYNC_DONE"
      echo "[sync-tuning] initial sync done: restarting bitcoind with the normal profile" >&2
      "${cli[@]}" stop >/dev/null 2>&1
      return 0
    fi
  done
}

[ "${1:-}" = watch ] && watch_sync
true
