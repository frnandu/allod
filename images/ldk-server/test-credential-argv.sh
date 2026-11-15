#!/usr/bin/env bash
# Regress H-8: reusable ldk-server and bitcoind credentials never enter argv.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

mkdir -p "$tmp/bin" "$tmp/data/.healthcheck/.ldk-server/bitcoin/macaroons"
health_secret='health-secret-must-not-enter-argv'
printf '%s\n' "$health_secret" \
  >"$tmp/data/.healthcheck/.ldk-server/bitcoin/macaroons/admin.macaroon"
cat >"$tmp/bin/ldk-server-cli" <<'SH'
#!/usr/bin/env bash
tr '\0' ' ' <"/proc/$$/cmdline" >"$HEALTH_ARGV_OUT"
cat "$HOME/.ldk-server/bitcoin/macaroons/admin.macaroon" >"$HEALTH_TOKEN_OUT"
SH
chmod 0755 "$tmp/bin/ldk-server-cli"
HEALTH_ARGV_OUT="$tmp/health.argv" HEALTH_TOKEN_OUT="$tmp/health.token" \
  LDK_DATA_DIR="$tmp/data" LDK_NETWORK=regtest PATH="$tmp/bin:$PATH" \
  "$here/ldk-healthcheck.sh"
health_argv=$(cat "$tmp/health.argv")
[[ $health_argv != *"$health_secret"* ]] || fail "health credential exposed in argv"
[[ $health_argv != *' -m '* && $health_argv != *' --macaroon '* ]] \
  || fail "healthcheck passes macaroon argument: $health_argv"
[[ $(cat "$tmp/health.token") == "$health_secret" ]] \
  || fail "healthcheck did not load private HOME credential"

cat >"$tmp/bin/curl" <<'SH'
#!/usr/bin/env bash
tr '\0' ' ' <"/proc/$$/cmdline" >"$RPC_ARGV_OUT"
config=''
while (($#)); do
  if [[ $1 == --config ]]; then config=$2; shift 2; else shift; fi
done
cat "$config" >"$RPC_CONFIG_OUT"
printf '%s\n' '{"result":{"ok":true}}'
SH
chmod 0755 "$tmp/bin/curl"
export DATA="$tmp/data" LDK_BITCOIND_RPC_ADDRESS=bitcoind:8332
export LDK_BITCOIND_RPC_USER='rpc-user' LDK_BITCOIND_RPC_PASS='rpc-password-must-not-enter-argv'
export RPC_ARGV_OUT="$tmp/rpc.argv" RPC_CONFIG_OUT="$tmp/rpc.config" PATH="$tmp/bin:$PATH"
# shellcheck source=images/ldk-server/ldk-rpc.sh
source "$here/ldk-rpc.sh"
prepare_bitcoind_rpc_credentials
bitcoind_rpc getblockchaininfo >/dev/null
rpc_argv=$(cat "$tmp/rpc.argv")
[[ $rpc_argv != *"$LDK_BITCOIND_RPC_PASS"* ]] || fail "RPC password exposed in argv"
[[ $rpc_argv != *' -u '* && $rpc_argv != *' --user '* ]] \
  || fail "curl user argument present: $rpc_argv"
[[ $rpc_argv == *'/proc/self/fd/3'* ]] || fail "curl did not receive credential fd"
[[ $(cat "$tmp/rpc.config") == "user = \"$LDK_BITCOIND_RPC_USER:$LDK_BITCOIND_RPC_PASS\"" ]] \
  || fail "curl did not read protected credential config"
[[ $(stat -c '%a' "$BITCOIND_CURL_CONFIG") == 600 ]] || fail "curl config is not mode 0600"
grep -q -- '--permissions node:read' "$here/ldk-entrypoint.sh" \
  || fail "healthcheck macaroon is not node:read scoped"

echo "PASS: ldk-server and bitcoind credentials absent from process argv"
