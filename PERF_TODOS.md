# Performance Optimization TODO List

## Procedure

For each item below:
1. **Baseline** — run the relevant local bench and record the result
2. **Implement** — make the change
3. **Measure** — run the same bench again and compare
4. **Adjust** — iterate if the change is partial or introduces regressions
5. **Recommend** — keep or abandon based on measured delta

All benchmarks run locally (`cargo bench`) unless the item depends on target architecture (e.g., CPU feature flags), in which case run on the prod box (DO AMD 4-core).

---

## TODO

- [x] **Fix shard stack branch-capacity retention**
  In `src/dfs_runner.rs` line ~301, `frontier_shards` clones the stack then calls `frame.branches.clear()` on ancestor frames. `clear()` retains capacity, so each of ~30k pending shards holds a large empty `Vec` buffer. Replace those branch `Vec`s with fresh empty `Vec`s, or construct stripped ancestor frames that never carry extra capacity. Expected impact: large RSS reduction (~15.8 GB live).
  - Bench: `baseline_bestfirst_prune_both` local DFS bench; also check RSS on prod box after deploy.

- [x] **Build with CPU features enabled**
  The remote binary does not appear to contain `popcnt` despite the AMD CPU supporting POPCNT/AVX2/BMI2. Update `start_fold.sh` to pass `-C target-cpu=native` (or a portable `x86-64-v3` feature set) so bitset `count_ones` uses hardware popcount. Benchmark and deploy on prod box only (arch-dependent).
  - Bench: interner bench and DFS bench on prod box before and after redeployment.
  - Result: Added `-C target-cpu=native` to RUSTFLAGS default in `start_fold.sh`. Clean A/B on idle AMD prod box: `interner_intersect_e_txt_large_vocab` (the large-vocab `intersect_into_count` hot path) improved from 186 ns → 107 ns (**−42%**). DFS `baseline_bestfirst_prune_both` unchanged within noise (59.5 ms vs 59.2 ms). Keep.

- [x] **Trim allocator memory after construction**
  Raw bitset storage for the interner is only ~376 MB despite 15 GB RSS, so most RSS is retained transient buffer capacity. Add a call to `malloc_trim(0)` after interner construction and after frontier creation (GNU/Linux only, gate behind `#[cfg(target_os = "linux")]`).
  - Bench: RSS on prod box; local DFS bench for throughput regression check.
  - Result: Added `libc::malloc_trim(0)` after `Interner::from_text` in `src/main.rs` and after `DfsRunner::frontier_shards` in `src/parallel_search.rs`, both gated on `#[cfg(target_os = "linux")]`. Local `baseline_bestfirst_prune_both` bench: 4.11M steps/5s before → 4.12M steps/5s after (no throughput regression). RSS on prod box (AMD 4-core): ~15 GB before → **~810 MB after** (VmRSS=829,512 kB, VmHWM=935,932 kB peak). Over 18x RSS reduction. Keep.

- [x] **Replace prefix `Vec<usize>` hash lookups with prefix IDs / trie nodes**
  `prefix_stats_with_appended` is 7.4% of CPU (`src/interner.rs` line ~379). A trie or prefix-ID scheme turns "prefix + appended token" into a child-pointer lookup instead of allocating/extending/hashing a slice key.
  - Bench: interner bench locally.
  - Result: Added interner-level prefix IDs plus `(parent_prefix_id, appended_token)` child stat lookups, and taught `CompletionContext` to cache required prefix IDs so completion bounds no longer allocate/extend/hash a `Vec` per required prefix. Microbench tradeoff: interner construction regressed (`interner_from_text` 214 µs → 235 µs, `interner_add_text` 88.3 µs → 106 µs) because the extra index is built eagerly, but the hot query path improved (`interner_intersect_simple` 126.8 ns → 112.8 ns, **-11%**). The behavior-scoped DFS bench `baseline_bestfirst_prune_both` improved from about **39.9 ms to 38.9 ms** (**-2.5%**, significant), with `completion_bound_ms` down to 6.05 ms on the probe run. Keep.

- [x] **Use hybrid sparse/dense completion storage**
  `intersect_into_count` is the top symbol at 22.2% (`src/interner.rs` line ~487). Total completion edges are ~454k across ~460k prefixes, so most prefixes have fanout ≤ 1. Store small fanouts as compact sorted `u16`/`u32` lists; keep `FixedBitSet` only for high-fanout prefixes.
  - Bench: interner bench locally.
  - Result: Added hybrid `CompletionSet` storage: fanout ≤ 64 uses sorted `Vec<u32>`, larger fanouts keep dense `FixedBitSet` plus cached count. Removed the duplicate `prefix_completion_counts` map and updated intersection/comparison helpers to consume the hybrid representation directly. Local `interner_intersect_e_txt_large_vocab` improved from **99.916 ns → 89.337 ns** (**−10.6%**). Focused comparison benches also stayed healthy after avoiding bitset rematerialization (`interner_impacted_keys` 43.970 µs, `interner_completions_equal_up_to_vocab` 33.044 ns, `interner_all_completions_equal_up_to_vocab` 87.046 ns). Keep.

- [x] **Reuse completion-bound work for child bounds**
  The DFS loop computes completion bounds (`src/dfs_runner.rs` line ~609) then recomputes existing child bounds (`src/dfs_runner.rs` line ~671). For normal in-fill children much of that prefix-stat work is redundant. Cache or thread results from the first pass into the second.
  - Bench: `baseline_bestfirst_prune_both` local DFS bench; target the `completion_bound + ctx_reset` timer (~37% of profiled step time).
  - Result: Reused the already-computed completion bound directly for normal in-fill child branches, while expansion children and runs without completion pruning still compute exact existing-child bounds. A stricter prefix-stat reuse attempt was correct but regressed and was abandoned. Final focused bench was statistically flat (`baseline_bestfirst_prune_both` 34.725 ms before → 35.028 ms after, no significant Criterion change; 3s budget 2.896M → 2.881M steps), but the duplicate existing-child-bound timer dropped 1.506 ms → 0.161 ms in the 20k-step probe. Keep as a neutral cleanup with no measured throughput regression.

- [x] **Specialize small top-k bound scoring**
  `upper_bound_score_with_scratch` is ~6.5% CPU. `dim_count <= 8` always. Replace the `Vec` insertion path with a fixed small-array top-k routine (stack-allocated `[u32; 8]` or similar).
  - Bench: `baseline_bestfirst_prune_both` local DFS bench.
  - Result: Added a `MAX_DIMS`-sized inline top-k path for the normal `dim_count <= 8` case, with the old `Vec` path retained as a defensive fallback for larger callers. Local `baseline_bestfirst_prune_both` was effectively flat by Criterion (`34.504 ms → 34.184 ms`, p=0.07), and 3s probe budget was flat (`2.987M → 2.975M` steps). The 20k-step profiled completion-bound timer dropped from `8.161 ms → 5.304 ms`, mostly by avoiding per-call scratch `Vec` allocation in the public upper-bound helper. Keep as a small cleanup with no measured throughput regression.
