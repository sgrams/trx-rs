#!/usr/bin/env bash
# Run trx-server with the dummy backend for development and testing.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

exec cargo run --manifest-path "$PROJECT_ROOT/Cargo.toml" \
    -p trx-server -- \
    --rig dummy \
    --access serial \
    "/dev/null 9600" \
    "$@"
