#!/usr/bin/env bash
set -euo pipefail

# Start two fold instances in tmux after staging fresh input and building with DWARF info for perf.

SCRIPT_DIR="$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

TMUX_SESSION="${TMUX_SESSION:-fold}"
TMUX_SOCKET="${TMUX_SOCKET:-/tmp/fold_tmux.sock}"
STATE_DIR="$SCRIPT_DIR/fold_state"
APP_BIN="$SCRIPT_DIR/target/release/fold"
CARGO_ENV="$HOME/.cargo/env"

# Default to debuginfo + frame pointers for better perf attribution; allow override via env.
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

# Ensure no stale sessions/servers (both default and custom socket).
tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
tmux -S "$TMUX_SOCKET" kill-server 2>/dev/null || true

ENV_INIT="if [ -f \"$CARGO_ENV\" ]; then source \"$CARGO_ENV\"; fi; cd \"$SCRIPT_DIR\""
APP_CMD="$ENV_INIT; \"$APP_BIN\""

echo "Starting tmux session $TMUX_SESSION with two fold instances..."
TMUX= tmux -S "$TMUX_SOCKET" new-session -d -s "$TMUX_SESSION" -n "fold-1" "$APP_CMD"
TMUX= tmux -S "$TMUX_SOCKET" new-window -d -t "$TMUX_SESSION:1" -n "fold-2" "$APP_CMD"

echo "Attach with: tmux -S $TMUX_SOCKET attach -t $TMUX_SESSION"
