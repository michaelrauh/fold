# Code Optimizations Implementation Plan

Based on the SPEED_ANALYSIS.md document, this file tracks the implementation status of Phase 1 code optimizations.

## Completed
- ✅ Added `rayon` and `dashmap` dependencies for parallel processing
- ✅ Added `smallvec` dependency (already present)  
- ✅ Implemented parallel batch processing using Rayon (see src/lib.rs process_text())

## Pending - Phase 1 Code Optimizations (35-60% speedup expected)

### 1. Cache Ortho IDs ⏸️
**Status**: Attempted but blocked by serialization requirements
**Issue**: Adding cached_id field (OnceCell or Cell) breaks bincode serialization
**Benefit**: 5-10% speedup (avoid redundant hashing)
**Solution needed**: Either:
- Implement custom Encode/Decode for Ortho to skip cache field
- Use a separate HashMap<payload_hash, id> cache outside Ortho
- Accept serialization format change and update all checkpoint files

### 2. Use SmallVec for Ortho Payload ⏸️
**Status**: Attempted but requires extensive test refactoring  
**Issue**: Changing `payload: Vec<Option<usize>>` to `payload: SmallVec<[Option<usize>; 16]>` breaks all test code that directly constructs Ortho
**Benefit**: 5-10% speedup (avoid heap allocation for small orthos, most are <16 elements)
**Solution needed**:
- Add helper method `Ortho::from_parts()` for test construction
- Update all 50+ test constructions to use helper
- Verify serialization compatibility

### 3. Reduce Cloning in Worker Loop ⚠️
**Status**: Partially optimized
**Current**: Line 187 in src/lib.rs clones child for frontier_orthos before pushing to queue
**Benefit**: 15-25% speedup (cloning is ~10-15% of total time)
**Potential optimizations**:
```rust
// Current (line 187-188):
frontier_orthos_ref.lock().unwrap().insert(child_id, child.clone());
work_queue.push(child)?;

// Option A: Use Rc for shared ownership
frontier_orthos_ref.lock().unwrap().insert(child_id, Rc::clone(&child));
work_queue.push(child)?;

// Option B: Store only IDs in frontier, retrieve from queue when needed
frontier_ref.insert(child_id);
work_queue.push(child)?;
// Later retrieve from disk-backed queue if needed
```

### 4. Shrink Ortho Memory Footprint 📋
**Status**: Design documented in SPEED_ANALYSIS.md
**Changes needed**:
```rust
// Remove version field (8 bytes) - only needed for empty orthos
// Change payload from Vec<Option<usize>> to dense storage
pub struct Ortho {
    dims: Vec<usize>,
    values: Vec<usize>,  // Only filled values
    current_position: u16,  // Track insertion point
}
```
**Benefit**: 5-10% speedup + 60-100 MB memory savings for 10M orthos
**Complexity**: HIGH - requires changes to:
- All Ortho construction code
- `get_requirements()` method
- Serialization/deserialization
- All tests

### 5. Optimize HashSet Operations ✅  
**Status**: Already done
**Implementation**: Worker loop uses `insert()` return value (line 157) instead of `contains()` + `insert()`

## Implementation Priority

**Recommended order**:
1. ✅ Multithreading (DONE) - 2-4x speedup
2. Reduce cloning (Option B above) - 15-25% speedup, moderate complexity
3. Cache Ortho IDs - 5-10% speedup, needs serialization solution
4. SmallVec for payload - 5-10% speedup, needs test refactoring
5. Shrink Ortho footprint - 5-10% speedup, high complexity

**Expected cumulative gain**: 35-60% on top of parallel speedup = ~3-7x total

## Notes

- ID caching and SmallVec are blocked by technical constraints, not design issues
- Reducing cloning should be next priority (good ROI, moderate complexity)
- Shrinking Ortho footprint should wait until other optimizations are done (high risk of regression)
