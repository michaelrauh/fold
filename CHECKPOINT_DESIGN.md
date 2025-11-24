# Checkpoint System Design

## Overview

The fold application implements a fully atomic checkpoint system that allows resumption after crashes or interruptions. The checkpoint preserves both the interner state and all generated results, with proper deduplication via a seen tracker that is reconstructed on resume.

## Three-Queue Strategy

At certain points in time, there are **three result queues** in different states:

1. **Checkpoint Backup** (`./fold_state/checkpoint/results_backup`)
   - Read-only preserved copy
   - Only updated when a new checkpoint succeeds
   - Immutable during processing

2. **Temporary Copy** (`./fold_state/results_temp`)
   - Created only during checkpoint load
   - Consumed to rebuild the seen set and new active queue
   - Deleted after consumption completes

3. **Active Queue** (`./fold_state/results`)
   - Current working queue receiving new orthos
   - Flushed to disk when saving checkpoint
   - Copied to become next checkpoint backup

## Save Checkpoint (Atomic)

```
save_checkpoint(interner, results_queue):
  1. Create temp directory: ./fold_state/checkpoint_temp
  2. Flush results_queue (write all buffer to disk)
  3. Serialize interner to temp/interner.bin
  4. Copy results queue directory to temp/results_backup
  5. ATOMIC: Remove old checkpoint, rename temp → checkpoint
```

**Atomicity**: If crash occurs during save, either old checkpoint survives intact or new checkpoint is complete. No partial state.

## Load Checkpoint (Three-Queue Consumption)

```
load_checkpoint() -> (Interner, DiskBackedQueue, SeenTracker):
  1. Load interner from ./fold_state/checkpoint/interner.bin
  2. Copy checkpoint/results_backup → results_temp (consumable copy)
  3. Create temporary queue from results_temp
  4. Count total items to calculate optimal configuration:
     - bloom_capacity = result_count * 3 (min 1,000,000)
     - num_shards = (result_count / 10,000).max(64).min(1024)
     - max_shards_in_memory = 32
  5. Clear old shard directory (./fold_state/seen_shards/)
  6. Create new SeenTracker with calculated configuration
  7. Create new active queue at results/
  8. Pop ALL items from temp queue:
     - Insert ortho.id() into seen tracker
     - Push ortho to new active queue
  9. Delete results_temp directory
  10. Return (interner, new_active_queue, seen_tracker)
```

**Key Behavior**: The checkpoint backup is never modified. A copy is consumed to rebuild state, preserving the backup for potential re-use. The seen tracker is fully reconstructed with optimal sizing based on the actual result count.

## Global Seen Tracker

The `SeenTracker` is **global across all files**:

- Initialized empty on fresh start (default config: 1M bloom, 64 shards, 32 in-memory)
- Reconstructed from checkpoint results on resume (with optimal config based on result count)
- Persists across file processing iterations
- Prevents duplicate orthos even after resume
- Uses disk-backed sharding for memory efficiency (see SEEN_TRACKER_DESIGN.md)

## File Processing Flow

```
main():
  1. Load checkpoint → (interner?, results_queue, global_seen_tracker)
  2. For each input file:
     - Extend interner with new text
     - Process work queue (use global_seen_tracker for deduplication)
     - Push new orthos to results_queue
     - Save checkpoint (atomic)
     - Delete processed file
```

## Directory Structure

```
fold_state/
├── checkpoint/              # Atomic checkpoint (updated atomically)
│   ├── interner.bin
│   └── results_backup/      # Preserved copy of results queue
│       ├── queue_00000001.bin
│       ├── queue_00000002.bin
│       └── ...
├── checkpoint_temp/         # Staging area during save (deleted after)
├── results_temp/            # Consumable copy during load (deleted after)
├── results/                 # Active working queue
│   ├── queue_00000001.bin
│   └── ...
├── seen_shards/             # Disk-backed tracker shards (LRU cache)
│   ├── shard_00000000.bin
│   ├── shard_00000001.bin
│   └── ...
└── input/                   # Files to process (deleted after)
```

## Recovery Guarantees

1. **Crash during save**: Old checkpoint remains valid, temp directory is cleaned up on next save
2. **Crash during load**: Checkpoint backup preserved, temp directory cleaned up on next load
3. **Crash during processing**: Checkpoint remains at last successful file boundary
4. **File deletion marker**: File is only deleted after successful checkpoint save

## Memory Considerations

- Seen tracker uses disk-backed sharding for memory efficiency
  - Bloom filter: ~12MB for 10M items (always in memory)
  - Hot shards: 32 shards × ~10K items × 24 bytes ≈ 7.7MB
  - Cold shards: automatically evicted to disk
  - Total hot memory: ~20MB (vs 252MB for all-in-memory approach)
- Results queue uses disk backing with 10,000 item buffer
- Checkpoint consumption reads from disk incrementally (doesn't load all results into memory)
- See SEEN_TRACKER_DESIGN.md for detailed memory analysis

## Test Coverage

- `test_save_and_load_checkpoint`: Basic checkpoint round-trip with SeenTracker
- `test_checkpoint_rehydrates_bloom_from_all_results`: Verifies three-queue strategy and tracker reconstruction
- `test_load_nonexistent_checkpoint`: Handles missing checkpoint gracefully
- `test_results_queue_persistence`: Queue survives process restarts
- `test_checkpoint_manager_integration`: Full integration test with tracker

All tests use `tempfile` for isolation and automatic cleanup.
