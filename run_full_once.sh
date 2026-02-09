#!/usr/bin/env bash
set -euo pipefail

# Run a single full-size ingest (no doubling) with a plain `cargo run --release`.
# No piping/tee. After the run (success or panic), collect the relevant logs
# into a timestamped folder under fold_history/full_runs for inspection.

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

TOTAL_WORDS=$(wc -w < "$SRC_FILE")
if [[ "$TOTAL_WORDS" -eq 0 ]]; then
  echo "Source file has zero words" >&2
  exit 1
fi

rm -rf ./fold_state
mkdir -p ./fold_state/input ./fold_history/logs

# Write full text to small.txt as expected by the app
cp "$SRC_FILE" ./fold_state/input/small.txt

log_ts=$(date +"%s")
log_dir="./fold_history/full_runs/full_run_${log_ts}"
mkdir -p "$log_dir"

collect_logs() {
  # Grab current log files and latest run summaries for post-mortem.
  for f in ./fold_history/logs/*.log; do
    [ -f "$f" ] && cp "$f" "$log_dir/" || true
  done

  # Copy a few most recent run_* txt files (leader/follower) if present.
  local run_files
  run_files=$(ls -t fold_history/run_*.txt 2>/dev/null | head -n5 || true)
  for f in $run_files; do
    cp "$f" "$log_dir/" || true
  done
}

trap collect_logs EXIT

echo "=== Running full ingest with ${TOTAL_WORDS} words from ${SRC_FILE} ==="
echo "Logs (if any) will be gathered into ${log_dir} after the run"

# Default: TUI enabled. Respect caller-provided FOLD_DISABLE_TUI if set.
FOLD_DISABLE_TUI=${FOLD_DISABLE_TUI:-0} \
RUST_BACKTRACE=1 \
cargo run --release

echo "Completed full ingest. Collected logs (if any) are in ${log_dir}"
