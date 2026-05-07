# Performance Baseline — 2026-05-06

Captured before any optimization changes. All 140+ tests green.
Platform: Apple Silicon (dev machine). Deploy target: x86-64-v3 (AVX2/BMI2/POPCNT).

## Test Suite

```
cargo test
```

- lib tests: 140 passed, 0 failed, 3 ignored
- integration tests: 16 passed across 8 suites
- **All green.**

---

## DFS Benchmarks (`dfs_ab_bench` — 110 iterations per sample)

| Benchmark | Median time |
|---|---|
| baseline_bestfirst_prune_both | 70.853 ms |
| branch_insertion_prune_both | 66.591 ms |
| branch_worstfirst_prune_both | 55.666 ms |
| bestfirst_no_completion_prune | 54.386 ms |
| bestfirst_no_node_prune | 55.862 ms |
| bestfirst_no_pruning | 44.223 ms |
| no_bounds_no_pruning | 39.216 ms |

*Primary production config is **baseline_bestfirst_prune_both** at 70.853 ms / 110 steps ≈ 644 µs/step.*

---

## Ortho Benchmarks (`ortho_bench`)

| Benchmark | Median time |
|---|---|
| ortho_new | 100.64 ns |
| ortho_add_simple | 180.56 ns |
| ortho_add_multiple | 172.66 ns |
| ortho_id | 211.77 ps |
| ortho_add_shape_expansion | 182.50 ns |
| ortho_get_requirements | 279.92 ns |
| ortho_get_requirement_phrases | 278.65 ns |
| ortho_remap | 106.05 ns |
| ortho_prefixes | 893.96 ns |
| ortho_prefixes_for_last_filled | 188.65 ns |
| ortho_get_current_position | 209.83 ps |
| ortho_dims | 209.61 ps |
| ortho_payload | 209.64 ps |
| ortho_display | 640.82 ps |

---

## Interner Benchmarks (`interner_bench`)

| Benchmark | Median time |
|---|---|
| interner_from_text | 220.31 µs |
| interner_from_text_large | 303.41 µs |
| interner_add_text | 79.827 µs |
| interner_intersect_simple | 108.64 ns |
| interner_intersect_complex | 57.644 ns |
| interner_intersect_many_forbidden | 130.15 ns |
| interner_merge | 63.116 µs |
| interner_completions_for_prefix | 8.3843 ns |
| interner_impacted_keys | 53.205 µs |
| interner_completions_equal_up_to_vocab | 125.06 ns |
| interner_all_completions_equal_up_to_vocab | 392.98 ns |
| interner_string_for_index | 1.1124 ns |
| interner_vocabulary | 393.45 ps |
| interner_vocab_size | 209.99 ps |
| interner_version | 209.64 ps |

---

## Spatial Benchmarks (`spatial_bench`)

| Benchmark | Median time |
|---|---|
| get_requirements | 210.52 ns |
| is_base | 2.5072 ns |
| expand_up | 329.30 ns |
| expand_over | 41.272 ns |
| capacity_2d | 1.4985 ns |
| capacity_3d | 2.6213 ns |
| get_axis_positions_2d | 71.106 ns |
| get_axis_positions_3d | 60.673 ns |

---

## Current Ortho Layout (before Item 1)

```rust
pub struct Ortho {
    dims: Vec<Dim>,                   // Vec<u8>  – heap alloc, 2–8 elements
    payload: Vec<Option<PayloadVal>>, // Vec<Option<u32>> – heap alloc, 4–64 cells
    up_axis: Option<Dim>,
    fill_count: u32,
    next_empty: u32,
    score: OrthoScore,
    id: OrthoId,
}
```

`std::mem::size_of::<Ortho>()` = stack frame + two fat-pointer Vecs (3×8 bytes each = 48 bytes heap overhead) + actual heap allocations for dims (2–8 bytes) and payload (4–64 × 8 bytes = 32–512 bytes with Option tag).

Each `clone()` in the hot loop allocates two heap buffers. Item 1 targets eliminating all heap allocation from Ortho.

---

## Real-world throughput reference

Reported on DigitalOcean s-4vcpu-8gb (x86-64, 4 cores, 8 GB), vocab ~7k:  
**~2.8 M msg/s** at baseline.
