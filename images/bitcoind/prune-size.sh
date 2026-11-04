#!/usr/bin/env bash
# BITCOIN_PRUNE=auto (the default): the prune target follows the disk (SPEC §2.3.2).
# Decided once, on the first start, from the space bitcoind can use: free space on its
# data volume plus what bitcoind already stores there. Kept in $DATA/.prune-auto, so it
# does not shrink as the blocks fill the disk. Delete that file to decide again.
#
# Budget: the space minus what pruning does not shrink (mainnet: chainstate ~12 GB, block
# filter index ~10 GB, ~2 GB for logs and peers) minus 10 % headroom, so the disk never
# fills up (ldk-server keeps channel state on the same disk). Rounded down to 1000 MiB,
# at most 50000 (the old fixed default) and at least the 5000 MiB minimum.
# Sourced by the entrypoint and by test-prune-size.sh.
PRUNE_MAX=50000
PRUNE_MIN=5000

# MiB that pruning does not reduce, per chain.
prune_reserve_mib() {
  case $1 in
    main) echo 24000 ;;
    regtest) echo 0 ;;
    *) echo 4000 ;; # signet, testnet4, testnet: small chainstate and filters
  esac
}

# auto_prune_mib CHAIN SPACE_MIB -> prune target in MiB
auto_prune_mib() {
  local chain=$1 space=$2 prune
  prune=$((space - $(prune_reserve_mib "$chain") - space / 10))
  prune=$((prune / 1000 * 1000))
  [ "$prune" -gt "$PRUNE_MAX" ] && prune=$PRUNE_MAX
  [ "$prune" -lt "$PRUNE_MIN" ] && prune=$PRUNE_MIN
  echo "$prune"
}

# Space bitcoind can use, in MiB: free space on the volume plus its own files.
prune_space_mib() {
  local dir=$1 free own
  free=$(df -Pm "$dir" | awk 'NR == 2 {print $4}')
  own=$(du -sm "$dir" 2>/dev/null | awk '{print $1}')
  echo $((${free:-0} + ${own:-0}))
}

# resolve_prune CHAIN DATA_DIR SETTING -> prune target in MiB (logs on stderr)
resolve_prune() {
  local chain=$1 dir=$2 setting=${3:-auto} saved space prune need
  [ "$setting" = auto ] || { echo "$setting"; return; }
  saved=$(cat "$dir/.prune-auto" 2>/dev/null || true)
  if [[ "$saved" =~ ^[0-9]+$ ]]; then
    echo "$saved"
    return
  fi
  space=$(prune_space_mib "$dir")
  prune=$(auto_prune_mib "$chain" "$space")
  need=$((prune + $(prune_reserve_mib "$chain")))
  echo "[bitcoind-entrypoint] BITCOIN_PRUNE=auto: $space MiB available for bitcoind -> prune=$prune" >&2
  if [ "$need" -gt "$((space - space / 20))" ]; then
    echo "[bitcoind-entrypoint] warning: the disk is too small for a pruned $chain node: it needs about $need MiB, $space MiB are available. Use a bigger disk or an external chain source." >&2
  fi
  echo "$prune" >"$dir/.prune-auto"
  echo "$prune"
}
