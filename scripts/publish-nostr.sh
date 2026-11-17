#!/usr/bin/env bash
# Publishes an Allod release on Nostr, once it is out on GitHub (git tag) and Docker Hub
# (images). Everything is signed by the Allod release key through ngit:
#   1. renders the Compose bundle from the git ref, pinned to the Docker Hub digests
#   2. pushes the git tag to the nostr remote (main releases)
#   3. publishes the release (kind 30063) with the bundle, install.sh and SHA256SUMS on
#      Allod's Blossom servers; install.sh finds it with `allod release fetch`
#   4. copies the six images from Docker Hub with their digests and publishes them as
#      Nostr container repositories (served by ncontainer.io/allod.me)
#
# Usage: scripts/publish-nostr.sh <version> [options]
#   scripts/publish-nostr.sh 0.3.1
#   scripts/publish-nostr.sh nightly-20261117-1200 --nightly --ref 92adeee
# Options:
#   --nightly        nightly channel: no git tag push
#   --ref REF        git ref to render from (default v<version>; with --nightly, HEAD)
#   --registry REG   where the images are (default docker.io/allodme)
#   --notes TEXT     release notes
#   --skip-images    do not publish the container images (step 4)
#   --yes            do not ask before publishing
#
# Needs: git, docker, jq, curl, ngit with the Allod account (ngit account login), and
# a git remote named "nostr" for the Allod repository.
set -euo pipefail

SIGNER=${ALLOD_NOSTR_SIGNER:-allod}
RELAYS=(wss://relay.ditto.pub wss://relay.primal.net)
IMAGES=(allod allod-bitcoind allod-gossip allod-ldk-server allod-tunnel allod-tor)
SKOPEO=quay.io/skopeo/stable:v1.18.0

version='' channel=main ref='' registry=docker.io/allodme notes='' images=1 yes=0
die() { echo "error: $*" >&2; exit 1; }
step() { printf '\n== %s\n' "$*" >&2; }

while [ $# -gt 0 ]; do
  case $1 in
    --nightly) channel=nightly; shift ;;
    --ref) ref=${2:?--ref needs a value}; shift 2 ;;
    --registry) registry=${2:?--registry needs a value}; shift 2 ;;
    --notes) notes=${2:?--notes needs a value}; shift 2 ;;
    --skip-images) images=0; shift ;;
    --yes) yes=1; shift ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) die "unknown option $1 (see --help)" ;;
    *) [ -z "$version" ] || die "one version only"; version=${1#v}; shift ;;
  esac
done
[ -n "$version" ] || die "usage: $0 <version> [options] (see --help)"
case $channel in
  main) ref=${ref:-v$version}
        notes=${notes:-"Allod $version: Compose bundle for VPS installs. Images on ncontainer.io/allod.me and $registry, pinned by digest."} ;;
  nightly) ref=${ref:-HEAD}
        notes=${notes:-"Nightly build $version: untested, for signet/testnet4, not for real funds. Images on $registry, pinned by digest."} ;;
esac

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
for t in git docker jq curl ngit; do command -v "$t" >/dev/null || die "missing $t"; done
git remote get-url nostr >/dev/null 2>&1 || die "no git remote named nostr"
commit=$(git rev-parse --verify -q "$ref^{commit}") || die "unknown git ref $ref"

work=$(mktemp -d)
trap 'git worktree remove --force "$work/src" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT

step "Image digests for $version on $registry"
for i in "${IMAGES[@]}"; do
  d=$(curl -fsSL "https://hub.docker.com/v2/repositories/${registry#docker.io/}/$i/tags/$version" | jq -r '.digest // empty') \
    || die "cannot read $registry/$i:$version from Docker Hub"
  [ -n "$d" ] || die "$registry/$i:$version has no digest"
  echo "$i=$registry/$i@$d"
done >"$work/digests.env"
sed 's/@sha256:\(.\{12\}\).*/@sha256:\1…/' "$work/digests.env" >&2

step "Rendering the bundle from $ref (${commit:0:12})"
git worktree add -q --detach "$work/src" "$commit"
(cd "$work/src" && bash scripts/render-bundle.sh "$version" "$work/digests.env" >&2)
mv "$work/src/dist" "$work/dist"
(cd "$work/dist" && sha256sum -c SHA256SUMS >&2)
grep -q '^RELEASE_HELPER=.*@sha256:[0-9a-f]\{64\}}$' "$work/dist/install.sh" \
  || die "the rendered install.sh pins no release image"

cat >&2 <<EOF

About to publish on Nostr, signed by the "$SIGNER" account (public, hard to retract):
  release   allod@$version  channel $channel  commit ${commit:0:12}
  files     $(cd "$work/dist" && echo *)
  images    $([ "$images" = 1 ] && echo "${IMAGES[*]} ($version)" || echo "skipped")
  git tag   $([ "$channel" = main ] && echo "push v$version to nostr" || echo "none")
EOF
if [ "$yes" != 1 ]; then
  read -r -p "Publish? [y/N] " a </dev/tty
  case $a in y|Y|yes) ;; *) die "nothing published" ;; esac
fi

relay_args=()
for r in "${RELAYS[@]}"; do relay_args+=(--relay "$r"); done

if [ "$channel" = main ]; then
  step "Pushing tag v$version to the nostr remote"
  git push nostr "refs/tags/v$version:refs/tags/v$version" </dev/null
fi

step "Publishing release allod@$version"
tag_args=(); [ "$channel" = main ] && tag_args=(--tag "v$version")
ngit --signer "$SIGNER" release publish "$version" --app allod --channel "$channel" "${tag_args[@]}" \
  --notes "$notes" \
  --platform-agnostic-file "$work/dist/allod-stack-$version.tar.gz" \
  --platform-agnostic-file "$work/dist/install.sh" \
  --platform-agnostic-file "$work/dist/SHA256SUMS" \
  "${relay_args[@]}" </dev/null

if [ "$images" = 1 ]; then
  # Container root is the calling user under rootless Docker.
  uid="$(id -u):$(id -g)"
  case $(docker info -f '{{range .SecurityOptions}}{{.}} {{end}}' 2>/dev/null) in *rootless*) uid=0:0 ;; esac
  mkdir -p "$work/oci"
  for i in "${IMAGES[@]}"; do
    step "Image $i:$version"
    docker run --rm --user "$uid" -v "$work/oci:/out" "$SKOPEO" \
      copy -q --all --preserve-digests "docker://$registry/$i:$version" "oci:/out/$i:$version"
    ngit --signer "$SIGNER" container publish "$i" --layout "$work/oci/$i" \
      --source https://allod.me "${relay_args[@]}" </dev/null
  done
fi

step "Done: allod@$version on channel $channel"
echo "  release:  https://gitworkshop.dev/allod.me/relay.ngit.dev/allod/releases" >&2
[ "$images" = 1 ] && echo "  images:   ncontainer.io/allod.me/<image>:$version" >&2
exit 0
