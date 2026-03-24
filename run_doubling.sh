#!/usr/bin/env bash
set -euo pipefail

# Iterate input sizes by doubling (1, 2, 4, 8, ...) and run `cargo run --release`
# for each size, resetting fold_state between runs and collecting a dedicated
# artifact bundle for each step.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$SCRIPT_DIR/run_bundle_lib.sh"

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

SRC_FILE="${1:-e.txt}"
if [[ ! -f "$SRC_FILE" ]]; then
  echo "Source file '$SRC_FILE' not found" >&2
  exit 1
fi

TOTAL_LINES=$(wc -l < "$SRC_FILE")
if [[ "$TOTAL_LINES" -eq 0 ]]; then
  echo "Source file is empty" >&2
  exit 1
fi

# Pure doubling over words: 1, 2, 4, ... up to the full word count.
TOTAL_WORDS=$(wc -w < "$SRC_FILE")
if [[ "$TOTAL_WORDS" -eq 0 ]]; then
  echo "Source file has zero words" >&2
  exit 1
fi

SESSION_TS=$(date +"%s")
SOURCE_PATH=$(run_bundle_abs_path "$SRC_FILE")
SOURCE_STEM=$(run_bundle_safe_stem "$SRC_FILE")
SESSION_ID="${SOURCE_STEM}_ts${SESSION_TS}"
SESSION_DIR="./fold_history/doubling_runs/session_${SESSION_ID}"
OFFLOAD_PREFIX="${FOLD_OFFLOAD_SPACES_PREFIX:-runs}"

mkdir -p "$SESSION_DIR" ./fold_history/logs

cat > "${SESSION_DIR}/session.txt" <<EOF
session_id: ${SESSION_ID}
source_file: ${SOURCE_PATH}
total_words: ${TOTAL_WORDS}
offload_prefix: ${OFFLOAD_PREFIX}
EOF

# Build the sequence first so we can log/verify each step.
counts=()
w=1
while [[ "$w" -lt "$TOTAL_WORDS" ]]; do
  counts+=("$w")
  w=$((w * 2))
done
counts+=("$TOTAL_WORDS")

echo "=== Doubling session ${SESSION_ID} ==="
echo "Source: ${SOURCE_PATH}"
echo "Steps: ${#counts[@]}"
echo "Bundle: ${SESSION_DIR}"

step_index=0
for words in "${counts[@]}"; do
  step_index=$((step_index + 1))
  step_label=$(printf 'step_%03d_words=%s' "$step_index" "$words")
  step_dir="${SESSION_DIR}/${step_label}"
  staged_stem="${SOURCE_STEM}_w${words}_ts${SESSION_TS}"
  staged_file="${staged_stem}.txt"
  transcript_path="${step_dir}/console.typescript"
  fatal_path="${step_dir}/fatal.log"
  run_manifest_path="${step_dir}/.run_files_before.txt"
  metadata_path="${step_dir}/metadata.txt"
  status_path="${step_dir}/status.txt"
  tui_signature_before=$(run_bundle_file_signature "./fold_history/logs/tui_state.log")

  mkdir -p "$step_dir"
  run_bundle_record_run_files "$run_manifest_path"

  echo "=== Doubling ${step_index}/${#counts[@]}: ${words} words ==="
  echo "Staged: ${staged_file}"
  echo "Bundle: ${step_dir}"

  rm -rf ./fold_state
  mkdir -p ./fold_state/input

  python3 - "$SRC_FILE" "$words" "./fold_state/input/${staged_file}" <<'PY'
import sys, pathlib
src = pathlib.Path(sys.argv[1]).read_text()
limit = int(sys.argv[2])
tokens = src.split()
subset = " ".join(tokens[:limit])
pathlib.Path(sys.argv[3]).write_text(subset)
PY

  actual_words=$(wc -w < "./fold_state/input/${staged_file}")
  if [[ "$actual_words" -ne "$words" ]]; then
    echo "Warning: expected $words words, wrote $actual_words" >&2
  fi

  cmd=(
    env
    "FOLD_RUN_ID=${staged_stem}"
    "FOLD_RUN_SOURCE_FILE=${SOURCE_PATH}"
    "FOLD_RUN_STAGE_FILE=${staged_file}"
    "FOLD_RUN_INPUT_WORDS=${actual_words}"
    "FOLD_RUN_BUNDLE_DIR=${step_dir}"
    "FOLD_RUN_TRANSCRIPT_PATH=${transcript_path}"
    "FOLD_FATAL_LOG_PATH=${fatal_path}"
    "RUST_BACKTRACE=1"
  )

  if [[ -n "${FOLD_DISABLE_TUI+x}" ]]; then
    cmd+=("FOLD_DISABLE_TUI=${FOLD_DISABLE_TUI}")
  fi

  cmd+=(cargo run --release)

  start_ts=$(date +"%s")
  set +e
  run_bundle_run_with_transcript "$transcript_path" "${cmd[@]}"
  run_exit_code=$?
  set -e
  end_ts=$(date +"%s")

  run_bundle_copy_new_run_files "$run_manifest_path" "$step_dir"
  run_bundle_copy_tui_snapshot_if_changed "$tui_signature_before" "$step_dir"

  if [[ "$run_exit_code" -eq 0 ]]; then
    run_status="success"
  else
    run_status="failed"
  fi

  run_bundle_write_metadata \
    "$metadata_path" \
    "$run_status" \
    "$run_exit_code" \
    "$SOURCE_PATH" \
    "$staged_file" \
    "$actual_words" \
    "$start_ts" \
    "$end_ts" \
    "$OFFLOAD_PREFIX" \
    "$staged_stem" \
    "$transcript_path" \
    "$fatal_path"
  run_bundle_write_status "$status_path" "$run_status" "$run_exit_code"

  if [[ "$run_exit_code" -ne 0 ]]; then
    echo "!!! DOUBLING STEP FAILED run_id=${staged_stem} words=${actual_words} bundle=${step_dir}" >&2
    echo "!!! Transcript: ${transcript_path}" >&2
    echo "!!! Fatal log: ${fatal_path}" >&2
    exit "$run_exit_code"
  fi

  rm -rf ./fold_state
done

echo "Completed doubling runs up to $TOTAL_WORDS words. Session: ${SESSION_DIR}"
