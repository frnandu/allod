#!/usr/bin/env bash
# Private curl credential handling for ldk-entrypoint.sh.
# shellcheck shell=bash

curl_config_escape() {
  local value=$1
  case "$value" in
    *$'\n'*|*$'\r'*) return 1 ;;
  esac
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  printf '%s' "$value"
}

prepare_bitcoind_rpc_credentials() {
  local user pass tmp
  user=$(curl_config_escape "${LDK_BITCOIND_RPC_USER:?LDK_BITCOIND_RPC_USER is required}") \
    || return 1
  pass=$(curl_config_escape "${LDK_BITCOIND_RPC_PASS:?LDK_BITCOIND_RPC_PASS is required}") \
    || return 1
  BITCOIND_CURL_CONFIG=${BITCOIND_CURL_CONFIG:-"${DATA:?DATA is required}/.bitcoind-rpc.curl"}
  tmp="$BITCOIND_CURL_CONFIG.tmp.$$"
  umask 077
  if ! printf 'user = "%s:%s"\n' "$user" "$pass" >"$tmp"; then
    rm -f "$tmp"
    return 1
  fi
  chmod 0600 "$tmp"
  mv -f "$tmp" "$BITCOIND_CURL_CONFIG"
  export BITCOIND_CURL_CONFIG
}

bitcoind_rpc() { # method [params JSON] -> result JSON
  curl -fsS --max-time 30 --config /proc/self/fd/3 \
    -H 'content-type: text/plain' \
    --data "{\"jsonrpc\":\"1.0\",\"id\":\"ldk-entrypoint\",\"method\":\"$1\",\"params\":${2:-[]}}" \
    "http://${LDK_BITCOIND_RPC_ADDRESS}/" 3<"$BITCOIND_CURL_CONFIG" | jq -c '.result'
}
