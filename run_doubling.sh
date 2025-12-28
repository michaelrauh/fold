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

n=1
while [[ "$n" -le "$TOTAL_LINES" ]]; do
  echo "=== Running with n=$n (of $TOTAL_LINES lines) ==="
  rm -rf ./fold_state
  mkdir -p ./fold_state/input
  head -n "$n" "$SRC_FILE" > ./fold_state/input/small.txt

  if ! cargo run --release; then
    echo "cargo run failed at n=$n" >&2
    exit 1
  fi

  rm -rf ./fold_state
  n=$((n * 2))
done

echo "Completed runs up to $TOTAL_LINES lines."
