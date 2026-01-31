# Troubleshooting Report: Early Termination on 8k Input

## Summary

The `run_doubling` process on the droplet is ending far too early when processing the 8k word input (e.txt - "A Princess of Mars" by Edgar Rice Burroughs). This report identifies the root cause after comparing the current branch to main.

## Root Cause Identified

**The current branch has introduced a new completion pruning feature (`completion_pruning.rs`) that aggressively prunes completions at the root level. This pruning is NOT present in the main branch.**

### Key Difference

| Aspect | Main Branch | Current Branch |
|--------|-------------|----------------|
| Completion Pruning | **Not present** | **Present** |
| Processing Model | Continuous BFS (work_queue until empty) | Generational processing with transitions |
| Root Pruning | No root-level pruning | Prunes single-span completions |
| Termination Condition | Queue empty and tracker buffer empty | `new_work == 0` after generation transition |

## Technical Analysis

### The Problematic Pruning Logic

The new pruning is implemented in `src/completion_pruning.rs`, function `bound_completion()`:

```rust
if required.is_empty() {
    // Empty ortho: require initial span > 1 (i.e., more than a single chain).
    let completion_count = interner
        .completions_for_prefix(&vec![completion])
        .expect("missing completions bitset for single-token prefix")
        .count_ones(..);
    return completion_count <= 1;  // <-- PRUNE if only 1 continuation
}
```

This means:
1. When processing the seed/root ortho (`required.is_empty()` is true)
2. For each possible first token `completion`
3. Look up how many continuations that token has
4. **Prune the token if it only has 1 continuation**

### Why This Causes Early Termination

The pruning triggers in `src/generation_runner.rs` at lines 371-379:

```rust
for completion in completions {
    if bound_completion(&ortho, completion, interner, best_score) {
        metrics.increment_pruned_completions(1);
        if required.is_empty() {
            // Root span prune
            metrics.increment_pruned_root_span(1);
        } else {
            metrics.increment_pruned_bound(1);
        }
        continue;  // <-- Skip this completion entirely
    }
    // ... rest of processing
}
```

After the generation transition (`on_generation_end`), if all completions were pruned, `new_work == 0`, and the loop terminates:

```rust
if new_work == 0 {
    metrics.add_log("No new work after transition; stopping generations".to_string());
    break;
}
```

### Why the 8k Input is Affected

**Hypothesis (requires verification):** The text "A Princess of Mars" may have vocabulary characteristics where many words only have single continuations in the sentence structure. This could occur because:

- Many unique word sequences where word A is always followed by word B (and B is never the start of another sequence)
- These "single chain" tokens are all pruned at the root level
- If *most* tokens in the vocabulary are single-chain, the system prunes almost everything

This hypothesis is supported by the characteristics of the text:
- Proper nouns (character names like "Dejah Thoris", place names like "Helium") that appear in fixed phrases
- Technical or archaic vocabulary with limited usage patterns
- Victorian-era prose style with specific patterns

**To confirm this hypothesis**, check the TUI metrics or log output for `pruned_root_span` counts during processing. If most completions show high `pruned_root_span` values relative to `expanded_completions`, this confirms the root-level pruning is the culprit.

### Main Branch Behavior (Why It Works)

The main branch does NOT have this pruning. It processes **all** completions regardless of their continuation count:

```rust
// From main branch src/main.rs - no bound_completion call
for chunk in completions.chunks(COMPLETION_CHUNK_SIZE) {
    let mut batch_ids = Vec::new();
    for completion in chunk {
        let children = ortho.add(*completion);  // <-- ALL completions processed
        // ...
    }
}
```

## Evidence

1. **Diff shows new file**: `src/completion_pruning.rs` is entirely new (258+ lines added)
2. **Diff shows usage**: `generation_runner.rs` imports and uses `bound_completion`
3. **No equivalent in main**: `git show origin/main:src/main.rs | grep "bound"` returns only one unrelated hit
4. **Test name suggests intent**: `test bound_prunes_deep_single_span_at_root` confirms this is intentional behavior

## Reproduction Steps

1. Use the 8k word input (e.txt)
2. Run `./run_doubling.sh e.txt`
3. Observe early termination with message "No new work after transition; stopping generations"

## Potential Fixes

### Option 1: Disable Root-Level Span Pruning (Conservative)

Modify `bound_completion` to not prune at root level:

```rust
if required.is_empty() {
    // Option 1: Remove root pruning entirely
    return false;
}
```

### Option 2: Relax the Threshold (Moderate)

```rust
if required.is_empty() {
    // Allow tokens with very few continuations (not just single-chain)
    let completion_count = interner
        .completions_for_prefix(&vec![completion])
        .expect("missing completions bitset for single-token prefix")
        .count_ones(..);
    return completion_count == 0;  // Only prune if NO continuations (dead end)
}
```

### Option 3: Add Fallback (Safe)

Add a fallback mechanism that detects when all completions are pruned and reverts to unpruned expansion for at least some candidates.

### Option 4: Make Pruning Configurable

Add an environment variable or config option to disable the aggressive root pruning for certain inputs.

## Recommendation

The root-level "single-span pruning" appears to be an optimization that makes assumptions about input characteristics that don't hold for all texts. 

**Recommended action**: Disable or relax the root-level pruning constraint (Option 1 or 2). The pruning at deeper levels (when `best_score` is established) may still be valuable, but the root-level heuristic is too aggressive.

## Files Involved

- `src/completion_pruning.rs` - New pruning module (root cause)
- `src/generation_runner.rs` - Uses the pruning, handles termination
- `src/generation_store.rs` - New generational model with `on_generation_end`
- `src/main.rs` - Orchestrates the processing

## Conclusion

The early termination is caused by the new completion pruning feature that rejects tokens with only single continuations. For the 8k "Princess of Mars" text, this pruning appears to be too aggressive, resulting in root-level completions being pruned, which causes the generation loop to terminate with "No new work after transition."

**To verify this root cause:**
1. Run the 8k input and monitor the `pruned_root_span` metric in the TUI or logs
2. Compare to the `expanded_completions` metric
3. If `pruned_root_span >> expanded_completions` (pruned much larger than expanded), this confirms the diagnosis

The main branch does not have this issue because it lacks the completion pruning feature entirely, processing all completions regardless of their continuation characteristics.
