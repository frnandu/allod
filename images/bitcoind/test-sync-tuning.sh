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
mem() { printf 'MemTotal: %s kB\nMemAvailable: %s kB\n' "$(($1 * 1024))" "$(($2 * 1024))" >"$tmp/meminfo"; }

mem 32768 16384; [ "$(mem_mib)" = 12288 ] || fail "host available memory less reserve"
[ "$(sync_dbcache "$(mem_mib)")" = 2048 ] || fail "32 GB host: capped at 2 GiB"
mem 4096 3072; [ "$(mem_mib)" = 2048 ] || fail "4 GB VPS"
[ "$(sync_dbcache "$(mem_mib)")" = 1024 ] || fail "4 GB cache"
mem 2048 1280; [ "$(mem_mib)" = 768 ] || fail "2 GB box"
[ "$(steady_dbcache 8000)" = 384 ] || fail "oversized steady setting is capped"
mem 32768 16384
echo $((2 * 1024 * 1024 * 1024)) >"$tmp/cg/memory.max"
echo $((128 * 1024 * 1024)) >"$tmp/cg/memory.current"
[ "$(mem_mib)" = 1152 ] || fail "container remaining memory less reserve"
[ "$(sync_dbcache "$(mem_mib)")" = 576 ] || fail "container limits sync cache"
[ "$(steady_dbcache 8000)" = 576 ] || fail "container limits steady cache"
echo max >"$tmp/cg/memory.max"; [ "$(mem_mib)" = 12288 ] || fail "no container limit"
rm "$tmp/cg/memory.max" "$tmp/cg/memory.current"
mkdir -p "$tmp/cg/memory"
echo $((1024 * 1024 * 1024)) >"$tmp/cg/memory/memory.limit_in_bytes"
echo $((128 * 1024 * 1024)) >"$tmp/cg/memory/memory.usage_in_bytes"
[ "$(mem_mib)" = 128 ] || fail "cgroup v1 limit and usage"
rm -r "$tmp/cg/memory"
mem 32768 4096; [ "$(mem_mib)" = 0 ] || fail "low host availability"
[ "$(sync_dbcache "$(mem_mib)")" = 64 ] || fail "low memory floor"
mem 32768 16384

profile=$(sync_profile main "")
[ "$profile" = $'dbcache=2048\nblocksonly=1' ] || fail "profile: $profile"
profile=$(sync_profile main $'blocksonly=0\ndbcache=300')
[ -z "$profile" ] || fail "operator settings win: $profile"
[ -z "$(sync_profile regtest "")" ] || fail "regtest"
[ -z "$(BITCOIN_SYNC_TUNING=off sync_profile main "")" ] || fail "off"
echo "50000 100000" >"$tmp/cg/cpu.max"
if [ "$(nproc)" -gt 1 ]; then
  sync_profile main "" | grep -q '^par=1$' || fail "cpu quota sets par"
fi
echo "max 100000" >"$tmp/cg/cpu.max"; sync_profile main "" | grep -q '^par=' && fail "no quota, no par"
touch "$SYNC_DONE"; [ -z "$(sync_profile main "")" ] || fail "normal profile after the sync"
echo "PASS: sync profile sized to the machine"
