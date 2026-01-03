#!/usr/bin/env bash
set -euo pipefail

# Run the exporter in release mode.
cargo run --release --bin export_data "$@"
