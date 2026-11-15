#!/usr/bin/env bash
# Cross-host channel-state fence. One replica object names current restore epoch and
# process owner. A restore atomically advances epoch, then waits long enough for old
# process watchdog to stop ldk-server before restored process starts.
set -euo pipefail

log() { echo "[restore-fence] $*" >&2; }
die() { log "error: $*"; exit 1; }

DATA=${LDK_DATA_DIR:-/data}
HANDOFF=${LDK_HANDOFF_DIR:-/handoff}
REPLICA=${LITESTREAM_REPLICA_URL:?LITESTREAM_REPLICA_URL is required for restore fencing}
INSTANCE=${ALLOD_INSTANCE_ID:?ALLOD_INSTANCE_ID is required when backup profile is enabled}
POLL=${LDK_RESTORE_FENCE_POLL_SECS:-5}
REQUEST=$HANDOFF/restore-request
LOCAL_EPOCH=$DATA/restore_epoch
BOOT=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
tmp=$(mktemp -d)
child=""
lock_dir=""
acquired=false

case "$POLL" in ''|*[!0-9]*|0) die "LDK_RESTORE_FENCE_POLL_SECS must be a positive integer" ;; esac
case "$INSTANCE" in *[!A-Za-z0-9._-]*|'') die "ALLOD_INSTANCE_ID contains unsupported characters" ;; esac

cleanup() {
  [ -n "$child" ] && kill -TERM "$child" 2>/dev/null || true
  [ -n "$child" ] && wait "$child" 2>/dev/null || true
  release || true
  [ -n "$lock_dir" ] && rmdir "$lock_dir" 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 143' INT TERM

json_epoch() { jq -er '.epoch | numbers | floor | select(. >= 0)' "$1"; }
json_instance() { jq -er '.instance | strings' "$1"; }
json_boot() { jq -er '.boot | strings' "$1"; }
json_active() { jq -er '.active | select(type == "boolean") | tostring' "$1"; }

# read_remote OUT_HEADERS OUT_BODY: exit 0 found, 1 missing, 2 unavailable.
read_file_remote() {
  local body=$1 root=${REPLICA#file://} object
  object=${root%/}/restore-fence.json
  [ -e "$object" ] || return 1
  cp "$object" "$body" || return 2
  printf '%s\n' "$(cksum <"$body")" >"$tmp/etag"
}

file_cas() {
  local expected=$1 input=$2 root=${REPLICA#file://} object current
  object=${root%/}/restore-fence.json
  mkdir -p "${object%/*}"
  lock_dir=$object.lock
  mkdir "$lock_dir" 2>/dev/null || return 2
  if [ -e "$object" ]; then current=$(cksum <"$object"); else current=missing; fi
  if [ "$current" != "$expected" ]; then rmdir "$lock_dir"; lock_dir=""; return 1; fi
  cp "$input" "$object.tmp.$$" && chmod 0600 "$object.tmp.$$" && mv -f "$object.tmp.$$" "$object"
  rmdir "$lock_dir"; lock_dir=""
}

s3_url() {
  local value=${REPLICA#s3://} bucket=${value%%/*} prefix=""
  [ "$bucket" != "$value" ] && prefix=/${value#*/}
  prefix=${prefix%/}
  if [ -n "${LITESTREAM_ENDPOINT:-}" ]; then
    printf '%s/%s%s/restore-fence.json' "${LITESTREAM_ENDPOINT%/}" "$bucket" "$prefix"
  else
    printf 'https://%s.s3.%s.amazonaws.com%s/restore-fence.json' "$bucket" "${LITESTREAM_REGION:-us-east-1}" "$prefix"
  fi
}

write_curl_config() {
  local escaped_key escaped_secret
  escaped_key=${LITESTREAM_ACCESS_KEY_ID//\\/\\\\}; escaped_key=${escaped_key//\"/\\\"}
  escaped_secret=${LITESTREAM_SECRET_ACCESS_KEY//\\/\\\\}; escaped_secret=${escaped_secret//\"/\\\"}
  umask 077
  cat >"$tmp/curl.conf" <<EOF
silent
show-error
connect-timeout = 10
max-time = 20
aws-sigv4 = "aws:amz:${LITESTREAM_REGION:-us-east-1}:s3"
user = "$escaped_key:$escaped_secret"
EOF
}

read_s3_remote() {
  local body=$1 code
  code=$(curl --config "$tmp/curl.conf" -D "$tmp/headers" -o "$body" -w '%{http_code}' "$(s3_url)") || return 2
  case "$code" in
    200) awk 'tolower($1) == "etag:" {gsub(/\r/, ""); sub(/^[^:]*:[[:space:]]*/, ""); print; exit}' "$tmp/headers" >"$tmp/etag" ;;
    404) return 1 ;;
    *) log "replica GET returned HTTP $code"; return 2 ;;
  esac
  [ -s "$tmp/etag" ] || return 2
}

s3_cas() {
  local expected=$1 input=$2 code condition
  if [ "$expected" = missing ]; then condition='If-None-Match: *'; else condition="If-Match: $expected"; fi
  code=$(curl --config "$tmp/curl.conf" -o "$tmp/put-body" -w '%{http_code}' \
    -X PUT -H 'Content-Type: application/json' -H "$condition" --data-binary "@$input" "$(s3_url)") || return 2
  case "$code" in 200|201|204) return 0 ;; 409|412) return 1 ;; *) log "replica PUT returned HTTP $code"; return 2 ;; esac
}

case "$REPLICA" in
  file://*) read_remote() { read_file_remote "$1"; }; remote_cas() { file_cas "$1" "$2"; } ;;
  s3://*)
    : "${LITESTREAM_ACCESS_KEY_ID:?LITESTREAM_ACCESS_KEY_ID is required for S3 fencing}"
    : "${LITESTREAM_SECRET_ACCESS_KEY:?LITESTREAM_SECRET_ACCESS_KEY is required for S3 fencing}"
    write_curl_config
    read_remote() { read_s3_remote "$1"; }; remote_cas() { s3_cas "$1" "$2"; }
    ;;
  *) die "restore fencing supports s3:// and file:// replica URLs; refusing unsafe start" ;;
esac

make_record() {
  jq -n --argjson epoch "$1" --arg instance "$INSTANCE" --arg boot "$BOOT" \
    --argjson active "$2" \
    '{epoch: $epoch, instance: $instance, boot: $boot, active: $active}' >"$tmp/new.json"
}

acquire() {
  local status epoch remote_instance remote_active expected request=false wait_for_old=false
  if read_remote "$tmp/remote.json"; then status=found
  else status=$?; [ "$status" = 1 ] || die "replica fence unavailable"; status=missing; fi

  if [ -e "$REQUEST" ]; then
    [ "$(cat "$REQUEST")" = latest-replica-confirmed ] || die "invalid restore request"
    request=true
  fi

  if [ "$status" = missing ]; then
    [ "$request" = false ] || die "replica has no fence record; cannot prove restore freshness"
    epoch=0; expected=missing
  else
    epoch=$(json_epoch "$tmp/remote.json") || die "invalid replica fence record"
    remote_instance=$(json_instance "$tmp/remote.json") || die "invalid replica fence record"
    remote_active=$(json_active "$tmp/remote.json") || die "invalid replica fence record"
    expected=$(cat "$tmp/etag")
    if [ "$request" = true ]; then
      epoch=$((epoch + 1))
      [ "$remote_active" = true ] && wait_for_old=true
    elif [ "$remote_active" = true ]; then
      die "replica has an active owner; use verified restore procedure after confirming it is permanently stopped"
    elif [ "$remote_instance" != "$INSTANCE" ]; then
      die "replica belongs to another instance; use verified restore procedure"
    elif [ ! -s "$LOCAL_EPOCH" ] || [ "$(cat "$LOCAL_EPOCH")" != "$epoch" ]; then
      die "local restore epoch is stale; use verified restore procedure"
    fi
  fi

  make_record "$epoch" true
  remote_cas "$expected" "$tmp/new.json" || die "replica fence changed while acquiring; another node may be running"
  acquired=true
  printf '%s\n' "$epoch" >"$LOCAL_EPOCH.tmp" && chmod 0600 "$LOCAL_EPOCH.tmp" && mv -f "$LOCAL_EPOCH.tmp" "$LOCAL_EPOCH"
  if [ "$request" = true ]; then
    rm -f "$REQUEST"
    log "restore epoch advanced to $epoch"
  fi
  if [ "$wait_for_old" = true ]; then
    log "waiting for previous owner watchdog"
    sleep $((POLL * 2 + 1))
  fi
  log "acquired restore epoch $epoch"
}

still_owner() {
  read_remote "$tmp/watch.json" \
    && [ "$(json_instance "$tmp/watch.json")" = "$INSTANCE" ] \
    && [ "$(json_boot "$tmp/watch.json")" = "$BOOT" ] \
    && [ "$(json_epoch "$tmp/watch.json")" = "$(cat "$LOCAL_EPOCH")" ] \
    && [ "$(json_active "$tmp/watch.json")" = true ]
}

release() {
  local expected epoch
  [ "$acquired" = true ] || return 0
  acquired=false
  if ! read_remote "$tmp/release.json" \
    || [ "$(json_instance "$tmp/release.json")" != "$INSTANCE" ] \
    || [ "$(json_boot "$tmp/release.json")" != "$BOOT" ]; then
    return 0
  fi
  expected=$(cat "$tmp/etag")
  epoch=$(json_epoch "$tmp/release.json") || return 0
  make_record "$epoch" false
  remote_cas "$expected" "$tmp/new.json" || log "could not release replica fence; next start requires verified restore"
}

acquire
"$@" &
child=$!
while kill -0 "$child" 2>/dev/null; do
  sleep "$POLL" & wait $! || true
  if ! still_owner; then
    log "lost replica fence; stopping ldk-server"
    acquired=false
    kill -TERM "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
    child=""
    exit 1
  fi
done
if wait "$child"; then status=0; else status=$?; fi
child=""
exit "$status"
