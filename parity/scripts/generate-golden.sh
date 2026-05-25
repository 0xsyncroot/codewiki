#!/usr/bin/env bash
# parity/scripts/generate-golden.sh
#
# Regenerate golden outputs from the TypeScript implementation.
# Run this when intentionally changing TS behavior; update metadata.json with new ts_commit.
#
# Usage:
#   ./parity/scripts/generate-golden.sh [--codebase <path>]
#
# Requirements:
#   - Node.js >= 18 with the TypeScript codegraph installed globally
#   - CODEGRAPH_TS_ROOT env var pointing to the TS source checkout

set -euo pipefail

CODEBASE="${1:-}"
GOLDEN_DIR="$(dirname "$0")/../golden-outputs"

echo "Generating golden outputs..."
echo "Golden output directory: ${GOLDEN_DIR}"

if [ -z "${CODEGRAPH_TS_ROOT:-}" ]; then
    echo "Warning: CODEGRAPH_TS_ROOT not set. Skipping actual golden generation."
    echo "Set CODEGRAPH_TS_ROOT to the path of the TypeScript codegraph checkout."
    exit 0
fi

TS_COMMIT="$(cd "${CODEGRAPH_TS_ROOT}" && git rev-parse HEAD)"
echo "TS commit: ${TS_COMMIT}"

# TODO: Run ts-codegraph dump-extraction, dump-queries, dump-resolution, dump-explore commands
# This script is a stub — full implementation in T-224/T-505.

echo "Stub: golden generation not yet implemented."
echo "Update metadata.json with ts_commit=${TS_COMMIT} when real data is generated."
