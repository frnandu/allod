#!/usr/bin/env bash
# BITCOIN_PRUNE=auto sizing.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
# shellcheck source=prune-size.sh
source "$here/prune-size.sh"
fail() { echo "FAIL: $*"; exit 1; }
gib() { echo $(($1 * 1024)); }

[ "$(auto_prune_mib main "$(gib 500)")" = 50000 ] || fail "big disk: capped at 50000"
[ "$(auto_prune_mib main "$(gib 80)")" = 49000 ] || fail "80 GiB: $(auto_prune_mib main "$(gib 80)")"
[ "$(auto_prune_mib main "$(gib 50)")" = 22000 ] || fail "50 GiB: $(auto_prune_mib main "$(gib 50)")"
[ "$(auto_prune_mib main "$(gib 40)")" = 12000 ] || fail "40 GiB: $(auto_prune_mib main "$(gib 40)")"
[ "$(auto_prune_mib main "$(gib 28)")" = 5000 ] || fail "too small: the minimum"
[ "$(auto_prune_mib signet "$(gib 28)")" = 21000 ] || fail "signet 28 GiB: $(auto_prune_mib signet "$(gib 28)")"

# Explicit values pass through; auto is decided once and kept.
[ "$(resolve_prune main "$tmp" 0)" = 0 ] || fail "0 = unpruned"
[ "$(resolve_prune main "$tmp" 12345)" = 12345 ] || fail "explicit value"
first=$(resolve_prune signet "$tmp" auto 2>/dev/null)
[[ "$first" =~ ^[0-9]+$ ]] && [ "$first" -ge "$PRUNE_MIN" ] || fail "auto: $first"
[ "$(cat "$tmp/.prune-auto")" = "$first" ] || fail "auto is saved"
echo 7000 >"$tmp/.prune-auto"
[ "$(resolve_prune signet "$tmp" auto)" = 7000 ] || fail "the saved choice wins"
echo "prune-size: ok"
