#!/usr/bin/env bash
set -euo pipefail

# Iterate input sizes by doubling (1, 2, 4, 8, ...) and run `cargo run --release`
# for each size, resetting fold_state between runs.

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

# Fixed line counts for ~5k and ~10k words in e.txt (approx)
START_LINES=507
END_LINES=1026
SAMPLES=10  # inclusive count

samples=()
if [[ "$START_LINES" -le "$END_LINES" && "$END_LINES" -le "$TOTAL_LINES" ]]; then
  step=$(( (END_LINES - START_LINES) / (SAMPLES - 1) ))
  for ((i=0; i<SAMPLES; i++)); do
    n=$((START_LINES + i * step))
    samples+=("$n")
  done
else
  echo "Start/end lines exceed file; falling back to start/end only" >&2
  samples=("$START_LINES" "$END_LINES")
fi

# Sort and dedupe
IFS=$'\n' read -r -d '' -a samples < <(printf "%s\n" "${samples[@]}" | sort -n -u && printf '\0')

for n in "${samples[@]}"; do
  echo "=== Running with n=$n (of $TOTAL_LINES lines) ==="
  rm -rf ./fold_state
  mkdir -p ./fold_state/input
  head -n "$n" "$SRC_FILE" > ./fold_state/input/small.txt

  if ! cargo run --release; then
    echo "cargo run failed at n=$n" >&2
    exit 1
  fi

  rm -rf ./fold_state
done

echo "Completed runs up to $TOTAL_LINES lines."
