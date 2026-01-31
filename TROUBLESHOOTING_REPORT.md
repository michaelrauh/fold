# Troubleshooting Report: Early Termination on 8k Input

## Summary

The `run_doubling` process on the droplet is ending far too early when processing the 8k word input (e.txt - "A Princess of Mars" by Edgar Rice Burroughs). After clarification that the pruning is intentional, this report identifies a **bug in the upper bound calculation** that causes over-pruning, leading to premature termination after ~5 generations.

## Root Cause Identified

**BUG: The `upper_bound_score` function in `completion_pruning.rs` incorrectly computes the fullness upper bound, causing valid search paths to be pruned prematurely.**

### The Bug

In `src/completion_pruning.rs`, the `upper_bound_score` function returns:

```rust
pub fn upper_bound_score(
    axis_totals: &[usize],
    min_volume: usize,
    dim_count: usize,
) -> (usize, usize) {
    let mut volume_upper: usize = 1;
    for t in axis_totals.iter().take(dim_count) {
        volume_upper = volume_upper.saturating_mul(t.saturating_sub(1));
    }
    volume_upper = volume_upper.max(min_volume);
    (volume_upper, volume_upper)  // <-- BUG: fullness_upper should NOT equal volume_upper
}
```

**The problem:** Setting `fullness_upper = volume_upper` is mathematically incorrect.

- **Volume** = product of (dim - 1) for each dimension
- **Fullness** = count of filled cells (total capacity)

For a 3×3 ortho:
- Volume = (3-1) × (3-1) = **4**
- Max Fullness = 3 × 3 = **9** (all cells can be filled)

**Fullness can exceed volume!** The current code underestimates the fullness potential, causing over-pruning.

### How This Causes Early Termination (~5 Generations)

1. As processing continues, `best_score` increases (e.g., to `(4, 5)`)
2. `upper_bound_score` returns `(volume_upper, volume_upper)`, e.g., `(4, 4)`
3. The comparison `potential_score <= best_score` becomes `(4, 4) <= (4, 5)`
4. Since `4 <= 5` when first elements are equal, this is **true** → we **PRUNE**
5. But the path could have led to fullness 6, 7, 8, or 9!

This explains why the search terminates after ~5 generations: by that point, `best_score.1` (fullness) has grown enough that the underestimated `fullness_upper` causes valid paths to be pruned.

## NOTE: Root-Level Pruning is Intentional

The root-level "single-span" pruning (lines 12-18) is **intentional** and correct:

```rust
if required.is_empty() {
    // Empty ortho: require initial span > 1 (i.e., more than a single chain).
    let completion_count = interner
        .completions_for_prefix(&vec![completion])
        .expect("missing completions bitset for single-token prefix")
        .count_ones(..);
    return completion_count <= 1;  // Intentional: prunes dead-end single chains
}
```

This prunes single-chain tokens at the root level, which is a valid optimization to avoid exploring dead paths. **This is NOT the bug.**

## Technical Analysis

### Key Difference from Main Branch

| Aspect | Main Branch | Current Branch |
|--------|-------------|----------------|
| Completion Pruning | **Not present** | **Present (with bug)** |
| `upper_bound_score` | N/A | Returns `(vol, vol)` - **underestimates fullness** |
| Processing Model | Continuous BFS | Generational with anti-join |
| Termination | Queue empty | `new_work == 0` after generation |

### The Pruning Flow

The pruning triggers in `src/generation_runner.rs` at lines 371-379:

```rust
for completion in completions {
    if bound_completion(&ortho, completion, interner, best_score) {
        metrics.increment_pruned_completions(1);
        // ...
        continue;  // Skip this completion
    }
    // ... process completion
}
```

Within `bound_completion()`, when `required` is NOT empty (generations > 0):

```rust
let potential_score = upper_bound_score(&totals, ortho.volume(), dim_count);
potential_score <= best_score  // <-- BUG: fullness comparison is wrong
```

## Fix Required

The `upper_bound_score` function should compute a proper upper bound for fullness:

```rust
pub fn upper_bound_score(
    axis_totals: &[usize],
    min_volume: usize,
    dim_count: usize,
) -> (usize, usize) {
    let mut volume_upper: usize = 1;
    let mut capacity_upper: usize = 1;  // NEW: track max cells separately
    
    for t in axis_totals.iter().take(dim_count) {
        volume_upper = volume_upper.saturating_mul(t.saturating_sub(1));
        capacity_upper = capacity_upper.saturating_mul(*t);  // Total cells = product of dims
    }
    volume_upper = volume_upper.max(min_volume);
    
    // Fullness can reach up to capacity (all cells filled)
    (volume_upper, capacity_upper)
}
```

Alternatively, use `usize::MAX` for fullness to never prune based on fullness:

```rust
(volume_upper, usize::MAX)
```

## Evidence

1. **Bug in `upper_bound_score`**: Returns `(volume_upper, volume_upper)` but fullness can exceed volume
2. **~5 generation cutoff**: Matches the pattern where `best_score.1` (fullness) grows enough to trigger bad pruning
3. **Math verification**: For 3×3 ortho, volume=4 but max_fullness=9
4. **Diff shows new file**: `src/completion_pruning.rs` is entirely new (258+ lines added)

## Reproduction Steps

1. Use the 8k word input (e.txt)
2. Run `./run_doubling.sh e.txt`
3. Observe termination after ~5 generations
4. Check logs for high `pruned_bound` counts (not `pruned_root_span`)

## Recommended Fix

**Option 1: Fix the fullness upper bound calculation (Recommended)**

```rust
pub fn upper_bound_score(
    axis_totals: &[usize],
    min_volume: usize,
    dim_count: usize,
) -> (usize, usize) {
    let mut volume_upper: usize = 1;
    for t in axis_totals.iter().take(dim_count) {
        volume_upper = volume_upper.saturating_mul(t.saturating_sub(1));
    }
    volume_upper = volume_upper.max(min_volume);
    
    // Use usize::MAX for fullness to prevent over-pruning on fullness
    // (volume-based pruning is the primary bound)
    (volume_upper, usize::MAX)
}
```

**Option 2: Compute proper capacity**

```rust
let mut capacity_upper: usize = 1;
for t in axis_totals.iter().take(dim_count) {
    capacity_upper = capacity_upper.saturating_mul(*t);
}
(volume_upper, capacity_upper)
```

## Files Involved

- `src/completion_pruning.rs` - Contains the buggy `upper_bound_score` function (line 119-130)
- `src/generation_runner.rs` - Uses `bound_completion` for pruning decisions
- `src/generation_store.rs` - Generational model with `on_generation_end`

## Conclusion

The early termination after ~5 generations is caused by a **bug in the `upper_bound_score` function** that incorrectly sets `fullness_upper = volume_upper`. Since fullness (number of filled cells) can significantly exceed volume (product of dim-1), the upper bound underestimates the potential score, causing valid search paths to be pruned.

**The fix is simple**: Change line 129 in `completion_pruning.rs` from:
```rust
(volume_upper, volume_upper)
```
to:
```rust
(volume_upper, usize::MAX)
```

This ensures pruning is based only on volume bounds, not the incorrectly computed fullness bound. The root-level "single-span" pruning (lines 12-18) is intentional and correct - only the bound-based pruning (line 41) has the bug in its upper bound calculation.
