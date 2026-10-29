#!/usr/bin/env bash
# Optional fast sync (AssumeUTXO), started by the entrypoint when /control is mounted.
# Allod writes /control/fastsync-request.json ({"url":"https://…/utxo-<height>.dat"})
# when the owner chooses fast sync during setup. This helper downloads the snapshot into
# the data directory (resumable), waits until bitcoind has the headers of the snapshot's
# base block, and calls `loadtxoutset`. bitcoind checks the snapshot against the hash
# built into its release, so the download source does not need to be trusted.
# Progress goes to /control/fastsync-status.json for the UI.
set -uo pipefail
CTRL=${FASTSYNC_CTRL:-/control}
DATA=${FASTSYNC_DATA:-/data}
REQ=$CTRL/fastsync-request.json
ST=$CTRL/fastsync-status.json
SNAP=$DATA/utxo-snapshot.dat
DONE=$DATA/.fastsync-done
log() { echo "[assumeutxo] $*" >&2; }
cli() { bitcoin-cli -conf="$DATA/bitcoin.conf" -datadir="$DATA" -rpcclienttimeout=0 "$@"; }
json_str() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | tr -d '\n\r'; }
status() { # state [extra json fields]
  printf '{"state":"%s","updated_at":%s%s}' "$1" "$(date +%s)" "${2:+,$2}" >"$ST.tmp" && mv "$ST.tmp" "$ST"
}
fail() {
  log "failed: $1"
  status failed "\"error\":\"$(json_str "$1")\""
  rm -f "$REQ" "$SNAP" "$SNAP.part"
  resume_network
}

# Base block hash from the snapshot header: magic "utxo" 0xff (5 bytes), version (2),
# network magic (4), then the block hash (32 bytes, little endian). Works on a partial
# download too, once its first 43 bytes are in.
base_hash() {
  local f=${1:-$SNAP}
  [ "$(stat -c %s "$f" 2>/dev/null || echo 0)" -ge 43 ] || return 1
  od -An -tx1 -j11 -N32 "$f" | tr -s ' \n' '\n' | grep -v '^$' | tac | tr -d '\n'
}

# While the snapshot downloads, bitcoind only needs block headers (loadtxoutset requires
# the snapshot's base header). Once that header is known, networking is paused so
# bitcoind does not spend bandwidth and disk on blocks from genesis meanwhile; it is
# switched back on after loading (or on failure or cancel). The marker re-applies the
# pause if bitcoind restarts mid-download.
PAUSED=$DATA/.fastsync-network-paused
pause_blocks_if_ready() {
  local h
  h=$(base_hash "$1") || return 0
  [ ${#h} -eq 64 ] || return 0
  cli getblockheader "$h" >/dev/null 2>&1 || return 0
  if cli getnetworkinfo 2>/dev/null | grep -q '"networkactive": true'; then
    cli setnetworkactive false >/dev/null 2>&1 && log "base header known: block download paused until the snapshot is loaded"
  fi
  touch "$PAUSED"
}
resume_network() {
  [ -f "$PAUSED" ] || return 0
  cli setnetworkactive true >/dev/null 2>&1 && { rm -f "$PAUSED"; log "networking resumed"; }
}

# Size of the file at $1 (0 if unknown).
remote_size() {
  curl -fsSLI --connect-timeout 30 --max-time 60 "$1" 2>/dev/null | tr -d '\r' \
    | awk 'tolower($1)=="content-length:"{v=$2} END{printf "%.0f", v+0}'
}

# Downloads $1 to $SNAP, resuming $SNAP.part. Survives network drops: curl gives up on a
# connection that stalls (below STALL_BYTES/s for STALL_SECS, or no growth at all for
# WATCHDOG_SECS), and the caller retries, continuing where the file stopped.
STALL_SECS=${FASTSYNC_STALL_SECS:-120}
STALL_BYTES=${FASTSYNC_STALL_BYTES:-10240}
WATCHDOG_SECS=${FASTSYNC_WATCHDOG_SECS:-180}
TICK=${FASTSYNC_TICK:-5}
FAILURES=0
download() {
  local url=$1 total have last_size last_change pid rc
  total=$(remote_size "$url")
  have=$(stat -c %s "$SNAP.part" 2>/dev/null || echo 0)
  if [ "$total" -gt 0 ] && [ "$have" -ge "$total" ]; then
    mv "$SNAP.part" "$SNAP"
    return 0
  fi
  log "downloading $url from byte $have of ${total}"
  curl -fsSL --connect-timeout 30 --speed-limit "$STALL_BYTES" --speed-time "$STALL_SECS" \
    -C - -o "$SNAP.part" "$url" &
  pid=$!
  last_size=$have
  last_change=$(date +%s)
  while kill -0 "$pid" 2>/dev/null; do
    have=$(stat -c %s "$SNAP.part" 2>/dev/null || echo 0)
    status downloading "\"bytes\":$have,\"total\":${total:-0},\"url\":\"$(json_str "$url")\""
    [ "${FASTSYNC_LIB:-0}" = 1 ] || pause_blocks_if_ready "$SNAP.part"
    if [ ! -f "$REQ" ]; then # canceled
      kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
      rm -f "$SNAP.part"; status canceled
      return 1
    fi
    if [ "$have" != "$last_size" ]; then
      last_size=$have; last_change=$(date +%s)
    elif [ $(( $(date +%s) - last_change )) -ge "$WATCHDOG_SECS" ]; then
      log "no data for ${WATCHDOG_SECS}s, restarting the download"
      kill "$pid" 2>/dev/null
    fi
    sleep "$TICK"
  done
  wait "$pid"; rc=$?
  have=$(stat -c %s "$SNAP.part" 2>/dev/null || echo 0)
  if [ "$rc" -eq 0 ] && { [ "$total" -eq 0 ] || [ "$have" -ge "$total" ]; }; then
    mv "$SNAP.part" "$SNAP"
    FAILURES=0
    log "download complete ($have bytes)"
    return 0
  fi
  if [ "$rc" -eq 33 ]; then # server cannot resume: start over
    rm -f "$SNAP.part"
  fi
  FAILURES=$((FAILURES + 1))
  local wait_s=$(( FAILURES < 6 ? 10 * FAILURES : 60 ))
  log "download interrupted (curl exit $rc) at $have bytes; retrying in ${wait_s}s"
  status download_error "\"bytes\":$have,\"total\":${total:-0},\"detail\":\"interrupted (curl $rc), resuming in ${wait_s}s\",\"url\":\"$(json_str "$url")\""
  sleep "$wait_s"
  return 1
}

# bitcoind's debug.log (mainnet in the data dir, other networks in a subdirectory).
debug_log() {
  local f
  for f in "$DATA/debug.log" "$DATA/signet/debug.log" "$DATA/testnet4/debug.log" "$DATA/regtest/debug.log"; do
    [ -f "$f" ] && { echo "$f"; return 0; }
  done
  return 1
}

# Progress of a running loadtxoutset, from the newest "[snapshot]" line logged after
# line $2 of log $1. Prints JSON fields: "phase":"…"[,"pct":N,"coins":N].
snapshot_progress() {
  # Phases: "[snapshot] N coins loaded (P%)" → coins; "loaded N coins from snapshot" and
  # "FlushSnapshotToDisk … started" → writing; "FlushSnapshotToDisk: completed" → checking
  # (bitcoind now hashes the whole UTXO set against the value built into it, silently,
  # on one core, for several minutes); "activated"/"validated" → finishing.
  local line pct coins
  line=$(tail -n +"${2:-1}" "$1" 2>/dev/null | grep -E '\[snapshot\]|FlushSnapshotToDisk' | tail -1)
  pct=$(printf '%s' "$line" | sed -n 's/.*\] \([0-9]*\) coins loaded (\([0-9.]*\)%.*/\2/p')
  coins=$(printf '%s' "$line" | sed -n 's/.*\] \([0-9]*\) coins loaded (.*/\1/p')
  if [ -n "$pct" ]; then
    printf '"phase":"coins","pct":%s,"coins":%s' "$pct" "$coins"
  elif printf '%s' "$line" | grep -q 'FlushSnapshotToDisk: completed'; then
    printf '"phase":"checking"'
  elif printf '%s' "$line" | grep -qE 'FlushSnapshotToDisk|loaded [0-9]+ .*coins from snapshot'; then
    printf '"phase":"writing"'
  elif printf '%s' "$line" | grep -qiE 'activated|validated|complete'; then
    printf '"phase":"finishing"'
  else
    printf '"phase":"starting"'
  fi
}

# Is a loadtxoutset error about the snapshot itself (wrong height, hash mismatch,
# unreadable file)? Only then is the file deleted. Anything else (an interrupt or
# shutdown, a lost RPC connection) is retried with the same file.
snapshot_is_bad() {
  printf '%s' "$1" | grep -qiE 'assumeutxo|not recognized|unrecognized|mismatch|hash|deserializ|bad snapshot|invalid snapshot|magic|network mismatch|unsupported|not found in headers|base block'
}

# Sourced by the test with FASTSYNC_LIB=1: functions only.
[ "${FASTSYNC_LIB:-0}" = 1 ] && return 0

while true; do
  sleep 10
  [ -f "$REQ" ] || { resume_network; continue; }
  if [ -f "$DONE" ]; then rm -f "$REQ"; continue; fi
  url=$(sed -n 's/.*"url"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$REQ")
  case "$url" in https://* | http://*) ;; *) fail "the snapshot URL must start with https://"; continue ;; esac
  info=$(cli getblockchaininfo 2>/dev/null) || continue
  if echo "$info" | grep -q '"initialblockdownload": false'; then
    status not_needed '"detail":"Bitcoin is already synced"'
    rm -f "$REQ"
    continue
  fi
  if [ "$(cli getchainstates 2>/dev/null | grep -c '"snapshot_blockhash"')" -gt 0 ]; then
    touch "$DONE"; status loaded; rm -f "$REQ"; resume_network; continue
  fi

  if [ ! -f "$SNAP" ]; then
    download "$url" || continue # status says why; the next round resumes
  fi

  hash=$(base_hash)
  [ ${#hash} -eq 64 ] || { fail "not a UTXO snapshot file"; continue; }
  if ! hdr=$(cli getblockheader "$hash" 2>/dev/null); then
    resume_network # headers are needed
    have=$(echo "$info" | sed -n 's/.*"headers": \([0-9]*\).*/\1/p')
    status waiting_for_headers "\"base_hash\":\"$hash\",\"headers\":${have:-0}"
    continue
  fi
  height=$(echo "$hdr" | sed -n 's/.*"height": \([0-9]*\).*/\1/p')
  status loading "\"base_height\":${height:-0},\"phase\":\"starting\""
  log "loadtxoutset (base height $height), this takes a while"
  dlog=$(debug_log) || dlog=/dev/null
  from=$(( $(wc -l <"$dlog" 2>/dev/null || echo 0) + 1 ))
  cli loadtxoutset "$SNAP" >"$DATA/.loadtxoutset.out" 2>&1 &
  lpid=$!
  while kill -0 "$lpid" 2>/dev/null; do
    status loading "\"base_height\":${height:-0},$(snapshot_progress "$dlog" "$from")"
    sleep 5
  done
  wait "$lpid"; lrc=$?
  out=$(cat "$DATA/.loadtxoutset.out" 2>/dev/null); rm -f "$DATA/.loadtxoutset.out"
  if [ "$lrc" -eq 0 ]; then
    touch "$DONE"
    rm -f "$SNAP" "$REQ" "$DATA/.fastsync-load-attempts"
    resume_network
    status loaded "\"base_height\":${height:-0}"
    log "snapshot loaded"
  else
    err=$(echo "$out" | tail -1)
    if snapshot_is_bad "$err"; then
      fail "$err"
    else
      attempts=$(( $(cat "$DATA/.fastsync-load-attempts" 2>/dev/null || echo 0) + 1 ))
      echo "$attempts" >"$DATA/.fastsync-load-attempts"
      if [ "$attempts" -ge 3 ]; then
        # Keep the file: the owner can retry or cancel (which deletes it).
        log "load failed $attempts times, giving up: $err"
        status failed "\"error\":\"$(json_str "$err")\",\"detail\":\"the snapshot file is kept\""
        rm -f "$REQ"
        resume_network
      else
        log "load interrupted ($err); retrying with the same file"
        status load_interrupted "\"base_height\":${height:-0},\"detail\":\"$(json_str "$err")\""
        sleep 30
      fi
    fi
  fi
done
