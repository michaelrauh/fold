# Seen Tracker Design

The seen tracker prevents duplicate ortho processing by combining a fast bloom filter with sharded hash maps that spill to disk.

## Structure
- **Bloom filter**: 1% false-positive rate sized by `bloom_capacity`. Used for fast negative checks.
- **Shards**: HashMap-backed shards keyed by `hash(id) % num_shards`. Each shard tracks `usize` ids and a `dirty` flag.
- **LRU cache**: Up to `max_shards_in_memory` shards are kept hot; evicting a shard writes it to `shard_{:08}.bin` via bincode.
- **Layout**: Shards live under `<work>/seen_shards/` (per job). Initializing a tracker deletes any existing shard directory to start clean.

## Operations
- `contains(id)`: Bloom check first; if positive, load or create the shard and check membership.
- `insert(id)`: Set bloom, load/create shard, add id if new, mark shard dirty, increment total count.
- `flush()`: Persist all dirty loaded shards. Eviction during normal operation also persists shards.
- `len()/is_empty()`: Derived from total inserted ids (not bloom bits).

## Persistence and recovery
- Trackers are intentionally rebuilt from result queues rather than reused directly:
  - `with_path` clears existing shard files before use.
  - Checkpoint load (`checkpoint_manager.rs`) replays all saved results into a fresh tracker and queue.
  - Merge/ingest work folders are deleted after completion, removing shards with them.
- To restore deduplication after a crash, rerun fold; stale work is recovered and results are replayed into a new tracker.

## Configuration knobs
- `bloom_capacity`: Target item count (minimum 1,000,000 in production). Set via `MemoryConfig`.
- `num_shards`: Hash prefix partitioning (default 64).
- `max_shards_in_memory`: Hot shard limit for the LRU (default = `num_shards`, but can be lower to save RAM).

## Usage notes
- False positives can occur (by bloom design), but the shard check eliminates false negatives.
- Because initialization wipes existing shard files, persisting deduplication requires replaying saved results rather than reusing prior shards.
