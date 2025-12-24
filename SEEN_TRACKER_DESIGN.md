# History Store Design

## Overview

The history store maintains **sorted runs of all previously seen orthos** across generations. It enables the anti-join operation that produces novel orthos for the next generation's work queue.

## Purpose

```
dedupe results(g) vs results(g) and history
→ work(g+1)
```

History provides the "already seen" set for anti-join:
- Novel orthos = orthos in generation but not in history
- History grows monotonically (accepted count increases)
- Correctness depends on complete history, not on optimization

## Architecture

### Per-Bucket Sorted Runs

Each bucket maintains its own history as a collection of sorted run files:

```
fold_state/
└── history/
    ├── b=00/
    │   ├── run-001.bin
    │   ├── run-002.bin
    │   └── run-003.bin
    ├── b=01/
    │   └── run-001.bin
    └── ...
```

### Run Format

- **Sorted**: Records ordered by dedupe key
- **Integers** (bootstrap): sorted by value
- **Orthos** (production): sorted by `ortho.id()`
- **Encoding**: `bincode` serialization
- **Immutable**: Runs never modified after creation

## Anti-Join Operation

### Streaming Merge

```rust
fn anti_join(
    gen: UniqueRun,          // Current generation (sorted, unique)
    history: impl Iterator   // All history runs (sorted)
) -> (Vec<Ortho>, Run, u64)
```

**Algorithm**:
1. Stream merge gen and history iterators
2. For each item in gen:
   - If not in history: emit to work(g+1)
   - If in history: skip
3. Return:
   - Novel orthos (next work queue)
   - New run (add to history)
   - Accepted count (monotonic tracker)

**Complexity**:
- Time: O(n + m) where n = gen size, m = history size
- Space: O(fan_in × read_buf)

### Worked Example

```
History runs:
  run-001: [1, 3, 5, 7]
  run-002: [2, 4, 6]

Current generation (after compact):
  [2, 3, 4, 5, 6, 8, 9]

Anti-join result:
  Novel: [8, 9]       → work(g+1)
  Accepted: 5 items   → seen_len_accepted += 5
  New run: [2, 3, 4, 5, 6, 8, 9] → add to history
```

## Monotonic Accepted Count

```rust
seen_len_accepted: u64  // Global, monotonically increasing
```

**Semantics**:
- Tracks total unique orthos seen across all generations
- Increases by count of items added to history per bucket
- Never decreases
- Visible in TUI for progress tracking

**Update**: After each bucket's anti-join:
```rust
seen_len_accepted += accepted_count;
```

## Optional History Compaction

### When to Compact

History compaction is **optional** and does **not affect correctness**:

```rust
if run_count > 64 {
    compact_history(bucket, cfg)?;
}
```

### Compaction Process

1. Select subset of runs (e.g., oldest N runs)
2. K-way merge with deduplication
3. Write merged run
4. Delete source runs
5. Update run count

**Goal**: Reduce file handle count and improve anti-join performance.

**Guarantee**: Correctness does not depend on compaction frequency or strategy.

## Dedupe Rules

### Integers (Bootstrap)
- **Sort key**: value
- **Dedupe key**: value
- **Equality**: `a == b`

### Orthos (Production)
- **Sort key**: `ortho.id()`
- **Dedupe key**: `ortho.id()`
- **Equality**: `ortho.id() == other.id() && ortho == other`
- **Collision**: Log warning, keep first occurrence

## Integration with Generational Pipeline

```
┌────────────────────────────────────────┐
│ COMPACTING PHASE                       │
│ - External sort: landing → unique run  │
│ - Within-generation dedupe             │
└────────────────────────────────────────┘
              ↓
┌────────────────────────────────────────┐
│ ANTI-JOIN PHASE (per bucket)          │
│ 1. Open unique run (sorted)            │
│ 2. Open history iterator (sorted)      │
│ 3. Stream merge:                       │
│    - gen ∩ ¬history → novel orthos     │
│    - track accepted count              │
│ 4. Write novel orthos to work(g+1)     │
│ 5. Add unique run to history           │
│ 6. Update seen_len_accepted            │
└────────────────────────────────────────┘
              ↓
┌────────────────────────────────────────┐
│ NEXT GENERATION                        │
│ - work(g+1) contains only novel orthos │
│ - history complete up to generation g  │
└────────────────────────────────────────┘
```

## Memory Usage

History is **disk-backed** and uses bounded RAM:

- **Read buffers**: `fan_in × read_buf_bytes` (e.g., 16 × 8MB = 128MB)
- **Output buffer**: One write buffer for work segments
- **No in-memory history**: All runs stay on disk
- **Streaming**: Process one item at a time

## Performance Characteristics

### Anti-Join
- **Time**: O(n + m) where n = gen size, m = total history size
- **Space**: O(fan_in × read_buf)
- **IO**: Sequential reads of gen + all history runs

### Compaction (Optional)
- **Time**: O(k × n log k) where k = runs to merge
- **Space**: O(fan_in × read_buf)
- **IO**: Sequential reads + one sequential write

## Correctness Invariants

1. **Completeness**: History contains all orthos from generations 0..g
2. **Monotonicity**: `seen_len_accepted` never decreases
3. **Idempotence**: Re-running anti-join with same inputs produces same output
4. **No false positives**: Novel orthos are truly not in history
5. **No false negatives**: All history orthos are detected in anti-join

## Directory Structure

```
fold_state/
├── history/
│   └── b=XX/
│       ├── run-001.bin    # Earliest history
│       ├── run-002.bin
│       └── run-NNN.bin    # Latest history
└── work/
    └── seg-NNN.bin         # Novel orthos (next work queue)
```

## Design Rationale

### Why Sorted Runs?

- Enables O(n + m) anti-join
- Standard LSM-tree approach
- Well-understood performance
- Simple implementation

### Why Per-Bucket?

- Parallel processing
- Bounded memory per bucket
- Uniform distribution via hash

### Why Optional Compaction?

- Correctness doesn't depend on it
- Optimization for long-running processes
- Avoids premature complexity
- Can tune based on workload

### Why Not Bloom Filter?

- False positives unacceptable
- Would need verification anyway
- Sorted runs provide exact membership
- Simple and correct

### Why Not Hash Table?

- Cannot fit in memory
- Disk-backed hash tables are complex
- Sorted runs are simpler
- Standard external memory algorithm
