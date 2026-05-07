#!/usr/bin/env bash
set -euo pipefail

# Start a single staged leader in the current shell after building with DWARF
# info for perf. Run this from inside tmux if you want it to stay attached.

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

SCRIPT_DIR="$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

STATE_DIR="$SCRIPT_DIR/fold_state"
APP_BIN="$SCRIPT_DIR/target/release/fold"
CARGO_ENV="$HOME/.cargo/env"
FOLD_OFFLOAD_ENABLED="${FOLD_OFFLOAD_ENABLED:-}"
FOLD_FORCE_ROLE="${FOLD_FORCE_ROLE:-leader}"
FOLD_MERGE_POLICY="${FOLD_MERGE_POLICY:-adjacent_balanced}"

# Default to debuginfo + frame pointers for better perf attribution; allow override via env.
# target-cpu=native enables hardware POPCNT/AVX2/BMI2 on the AMD prod box.
RUSTFLAGS="${RUSTFLAGS:--C force-frame-pointers=yes -C debuginfo=2 -C target-cpu=native}"
export RUSTFLAGS

if [ -f "$CARGO_ENV" ]; then
  # shellcheck disable=SC1090
  source "$CARGO_ENV"
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required; ensure Rust is installed and in PATH" >&2
  exit 1
fi

if [ ! -f "$SCRIPT_DIR/e.txt" ]; then
  echo "e.txt not found in $SCRIPT_DIR" >&2
  exit 1
fi

echo "Building release binary with DWARF + frame pointers for perf..."
cargo build --release --bins
if [ ! -x "$APP_BIN" ]; then
  echo "Binary $APP_BIN not found after build" >&2
  exit 1
fi

echo "Resetting fold_state..."
rm -rf "$STATE_DIR"

echo "Staging e.txt into fold_state/input..."
./stage.sh "$SCRIPT_DIR/e.txt"

if [ -z "${FOLD_MERGE_THREADS:-}" ]; then
  CPU_COUNT=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)
  if [ "${CPU_COUNT:-1}" -le 1 ]; then
    FOLD_MERGE_THREADS=1
  else
    FOLD_MERGE_THREADS=$((CPU_COUNT - 1))
  fi
fi

# Export offload env if set so the app can pick up Spaces/local store config.
# Auto-map legacy SPACES_* vars into FOLD_OFFLOAD_* if the latter are unset.
if [ -z "${FOLD_OFFLOAD_SPACES_BUCKET:-}" ] && [ -n "${SPACES_BUCKET:-}" ]; then
  FOLD_OFFLOAD_SPACES_BUCKET="$SPACES_BUCKET"
fi
if [ -z "${FOLD_OFFLOAD_SPACES_REGION:-}" ] && [ -n "${SPACES_REGION:-}" ]; then
  FOLD_OFFLOAD_SPACES_REGION="$SPACES_REGION"
fi
if [ -z "${FOLD_OFFLOAD_SPACES_ENDPOINT:-}" ] && [ -n "${SPACES_ENDPOINT:-}" ]; then
  FOLD_OFFLOAD_SPACES_ENDPOINT="$SPACES_ENDPOINT"
fi
if [ -z "${FOLD_OFFLOAD_SPACES_PREFIX:-}" ]; then
  FOLD_OFFLOAD_SPACES_PREFIX="runs"
fi
if [ -z "${FOLD_OFFLOAD_SPACES_ACCESS_KEY:-}" ] && [ -n "${SPACES_ACCESS_KEY:-}" ]; then
  FOLD_OFFLOAD_SPACES_ACCESS_KEY="$SPACES_ACCESS_KEY"
fi
if [ -z "${FOLD_OFFLOAD_SPACES_SECRET_KEY:-}" ] && [ -n "${SPACES_SECRET_KEY:-}" ]; then
  FOLD_OFFLOAD_SPACES_SECRET_KEY="$SPACES_SECRET_KEY"
fi
if [ -z "${FOLD_OFFLOAD_CACHE_DIR:-}" ]; then
  FOLD_OFFLOAD_CACHE_DIR="$STATE_DIR/offload_cache"
fi
if [ -n "${FOLD_OFFLOAD_MIN_FILE_BYTES:-}" ]; then export FOLD_OFFLOAD_MIN_FILE_BYTES; fi
if [ -n "${FOLD_OFFLOAD_BATCH_BYTES:-}" ]; then export FOLD_OFFLOAD_BATCH_BYTES; fi

if [ -z "${FOLD_OFFLOAD_ENABLED:-}" ] && [ -n "${FOLD_OFFLOAD_SPACES_ACCESS_KEY:-}" ] && [ -n "${FOLD_OFFLOAD_SPACES_SECRET_KEY:-}" ]; then
  FOLD_OFFLOAD_ENABLED=1
fi

if [ -n "${FOLD_OFFLOAD_ENABLED:-}" ]; then export FOLD_OFFLOAD_ENABLED; fi
if [ -n "${FOLD_OFFLOAD_SPACES_ENDPOINT:-}" ]; then export FOLD_OFFLOAD_SPACES_ENDPOINT; fi
if [ -n "${FOLD_OFFLOAD_SPACES_REGION:-}" ]; then export FOLD_OFFLOAD_SPACES_REGION; fi
if [ -n "${FOLD_OFFLOAD_SPACES_BUCKET:-}" ]; then export FOLD_OFFLOAD_SPACES_BUCKET; fi
if [ -n "${FOLD_OFFLOAD_SPACES_PREFIX:-}" ]; then export FOLD_OFFLOAD_SPACES_PREFIX; fi
if [ -n "${FOLD_OFFLOAD_SPACES_ACCESS_KEY:-}" ]; then export FOLD_OFFLOAD_SPACES_ACCESS_KEY; fi
if [ -n "${FOLD_OFFLOAD_SPACES_SECRET_KEY:-}" ]; then export FOLD_OFFLOAD_SPACES_SECRET_KEY; fi
if [ -n "${FOLD_OFFLOAD_CACHE_DIR:-}" ]; then export FOLD_OFFLOAD_CACHE_DIR; fi
if [ -n "${FOLD_OFFLOAD_CACHE_BYTES_CAP:-}" ]; then export FOLD_OFFLOAD_CACHE_BYTES_CAP; fi
if [ -n "${FOLD_OFFLOAD_LANDING_BYTES_HIGH_WATER:-}" ]; then export FOLD_OFFLOAD_LANDING_BYTES_HIGH_WATER; fi
if [ -n "${FOLD_OFFLOAD_DISK_FREE_LOW_WATER:-}" ]; then export FOLD_OFFLOAD_DISK_FREE_LOW_WATER; fi
if [ -n "${FOLD_OFFLOAD_LOCAL_STORE_DIR:-}" ]; then export FOLD_OFFLOAD_LOCAL_STORE_DIR; fi
if [ -n "${FOLD_OFFLOAD_IN_MEMORY_STORE:-}" ]; then export FOLD_OFFLOAD_IN_MEMORY_STORE; fi
export FOLD_FORCE_ROLE
export FOLD_MERGE_POLICY
export FOLD_MERGE_THREADS

echo "Starting single staged leader in the current shell..."
echo "FOLD_FORCE_ROLE=$FOLD_FORCE_ROLE"
echo "FOLD_MERGE_POLICY=$FOLD_MERGE_POLICY"
echo "FOLD_MERGE_THREADS=$FOLD_MERGE_THREADS"
echo "Legacy two-worker startup: ./start_fold_legacy.sh"
echo "Run this script from inside tmux if you want the session to stay alive."

exec "$APP_BIN"
