#!/usr/bin/env bash
# Healthy when ldk-server answers GetNodeInfo with a node:read-only credential.
set -euo pipefail
DATA=${LDK_DATA_DIR:-/data}
grpc=${LDK_GRPC_ADDRESS:-0.0.0.0:3536}
export HOME="$DATA/.healthcheck"
exec ldk-server-cli -b "127.0.0.1:${grpc##*:}" -t "$DATA/tls.crt" get-node-info >/dev/null
