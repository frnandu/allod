#!/bin/sh
# Installs a pinned, checksum-verified sccache into /usr/local/bin (used by cargo-sccache.sh).
# Usage: install-sccache.sh <TARGETARCH>
set -eu
version=0.19.0
case "${1:?usage: install-sccache.sh <amd64|arm64>}" in
  amd64) triple=x86_64-unknown-linux-musl; sum=aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd ;;
  arm64) triple=aarch64-unknown-linux-musl; sum=3e47fc4a71b1ddd1b4294a0de9d34498922fdf41e3cd0bda5df3954e3d6deb9d ;;
  *) echo "unsupported arch $1" >&2; exit 1 ;;
esac
name="sccache-v${version}-${triple}"
tmp=$(mktemp -d)
curl -fsSL -o "$tmp/sccache.tar.gz" "https://github.com/mozilla/sccache/releases/download/v${version}/${name}.tar.gz"
echo "$sum  $tmp/sccache.tar.gz" | sha256sum -c -
tar -xzf "$tmp/sccache.tar.gz" -C "$tmp"
install -m 0755 "$tmp/$name/sccache" /usr/local/bin/sccache
rm -rf "$tmp"
