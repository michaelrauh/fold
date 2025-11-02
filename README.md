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

Fold provides two commands:

### Ingest Text

Add text to the vocabulary:

```bash
fold_single ingest <path-to-text-file>
```

This command:
- Reads the text file
- Updates the interner with new vocabulary from the text
- Increments the interner version
- Saves the updated state to the resume file

### Run Worker

Execute the worker loop to generate orthos:

```bash
fold_single run
```

This command:
- Loads the frontier and interner from the resume file (or creates a blank state)
- Adds a seed ortho to explore the vocabulary
- Processes all orthos in the work queue
- Generates child orthos based on vocabulary completions
- Deduplicates the frontier using the prefix rule
- Saves the updated frontier to the resume file

## Example Workflow

```bash
# Ingest initial vocabulary
fold_single ingest corpus.txt

# Run the worker to generate orthos
fold_single run

# Add more vocabulary
fold_single ingest more_text.txt

# Run again - will resume with expanded vocabulary
fold_single run
```

## Resume File

The system maintains state in `fold_resume.bin` which contains:
- **Frontier**: A Vec of Ortho objects representing the current search frontier
- **Interner**: The vocabulary and phrase completion mappings

The resume file is automatically created on first run if it doesn't exist.

## How It Works

### Frontier Management

The frontier represents all discovered orthos in the branch-and-bound search tree, with non-lead nodes removed:
- **Lead nodes**: Orthos that are not prefixes of other orthos with the same shape
- **Non-lead nodes**: Orthos whose canonicalized payloads are prefixes of others (removed during deduplication)

### Work Queue

The system uses an in-memory VecDeque for the work queue:
1. Initialize queue with previous frontier
2. Add a seed ortho (empty ortho with current interner version)
3. Process each ortho by generating children for all valid completions
4. Deduplicate before saving

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
- **fold_single**: Main binary with ingest/run commands
