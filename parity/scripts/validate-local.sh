#!/usr/bin/env bash
# parity/scripts/validate-local.sh
#
# Run parity validation locally before pushing.
# Calls parity-runner with the checked-in golden outputs.
#
# Usage:
#   ./parity/scripts/validate-local.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PARITY_RUNNER="${REPO_ROOT}/target/release/parity-runner"

if [ ! -f "${PARITY_RUNNER}" ]; then
    echo "Building parity-runner..."
    cargo build --release --bin parity-runner --manifest-path "${REPO_ROOT}/Cargo.toml"
fi

exec "${PARITY_RUNNER}" compare ts-baseline \
    --golden "${REPO_ROOT}/parity/golden-outputs" \
    --threshold-counts 0.01 \
    --threshold-mrr 0.10 \
    --threshold-nodes 5
