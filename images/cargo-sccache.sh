#!/bin/sh
# cargo, through sccache when the GitHub Actions cache is reachable. The release and
# nightly workflows pass ACTIONS_RESULTS_URL and ACTIONS_RUNTIME_TOKEN as build secrets,
# so a crate compiled by an earlier run (any image, same target and flags) is downloaded
# instead of rebuilt when a Docker layer misses. Everywhere else this is plain cargo.
set -eu
if [ -z "${ACTIONS_RESULTS_URL:-}" ] || [ -z "${ACTIONS_RUNTIME_TOKEN:-}" ] \
  || ! command -v sccache >/dev/null; then
  exec cargo "$@"
fi
export SCCACHE_GHA_ENABLED=on
# A cache that cannot be reached must not fail the build: fall back to plain cargo.
if ! sccache --start-server; then
  echo "sccache unavailable, building without it" >&2
  exec cargo "$@"
fi
export RUSTC_WRAPPER=sccache
status=0
cargo "$@" || status=$?
# Stopping the server flushes pending cache writes and prints the hit rate.
sccache --stop-server || true
exit "$status"
