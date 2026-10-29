#!/usr/bin/env bash
# Healthy when ldk-server answers GetNodeInfo with its own admin credentials.
set -euo pipefail
DATA=${LDK_DATA_DIR:-/data}
mac="$DATA/${LDK_NETWORK}/macaroons/admin.macaroon"
grpc=${LDK_GRPC_ADDRESS:-0.0.0.0:3536}
exec ldk-server-cli -b "127.0.0.1:${grpc##*:}" -t "$DATA/tls.crt" -m "$(cat "$mac")" get-node-info >/dev/null
