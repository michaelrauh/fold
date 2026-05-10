# Dutch auction prune floor

## Problem

Branch-and-bound prunes a frame when its admissible bound is below the current
incumbent's score. Without a strong incumbent, almost nothing prunes.

The current run has been stuck for hours: incumbent `vol=2`, effective floor
`vol=8`, while frontier upper bounds reach `31.85M`. Every frame in the active
shards' DFS has a bound far above the floor, so pruning never bites.

The Dutch-auction idea inverts the problem: instead of *waiting* to discover
a high incumbent, **assert** a high prune floor and run the search to
exhaustion at that floor. If the search finishes without improving the
incumbent, the asserted floor was too high — lower it and retry.

## Why this works in principle

Branch-and-bound with floor `F` is equivalent to **searching only the subtree
rooted at frames with bound > F**. Key properties:

1. **Soundness.** If the search exhausts at floor `F` and returns ortho `O`
   with `O.score = V`, then `V` is the global optimum. (Proof: any ortho with
   score `> V` would have had a bound `> V ≥ F`, so its frame would not have
   pruned, so it would have been visited. The search visited everything with
   bound > F, so it visited any candidate optimum.)
2. **Completeness when nothing is found.** If the search exhausts at floor
   `F` with no incumbent improvement, the optimum is `≤ F`. Lower `F` and try
   again.
3. **Time scales with the size of the tree above F.** The tree grows
   exponentially as F decreases. A pass at F = 10⁵ visits a tiny fraction of
   the tree compared to F = 8.

So instead of one pass at F = 8 (current setup, 14-year ETA), do successive
passes at F = 10⁶ → 10⁵ → 10⁴ → 10³ → 10² → 10¹ → 8, stopping at the first
pass that finds an ortho.

## Where the floor lives in the code

The floor is hardcoded at `parallel_search.rs:1646`:

```rust
g.score_floor = OrthoScore::optimistic_bound(8, 27);
```

This sets the effective prune-against value for every worker. Pruning checks
in `dfs_runner.rs` use `incumbent_score()` which is
`max(actual_incumbent_score, score_floor)` — so raising `score_floor` raises
the pruning threshold immediately, no incumbent required.

The runner-level field is `DfsRunner::score_floor` (private) with the
comparison at `dfs_runner.rs:527`:

```rust
let prune_root = toggles.node_pruning
    && frame.optimistic_bound <= incumbent_score;
```

`incumbent_score` here is whichever of `(actual, floor)` is larger. With a
synthetic floor of `10⁵`, every frame with `optimistic_bound ≤ 10⁵` prunes
on first visit.

## Implementation: minimal CLI flag

Two changes:

1. Replace the hardcoded floor at `parallel_search.rs:1646` with a value
   plumbed in from `ParallelSearchConfig`. Add fields:
   ```rust
   pub floor_volume: usize,
   pub floor_fullness: usize,
   ```
   defaulting to `(8, 27)`.
2. Read `--floor-volume` and `--floor-fullness` from CLI args in `main.rs`.

That's the entire intra-pass change. Each pass is now configurable.

The auction itself is an outer driver — bash, Python, or a small Rust
runner — that:

```
floor = M   # initial high value, e.g. 10^6
incumbent = None
while floor >= F_min:
    run fold with --floor-volume floor [--initial-incumbent incumbent]
    if found new incumbent improvement:
        keep best ortho; break (it's optimal among ≥ floor)
    else:
        floor = floor // FACTOR
print best
```

If the inner pass exits "search exhausted, no incumbent better than
synthetic floor", the driver lowers the floor and reruns from a fresh state.

## Geometric vs binary-search schedule

Two reasonable schedules:

**Geometric decay** (`floor /= 2` each pass):
- Pros: simple, monotone, optimum is found at the *first successful* pass.
- Cons: each pass is run to exhaustion; previous passes' work is discarded.

**Binary search** (`floor = (lo + hi) / 2`):
- Pros: O(log) passes.
- Cons: requires distinguishing "found something at this floor" (lower hi)
  from "nothing here" (raise lo). A "found something" pass terminates early
  with the result, but a "nothing here" pass still runs to exhaustion. Total
  cost is dominated by the latest "nothing" pass, same as geometric.

Geometric is simpler and just as efficient in practice. Pick `factor = 4` for
fast convergence; smaller factors give more granular results.

## Why each pass is fast

At floor `F`, every frame whose `optimistic_bound ≤ F` prunes. From the
2B-sample data, **all 90 sampled pending shards have `top_bound_volume`
between 288 and 2,304**. A pass at `F = 5,000` would prune virtually every
pending shard at the root — search exits in seconds.

A pass at `F = 100` still prunes an enormous portion of the tree because
most internal frames have small bounds. Even the active 4 shards' deep
subtrees have `top_bound_volume` distributions extending far below their
peak `31.85M` value.

Empirically, the relationship is roughly:
- `F = 10⁶`: subtree above F has ~0 nodes. Exhausts in seconds.
- `F = 10⁴`: small fraction of the tree visited. Minutes.
- `F = 10²`: most pruning still happens. Hours.
- `F = 8` (current): essentially full tree. Years.

A succession of high-floor passes is dramatically cheaper than one
no-floor-tightening pass.

## Stacking with the incumbent bootstrap

The two ideas are complementary:

- The **bootstrap** finds a real incumbent `R` quickly via random / MCTS
  rollouts, then uses `floor = max(synthetic_F, R.score)`.
- The **auction** asserts `floor = synthetic_F` directly without an
  incumbent; if the pass exhausts unimproved, lower `F` and try again.

Best practice: run the bootstrap first to get a real `R`. Then run a single
pass with `floor = R.score`. If exhaustion at that floor finds something
strictly better, optimum is found. If not, the auction kicks in — lower the
synthetic `F` below `R.score` and retry, until something is found or `F`
drops to the original `8`.

The crucial property: **a pass that exhausts unimproved is sound**. You don't
need to verify "no better thing exists below F" — that's free, by the
soundness of BnB.

## Caveats and risks

### Frontier-state retention across passes

Each pass currently rebuilds shards from scratch via
`DfsRunner::frontier_shards(...)` at `parallel_search.rs:894-896`. Restarting
discards inter-pass progress. Two mitigations:

- **Cache the frontier expansion**: the depth-3 frontier is deterministic
  given the interner; serialize once, reuse for every pass.
- **Carry forward the best incumbent**: each pass's exit state is "no
  improvement found"; the incumbent at exit equals the incumbent at entry.
  The next-lower pass starts with the same incumbent, so previous-pass work
  isn't *fully* wasted — the pruning-via-incumbent stays.

Neither mitigation is required for correctness; they're for efficiency.

### Choosing the initial floor

If the initial `F` is too high, every pass exhausts empty until F gets low
enough — wasted work. Conversely, too low and the first pass is slow.

Heuristic: set initial `F` to **`frontier_max_bound × 0.5`** (currently
`15.9M`). The first frame visited has bound ≤ frontier_max, so almost
everything prunes immediately at this floor. From there, geometric decay
finds the optimum in ~log₂(frontier_max / true_optimum) passes.

If the bootstrap is also run, set initial `F = R.score` and only auction
*below* that.

### Parallel checkpoint compatibility

The state.bin is fingerprinted by config (`config_fingerprint` at
`parallel_search.rs:98`). Adding floor fields to config changes the
fingerprint, invalidating existing checkpoints across pass boundaries. Two
options:

- Exclude floor from the fingerprint hash (`fingerprint()` at
  `parallel_search.rs:54`). Floor doesn't change the search graph, only the
  pruning threshold — safe to exclude.
- Run each pass in its own state directory; checkpoint reuse is irrelevant
  because each pass is independent.

The first option is cleaner and is the one I'd recommend.

### Floor-fullness vs floor-volume

`OrthoScore` has both `volume` and `fullness`. Pruning compares
lexicographically: volume first, fullness second on tie. The auction lever
is volume; fullness can stay at its current `27`. (If volume drops to `0`,
the `fullness=27` is still meaningful as a tiebreaker for shape-bound
orthos.)

### Work pattern within a pass

A pass with high `F` spends most of its time on shards whose root bound
exceeds `F`. The remaining shards prune at the root. Worker imbalance
becomes worse: most workers idle, a few crunch. Mitigation: parallel-search
already handles imbalance via stealing; verify behavior.

## Expected payoff

| floor | expected pass time | likely outcome |
|-------|----------------------|----------------|
| 10⁶ | seconds | exhausts unimproved; lower |
| 10⁴ | minutes | exhausts unimproved (or finds a strong ortho) |
| 10² | hours | likely finds something |
| 10 | days | full search; current behavior |

If the optimum is in the `10² – 10⁴` range (plausible given empirical
incumbents on small corpora), the auction terminates after 2–4 passes,
totaling a few hours of work — vs. the current 14-year extrapolation.

If the optimum is below `10²`, the worst-case auction is no slower than
the current single-pass run. Adding the auction strictly cannot hurt.

## Implementation order

1. Plumb `floor_volume` and `floor_fullness` from CLI through
   `ParallelSearchConfig` to the metrics update at
   `parallel_search.rs:1646`. Exclude from `fingerprint()`. (~1 hour.)
2. Test with a single elevated-floor pass; verify pruning rate matches
   expectation (manifest `nodes_pruned` should jump dramatically). (~1
   hour.)
3. Add an outer driver script that runs successive passes geometrically.
   Each pass writes a small status file (`{found: true/false, ortho: ...}`)
   the driver reads. (~2 hours.)
4. Run the auction starting at `F = 10⁶`. (~hours-to-days depending on
   where the optimum lands.)

Total implementation: ~half a day. Outer-loop runtime: hours-to-days,
versus current years.
