# fold

A text processing system that finds optimal orthogonal structures in text using spatial algorithms.

## Overview

Fold processes text files by:
1. Staging raw text into processable chunks (`stage.sh` → `fold_state/input/`)
2. Building an interner (vocabulary and phrase completion mappings) from staged text
3. Generating orthogonal structures (orthos) through a work queue
4. Selecting the optimal ortho based on dimensional scoring

See `ARCHITECTURE.md` for the full pipeline and component details.

## Usage

### Prepare Input Files

Use `stage.sh` to split a large text file into chunks:

```bash
./stage.sh <input_file> [min_length] [state_dir]
```

Example:
```bash
./stage.sh book.txt 10 ./fold_state
```

This splits `book.txt` into sentence-level chunks (paragraphs, then `. ? ; ! ,` delimiters), removes chunks shorter than `min_length` words (default 2), and places the results in `./fold_state/input/`.

### Run Fold

Process staged files and merge archives:

```bash
cargo run --release
```

The program will:
- Check `fold_state/in_process/` for any abandoned `.txt` files or stale heartbeats from previous runs and recover them
- If two or more archives exist, merge the two largest by ortho count (rehydrate interners/results, remap the smaller vocab, replay impacted orthos)
- Otherwise, pull the next-largest `.txt`, move it to `fold_state/in_process/`, and ingest it
- Create a heartbeat file for each job that is updated every 100,000 orthos processed
- Build an interner, explore orthos via disk-backed work/results queues, and update the TUI as work progresses
- Save each finished archive (`archive_*.bin/`) back into `fold_state/input/` (ready to be merged); delete the work folder (including the original txt) after success
- Continue looping until no `.txt` files remain and fewer than two archives are present

Each archive is a directory containing:
- `interner.bin`: The interner built from that specific file
- `results/`: DiskBackedQueue directory with all ortho results
- `optimal.txt`: Formatted text of the optimal ortho (ID, version, dimensions, score, geometry)
- `lineage.txt`: S-expression tracking which source TXT files contributed to this archive
- `metadata.txt`: Count of orthos in the archive
- `text_meta.txt`: Word count and text preview

### Process Safety

The program uses an in-process directory to ensure mutual exclusion when multiple instances run concurrently. Files are moved to `fold_state/in_process/` before processing, preventing race conditions. 

**Heartbeat Mechanism**: A heartbeat file is created for each processing job and updated every 100,000 orthos. On startup, the program checks for heartbeat files that haven't been updated for more than 10 minutes (grace period) and considers them stale. Files with stale heartbeats are automatically recovered and moved back to input for reprocessing.

**Recovery**: On startup, any abandoned `.txt` files in the in-process directory are automatically recovered and moved back to input for reprocessing.

### Lineage Tracking

Each archive includes a `lineage.txt` file containing an S-expression that tracks which source TXT files contributed to the archive through any merges. This provides complete provenance information showing the merge tree structure.

**Examples:**
- Single file archive: `"file1"` 
- Simple merge: `("file1" "file2")` represents merging file1 and file2
- Nested merges: `("file3" ("file1" "file2"))` represents (file1 + file2) then merged with file3
- Deep nesting: `(("file1" "file2") ("file3" "file4"))` represents (file1 + file2) merged with (file3 + file4)

The S-expression format makes it clear to distinguish between different merge orders like:
- `(((a b) c) d)` - left-associative sequential merging
- `((a b) (c d))` - balanced binary tree merging

## Development

### Run Tests

```bash
cargo test
```

### Build

```bash
cargo build --release
```

### Code Style Guidelines

This project follows specific coding principles for performance and clarity:

1. **Functional Style Preferred**: Use non-mutating operations where performance allows
2. **Self-Documenting Code**: Avoid comments; let code express intent through clear naming
3. **Support Actual Usage**: Avoid defensive programming; implement what the call patterns require
4. **Memory Critical**: Minimize cloning and unnecessary allocations; orthos are 80-900+ bytes each
5. **Test-Driven Development**: Write failing tests first, then implement fixes
6. **Disk-Backed Operations**: Design with streaming/disk storage from the start

## Architecture

- **Interner**: Manages vocabulary and phrase completion mappings
- **Ortho**: Represents orthogonal structures with spatial dimensions
- **Spatial**: Handles spatial transformations and expansions
- **Splitter**: Tokenizes and extracts phrases from text
