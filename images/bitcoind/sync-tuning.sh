#!/usr/bin/env bash
# Initial sync tuning (SPEC §2.3.2). While bitcoind is in its first initial block
# download, bitcoin.conf gets a sync profile sized to available memory: dbcache (the
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

# Headroom in MiB after reserving memory for the host and bitcoind's other uses.
# /proc/meminfo inside a container reports host memory; account for both it and the
# cgroup's remaining allowance. Unknown memory is treated as zero headroom.
mem_mib() {
  local total available reserve headroom limit used cgroup_headroom
  total=$(awk '/^MemTotal:/ {printf "%d", $2 / 1024}' "$MEMINFO" 2>/dev/null)
  available=$(awk '/^MemAvailable:/ {printf "%d", $2 / 1024}' "$MEMINFO" 2>/dev/null)
  total=${total:-0}
  available=${available:-0}
  reserve=$((total / 4))
  [ "$reserve" -lt 512 ] && reserve=512
  [ "$reserve" -gt 4096 ] && reserve=4096
  headroom=$((available - reserve))
  [ "$headroom" -lt 0 ] && headroom=0
  limit=""
  used=0
  if [ -r "$CGROUP/memory.max" ]; then
    limit=$(cat "$CGROUP/memory.max")
    [ -r "$CGROUP/memory.current" ] && used=$(cat "$CGROUP/memory.current")
  elif [ -r "$CGROUP/memory/memory.limit_in_bytes" ]; then
    limit=$(cat "$CGROUP/memory/memory.limit_in_bytes")
    [ -r "$CGROUP/memory/memory.usage_in_bytes" ] && used=$(cat "$CGROUP/memory/memory.usage_in_bytes")
  fi
  if [[ "$limit" =~ ^[0-9]+$ ]] && [[ "$used" =~ ^[0-9]+$ ]]; then
    limit=$((limit / 1024 / 1024))
    used=$((used / 1024 / 1024))
    # Cgroup v1 uses a huge sentinel when there is no real memory limit.
    if [ "$limit" -lt 1048576 ]; then
      cgroup_headroom=$((limit - used - 768))
      [ "$cgroup_headroom" -lt 0 ] && cgroup_headroom=0
      [ "$cgroup_headroom" -lt "$headroom" ] && headroom=$cgroup_headroom
    fi
  fi
  echo "$headroom"
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

# dbcache (MiB): at most half of current headroom, capped at 2 GiB. The
# 64 MiB floor lets bitcoind start under memory pressure while the container's
# hard limit prevents it from swapping.
sync_dbcache() {
  local mem=$1 v
  v=$((mem / 2))
  [ "$v" -gt 2048 ] && v=2048
  [ "$v" -lt 64 ] && v=64
  echo "$v"
}

steady_dbcache() {
  local requested=$1 cap
  cap=$(sync_dbcache "$(mem_mib)")
  [ "$requested" -gt "$cap" ] && requested=$cap
  echo "$requested"
}

# Sync profile lines for bitcoin.conf, or nothing when tuning is off or done.
# Settings the operator put in BITCOIN_EXTRA_CONF are left alone.
sync_profile() {
  local chain=$1 extra=${2:-} mem par
  [ "${BITCOIN_SYNC_TUNING:-auto}" = auto ] || return 0
  [ "$chain" = regtest ] && return 0
  [ -f "$SYNC_DONE" ] && return 0
  mem=$(mem_mib)
  printf '%s' "$extra" | grep -q '^dbcache=' || echo "dbcache=$(sync_dbcache "$mem")"
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
