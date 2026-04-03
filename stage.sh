#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

STAGE_BIN="$SCRIPT_DIR/target/release/stage_chunks"
CARGO_ENV="$HOME/.cargo/env"

if [ -f .env ]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

if [ -x "$STAGE_BIN" ]; then
  exec "$STAGE_BIN" "$@"
fi

if [ -f "$CARGO_ENV" ]; then
  # shellcheck disable=SC1090
  source "$CARGO_ENV"
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required; ensure Rust is installed and in PATH" >&2
  exit 1
fi

exec cargo run --quiet --release --bin stage_chunks -- "$@"
