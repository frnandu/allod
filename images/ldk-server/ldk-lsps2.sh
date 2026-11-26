#!/usr/bin/env bash
# The LSPS2 LSPs ldk-server starts with (SPEC §2.3), for ldk-entrypoint.sh. ldk-node asks
# every one for a just-in-time channel offer and takes the cheapest that fits the payment.
# shellcheck shell=bash

# pubkey@host:port, with a compressed pubkey.
valid_lsp() {
  local pubkey=${1%@*} address=${1##*@}
  [ "$pubkey" != "$1" ] && [[ "$pubkey" =~ ^(02|03)[0-9a-fA-F]{64}$ ]] \
    && [[ "$address" =~ ^(\[[^]]+\]|[^:]+):[0-9]+$ ]]
}

# The stack's LSPs: LDK_LSPS2_LSP is one or more pubkey@host:port, comma-separated, and
# LDK_LSPS2_LSP_TOKEN their tokens in the same order (comma-separated, empty for none).
# Sets STACK_LSPS and STACK_TOKENS; returns 1 naming the first malformed LSP.
lsps2_stack() {
  local raw=${LDK_LSPS2_LSP:-} tokens=${LDK_LSPS2_LSP_TOKEN:-} lsp i=0
  local -a list toks
  STACK_LSPS=() STACK_TOKENS=()
  IFS=',' read -r -a list <<<"${raw//[[:space:]]/}"
  IFS=',' read -r -a toks <<<"$tokens"
  for lsp in "${list[@]}"; do
    [ -n "$lsp" ] || continue
    valid_lsp "$lsp" || { STACK_BAD=$lsp; return 1; }
    STACK_LSPS+=("$lsp") STACK_TOKENS+=("${toks[$i]:-}")
    i=$((i + 1))
  done
}
# The stack's token for an LSP, if it is one of the stack's.
lsps2_stack_token() {
  local i
  for i in "${!STACK_LSPS[@]}"; do
    [ "${STACK_LSPS[$i]}" = "$1" ] && { printf '%s' "${STACK_TOKENS[$i]}"; return 0; }
  done
}

# Sets LSPS2_LSPS and LSPS2_TOKENS (arrays, one entry per LSP) and LSPS2_TRUST_0CONF from
# the stack (LDK_LSPS2_LSP, LDK_LSPS2_LSP_TOKEN, LDK_LSPS2_LSP_TRUST_0CONF) and the
# owner's choice in Allod, $1 (Settings → Lightning → Liquidity), which replaces it:
#   {"enabled": false}
#   {"enabled": true, "trust_0conf": false,
#    "lsps": [{"lsp": "pubkey@host:port", "token": "..."}, ...]}
# An LSP without a token gets the stack's token when it is one of the stack's LSPs. A choice that
# cannot be used is ignored as a whole (logged), so a typo in it never stops the node.
# LSPS2_REQUEST_STATUS says what became of the choice, for Allod's settings page: none (no
# choice: the stack's), applied, off, ignored or unreadable; LSPS2_REQUEST_ERROR says why.
lsps2_ignore() {
  LSPS2_REQUEST_STATUS=ignored LSPS2_REQUEST_ERROR=$2
  log "ignoring $1: $2"
}
lsps2_resolve() {
  local request=$1 lsp token trust enabled n i
  LSPS2_LSPS=() LSPS2_TOKENS=()
  LSPS2_TRUST_0CONF=${LDK_LSPS2_LSP_TRUST_0CONF:-false}
  LSPS2_REQUEST_STATUS=none LSPS2_REQUEST_ERROR=""
  if lsps2_stack && [ "${#STACK_LSPS[@]}" -gt 0 ]; then
    LSPS2_LSPS=("${STACK_LSPS[@]}") LSPS2_TOKENS=("${STACK_TOKENS[@]}")
  fi
  [ -e "$request" ] || return 0
  if ! [ -r "$request" ]; then
    LSPS2_REQUEST_STATUS=unreadable LSPS2_REQUEST_ERROR="ldk-server cannot read $request (owner or mode)"
    log "cannot read $request; the stack's LSPS2 settings apply"
    return 0
  fi
  [ -s "$request" ] || return 0
  # `// empty` would drop false: ask for the key itself.
  enabled=$(jq -r 'if has("enabled") then .enabled | tostring else empty end' "$request" 2>/dev/null || true)
  case "$enabled" in
    false)
      LSPS2_LSPS=() LSPS2_TOKENS=() LSPS2_REQUEST_STATUS=off
      log "LSPS2 LSPs turned off in Allod settings"
      return 0
      ;;
    true) ;;
    *)
      lsps2_ignore "$request" "enabled must be true or false"
      return 0
      ;;
  esac
  trust=$(jq -r '.trust_0conf // false | tostring' "$request" 2>/dev/null || true)
  n=$(jq -r '.lsps | if type == "array" then length else 0 end' "$request" 2>/dev/null || echo 0)
  if ! [[ "$trust" =~ ^(true|false)$ ]] || ! [[ "$n" =~ ^[0-9]+$ ]] || [ "$n" -eq 0 ]; then
    lsps2_ignore "$request" "needs lsps and trust_0conf true or false"
    return 0
  fi
  local lsps=() tokens=()
  for ((i = 0; i < n; i++)); do
    lsp=$(jq -r --argjson i "$i" '.lsps[$i].lsp // empty' "$request" 2>/dev/null || true)
    token=$(jq -r --argjson i "$i" '.lsps[$i].token // empty' "$request" 2>/dev/null || true)
    if ! valid_lsp "$lsp"; then
      lsps2_ignore "$request" "each lsp must be pubkey@host:port"
      return 0
    fi
    if [ -z "$token" ]; then token=$(lsps2_stack_token "$lsp"); fi
    lsps+=("$lsp") tokens+=("$token")
  done
  LSPS2_LSPS=("${lsps[@]}") LSPS2_TOKENS=("${tokens[@]}") LSPS2_TRUST_0CONF=$trust
  LSPS2_REQUEST_STATUS=applied
  log "LSPS2 LSPs from Allod settings: ${#LSPS2_LSPS[@]}"
}
