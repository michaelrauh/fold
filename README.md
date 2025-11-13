# fold

A text processing system that finds optimal orthogonal structures in text using spatial algorithms.

## Overview

Fold processes text files by:
1. Building an interner (vocabulary and phrase completion mappings) from input text
2. Generating orthogonal structures (orthos) through a work queue
3. Finding the optimal ortho based on dimensional scoring

## Usage

### Prepare Input Files

Use `stage.sh` to split a large text file into chunks:

```bash
./stage.sh <input_file> <delimiter> [min_length]
```

Example:
```bash
./stage.sh book.txt "CHAPTER" 50000
```

This splits `book.txt` by "CHAPTER" delimiter, filtering out chunks smaller than 50000 characters, and places the results in `./fold_state/input/`.

### Run Fold

Process all files in the input directory:

```bash
cargo run --release
```

The program will:
- Loop through `fold_state/input/` to find `.txt` files
- Process each `.txt` file independently (one at a time)
- Build a separate interner for each file
- Generate and track orthos for each file
- Print optimal ortho after each file
- Save an archive directory (`.bin`) in the same location as the input file
- Delete the input `.txt` file after successful archiving
- Continue looping until no `.txt` files remain

Each archive is a directory containing:
- `interner.bin`: The interner built from that specific file
- `results/`: DiskBackedQueue directory with all ortho results

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
