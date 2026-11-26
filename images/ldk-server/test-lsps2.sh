#!/usr/bin/env bash
# lsps2_resolve: the stack's LSPS2 LSP, replaced by the owner's choice in Allod (several
# LSPs, one 0-conf setting).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
log() { :; }
# shellcheck source=images/ldk-server/ldk-lsps2.sh
source "$here/ldk-lsps2.sh"

stack="02$(printf 'a%.0s' {1..64})@stack.example:9735"
other="03$(printf 'b%.0s' {1..64})@203.0.113.5:9735"
req=$tmp/lsps2.json
# "lsp|token lsp|token … 0conf"
run() {
  LDK_LSPS2_LSP=$1 LDK_LSPS2_LSP_TOKEN=$2 LDK_LSPS2_LSP_TRUST_0CONF=${3:-false} lsps2_resolve "$req"
  local out="" i
  for i in "${!LSPS2_LSPS[@]}"; do out+="${LSPS2_LSPS[$i]}|${LSPS2_TOKENS[$i]} "; done
  echo "$out$LSPS2_TRUST_0CONF"
}
status() { LDK_LSPS2_LSP=$stack lsps2_resolve "$req"; echo "$LSPS2_REQUEST_STATUS${LSPS2_REQUEST_ERROR:+: $LSPS2_REQUEST_ERROR}"; }

# No choice yet: the stack's, with its 0-conf setting.
[ "$(run "$stack" stacktoken true)" = "$stack|stacktoken true" ] || fail "no request"
[ "$(run "" "")" = "false" ] || fail "no LSP at all"
# Off.
echo '{"enabled": false}' >"$req"
[ "$(run "$stack" stacktoken)" = "false" ] || fail "off"
# Several LSPs: the stack's keeps its token, another has its own or none; 0-conf for all.
echo "{\"enabled\": true, \"trust_0conf\": true, \"lsps\": [{\"lsp\": \"$stack\"}, {\"lsp\": \"$other\", \"token\": \"t1\"}]}" >"$req"
[ "$(run "$stack" stacktoken)" = "$stack|stacktoken $other|t1 true" ] || fail "several: $(run "$stack" stacktoken)"
echo "{\"enabled\": true, \"trust_0conf\": false, \"lsps\": [{\"lsp\": \"$other\"}]}" >"$req"
[ "$(run "$stack" stacktoken true)" = "$other| false" ] || fail "other only"
# Unusable choices leave the stack's in place.
for bad in '{"enabled": true, "lsps": [{"lsp": "nope"}]}' '{"enabled": true, "lsps": []}' \
  '{"enabled": true, "trust_0conf": "maybe", "lsps": [{"lsp": "'"$other"'"}]}' '{"enabled": "yes"}' 'not json'; do
  echo "$bad" >"$req"
  [ "$(run "$stack" stacktoken)" = "$stack|stacktoken false" ] || fail "bad request: $bad"
done
# What became of the choice, for Allod's page.
rm -f "$req"; [ "$(status)" = "none" ] || fail "status none"
echo '{"enabled": false}' >"$req"; [ "$(status)" = "off" ] || fail "status off"
echo "{\"enabled\": true, \"lsps\": [{\"lsp\": \"$other\"}]}" >"$req"; [ "$(status)" = "applied" ] || fail "status applied"
echo '{"enabled": "yes"}' >"$req"; [ "$(status)" = "ignored: enabled must be true or false" ] || fail "status ignored: $(status)"
if [ "$(id -u)" != 0 ]; then
  echo "{\"enabled\": true, \"lsps\": [{\"lsp\": \"$other\"}]}" >"$req"; chmod 000 "$req"
  [[ "$(status)" == unreadable:* ]] || fail "status unreadable: $(status)"
  [ "$(run "$stack" stacktoken)" = "$stack|stacktoken false" ] || fail "unreadable keeps the stack's"
  chmod 600 "$req"
fi
echo "PASS: LSPS2 LSPs from the stack and Allod's settings"
