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

# Pure doubling over words: 1, 2, 4, ... up to the full word count.
TOTAL_WORDS=$(wc -w < "$SRC_FILE")
if [[ "$TOTAL_WORDS" -eq 0 ]]; then
  echo "Source file has zero words" >&2
  exit 1
fi

# Build the sequence first so we can log/verify each step.
counts=()
w=1
while [[ "$w" -lt "$TOTAL_WORDS" ]]; do
  counts+=("$w")
  w=$((w * 2))
done
counts+=("$TOTAL_WORDS")

for words in "${counts[@]}"; do
  echo "=== Running with $words words (of $TOTAL_WORDS) ==="
  rm -rf ./fold_state
  mkdir -p ./fold_state/input

  python3 - "$SRC_FILE" "$words" <<'PY'
import sys, pathlib
src = pathlib.Path(sys.argv[1]).read_text()
limit = int(sys.argv[2])
tokens = src.split()
subset = " ".join(tokens[:limit])
pathlib.Path("./fold_state/input/small.txt").write_text(subset)
PY

  actual_words=$(wc -w < ./fold_state/input/small.txt)
  if [[ "$actual_words" -ne "$words" ]]; then
    echo "Warning: expected $words words, wrote $actual_words" >&2
  fi

  if ! cargo run --release; then
    echo "cargo run failed at $words words" >&2
    exit 1
  fi

  rm -rf ./fold_state
done

echo "Completed doubling runs up to $TOTAL_WORDS words."
