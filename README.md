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
- Process each file in `fold_state/input/`
- Print optimal ortho after each file
- Print final optimal ortho at the end

## Development

### Run Tests

```bash
cargo test
```

### Build

```bash
cargo build --release
```

## Architecture

- **Interner**: Manages vocabulary and phrase completion mappings
- **Ortho**: Represents orthogonal structures with spatial dimensions
- **Spatial**: Handles spatial transformations and expansions
- **Splitter**: Tokenizes and extracts phrases from text
