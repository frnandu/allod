#!/usr/bin/env bash
# Sync profile sizing against fake /proc/meminfo and cgroup files.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/cg" "$tmp/data"
export SYNC_DATA="$tmp/data" SYNC_MEMINFO="$tmp/meminfo" SYNC_CGROUP="$tmp/cg"
# shellcheck source=sync-tuning.sh
source "$here/sync-tuning.sh"
fail() { echo "FAIL: $*"; exit 1; }
mem() { echo "MemTotal: $(($1 * 1024)) kB" >"$tmp/meminfo"; }

mem 32768; [ "$(mem_mib)" = 32768 ] || fail "host memory"
[ "$(sync_dbcache 32768 450)" = 16384 ] || fail "32 GB host: capped at 16 GiB"
[ "$(sync_dbcache 4096 450)" = 1228 ] || fail "4 GB VPS"
[ "$(sync_dbcache 2048 450)" = 450 ] || fail "2 GB box: floor"
[ "$(sync_dbcache 4096 8000)" = 8000 ] || fail "never below the configured value"
echo $((6 * 1024 * 1024 * 1024)) >"$tmp/cg/memory.max"; [ "$(mem_mib)" = 6144 ] || fail "container limit wins"
echo max >"$tmp/cg/memory.max"; [ "$(mem_mib)" = 32768 ] || fail "no container limit"

profile=$(sync_profile main 450 "")
[ "$profile" = $'dbcache=16384\nblocksonly=1' ] || fail "profile: $profile"
profile=$(sync_profile main 450 $'blocksonly=0\ndbcache=300')
[ -z "$profile" ] || fail "operator settings win: $profile"
[ -z "$(sync_profile regtest 450 "")" ] || fail "regtest"
[ -z "$(BITCOIN_SYNC_TUNING=off sync_profile main 450 "")" ] || fail "off"
echo "50000 100000" >"$tmp/cg/cpu.max"
if [ "$(nproc)" -gt 1 ]; then
  sync_profile main 450 "" | grep -q '^par=1$' || fail "cpu quota sets par"
fi
echo "max 100000" >"$tmp/cg/cpu.max"; sync_profile main 450 "" | grep -q '^par=' && fail "no quota, no par"
touch "$SYNC_DONE"; [ -z "$(sync_profile main 450 "")" ] || fail "normal profile after the sync"
echo "PASS: sync profile sized to the machine"
