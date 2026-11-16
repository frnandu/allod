# Updating

Upstream is this repository's own release pipeline (SPEC §2.3.8): images
`ghcr.io/frnandu/allod`, `allod-ldk-server` and `allod-gossip`, one tag for all three.

1. Set `imageTag` in `startos/images.ts` to the release tag. For a multi-arch tag, set
   `imageArch` to `['x86_64', 'aarch64']` and `ARCHES := x86 arm` in the `Makefile`.
2. Bump `version` in `startos/versions/current.ts` and its release notes.
3. `make`, install on a StartOS box (`make install`), verify, then publish (README of
   `packaging/`).
