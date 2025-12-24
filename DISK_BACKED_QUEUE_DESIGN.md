# External Sort Design

## Overview

Fold uses **bucketed external sort** to deduplicate results within each generation. External sort enables processing datasets larger than RAM by using arena-based run generation and k-way merge, with disk used to bound RAM usage rather than for durability.

## Purpose

Transform unsorted landing logs into sorted, deduplicated runs:

```
landing/b=XX/drain-*.log  →  [external sort]  →  history/b=XX/run-NNN.bin
```

Each bucket is processed independently, enabling parallelization and bounded memory usage per bucket.

## Architecture

### Arena-Based Run Generation

**Goal**: Sort landing data in RAM-bounded chunks.

**Process**:
1. Allocate arena (e.g., `Vec<i64>` for bootstrap, `Vec<Ortho>` for production)
2. Fill arena until capacity reached:
   - Integers: `arena.len() * 8 <= run_budget_bytes`
   - Orthos: `sum(bincode::serialized_size(&ortho)) <= run_budget_bytes`
3. Sort arena in-place
4. Write arena to run file
5. Clear arena (reuse capacity) and repeat

**Run file format**:
- Sorted sequence of records
- Integers: raw `i64` (8 bytes LE) during bootstrap
- Orthos: `bincode` encoding in production
- File naming: `run-001.bin`, `run-002.bin`, etc.

### K-Way Merge

**Goal**: Merge multiple sorted runs into a single sorted, deduplicated run.

**Process**:
1. Open up to `fan_in` runs simultaneously
2. Maintain min-heap of (value, run_id) based on sort key
3. Stream merge with duplicate elimination:
   - Integers: dedupe by value
   - Orthos: dedupe by `ortho.id()` (with struct equality check for collisions)
4. Write merged output to new run file
5. If more than `fan_in` runs, repeat merge (multi-pass)

**Memory usage**:
- `fan_in` read buffers × `read_buf_bytes`
- One write buffer
- Min-heap overhead: O(fan_in)

### Multi-Pass Strategy

If run count exceeds `fan_in`, perform multiple merge passes:

```
Pass 1: 128 runs → 16 runs (fan_in=8)
Pass 2: 16 runs → 2 runs
Pass 3: 2 runs → 1 run (final unique run)
```

Each pass reduces run count by factor of `fan_in`.

## RAM Budget Configuration

### Leader Role
- `run_budget_bytes`: 2-6 GB (aggressive)
- Target RAM usage: 65-85% of available
- Suitable for primary processing nodes

### Follower Role
- `run_budget_bytes`: 256 MB - 1 GB (conservative)
- Target RAM usage: 50-70% of available
- Minimum viable: 128 MB
- Bail if insufficient RAM and memory pressure persists

### Dynamic Adjustment

RAM budget adjusts continuously based on:
- Global RSS percentage
- Available headroom
- Role-specific targets

Formula:
```rust
run_budget = 0.7 * budget  // 70% for arena
fan_in = clamp(budget / read_buf_bytes, 8, 128)
```

See MEMORY_OPTIMIZATION.md for detailed RAM policy.

## Bucketing Strategy

### Power-of-Two Buckets

```rust
bucket = ortho.id() as u64 & (B - 1)
```

where `B` is power-of-two bucket count (e.g., 16, 32, 64).

**Benefits**:
- Fast bucket calculation (bitwise AND)
- Uniform distribution (assuming good hash)
- Independent processing per bucket
- Parallelization opportunity

### Bucket Independence

Each bucket processes independently:
- Separate landing zones
- Separate sort phases
- Separate history stores
- No cross-bucket dependencies

## Disk Usage Model

**Disk bounds RAM, not durability**:
- Spill to disk when arena fills
- Runs are intermediate artifacts (NOT durable)
- Crash loses all intermediate state
- Heartbeat staleness triggers file-level restart from scratch

**No intermediate recovery**:
- Landing logs, runs, history are ephemeral during processing
- Only completed archives (after successful processing) are preserved
- Crash detection via heartbeat mechanism, not state inspection

## Performance Characteristics

### Arena Sort
- **Time**: O(n log n) per run
- **Space**: O(run_budget_bytes)
- **IO**: One sequential write per run

### K-Way Merge
- **Time**: O(n log k) where k = fan_in
- **Space**: O(fan_in × read_buf_bytes)
- **IO**: Sequential reads + one sequential write

### Multi-Pass
- **Passes**: ⌈log_fan_in(run_count)⌉
- **Total IO**: O(n × passes)

## Example: 10 GB Data, 2 GB RAM Budget

```
Arena capacity: ~200M integers (or ~2.5M orthos)
Run size: ~2 GB
Initial runs: 5 runs

K-way merge (fan_in=8):
  Pass 1: 5 runs → 1 run (single pass sufficient)

Total IO: ~20 GB (10 GB read, 10 GB write)
Peak RAM: 2 GB
```

## Record Formats

### Bootstrap (Integers)
- Raw `i64`: 8 bytes little-endian
- Sort key: value itself
- Dedupe key: value itself

### Production (Orthos)
- `bincode` encoding of `ortho::Ortho`
- Sort key: `ortho.id()`
- Dedupe key: `ortho.id()` + struct equality check
- Collision handling: Log and keep first

See `src/ortho.rs` for canonical type definition.

## Integration with Generational Pipeline

```
┌────────────────────────────────────────┐
│ DRAINING PHASE                         │
│ - Flush active landing logs            │
│ - Create immutable drain-N.log files   │
└────────────────────────────────────────┘
              ↓
┌────────────────────────────────────────┐
│ COMPACTING PHASE (per bucket)         │
│ 1. Arena-based run generation          │
│    - Read drain logs                   │
│    - Fill arena (RAM-bounded)          │
│    - Sort + write run                  │
│ 2. K-way merge                         │
│    - Merge runs → unique sorted run    │
│    - Dedupe within generation          │
└────────────────────────────────────────┘
              ↓
┌────────────────────────────────────────┐
│ ANTI-JOIN PHASE                        │
│ - Stream merge: gen vs history         │
│ - Emit novel orthos                    │
│ - Add run to history                   │
└────────────────────────────────────────┘
```

## Directory Structure

```
fold_state/
├── landing/
│   └── b=XX/
│       ├── active.log     # Receiving new orthos
│       └── drain-*.log    # Immutable, ready for sort
├── compact_temp/
│   └── b=XX/
│       └── run-*.bin      # Intermediate sorted runs
└── history/
    └── b=XX/
        └── run-*.bin      # Final deduplicated runs
```

## Design Rationale

### Why Arena-Based?

- Predictable memory usage
- Good cache locality (contiguous array)
- Fast in-place sort
- No allocation churn


### Why Bucketed?

- Parallel processing potential
- Bounded memory per bucket
- Uniform distribution via hash

### Why K-Way Merge?

- Flexible fan-in tuning
- Bounded file handles
- Predictable IO pattern
- Standard external sort approach

### Why Not In-Memory Sort?

- Dataset exceeds RAM
- Peak memory unpredictable
- Must bound memory usage
- External sort is proven approach
