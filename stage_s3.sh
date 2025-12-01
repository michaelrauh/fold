#!/usr/bin/env bash
# stage_s3.sh - Split a file locally and upload chunks to S3/Spaces shared state
#
# Usage: ./stage_s3.sh <input_file> [min_length] [state_dir]
#   input_file  - Path to the file to split
#   min_length  - Optional: Minimum length in words to keep chunk (default: 2)
#   state_dir   - Optional: State directory path (default: ./fold_state)
#
# Env required:
#   FOLD_S3_BUCKET   - Target bucket/Space name
#   FOLD_S3_REGION   - Region (e.g., us-east-1 or nyc3)
#   FOLD_S3_PREFIX   - Prefix inside the bucket (default: fold)
#   FOLD_S3_ENDPOINT - Optional: Custom endpoint (e.g., https://nyc3.digitaloceanspaces.com)

set -euo pipefail

INPUT_FILE="${1:-}"
MIN_LENGTH="${2:-2}"
STATE_DIR="${3:-./fold_state}"

[ -n "$INPUT_FILE" ] || { echo "error: input file required" >&2; exit 1; }
[ -n "${FOLD_S3_BUCKET:-}" ] || { echo "error: FOLD_S3_BUCKET not set" >&2; exit 1; }

REGION="${FOLD_S3_REGION:-us-east-1}"
PREFIX="${FOLD_S3_PREFIX:-fold}"
ENDPOINT="${FOLD_S3_ENDPOINT:-}"

command -v aws >/dev/null 2>&1 || { echo "error: aws CLI not found" >&2; exit 1; }

SCRIPT_DIR="$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "[stage_s3] Running local split via stage.sh..."
"$SCRIPT_DIR/stage.sh" "$INPUT_FILE" "$MIN_LENGTH" "$STATE_DIR"

DEST="s3://${FOLD_S3_BUCKET}/${PREFIX}/input/"
echo "[stage_s3] Uploading chunks to ${DEST} (region=${REGION}${ENDPOINT:+, endpoint=$ENDPOINT})"

aws_args=(--region "$REGION")
if [ -n "$ENDPOINT" ]; then
  aws_args+=(--endpoint-url "$ENDPOINT")
fi

aws "${aws_args[@]}" s3 sync "${STATE_DIR}/input/" "$DEST" --only-show-errors

echo "[stage_s3] Done. Remote workers with FOLD_S3_* set will see these chunks."
