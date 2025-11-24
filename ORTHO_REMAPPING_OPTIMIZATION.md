# Ortho Remapping Optimization Decision Matrix

## Background

When merging archives in a linear merge chain, the ortho remapping stage processes ALL orthos from both archives to translate vocabulary indices from the source interner to the merged interner. This is performed in the `merge_archives()` function in `main.rs`.

The current implementation loop performs multiple operations per ortho:
1. Loads each ortho from Archive A and Archive B via `DiskBackedQueue::pop()`
2. Calls `ortho.remap(&vocab_map, new_version)` to translate payload indices
3. Computes remapped ID and checks deduplication via `SeenTracker`
4. Pushes remapped orthos to `merged_results` (rehydrating results queue)
5. Checks if ortho is impacted and conditionally adds to `work_queue`

**Important architectural note**: The loop that performs remapping also rehydrates the `SeenTracker` and `merged_results` queue. These operations are essential for the merge and cannot be eliminated—every ortho must be visited to populate the deduplication set and results. Therefore, **the loop itself cannot be removed under the current design**; optimization strategies should focus on reducing the cost of remapping within the loop or restructuring how state is persisted.

**Key bottleneck**: The loop is O(n) where n is the total number of orthos across both archives, and becomes increasingly expensive as archives grow through successive merges. While remapping adds per-ortho cost, the loop traversal is unavoidable given current state management.

---

## Decision Matrix

| Option | Description | Implementation Complexity | Performance Improvement | Memory Impact | Correctness Risk | Recommended |
|--------|-------------|--------------------------|------------------------|---------------|------------------|-------------|
| **1. Lazy Remapping** | Defer remapping until ortho is accessed for processing | Medium | Medium (still traverses all) | Low | Low | ✅ Yes |
| **2. Parallel Remapping** | Use Rayon to remap orthos in parallel batches | Low | Medium (linear speedup) | Medium | Low | ✅ Yes |
| **3. Eliminate Remapping via Stable Canonical Indices** | Use canonical vocabulary ordering so indices are stable across interners (bitset-compatible) | High | Medium (loop still required for seen/results) | Low | Medium | ⚠️ Maybe |
| **4. Incremental Merge** | Only remap orthos that are impacted by vocabulary changes | Medium | Variable (high when few changes) | Low | Medium | ⚠️ Maybe |
| **5. Streaming Archive Format** | Store orthos pre-indexed for merged vocabulary during archive save | High | Medium (loop still required) | Low | Low | ⚠️ Maybe |
| **6. Persist SeenTracker/Results in Archive** | Store seen set and results state in archive; merge via union/concat | Very High | Very High (eliminates O(n) loop) | High | Medium | ⚠️ Maybe |

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

### Option 3: Eliminate Remapping via Stable Canonical Indices

**Description**: Use a canonical vocabulary ordering so that indices are stable across all interners, eliminating the need for remapping during merges.

**Critical Constraint**: Vocabulary indices in this system are not arbitrary identifiers—they serve as **bitset positions** in `FixedBitSet` structures used by `prefix_to_completions`. The `intersect()` method performs bitset operations where indices must be dense and contiguous. Arbitrary hashes or sparse IDs would break this fundamental design.

**Implementation** (bitset-compatible approaches):

1. **Global Canonical Vocabulary**:
   - Maintain a single canonical vocabulary ordering (e.g., sorted alphabetically, or insertion-order across all files)
   - All interners share the same index assignments for common tokens
   - New tokens are always appended at the end, preserving existing indices
   - Requires coordination across archive boundaries

2. **Sorted Vocabulary Convention**:
   - Always store vocabulary in sorted order
   - When merging, the merged vocabulary is the sorted union
   - Tokens with the same string always get the same index in any interner that contains them
   - Remapping becomes a simple sorted-merge operation that can be precomputed

3. **Append-Only with Index Registry**:
   - Maintain a persistent index registry file that assigns permanent indices to tokens
   - New tokens are appended to the registry, never reassigned
   - All interners reference the same registry for index lookups
   - Bitsets grow as vocabulary grows but never need reindexing

**Pros**:
- Eliminates remapping stage entirely
- Maintains bitset compatibility for `intersect()` operations
- Ortho IDs become stable across merges

**Cons**:
- Requires coordination mechanism for canonical index assignment
- Breaking change to interner construction logic
- Sorted approach may reorder existing vocabulary (one-time migration)
- Append-only registry introduces external state dependency

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
- Note: Full string storage increases archive size (e.g., 8 bytes for index vs average ~6-12 bytes per token string plus length prefix); consider string interning with a shared dictionary to mitigate

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

## Eliminating the Loop Entirely

As noted in the Background, the current remapping loop also rehydrates the `SeenTracker` and `merged_results` queue. To truly eliminate the O(n) traversal (not just the remapping), consider these additional architectural changes:

### Option 6: Persist SeenTracker and Results in Archive

**Description**: Store the `SeenTracker` state (bloom filter + shards) and `merged_results` queue alongside the orthos in the archive format. On merge, union the seen sets and concatenate results without per-ortho iteration.

**Implementation**:
- Serialize `SeenTracker` bloom filter and shard data into archive
- Store results queue metadata (count, disk locations) in archive
- Merge becomes: union seen sets + concatenate result files + remap impacted orthos only

**Pros**:
- Eliminates O(n) loop for non-impacted orthos
- Only impacted orthos need individual processing

**Cons**:
- Larger archive files (seen set state can be significant)
- Bloom filter union may increase false positive rate
- Complex archive format with multiple data streams

**Expected Speedup**: 90%+ reduction in merge time when few orthos are impacted

---

## Recommendation

**Key insight**: Under the current design, the O(n) loop cannot be eliminated because it rehydrates the `SeenTracker` and `merged_results` queue. Options 1-5 can only reduce per-ortho cost within the loop; only Option 6 addresses the loop itself.

For **immediate improvement** with minimal risk:
1. **Option 2 (Parallel Remapping)** - Quick win with Rayon, low risk

For **medium-term optimization**:
2. **Option 1 (Lazy Remapping)** - Reduces remapping cost but loop still required
3. **Option 4 (Incremental Merge)** - Skip remapping for unchanged indices

For **long-term architectural improvement**:
4. **Option 6 (Persist SeenTracker/Results)** - Only approach that eliminates the O(n) loop entirely

---

## Metrics to Track

When implementing any option, measure:
- Total remapping wall-clock time per merge
- Orthos remapped vs orthos actually processed
- Memory high-water mark during merge
- Archive size impact (if format changes)
