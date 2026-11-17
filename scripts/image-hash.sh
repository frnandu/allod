#!/usr/bin/env bash
# Content hash of one published image's inputs, for .github/workflows/images.yml.
#
# Usage: scripts/image-hash.sh <image> [salt...]
#   image: allod-gossip, allod-ldk-server, allod-bitcoind, allod-tor or allod-tunnel
#   salt:  build settings that change the result (profile, attestations, platforms)
#
# Prints a hex digest. CI tags every image it publishes `src-<digest>`; a later run whose
# inputs hash the same reuses that image (same digest) instead of rebuilding it, so users
# pull nothing and Compose keeps the container running across an Allod upgrade.
#
# Inputs are the git blobs (path, mode, content) of:
#   - the image's Dockerfile and every build-context file it COPYs or ADDs, read from the
#     Dockerfile itself so a new COPY is covered without editing this script;
#   - for allod-gossip, which shares allod.Dockerfile's Rust build: the workspace
#     manifests, Cargo.lock and toolchain, and the sources of allod-gossip and the
#     workspace crates it depends on (not web/ or the other crates, which only go into
#     the allod image).
# allod itself bakes the release version into its image and is always rebuilt.
#
# Debian packages are installed at build time and are not part of the hash: bump the
# dated DEBIAN_IMAGE pin in the Dockerfile to ship package updates (that changes the hash).
set -euo pipefail

image=${1:?usage: image-hash.sh <image> [salt...]}
shift
cd "$(git rev-parse --show-toplevel)"

# Build-context sources of every COPY/ADD without --from in a Dockerfile.
copy_sources() {
  awk '
    { line = buf $0; buf = "" }
    sub(/\\$/, "", line) { buf = line " "; next }
    { print line }
  ' "$1" | awk '
    toupper($1) == "COPY" || toupper($1) == "ADD" {
      from = 0; n = 0
      for (i = 2; i <= NF; i++) {
        if ($i ~ /^--from=/) from = 1
        else if ($i ~ /^--/) continue
        else args[++n] = $i
      }
      if (!from) for (i = 1; i < n; i++) print args[i]
    }
  '
}

# Workspace crates (directory names under crates/) that crate $1 depends on, recursively.
crate_closure() {
  local -A seen=()
  local queue=("$1") c dep
  while [ "${#queue[@]}" -gt 0 ]; do
    c=${queue[0]}
    queue=("${queue[@]:1}")
    [ -n "${seen[$c]:-}" ] && continue
    seen[$c]=1
    echo "crates/$c"
    while read -r dep; do
      [ -d "crates/$dep" ] && queue+=("$dep")
    done < <(sed -n -E 's/^(allod-[a-z0-9-]+)[[:space:]]*=.*/\1/p' "crates/$c/Cargo.toml")
  done
}

case "$image" in
  allod-gossip)
    dockerfile=images/allod.Dockerfile
    # Every member's manifest: cargo unifies features across the packages built together.
    mapfile -t inputs < <(
      echo Cargo.toml; echo Cargo.lock; echo rust-toolchain.toml
      echo images/install-sccache.sh; echo images/cargo-sccache.sh
      git ls-files 'crates/*/Cargo.toml'
      crate_closure allod-gossip
    )
    ;;
  allod-ldk-server | allod-bitcoind | allod-tor | allod-tunnel)
    dockerfile=images/${image#allod-}.Dockerfile
    mapfile -t inputs < <(copy_sources "$dockerfile")
    ;;
  *) echo "image-hash.sh: no input list for $image" >&2; exit 2 ;;
esac

for path in "${inputs[@]}"; do
  case "$path" in
    \[*) echo "image-hash.sh: $dockerfile uses JSON-form COPY, not supported: $path" >&2; exit 1 ;;
  esac
  # An untracked or misspelled source would silently drop out of the hash.
  if [ -z "$(git ls-files -- "$path")" ]; then
    echo "image-hash.sh: $path ($image) is not tracked by git" >&2
    exit 1
  fi
done

{
  echo "image-hash v1 $image"
  printf 'salt %s\n' "$@"
  git ls-files -s -- "$dockerfile" "${inputs[@]}" | sort -u
} | sha256sum | cut -c1-32
