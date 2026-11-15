# Shared helpers for the regtest tools container.
# shellcheck shell=bash

log() { echo "[$(date +%H:%M:%S)] $*" >&2; }
die() { log "error: $*"; exit 1; }

btc() {
  printf '%s\n' "${BITCOIND_RPC_PASS:-ldk-regtest}" | bitcoin-cli \
    -regtest -rpcconnect=bitcoind -rpcport=8332 \
    -rpcuser="${BITCOIND_RPC_USER:-ldk}" -stdinrpcpass "$@"
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
ldk() (
  local name=$1; shift
  local dir=/dev-secrets/$name
  local cli_home
  cli_home=$(mktemp -d)
  trap 'rm -rf "$cli_home"' EXIT
  mkdir -p "$cli_home/.ldk-server/bitcoin/macaroons"
  chmod 0700 "$cli_home" "$cli_home/.ldk-server" \
    "$cli_home/.ldk-server/bitcoin" "$cli_home/.ldk-server/bitcoin/macaroons"
  ln -s "$dir/admin.macaroon" "$cli_home/.ldk-server/bitcoin/macaroons/admin.macaroon"
  HOME="$cli_home" ldk-server-cli -b "$(ldk_host "$name"):3536" -t "$dir/tls.crt" "$@"
)

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
