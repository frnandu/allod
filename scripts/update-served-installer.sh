#!/bin/sh
# Keeps the install.sh served at allod.me/install.sh (or any web root) at the newest
# Allod release published on Nostr. Run it on the machine that serves the file, by hand
# after a release or from cron / a systemd timer; it does nothing when already current.
#
# The new install.sh is fetched with `allod release fetch`, run from the allod image
# pinned (by digest) in the install.sh served now. That only accepts a file from a
# release signed by the Allod release key whose sha256 matches the signed release, so
# each served installer vouches for the next one. Nightlies are never served.
#
# Usage: update-served-installer.sh [options]
#   --dir DIR        folder the web server serves install.sh from (default ~/allod-release)
#   --version VER    serve this release instead of the newest one
#   --helper IMAGE   allod image to fetch with, pinned by digest; only needed the first
#                    time, when DIR has no install.sh yet
# The previous copy is kept as DIR/install.sh.prev (roll back: mv install.sh.prev install.sh).
set -eu

DIR=${HOME:?HOME is not set}/allod-release
VERSION=''
HELPER=''
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case $1 in
    --dir) DIR=${2:?--dir needs a value}; shift 2 ;;
    --version) VERSION=${2:?--version needs a value}; VERSION=${VERSION#v}; shift 2 ;;
    --helper) HELPER=${2:?--helper needs a value}; shift 2 ;;
    -h|--help) sed -n '2,/^set -eu/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
done

command -v docker >/dev/null 2>&1 || die "docker is needed"
mkdir -p "$DIR"
cd "$DIR"

# The image to fetch with: pinned by the install.sh served now, unless given.
# shellcheck disable=SC2016 # a literal ${ALLOD_RELEASE_HELPER:-...} in install.sh
pinned() { sed -n 's/^RELEASE_HELPER=${ALLOD_RELEASE_HELPER:-\(.*\)}$/\1/p' "$1"; }
if [ -z "$HELPER" ]; then
  [ -f install.sh ] || die "$DIR has no install.sh yet: pass --helper <allod image@sha256:...> once"
  HELPER=$(pinned install.sh)
  [ -n "$HELPER" ] || die "cannot read the image pin of $DIR/install.sh"
fi
case $HELPER in *@sha256:*) ;; *) die "--helper must be pinned by digest (image@sha256:...)" ;; esac

new=$(mktemp -d "$DIR/.new.XXXXXX")
trap 'rm -rf "$new"' EXIT
chmod 0777 "$new" # the container may write as another uid
uid="$(id -u):$(id -g)"
case $(docker info -f '{{range .SecurityOptions}}{{.}} {{end}}' 2>/dev/null) in *rootless*) uid=0:0 ;; esac

set -- --channel main --file install.sh --out /out
[ -z "$VERSION" ] || set -- "$@" --version "$VERSION"
got=$(docker run --rm --pull missing --user "$uid" -v "$new:/out" "$HELPER" release fetch "$@") \
  || die "cannot fetch the release's install.sh (relays or Blossom servers unreachable?)"
[ -f "$new/install.sh" ] || die "release $got has no install.sh"

if [ -f install.sh ] && cmp -s "$new/install.sh" install.sh; then
  echo "install.sh is already $got"
  exit 0
fi
sh -n "$new/install.sh" || die "the fetched install.sh is not valid shell"
pinned "$new/install.sh" | grep -q '@sha256:[0-9a-f]\{64\}$' \
  || die "the fetched install.sh pins no release image"

[ ! -f install.sh ] || cp -p install.sh install.sh.prev
chmod 0644 "$new/install.sh"
mv "$new/install.sh" install.sh.next && mv install.sh.next install.sh
echo "install.sh is now $got${VERSION:+ (pinned with --version)}"
