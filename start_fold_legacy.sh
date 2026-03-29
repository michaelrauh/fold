#!/bin/bash
set -euo pipefail

# Legacy local workflow: start two fold instances in tmux after staging fresh
# input and building with DWARF info for perf.

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

SCRIPT_DIR="$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

TMUX_SESSION="${TMUX_SESSION:-fold}"
TMUX_SOCKET="${TMUX_SOCKET:-/tmp/fold_tmux.sock}"
STATE_DIR="$SCRIPT_DIR/fold_state"
APP_BIN="$SCRIPT_DIR/target/release/fold"
CARGO_ENV="$HOME/.cargo/env"
FOLD_OFFLOAD_ENABLED="${FOLD_OFFLOAD_ENABLED:-}"

RUSTFLAGS="${RUSTFLAGS:--C force-frame-pointers=yes -C debuginfo=2}"
export RUSTFLAGS

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required to run this script" >&2
  exit 1
fi

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
cargo build --release
if [ ! -x "$APP_BIN" ]; then
  echo "Binary $APP_BIN not found after build" >&2
  exit 1
fi

echo "Resetting fold_state..."
rm -rf "$STATE_DIR"

echo "Staging e.txt into fold_state/input..."
./stage.sh "$SCRIPT_DIR/e.txt"

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

tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
tmux -S "$TMUX_SOCKET" kill-server 2>/dev/null || true

ENV_INIT="if [ -f \"$CARGO_ENV\" ]; then source \"$CARGO_ENV\"; fi; cd \"$SCRIPT_DIR\""
APP_CMD="$ENV_INIT; \"$APP_BIN\""

echo "Starting legacy tmux session $TMUX_SESSION with two fold instances..."
TMUX= tmux -S "$TMUX_SOCKET" new-session -d -s "$TMUX_SESSION" -n "fold-1" "$APP_CMD"
TMUX= tmux -S "$TMUX_SOCKET" new-window -d -t "$TMUX_SESSION:1" -n "fold-2" "$APP_CMD"
TMUX= tmux -S "$TMUX_SOCKET" set-window-option -g remain-on-exit on

echo "Attach with: tmux -S $TMUX_SOCKET attach -t $TMUX_SESSION"
