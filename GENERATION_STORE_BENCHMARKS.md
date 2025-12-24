# Generation Store Benchmarks

This document describes the performance benchmarks for the generation store system and what they measure.

## Overview

The generation store is a disk-backed, memory-bounded system for processing orthos through multiple generations. The benchmarks cover the core operations: sorting (compact_landing), deduplication (merge_unique), novelty detection (anti_join), and full generation cycles.

## Benchmark Suite

### 1. Sort Throughput vs RAM (compact_landing)

**Purpose**: Measure external sort performance with varying memory budgets.

**Benchmarks**:
- `compact_landing_128mb_6.25m_ints`: Sort 6.25M integers (~50MB) with 128MB RAM budget
- `compact_landing_512mb_6.25m_ints`: Sort 6.25M integers (~50MB) with 512MB RAM budget
- `compact_landing_2gb_6.25m_ints`: Sort 6.25M integers (~50MB) with 2GB RAM budget

**What's measured**: Time to read unsorted data from disk, perform arena-based external sort, and write sorted runs back to disk.

**Expected behavior**:
- With sufficient RAM (2GB >> 50MB data), should complete in a single in-memory sort
- With limited RAM (128MB), should create multiple runs and perform external merge
- Throughput should increase with RAM budget but plateau once data fits in memory

**Performance characteristics**:
- Arena reuse eliminates allocation overhead across flushes
- Bounded by disk I/O when data exceeds RAM
- Sort is stable and uses Rust's unstable_sort for speed

### 2. Anti-Join vs History Size

**Purpose**: Measure novelty detection performance as history grows.

**Benchmarks**:
- `anti_join_history_1k`: 10K new orthos vs 1K history orthos
- `anti_join_history_10k`: 100K new orthos vs 10K history orthos  
- `anti_join_history_100k`: 1M new orthos vs 100K history orthos

**What's measured**: Time to perform streaming merge-based anti-join, filtering out previously-seen orthos and returning only novel ones.

**Expected behavior**:
- Linear time complexity: O(n + h) where n = new orthos, h = history size
- Streaming algorithm with minimal memory overhead
- Performance should scale linearly with combined size of inputs

**Performance characteristics**:
- Streaming merge requires both inputs sorted by ortho ID
- Single pass through both datasets
- Writes accepted orthos to new seen run
- Memory usage bounded to iterator buffers regardless of data size

### 3. Full Generation with Duplicates

**Purpose**: Measure complete generation cycle end-to-end.

**Benchmarks**:
- `full_generation_1m_ints_50pct_dupes`: 1M integers with 50% duplicates through full cycle
  1. Compact landing (external sort)
  2. Merge unique (k-way merge with deduplication)
  3. Anti-join (novelty detection vs empty history)

**What's measured**: Total time for a complete generation processing cycle including all intermediate disk I/O.

**Expected behavior**:
- Demonstrates realistic workload with duplicates
- Tests integration of all core components
- Validates memory stays bounded throughout full cycle

**Performance characteristics**:
- Duplicate elimination happens in merge_unique phase
- Anti-join with empty history is fastest path (all orthos novel)
- Total time dominated by sort phase for cold data
- Efficient when history is small (early generations)

### 4. Merge Unique Varying Fan-In

**Purpose**: Measure k-way merge performance with different numbers of input runs.

**Benchmarks**:
- `merge_unique_fan_in/4`: Merge 4 runs (40K items total)
- `merge_unique_fan_in/8`: Merge 8 runs (80K items total)
- `merge_unique_fan_in/16`: Merge 16 runs (160K items total)
- `merge_unique_fan_in/32`: Merge 32 runs (320K items total)
- `merge_unique_fan_in/64`: Merge 64 runs (640K items total)

**What's measured**: Time to perform k-way merge of sorted runs into a single deduplicated unique run.

**Expected behavior**:
- Multi-pass merge when k exceeds fan_in limit (32 in these benchmarks)
- Throughput measured in elements/second
- Performance should scale reasonably with total data size
- Fan-in limit prevents excessive open file handles

**Performance characteristics**:
- Uses binary heap for efficient k-way merge
- Duplicate detection via adjacent value comparison
- Multi-pass strategy when k > fan_in
- Read buffer size (64KB) affects I/O efficiency

## Running Benchmarks

```bash
# Run all generation_store benchmarks
cargo bench --bench generation_store_bench

# Run specific benchmark group
cargo bench --bench generation_store_bench sort_throughput
cargo bench --bench generation_store_bench anti_join_benches
cargo bench --bench generation_store_bench full_generation

# Quick smoke test (no statistical analysis)
cargo bench --bench generation_store_bench -- --test
```

## Performance Notes

### Memory Management
- All operations maintain strict memory bounds via `run_budget_bytes`
- Arena allocation pattern reuses capacity across multiple flushes
- Disk is used to bound RAM, not for durability

### I/O Patterns
- Sequential reads via 64KB buffers for optimal disk throughput
- Writes use BufWriter to amortize syscall overhead
- Temporary files in `base_path/runs/` cleaned up by test framework

### Scalability
- External sort handles data >> RAM via multi-pass approach
- Streaming anti-join processes arbitrarily large history
- K-way merge adapts fan-in based on memory budget

### Configuration Impact
The RAM policy (Task 10) dynamically adjusts:
- `run_budget_bytes`: 0.7 * available memory budget
- `fan_in`: clamp(budget / read_buf, 8, 128)
- Leader: 2-6GB budget (aggressive < 65%, conservative > 85%)
- Follower: 256MB-1GB budget (aggressive < 50%, conservative > 70%)

These benchmarks use fixed configurations for repeatability, but production uses adaptive policies.

## Comparison with Old Architecture

The old architecture used:
- Tiered hashset/bloom filter seen tracker
- Spill-half disk queue
- Complex memory tuning with multiple parameters

The new generational store:
- Eliminates bloom filters (external sort provides deduplication)
- Replaces disk queue with simple work segments
- Simplifies memory policy to single RAM budget
- Provides predictable O(n) space complexity

Benchmark results should demonstrate comparable or better throughput with reduced complexity.
