#!/usr/bin/env bash
set -euo pipefail

# Run a single full-size ingest (no doubling) with a plain `cargo run --release`.
# Run inside a PTY so the TUI stays live, and collect only the artifacts
# created by this run into a unique folder under fold_history/full_runs.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/run_bundle_lib.sh"

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi
run_bundle_export_perf_rustflags

SRC_FILE="${1:-e.txt}"
if [[ ! -f "$SRC_FILE" ]]; then
  echo "Source file '$SRC_FILE' not found" >&2
  exit 1
fi

TOTAL_WORDS=$(wc -w < "$SRC_FILE")
if [[ "$TOTAL_WORDS" -eq 0 ]]; then
  echo "Source file has zero words" >&2
  exit 1
fi

RUN_TS=$(date +"%s")
SOURCE_PATH=$(run_bundle_abs_path "$SRC_FILE")
SOURCE_STEM=$(run_bundle_safe_stem "$SRC_FILE")
RUN_ID="${SOURCE_STEM}_w${TOTAL_WORDS}_ts${RUN_TS}"
STAGED_FILE="${RUN_ID}.txt"
LOG_DIR="./fold_history/full_runs/full_run_${RUN_ID}"
TRANSCRIPT_PATH="${LOG_DIR}/console.typescript"
FATAL_PATH="${LOG_DIR}/fatal.log"
RUN_MANIFEST_PATH="${LOG_DIR}/.run_files_before.txt"
STATUS_PATH="${LOG_DIR}/status.txt"
METADATA_PATH="${LOG_DIR}/metadata.txt"
OFFLOAD_PREFIX="${FOLD_OFFLOAD_SPACES_PREFIX:-runs}"
TUI_SIGNATURE_BEFORE=$(run_bundle_file_signature "./fold_history/logs/tui_state.log")

mkdir -p "$LOG_DIR" ./fold_history/logs
run_bundle_record_run_files "$RUN_MANIFEST_PATH"

rm -rf ./fold_state
mkdir -p ./fold_state/input
cp "$SRC_FILE" "./fold_state/input/${STAGED_FILE}"

echo "=== Full run ${RUN_ID} ==="
echo "Source: ${SOURCE_PATH}"
echo "Staged: ${STAGED_FILE}"
echo "Words: ${TOTAL_WORDS}"
echo "Bundle: ${LOG_DIR}"

cmd=(
  env
  "FOLD_RUN_ID=${RUN_ID}"
  "FOLD_RUN_SOURCE_FILE=${SOURCE_PATH}"
  "FOLD_RUN_STAGE_FILE=${STAGED_FILE}"
  "FOLD_RUN_INPUT_WORDS=${TOTAL_WORDS}"
  "FOLD_RUN_BUNDLE_DIR=${LOG_DIR}"
  "FOLD_RUN_TRANSCRIPT_PATH=${TRANSCRIPT_PATH}"
  "FOLD_FATAL_LOG_PATH=${FATAL_PATH}"
  "RUST_BACKTRACE=1"
)

if [[ -n "${FOLD_DISABLE_TUI+x}" ]]; then
  cmd+=("FOLD_DISABLE_TUI=${FOLD_DISABLE_TUI}")
fi

cmd+=(cargo run --release)

START_TS=$(date +"%s")
set +e
run_bundle_run_with_transcript "$TRANSCRIPT_PATH" "${cmd[@]}"
RUN_EXIT_CODE=$?
set -e
END_TS=$(date +"%s")

run_bundle_copy_new_run_files "$RUN_MANIFEST_PATH" "$LOG_DIR"
run_bundle_copy_tui_snapshot_if_changed "$TUI_SIGNATURE_BEFORE" "$LOG_DIR"

if [[ "$RUN_EXIT_CODE" -eq 0 ]]; then
  RUN_STATUS="success"
else
  RUN_STATUS="failed"
fi

run_bundle_write_metadata \
  "$METADATA_PATH" \
  "$RUN_STATUS" \
  "$RUN_EXIT_CODE" \
  "$SOURCE_PATH" \
  "$STAGED_FILE" \
  "$TOTAL_WORDS" \
  "$START_TS" \
  "$END_TS" \
  "$OFFLOAD_PREFIX" \
  "$RUN_ID" \
  "$TRANSCRIPT_PATH" \
  "$FATAL_PATH"
run_bundle_write_status "$STATUS_PATH" "$RUN_STATUS" "$RUN_EXIT_CODE"

if [[ "$RUN_EXIT_CODE" -ne 0 ]]; then
  echo "!!! FULL RUN FAILED run_id=${RUN_ID} words=${TOTAL_WORDS} bundle=${LOG_DIR}" >&2
  echo "!!! Transcript: ${TRANSCRIPT_PATH}" >&2
  echo "!!! Fatal log: ${FATAL_PATH}" >&2
  exit "$RUN_EXIT_CODE"
fi

echo "Completed full ingest. Bundle: ${LOG_DIR}"
