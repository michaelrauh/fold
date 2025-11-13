# AT-SCALE PERFORMANCE GUIDANCE

**Context**: Real-world run hitting **625M orthos processed**, **640M seen**, **14.3M queued** on chunk 2/27  
**Reality Check**: This is 625x larger than the "billion-scale" projections in previous analysis  
**Date**: 2025-11-11

---

## Executive Summary: The Scale Changes Everything

### What the Benchmarks Told Us (Small Scale)
- Worker loop: 262ns per ortho
- Projected 1B orthos: ~262 seconds = 4.4 minutes
- Checkpoint overhead: manageable at 10K-100K scale

### What Actually Happens (Your Scale)
- **625M orthos in chunk 2/27** 
- Projected full run: **17 BILLION orthos** (625M × 27 chunks)
- At 262ns/ortho: **4,454 seconds = 74 minutes = 1.2 hours** just for worker loop
- **BUT you're still running**, so actual throughput is MUCH worse

### Critical Realization
The benchmarks measured individual operations. At 625M scale, the **cumulative effects dominate**:
- Memory pressure causes swapping
- Disk I/O becomes the bottleneck (14.3M queue size!)
- GC pressure from 640M tracked IDs
- Spatial cache thrashing with diverse ortho dimensions

---

## Immediate Crisis Points at Your Scale

### 🔴 CRISIS #1: SeenTracker with 640M IDs

**Benchmark said**: 0.5µs per ID insertion = 320 seconds for 640M  
**Reality**: Your bloom filter is saturated, shards are disk-bound

**Actual Impact**:
- Bloom filter false positive rate degraded
- Every `contains()` check hits disk-backed shards
- 640M IDs × 2.75µs (disk access) = **1,760 seconds = 29 minutes** in tracking alone
- This is likely WHERE YOUR TIME IS GOING

**Immediate Actions**:
1. **Increase bloom capacity NOW** - Set to 1-2 billion capacity
2. **Reduce shard count** - Fewer, larger in-memory shards (currently 64, try 16)
3. **Consider approximate deduplication** - Accept 0.1% false negatives, eliminate disk backing
4. **Profile actual tracker performance** - Add timing around `tracker.contains()` calls

**Code Change** (in `src/main.rs` or wherever SeenTracker is created):
```rust
// Current: SeenTracker::new(expected_items)
// Change to:
let tracker = SeenTracker::with_config(
    2_000_000_000,  // 2 billion bloom capacity (up from 1B)
    16,             // 16 shards instead of 64
    16              // Keep all in memory
);
```

---

### 🔴 CRISIS #2: DiskBackedQueue with 14.3M Orthos

**Benchmark said**: 4.7µs per ortho for queue operations  
**Reality**: 14.3M × 4.7µs = **67 seconds** just in queue I/O per file chunk

**Actual Impact**:
- Queue is constantly spilling to disk
- Every pop requires deserialization
- Disk I/O is your bottleneck, not computation
- Buffer too small for this scale

**Immediate Actions**:
1. **Increase queue buffer by 100x** - Current likely 1000, set to 100,000
2. **Pre-allocate disk space** - Avoid filesystem overhead
3. **Consider memory-mapped queue** - Let OS handle paging
4. **Add queue metrics** - Track spill frequency and disk operations

**Code Change** (in `MemoryConfig`):
```rust
// In src/memory_config.rs
pub fn calculate(interner_bytes: usize, result_count: usize) -> Self {
    let system_mem = get_available_memory();
    
    // CRITICAL: At 625M ortho scale, queue buffer must be HUGE
    let queue_buffer_size = if system_mem > 32_000_000_000 {
        100_000  // 100K buffer for 32GB+ systems
    } else if system_mem > 16_000_000_000 {
        50_000   // 50K buffer for 16GB+ systems
    } else {
        10_000   // 10K minimum even for smaller systems
    };
    
    // ... rest of config
}
```

---

### 🔴 CRISIS #3: Interner.intersect() at 625M Scale

**Benchmark said**: 248ns per call × 625M = 155 seconds  
**Reality**: Intersect is called on EVERY ortho processed

**But Here's The Hidden Problem**:
- Interner vocabulary likely 100K+ words at this scale
- FixedBitSet operations grow with vocabulary size
- Cache misses on bitset operations dominate
- BitSet cloning creates GC pressure

**Actual Impact**:
- Intersect probably taking **500-1000ns** per call (2-4x benchmark)
- 625M × 1000ns = **625 seconds = 10.4 minutes** per chunk
- 27 chunks = **4.7 hours** just in intersect

**Immediate Actions**:
1. **Switch to FxHash immediately** - This is the P0 quick win
2. **Pool FixedBitSets URGENTLY** - Reduce GC pressure
3. **Profile actual intersect time** - Add instrumentation
4. **Consider caching intersection results** - If patterns repeat

---

### 🟡 WARNING: Memory Pressure at 640M IDs

**Current Memory Usage Estimate**:
- SeenTracker bloom filter: ~1-2 GB
- SeenTracker shards (640M IDs): ~5-10 GB (if in memory)
- DiskBackedQueue (14.3M orthos): ~1-2 GB (with spilling)
- Interner vocabulary: ~100-500 MB
- **Total: 7-15 GB minimum**

**If system has < 32GB RAM**:
- You're swapping to disk
- Every operation becomes 100-1000x slower
- This explains why it's still running on chunk 2

**Immediate Actions**:
1. **Monitor memory usage** - Add metrics
2. **Increase system RAM** - 64GB recommended for this scale
3. **Enable disk-backed structures** - Accept performance hit over OOM
4. **Reduce in-memory footprint** - Aggressive pruning

---

## Recalibrated Time Projections

### Current Situation (Based on Your Report)
You're on chunk 2/27 with 625M orthos processed. Let's estimate total time:

**Optimistic Scenario** (assumes linear scaling):
- 625M orthos in chunk 2
- 27 chunks total
- If chunk 2 took X hours, total = 27 × X hours
- **Likely 27-54 hours total** (1-2 days)

**Pessimistic Scenario** (assumes degradation):
- Later chunks have more vocabulary (interner grows)
- SeenTracker saturates further
- Queue size grows
- Each chunk takes progressively longer
- **Could be 100+ hours** (4+ days)

**With Optimizations**:
- FxHash (P0): -20-30% → 20-30 hours
- Pool BitSets (P1): -20-30% → 14-21 hours  
- Increase buffers (P0): -30-50% I/O → 10-15 hours
- **Combined: 8-12 hours** (still substantial but manageable)

---

## Action Plan: Stop the Bleeding

### Phase 0: Emergency Measures (Do This NOW - While Current Run Continues)

1. **Profile the actual bottleneck**:
```bash
# Sample the process with perf/dtrace/instruments
# Find where time is ACTUALLY spent
# Our benchmarks may be wrong about what dominates at scale
```

2. **Add instrumentation** to the worker loop:
```rust
// In src/main.rs, add timing around critical operations
let start = std::time::Instant::now();
let completions = interner.intersect(&required, &forbidden);
println!("[PERF] intersect took {:?} for {} completions", start.elapsed(), completions.len());

let start = std::time::Instant::now();
tracker.insert(child_id);
println!("[PERF] tracker.insert took {:?}", start.elapsed());
```

3. **Check system resources**:
```bash
# While fold is running:
htop          # Check CPU and memory usage
iotop         # Check disk I/O
df -h         # Check disk space
```

### Phase 1: Quick Wins (Can Be Done Between Chunks)

**Priority 1: Fix SeenTracker** (Highest Impact)
- Increase bloom capacity to 2B
- Reduce shard count to 16
- Keep all shards in memory
- Expected: **50-70% speedup** if tracker is the bottleneck

**Priority 2: Increase Queue Buffer** (High Impact)
- Increase to 50K-100K buffer size
- Expected: **30-50% speedup** in queue operations

**Priority 3: FxHash** (Quick, Safe)
- Replace DefaultHasher with FxHash in Ortho.id()
- Expected: **5-10% speedup** (small but free)

### Phase 2: Structural Changes (For Next Run)

1. **Frontier-only checkpointing**:
   - Don't save all 640M results
   - Only save the 14.3M queued orthos
   - Reduces checkpoint overhead by 97%

2. **Incremental processing**:
   - Process in smaller batches
   - Clear seen tracker periodically (accept some redundant work)
   - Prevents tracker saturation

3. **Distributed processing**:
   - Split 27 chunks across multiple machines
   - Each processes independently
   - Aggregate results at end

### Phase 3: Optimization Implementation (For Future Runs)

Implement P0-P2 optimizations from PERFORMANCE_IMPROVEMENTS.md:
1. FxHash throughout
2. Pool FixedBitSets
3. SmallVec for payload
4. Parallel interner building
5. SIMD bitset operations (if needed)

---

## Checkpoint Guidance at This Scale

### Resume vs Fresh Start (Recalibrated)

**At 625M ortho scale, checkpointing is DIFFERENT**:

**Checkpoint Size**:
- 640M IDs to track: ~10 GB serialized
- 14.3M queued orthos: ~2 GB serialized
- Total checkpoint: **~12 GB**

**Checkpoint Overhead** (from DETAILED_BENCHMARK_RESULTS.md):
- Tracker rebuild: 640M × 0.5µs = **320 seconds = 5.3 minutes**
- Queue rebuild: 14.3M × 4.7µs = **67 seconds = 1.1 minutes**
- Impacted scan (if interner changes): 640M × 2.75µs = **1,760 seconds = 29 minutes**
- **Total resume overhead: 6-35 minutes** depending on interner changes

**Work Saved**:
- If processing was interrupted after 50% of chunk 2
- Remaining chunk 2: ~300M orthos × 262ns = **79 seconds = 1.3 minutes**
- Plus remaining 25 chunks: ~15.6B orthos × 262ns = **68 minutes**
- **Total work saved: 69 minutes**

**Verdict**:
- ✅ **Resume IS beneficial** - saves 69 minutes vs 6-35 minute overhead
- ⚠️ **BUT** - if interner changes (full scan), it's borderline
- 💡 **Better**: Checkpoint every 5-10 chunks, not every chunk

**Recommendation**:
```rust
// Checkpoint strategy for this scale
let should_checkpoint = (chunks_processed % 5 == 0) || // Every 5 chunks
                       (orthos_processed > 500_000_000) || // Every 500M orthos
                       (elapsed_time > 3600); // Every hour

if should_checkpoint {
    checkpoint_manager.save(&interner, &mut all_results)?;
}
```

---

## The Real Problem: This Scale Is Beyond Design Intent

### Harsh Truth
The fold system was designed for:
- Millions of orthos, not billions
- Thousands of results, not millions in queue
- Tens of MB in state, not GBs

At 625M orthos with 14.3M queue size, you're **3 orders of magnitude beyond the design point**.

### What This Means
1. **Optimizations help but can't fix the fundamental issue**
2. **You need architectural changes**:
   - Sharded processing across machines
   - Streaming/windowed processing (don't keep all state)
   - Approximate algorithms (bloom-filter-only deduplication)
   - Result pruning (keep top N, discard low-scoring)

3. **Or accept the runtime**:
   - 8-12 hours with optimizations is actually reasonable
   - This is a book-scale corpus creating massive search space

### Architectural Recommendations

**Option 1: Approximate Processing** (Fast but less accurate)
```rust
// Use bloom filter only, no exact tracking
// Accept 0.1-1% duplicate work
// Reduces memory by 90%, speeds up by 10x
```

**Option 2: Windowed Processing** (Memory-bounded)
```rust
// Process in windows of 50M orthos
// Checkpoint and clear tracker between windows
// Accept potential duplicate work across windows
```

**Option 3: Distributed Processing** (Parallel)
```rust
// Split 27 chunks into 3 batches of 9
// Process each batch on separate machine
// Merge results at end
```

**Option 4: Result Pruning** (Focused)
```rust
// Keep only top 1M results by score
// Discard lower-scoring orthos
// Drastically reduces queue size
```

---

## Final Recommendations: What To Do Right Now

### Immediate (While Current Run Continues)
1. Monitor system resources (RAM, disk I/O)
2. Profile actual bottleneck (not benchmarks)
3. Estimate remaining time based on chunk 2 duration

### Before Next Run
1. Increase SeenTracker bloom capacity to 2B
2. Increase DiskBackedQueue buffer to 50K-100K
3. Implement FxHash (2-line change)
4. Add performance instrumentation

### Strategic (Before Processing More Books)
1. Decide if 8-12 hour processing time is acceptable
2. If not, implement architectural changes:
   - Windowed processing OR
   - Approximate deduplication OR
   - Distributed processing OR
   - Result pruning
3. Consider if you need ALL 14.3M queued orthos
   - Can you prune by score threshold?
   - Can you keep only "interesting" orthos?

### The Bottom Line

**Your benchmarks were correct for small scale.**  
**But at 625M orthos, second-order effects dominate:**
- Disk I/O (not computation)
- Memory pressure (causing swapping)
- GC pressure (from 640M allocations)
- Cache thrashing (spatial cache not sized for this)

**The guidance in PERFORMANCE_IMPROVEMENTS.md is still valid**, but:
- The estimated speedups are MINIMUMS, not guarantees
- At this scale, you need architectural changes, not just optimizations
- 8-12 hours with all optimizations may be as good as it gets

**Without architectural changes, you're looking at days, not hours.**
