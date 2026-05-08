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

Captured from the 2026-05-08 prod `e.txt` parallel search run on `142.93.70.195`.

- Run: `10:32:14`, phase `Parallel Search`
- Workers: `4/4`, queue `30.4k` pending, `4` running, `1.87k` done
- Expanded/pruned/completion-pruned: `234B` / `219B` / `2.81B`
- Rates: `6.44M n/s`, `6.01M p/s`, `431k accepted/s`, `117k cp/s`
- Depth: `32/35`, current bound `vol=8541936 full=11200000`
- Checkpoint: saved in `0.23s` at `1778238864`, age `00:00:23`
- Worker balance: min/avg/max `1.29M/1.63M/2.01M`
- Best age: `09:32:39` at depth `33`
- Incumbent: actual `vol=2 var=4/25 full=32`, effective prune `vol=8 full=27`, floor `vol=8`
- Best dims: `[2, 2, 2, 2, 3]`, capacity `48`

```text
[dim0=0, dim1=0, dim2=0]
captive       a       .
  among  people       .

[dim0=0, dim1=0, dim2=1]
    was    much       .
    all      of       .

[dim0=0, dim1=1, dim2=0]
     to  battle       .
    her      as       .

[dim0=0, dim1=1, dim2=1]
     as      in       .
   that      my       .

[dim0=1, dim1=0, dim2=0]
   from   heart       .
   them     and       .

[dim0=1, dim1=0, dim2=1]
     it       i       .
 before       a       .

[dim0=1, dim1=1, dim2=0]
    him    with       .
```

---

## TODO

- [x] **Verify prod build uses native CPU features**
  The 2026-05-08 perf run on `142.93.70.195` sampled the active `target/release/fold e.txt` process at ~7.3M n/s, but `objdump -d target/release/fold | grep -i popcnt` found no POPCNT instructions even though the DO Premium AMD CPU supports POPCNT/AVX2/BMI2. Confirm the deployed binary is built through `start_fold.sh` or another path that exports `RUSTFLAGS="-C force-frame-pointers=yes -C debuginfo=2 -C target-cpu=native"`. If not, fix the deploy/start path and re-check the binary before rerunning perf.
  - Bench: prod-box `interner_intersect_e_txt_large_vocab`, live `e.txt` throughput, and `perf stat` before/after rebuild.
  - Result 2026-05-08: added repo `.cargo/config.toml` so plain `cargo run --release` uses frame pointers and `target-cpu=native`; kept script defaults aligned. Rebuilt prod `target/release/fold` and verified POPCNT instructions are present.
  - Prod bench: non-native `158.22 ns` mean-ish midpoint (`[154.67, 158.22, 161.56]`), native `75.33 ns` (`[73.09, 75.33, 77.59]`).
  - Short live `e.txt` run: non-native `6.73M n/s`, native `7.30M n/s` at the same 67s snapshot window.
  - Live `perf stat` 45s: non-native `469.5B cycles / 1.150T instructions`; native `450.9B cycles / 1.045T instructions`.
  - Recommendation: keep.

- [x] **Use prefix IDs for completion intersection lookups**
  `Interner::intersect_into_count` was the top sampled symbol at ~21.3% CPU. Perf annotate showed much of this is hashing/probing `Vec<usize>` keys in `prefix_to_completions.get(prefix)`, including hash work and `bcmp`, not just raw bitset intersection. Add a prefix-ID completion table (for example `prefix_completions_by_id: Vec<CompletionSet>`) and thread `CompletionContext` required prefix IDs into a new `intersect_prefix_ids_into_count` path.
  - Bench: local `interner_intersect_e_txt_large_vocab`, local DFS probe/bench, then prod-box live throughput and flat perf symbol share.
  - Result 2026-05-08: added `prefix_completions_by_id` and `Interner::intersect_prefix_ids_into_count`; DFS now calls the ID path after `CompletionContext::ensure_prefix_ids`. The public prefix API keeps the direct map lookup to avoid adding an ID indirection for callers that only have `Vec` prefixes.
  - Local bench: public `interner_intersect_e_txt_large_vocab` `67.96 ns`; new `interner_intersect_e_txt_large_vocab_prefix_ids` `50.72 ns`. Local DFS `baseline_bestfirst_prune_both` was statistically flat (`39.70 ms`, p=0.63) with probe budget `4.61M` steps/5s.
  - Prod bench: public prefix path `75.72 ns`; prefix-ID path `55.56 ns`.
  - Prod short live `e.txt`: `7.55M n/s` at the 67s snapshot, up from `7.30M n/s` after the native-build fix.
  - Prod flat perf: `intersect_prefix_ids_into_count` `18.17%`; old `intersect_into_count` symbol no longer appears as the top path. `ensure_prefix_ids` is now visible at `3.45%`, which should compound with the compact requirement-data TODO.
  - Recommendation: keep.

- [x] **Replace parent/completion hash stats with indexed child stats**
  `completion_upper_bound_ctx` was ~7.6% CPU, and annotate showed hot `(parent_prefix_id, completion)` hash probes in `child_stats_by_id`. Replace `FxHashMap<(u32, u32), usize>` with parent-indexed child storage: sparse sorted child lists for low fanout and dense/table storage only where fanout justifies it. The goal is one cheap parent lookup plus a fast child lookup per required prefix.
  - Bench: local DFS probe timers for `completion_bound_ms`, `baseline_bestfirst_prune_both`, and prod-box perf share for `completion_upper_bound_ctx`.
  - Result 2026-05-08: replaced tuple-key `FxHashMap<(u32, u32), usize>` with `Vec<ChildStats>` indexed by parent prefix ID. Low-fanout parents use compact sorted child lists; high-fanout parents use direct vocab-indexed `u32` tables.
  - Local bench: `interner_prefix_stats_by_parent_id` about `2.06 ns`; `baseline_bestfirst_prune_both` improved to `35.19 ms` and `completion_bound_ms` dropped from `6.114 ms` after prefix IDs to `3.048 ms`.
  - Prod lookup bench: `3.72 ns`. Prod flat perf: `completion_upper_bound_ctx` dropped to `2.59%` plus `upper_bound_score_with_scratch` `3.23%`, down from roughly `5.98% + 2.88%` after the prefix-ID change.
  - Prod short live `e.txt`: noisy/negative in two 65-66s samples (`7.26M n/s`, then `5.98M n/s`) versus the prior prefix-ID sample `7.55M n/s`; do not claim a live throughput win from this item alone.
  - Recommendation: keep for the targeted CPU-share and local DFS-probe improvement, but re-evaluate after the requirement-materialization change because live throughput did not confirm the gain.

- [x] **Avoid eager full-payload hashing for every child Ortho ID**
  `Ortho::compute_id` was ~6.1% CPU. In-fill child creation currently hashes the full dims/payload slice for every generated child even though `id()` is mostly used for deterministic tie-breaking and progress metadata. Explore a lazy ID, cached incremental fingerprint, or specialized in-fill update that avoids rehashing the full payload on every `Ortho::in_fill_child_from_payload_raw` call.
  - Bench: local `ortho_add_*`, DFS probe `child_gen_ms`, `baseline_bestfirst_prune_both`, and prod-box perf share for `Ortho::compute_id`.
  - Result 2026-05-08: changed `Ortho` IDs to a deterministic content hash made from dims/up-axis/cap plus per-cell mixed hashes. Normal in-fill children now update `self.id` with the inserted cell hash instead of hashing the whole payload slice; expansion/remap/canonicalizing paths still compute from the full payload.
  - Local bench: tiny `ortho_add_*` benches regressed locally, but the DFS hot-path probe improved: `child_gen_ms` dropped from `4.214 ms` to `3.425 ms`, and probe budget rose to `4.71M` steps/5s. `baseline_bestfirst_prune_both` was statistically flat (`38.28 ms`, p=0.17).
  - Prod bench: `ortho_add_simple` `104 ns`, `ortho_add_multiple` `104 ns`, `ortho_add_shape_expansion` `85.5 ns`, all improved in Criterion's local prod history.
  - Prod short live `e.txt`: `7.32M n/s` at the 66s snapshot.
  - Prod flat perf: `Ortho::compute_id` no longer appears in the top flat symbols; remaining Ortho child cost is attributed to `Ortho::add_into` at `4.28%`.
  - Recommendation: keep.

- [x] **Materialize requirements directly into compact hot-path data**
  `Ortho::fill_from_meta_data` was ~4.3% CPU and still builds/clears nested `Vec<Vec<usize>>` requirement data. Replace the hot reset representation with compact fixed-capacity requirement buffers, or derive required prefix IDs directly while scanning payload positions. This should reduce reset cost and compound with the prefix-ID intersection and child-stat lookup changes.
  - Bench: local DFS probe `ctx_reset_ms`, `intersect_ms`, `completion_bound_ms`, `baseline_bestfirst_prune_both`, and prod-box live perf after the prefix-ID changes.
  - Result 2026-05-08: added a compact DFS reset path with flat requirement values plus `(start, len)` ranges. Hot DFS reset/intersection/bound paths read prefix slices from the compact buffers; the legacy `required_usize()` API remains for tests and non-hot callers.
  - Local DFS probe: `ctx_reset_ms` dropped from `7.624 ms` to `5.702 ms`; `intersect_ms` `3.852 ms` to `3.482 ms`; total probe `31.997 ms` to `29.246 ms`; probe budget `4.71M` to `6.22M` steps/5s. `baseline_bestfirst_prune_both` improved to `32.19 ms` (**-15.8%**, p=0.01).
  - Prod short live `e.txt`: `7.41M n/s` at the 66s snapshot.
  - Prod flat perf: old `Ortho::fill_from_meta_data` path is replaced by `fill_flat_from_meta_data` at `4.84%`; `reset_for_node_compact` is `3.16%`. `ensure_prefix_ids` remains visible at `4.95%`, so deriving prefix IDs during reset or adding trie-style prefix IDs is the next reset-side opportunity.
  - Recommendation: keep.
