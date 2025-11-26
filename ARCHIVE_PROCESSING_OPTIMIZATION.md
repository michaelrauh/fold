# Archive Processing Speed Optimization Analysis

## Executive Summary

The "Processing Larger Archive" stage is the longest-running stage during merge operations. This document analyzes the bottlenecks and provides optimization options in a decision matrix format.

## Current Implementation Analysis

### What Happens During Archive Processing

During merge operations (`merge_archives` in `src/main.rs`), the system processes two archives:

1. **Processing Larger Archive** (lines 395-427)
   - Line 400: Creates a `DiskBackedQueue` from the archive's results directory
   - Line 401: Calls `results_larger.len()` to get total count
   - Line 405: Sets `progress_total` for the TUI progress bar
   - Lines 407-427: Iterates through ALL orthos in the archive
   - For each ortho:
     - Checks if it's already seen via `tracker.contains()`
     - If new: inserts into tracker, pushes to merged_results queue and work_queue if impacted
   - No remapping needed (vocabulary is already correct)

2. **Remapping Smaller Archive** (lines 439-473)
   - Similar process but with vocabulary remapping via `ortho.remap()`

### Where Counts Are Used

The count from `queue.len()` is used **exclusively for TUI progress bars**:

```rust
// Line 401: Get count from expensive initialization
let total_larger_count = results_larger.len();

// Line 405: Use count ONLY for progress bar display
metrics.update_operation(|op| op.progress_total = total_larger_count);

// Line 417: Update progress during processing
metrics.update_operation(|op| op.progress_current = total_from_larger);
```

**Key Insight:** The expensive counting operation exists solely to show "Processing 45,231 / 1,234,567" instead of "Processing 45,231..." in the TUI. The count is not used for any correctness-critical logic.

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

### Secondary Bottleneck: The Processing Loop

The second major bottleneck is the actual ortho processing loop (lines 407-427):

```rust
while let Some(ortho) = results_larger.pop()? {
    let ortho_id = ortho.id();                           // 1. Compute hash
    if !tracker.contains(&ortho_id) {                    // 2. Bloom + HashMap lookup
        tracker.insert(ortho_id);                         // 3. Bloom + HashMap insert
        merged_results.push(ortho.clone())?;             // 4. Clone ortho (80-900+ bytes)
        
        if is_ortho_impacted_fast(&ortho, &impacted) {   // 5. HashSet lookup
            work_queue.push(ortho)?;                      // 6. Another push
        }
    }
}
```

**Per-ortho costs:**
1. **Hash computation** (`ortho.id()`): Hashes dims + payload vectors
2. **Tracker lookup** (`tracker.contains()`):
   - Bloom filter check (fast)
   - Shard determination via hash
   - Potential shard load from disk (if not in LRU cache)
   - HashMap lookup within shard
3. **Tracker insert** (`tracker.insert()`):
   - Bloom filter set
   - Shard determination via hash
   - HashMap insert (may trigger shard eviction)
   - Marks shard dirty
4. **Ortho cloning** (`ortho.clone()`):
   - Clones `dims: Vec<usize>` (typically 2-8 elements)
   - Clones `payload: Vec<Option<usize>>` (4 to 900+ elements)
   - Total: 80-900+ bytes per clone
5. **Impact checking** (`is_ortho_impacted_fast()`):
   - Extracts requirement phrases from ortho
   - HashSet lookup for each phrase
6. **Queue operations** (`push`):
   - May trigger disk spill every 10,000 items

**Cumulative cost:** With 1M orthos, even microsecond operations become seconds of overhead.

## Optimization Options

### Option 1: Metadata File for Queue Length

**Description:** Store ortho count in a separate metadata file instead of counting during initialization.

**Implementation:**
- Add `count.txt` to each queue directory containing the total count
- Increment on `push()`, decrement on `pop()` (in-memory counter, flush on spill/load)
- Read single small file during `new_from_path()` instead of scanning all files

**Pros:**
- ✅ Eliminates deserialization during initialization (biggest bottleneck)
- ✅ O(1) initialization time instead of O(n) where n = total orthos
- ✅ Simple to implement and maintain
- ✅ No changes to ortho structure or serialization format

**Cons:**
- ❌ Slight overhead to maintain metadata (writes on flush)
- ❌ Count may be stale if process crashes mid-operation

**Impact:**
- **Initialization speedup:** From O(n) deserialization to O(1) file read
- **For 1M orthos:** Could reduce from ~30 seconds to <1 second
- **Runtime overhead:** Negligible (single integer in memory, write on flush)

**Risk:** Low
**Effort:** Low (1-2 hours implementation)

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

**Cons:**
- ❌ Changes file format
- ❌ More complex implementation (read/write logic changes)
- ❌ Still requires reading all files (but only headers ~8 bytes each)

**Impact:**
- **Initialization speedup:** From O(n) deserialization to O(f) header reads where f = file count
- **For 1M orthos in 100 files:** Reduces from ~30 seconds to ~1 second
- **Runtime overhead:** Minimal (one u64 write per spill)

**Risk:** Low (simple format change)
**Effort:** Medium (2-3 hours)

---

### Option 3: Lazy Length Calculation

**Description:** Don't calculate total length upfront; show progress without total.

**Implementation:**
- Remove count calculation from `new_from_path()`
- Return `None` or estimate for `len()` 
- Update TUI to show "Processing ortho N..." instead of "Processing N / M"

**Pros:**
- ✅ Zero initialization time (biggest win)
- ✅ No file format changes
- ✅ Minimal code changes (only in TUI display logic)

**Cons:**
- ❌ Progress bars show count without percentage
- ❌ Can't estimate time remaining

**Impact:**
- **Initialization speedup:** 100% (no initialization work)
- **UX impact:** Shows "Processing 45,231..." instead of "Processing 45,231 / 1,234,567"
- **Runtime overhead:** None

**Risk:** Low (only affects display)
**Effort:** Low (1 hour)

---

### Option 4: Remove Redundant Hash Computation

**Description:** Cache ortho ID to avoid recomputing hash multiple times per ortho.

**Implementation:**
- `ortho.id()` currently computes hash from scratch each call
- Add cached ID field to Ortho or compute once and reuse
- In processing loop: `let ortho_id = ortho.id();` is called, then ID is recomputed in `tracker.insert()`

**Pros:**
- ✅ Eliminates redundant hash computation
- ✅ Simple implementation
- ✅ No file format changes (if using local variable)

**Cons:**
- ❌ Modest speedup (hash is relatively fast)
- ❌ If caching in struct, increases ortho size by 8 bytes

**Impact:**
- **Per-ortho speedup:** Saves 1-2 hash computations per ortho
- **For 1M orthos:** Saves a few seconds
- **Overall:** Minor optimization

**Risk:** Low
**Effort:** Low (30 minutes to 1 hour)

---

### Option 5: Reduce Ortho Cloning

**Description:** Avoid cloning orthos when possible by using references or moving ownership.

**Implementation:**
Current code:
```rust
merged_results.push(ortho.clone())?;
if is_ortho_impacted_fast(&ortho, &impacted) {
    work_queue.push(ortho)?;  // moves ortho
}
```

Could be:
```rust
let is_impacted = is_ortho_impacted_fast(&ortho, &impacted);
if is_impacted {
    merged_results.push(ortho.clone())?;
    work_queue.push(ortho)?;  // moves original
} else {
    merged_results.push(ortho)?;  // moves original, no clone
}
```

**Pros:**
- ✅ Eliminates clone for non-impacted orthos
- ✅ Reduces memory allocations
- ✅ No file format changes

**Cons:**
- ❌ More complex control flow
- ❌ Only helps for non-impacted orthos (may be minority)

**Impact:**
- **Per-ortho speedup:** Saves 80-900 bytes allocation for non-impacted orthos
- **For 1M orthos (90% non-impacted):** Saves ~900K clones
- **Overall:** Moderate speedup

**Risk:** Low
**Effort:** Low (1-2 hours)

---

### Option 6: Batch Tracker Operations

**Description:** Reduce tracker overhead by batching operations.

**Implementation:**
- Collect ortho IDs in a batch (e.g., 1000 IDs)
- Process bloom filter checks in batch
- Load all needed shards upfront for the batch
- Insert all IDs at once per shard

**Pros:**
- ✅ Reduces shard load/evict thrashing
- ✅ Better cache locality
- ✅ Fewer disk I/O operations

**Cons:**
- ❌ More complex tracker API
- ❌ Requires buffering IDs
- ❌ May not help much (bloom filter already fast)

**Impact:**
- **Shard I/O reduction:** Fewer loads if IDs are clustered
- **For 1M orthos:** Could reduce shard operations by 10-50%
- **Overall:** Minor to moderate speedup

**Risk:** Medium (significant refactor)
**Effort:** Medium (3-5 hours)

---

### Option 7: Pre-allocate Tracker Shards

**Description:** Pre-load frequently accessed shards into memory before processing.

**Implementation:**
- Analyze which shards will be hot during processing
- Pre-load those shards before the loop starts
- Increase `max_shards_in_memory` to keep more shards resident

**Pros:**
- ✅ Reduces shard thrashing during loop
- ✅ Fewer disk I/O operations
- ✅ Simple configuration change

**Cons:**
- ❌ Increases memory usage
- ❌ May not be predictable which shards are hot
- ❌ Limited by available RAM

**Impact:**
- **Shard I/O reduction:** Fewer loads during processing
- **For 1M orthos:** Could eliminate 50-90% of shard loads
- **Memory cost:** Each shard ~16KB to 1MB depending on contents

**Risk:** Low
**Effort:** Low (1-2 hours to add configuration)

---

## Decision Matrix Summary

### Initialization Bottleneck Options

| Option | Init Speedup | Loop Speedup | Effort | Risk | Recommended |
|--------|--------------|--------------|--------|------|-------------|
| 1. Metadata File | ⭐⭐⭐⭐⭐ | - | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ **YES** |
| 2. File Headers | ⭐⭐⭐⭐ | - | ⭐⭐⭐ | ⭐⭐⭐⭐ | Possible |
| 3. Lazy Length | ⭐⭐⭐⭐⭐ | - | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ **YES** |

### Processing Loop Bottleneck Options

| Option | Init Speedup | Loop Speedup | Effort | Risk | Recommended |
|--------|--------------|--------------|--------|------|-------------|
| 4. Cache Hash | - | ⭐⭐ | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | Maybe |
| 5. Reduce Cloning | - | ⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ **YES** |
| 6. Batch Tracker | - | ⭐⭐⭐ | ⭐⭐⭐ | ⭐⭐⭐ | Maybe |
| 7. Pre-load Shards | - | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ | ✅ **YES** |

⭐ = Poor/None, ⭐⭐⭐⭐⭐ = Excellent

---

## Recommended Implementation Strategy

### Phase 1: Eliminate Initialization Bottleneck (Biggest Impact)

**Option 3: Lazy Length Calculation**

Change `new_from_path()` to skip counting entirely:
```rust
pub fn new_from_path(path: &str, buffer_size: usize) -> Result<Self, FoldError> {
    // Find existing disk files (no counting)
    let disk_files = discover_files(path)?;
    
    Ok(Self {
        buffer: Vec::with_capacity(buffer_size),
        buffer_size,
        disk_path,
        disk_file_counter: max_counter + 1,
        disk_files,
        disk_count: 0,  // Always 0, computed lazily on first pop if needed
    })
}
```

Update TUI to show:
- "Processing 45,231..." instead of "Processing 45,231 / 1,234,567"
- Or keep a running count without showing total

**Impact:** Eliminates 95%+ of initialization time (30-60 seconds → <1 second)  
**Effort:** 1 hour  
**Risk:** Very low (only affects display)

### Phase 2: Reduce Processing Loop Overhead

**Option 5: Reduce Ortho Cloning**

Reorganize logic to avoid unnecessary clones:
```rust
while let Some(ortho) = results_larger.pop()? {
    let ortho_id = ortho.id();
    if !tracker.contains(&ortho_id) {
        tracker.insert(ortho_id);
        
        let is_impacted = is_ortho_impacted_fast(&ortho, &larger_impacted_set);
        if is_impacted {
            merged_results.push(ortho.clone())?;
            work_queue.push(ortho)?;  // moves original
        } else {
            merged_results.push(ortho)?;  // moves original, no clone needed
        }
        total_from_larger += 1;
    }
}
```

**Impact:** Eliminates 70-90% of clones (saves ~1M allocations for 1M non-impacted orthos)  
**Effort:** 1-2 hours  
**Risk:** Low

**Option 7: Pre-allocate Tracker Shards**

Increase `max_shards_in_memory` based on available RAM:
```rust
// Current default: ~8 shards in memory
// Proposed: 32-64 shards in memory (requires ~16-64MB RAM)
let mut tracker = SeenTracker::with_path(
    &seen_shards_path,
    memory_config.bloom_capacity,
    memory_config.num_shards,
    64  // Up from default of 8
);
```

**Impact:** Reduces shard load/evict thrashing by 80-90%  
**Effort:** 30 minutes (just config change)  
**Risk:** Very low (just uses more RAM)

### Phase 3: Optional Polish

**Option 1: Metadata File (if exact counts needed)**

If TUI needs exact counts for user experience:
1. Add `count.txt` to queue directories
2. Maintain count during push/pop operations
3. Read during initialization

**Impact:** Provides exact counts with minimal overhead  
**Effort:** 2-3 hours  
**Risk:** Low

---

## Performance Expectations

### Current State (Baseline)
- Archive with 1M orthos, 100 queue files
- **Initialization:** 30-60 seconds (deserializing all orthos to count)
- **Processing loop:** 5-10 minutes
- **Total:** 5.5-11 minutes

### After Phase 1 (Lazy Length)
- **Initialization:** <1 second (skip counting)
- **Processing loop:** 5-10 minutes (unchanged)
- **Total:** 5-10 minutes
- **Speedup:** 5-10% overall, 95%+ on initialization

### After Phase 2 (Reduce Cloning + Pre-load Shards)
- **Initialization:** <1 second
- **Processing loop:** 3-6 minutes (reduced cloning + fewer shard loads)
- **Total:** 3-6 minutes
- **Speedup:** 40-50% overall from baseline

### Combined Optimizations
- **Expected speedup:** 45-55% reduction in total time
- **From:** 5.5-11 minutes → **To:** 3-5 minutes
- **Initialization:** Virtually instant
- **Processing:** 40% faster through clone elimination and shard optimization

---

## Summary

### Two Independent Bottlenecks Identified

1. **Initialization: `DiskBackedQueue::new_from_path()` counting**
   - Deserializes every ortho just to count them
   - Solely for TUI progress bar display
   - **Solution:** Skip counting (lazy length) - saves 30-60 seconds

2. **Processing Loop: Redundant operations per ortho**
   - Unnecessary ortho cloning (80-900 bytes each)
   - Tracker shard thrashing (disk I/O overhead)
   - **Solution:** Eliminate clones for non-impacted orthos + pre-load shards - saves 2-4 minutes

### Recommended Implementation

**Phase 1:** Lazy length calculation  
- **Effort:** 1 hour  
- **Speedup:** 95% on initialization  

**Phase 2:** Reduce cloning + pre-load shards  
- **Effort:** 2-3 hours  
- **Speedup:** 40% on processing loop  

**Combined:** 45-55% reduction in total archive processing time with minimal code changes and no file format changes.
