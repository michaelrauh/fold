# Dynamic Memory Configuration

## Overview

The fold application now dynamically calculates optimal cache sizes to utilize up to 75% of available system RAM. This ensures efficient memory usage across different hardware configurations.

## Implementation

### New Module: `memory_config.rs`

The `MemoryConfig` struct calculates optimal values for:

1. **Queue Buffer Sizes** - In-memory buffers for work and results queues
2. **Bloom Filter Capacity** - Fast negative lookup for seen orthos
3. **Shard Count** - Number of hash shards for seen tracking
4. **Shards in Memory** - How many shards to keep hot in RAM

### Memory Budget Breakdown

The 75% target RAM allocation is divided as follows:

1. **Interner** - Vocabulary and prefix-to-completions mappings (measured from serialized size)
2. **Runtime Reserve** - 20% of target for working memory (ortho processing, vectors, etc.)
3. **Bloom Filter** - ~2 bytes per item (1% false positive rate)
4. **Queue Buffers** - 30% of remaining memory (2 queues × buffer_size × ~200 bytes per ortho)
5. **Shards in Memory** - 70% of remaining memory (~12 bytes per hash entry)

### Calculation Strategy

```
Total System RAM = 100%
Target Usage = 75%
Runtime Reserve = 20% of target
Available for Caches = Target - Interner - Runtime Reserve
Queue Memory = 30% of Available
Shard Memory = 70% of Available
```

### Conservative Estimates

- **Ortho size**: 200 bytes (accounts for small ~80 byte and large ~900 byte orthos)
- **Bloom filter**: 2 bytes per item (optimal 1.44 bytes, rounded up for safety)
- **Hash entry**: 12 bytes (usize key + () value + HashMap overhead)
- **Shard size**: ~10,000 items (good disk I/O granularity)

## Usage

### Fresh Start

When starting fresh (no checkpoint):
```rust
let config = MemoryConfig::calculate(0, 0);
let tracker = SeenTracker::with_config(
    config.bloom_capacity,
    config.num_shards,
    config.max_shards_in_memory
);
let queue = DiskBackedQueue::new(config.queue_buffer_size)?;
```

### Resume from Checkpoint

When loading from checkpoint, the system automatically measures the interner size and result count, then recalculates optimal memory configuration:

```rust
// Measure interner size and result count from checkpoint
let interner_bytes = /* size of serialized interner */;
let result_count = /* number of orthos in results queue */;

let config = MemoryConfig::calculate(interner_bytes, result_count);
// Config is then used when loading checkpoint
```

**Key behavior**: The bloom filter and shard configuration automatically scale based on the actual result count, ensuring efficient memory usage as the dataset grows.

## Dynamic Rebalancing

The system rebalances bloom filter capacity and shard count based on the number of results:

- **Bloom capacity**: `max(expected_results * 3, 1_000_000)`
  - Maintains 3× headroom for growth
  - Minimum 1M items to avoid degraded false positive rates
  
- **Shard count**: `clamp(expected_results / 10_000, 64, 1_024)`
  - Target ~10K items per shard for optimal disk I/O
  - Minimum 64 shards, maximum 1024 shards

**Examples**:
- 10K results → 1M bloom (minimum), 64 shards (minimum)
- 100K results → 1M bloom (minimum), 64 shards (minimum)
- 1M results → 3M bloom (3×), 100 shards (1M/10K)
- 10M results → 30M bloom (3×), 1000 shards (10M/10K, at max)

## Examples

### Small Machine (8 GB RAM)

```
Total System RAM: 8192 MB
Target memory usage: 6144 MB (75%)
Interner size: 50 MB
Runtime reserve: 1229 MB
Available for caches: 4865 MB

Queue buffer size: 7,297 orthos (~1,459 MB per queue)
Bloom capacity: 3,000,000 items (~6 MB)
Shards: 300 total, 272 in memory (~3,398 MB)
Estimated total: 6144 MB
```

### Large Machine (64 GB RAM)

```
Total System RAM: 65536 MB
Target memory usage: 49152 MB (75%)
Interner size: 500 MB
Runtime reserve: 9830 MB
Available for caches: 38822 MB

Queue buffer size: 58,233 orthos (~11,646 MB per queue)
Bloom capacity: 30,000,000 items (~60 MB)
Shards: 1000 total, 1000 in memory (~27,115 MB)
Estimated total: 49152 MB
```

## Benefits

1. **Automatic Scaling** - Works efficiently on any machine from laptops to servers
2. **Maximized Performance** - Uses available RAM rather than leaving it unused
3. **Conservative Overhead** - 20% runtime reserve ensures stability during peak processing
4. **Balanced Allocation** - Prioritizes shards (70%) over queues (30%) based on typical access patterns
5. **Graceful Degradation** - Shards spill to disk when needed, maintaining correctness

## Configuration Bounds

The implementation includes sensible bounds:

- **Queue buffer**: 1,000 - 100,000 orthos
- **Bloom capacity**: minimum 1,000,000 items (3x expected for growth)
- **Shard count**: 64 - 1,024 shards
- **Shards in memory**: 16 - total shard count

## Testing

All existing tests pass with the new memory configuration system. The `MemoryConfig::default_config()` provides test-friendly defaults (10K queue buffers, 10M bloom capacity, 64 shards).

## Dependencies

Added `sysinfo = "0.32"` to query system memory information.
