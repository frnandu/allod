#!/bin/sh
# Restore latest Litestream replicas, rotate host identity, then request atomic
# restore-epoch takeover. Services remain stopped if any verification step fails.
set -eu
umask 077

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
[ "${1:-}" = --confirm-original-permanently-stopped ] || die \
  "usage: ./restore.sh --confirm-original-permanently-stopped"
[ -f .env ] || die "run from deploy directory containing .env"

value() { sed -n "s/^$1=//p" .env | tail -n 1 | sed -e "s/^'\(.*\)'$/\1/" -e 's/^"\(.*\)"$/\1/'; }
replica=$(value LITESTREAM_REPLICA_URL)
network=$(value ALLOD_NETWORK); network=${network:-bitcoin}
[ -n "$replica" ] || die "LITESTREAM_REPLICA_URL is not configured"
case "$replica" in s3://*|file://*) ;; *) die "safe restore supports s3:// and file:// replicas" ;; esac

printf '%s\n' "Stopping writers before restoring latest verified replicas..." >&2
docker compose stop litestream allod ldk-server
docker compose run --rm --no-deps litestream restore -force -integrity-check full \
  -o "/ldk/$network/ldk_node_data.sqlite" "$replica/ldk"
docker compose run --rm --no-deps litestream restore -force -integrity-check full \
  -o /allod/allod.db "$replica/allod"

instance=$(od -An -tx1 -N24 /dev/urandom | tr -d ' \n')
tmp=.env.restore.$$
trap 'rm -f "$tmp"' 0 1 2 15
awk -v id="$instance" '
  BEGIN { done = 0 }
  /^#? ?ALLOD_INSTANCE_ID=/ { if (!done) print "ALLOD_INSTANCE_ID=" id; done = 1; next }
  { print }
  END { if (!done) print "ALLOD_INSTANCE_ID=" id }
' .env >"$tmp"
mv "$tmp" .env
trap - 0 1 2 15

docker compose run --rm --no-deps --entrypoint /bin/bash ldk-server -c \
  'umask 077; printf "%s\n" latest-replica-confirmed > /handoff/restore-request'

printf '%s\n' "Replica integrity verified. Requesting fenced epoch takeover..." >&2
docker compose up -d ldk-server allod litestream
printf '%s\n' "Restore started. ldk-server waits until previous replica owner has stopped." >&2
