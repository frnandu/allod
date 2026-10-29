#!/usr/bin/env bash
# Renders the release Compose bundle (SPEC §2.3.5, §2.3.8):
#   dist/allod-stack-<version>.tar.gz  compose.yaml with pinned image digests, .env.example,
#                                       Caddyfile, litestream.yml
#   dist/SHA256SUMS
# Usage: scripts/render-bundle.sh <version> [digests.env]
# digests.env lines: <image-name>=<registry>/<image>@sha256:<digest>
set -euo pipefail

version=${1:?usage: render-bundle.sh <version> [digests.env]}
digests=${2:-}
root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/dist"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
name="allod-stack-$version"
mkdir -p "$stage/$name" "$out"

compose="$stage/$name/compose.yaml"
# Drop build sections: release bundles only pull signed images.
awk '
  /^x-build-ldk:/ { skip_anchor=1; next }
  skip_anchor && /^[^ ]/ { skip_anchor=0 }
  skip_anchor { next }
  /^    build:/ {
    if ($0 ~ /\{.*\}/ || $0 ~ /\*build-ldk/) next
    skip_build=1; next
  }
  skip_build && /^      / { next }
  { skip_build=0; print }
' "$root/deploy/compose.yaml" >"$compose"

if [ -n "$digests" ]; then
  while IFS='=' read -r image ref; do
    [ -z "$image" ] && continue
    # Replace "<registry>/<image>:<tag>" with the digest reference.
    sed -i -E "s#image: \\\$\\{ALLOD_REGISTRY:-[^}]*\\}/${image}:\\\$\\{ALLOD_VERSION:-dev\\}#image: ${ref}#" "$compose"
  done <"$digests"
  if grep -q 'ALLOD_VERSION:-dev' "$compose"; then
    echo "error: some Allod images have no digest in $digests" >&2
    grep -n 'ALLOD_VERSION:-dev' "$compose" >&2
    exit 1
  fi
else
  sed -i "s#\${ALLOD_VERSION:-dev}#\${ALLOD_VERSION:-$version}#g" "$compose"
fi

cp "$root/deploy/.env.example" "$root/deploy/Caddyfile" "$root/deploy/litestream.yml" "$stage/$name/"
sed -i "s#^ALLOD_VERSION=.*#ALLOD_VERSION=$version#" "$stage/$name/.env.example"

# Bundle must still be a valid Compose project.
cp "$stage/$name/.env.example" "$stage/$name/.env"
docker compose -f "$compose" --env-file "$stage/$name/.env" config -q
rm "$stage/$name/.env"

tar -C "$stage" --owner=0 --group=0 --sort=name --mtime='2020-01-01' -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" >SHA256SUMS)
echo "wrote $out/$name.tar.gz"
