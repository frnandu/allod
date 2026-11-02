#!/bin/sh
# Build Allod appliance images (proof of concept).
#
# A Debian live system that boots straight into Docker and runs the same
# deploy/compose.yaml as a VPS install. The stack images are baked in, so nothing is
# pulled at boot. Docker and all stack data live on a separate data disk (see
# overlay/usr/local/sbin/allod-data-setup).
#
# Usage: iso/build.sh [amd64|rpi]...        (default: amd64)
#   amd64  iso/out/allod-amd64.iso          hybrid BIOS/UEFI ISO for PCs; local images
#                                           ${ALLOD_REGISTRY}/*:${ALLOD_VERSION:-dev}, or
#                                           the registry's with ALLOD_IMAGES=registry
#   rpi    iso/out/allod-rpi-arm64.img      SD card / USB image for Raspberry Pi 4 and 5;
#                                           arm64 images from the registry
#                                           (${ALLOD_RPI_VERSION:-nightly})
# With ALLOD_ISO_SNAPSHOT=/path/utxo-<height>.dat each target also gets a "-snapshot"
# variant that carries the UTXO snapshot for fast sync (SPEC §2.3.10).
# Build hosts: amd64 builds both (rpi through qemu-user-static, slow the first time);
# arm64 builds rpi natively (CI: .github/workflows/appliance.yml).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
work="$here/work"
out="$here/out"
export ALLOD_REGISTRY="${ALLOD_REGISTRY:-ghcr.io/frnandu}"
snapshot="${ALLOD_ISO_SNAPSHOT:-}"
[ -z "$snapshot" ] || [ -f "$snapshot" ] || { echo "ALLOD_ISO_SNAPSHOT: no such file $snapshot" >&2; exit 1; }
targets="${*:-amd64}"

mkdir -p "$work" "$out"
# Build containers run natively on the host's architecture.
platform="linux/$(docker version -f '{{.Server.Arch}}')"

# Stack bundle: the same files a VPS install uses.
rm -rf "$work/stack" && mkdir -p "$work/stack"
cp "$repo/deploy/compose.yaml" "$repo/deploy/.env.example" "$repo/deploy/Caddyfile" \
   "$repo/deploy/litestream.yml" "$work/stack/"

# Images for every profile (bundled bitcoind, https, tor, backup, postgres), so no
# profile needs the network to start.
image_list() {
  ALLOD_VERSION=$1 docker compose -f "$repo/deploy/compose.yaml" \
    --env-file "$repo/deploy/.env.example" --profile "*" config --images | sort -u
}

# Fetches the images of one architecture from the registry with skopeo (so the host's
# images with the same tags stay untouched), one archive per image, cached by digest.
fetch_images() { # arch images
  echo "Fetching images ($1):"; echo "$2" | sed 's/^/  /'
  docker run --rm --platform "$platform" -v "$work:/work" -e ARCH="$1" -e IMAGES="$2" debian:trixie sh -euc '
    apt-get update -qq && apt-get install -y -qq --no-install-recommends skopeo zstd ca-certificates >/dev/null
    d=/work/images-$ARCH; mkdir -p "$d"; ids=""
    rm -f "$d/images.tar.zst"
    for i in $IMAGES; do
      digest=$(skopeo inspect --override-os linux --override-arch "$ARCH" --format "{{.Digest}}" "docker://$i")
      f="$d/$(echo "$i" | tr "/:" "__").tar.zst"
      if [ "$(cat "$f.digest" 2>/dev/null)" != "$digest" ]; then
        rm -f "$f" /tmp/i.tar
        skopeo copy -q --override-os linux --override-arch "$ARCH" "docker://$i" "docker-archive:/tmp/i.tar:$i"
        zstd -T0 -10 -q --rm /tmp/i.tar -o "$f"
        echo "$digest" > "$f.digest"
      fi
      ids="$ids$digest"
    done
    echo "$ids" | sha256sum | cut -c1-16 > "$d/images.id"'
}

build_target() {
  target=$1
  case $target in
    amd64)
      arch=amd64 version="${ALLOD_VERSION:-dev}"
      imgdir="$work/images-amd64"
      images=$(image_list "$version")
      if [ "${ALLOD_IMAGES:-local}" = registry ]; then
        fetch_images amd64 "$images"
        echo "$version" > "$imgdir/version"
        build_image "$target" amd64
        return
      fi
      for i in $images; do
        docker image inspect "$i" >/dev/null 2>&1 || { echo "missing image $i (run: just iso)" >&2; return 1; }
      done
      # shellcheck disable=SC2086
      id=$(docker image inspect --format '{{.Id}}' $images | sha256sum | cut -c1-16)
      if [ "$id" != "$(cat "$imgdir/images.id" 2>/dev/null)" ]; then
        echo "Saving images ($arch):"; echo "$images" | sed 's/^/  /'
        rm -rf "$imgdir" && mkdir -p "$imgdir"
        # shellcheck disable=SC2086
        docker save $images | zstd -T0 -10 -q -o "$imgdir/images.tar.zst"
        echo "$id" > "$imgdir/images.id"
      fi
      ;;
    rpi)
      arch=arm64 version="${ALLOD_RPI_VERSION:-nightly}"
      imgdir="$work/images-arm64"
      images=$(image_list "$version")
      fetch_images arm64 "$images"
      ;;
    *) echo "unknown target $target (amd64 or rpi)" >&2; return 1 ;;
  esac
  echo "$version" > "$imgdir/version"
  build_image "$target" "$arch"
}

# Root filesystem, squashfs and boot images, built in a privileged Debian container.
build_image() { # target arch
  target=$1 arch=$2
  snap_mount=""
  [ -n "$snapshot" ] && snap_mount="-v $(cd "$(dirname "$snapshot")" && pwd)/$(basename "$snapshot"):/snapshot/$(basename "$snapshot"):ro"
  # shellcheck disable=SC2086
  docker run --rm --privileged --platform "$platform" -v "$here:/iso" $snap_mount \
    -e ARCH="$arch" -e TARGET="$target" \
    debian:trixie /iso/build-in-container.sh
}

# One target runs in the foreground. Several run in parallel (they share no files), each
# logging to iso/work/build-<target>.log.
set -- $targets
if [ $# -eq 1 ]; then
  build_target "$1"
else
  pids=""
  for t in "$@"; do
    echo "building $t (log: $work/build-$t.log)"
    ( build_target "$t" ) > "$work/build-$t.log" 2>&1 &
    pids="$pids $!:$t"
  done
  failed=""
  for p in $pids; do
    wait "${p%%:*}" || failed="$failed ${p#*:}"
  done
  [ -z "$failed" ] || { echo "failed:$failed (see $work/build-<target>.log)" >&2; exit 1; }
fi
ls -lh "$out"
