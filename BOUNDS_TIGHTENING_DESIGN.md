# Bound-tightening proposals

## Problem

The optimistic bound function in `src/completion_pruning.rs` is the engine
that drives pruning. Looser bounds → less pruning → bigger trees → longer
runs. Empirically the bounds are loose enough that the current run faces a
~14-year ETA.

The clearest evidence: every one of the 90 sampled pending shards has a root
`top_bound_volume` between 288 and 2,304, but the largest *frontier* bound
reaches 31,850,496 — a 4–5 order-of-magnitude gap between root estimates and
deep-frame estimates within the same shard. The bound function blows up as
the search descends, and most of that blow-up is slack.

This document enumerates the slack sources visible in the code and proposes
tightenings ranked by effort vs. payoff.

## How the bound is computed today

Entry point: `existing_ortho_upper_bound_ctx_with_impacted` at
`completion_pruning.rs:565`. Logic:

1. For each currently-required prefix in the partial ortho, look up
   `interner.prefix_stats(prefix) = max_desc_len` — the longest descendant
   chain in the corpus that starts from this prefix.
   (`completion_pruning.rs:572-583`)
2. Collect those values into `axis_totals`.
3. For axes that *don't yet exist* (because the ortho hasn't expanded into
   them), use a **global fallback**:
   `fallback_total = interner.max_prefix_len()` — the maximum across the
   *entire corpus*. (`completion_pruning.rs:600`)
4. `upper_bound_score_inline` (`completion_pruning.rs:670-723`) takes the
   top-`dim_count` axis totals, multiplies them, and returns:
   - `volume_upper = ∏(top[i] - 1)`
   - `fullness_upper = ∏(top[i])`

A secondary tightening in the runner at `dfs_runner.rs:543-557` uses the
intersection count `k` of valid completions and replaces the bound if
`(k-1)^dim_count` is tighter.

## Five concrete looseness sources

### 1. `fallback_total` is the global max (biggest single source of slack)

`completion_pruning.rs:600,469`:

```rust
let fallback_total = interner.max_prefix_len().max(2);
```

Used for axes that don't exist yet. The intent is to upper-bound what could
*eventually* be added. But picking the **whole-corpus** maximum means a
partial ortho with one cell filled gets a bound that assumes future axes
could each independently reach the longest chain in the corpus.

In practice the maximum chain length depends heavily on the words already
placed. A partial ortho `[the, of, ...]` will not be extendable in axes
where chains starting with those words are short — but the bound assumes
the global max anyway.

**Tightening:** replace with `interner.max_suffix_depth(filled_prefix)`
(`interner.rs:695`), which gives the max depth conditional on the current
filled prefix.

**Effort:** small (~10 lines: collect `filled_prefix` from the ortho,
replace the call site).
**Expected impact:** 2–10× tightening. The fallback is invoked for every
axis above `existing_axes`, which is most of them in early-stage orthos.

### 2. Independent multiplication across axes

`upper_bound_score_inline` at `completion_pruning.rs:706-708`:

```rust
volume_upper = volume_upper.saturating_mul(total.saturating_sub(1));
fullness_upper = fullness_upper.saturating_mul(total);
```

This treats each axis independently. In reality:

- A word placed at one cell *forbids* itself from any other cell in the
  ortho (distinctness).
- The corpus's phrase rules forbid certain co-occurrences across axes
  (e.g., "if the word at `(0,0)` is X, then `(0,1)` must be in some narrow
  set").

Independent multiplication ignores both constraints, vastly overestimating
the joint feasibility.

**Tightening A (cheap):** cap `fullness_upper` at `interner.vocab_size()`.
You can't fit more distinct cells than there are distinct words.

```rust
fullness_upper = fullness_upper.min(interner.vocab_size());
volume_upper = volume_upper.min(interner.vocab_size().saturating_sub(1));
```

This binds rarely for small orthos but caps the absurd upper-bound regime
where the product runs into the millions despite a vocab of 6,570 words.

**Effort:** trivial (~3 lines).
**Expected impact:** small typical case, occasional big-impact in deep
frames. Mostly a sanity cap.

**Tightening B (medium):** model pairwise compatibility. Precompute, for
each pair of words `(w1, w2)`, the maximum number of cells that pair could
co-occupy in any ortho consistent with the corpus. Use during bound
calculation.

**Effort:** medium (~100 lines + memory: vocab² table = 43M entries here,
manageable).
**Expected impact:** 10–100× on dense corpora.

### 3. The `(k-1)^dim_count` tightening uses one `k` for every dimension

`dfs_runner.rs:543-553`:

```rust
let dim_count = frame_ctx.dim_count();
let k_vol = saturating_pow_usize(k.saturating_sub(1), dim_count)
    .max(frame_ctx.base_volume());
let k_full = saturating_pow_usize(k, dim_count)
    .max(frame_ctx.base_fullness());
let k_bound = OrthoScore::optimistic_bound(k_vol, k_full);
if k_bound < frame.optimistic_bound {
    frame.optimistic_bound = k_bound;
}
```

`k` is the intersection count *at the next-completion level*, then the code
assumes every dimension can independently realize `k`. Same independence
fallacy as #2.

**Tightening:** for each candidate first placement, compute `k₂` (the
intersection count *after* placing it). The actual bound for the second
axis is at most `max(k₂ over candidates)`. Apply the same one level deeper
for `k₃` if budget allows.

**Effort:** medium (~30 lines: extra intersection calls per top-level
candidate).
**Expected impact:** 5–50× when corpora narrow rapidly with depth — which
is the common case in natural language.

### 4. `prefix_stats` returns chain length, not distinct-value count

`completion_pruning.rs:574,427`:

```rust
let max_desc_len = interner.prefix_stats(prefix);
```

`prefix_stats` returns the *longest descendant chain* starting from the
prefix (`interner.rs:666`). For ortho volume, what matters is *how many
distinct values* can fill the axis, not how deep the chain goes. The two
coincide only when chains never repeat words — the optimistic assumption.

**Tightening:** use `interner.completion_count_for_prefix(prefix)`
(`interner.rs:717`) for the per-axis distinct-value count. Combine with the
words already placed in cells that share the axis: subtract those (already
tracked as `forbidden_usize` in `CompletionContext`).

**Effort:** medium (need to thread per-axis forbidden sets, which already
partially exist as `CompletionContext::forbidden_usize`).
**Expected impact:** modest in shallow frames, larger as the ortho fills.

### 5. Parent frames don't get bound-tightened from child outcomes

When a child frame finishes with an actual bound much lower than its
parent's stored estimate, the parent's
`SearchFrame::optimistic_bound` is *not* updated. The next time the parent
re-evaluates, it's still using the original loose estimate.

Looking at the post-children pass at `dfs_runner.rs:632-640`:

```rust
if toggles.compute_bounds && !prune_completions && !frame.branches.is_empty() {
    for branch in &mut frame.branches {
        frame_ctx.reset_for_node_compact(&branch.child);
        // ... (refines per-branch bound)
    }
}
```

This refines per-branch bounds but doesn't propagate the refined bounds
*upward* to constrain the parent. A parent with bound 31.85M whose only
remaining children turn out to be cheap (real bounds 1k each) keeps the
31.85M label and stays unprunable until exhaustion.

**Tightening:** when the runner pops a frame (i.e., a frame is exhausted)
**or** when all of a frame's branches have been pre-computed, set:

```rust
parent.optimistic_bound = parent.branches
    .iter()
    .map(|b| b.optimistic_bound)
    .max()
    .unwrap_or(parent.optimistic_bound);
```

If the parent's children's max bound is much lower than the parent's
stored bound, the parent itself is now prunable against the incumbent.

**Effort:** small (~15 lines, mostly inside the existing pop logic).
**Expected impact:** 2–10× for deep, partially-explored shards — exactly
the regime that dominates current runtime.

## Summary table

| Change | File:line | Effort (LoC) | Expected tightening |
|--------|-----------|--------------|---------------------|
| `fallback_total` → `max_suffix_depth(filled_prefix)` | `completion_pruning.rs:600,469` | ~10 | 2–10× |
| Cap `volume_upper` / `fullness_upper` at `vocab_size` | `completion_pruning.rs:665-666` | ~3 | 1.1–3× |
| Ancestor bound back-propagation on pop | `dfs_runner.rs` (pop path) | ~15 | 2–10× |
| 2-step lookahead intersection | `dfs_runner.rs:543-557` | ~30 | 5–50× |
| Per-axis distinct-value count from `forbidden_usize` | `completion_pruning.rs:570-585` | ~20 | 1.5–5× |
| Pairwise compatibility table | new module | ~100 + memory | 10–100× |
| LP relaxation per node | major | ≫100 | 50–1000× |

## The recommended trio (shortest path to weeks-not-decades)

These three are surgical, locally-scoped, and stack:

1. **Context-aware fallback.** Replace
   `interner.max_prefix_len()` with `interner.max_suffix_depth(filled_prefix)`
   at `completion_pruning.rs:600,469`. This single change eliminates the
   biggest egregious source of slack — using the *global* corpus maximum for
   axes that haven't been added yet.
2. **Vocab-size cap.** Add `min(volume_upper, vocab_size - 1)` and
   `min(fullness_upper, vocab_size)` at `completion_pruning.rs:665-666`.
   Three lines. Catches the absurd-bound regime.
3. **Ancestor bound back-propagation.** When a frame's branches are all
   computed (or the frame pops), update parent's stored bound to
   `max(child.optimistic_bound)`. ~15 lines added to the pop logic in
   `dfs_runner.rs`.

Combined cost: ~30 lines spread across two files. Combined expected
tightening: 10–100×.

## How to validate without committing

For each change:

1. Take a fresh `state.bin` snapshot.
2. Build the modified estimator binary against the changed bound function.
3. Run `estimate_parallel --samples-per-bucket 2 --max-sample-steps
   200000000 --no-running` against the snapshot.
4. Compare the resulting per-bucket `lower_bound_nodes` and `min_censored_nodes`
   to the baseline. A tighter bound should:
   - Reduce censoring rate (fewer samples hit the budget cap because they
     prune earlier).
   - Lower per-shard `nodes_expanded` for finished samples.
   - Reduce the run-level `nodes_expanded / sample` ratio.

If the tightening doesn't measurably improve those metrics, it's not
fixing the right slack and should be reverted.

## How this stacks with the other ideas

- **Incumbent bootstrap** (separate design): a real high-volume incumbent
  multiplies the value of *every* tightening, because pruning fires only
  when `bound ≤ incumbent`. Tighter bounds mean the incumbent prunes more.
- **Dutch auction** (separate design): tighter bounds make each pass at a
  given floor finish faster, because more frames prune at the root.

The three approaches are multiplicative, not additive. Bootstrap raises the
incumbent (1 OoM); auction adds a synthetic floor when no incumbent yet
exists; tightening makes both more effective. Doing all three is the
cheapest path to closing a 14-year run in weeks.

## Risk register

| risk | mitigation |
|------|------------|
| A "tightening" introduces a non-admissible bound, missing the optimum | Validate by running the smaller benchmark inputs (`e_tiny.txt`, `e_bench.txt`) and confirm the same incumbent is found. Bounds must be `≥ true optimum` for soundness. |
| `max_suffix_depth` is more expensive than `max_prefix_len` per call | Cache per-prefix at the `CompletionContext` level; lookup is O(1) amortized. |
| Vocab-size cap binds when the *actual* answer exceeds it | Mathematically impossible: an ortho cannot have more distinct cells than the vocab has words. The cap is admissible by definition. |
| Ancestor back-propagation has a subtle bug (loses a candidate optimum) | Bound is a *max* over child bounds; this is monotone and admissible by induction. |
| 2-step lookahead doubles bound-compute cost per frame | Net win unless the bound-tightening fails to prune a single extra frame per node. The cost is O(k) extra intersections; the benefit, when it fires, is removing entire subtrees. |

## Implementation order

1. **Vocab-size cap** (hours): trivial, immediate sanity check.
2. **Context-aware fallback** (1 day): one of the biggest wins per LoC.
   Validate against benchmark inputs before deploying.
3. **Ancestor back-propagation** (1 day): orthogonal to (1) and (2);
   compounds with both.
4. **Verify with the estimator + benchmark inputs** that all three
   together don't break correctness (incumbent for `e_tiny.txt` is
   unchanged) and demonstrably improve pruning (`nodes_pruned /
   nodes_expanded` ratio increases).
5. (Optional) 2-step lookahead and pairwise compat — only if the trio
   above doesn't bring runtime under control.
