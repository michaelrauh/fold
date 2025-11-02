# fold

A single-node ortho generator using branch-and-bound search with vocabulary-based phrase completion.

## Overview

Fold generates orthogonal structures (orthos) through systematic exploration of a vocabulary space. It uses a branch-and-bound search to efficiently navigate the search tree, maintaining a frontier of lead nodes while pruning non-lead nodes using prefix detection.

## Building

```bash
cargo build --release
```

The binary will be created at `target/release/fold_single`.

## Usage

Fold operates in a single automatic mode that processes text files from an input folder.

### Quick Start

1. **Stage your text files** using the provided script:
   ```bash
   ./stage.sh book.txt "CHAPTER" 50000
   ```
   This splits `book.txt` by the delimiter "CHAPTER" with a maximum chunk size of 50,000 characters, placing the chunks into `./fold_state/input/`.

2. **Run the processor**:
   ```bash
   cargo run --release
   ```
   Or use the built binary:
   ```bash
   ./target/release/fold_single
   ```

The program will automatically:
- Process all `.txt` files in `./fold_state/input/`
- For each file: ingest vocabulary → run worker → delete file
- Continue until the input folder is empty
- Save state to `./fold_state/fold_resume.bin` after each file

### State Directory Structure

```
./fold_state/
├── fold_resume.bin      # State file (frontier + interner)
└── input/               # Text files to process
    ├── file1.txt
    └── file2.txt
```

### Staging Script

The `stage.sh` script prepares large files for processing:

```bash
./stage.sh <input_file> <delimiter> [max_length] [state_dir]
```

**Parameters:**
- `input_file`: Path to the file to split
- `delimiter`: Word or phrase to split on (case-insensitive, e.g., "CHAPTER")
- `max_length`: Optional maximum chunk size in characters (default: unlimited)
- `state_dir`: Optional state directory path (default: ./fold_state)

**Example:**
```bash
# Split a book by chapters, max 50k characters per chunk
./stage.sh book.txt "CHAPTER" 50000

# Run the processor
cargo run --release
```

## Progress Logging

The program provides detailed progress information:

```
[process] FILE 3/10 (30.0% complete)
[process] Name: chapter_003.txt
[process] Timestamp: 1762106040

[ingest] Read 5234 characters from file
[ingest] New interner v4, vocab size: 127 (+15 new words)
[ingest] CHECKPOINT SAVED at timestamp 1762106041 (duration: 1s)
[ingest] Ingest complete - safe to stop before next stage

[run] Progress: processed 500/1000 orthos (50.0%), queue: 324, frontier: 892
[run] CHECKPOINT SAVED at timestamp 1762106055 (duration: 14s)
[run] Run complete - safe to stop before next stage
```

**Checkpoint Indicators:**
- Clear "CHECKPOINT SAVED" messages show when state is safely saved
- UNIX epoch timestamps allow recovery point identification
- Duration metrics help estimate remaining time
- "safe to stop before next stage" indicates when you can safely interrupt

## How It Works

### Processing Pipeline

For each file in the input folder:

1. **Ingest**: Reads the text file and updates the interner with new vocabulary
   - Detects vocabulary changes
   - Identifies affected orthos in the frontier
   - Saves updated state (checkpoint)

2. **Run**: Executes the worker loop to generate orthos
   - Loads frontier and interner
   - Adds a seed ortho to explore vocabulary at origin
   - Processes all orthos in the work queue
   - Generates child orthos based on vocabulary completions
   - Deduplicates the frontier using the prefix rule
   - Saves updated state (checkpoint)

3. **Cleanup**: Deletes the processed file

### Resume File

The system maintains state in `./fold_state/fold_resume.bin` which contains:
- **Frontier**: A Vec of Ortho objects representing the current search frontier
- **Interner**: The vocabulary and phrase completion mappings

State is automatically preserved across runs, allowing you to:
- Stop and resume processing at any time (after checkpoints)
- Add more files to the input folder and continue
- Incrementally expand vocabulary across multiple runs

### Frontier Management

The frontier represents all discovered orthos in the branch-and-bound search tree, with non-lead nodes removed:
- **Lead nodes**: Orthos that are not prefixes of other orthos with the same shape
- **Non-lead nodes**: Orthos whose canonicalized payloads are prefixes of others (removed during deduplication)

### Deduplication

The prefix rule identifies non-lead nodes:
- Group orthos by shape (dims)
- For each group, detect if an ortho's payload is a prefix of another
- Keep only lead nodes (those not prefixes of others)

## Testing

```bash
# Run all tests
cargo test

# Run only fold_single tests
cargo test --bin fold_single

# Run benchmarks
cargo bench
```

## Architecture

- **ortho**: Core ortho data structure and operations
- **spatial**: Spatial indexing and dimension management
- **interner**: Vocabulary and phrase completion mapping
- **splitter**: Text tokenization and phrase extraction
- **error**: Error types and handling
- **fold_single**: Main binary with automatic batch processing
