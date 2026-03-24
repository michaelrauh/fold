#!/usr/bin/env bash
# shellcheck shell=bash

run_bundle_safe_stem() {
  local input="$1"
  local stem
  stem=$(basename "$input")
  stem="${stem%.*}"
  stem=$(printf '%s' "$stem" | tr -cs '[:alnum:]' '_')
  stem="${stem##_}"
  stem="${stem%%_}"
  if [[ -z "$stem" ]]; then
    stem="input"
  fi
  printf '%s\n' "$stem"
}

run_bundle_abs_path() {
  local input="$1"
  local dir
  local base
  dir=$(cd "$(dirname "$input")" && pwd -P)
  base=$(basename "$input")
  printf '%s/%s\n' "$dir" "$base"
}

run_bundle_file_signature() {
  local path="$1"
  local size
  local checksum
  if [[ ! -f "$path" ]]; then
    printf 'absent\n'
    return
  fi

  size=$(wc -c < "$path" | tr -d '[:space:]')
  checksum=$(cksum < "$path" | awk '{print $1 ":" $2}')
  printf '%s:%s\n' "$size" "$checksum"
}

run_bundle_record_run_files() {
  local manifest="$1"
  : > "$manifest"

  shopt -s nullglob
  local path
  for path in ./fold_history/run_*.txt; do
    printf '%s\n' "$path" >> "$manifest"
  done
  shopt -u nullglob
}

run_bundle_copy_new_run_files() {
  local manifest="$1"
  local dest_dir="$2"

  shopt -s nullglob
  local path
  for path in ./fold_history/run_*.txt; do
    if ! grep -Fqx -- "$path" "$manifest"; then
      cp "$path" "$dest_dir/"
    fi
  done
  shopt -u nullglob
}

run_bundle_copy_tui_snapshot_if_changed() {
  local previous_signature="$1"
  local dest_dir="$2"
  local tui_log="./fold_history/logs/tui_state.log"
  local current_signature

  if [[ ! -f "$tui_log" ]]; then
    return
  fi

  current_signature=$(run_bundle_file_signature "$tui_log")
  if [[ "$current_signature" != "$previous_signature" ]]; then
    cp "$tui_log" "$dest_dir/"
  fi
}

run_bundle_write_metadata() {
  local output_path="$1"
  local status="$2"
  local exit_code="$3"
  local source_file="$4"
  local staged_file="$5"
  local input_words="$6"
  local start_ts="$7"
  local end_ts="$8"
  local offload_prefix="$9"
  local run_id="${10}"
  local transcript_path="${11}"
  local fatal_path="${12}"

  cat > "$output_path" <<EOF
run_id: $run_id
status: $status
exit_code: $exit_code
source_file: $source_file
staged_file: $staged_file
input_words: $input_words
start_ts: $start_ts
end_ts: $end_ts
offload_prefix: $offload_prefix
transcript_path: $transcript_path
fatal_path: $fatal_path
EOF
}

run_bundle_write_status() {
  local output_path="$1"
  local status="$2"
  local exit_code="$3"

  cat > "$output_path" <<EOF
status: $status
exit_code: $exit_code
EOF
}

run_bundle_run_with_transcript() {
  local transcript_path="$1"
  shift

  if ! command -v script >/dev/null 2>&1; then
    echo "Required command 'script' not found" >&2
    return 127
  fi

  if script --version >/dev/null 2>&1; then
    local command_string
    printf -v command_string '%q ' "$@"
    script -qefc "$command_string" "$transcript_path"
  else
    script -qFe "$transcript_path" "$@"
  fi
}
