# DiskBackedQueue Design

## Purpose

`DiskBackedQueue` provides a hybrid memory/disk queue for orthos that prevents out-of-memory errors during large-scale fold processing. It maintains working set in memory while spilling overflow to disk.

## Problem Statement

### The Memory Challenge

- Orthos: 80-900+ bytes each
- BFS frontier can grow to millions of items
- Peak memory usage unpredictable (depends on text structure)
- Cannot hold entire work queue in memory

### The Solution

Hybrid queue with automatic memory management:
- Keep hot working set in RAM (default: 10,000 items)
- Spill overflow to disk automatically
- Load from disk as memory becomes available
- Persistent across process restarts

## Architecture

### Three-Component Design

```
┌─────────────────────────────────────────┐
│  In-Memory VecDeque (hot working set)   │
│  Capacity: 10,000 items (configurable)  │
└─────────────┬───────────────────────────┘
              │
              ↓ spill when full
┌─────────────────────────────────────────┐
│  Disk Files (cold storage)              │
│  Format: queue_NNNNNNNN.bin (bincode)   │
│  Location: configurable base path       │
└─────────────────────────────────────────┘
              │
              ↓ load on demand
┌─────────────────────────────────────────┐
│  File Index Tracker                     │
│  next_write_file: which file to write   │
│  next_read_file: which file to read     │
└─────────────────────────────────────────┘
```

### Memory Threshold Strategy

- **Push behavior**: Add to in-memory queue
  - If queue reaches capacity → flush to disk
  - Memory queue cleared after flush
  
- **Pop behavior**: Remove from in-memory queue
  - If queue empty → load next disk file
  - Entire file loaded at once into memory

## File Format

### Naming Convention

```
queue_00000001.bin  ← first spill
queue_00000002.bin  ← second spill
queue_00000003.bin  ← third spill
...
```

### Serialization

- Format: bincode (binary, compact)
- Contents: `Vec<Ortho>` serialized as single blob
- Each file contains one "chunk" of items (up to capacity)

## Operations

### Push

```rust
queue.push(ortho)?;
```

1. Add ortho to in-memory `VecDeque`
2. If `VecDeque.len() >= memory_capacity`:
   - Serialize all items to `queue_NNNNNNNN.bin`
   - Increment `next_write_file`
   - Clear in-memory queue
   - Increment disk item counter

### Pop

```rust
if let Some(ortho) = queue.pop()? {
    // process ortho
}
```

1. If in-memory queue not empty:
   - Return front item from `VecDeque`
2. If in-memory queue empty AND disk files exist:
   - Load `queue_NNNNNNNN.bin` into memory
   - Delete loaded file
   - Increment `next_read_file`
   - Return front item from newly loaded queue

### Len

```rust
let total = queue.len();  // in-memory + on-disk
```

Returns: `in_memory_queue.len() + items_on_disk_count`

### Flush

```rust
queue.flush()?;
```

Forces immediate spill of in-memory items to disk. Used before:
- Checkpointing
- Process shutdown
- Manual persistence points

## Persistence Strategy

### State Preservation

The queue can be reconstructed across process restarts:

```rust
// Initial creation
let queue = DiskBackedQueue::new_from_path("./work_queue", 10000)?;

// ... process items, some spill to disk ...

// Process restarts
let queue = DiskBackedQueue::new_from_path("./work_queue", 10000)?;
// Automatically discovers existing files and continues
```

### File Discovery

On `new_from_path()`:
1. List all `queue_*.bin` files in directory
2. Parse file numbers to find gaps
3. Set `next_write_file` to first gap or max+1
4. Set `next_read_file` to lowest numbered file
5. Count items across all files for initial `len()`

## Memory Characteristics

### Bounded Memory Usage

- **Maximum in-memory**: `memory_capacity × sizeof(Ortho)`
- **Typical**: 10,000 items × ~400 bytes = ~4MB
- **Configurable**: Adjust capacity based on available RAM

### Disk Usage

- **Unbounded**: Limited only by disk space
- **Typical file size**: ~4MB per file (10,000 × 400 bytes)
- **Cleanup**: Files deleted as they're consumed

## Checkpoint Integration

### Three-Queue Strategy

During checkpoint save/load, the results queue uses a three-instance pattern:

1. **Checkpoint backup** (preserved, read-only)
   - Located at: `checkpoint/results_backup/`
   - Never modified after checkpoint creation
   - Source of truth for recovery

2. **Temporary copy** (consumed during load)
   - Located at: `results_temp/`
   - Copy of checkpoint backup
   - Consumed to rebuild bloom filter and seen set
   - Deleted after rehydration complete

3. **Active queue** (being built)
   - Located at: `results/`
   - Receives all items from temporary queue
   - Becomes the new active results

This ensures checkpoint data is never corrupted during load operations.

## Performance Characteristics

### Push Performance

- **Memory-only**: O(1) amortized (VecDeque push_back)
- **With spill**: O(n) every n items (where n = memory_capacity)
  - Serialize n items: O(n)
  - Write to disk: O(n)
  - Clear memory: O(1)

### Pop Performance

- **Memory-only**: O(1) (VecDeque pop_front)
- **With load**: O(n) every n items
  - Read from disk: O(n)
  - Deserialize: O(n)
  - Delete file: O(1)

### Space Complexity

- **Memory**: O(memory_capacity)
- **Disk**: O(total_items)

## Error Handling

### Transient Failures

- IO errors during spill → propagated as `FoldError::Io`
- Disk full → propagated as `FoldError::Io`
- Serialization errors → propagated as `FoldError::Bincode`

### Recovery

- Partially written files: overwritten on next spill (atomic file creation)
- Missing sequence numbers: automatically detected and filled
- Corrupt files: fail fast with descriptive error

## Design Rationale

### Why VecDeque for Memory?

- Efficient FIFO operations (O(1) push_back, pop_front)
- Contiguous memory for cache locality
- No reallocation in steady state (pre-sized to capacity)

### Why File-Per-Chunk?

- Atomic writes (single file write operation)
- Simple cleanup (delete consumed files)
- No file growth/truncation complexity
- Easy to reason about and debug

### Why Bincode?

- Fast serialization/deserialization
- Compact binary format
- Native Rust type support
- Zero-copy where possible

### Why Not Database?

- Adds complexity (SQL/NoSQL engine)
- Overkill for simple FIFO queue
- No query requirements
- File-based is simpler and faster

### Why Not Memory-Mapped Files?

- Complex lifetime management
- OS-dependent behavior
- Overkill for append-only queue
- Explicit I/O is more portable
