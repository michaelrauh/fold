# Ortho Remapping Optimization Decision Matrix

## Background

When merging archives in a linear merge chain, the ortho remapping stage processes ALL orthos from both archives to translate vocabulary indices from the source interner to the merged interner. This is performed in the `merge_archives()` function in `main.rs`.

The current implementation:
1. Loads each ortho from Archive A and Archive B via `DiskBackedQueue::pop()`
2. Calls `ortho.remap(&vocab_map, new_version)` to translate payload indices
3. Checks deduplication via `SeenTracker`
4. Pushes remapped orthos to `merged_results` and conditionally to `work_queue`

**Key bottleneck**: This is O(n) where n is the total number of orthos across both archives, and becomes increasingly expensive as archives grow through successive merges.

---

## Decision Matrix

| Option | Description | Implementation Complexity | Performance Improvement | Memory Impact | Correctness Risk | Recommended |
|--------|-------------|--------------------------|------------------------|---------------|------------------|-------------|
| **1. Lazy Remapping** | Defer remapping until ortho is accessed for processing | Medium | High (O(1) per unused ortho) | Low | Low | ✅ Yes |
| **2. Parallel Remapping** | Use Rayon to remap orthos in parallel batches | Low | Medium (linear speedup) | Medium | Low | ✅ Yes |
| **3. Eliminate Remapping via Stable IDs** | Use string-based or hash-based stable identifiers instead of vocabulary indices | High | Very High (eliminate stage) | High | Medium | ⚠️ Maybe |
| **4. Incremental Merge** | Only remap orthos that are impacted by vocabulary changes | Medium | Variable (high when few changes) | Low | Medium | ⚠️ Maybe |
| **5. Streaming Archive Format** | Store orthos pre-indexed for merged vocabulary during archive save | High | Very High (shift cost to save) | Low | Low | ✅ Yes |

---

## Detailed Analysis

### Option 1: Lazy Remapping

**Description**: Instead of eagerly remapping all orthos upfront, store the vocabulary mapping alongside the ortho reference and perform remapping only when the ortho is actually accessed for expansion/processing.

**Implementation**:
- Create a `LazyOrtho` wrapper that holds the original ortho and vocab_map reference
- Defer `remap()` call to first access of payload
- Orthos that are never expanded (duplicates, non-impacted) skip remapping entirely

**Pros**:
- Eliminates work for orthos that are never processed
- Minimal changes to existing architecture
- Memory efficient

**Cons**:
- Adds indirection layer
- Deduplication check still requires computing remapped ID

**Expected Speedup**: 30-70% reduction in remapping time (depending on duplicate/non-impacted ratio)

---

### Option 2: Parallel Remapping

**Description**: Use Rayon's parallel iterators to remap orthos in batches across multiple CPU cores.

**Implementation** (pseudocode):
```rust
// Instead of sequential pop/remap/push loop:
// Note: drain_batch() would need to be added to DiskBackedQueue
let batch: Vec<Ortho> = results_a.drain_batch(1000)?;
let remapped: Vec<Ortho> = batch.par_iter()
    .filter_map(|o| o.remap(&vocab_map_a, new_version))
    .collect();
```

**Pros**:
- Simple implementation using existing Rayon dependency
- Linear speedup with core count
- No correctness concerns

**Cons**:
- Requires batch API on `DiskBackedQueue`
- Deduplication still sequential (requires synchronization)
- Limited by I/O bandwidth on disk-backed queues

**Expected Speedup**: 2-8x depending on core count and I/O characteristics

---

### Option 3: Eliminate Remapping via Stable IDs

**Description**: Store token identifiers in ortho payloads as stable hashes or interned strings instead of vocabulary indices that change between interners.

**Implementation**:
- Change ortho payload from vocabulary index references to stable token identifiers (e.g., `u64` hashes of token strings using a deterministic hash like SipHash or xxHash)
- Handle hash collisions via secondary lookup table, or use 128-bit hashes where collision is negligible for practical vocabulary sizes (< billions of tokens)
- Or use a global string interner that assigns permanent IDs
- Merging interners just unions the vocabulary without reindexing

**Pros**:
- Completely eliminates remapping stage
- Simplifies merge logic significantly
- Ortho IDs become truly stable across merges

**Cons**:
- Breaking change to serialization format (migration required)
- Higher memory per ortho (`u64` hash = 8 bytes vs `usize` index = 4-8 bytes depending on platform)
- Hash collisions require handling (though extremely rare with good hash functions)

**Expected Speedup**: 100% elimination of remapping overhead

---

### Option 4: Incremental Merge (Impacted-Only Remapping)

**Description**: Leverage the existing `impacted_keys` computation to skip remapping orthos whose vocabulary indices don't change.

**Implementation**:
- If vocabulary grows at the end only (common case), old indices stay valid
- Track which index ranges are "stable" vs "shifted"
- Only remap orthos containing tokens in shifted ranges

**Pros**:
- Exploits common case of append-only vocabulary growth
- Compatible with current architecture
- Low implementation risk

**Cons**:
- Benefit depends on vocabulary overlap patterns
- Still requires scanning all orthos to check if impacted
- Complex edge cases when vocabulary has insertions

**Expected Speedup**: 0-90% depending on vocabulary change pattern

---

### Option 5: Streaming Archive Format

**Description**: When saving an archive, pre-compute and store the remapped orthos for all possible merge targets (or use a canonical vocabulary ordering).

**Implementation**:
- Define a canonical vocabulary ordering (e.g., sorted alphabetically)
- Store orthos using canonical indices
- Merging just concatenates archives without remapping

**Alternative approach**:
- Store orthos as `(payload_tokens: Vec<String>, dims)` in archives
- Remap to indices on load (shifts cost from merge to load)
- Note: Full string storage increases archive size significantly (~10-50x per token); consider string interning with a shared dictionary to mitigate

**Pros**:
- Eliminates merge-time remapping entirely
- Cleaner archive format
- Idempotent merge operations

**Cons**:
- Increases archive save time
- Larger archive files if storing strings
- Requires archive format migration

**Expected Speedup**: 100% elimination at merge time (cost shifted to save/load)

---

## Recommendation

For **immediate improvement** with minimal risk:
1. **Option 2 (Parallel Remapping)** - Quick win with Rayon, low risk

For **medium-term optimization**:
2. **Option 1 (Lazy Remapping)** - Good ROI for typical workloads

For **long-term architectural improvement**:
3. **Option 5 (Streaming Archive Format)** - Best for eliminating the bottleneck entirely

---

## Metrics to Track

When implementing any option, measure:
- Total remapping wall-clock time per merge
- Orthos remapped vs orthos actually processed
- Memory high-water mark during merge
- Archive size impact (if format changes)
