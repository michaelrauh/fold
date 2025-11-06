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

## Checkpointing

Fold automatically saves checkpoints after processing each file. This allows you to:
- **Resume interrupted processing**: If the program is stopped (Ctrl+C, system crash, etc.), it will automatically resume from the last completed file when restarted
- **Inspect progress**: Checkpoints are saved as human-readable JSON in `fold_state/checkpoint.json`
- **Monitor state**: Each checkpoint includes a timestamp showing when it was created

The checkpoint includes:
- Interner state (vocabulary and phrase mappings)
- All seen ortho IDs
- Current optimal ortho
- Frontier orthos for continuation

Checkpoints are automatically cleared after successful completion. If you want to restart from scratch, simply delete the `checkpoint.json` file.

### Checkpoint timestamps in logs

The program displays checkpoint timestamps in the logs:
```
✓ Checkpoint saved at 2025-11-06T00:27:16.680116869+00:00
```

When resuming from a checkpoint:
```
📦 Checkpoint found from 2025-11-06T00:27:16.680116869+00:00
   Resuming from file index: 2
```

This helps you understand how much progress would be lost if the process is interrupted.

## Logging

The program provides comprehensive logging:
- **File progress**: Shows which file is being processed (e.g., "Processing file 3/5")
- **Queue metrics**: Logs queue length every 1000 orthos processed in the worker loop
- **State metrics**: Shows vocabulary size, seen orthos count, frontier size
- **Redrawing output**: Screen clears between file updates for cleaner visualization

Example log output:
```
[worker] Processed: 1000, Queue: 450, Seen: 1200, Frontier: 85
```

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