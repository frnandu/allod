#!/usr/bin/env bash
# The ssh commands are built here on purpose: every local value in them is quoted with %q.
# shellcheck disable=SC2029
# Ships local builds of stack images to a running Allod stack over SSH, for quick tests
# without a release (no registry, Nostr or Blossom).
#
# Usage: scripts/ship.sh <ssh-target> [services] [remote-dir]
#   ssh-target  anything ssh accepts: user@host, host, an ~/.ssh/config alias
#               (default: $ALLOD_SHIP_HOST)
#   services    space- or comma-separated compose services (default: allod), e.g.
#               "allod bitcoind"; built as deploy/compose.yaml's build: sections say
#   remote-dir  the stack's directory there (default: found from the running stack)
#
# Nothing is written on the remote host except the images: the services are recreated
# with a compose override read from stdin for that one command. The next plain
# `docker compose up -d` there (the installer runs one on every update) puts the stack's
# own images back, so updating to a tag or nightly needs no cleanup.
#
# Env: ALLOD_SHIP_PROFILE (cargo profile, default fast-release: no LTO, quick relinks),
#      ALLOD_SHIP_CHECK=1 (stop after checking the remote stack: nothing built or sent).
set -euo pipefail

target=${1:-${ALLOD_SHIP_HOST:-}}
services=${2:-allod}
remote_dir=${3:-}
profile=${ALLOD_SHIP_PROFILE:-fast-release}
[ -n "$target" ] || { echo "usage: $0 <ssh-target> [services] [remote-dir]" >&2; exit 2; }
services=$(echo "$services" | tr ',' ' ' | xargs)

root=$(cd "$(dirname "$0")/.." && pwd)
say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
die() { printf 'ship: %s\n' "$*" >&2; exit 1; }

rev=$(git -C "$root" rev-parse --short HEAD)
git -C "$root" diff --quiet HEAD -- || rev="$rev-dirty"
version="dev-$rev"

# The build: sections of deploy/compose.yaml, with every profile so any service is found.
config=$(docker compose -f "$root/deploy/compose.yaml" --env-file "$root/deploy/.env.example" \
  --profile '*' config --format json)

# --- Remote preflight -------------------------------------------------------------
# Prints: arch, stack directory, compose files (comma-separated), decompressor.
say "checking $target"
# shellcheck disable=SC2016 # expanded on the remote host
preflight='
set -eu
dir=$1
if [ -z "$dir" ]; then
  dirs=$(docker ps --filter label=com.docker.compose.service=allod \
    --format "{{.Label \"com.docker.compose.project.working_dir\"}}" | sort -u)
  [ -n "$dirs" ] || { echo "no running Allod stack found; pass its directory" >&2; exit 1; }
  [ "$(echo "$dirs" | wc -l)" = 1 ] || { echo "several Allod stacks: pass one of" $dirs >&2; exit 1; }
  dir=$dirs
fi
[ -d "$dir" ] || { echo "$dir: no such directory" >&2; exit 1; }
# The files the running stack was started with (an override of its own included).
# "-" is the stdin override of an earlier ship: recorded by compose, never a file.
files=$(docker ps -a --filter "label=com.docker.compose.project.working_dir=$dir" \
  --format "{{.Label \"com.docker.compose.project.config_files\"}}" |
  tr "," "\n" | grep -vx -- "-" | grep . | awk "!seen[\$0]++" | paste -sd, -)
[ -n "$files" ] || files=$dir/compose.yaml
if command -v zstd >/dev/null; then dec="zstd -dc"; else dec="gzip -dc"; fi
echo "$(docker version -f "{{.Server.Arch}}")|$dir|$files|$dec"
'
IFS='|' read -r remote_arch remote_dir files decompress < <(
  ssh "$target" "bash -c $(printf %q "$preflight") ship $(printf %q "$remote_dir")"
) || die "preflight on $target failed"
[ -n "${remote_arch:-}" ] || die "preflight on $target failed"
local_arch=$(docker version -f '{{.Server.Arch}}')
[ "$remote_arch" = "$local_arch" ] ||
  die "$target runs $remote_arch, this machine builds $local_arch images"
compose_files=""
IFS=',' read -ra list <<<"$files"
for f in "${list[@]}"; do compose_files+=" -f $(printf %q "$f")"; done
echo "    stack: $remote_dir ($remote_arch)"

remote_services=$(ssh "$target" "cd $(printf %q "$remote_dir") && docker compose$compose_files config --services </dev/null")
for s in $services; do
  grep -qx "$s" <<<"$remote_services" || die "service $s is not in the stack at $remote_dir"
done
echo "    services: $services"
if [ "${ALLOD_SHIP_CHECK:-}" = 1 ]; then say "check only: nothing built or sent"; exit 0; fi

# --- Build ------------------------------------------------------------------------
images=()
override="services:"
for s in $services; do
  build=$(jq -c --arg s "$s" '.services[$s].build // empty' <<<"$config")
  [ -n "$build" ] || die "service $s has no build: section in deploy/compose.yaml"
  base=$(jq -r --arg s "$s" '.services[$s].image | split("/")[-1] | split(":")[0]' <<<"$config")
  tag="allod-ship/$base:$version"
  args=(--build-arg "CARGO_PROFILE=$profile" --build-arg "ALLOD_BUILD_VERSION=$version")
  while IFS= read -r a; do [ -n "$a" ] && args+=(--build-arg "$a"); done < <(
    jq -r '(.args // {}) | to_entries[] | "\(.key)=\(.value)"' <<<"$build")
  t=$(jq -r '.target // empty' <<<"$build")
  [ -n "$t" ] && args+=(--target "$t")
  say "building $s as $tag"
  docker buildx build --load \
    -f "$(jq -r '.context' <<<"$build")/$(jq -r '.dockerfile' <<<"$build")" \
    "${args[@]}" -t "$tag" "$(jq -r '.context' <<<"$build")"
  images+=("$tag")
  override+=$'\n'"  $s:"$'\n'"    image: $tag"$'\n'"    pull_policy: never"
done

# --- Transfer ---------------------------------------------------------------------
say "sending ${images[*]}"
if [ "$decompress" = "zstd -dc" ]; then compress=(zstd -T0 -3 -c); else compress=(gzip -c); fi
docker save "${images[@]}" | "${compress[@]}" | ssh "$target" "$decompress | docker load"

# --- Recreate ---------------------------------------------------------------------
say "recreating $services in $remote_dir"
# shellcheck disable=SC2016 # expanded on the remote host
recreate='
set -eu
dir=$1 override=$2 files=$3; shift 3
cd "$dir"
printf "%s\n" "$override" | eval docker compose $files -f - up -d --no-deps "$@"
for s in "$@"; do
  c=$(eval docker compose $files ps -q "$s")
  for _ in $(seq 1 60); do
    h=$(docker inspect -f "{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}" "$c")
    [ "$h" = starting ] || break
    sleep 2
  done
  echo "    $s: $h"
done
# Earlier shipped images no container uses any more.
docker image ls --format "{{.Repository}}:{{.Tag}}" | grep "^allod-ship/" |
  xargs -r docker rmi >/dev/null 2>&1 || true
'
# shellcheck disable=SC2086 # $services: one argument per service
ssh "$target" "bash -c $(printf %q "$recreate") ship $(printf %q "$remote_dir") $(printf %q "$override") $(printf %q "$compose_files") $services"

say "done: $services at $version on $target"
echo "    back to the stack's own images: ssh $target 'cd $remote_dir && docker compose up -d'"
