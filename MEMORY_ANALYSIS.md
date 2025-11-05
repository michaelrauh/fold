# Ortho Storage OOM Analysis

## Executive Summary

The current generation loop suffers from unbounded memory growth due to storing large collections of Ortho structures in memory simultaneously. This document analyzes the problem, evaluates storage alternatives, and identifies optimization opportunities.

## 1. Current Memory Usage Analysis

### 1.1 Data Structures Holding Orthos

The system maintains several collections that grow during processing:

1. **`seen_ids: HashSet<usize>`** (in `main.rs`)
   - Purpose: Deduplication - tracks IDs of ALL generated orthos
   - Size: O(total orthos generated across all files)
   - Content: Just IDs (8 bytes each)
   - **Impact: LOW** - Only stores IDs, not full orthos

2. **`frontier: HashSet<usize>`** (in `main.rs`)
   - Purpose: Tracks which orthos are on the frontier (leaf nodes)
   - Size: O(frontier size)
   - Content: Just IDs (8 bytes each)
   - **Impact: LOW** - Only stores IDs, not full orthos

3. **`frontier_orthos_saved: HashMap<usize, Ortho>`** (in `main.rs`)
   - Purpose: Stores full Ortho objects for the frontier between file processing iterations
   - Size: O(frontier size) × sizeof(Ortho)
   - Content: Full Ortho structures
   - **Impact: HIGH** - Stores complete Ortho objects
   - Location: `src/lib.rs:116` - `frontier_orthos_saved.extend(frontier_orthos)`

4. **`frontier_orthos: HashMap<usize, Ortho>`** (in `lib.rs::process_text()`)
   - Purpose: Temporary storage during single text processing
   - Size: O(frontier size) × sizeof(Ortho)
   - Content: Full Ortho structures
   - **Impact: MEDIUM** - Temporary, cleared after each file
   - Location: `src/lib.rs:41-116`

5. **`work_queue: VecDeque<Ortho>`** (in `lib.rs::process_text()`)
   - Purpose: BFS work queue for generation loop
   - Size: Can grow to O(total children generated in a single iteration)
   - Content: Full Ortho structures
   - **Impact: HIGH** - Can contain millions of orthos during processing
   - Location: `src/lib.rs:55-99`

### 1.2 Ortho Structure Size

```rust
pub struct Ortho {
    version: usize,           // 8 bytes
    dims: Vec<usize>,        // 24 bytes + heap (typically 2-4 usizes = 16-32 bytes)
    payload: Vec<Option<usize>>, // 24 bytes + heap (capacity 4-100+ = 32-800+ bytes)
}
```

**Estimated size per ortho:** 80-900+ bytes depending on dimensions and payload size

For a run generating 10M orthos with avg 200 bytes each:
- `work_queue` peak: potentially GBs if all queued at once
- `frontier_orthos_saved`: 10K-100K orthos = 2-90 MB (moderate)
- Total peak memory: **Could exceed 20+ GB**

### 1.3 Memory Growth Pattern

```
Initial State:
  work_queue: [seed_ortho]
  frontier: {seed_id}
  frontier_orthos: {seed_id: seed_ortho}

After N iterations:
  work_queue: [child1, child2, ..., childM] where M can be 100K-1M+
  frontier: {all_leaf_ids}
  frontier_orthos: {all_leaf_orthos}

Problem: work_queue grows exponentially before shrinking
```

## 2. Stream vs. Full Collection Analysis

### 2.1 Where All Orthos Are Needed

**NONE** - No operation requires all orthos to be in memory simultaneously.

### 2.2 Where Streaming Is Sufficient

**ALL OPERATIONS** can work with streaming:

1. **Deduplication** (seen_ids)
   - Only needs IDs, not full orthos ✓
   - Can use disk-backed ID set if needed

2. **Work Queue Processing** (work_queue)
   - BFS/DFS only needs current ortho + ability to enqueue children ✓
   - Children can be written to disk immediately
   - Can process in batches

3. **Frontier Tracking** (frontier, frontier_orthos_saved)
   - Needs frontier orthos for next iteration ✓
   - Can serialize to disk between files
   - Can process frontier in batches

4. **Optimal Ortho Tracking** (optimal_ortho)
   - Only needs current best ✓
   - Single ortho in memory

### 2.3 Key Insight

**The work_queue is the primary memory bottleneck.** It can grow to contain millions of orthos before being processed. All other structures are manageable.

## 3. Data Storage Waste Analysis

### 3.1 Identified Waste

1. **Multiple Clones of Same Ortho**
   - Location: `src/lib.rs:99` - `frontier_orthos.insert(child_id, child.clone())`
   - Ortho cloned into both work_queue AND frontier_orthos
   - **Waste: 2x memory for frontier orthos**

2. **Redundant Version Storage**
   - Every ortho stores `version` field (8 bytes)
   - All orthos in a single processing batch have same version
   - **Waste: 8 bytes × millions = tens of MB**

3. **Hash Keys Duplicated**
   - `frontier` stores IDs, `frontier_orthos_saved` also has IDs as keys
   - **Waste: Moderate - hash overhead**

4. **Empty Frontier Orthos Between Files**
   - When no orthos remain on frontier, still maintains HashMap
   - **Waste: Negligible**

### 3.2 Optimization Opportunities

1. **Eliminate Double Storage**
   - Don't clone into both work_queue and frontier_orthos
   - Use ID-based references where possible

2. **Version Deduplication**
   - Store version once per batch
   - Reconstruct when needed

3. **Batch Processing**
   - Process work_queue in chunks
   - Write overflow to disk

## 4. Storage Alternatives Decision Matrix

| Criterion | Plain Bin Files | Disk-Backed Vector Libs | SQLite | RedB |
|-----------|----------------|------------------------|--------|------|
| **Implementation Complexity** | ⭐⭐⭐⭐ Simple | ⭐⭐⭐ Moderate | ⭐⭐ Complex | ⭐⭐ Complex |
| **Performance (Read)** | ⭐⭐⭐⭐ Fast sequential | ⭐⭐⭐⭐ Fast | ⭐⭐⭐ Good | ⭐⭐⭐⭐ Fast |
| **Performance (Write)** | ⭐⭐⭐⭐⭐ Fastest | ⭐⭐⭐⭐ Fast | ⭐⭐⭐ Good | ⭐⭐⭐ Good |
| **Random Access** | ⭐ Poor | ⭐⭐⭐ Good | ⭐⭐⭐⭐⭐ Excellent | ⭐⭐⭐⭐⭐ Excellent |
| **Memory Control** | ⭐⭐⭐⭐⭐ Perfect | ⭐⭐⭐⭐ Good | ⭐⭐⭐ Auto-managed | ⭐⭐⭐⭐ Good |
| **Dependencies** | ⭐⭐⭐⭐⭐ None (just std) | ⭐⭐⭐ memmap2 | ⭐⭐ rusqlite | ⭐⭐ redb crate |
| **ACID Properties** | ⭐ None | ⭐ None | ⭐⭐⭐⭐⭐ Full | ⭐⭐⭐⭐ Good |
| **Crash Recovery** | ⭐ Manual | ⭐ Manual | ⭐⭐⭐⭐⭐ Excellent | ⭐⭐⭐⭐ Good |
| **Size Overhead** | ⭐⭐⭐⭐⭐ Minimal | ⭐⭐⭐⭐ Low | ⭐⭐⭐ Moderate | ⭐⭐⭐ Moderate |
| **Query Capability** | ⭐ None | ⭐ None | ⭐⭐⭐⭐⭐ SQL | ⭐⭐ Key-value |
| **Fit for Use Case** | ⭐⭐⭐⭐⭐ Perfect | ⭐⭐⭐⭐ Good | ⭐⭐ Over-engineered | ⭐⭐ Over-engineered |

### 4.1 Detailed Analysis

#### Option 1: Plain Bin Files (RECOMMENDED)
**Pros:**
- Simplest implementation - just serialize/deserialize
- Already using bincode for Ortho serialization
- Maximum control over memory usage
- No external dependencies
- Perfect for sequential FIFO queue processing
- Lowest overhead

**Cons:**
- No random access
- No automatic crash recovery
- Manual buffer management needed

**Use Case Fit:** ⭐⭐⭐⭐⭐
- Work queue is naturally FIFO
- No need for random access
- Simple append/read pattern

#### Option 2: Disk-Backed Vector Libraries (memmap2)
**Pros:**
- Memory-mapped files provide virtual memory semantics
- OS handles paging automatically
- Can treat disk as extended RAM

**Cons:**
- Still limited by virtual address space
- Less control over what's in memory
- Requires understanding of mmap semantics
- Can still OOM if too many pages are resident

**Use Case Fit:** ⭐⭐⭐⭐
- Good for large arrays with locality
- Our access pattern is sequential, so less benefit from mmap

#### Option 3: SQLite
**Pros:**
- Robust, battle-tested
- ACID properties
- Can query orthos by various criteria
- Excellent crash recovery

**Cons:**
- Significant overhead for simple queue operations
- Transaction overhead
- More complex API
- Over-engineered for our needs (we just need a queue)

**Use Case Fit:** ⭐⭐
- Too heavyweight for simple queue semantics
- Don't need SQL queries or transactions
- Added complexity without clear benefit

#### Option 4: RedB (Rust Embedded Database)
**Pros:**
- Pure Rust
- ACID properties
- Key-value store semantics
- Better performance than SQLite for simple ops

**Cons:**
- Still more complex than needed
- Transaction overhead
- Don't need durability guarantees for work queue

**Use Case Fit:** ⭐⭐
- Over-engineered for temporary work queue
- Could be useful for frontier storage between runs
- Added complexity for uncertain benefit

### 4.2 Recommendation

**Primary: Plain Bin Files**
- Implement simple disk-backed FIFO queue using bincode
- Separate file for each batch chunk
- In-memory buffer with configurable size
- Flush to disk when buffer exceeds threshold

**Rationale:**
1. Simplest solution that directly addresses the problem
2. No new dependencies
3. Maximum control over memory usage
4. Matches our access pattern (FIFO queue)
5. Can easily tune buffer sizes

## 5. Proposed Solution Architecture

### 5.1 Disk-Backed Work Queue

```rust
pub struct DiskBackedQueue {
    memory_buffer: VecDeque<Ortho>,
    buffer_size_limit: usize,
    disk_files: VecDeque<PathBuf>,
    temp_dir: PathBuf,
    file_counter: usize,
}
```

**Operations:**
- `push()` - add to memory buffer, flush to disk if needed
- `pop()` - take from memory buffer, reload from disk if needed
- `len()` - track total items (memory + disk)

**Memory Control:**
- Keep only N orthos in memory (configurable, e.g., 10,000)
- Write overflow to disk files
- Read disk files back when memory buffer depletes

### 5.2 Optimized Frontier Storage

Current: `HashMap<usize, Ortho>` - stores full orthos

Options:
1. **Keep in memory** - frontier is typically small (1K-10K orthos)
2. **Serialize between files** - write to disk, reload next iteration
3. **Hybrid** - memory if small, disk if large

Recommendation: Keep in memory with disk fallback if > threshold

### 5.3 Memory Budget

Proposed limits:
- Work queue memory buffer: 10,000 orthos (2-9 MB)
- Frontier storage: 10,000 orthos (2-9 MB)  
- seen_ids: Unlimited (IDs only, ~8MB per 1M orthos)
- optimal_ortho: 1 ortho (~200 bytes)

**Total memory budget: ~20 MB** (vs. current potential 20+ GB)

## 6. Implementation Plan

### Phase 1: Basic Disk-Backed Queue
1. Create `DiskBackedQueue` struct
2. Implement push/pop with memory buffer
3. Add disk overflow logic
4. Add tests

### Phase 2: Integration
1. Replace `VecDeque<Ortho>` with `DiskBackedQueue` in `lib.rs`
2. Add configuration for buffer size
3. Test with large workloads

### Phase 3: Optimization
1. Eliminate double cloning (work_queue + frontier_orthos)
2. Consider batch serialization optimizations
3. Add metrics/logging for disk usage

### Phase 4: Frontier Optimization (if needed)
1. Add disk fallback for large frontiers
2. Serialize frontier between file processing

## 7. Testing Strategy

### 7.1 Unit Tests
- DiskBackedQueue operations (push, pop, len)
- Buffer overflow handling
- Disk file cleanup

### 7.2 Integration Tests
- Process text with disk queue
- Verify same results as in-memory
- Test with various buffer sizes

### 7.3 Performance Tests
- Benchmark disk-backed vs in-memory
- Memory usage profiling
- Large workload testing (millions of orthos)

## 8. Risks and Mitigations

### Risk 1: Performance Degradation
- **Risk:** Disk I/O slower than memory
- **Mitigation:** Large memory buffer (10K orthos), batch writes
- **Fallback:** Configurable buffer size, can increase if needed

### Risk 2: Disk Space Exhaustion
- **Risk:** Generating too many disk files
- **Mitigation:** Clean up files as processed, monitor disk usage
- **Fallback:** Fail gracefully with error message

### Risk 3: Serialization Overhead
- **Risk:** bincode serialization adds CPU overhead
- **Mitigation:** Already using bincode, overhead is acceptable
- **Fallback:** Optimize serialization if needed

## 9. Success Metrics

1. **Memory Usage**: Peak memory < 100 MB (vs. current 20+ GB)
2. **Performance**: < 20% slower than current in-memory (acceptable tradeoff)
3. **Correctness**: Identical results to current implementation
4. **Scalability**: Can process 10M+ orthos without OOM

## 10. Conclusion

The OOM issue is primarily caused by the unbounded `work_queue` holding millions of Ortho structures. The solution is to implement a simple disk-backed FIFO queue using plain bin files and bincode serialization.

**Key Benefits:**
- Predictable memory usage (~20 MB vs. 20+ GB)
- Simple implementation (no new dependencies)
- Matches our access pattern (FIFO)
- Easy to tune via buffer size configuration

**Next Steps:**
1. Get approval on this analysis
2. Implement DiskBackedQueue
3. Integrate and test
4. Deploy and monitor
