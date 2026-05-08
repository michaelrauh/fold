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

## Incumbent

Captured 2026-05-08 16:33 local from prod `e.txt` parallel search run on `142.93.70.195` (`tmux capture-pane -t 0 -p` on the droplet).

- Phase: `Parallel Search`, run time `02:58:50`
- Workers: `4/4`, queue `30.4k` pending, `4` running, `1.87k` done
- Expanded/pruned/completion-pruned: `65.6B` / `62.0B` / `565M`
- Rates: `7.04M n/s`, `6.63M p/s`, `403k accepted/s`, `62.8k cp/s`
- Worker balance min/avg/max: `1.83M/2.01M/2.14M`
- Per-worker (shard / depth / rate / vol): W0 `s1865 d32 1.97M/s vol=3.23M`; W1 `s1870 d21 2.14M/s vol=16.2M`; W2 `s1866 d32 1.83M/s vol=5.22M`; W3 `s1867 d21 2.12M/s vol=8.96M`
- Depth: `32/35`, current bound `vol=16220160 full=19549728`, frontier max `vol=16220160`
- Checkpoint: saved in `0.27s` at `1778258009`, age `00:00:27`
- Best age: `02:01:53` at depth `33`
- Incumbent: actual `vol=2 var=4/25 full=32`, effective prune `vol=8 full=27`, floor `vol=8`
- Best dims: `[2, 2, 2, 2, 3]`, capacity `48`
- Vocab: `9823` words; corpus `e.txt` `67200` words (corpus + vocab are static; no need to re-derive on each refresh)

### How to refresh this section

Run on the operator box (one-shot, no edit needed unless the corpus/vocab changes):

```bash
ssh root@142.93.70.195 'tmux capture-pane -t 0 -p | head -40'
```

Update each bullet above from the captured fields (`Phase`, run-time `[Time: ...]`, `Workers`, `Expanded`/`Pruned`/`CPruned`, `Rates`, `Balance`, per-worker `W0..W3` line, `Depth`, `Current bound`, `Frontier max`, `Checkpoint`, `Best age`, `Best actual`, `Effective prune`, `Floor`, `Best dims`). Bump the timestamp at the top of the section. The incumbent payload table below is a separate snapshot of the best ortho's cells — only re-paste it if `Best age` resets (i.e., a new best was found); otherwise it's still valid. Refresh whenever the rates or expanded/pruned counts in this doc are more than ~30 minutes behind reality, or before starting a new TODO so its baseline is honest.

---

## Latest perf hotspots (2026-05-08, 30s record, 11.7k samples)

| % self | Function | Notes |
|---|---|---|
| 26.0 | `dfs_runner::step_with_toggles_profiled` | Main DFS step body |
| 17.4 | `interner::intersect_prefix_ids_into_count` | Bitset AND + popcount (already AVX2-vectorized inner) |
| 5.5 | `completion_pruning::ensure_prefix_ids` | Per-frame prefix→id rebuild |
| 5.0 | `ortho::fill_flat_from_meta_data` | Per-frame requirement rebuild |
| 3.9 | `interner::intersect_two_bitsets_into_count` | AVX2 vpand + Mula popcount confirmed in disassembly |
| 3.9 | `ortho::add_into` | Constructs child Ortho |
| ~9 | `libc.so.6` (memcpy/memset) | Bitset clear/copy + Ortho copy |
| 3.5 | `completion_pruning::upper_bound_score_with_scratch` | Inline path (dim ≤ 8) |
| 3.2 | `completion_pruning::completion_upper_bound_ctx` | Per-completion bound |
| 3.0 | `completion_pruning::reset_for_node_compact` | Per-frame reset |

Confirmed in disassembly: the bitset intersect inner loop is fully AVX2-vectorized with 4-way unrolled `vpand` + Mula popcount (`vpshufb` nibble lookup + `vpsadbw` byte-sum + `vpaddq`). EPYC Rome/Milan lacks AVX-512 VPOPCNTDQ, so this is the fastest popcount available on this hardware.

---

## TODO

- [ ] **Replace `BranchOrdering` machinery with pure LIFO**
  Production currently uses `BranchOrdering::WorstFirst` (`src/parallel_search.rs:69`), which incurs an O(n log n) `sort_by` over `frame.branches` plus the unconditional `frame.branches.reverse()` at `src/dfs_runner.rs:733` to fix up `pop_back` order. The other variants (`BestFirst`, `Insertion`) are not used in production and add code complexity without buying anything: BestFirst yields a wider working set with no measured search-quality win, and FIFO-style ordering blows up memory. Pure LIFO via `push` then `pop_back` is the right semantic for DFS — best locality, smallest frontier RAM. Remove the `BranchOrdering` enum, the `branch_ordering` field in `SearchToggles`, the entire `match toggles.branch_ordering { ... }` block at `src/dfs_runner.rs:703-731`, the unconditional `frame.branches.reverse()` at line 733, the bench/test references in `benches/dfs_ab_bench.rs` and `src/parallel_search.rs:1204+`, and the test-only inits at `src/dfs_runner.rs:846,880,904`. Keep `compute_bounds` and the per-branch `optimistic_bound` — bounds are still consulted for pruning at `src/dfs_runner.rs:751`, just not for sorting. Net: drops one sort + one memmove per prepared frame and removes a config knob the production path doesn't actually want.
  - Bench: prod-box live `e.txt` throughput, prod flat perf share for `step_with_toggles*` and `core::slice::sort` symbols, and `baseline_bestfirst_prune_both` (rename to `baseline_lifo_prune_both` if kept).
  - Watch: search ordering changes — incumbent discovery time may shift even though total expanded count is the same shape.

- [ ] **Enable LTO and `codegen-units=1` in `Cargo.toml`**
  No `[profile.release]` section currently exists; the build is on cargo defaults (`lto = false`, `codegen-units = 16`). Add a release profile that enables fat (or thin) LTO, sets `codegen-units = 1`, and switches to `panic = "abort"`. Typically buys 5–15% on tight numerical Rust code. The hot `step_with_toggles_profiled` (26% self) is not currently inlined into `worker_loop`; LTO is what would let LLVM inline it, fold the bitset helpers, and eliminate dead branches. Free win, no source change required.
  - Bench: prod-box `interner_intersect_e_txt_large_vocab_prefix_ids`, local `baseline_bestfirst_prune_both`, prod-box live `e.txt` throughput, and `perf stat` cycles/instructions before vs after.
  - Verify the profile applies through `start_fold.sh`'s `RUSTFLAGS` path and any `.cargo/config.toml` overrides.

- [ ] **Remove profiling instrumentation from the main DFS step**
  `step_with_toggles_profiled` at `src/dfs_runner.rs:490` carries ~18 `profile_start!()` / `profile_end!()` macro sites, each compiling to a runtime check on the `profiling` bool plus a conditional `Instant::now()`. Even when profile is `None`, the branches sit in the I-cache footprint of the 26%-self hot function. Replace with a clean `step_with_toggles` containing no `Option<&mut SearchProfile>` parameter, no `profile_start!`/`profile_end!` macros, no `profiling` flag, and no `step_with_toggles_and_profile` shim — just the search logic. When profiling is needed during a future optimization session, reintroduce the instrumentation locally for that change and remove it after the result is recorded. Profiling should never live in the production trunk.
  - Bench: local DFS probe `total_step_ns` is no longer meaningful after this change; switch to `baseline_bestfirst_prune_both` and prod-box live `e.txt` throughput. Compare before/after `perf report --no-children` to confirm `step_with_toggles_*` self-time drops.

- [ ] **Cache requirement metadata across parent→child frames**
  `reset_for_node_compact` + `ensure_prefix_ids` + `fill_flat_from_meta_data` together consume ~14% self time. They rebuild the flat requirement ranges/values and the prefix-id list from scratch every step, but a child frame differs from its parent by exactly one filled cell — sibling frames at the same parent often produce identical or near-identical requirement sets. Add a fast-path early-out: if `(dims, up_axis, payload-cells-touched-by-required-prefixes)` matches the previous reset, skip the rebuild entirely. As a more involved variant, version-stamp the requirement buffers and only rerun `ensure_prefix_ids` when the version changes.
  - Bench: local DFS probe `ctx_reset_ms`, `intersect_ms`, `completion_bound_ms`, `baseline_bestfirst_prune_both`, prod flat perf share for `reset_for_node_compact` / `ensure_prefix_ids` / `fill_flat_from_meta_data`.

- [ ] **Shrink `OrthoScore` from 48→16 bytes**
  `OrthoScore` at `src/ortho.rs:25` uses two `u128` fields (`variance_num`, `variance_den`). With `MAX_DIMS = 8` and `Dim = u8`, `variance_num = dim_count·Σdᵢ² − (Σdᵢ)²` is bounded above by `8·8·255²` ≈ 4.16M, well within `u32`; `variance_den = dim_count²` ≤ 64. The `u128` saturating arithmetic in `variance_cmp` then becomes a native `u64` multiply. `Ortho` is currently ~330 bytes; saving 32 bytes per Ortho makes child push, `frame.branches` Vec moves, and the worker scratch all cheaper, attacking part of the ~9% libc memcpy share. Verify there is no overflow path before switching (audit `compute_score_components` and any external callers of `OrthoScore`).
  - Bench: local `ortho_add_*`, `ortho_score`, DFS probe `child_gen_ms`, `baseline_bestfirst_prune_both`, prod flat perf share for `Ortho::add_into` and the libc memcpy/memset symbols.

- [ ] **Prefetch in the bitset intersect inner loop**
  At vocab `9823`, each bitset is 154 `u64` words ≈ 1.2 KB; the `seed × first × out` working set is ~3.6 KB and fits in L1, but L1 fills are still visible at the auto-vectorized loop in `intersect_two_bitsets_into_count` (`src/interner.rs:738`) and in `intersect_bitsets_into_count` (`src/interner.rs:721`). Add `core::arch::x86_64::_mm_prefetch` (or stable `core::intrinsics::prefetch_read_data` if accepting nightly) hints 8–16 `u64` words ahead of the current iteration index for both `left_words` and `right_words`. Keep the change behind a target-feature gate so non-x86_64 builds compile.
  - Bench: prod-box `interner_intersect_e_txt_large_vocab_prefix_ids`, prod live `e.txt` throughput, prod flat perf share for `intersect_two_bitsets_into_count` and `intersect_prefix_ids_into_count`.

- [ ] **Drop `force-frame-pointers=yes` from the production build path**
  `start_fold.sh:33` defaults `RUSTFLAGS` to `-C force-frame-pointers=yes -C debuginfo=2 -C target-cpu=native`. Frame pointers cost ~1–2% globally on this workload. They are only required for `perf record -g fp`; `perf record -g dwarf` works without them as long as `debuginfo=2` stays. Make frame-pointer enabling an opt-in env flag (`FOLD_PROFILE=1` → adds `-C force-frame-pointers=yes`) so production runs default to the faster build, while a profiling session sets the flag. Mirror the change in `.cargo/config.toml` if it duplicates the flag.
  - Bench: prod-box live `e.txt` throughput before vs after, `perf stat` cycles/instructions, and verify `perf record -g dwarf` still produces a usable call graph for the next perf pass.

- [ ] **Inline `saturating_pow_usize` for small `dim_count`**
  `dfs_runner.rs:590-592` calls `saturating_pow_usize` twice per step (`k_vol`, `k_full`), and `dim_count ≤ MAX_DIMS = 8`. The generic pow-by-squaring path includes branches and a loop that the compiler can flatten only with strong inline hints. Replace with an inline unrolled implementation specialized for `dim_count ≤ 8`, or mark the helper `#[inline(always)]` and verify the small-loop path actually inlines into `step_with_toggles`. Modest but free win once the macro instrumentation in item 2 is gone and the function inlines under LTO.
  - Bench: local `baseline_bestfirst_prune_both`, prod flat perf share for `step_with_toggles*` and any pow helpers that surface.

---

## Notes

- The bitset intersect inner loop is already optimal for this hardware — do not attempt further hand-vectorization. AVX-512 VPOPCNTDQ would help but is unavailable on EPYC Rome/Milan. Re-evaluate only if the deployment moves to Intel Ice Lake / Sapphire Rapids or AMD Zen 4+.
