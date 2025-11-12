# Path Independence Fix - Restoring Canonical ID Computation

## Problem Identified

The incremental ID computation introduced in commit 6961816 caused **path-dependent IDs**, which violated the fundamental correctness requirement of the fold system: **IDs must be path and rotation independent**.

### What Went Wrong

**Before (Correct)**:
```rust
ID = hash(dims, payload)  // Based on canonical state
```
- Ortho with `[Some(10), Some(20), Some(30), None]` always got the same ID
- Canonicalization swap at position 2 ensured `[10, 30, 20]` → `[10, 20, 30]` → same ID

**After Incremental Hashing (Incorrect)**:
```rust
ID = hash(parent_id, added_value)  // Based on construction path
```
- Ortho built as `add(10).add(20).add(30)` got different ID than `add(10).add(30).add(20)`
- Even after canonicalization swapped values, IDs remained different
- **Broke deduplication** - same canonical orthos treated as different

### Why This Matters

The canonicalization code (line 62 in ortho.rs):
```rust
if second > third { 
    new_payload[1] = Some(third); 
    new_payload[2] = Some(second); 
}
```

This swap ensures that different construction orders produce the same canonical payload. The ID **must** reflect this canonical state, not the path taken.

## Solution

Reverted to **canonical state-based ID computation** while keeping the optimizations:

### What We Kept (Good)

1. ✅ **Stored `id` field** - Fast O(1) lookup via field access
2. ✅ **FxHash** - 81% faster than DefaultHasher (51ns → 9.6ns)
3. ✅ **Removed `version` field** - Cleaner design, no size change
4. ✅ **Optimized expand()** - No intermediate clones

### What We Changed (Fix)

```rust
// NEW: Compute ID from canonical state
fn compute_id(dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
    let mut hasher = FxHasher::default();
    dims.hash(&mut hasher);
    payload.hash(&mut hasher);
    (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
}

// In add() and expand()
let new_id = Self::compute_id(&dims, &new_payload);
```

Now IDs are computed **after** canonicalization, ensuring path-independence.

## Performance Impact

### Before This Fix (Incremental Hashing)

| Operation | Time | Notes |
|-----------|------|-------|
| ortho_id | 0.31ns | Field access only |
| ortho_add | 49ns | With incremental ID |
| worker_loop | ~235ns | ~10% improvement |

**Problem**: Path-dependent, incorrect behavior.

### After This Fix (Canonical Hashing)

| Operation | Time (est) | Notes |
|-----------|------------|-------|
| ortho_id | 0.31ns | Still field access (stored) |
| ortho_add | 58-60ns | +Hash computation |
| worker_loop | ~248ns | ~5% improvement vs baseline |

**Result**: Path-independent, correct behavior + FxHash speedup.

### Trade-off Analysis

**What we lost**:
- ~10ns per add operation (hash computation)
- ~5% of the incremental speedup

**What we gained back**:
- **Correctness** - IDs are now path-independent ✅
- **Canonicalization works** - Same canonical state → same ID ✅
- **Deduplication works** - Proper ortho deduplication ✅

**What we kept**:
- 81% faster hashing (FxHash vs DefaultHasher)
- Fast ID lookup (stored field, not recomputed)
- Optimized expand() (no clones)
- ~5% overall worker loop improvement

## At Scale Impact

### Time Estimates (625M Orthos Per Chunk)

**Baseline (DefaultHasher, no optimizations)**:
- ID: 51ns × 625M = 31.9 seconds
- Worker loop: 262ns × 625M = 164 seconds

**With Incremental Hashing (WRONG)**:
- ID: 0.31ns × 625M = 0.2 seconds (fast but incorrect!)
- Worker loop: 235ns × 625M = 147 seconds

**With This Fix (CORRECT)**:
- ID: 0.31ns × 625M = 0.2 seconds (field access, computed once)
- Worker loop: 248ns × 625M = 155 seconds
- **Net: ~9 seconds saved vs baseline, CORRECT behavior**

### Full Book (17B Orthos)

- Saved vs baseline: ~4 minutes (not 14 minutes, but correct!)
- Still significant improvement from FxHash + optimizations

## Verification

### Test Coverage

All 92 tests pass, including:

1. ✅ `test_canonicalization_invariant_axis_permutation` - Verifies path independence
2. ✅ `test_add_path_independent_ids` - Different paths → same canonical state → same ID
3. ✅ `test_id_path_independent_behavior` - Comprehensive path independence check
4. ✅ `test_get_requirements_order_independent` - Requirements same regardless of order

### Example Behavior

```rust
// Path 1: add in order
let o1 = Ortho::new(1).add(10).add(20).add(30);
// Payload: [Some(10), Some(20), Some(30), None]
// ID: hash([2,2], [Some(10), Some(20), Some(30), None])

// Path 2: add out of order (canonicalized by swap)
let o2 = Ortho::new(1).add(10).add(30).add(20);
// Payload after swap: [Some(10), Some(20), Some(30), None]
// ID: hash([2,2], [Some(10), Some(20), Some(30), None])

assert_eq!(o1.id(), o2.id());  // ✅ SAME - path independent!
assert_eq!(o1.payload(), o2.payload());  // ✅ SAME - canonical
```

## Bottom Line

**Correctness over speed**: We reverted the incremental hashing because path-independence is a **correctness requirement**, not a performance nice-to-have.

**Still optimized**: We kept all other optimizations (FxHash, stored ID, optimized expand) for a **~5% net improvement** that maintains correctness.

**At 625M scale**: We save ~9 seconds per chunk (vs baseline) while ensuring proper canonicalization and deduplication.

## Commit Summary

- Reverted incremental ID computation (was path-dependent, incorrect)
- Restored canonical state-based ID computation (path-independent, correct)
- Kept FxHash optimization (81% faster than DefaultHasher)
- Kept stored `id` field (O(1) lookup)
- Kept optimized expand() (no clones)
- All 92 tests pass
- ~5% net improvement vs baseline, correct behavior ✅
