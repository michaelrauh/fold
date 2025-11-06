# Copilot Instructions for Fold

## Repository Overview

Fold is a text processing system that generates and optimizes ortho structures from text input. It builds interner vocabularies and generates ortho structures through iterative expansion, tracking optimal configurations based on volume calculation.

## Architecture

### Core Components

1. **Interner** (`src/interner.rs`)
   - Builds and maintains vocabulary and phrase prefix mappings across versions
   - Tracks version increments for change detection
   - Key method: `intersect()` - finds completions for given prefixes and forbidden tokens

2. **Ortho** (`src/ortho.rs`)
   - Multi-dimensional structures that track token combinations
   - Has dimensions, payload, and version
   - Key methods: `add()`, `get_requirements()`, `rebuild_to_position()`

3. **Worker Loop** (`src/lib.rs::process_text()`)
   - Processes orthos by intersecting requirements with interner completions
   - Uses a disk-backed queue to handle large workloads without OOM
   - Generates children until the queue is empty
   - Tracks frontier orthos for incremental updates

4. **Optimal Tracking**
   - Identifies ortho with maximum volume (product of dimension sizes minus 1)
   - Maintained throughout processing

5. **Disk-Backed Queue** (`src/disk_backed_queue.rs`)
   - FIFO queue with memory buffer and disk overflow
   - Prevents OOM when processing millions of orthos
   - Uses bincode for serialization

### Key Data Structures

- `seen_ids: HashSet<usize>` - deduplication across all generated orthos
- `frontier: HashSet<usize>` - tracks leaf ortho IDs
- `frontier_orthos_saved: HashMap<usize, Ortho>` - stores frontier between file iterations
- `work_queue: DiskBackedQueue<Ortho>` - BFS queue with disk overflow

## Coding Guidelines

### Memory Management

**CRITICAL**: This project is highly memory-sensitive. Follow these rules:

1. **Never load all orthos into memory simultaneously**
   - Use streaming/iterative processing
   - Leverage the `DiskBackedQueue` for work queues
   - Keep memory buffers limited (default: 10K orthos)

2. **Avoid unnecessary cloning**
   - Ortho structures are 80-900+ bytes each
   - Clone only when absolutely necessary
   - Consider using references where possible

3. **Be mindful of frontier storage**
   - Frontier orthos are stored between file iterations
   - Keep frontier_orthos_saved size reasonable

### Code Style

1. **Use existing patterns**
   - Follow the worker loop pattern for ortho generation
   - Use `bincode` for serialization/deserialization
   - Leverage `rustc-hash` for faster HashMaps when appropriate

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

1. **Disk-backed operations**
   - Understand when data spills to disk
   - Balance buffer size vs. memory usage
   - Default buffer: 10K orthos (~2-9 MB)

2. **Version tracking**
   - Versions increment with each new interner
   - Changed keys trigger rewinding and backtracking
   - Impacted frontier orthos are rewound and re-explored

3. **Deduplication**
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

1. Consider memory impact (see MEMORY_ANALYSIS.md)
2. Maintain streaming/iterative processing patterns
3. Test with large inputs (millions of orthos)
4. Update tests to cover new functionality

### Modifying the Worker Loop

- Located in `src/lib.rs::process_text()`
- Key sections: interner update, frontier tracking, work queue processing
- Always maintain: deduplication, frontier updates, optimal tracking
- Test with multiple files to ensure frontier persistence works

## Key Constraints

1. **Memory budget**: Aim for <100 MB peak usage
2. **Disk-backed queue**: Default 10K ortho buffer
3. **Frontier size**: Typically 1K-10K orthos
4. **Deduplication**: All orthos tracked in `seen_ids` (IDs only)

## Special Considerations

### Impacted Backtracking

When the interner updates (new completions for existing prefixes):
1. Detect changed keys via `find_changed_keys()`
2. Find frontier orthos containing those keys
3. Rewind them to position where changed key is "most advanced"
4. Re-explore with updated interner completions

This ensures we explore all possible combinations with new vocabulary.

### Ortho IDs

- Based on payload content hash
- Used for deduplication
- Stored in `seen_ids` to prevent reprocessing

## When Making Changes

1. **Read MEMORY_ANALYSIS.md** if touching memory-related code
2. **Run all tests** before submitting
3. **Check for warnings**: The build has some unused import warnings to clean up
4. **Profile memory usage** for changes affecting ortho storage or queues
5. **Consider disk I/O impact** when modifying DiskBackedQueue

## Dependencies

- `bincode` - serialization for orthos
- `fixedbitset` - efficient bit operations
- `itertools` - iterator utilities
- `serde` - serialization framework
- `rustc-hash` - fast hash functions
- `smallvec` - stack-allocated vectors

## Project State

- Edition: 2024 (Rust 2024 edition)
- Current focus: Memory optimization for large-scale processing
- Recent work: Disk-backed queue implementation to prevent OOM
