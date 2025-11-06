# Multithreading Implementation

## Overview

This document describes the parallel implementation of the worker loop using Rayon for batch parallelism.

## Implementation

A new function `process_text_parallel()` has been added alongside the original `process_text()` function in `src/lib.rs`. The parallel version uses:

- **Rayon** for data parallelism via `.par_iter()`
- **DashSet** for thread-safe concurrent seen_ids tracking
- **Arc<Mutex<_>>** for shared state (optimal ortho, frontier orthos)
- **Batch processing** approach that processes entire generations in parallel

## Key Design Decisions

### 1. Batch Parallelism vs Work-Stealing
We chose the **batch parallelism** approach recommended in SPEED_ANALYSIS.md because:
- Lower contention on shared state (synchronization happens between batches, not per-ortho)
- Simpler implementation with Rayon's `.par_iter()`
- Better cache locality within batches
- 70-80% parallel efficiency vs 60-70% for work-stealing

### 2. In-Memory Queue for Parallel Processing
The parallel version uses in-memory Vec instead of DiskBackedQueue because:
- Batch parallelism processes entire generations at once
- Eliminates disk I/O overhead during parallel processing
- Workload fits in memory for most use cases
- Allows Rayon to efficiently distribute work across threads

### 3. Thread-Safe Data Structures
- `DashSet` for seen_ids: Lock-free concurrent hash set
- `Arc<Mutex<HashMap>>` for frontier_orthos: Protected HashMap
- `Arc<Mutex<Option<Ortho>>>` for optimal tracking

## Benchmark Results

### Small Workload
Text: "the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy cat"

```
Sequential: 227.77 µs
Parallel:   362.82 µs
Speedup:    0.63x (slower due to thread overhead)
```

### Medium Workload  
Text: ~300 bytes with more vocabulary

```
Sequential: 10.59 ms
Parallel:   10.40 ms
Speedup:    1.02x (slight speedup)
```

## When to Use Parallel vs Sequential

### Use Sequential When:
- Processing single small files (<100 words)
- Workload generates <1000 orthos
- Thread overhead > parallel benefit
- Single-core environment

### Use Parallel When:
- Processing large files or many files
- Workload generates >10,000 orthos
- Multi-core CPU available (4+ cores recommended)
- Maximum throughput is important

## Performance Analysis

The parallel version shows:
- **Overhead**: ~60% for very small workloads (< 500µs sequential time)
- **Breakeven**: Around 5-10ms sequential time
- **Benefit**: Expected 2-4x speedup for large workloads on 8-core machines

The overhead comes from:
1. Thread pool initialization
2. Work distribution across threads
3. Synchronization on DashSet and Mutex
4. Final data structure consolidation

## Testing

Two comprehensive tests verify correctness:
- `test_parallel_produces_same_results_as_sequential`: Verifies identical outputs
- `test_parallel_with_multiple_texts`: Tests with text versioning and backtracking

All tests pass, confirming the parallel version produces identical results to sequential.

## Future Optimizations

To improve parallel performance:

1. **Adaptive parallelism**: Use sequential for small workloads, parallel for large
2. **Batch size tuning**: Process larger batches to amortize overhead
3. **Better work distribution**: Use work-stealing for better load balancing
4. **Lock-free data structures**: Replace Mutex with lock-free alternatives where possible

## Usage Example

```rust
use fold::{process_text_parallel};
use std::collections::{HashMap, HashSet};

let mut seen_ids = HashSet::new();
let mut optimal_ortho = None;
let mut frontier = HashSet::new();
let mut frontier_orthos_saved = HashMap::new();

let (interner, changed, frontier_size, impacted, processed) = 
    process_text_parallel(
        "your text here",
        None,
        &mut seen_ids,
        &mut optimal_ortho,
        &mut frontier,
        &mut frontier_orthos_saved,
        |_| Ok(()),
    )?;
```

## Benchmarking

Run benchmarks to compare performance:

```bash
cargo bench --bench parallel_bench
```

This will compare sequential vs parallel on different workload sizes.
