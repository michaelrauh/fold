# Incumbent bootstrap via random / MCTS rollouts

## Problem

Branch-and-bound's runtime is exponential in the gap between the *incumbent*
score and the true optimum. Today the run starts with no incumbent (effective
score=2) while `frontier_max_bound` reaches 31.85M; pruning at frame creation
(`dfs_runner.rs:527`, `:553`) almost never fires because `optimistic_bound <=
incumbent_score` is rarely true. Most of the search tree is therefore visited.

The empirical impact: 2057 / 32,252 shards finished in 23.6h while the 4
currently-active shards have each consumed 60–160B nodes without completing.
Per-deep-shard cost extrapolates to ~230B nodes; with ~12,000 such shards
pending, the all-in estimate at current throughput is ~14 years.

## Idea

Decouple incumbent finding from tree search. Spend a small upfront budget on
*constructive* rollouts that build full orthos directly — picking one valid
completion at each step, descending to a leaf, scoring, repeating. The best
result becomes the seed incumbent for the BnB search.

Rollouts have no pruning, no stack management, no bound calculation. Each
rollout is `depth × (intersect + add_into)` ≈ tens of microseconds, so
millions of rollouts complete in seconds–minutes. The probability of stumbling
on an ortho with `volume ≫ 2` is high for any non-degenerate corpus.

## All primitives are already public

The constructive ortho-building chain is fully exposed:

| step | API | location |
|------|-----|----------|
| empty starting ortho | `Ortho::new()` | `src/ortho.rs:323` |
| valid completions for a given ortho | `CompletionContext::reset_for_node_compact(&ortho)` + `ensure_prefix_ids(interner)` | `src/completion_pruning.rs:146,348` |
| count + bitset of completions | `Interner::intersect_prefix_ids_into_count(req, forbidden, &mut bits)` | `src/interner.rs:1016` |
| children orthos given a chosen completion | `Ortho::add_into(value, &mut Vec<Ortho>)` | `src/ortho.rs:362` |
| score | `Ortho::score()` (returns `OrthoScore`) | `src/ortho.rs:250-ish` |
| inject incumbent into a runner | `DfsRunner::import_incumbent_if_better(&Ortho)` | `src/dfs_runner.rs:343` |

The DFS step at `dfs_runner.rs:511-540` is the same chain — just with bound
computation, branch storage, and stack management around it. Strip those out
and you have a rollout.

## Pseudocode for a new `src/bin/incumbent_finder.rs`

```rust
let interner = Interner::from_bytes(read("interner.bin"))?;
let mut ctx = CompletionContext::default();
let mut bits = CompletionBitset::with_capacity(interner.vocab_size());
let mut children = Vec::new();
let mut best: Option<Ortho> = None;
let mut rng = SmallRng::from_entropy();

for _ in 0..N_ROLLOUTS {
    let mut ortho = Ortho::new();
    loop {
        bits.clear();
        ctx.reset_for_node_compact(&ortho);
        ctx.ensure_prefix_ids(&interner);
        let k = interner.intersect_prefix_ids_into_count(
            ctx.required_prefix_ids(),
            ctx.forbidden_usize(),
            &mut bits,
        );
        if k == 0 { break; }                    // leaf
        let comp = sample_random_one_bit(&bits, &mut rng);
        children.clear();
        ortho.add_into(comp as PayloadVal, &mut children);
        if children.is_empty() { break; }       // no valid expansion
        ortho = children[rng.gen_range(0..children.len())].clone();
    }
    if best.as_ref().map_or(true, |b| ortho.score() > b.score()) {
        best = Some(ortho);
    }
}

write("incumbent.bin", best.unwrap().to_bytes()?);
```

Total: ~80 lines including arg parsing, error handling, and progress
reporting. No edits to the search machinery.

## Three plug-in points (pick by edit-tolerance)

### α — patch `state.bin`'s `best_incumbent` field

`ParallelCheckpointState.best_incumbent: Ortho` lives at
`parallel_search.rs:102`. A small "patch" binary loads state via `rkyv`,
overwrites the field, writes back. On restart the live workers pick up the
new incumbent at `parallel_search.rs:878`. Most pruning checks then succeed
immediately. Zero edits to the search code.

### β — `--initial-incumbent` flag in main.rs

About 10 lines: read a path, deserialize, pass into `run_parallel_search` as
an extra parameter. Cleaner than α; requires a small signature change to
`run_parallel_search` at `parallel_search.rs:834`.

### γ — independent of the bootstrap: raise `score_floor`

Currently hardcoded at `parallel_search.rs:1646`:

```rust
g.score_floor = OrthoScore::optimistic_bound(8, 27);
```

Promote to a CLI flag. The pruning compares against
`incumbent_score = max(actual_incumbent, score_floor)`
(`dfs_runner.rs:331-335`), so a synthetic floor prunes immediately even
without a real incumbent. Use this *together with* the bootstrap by setting
`floor = max(synthetic, real_incumbent.score())`.

## Going beyond uniform random — MCTS

Replace the uniform `sample_random_one_bit` with a UCB-biased sampler. Use
`Interner::completion_count_for_prefix` (`interner.rs:717`) as a prior:
completions whose prefix has more downstream phrases get higher visit
priority. Track per-prefix visit/value statistics across rollouts; bias new
rollouts toward high-value paths. Same loop structure, different sampler.

For corpora with even mild structure this should find incumbents
1–3 orders of magnitude better than uniform random in the same wall time.

## Caveats from the code

- **Canonicalization.** The DFS enforces `min_insert_axis`
  (`dfs_runner.rs:589`) and the up-axis swap rule
  (`ortho.rs:381-384`) for symmetry breaking. Random rollouts will sometimes
  produce non-canonical orthos. Score is invariant under canonicalization, so
  this doesn't hurt incumbent quality, but if you compare orthos by `id`,
  canonicalize first.
- **`expanding_insert_axis` constraint.** At `ortho.rs:348`, only certain
  axis insertions are valid when expanding up. If `add_into` returns 0
  children for a chosen completion, skip and try another. (Don't treat it
  as a leaf.)
- **Bound vs actual.** Workers compare against `incumbent_score()` which
  uses the *actual* score of the injected ortho. A constructed ortho with
  `vol=10⁵` immediately makes every bound ≤ 10⁵ prune.

## Expected impact

If a one-hour rollout produces an incumbent with `volume ≈ 10⁵` (modest
given `frontier_max = 31.85M`), every node in the pending pool with
`top_bound_volume ≤ 10⁵` prunes on first visit. Inspecting the 2B-sample
data: **all 90 sampled pending shards had starting `top_bound_volume`
between 288 and 2,304**. Every one of them would prune instantly under a
10⁵ floor.

The remaining work would be the small fraction of shards whose true
descendant volume actually exceeds the incumbent — currently unknown, but
empirically a tiny minority of the 30k pending pool.

## Stacking with other ideas

- **Best-first ordering** (always-on improvement): finds high-volume
  incumbents during normal search faster. The bootstrap gives a head start;
  best-first sustains improvement.
- **Bound tightening** (separate design): makes existing bounds tighter, so
  the incumbent's pruning power is amplified.
- **Dutch auction** (separate design): synthetic floor as a backstop if
  rollouts can't find a strong-enough incumbent.

## Order-of-magnitude estimate

- Probability of an arbitrary rollout producing `vol ≥ X` is some
  decreasing function `P(X)`. For natural-language corpora, structures with
  modest volume (say `vol ≥ 100`) are abundant.
- A million rollouts in ~10 minutes of single-threaded work is realistic.
- The maximum across a million IID samples is well within the upper tail.
- Plausible incumbent: `vol = 10³ – 10⁵`. Compared to current `vol = 2`,
  pruning power increases by 3–5 orders of magnitude.

## Risk register

| risk | mitigation |
|------|------------|
| Random rollouts are too biased toward shallow orthos | Switch to MCTS with prior; compare to a beam-search rollout variant |
| Rollouts hit invalid expansions frequently | Use `expanding_insert_axis` to validate before calling `add_into` |
| Constructed incumbent is "bad" canonical-form variant | Canonicalize before injection (call `add_into` from a known-canonical ancestor) |
| Patch path corrupts `state.bin` | Test on a copy; verify by re-deserializing before overwriting |
| Workers don't pick up new incumbent | Fall back to β (CLI flag); workers use the path that's been tested |

## Implementation order

1. Build `incumbent_finder` binary using public APIs only. Verify it produces
   valid (canonicalizable) orthos with non-trivial volumes against the live
   interner. (1 day)
2. Add `--initial-incumbent` to `main.rs` (option β). 10 lines, low risk.
   (1 hour)
3. Run the live system from a clean state with `--initial-incumbent
   <output>` and `--floor-volume X` where X is calibrated from rollout
   results. Observe pruning rate; expect orders-of-magnitude improvement.
4. If rollouts are good but workers still spend time on heavy shards,
   layer in MCTS biasing. (1–2 days)
