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

Fold automatically saves checkpoints after processing each file and every 100k orthos. This allows you to:
- **Resume interrupted processing**: If the program is stopped (Ctrl+C, system crash, etc.), it will automatically resume from the last completed file when restarted
- **Inspect progress**: Checkpoints are saved in binary format in `fold_state/checkpoint.bin`
- **Monitor state**: Each checkpoint includes a timestamp showing when it was created

The checkpoint includes:
- Interner state (vocabulary and phrase mappings)
- All seen ortho IDs
- Current optimal ortho
- Frontier orthos for continuation
- Processed count for resumption

Checkpoints are automatically cleared after successful completion. If you want to restart from scratch, simply delete the `checkpoint.bin` file.

**Note**: Input files are automatically deleted after being successfully processed and checkpointed, so the checkpoint is the only way to resume processing.

### Checkpoint timestamps in logs

The program displays checkpoint timestamps in the display:
```
Last checkpoint: 2025-11-06T00:27:16.680116869+00:00
```

This helps you understand how much progress would be lost if the process is interrupted.

## Logging

The program provides comprehensive logging with a non-scrolling display:
- **File progress**: Shows which file is being processed at the top (e.g., "File 3/5: filename.txt")
- **State metrics**: Shows vocabulary size, seen orthos count, frontier size below the file info
- **Worker progress**: Shows total orthos processed
- **Non-scrolling output**: Screen clears and redraws in place for cleaner visualization
- **Checkpoint timestamps**: Displays when last checkpoint was saved

The display format:
```
╔══════════════════════════════════════════════════════════════════╗
║ File 2/5: chapter2.txt                                           ║
╚══════════════════════════════════════════════════════════════════╝

Interner version: 2
Vocabulary size: 1523
Total orthos generated: 45231
Frontier size: 1842

─── Worker Progress ───
Processed: 45231

Last checkpoint: 2025-11-06T01:47:12.070835822+00:00
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