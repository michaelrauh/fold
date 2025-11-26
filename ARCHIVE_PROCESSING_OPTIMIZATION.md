# Archive Processing Speed Optimization Analysis

## Executive Summary

The "Processing Larger Archive" stage is the longest-running stage during merge operations. This document analyzes the bottlenecks and provides optimization options in a decision matrix format.

## Current Implementation Analysis

### What Happens During Archive Processing

During merge operations (`merge_archives` in `src/main.rs`), the system processes two archives:

1. **Processing Larger Archive** (lines 395-427)
   - Creates a `DiskBackedQueue` from the archive's results directory
   - Iterates through ALL orthos in the archive
   - For each ortho:
     - Checks if it's already seen via `tracker.contains()`
     - If new: inserts into tracker, pushes to merged_results queue and work_queue if impacted
   - No remapping needed (vocabulary is already correct)

2. **Remapping Smaller Archive** (lines 439-473)
   - Similar process but with vocabulary remapping via `ortho.remap()`

### Primary Bottleneck: Initial Queue Loading

**Critical Issue in `DiskBackedQueue::new_from_path()` (lines 35-94 in disk_backed_queue.rs):**

```rust
// For EACH disk file:
for entry in fs::read_dir(&disk_path) {
    let file = File::open(&path)?;
    let mut reader = BufReader::new(file);
    
    // Count items by DESERIALIZING EVERY ORTHO
    let mut count = 0;
    loop {
        match bincode::decode_from_std_read::<Ortho, _, _>(&mut reader, config) {
            Ok(_) => count += 1,  // Deserialize but discard!
            Err(_) => break,
        }
    }
    disk_count += count;
}
```

**Why This is Expensive:**

1. Opens and reads EVERY disk file
2. Deserializes EVERY ortho (80-900+ bytes each) just to count
3. With archives containing millions of orthos across hundreds of files, this can take minutes
4. The deserialized orthos are immediately discarded - only the count is kept
5. This happens TWICE per merge (once for larger archive, once for smaller)

### Secondary Bottlenecks

1. **Tracker Operations:**
   - `tracker.contains()` - bloom filter check + potential shard load from disk
   - `tracker.insert()` - marks shard as dirty, eventual disk write
   - With millions of orthos, these add up even though individually fast

2. **Queue Operations:**
   - Each `pop()` may trigger disk file load (deserialize entire file)
   - Each `push()` may trigger disk spill (serialize buffer to disk)
   - Buffer size (typically 10,000) determines frequency

3. **Impact Checking:**
   - `is_ortho_impacted_fast()` called for every ortho
   - O(1) lookup but still function call overhead

4. **Ortho Cloning:**
   - `merged_results.push(ortho.clone())` - clones dims and payload vectors
   - Each ortho is 80-900+ bytes depending on payload size
   - Happens for every non-duplicate ortho

## Optimization Options

### Option 1: Metadata File for Queue Length

**Description:** Store ortho count in a separate metadata file instead of counting during initialization.

**Implementation:**
- Add `metadata.txt` to each queue directory containing the total count
- Update on each `push()` and `pop()` operation (in-memory, flush at end)
- Read single small file during `new_from_path()` instead of scanning all files

**Pros:**
- ✅ Eliminates deserialization during initialization (biggest bottleneck)
- ✅ O(1) initialization time instead of O(n) where n = total orthos
- ✅ Simple to implement and maintain
- ✅ Backward compatible (can fallback to counting if metadata missing)
- ✅ No changes to ortho structure or serialization format

**Cons:**
- ❌ Slight overhead to maintain metadata (writes on push/pop)
- ❌ Potential for metadata drift if process crashes (can be fixed with validation)
- ❌ Requires careful synchronization with disk operations

**Impact:**
- **Initialization speedup:** From O(n) deserialization to O(1) file read
- **For 1M orthos:** Could reduce from ~30 seconds to <1 second
- **Runtime overhead:** Negligible (single integer update)

**Risk:** Low
**Effort:** Low (1-2 hours implementation)
**Compatibility:** High (backward compatible)

---

### Option 2: File-Level Metadata Headers

**Description:** Store count at the beginning of each queue file as a header.

**Implementation:**
- Prepend count to each `queue_*.bin` file: `[count: u64][ortho1][ortho2]...`
- Read header only during initialization
- Sum headers across all files for total count

**Pros:**
- ✅ Eliminates most deserialization (only reads headers)
- ✅ More robust than separate metadata file (count is with data)
- ✅ No separate metadata file to keep in sync
- ✅ Reduced risk of metadata drift

**Cons:**
- ❌ Changes file format (breaks backward compatibility)
- ❌ More complex implementation (read/write logic changes)
- ❌ Need migration path for existing queue files
- ❌ Still requires reading all files (but only headers)

**Impact:**
- **Initialization speedup:** From O(n) deserialization to O(f) header reads where f = file count
- **For 1M orthos in 100 files:** Reduces from ~30 seconds to ~1 second
- **Runtime overhead:** Minimal (one u64 write per spill)

**Risk:** Medium (file format change)
**Effort:** Medium (3-5 hours including migration)
**Compatibility:** Low (requires migration)

---

### Option 3: Lazy Length Calculation

**Description:** Don't calculate total length upfront; compute it lazily as files are processed.

**Implementation:**
- Remove count calculation from `new_from_path()`
- Track `processed_count` and estimate remaining as `num_files_remaining * avg_file_size`
- Update progress bars with estimates instead of exact counts

**Pros:**
- ✅ Zero initialization time
- ✅ No file format changes
- ✅ Minimal code changes
- ✅ Works with existing queue files

**Cons:**
- ❌ Progress bars become estimates instead of exact
- ❌ UX degradation (users prefer exact progress)
- ❌ Total count not available until end
- ❌ May complicate metrics tracking

**Impact:**
- **Initialization speedup:** 100% (no initialization work)
- **UX impact:** Progress becomes estimated
- **Runtime overhead:** None

**Risk:** Low (no file format changes)
**Effort:** Low (2-3 hours)
**Compatibility:** High (fully backward compatible)

---

### Option 4: Parallel Queue Loading

**Description:** Load and count multiple queue files in parallel using rayon.

**Implementation:**
- Use `rayon::par_iter()` to process directory entries in parallel
- Count orthos in each file concurrently
- Aggregate counts at the end

**Pros:**
- ✅ Leverages multiple CPU cores
- ✅ No file format changes
- ✅ Fully backward compatible
- ✅ Can combine with other optimizations

**Cons:**
- ❌ Still deserializes all orthos (just faster)
- ❌ Adds dependency on rayon (already in dependencies)
- ❌ Limited by disk I/O bandwidth on spinning disks
- ❌ Memory pressure from multiple readers

**Impact:**
- **Initialization speedup:** 2-4x on multi-core systems (CPU bound)
- **For 1M orthos on 4 cores:** ~30 seconds → ~8-15 seconds
- **Disk-bound systems:** Limited improvement

**Risk:** Low
**Effort:** Low (2-3 hours)
**Compatibility:** High (fully backward compatible)

---

### Option 5: Streaming Counter During Save

**Description:** Count orthos during the original save operation and store in archive metadata.

**Implementation:**
- When creating an archive, track count during `merged_results.push()`
- Save count to `archive_metadata.txt` at finalization
- Read count from metadata during merge initialization
- Eliminates need to count queue files

**Pros:**
- ✅ No counting needed during merge at all
- ✅ Count is exact and always available
- ✅ Natural place to store this information
- ✅ Archive metadata already exists (`metadata.txt`)

**Cons:**
- ❌ Requires changes to archive creation logic
- ❌ Need to ensure metadata is always written
- ❌ Doesn't help with work queues (only result archives)
- ❌ Queue rehydration still needs counting unless combined with Option 1

**Impact:**
- **Archive loading:** Instant (metadata read)
- **Queue loading:** No improvement unless combined with Option 1
- **Overall merge speedup:** Significant for archive processing

**Risk:** Low
**Effort:** Low (1-2 hours)
**Compatibility:** High (existing archives can use fallback counting)

---

### Option 6: Binary Search File Format

**Description:** Use a structured file format that supports seeking to count efficiently.

**Implementation:**
- Store orthos with size prefix: `[size: u32][ortho_data]...`
- Can skip over orthos without full deserialization
- Seek through file counting size prefixes

**Pros:**
- ✅ Fast counting without full deserialization
- ✅ Enables other optimizations (random access, checksums)
- ✅ Professional file format design

**Cons:**
- ❌ Major file format change
- ❌ Significant implementation effort
- ❌ Breaking change requiring migration
- ❌ Adds complexity to all serialization code

**Impact:**
- **Initialization speedup:** O(n) seeks instead of O(n) deserializations
- **Improvement:** ~5-10x faster counting
- **Complexity:** High

**Risk:** High (major refactor)
**Effort:** High (2-3 days)
**Compatibility:** Low (requires migration strategy)

---

### Option 7: Incremental Queue Rehydration

**Description:** Don't load tracker from previous run; rebuild incrementally as orthos are processed.

**Implementation:**
- Start with empty tracker
- Process queue normally, building up seen set organically
- First-time orthos will be reprocessed, but duplicates are caught

**Pros:**
- ✅ Zero initialization time for tracker
- ✅ Simpler recovery logic
- ✅ No need to persist/restore bloom filter

**Cons:**
- ❌ Reprocesses some work (inefficient)
- ❌ Increases total ortho processing time
- ❌ Defeats purpose of checkpointing
- ❌ May actually be slower overall

**Impact:**
- **Initialization speedup:** 100% for tracker
- **Overall runtime:** Likely SLOWER due to duplicate work
- **Not recommended**

**Risk:** Medium (correctness concerns)
**Effort:** Low
**Compatibility:** High

---

## Decision Matrix Summary

| Option | Init Speedup | Runtime Cost | Effort | Risk | Compat | Recommended |
|--------|--------------|--------------|--------|------|--------|-------------|
| 1. Metadata File | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ **YES** |
| 2. File Headers | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐ | Possible |
| 3. Lazy Length | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ **YES** |
| 4. Parallel Load | ⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | Possible |
| 5. Archive Meta | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ **YES** |
| 6. Binary Search | ⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐ | ⭐⭐ | ⭐ | Not Now |
| 7. Incremental | ⭐⭐⭐⭐⭐ | ⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ❌ **NO** |

⭐ = Poor, ⭐⭐⭐⭐⭐ = Excellent

---

## Recommended Approach: Hybrid Strategy

### Phase 1: Quick Wins (Implement First)

**Combine Options 3 + 5:**

1. **Option 3 (Lazy Length):** Modify `DiskBackedQueue` to skip initial counting
   - Show progress as "Processing ortho X..." without total
   - Or show "~N remaining" based on file count estimates

2. **Option 5 (Archive Metadata):** Store final ortho count in archive metadata
   - Already partially done (see `load_archive_metadata` in file_handler.rs)
   - Use this for initial progress estimation

**Benefits:**
- Immediate ~95% reduction in initialization time
- Zero file format changes
- Fully backward compatible
- Can implement in 2-3 hours total

**Tradeoffs:**
- Progress bars are estimated for work queues
- Exact count only available for completed archives

### Phase 2: Polish (Implement If Needed)

**Add Option 1 (Metadata File):**

If Phase 1 UX with estimated progress is insufficient, add precise counting via metadata:

1. Add `queue_metadata.txt` to each queue directory
2. Track count during push/pop operations
3. Persist at flush/close
4. Read during `new_from_path()`

**Benefits:**
- Restores exact progress bars
- Still avoids deserialization during init
- Can be added incrementally

### Phase 3: Future Optimization (If Bottleneck Persists)

**Add Option 4 (Parallel Loading):**

If initialization is still too slow after Phase 1+2:

1. Use rayon for parallel file processing
2. Helps with header reading or metadata aggregation
3. Provides 2-4x speedup on multi-core systems

---

## Implementation Notes

### Correctness Considerations

1. **Metadata Consistency:** Any metadata approach must handle:
   - Partial writes (process crashes during flush)
   - Concurrent access (multiple processes - handled by work folder isolation)
   - Corruption detection (checksums or fallback to counting)

2. **Backward Compatibility:** 
   - Always provide fallback to current counting method
   - Detect missing metadata and regenerate
   - Allow gradual migration

3. **Testing:**
   - Verify count accuracy with existing tests
   - Add tests for metadata persistence across restarts
   - Stress test with large archives (1M+ orthos)

### Performance Expectations

**Current State:**
- Archive with 1M orthos, 100 queue files
- Initialization: ~30-60 seconds (deserialize all)
- Processing: ~5-10 minutes (depending on work)

**After Phase 1 (Lazy + Archive Meta):**
- Initialization: <1 second (skip counting)
- Processing: ~5-10 minutes (unchanged)
- Progress: Estimated or from archive metadata

**After Phase 2 (Add Queue Metadata):**
- Initialization: <1 second (read metadata)
- Processing: ~5-10 minutes (unchanged)
- Progress: Exact counts

---

## Conclusion

The primary bottleneck is **`DiskBackedQueue::new_from_path()` counting all orthos by deserializing them**. The recommended hybrid approach:

1. **Quick Win:** Lazy length calculation (Option 3) - eliminate initialization cost entirely
2. **Enhancement:** Use archive metadata (Option 5) - provide exact counts where they matter
3. **Polish:** Queue metadata file (Option 1) - restore exact progress if needed

This provides 95%+ speedup with minimal risk and effort, while maintaining backward compatibility and correct semantics.

The implementation can be done in phases, allowing for validation at each step, and doesn't require changes to the core ortho serialization format or merge logic.
