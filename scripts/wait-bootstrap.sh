#!/usr/bin/env bash
# Waits for the regtest bootstrap container to finish; exits with its exit code.
# Usage: scripts/wait-bootstrap.sh <compose command...>
set -euo pipefail
for _ in $(seq 1 450); do
  id=$("$@" ps -aq bootstrap)
  if [ -n "$id" ]; then
    state=$(docker inspect -f '{{.State.Status}} {{.State.ExitCode}}' "$id")
    case "$state" in
      exited\ *)
        code=${state#exited }
        if [ "$code" != 0 ]; then "$@" logs bootstrap >&2; fi
        exit "$code"
        ;;
    esac
  fi
  sleep 2
done
echo "bootstrap did not finish in 15 minutes" >&2
exit 1
