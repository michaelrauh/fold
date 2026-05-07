# Performance Results — Item-by-Item

All measurements on Apple Silicon dev machine.
Deploy target: x86-64-v3 (AVX2/BMI2/POPCNT).
Baseline is in PERF_BASELINE.md; each item records the delta.

---

## Item 1 — Inline payload + dims in Ortho

**Change:** Replaced `Vec<Dim>` and `Vec<Option<PayloadVal>>` with
`[u8; MAX_DIMS]` + `dims_len: u8` and `[u32; MAX_PAYLOAD]` + `payload_cap: u8`.
Sentinel `EMPTY_CELL = u32::MAX` replaces `None`.
Checkpoint version bumped 4 → 5.

### DFS bench (`dfs_ab_bench`)

| Benchmark | Baseline | Item 1 | Δ |
|---|---|---|---|
| baseline_bestfirst_prune_both | 70.853 ms | 63.741 ms | **−10.0%** |
| branch_insertion_prune_both | 66.591 ms | 63.904 ms | −4.0% |
| branch_worstfirst_prune_both | 55.666 ms | 54.379 ms | −2.3% |
| bestfirst_no_completion_prune | 54.386 ms | 49.523 ms | **−8.9%** |
| bestfirst_no_node_prune | 55.862 ms | 50.694 ms | −9.2% |
| bestfirst_no_pruning | 44.223 ms | 38.654 ms | **−12.6%** |
| no_bounds_no_pruning | 39.216 ms | 33.534 ms | **−14.5%** |

Primary production config (**baseline_bestfirst_prune_both**):
70.853 ms → 63.741 ms, **−10.0%**

### Ortho bench (`ortho_bench`)

| Benchmark | Baseline | Item 1 | Δ |
|---|---|---|---|
| ortho_new | 100.64 ns | 8.789 ns | **−91.3%** |
| ortho_add_simple | 180.56 ns | 55.013 ns | **−69.5%** |
| ortho_add_multiple | 172.66 ns | 55.275 ns | **−68.0%** |
| ortho_id | 211.77 ps | 209.27 ps | ~flat |
| ortho_add_shape_expansion | 182.50 ns | 54.669 ns | **−70.0%** |
| ortho_get_requirements | 279.92 ns | 271.58 ns | −3.0% |
| ortho_get_requirement_phrases | 278.65 ns | 271.61 ns | −2.5% |
| ortho_remap | 106.05 ns | 55.687 ns | **−47.5%** |
| ortho_prefixes | 893.96 ns | 897.45 ns | ~flat |
| ortho_prefixes_for_last_filled | 188.65 ns | 195.44 ns | +3.6% |
| ortho_dims | 209.61 ps | 869.34 ps | +4.1x (now returns slice len) |
| ortho_payload | 209.64 ps | 850.71 ps | +4.1x (now returns slice ref) |
| ortho_display | 640.82 ps | 703.55 ps | ~flat |

`ortho_new`, `ortho_add_*`, and `ortho_remap` all drop sharply — no heap allocation in construction/copy path.

---

## Item 2 — Array-keyed single-token prefix_stats

**Change:** Added `single_token_stats: Vec<usize>` to `Interner` (indexed by token id, `0` = missing).
`prefix_stats(&[i])` now does a direct `Vec` index instead of a `FxHashMap` lookup.
Multi-token prefixes still fall through to the map.

### DFS bench (`dfs_ab_bench`)

| Benchmark | Item 1 | Item 2 | Δ |
|---|---|---|---|
| baseline_bestfirst_prune_both | 63.741 ms | 60.693 ms | **−4.8%** |
| branch_insertion_prune_both | 63.904 ms | 61.026 ms | −4.5% |
| branch_worstfirst_prune_both | 54.379 ms | 52.777 ms | −2.9% |
| bestfirst_no_completion_prune | 49.523 ms | 47.062 ms | **−5.0%** |
| bestfirst_no_node_prune | 50.694 ms | 50.892 ms | ~flat |
| bestfirst_no_pruning | 38.654 ms | 36.366 ms | **−5.9%** |
| no_bounds_no_pruning | 33.534 ms | 33.308 ms | ~flat |

Primary production config: 63.741 ms → 60.693 ms, **−4.8%**
Cumulative vs baseline (70.853 ms): **−14.3%**

---

## Item 3 — 1-token shortcut in prefix_stats_with_appended

**Change:** When prefix is a single token `[a]`, bypass scratch-Vec writes and call
`self.prefix_stats.get([a, appended])` directly via a stack-allocated slice. Avoids 3 Vec
operations (clear + extend + push) in the common case.

Note: attempted also adding `two_token_stats: FxHashMap<(usize, usize), usize>` for
2-token dispatch, but that added memory pressure with no net gain and was reverted.

### DFS bench — approximately neutral (±bench noise ~2 ms)

Primary config: 60.693 ms → ~62 ms (within noise). Benefit most visible in
`bestfirst_no_completion_prune`: 47.1 ms → 45.3 ms (−3.7%).

---

## Item 4 — Sparse bitset intersect fallback

**Change:** Added `SPARSE_THRESHOLD = 20` in `intersect_into_count_baseline`. When the seed
bitset has ≤ 20 set bits, iterate seed bits and check each in other required bitsets instead
of cloning 880 bytes + doing a full 110-word AND sweep. Avoids 880-byte memcpy + 110 AND+popcount
iterations for the common sparse-completion case at deeper DFS levels.

### DFS bench (`dfs_ab_bench`)

| Benchmark | Item 2+3 | Item 4 | Δ |
|---|---|---|---|
| baseline_bestfirst_prune_both | ~62 ms | 57.629 ms | **−7.1%** |
| branch_insertion_prune_both | ~63 ms | 58.331 ms | −7.4% |
| branch_worstfirst_prune_both | ~51 ms | 50.105 ms | −1.9% |
| bestfirst_no_completion_prune | ~45 ms | 44.521 ms | −1.1% |
| bestfirst_no_node_prune | ~49 ms | 47.633 ms | −2.8% |
| bestfirst_no_pruning | ~37 ms | 36.326 ms | ~flat |
| no_bounds_no_pruning | ~33 ms | 32.173 ms | −2.2% |

Primary production config: ~62 ms → 57.6 ms, **−7.1%**
Cumulative vs baseline (70.853 ms): **−18.7%**

---

## Items 5a+5b — Eliminate redundant Vec copies in CompletionContext reset

**Change A — Remove intermediate position buffers (`prefix_positions`, `diagonal_positions`):**
Added `spatial::with_requirements` callback that gives direct access to cached position data
without copying. `Ortho::fill_requirements_usize` now uses this callback, eliminating
one full Vec-of-Vec copy per `reset_for_node` call.

**Change B — Remove `prefix_with_completion` copy:**
`completion_upper_bound_ctx` previously iterated a pre-copied `prefix_with_completion` Vec
(an exact duplicate of `required_usize`, reserved with +1 capacity). Removed this copy entirely;
`completion_upper_bound_ctx` now calls `interner.prefix_stats_with_appended` with `required_usize`
directly. `CompletionContext` lost 2 + 1 = 3 Vec fields, shrinking by ~72 bytes.

Previously `ctx_reset` accounted for 33% of primary config (18 ms / 54 ms probe time).
After: 7 ms (13% of probe time).

### DFS bench (`dfs_ab_bench`)

| Benchmark | Item 4 | Items 5a+5b | Δ |
|---|---|---|---|
| baseline_bestfirst_prune_both | 57.629 ms | 40.584 ms | **−29.5%** |
| branch_insertion_prune_both | 58.331 ms | ~41 ms | ~−29% |
| branch_worstfirst_prune_both | 50.105 ms | ~39 ms | ~−22% |
| bestfirst_no_completion_prune | 44.521 ms | ~35 ms | ~−21% |
| bestfirst_no_node_prune | 47.633 ms | ~40 ms | ~−16% |
| bestfirst_no_pruning | 36.326 ms | ~30 ms | ~−17% |
| no_bounds_no_pruning | 32.173 ms | 27.993 ms | **−13.0%** |

Primary production config: 57.6 ms → 40.6 ms, **−29.5%**
Cumulative vs baseline (70.853 ms): **−42.7%**

Note: `no_bounds_no_pruning` floor also improved (32.2 → 28.0 ms) because
`fill_requirements_usize` is called even without bounds for the intersection step.

---

## Item 5c — DimKey size reduction (DIM_KEY_CAP: 64 → 8)

**Change:** Reduced `DIM_KEY_CAP` from 64 to 8 to match `MAX_DIMS`. The `DimKey` struct shrinks
from ~66 bytes to ~11 bytes. The FxHasher iteration count for key hashing drops from 9 to 2.
However, since the thread-local meta cache has near-100% hit rate (only misses on first call
per unique (dims, up_axis) shape), the hash computation is rarely on the hot path.

**Result:** Within measurement noise (~flat). Kept as a structural correctness improvement
(key matches actual data size).

---

## Item 5d — DimMeta caching in CompletionContext

**Change:** `CompletionContext` now caches `Rc<DimMeta>` between consecutive `reset_for_node`
calls. When the ortho's (dims, up_axis) pair is unchanged, the cached Rc is reused directly,
skipping the `RefCell::borrow_mut` + DimKey hash + FxHashMap lookup + `Rc::clone` (~21 ns/call).
A dims array comparison (~7 ns) guards the cache.

Benefit is highest when many branch children share the same shape (in-fill expansions), which
is the common case. `existing_bound_ns` dropped by ~20% (branch child bounds computed in bulk
with same dims).

### DFS bench (`dfs_ab_bench`) vs Items 5a+5b baseline (~40.6 ms)

| Benchmark | Items 5a+5b | Item 5d | Δ |
|---|---|---|---|
| baseline_bestfirst_prune_both | 40.584 ms | 40.655 ms | ~flat* |
| bestfirst_no_node_prune | ~40 ms | 37.154 ms | **−7.7%** |
| bestfirst_no_pruning | ~30 ms | 29.213 ms | ~−4% |
| no_bounds_no_pruning | 27.993 ms | 27.920 ms | ~flat |

*Primary config within noise; throughput probe_budget improved 3.90M → 3.98M steps/5s (+2.2%).

---

## Item 6 — Hash payload as raw bytes in `compute_id`

**Change:** In `Ortho::compute_id`, replaced `payload.hash(&mut hasher)` (which calls
`write_u32` per element — 4-byte writes processed in remainder path) with
`hasher.write_usize(len)` + `hasher.write(raw_bytes)` (processes 8 bytes/chunk for the payload).
Halves the number of FxHasher iterations for payload hashing.
Checkpoint version bumped 5 → 6.

### DFS bench (`dfs_ab_bench`) vs Item 5d (~40.6 ms)

| Benchmark | Item 5d | Item 6 | Δ |
|---|---|---|---|
| baseline_bestfirst_prune_both | 40.655 ms | 38.520 ms | **−5.3%** |
| branch_insertion_prune_both | 41.260 ms | ~39 ms | ~−5% |
| branch_worstfirst_prune_both | 39.858 ms | ~38 ms | ~−5% |
| bestfirst_no_completion_prune | 34.479 ms | ~33 ms | ~−4% |
| bestfirst_no_node_prune | 37.154 ms | ~35 ms | ~−5% |
| bestfirst_no_pruning | 29.213 ms | ~28 ms | ~−4% |
| no_bounds_no_pruning | 27.920 ms | ~27.9 ms | ~flat |

Primary production config: ~40.6 ms → 38.5 ms, **−5.3%**
Probe budget: 3.98M → 4.10M steps/5s (+3.0%).
Cumulative vs baseline (70.853 ms): **~−45.6%**
