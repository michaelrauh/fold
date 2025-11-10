# Copilot Instructions for Fold

## Repository Overview

Fold is a text processing system that generates and optimizes ortho structures from text input. It builds interner vocabularies and generates ortho structures through iterative expansion, tracking optimal configurations based on volume calculation.

## Core Directives

### Style Principles

1. **High reading level functional style is preferred**
   - Prefer non-mutation unless there is a large performance hit
   - Use functional patterns and immutable data structures where practical

2. **Avoid comments - code should self-document**
   - Write clear, expressive code that explains itself
   - Use descriptive names for functions, variables, and types
   - Only add comments when absolutely necessary to explain complex algorithms

3. **Do not use "defensive programming" or "harden" methods**
   - Look at the call pattern and support the actual calls and behavior in the current program
   - This limits complexity and avoids unnecessary validation code
   - Focus on what the code actually does, not what it might do

4. **Practice a TDD style**
   - If there is a bug, prove it with a failing test first
   - If there is new behavior needed, add a test for it first
   - Let tests drive the design and implementation

### Project Principles

1. **Performance critical application**
   - Prefer small simple structures
   - Avoid lots of clones and unnecessary data shuffles
   - Be mindful of allocation overhead

2. **Disk usage is mandatory**
   - Keeping everything in memory is impossible
   - Design with disk-backed storage from the start
   - Use streaming patterns over loading entire datasets

3. **Correct by construction**
   - Results and intermediates should be correct by construction
   - Do not make junk and then check for it or delete it
   - Just create it in the correct configuration from the beginning
   - Avoid generate-and-filter patterns

## Architecture

### Core Components

1. **Interner** (`src/interner.rs`)
   - Builds and maintains vocabulary and phrase prefix mappings across versions
   - Tracks version increments for change detection
   - Key method: `intersect()` - finds completions for given prefixes and forbidden tokens

2. **Ortho** (`src/ortho.rs`)
   - Multi-dimensional structures that track token combinations
   - Has dimensions, payload, and version
   - Key methods: `add()`, `get_requirements()`

3. **Worker Loop** (`src/main.rs`)
   - Processes orthos by intersecting requirements with interner completions
   - Uses a disk-backed queue to handle large workloads without OOM
   - Generates children until the queue is empty

4. **Optimal Tracking**
   - Identifies ortho with maximum volume (product of dimension sizes minus 1)
   - Maintained throughout processing

### Key Data Structures

- `seen_ids: HashSet<usize>` - deduplication across all generated orthos
- `frontier: HashSet<usize>` - tracks leaf ortho IDs
- `frontier_orthos_saved: HashMap<usize, Ortho>` - stores frontier between file iterations
- `work_queue: DiskBackedQueue<Ortho>` - BFS queue with disk overflow

## Coding Guidelines

### Memory Management

**CRITICAL**: This project is highly memory-sensitive. Follow these rules:

1. **Avoid unnecessary cloning**
   - Ortho structures are 80-900+ bytes each
   - Clone only when absolutely necessary
   - Consider using references where possible

### Code Style

1. **Use existing patterns**
   - Follow the worker loop pattern for ortho generation
   - Use `bincode` for serialization/deserialization

2. **Error Handling**
   - Use the `FoldError` type from `src/error.rs`
   - Propagate errors with `?` operator
   - Provide meaningful error messages

3. **Testing**
   - Add unit tests in the same file as the code (following Rust convention)
   - Add integration tests in `tests/` directory
   - Test both happy path and edge cases
   - Include memory/performance considerations in tests

### Performance Considerations

1. **Version tracking**
   - Versions increment with each new interner
   - Changed keys trigger rewinding and backtracking
   - Impacted frontier orthos are rewound and re-explored

2. **Deduplication**
   - All orthos are deduplicated by ID via `seen_ids`
   - Never process the same ortho twice

## Building and Testing

```bash
# Build the project
cargo build --release

# Run tests
cargo test

# Run specific test suite
cargo test --lib              # Library tests only
cargo test --test integration_test  # Integration tests

# Run benchmarks
cargo bench
```

## Common Workflows

### Processing Files

1. Use `stage.sh` to prepare input files
2. Run `cargo run --release` to process staged files
3. Files are processed in alphabetical order from `fold_state/input/`

### Adding New Features

1. Maintain streaming/iterative processing patterns
2. Update tests to cover new functionality

### Modifying the Worker Loop

- Located in `src/main.rs`
- Key sections: interner update, work queue processing
- Always maintain: deduplication, optimal tracking

## Special Considerations

### Ortho IDs

- Based on payload content hash
- Used for deduplication
- Stored in `seen_ids` to prevent reprocessing

## When Making Changes

2. **Run all tests** before submitting

## Dependencies

- `bincode` - serialization for orthos
- `fixedbitset` - efficient bit operations
- `itertools` - iterator utilities
- `serde` - serialization framework
- `rustc-hash` - fast hash functions

## Project State

- Edition: 2024 (Rust 2024 edition)
- Current focus: Memory optimization for large-scale processing
- Recent work: Disk-backed queue implementation to prevent OOM