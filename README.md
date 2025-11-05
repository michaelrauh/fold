# fold

A text processing system that generates and optimizes ortho structures from text input.

## Overview

Fold reads text files, builds an interner vocabulary, and generates ortho structures through iterative expansion. It tracks and reports the optimal ortho configuration based on volume calculation.

## Building

```bash
cargo build --release
```

## Usage

1. **Prepare input files** using the `stage.sh` script:

```bash
./stage.sh <input_file> <delimiter> [min_length] [state_dir]
```

Example:
```bash
./stage.sh book.txt "CHAPTER" 50000 ./fold_state
```

This splits the input file by delimiter and places chunks in `fold_state/input/`.

2. **Run fold** to process the staged files:

```bash
cargo run --release
# or
./target/release/fold
```

The program will:
- Read all `.txt` files from `fold_state/input/` (sorted alphabetically)
- Process each file by building an interner and running the worker loop
- Print the optimal ortho after each file
- Print the final optimal ortho at the end

## Configuration

Set the `FOLD_STATE_DIR` environment variable to use a different state directory:

```bash
FOLD_STATE_DIR=/path/to/state ./target/release/fold
```

Default: `./fold_state`

## Architecture

- **Interner**: Builds and maintains vocabulary and phrase prefix mappings across versions
- **Ortho**: Multi-dimensional structures that track token combinations
- **Worker Loop**: Processes orthos by intersecting requirements with interner completions, generating children until the queue is empty
- **Optimal Tracking**: Identifies and reports the ortho with maximum volume (product of dimension sizes minus 1)

## Testing

```bash
# Run all tests
cargo test

# Run only library tests
cargo test --lib

# Run only integration tests
cargo test --test integration_test
```

## Example

```bash
# Create test data
mkdir -p fold_state/input
echo "the quick brown fox jumps over the lazy dog" > fold_state/input/test.txt

# Run
cargo run --release
```

Output will show processing progress and the optimal ortho with its dimensions, volume, and tokens.