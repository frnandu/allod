#!/usr/bin/env bash
# One-shot, idempotent setup of the regtest stack (SPEC §2.3.9):
# mature coins, the managed-mode recovery phrase for Alice, funded nodes and the
# Alice -> Bob -> Carol -> Alice channel ring.
set -euo pipefail
# shellcheck source-path=SCRIPTDIR
source "$(dirname "$0")/lib.sh"

CHANNEL_SAT=${CHANNEL_SAT:-5000000}
PUSH_SAT=${PUSH_SAT:-2000000}
NODES=(alice bob carol)

wait_for 120 bitcoind btc getblockchaininfo
miner_address >/dev/null
height=$(btc getblockcount)
if (( height < 101 )); then
  log "mining $((101 - height)) blocks to mature coinbase"
  mine $((101 - height))
fi

# Alice's recovery phrase comes from Allod (dev auto-setup, managed mode).

for n in "${NODES[@]}"; do
  wait_for 600 "$n credentials" test -s "/dev-secrets/$n/admin.macaroon"
  wait_for 300 "$n ldk-server" ldk "$n" get-node-info
done

declare -A PUBKEY
for n in "${NODES[@]}"; do
  PUBKEY[$n]=$(ldk "$n" get-node-info | jq -r .node_id)
  log "$n ${PUBKEY[$n]}"
done

synced() { # every node at bitcoind's tip
  local tip n h
  tip=$(btc getblockcount)
  for n in "${NODES[@]}"; do
    h=$(ldk "$n" get-node-info | jq -r .current_best_block.height)
    [ "$h" = "$tip" ] || return 1
  done
}

spendable() { ldk "$1" get-balances | jq -r '.spendable_onchain_balance_sats // 0'; }

funded=false
for n in "${NODES[@]}"; do
  if (( $(spendable "$n") < 50000000 )); then
    addr=$(ldk "$n" onchain-receive | jq -r .address)
    log "funding $n with 1 BTC at $addr"
    btc -rpcwallet=miner sendtoaddress "$addr" 1 >/dev/null
    funded=true
  fi
done
has_funds() { (( $(spendable "$1") >= 50000000 )); }
if $funded; then
  mine 6
  wait_for 300 "nodes to sync" synced
  for n in "${NODES[@]}"; do
    wait_for 300 "$n on-chain balance" has_funds "$n"
  done
fi

has_channel() { # from to
  ldk "$1" list-channels | jq -e --arg pk "${PUBKEY[$2]}" \
    '.channels // [] | map(select(.counterparty_node_id == $pk)) | length > 0' >/dev/null
}

opened=false
open_channel() { # from to
  if has_channel "$1" "$2" || has_channel "$2" "$1"; then return; fi
  log "opening $1 -> $2 (${CHANNEL_SAT} sat, push ${PUSH_SAT} sat)"
  ldk "$1" open-channel "${PUBKEY[$2]}" "$(ldk_host "$2"):9735" "${CHANNEL_SAT}sat" \
    --push-to-counterparty "${PUSH_SAT}sat" --announce-channel >/dev/null
  opened=true
}
open_channel alice bob
open_channel bob carol
open_channel carol alice

usable_count() { ldk "$1" list-channels | jq '[.channels // [] | .[] | select(.is_usable)] | length'; }
all_usable() {
  local n
  for n in "${NODES[@]}"; do (( $(usable_count "$n") >= 2 )) || return 1; done
}
if $opened; then
  # Give the funding transactions a moment to reach the mempool, then confirm them.
  for _ in $(seq 1 30); do
    (( $(btc getmempoolinfo | jq .size) >= 3 )) && break
    sleep 2
  done
  mine 6
fi
wait_for 600 "all channels usable" all_usable
# Announcements need 6 confirmations; mine a few more so gossip can propagate.
$opened && mine 6

{
  echo "ALLOD_LDK_SERVER_ADDRESS=127.0.0.1:3536"
  echo "ALLOD_LDK_SERVER_CERT_FILE=deploy/.dev/alice/tls.crt"
  echo "ALLOD_LDK_SERVER_MACAROON_FILE=deploy/.dev/alice/admin.macaroon"
  echo "ALLOD_NETWORK=regtest"
  echo "ALLOD_RELAYS=ws://127.0.0.1:7000,ws://127.0.0.1:7001"
} >/dev-secrets/alice.env

cat >/dev-secrets/bootstrap.done <<SUMMARY
chain_tip=$(btc getblockcount)
alice=${PUBKEY[alice]}
bob=${PUBKEY[bob]}
carol=${PUBKEY[carol]}
alice_chain=${ALICE_CHAIN:-bundled}
SUMMARY

log "regtest stack ready"
log "  Allod UI     http://127.0.0.1:18029"
log "  alice gRPC   127.0.0.1:3536 (bob 3537, carol 3538)"
log "  relays       ws://127.0.0.1:7000 ws://127.0.0.1:7001"
log "  bitcoind     http://127.0.0.1:18443 (ldk / ldk-regtest)"
