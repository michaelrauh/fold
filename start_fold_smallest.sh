#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

export FOLD_MERGE_POLICY="${FOLD_MERGE_POLICY:-smallest_smallest}"

echo "Starting staged leader experiment with FOLD_MERGE_POLICY=$FOLD_MERGE_POLICY"
exec "$SCRIPT_DIR/start_fold.sh"
