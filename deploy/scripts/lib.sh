# Shared helpers for the regtest tools container.
# shellcheck shell=bash

log() { echo "[$(date +%H:%M:%S)] $*" >&2; }
die() { log "error: $*"; exit 1; }

btc() {
  bitcoin-cli -regtest -rpcconnect=bitcoind -rpcport=8332 \
    -rpcuser="${BITCOIND_RPC_USER:-ldk}" -rpcpassword="${BITCOIND_RPC_PASS:-ldk-regtest}" "$@"
}

# Node name -> gRPC host
ldk_host() {
  case "$1" in
    alice) echo ldk-server ;;
    bob) echo ldk-bob ;;
    carol) echo ldk-carol ;;
    *) die "unknown node $1" ;;
  esac
}

# ldk NAME COMMAND... -> runs ldk-server-cli against that node
ldk() {
  local name=$1; shift
  local dir=/dev-secrets/$name
  ldk-server-cli -b "$(ldk_host "$name"):3536" -t "$dir/tls.crt" -m "$(cat "$dir/admin.macaroon")" "$@"
}

wait_for() { # seconds description command...
  local timeout=$1 what=$2; shift 2
  local start=$SECONDS
  until "$@" >/dev/null 2>&1; do
    (( SECONDS - start > timeout )) && die "timed out after ${timeout}s waiting for $what"
    sleep 2
  done
}

miner_address() {
  btc -rpcwallet=miner getnewaddress >/dev/null 2>&1 || {
    btc createwallet miner >/dev/null 2>&1 || btc loadwallet miner >/dev/null 2>&1 || true
  }
  btc -rpcwallet=miner getnewaddress
}

mine() { btc generatetoaddress "${1:-1}" "$(miner_address)" >/dev/null; }
