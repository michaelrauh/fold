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
| **4. Per-Ortho Remap Check** | Check if remap is identity for each ortho; skip if indices unchanged | Very Low | Variable (high when vocabs align) | None | Low | ✅ Yes |
| **5. Incremental Merge** | Only remap orthos that are impacted by vocabulary changes | Medium | Variable (high when few changes) | Low | Medium | ⚠️ Maybe |
| **6. Streaming Archive Format** | Store orthos pre-indexed for merged vocabulary during archive save | High | Medium (loop still required) | Low | Low | ⚠️ Maybe |
| **7. Persist SeenTracker/Results in Archive** | Store seen set and results state in archive; merge via union/concat | Very High | Very High (eliminates O(n) loop) | High | Medium | ⚠️ Maybe |
| **8. File Concatenation with Deferred Dedup** | Concatenate result files; rebuild seen set lazily or at query time | High | Very High (eliminates loop) | Low | Medium | ⚠️ Maybe |
| **9. Structural Sharing / Copy-on-Write Archives** | Use immutable data structures; merge by referencing parent archives | Very High | Very High (O(1) merge) | Medium | High | ⚠️ Maybe |

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

### Option 4: Per-Ortho Remap Check (Skip Identity Mappings)

**Description**: Before calling `ortho.remap()`, check if the vocabulary mapping is an identity for that ortho's tokens. If so, skip the remap call entirely.

**Implementation**:
```rust
// Check if vocab_map is identity for this ortho's payload
fn needs_remap(ortho: &Ortho, vocab_map: &[usize]) -> bool {
    ortho.payload().iter()
        .filter_map(|opt| *opt)
        .any(|idx| vocab_map[idx] != idx)
}

// In the loop:
let remapped = if needs_remap(&ortho, &vocab_map_a) {
    ortho.remap(&vocab_map_a, new_version)
} else {
    Some(ortho.with_version(new_version)) // Just update version
};
```

**When this helps**:
- When archive A's vocabulary is a prefix of the merged vocabulary (append-only growth)
- When merging archives with identical or overlapping vocabularies
- Common in linear merge chains where each new file adds few new tokens

**Pros**:
- Very low implementation complexity (simple check before remap)
- Zero overhead when remap is needed
- Significant savings when vocabularies align

**Cons**:
- Still requires the O(n) loop for seen/results rehydration
- Negligible benefit when vocabularies are disjoint or reordered
- Adds a check cost (but much cheaper than full remap)

**Expected Speedup**: 0-90% of remapping time (depends on vocabulary alignment pattern)

---

### Option 5: Incremental Merge (Impacted-Only Processing)

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

### Option 6: Streaming Archive Format

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

## Eliminating the Rehydrate Loop Entirely

As noted in the Background, the current remapping loop also rehydrates the `SeenTracker` and `merged_results` queue. To truly eliminate the O(n) traversal (not just the remapping), consider these architectural changes:

### Option 7: Persist SeenTracker and Results in Archive

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

### Option 8: File Concatenation with Deferred Deduplication

**Description**: Instead of eagerly deduplicating during merge, concatenate result files and defer deduplication to query time or a background compaction process.

**Implementation**:
- Merge simply concatenates archive result files (O(1) file operation)
- Maintain a "layers" structure: each merge adds a new layer
- Deduplication happens lazily when orthos are accessed, or during background compaction
- Query: iterate through layers, skip already-seen IDs

**Pros**:
- O(1) merge time (just file concat + metadata update)
- Deduplication cost amortized over queries
- Simple implementation for merge operation

**Cons**:
- Query time increases with number of uncompacted layers
- Requires background compaction strategy to bound layer count
- More complex read path with layer iteration
- Duplicates temporarily consume disk space

**Expected Speedup**: 99%+ at merge time (cost shifted to query/compaction)

---

### Option 9: Structural Sharing / Copy-on-Write Archives

**Description**: Use immutable, content-addressed data structures where merge creates a new archive that references parent archives rather than copying data.

**Implementation**:
- Archives are immutable; merge creates a "merge node" referencing A and B
- Ortho lookup traverses the merge DAG
- Vocabulary indices are resolved through the inheritance chain
- Compaction flattens the DAG when too deep

**Analogy**: Similar to Git's commit graph or persistent data structures.

**Pros**:
- O(1) merge time (just create reference node)
- No data copying during merge
- Full history preserved
- Natural support for branching/parallel exploration

**Cons**:
- Complex query resolution through DAG
- Need garbage collection for unreachable archives
- Vocabulary mapping must be resolved per-query
- Deep DAG = slow queries; requires periodic compaction

**Expected Speedup**: 99%+ at merge time (cost shifted to query time)

---

## Recommendation

**Key insight**: Under the current design, the O(n) loop cannot be eliminated because it rehydrates the `SeenTracker` and `merged_results` queue. Options 1-6 can only reduce per-ortho cost within the loop; Options 7-9 address eliminating the loop itself through architectural changes.

### Within-Loop Optimizations (Quick Wins)

For **immediate improvement** with minimal risk:
1. **Option 4 (Per-Ortho Remap Check)** - Very low implementation cost; skip remap when indices unchanged
2. **Option 2 (Parallel Remapping)** - Quick win with Rayon, low risk

For **medium-term optimization**:
3. **Option 1 (Lazy Remapping)** - Reduces remapping cost but loop still required
4. **Option 5 (Incremental Merge)** - Skip remapping for unchanged indices

### Loop Elimination (Architectural Changes)

For **long-term architectural improvement** to eliminate the O(n) loop:
5. **Option 8 (File Concatenation with Deferred Dedup)** - Simplest path to O(1) merge; shifts dedup to query/compaction
6. **Option 7 (Persist SeenTracker/Results)** - Eliminates loop for non-impacted orthos; moderate complexity
7. **Option 9 (Structural Sharing)** - Most flexible but highest complexity; enables new use cases like branching

---

## Metrics to Track

When implementing any option, measure:
- Total remapping wall-clock time per merge
- Orthos remapped vs orthos actually processed
- Memory high-water mark during merge
- Archive size impact (if format changes)
