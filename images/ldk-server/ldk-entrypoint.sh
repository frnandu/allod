#!/usr/bin/env bash
# Renders ldk-server's config.toml from environment variables, runs the managed-mode
# seed handoff (SPEC §2.3.3), publishes credentials for Allod, then starts ldk-server.
set -euo pipefail

log() { echo "[ldk-entrypoint] $*" >&2; }
die() { log "error: $*"; exit 1; }

DATA=${LDK_DATA_DIR:-/data}
HANDOFF=${LDK_HANDOFF_DIR:-/handoff}
NETWORK=${LDK_NETWORK:?LDK_NETWORK is required (bitcoin, signet, testnet4, regtest)}
CHAIN=${LDK_CHAIN:-bundled}
SEED_HANDOFF=${LDK_SEED_HANDOFF:-true}
# Anchor channels keep a per-channel on-chain reserve for fee bumps. Allod sets it to
# zero so inbound anchor channels (including LSP-funded ones) work without an on-chain
# balance. See SPEC §2.3.2. Operators can restore a safety margin by setting this.
ANCHOR_RESERVE=${LDK_ANCHOR_PER_CHANNEL_RESERVE_SATS:-0}
case "$ANCHOR_RESERVE" in ''|*[!0-9]*) die "LDK_ANCHOR_PER_CHANNEL_RESERVE_SATS must be a non-negative integer" ;; esac
# Space-separated directories that receive tls.crt and admin.macaroon after start.
CREDENTIALS_DIRS=${LDK_CREDENTIALS_DIRS:-$HANDOFF}

case "$NETWORK" in bitcoin|signet|testnet|testnet4|regtest) ;; *) die "unknown LDK_NETWORK $NETWORK" ;; esac

toml_str() { # quote a TOML basic string
  local s=${1//\\/\\\\}; s=${s//\"/\\\"}; printf '"%s"' "$s"
}
toml_list() { # comma-separated -> TOML array of strings
  local out="" item
  IFS=',' read -ra items <<<"$1"
  for item in "${items[@]}"; do
    item=$(echo "$item" | xargs)
    [ -n "$item" ] && out+="${out:+, }$(toml_str "$item")"
  done
  printf '[%s]' "$out"
}

bitcoind_rpc() { # method [params JSON] -> result JSON, using the ldk-server RPC credentials
  curl -fsS --max-time 30 -u "${LDK_BITCOIND_RPC_USER}:${LDK_BITCOIND_RPC_PASS}" \
    -H 'content-type: text/plain' \
    --data "{\"jsonrpc\":\"1.0\",\"id\":\"ldk-entrypoint\",\"method\":\"$1\",\"params\":${2:-[]}}" \
    "http://${LDK_BITCOIND_RPC_ADDRESS}/" | jq -c '.result'
}

# --- chain backend --------------------------------------------------------------
chain_section=""
uses_bitcoind=false
case "$CHAIN" in
  bundled|external-bitcoind|bundled-rest)
    uses_bitcoind=true
    : "${LDK_BITCOIND_RPC_ADDRESS:?LDK_BITCOIND_RPC_ADDRESS is required for chain mode $CHAIN}"
    : "${LDK_BITCOIND_RPC_USER:?LDK_BITCOIND_RPC_USER is required}"
    : "${LDK_BITCOIND_RPC_PASS:?LDK_BITCOIND_RPC_PASS is required}"
    chain_section="[bitcoind]
rpc_address = $(toml_str "$LDK_BITCOIND_RPC_ADDRESS")
rpc_user = $(toml_str "$LDK_BITCOIND_RPC_USER")
rpc_password = $(toml_str "$LDK_BITCOIND_RPC_PASS")"
    if [ "$CHAIN" = bundled-rest ]; then
      chain_section+="
rest_address = $(toml_str "${LDK_BITCOIND_REST_ADDRESS:-$LDK_BITCOIND_RPC_ADDRESS}")"
    fi
    ;;
  electrum)
    : "${LDK_ELECTRUM_URL:?LDK_ELECTRUM_URL is required for chain mode electrum}"
    chain_section="[electrum]
server_url = $(toml_str "$LDK_ELECTRUM_URL")"
    ;;
  esplora)
    : "${LDK_ESPLORA_URL:?LDK_ESPLORA_URL is required for chain mode esplora}"
    chain_section="[esplora]
server_url = $(toml_str "$LDK_ESPLORA_URL")"
    ;;
  *) die "unknown LDK_CHAIN $CHAIN (bundled, bundled-rest, external-bitcoind, electrum, esplora)" ;;
esac

# Wait for bitcoind so the first start does not fail on a cold stack.
if $uses_bitcoind; then
  for i in $(seq 1 120); do
    if info=$(bitcoind_rpc getblockchaininfo 2>/dev/null) && [ "$info" != null ]; then break; fi
    [ "$i" = 120 ] && die "bitcoind at $LDK_BITCOIND_RPC_ADDRESS not reachable"
    [ $((i % 10)) = 1 ] && log "waiting for bitcoind at $LDK_BITCOIND_RPC_ADDRESS"
    sleep 2
  done
fi

# ldk-node retries fee estimation against bitcoind without backoff; during initial block
# download bitcoind cannot estimate fees, and the retries exhaust local ports (os error 99).
# Start ldk-server only once bitcoind has left IBD (SPEC open item O-12).
if $uses_bitcoind && [ "${LDK_WAIT_FOR_IBD:-true}" = true ]; then
  n=0
  while true; do
    # A failed or slow call (bitcoind is busy during IBD) means "keep waiting".
    if info=$(bitcoind_rpc getblockchaininfo 2>/dev/null) \
      && [ "$(echo "$info" | jq -r .initialblockdownload)" = false ]; then
      break
    fi
    if (( n % 10 == 0 )); then
      progress=$(echo "${info:-null}" | jq -r 'if . == null then "bitcoind busy" else "\(.blocks)/\(.headers) blocks, \(.verificationprogress * 100 | floor)%" end' 2>/dev/null || echo "bitcoind busy")
      log "bitcoind initial block download: $progress; ldk-server starts when it finishes"
    fi
    n=$((n + 1))
    sleep 30
  done
  log "bitcoind finished initial block download"
fi

# --- LSPS2 LSP for just-in-time (JIT) receive channels (SPEC §2.3) ---------------------
# LDK_LSPS2_LSP is pubkey@host:port; empty or unset disables the liquidity source. The
# token authenticates us to the LSP's LSPS2 API; trust_peer_0conf additionally accepts
# 0-conf channels from the LSP (payments land without waiting for a confirmation).
LSPS2_LSP=${LDK_LSPS2_LSP:-}
LSPS2_TOKEN=${LDK_LSPS2_LSP_TOKEN:-}
LSPS2_TRUST_0CONF=${LDK_LSPS2_LSP_TRUST_0CONF:-false}
case "$LSPS2_TRUST_0CONF" in true|false) ;; *) die "LDK_LSPS2_LSP_TRUST_0CONF must be true or false" ;; esac
liquidity_section=""
if [ -n "$LSPS2_LSP" ]; then
  lsp_pubkey=${LSPS2_LSP%@*}
  lsp_address=${LSPS2_LSP##*@}
  if [ "$lsp_pubkey" = "$LSPS2_LSP" ] || [ "$lsp_pubkey" = "" ] \
    || ! [[ "$lsp_pubkey" =~ ^(02|03)[0-9a-fA-F]{64}$ ]] \
    || ! [[ "$lsp_address" =~ ^(\[[^]]+\]|[^:]+):[0-9]+$ ]]; then
    die "LDK_LSPS2_LSP must be pubkey@host:port, got: $LSPS2_LSP"
  fi
  liquidity_section="
[liquidity]
[[liquidity.lsps_client]]
node_pubkey = $(toml_str "$(echo "$lsp_pubkey" | tr 'A-F' 'a-f')")
address = $(toml_str "$lsp_address")"
  if [ -n "$LSPS2_TOKEN" ]; then
    liquidity_section+="
token = $(toml_str "$LSPS2_TOKEN")"
  fi
  liquidity_section+="
trust_peer_0conf = $LSPS2_TRUST_0CONF"
  log "LSPS2 LSP configured: $lsp_pubkey ($LSPS2_TRUST_0CONF 0conf)"
fi

# On mainnet ldk-node refuses to start without fee estimates (it would misprice
# force-close and sweep transactions). A fresh node, especially one synced from a
# snapshot or with blocksonly, has none yet: bitcoind learns them from transactions it
# sees enter its mempool and confirm, usually within an hour of normal operation.
# Wait for estimates at every target ldk-node uses instead of crash-looping.
fee_ready() {
  local t
  for t in 2 6 12 144 1008; do
    bitcoind_rpc estimatesmartfee "[$t]" 2>/dev/null | jq -e '.feerate' >/dev/null || return 1
  done
}
if $uses_bitcoind && [ "$NETWORK" = bitcoin ] && [ "${LDK_WAIT_FOR_FEES:-true}" = true ]; then
  n=0
  until fee_ready; do
    if (( n % 10 == 0 )); then
      log "waiting for bitcoind fee estimates (learned from new blocks and mempool transactions); ldk-server starts when they are available"
    fi
    n=$((n + 1))
    sleep 30
  done
  log "bitcoind fee estimates available"
fi

# --- gossip: P2P by default; RGS or automatic selection is opt-in (SPEC §2.3.2 pruned rule 5) ---
GOSSIP=${LDK_GOSSIP:-p2p}
RGS_URL=${LDK_RGS_URL:-https://rapidsync.lightningdevkit.org/snapshot/v2/}
if [ "$GOSSIP" = auto ]; then
  GOSSIP=p2p
  if $uses_bitcoind && [ "$NETWORK" = bitcoin ] && [ "$(echo "$info" | jq -r '.pruned')" = true ]; then
    GOSSIP=rgs
  fi
fi
case "$GOSSIP" in p2p|rgs) ;; *) die "LDK_GOSSIP must be auto, p2p or rgs" ;; esac
log "network=$NETWORK chain=$CHAIN gossip=$GOSSIP"

# --- config.toml --------------------------------------------------------------------
mkdir -p "$DATA"
{
  echo "[node]"
  echo "network = $(toml_str "$NETWORK")"
  echo "listening_addresses = $(toml_list "${LDK_LISTEN:-0.0.0.0:9735}")"
  [ -n "${LDK_ANNOUNCE:-}" ] && echo "announcement_addresses = $(toml_list "$LDK_ANNOUNCE")"
  echo "grpc_service_address = $(toml_str "${LDK_GRPC_ADDRESS:-0.0.0.0:3536}")"
  echo "alias = $(toml_str "${LDK_ALIAS:-allod}")"
  echo "anchor_per_channel_reserve_sats = $ANCHOR_RESERVE"
  [ "$GOSSIP" = rgs ] && echo "rgs_server_url = $(toml_str "$RGS_URL")"
  [ -n "${LDK_PATHFINDING_SCORES_URL+x}" ] && echo "pathfinding_scores_source_url = $(toml_str "$LDK_PATHFINDING_SCORES_URL")"
  echo
  echo "[storage.disk]"
  echo "dir_path = $(toml_str "$DATA")"
  if [ -n "${LDK_POSTGRES_URL:-}" ]; then
    echo
    echo "[storage.postgres]"
    echo "connection_string = $(toml_str "$LDK_POSTGRES_URL")"
    echo "db_name = $(toml_str "${LDK_POSTGRES_DB:-ldk_node}")"
  fi
  echo
  echo "[log]"
  echo "level = $(toml_str "${LDK_LOG_LEVEL:-Info}")"
  echo "log_to_file = false"
  echo
  echo "[tls]"
  echo "hosts = $(toml_list "${LDK_TLS_HOSTS:-ldk-server}")"
  if [ -n "${LDK_TOR_PROXY:-}" ]; then
    echo
    echo "[tor]"
    echo "proxy_address = $(toml_str "$LDK_TOR_PROXY")"
  fi
  echo
  echo "$chain_section"
  echo "$liquidity_section"
} >"$DATA/config.toml"
chmod 0600 "$DATA/config.toml"

# --- seed handoff ---------------------------------------------------------------------
extra_args=()
if [ ! -f "$DATA/keys_mnemonic" ] && [ "$SEED_HANDOFF" = true ]; then
  log "waiting for the recovery phrase in $HANDOFF/keys_mnemonic (finish setup in the Allod UI)"
  n=0
  until [ -s "$HANDOFF/keys_mnemonic" ]; do
    sleep 2; n=$((n + 1))
    [ $((n % 30)) = 0 ] && log "still waiting for $HANDOFF/keys_mnemonic"
  done
  words=$(wc -w <"$HANDOFF/keys_mnemonic")
  [ "$words" = 12 ] || [ "$words" = 24 ] || die "keys_mnemonic must hold 12 or 24 words"
  install -m 0400 "$HANDOFF/keys_mnemonic" "$DATA/keys_mnemonic"
  rm -f "$HANDOFF/keys_mnemonic"
  if [ -s "$HANDOFF/rescan_from_height" ]; then
    extra_args+=(--rescan-from-height "$(tr -dc 0-9 <"$HANDOFF/rescan_from_height")")
    rm -f "$HANDOFF/rescan_from_height"
  fi
  log "recovery phrase installed"
fi

# --- publish credentials for Allod / tools once ldk-server created them ----------------
publish_credentials() {
  local mac="$DATA/$NETWORK/macaroons/admin.macaroon" dir
  for _ in $(seq 1 900); do
    [ -s "$DATA/tls.crt" ] && [ -s "$mac" ] && break
    sleep 1
  done
  [ -s "$mac" ] || { log "credentials did not appear; not publishing"; return; }
  for dir in $CREDENTIALS_DIRS; do
    mkdir -p "$dir"
    install -m 0444 "$DATA/tls.crt" "$dir/tls.crt.tmp" && mv -f "$dir/tls.crt.tmp" "$dir/tls.crt"
    # Once Allod has its own scoped macaroon it marks the handoff directory; the admin
    # macaroon is then no longer published there (SPEC §2.3.3).
    if [ "$dir" = "$HANDOFF" ] && [ -e "$HANDOFF/.provisioned" ]; then
      rm -f "$dir/admin.macaroon"
      continue
    fi
    install -m "${LDK_CREDENTIALS_MODE:-0440}" "$mac" "$dir/admin.macaroon.tmp" && mv -f "$dir/admin.macaroon.tmp" "$dir/admin.macaroon"
  done
  log "published tls.crt and admin.macaroon to: $CREDENTIALS_DIRS"
}
publish_credentials &

exec /usr/local/bin/ldk-server "$DATA/config.toml" "${extra_args[@]}"
