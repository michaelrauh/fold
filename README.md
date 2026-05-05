# fold

A text processing system that generates and optimizes orthogonal structures through generational frontier exploration.

## Overview

Fold uses a **generational frontier model** to process text:

1. **Build interner**: Extract vocabulary and phrase completion mappings from input text
2. **Generational processing**: Each generation processes a work queue to produce results
3. **Dedupe and advance**: Results are deduplicated against history; novel orthos become next generation's work
4. **Find optimal**: Track the ortho with maximum volume across all generations

### Generational Cycle

```
work(g) → process → results(g)

when work(g) empty:
  1. dedupe results(g) vs results(g) and history
  2. novel orthos → work(g+1)
```

Key features:
- **Landing → Compact → Anti-Join**: Results land in bucketed logs, compact via external sort, anti-join with history
- **Disk bounds RAM**: External sort ensures memory-bounded operation
- **Heartbeat-based recovery**: Crashes detected via stale heartbeats; files restart from scratch
- **Dynamic RAM allocation**: Leader/follower roles with continuous memory pressure adaptation

## Usage

### Prepare Input Files

Use `stage.sh` to split a large text file into chunks:

```bash
./stage.sh <input_file> <delimiter> [min_length]
```

Example:
```bash
./stage.sh book.txt "CHAPTER" 50000
```

This splits `book.txt` by "CHAPTER" delimiter, filtering out chunks smaller than 50000 characters, and places the results in `./fold_state/input/`.

### Run Fold

Process all files in the input directory:

```bash
cargo run --release
```

Or run against a specific file directly:

```bash
cargo run --release -- e.txt
```

The program will:
- Process text files from `fold_state/input/`
- Move file to `in_process/` and create heartbeat
- Build interner from text content
- Run generational frontier exploration:
  - Process work queue items
  - Land results in bucketed logs
  - Compact via external sort
  - Anti-join with history to find novel orthos
  - Advance to next generation
  - Update heartbeat periodically (e.g., every 100K orthos)
- Track optimal ortho across generations
- Save archive and delete heartbeat on success

Release builds enable parallel child-bound computation by default. To disable it for comparison or debugging:

```bash
FOLD_PARALLEL_CHILD_BOUNDS=0 cargo run --release -- e.txt
```

Fold defaults Rayon to `2` worker threads. Override it if you want a different setting:

```bash
RAYON_NUM_THREADS=1 cargo run --release -- e.txt
RAYON_NUM_THREADS=4 cargo run --release -- e.txt
```

### Crash Recovery

**Heartbeat mechanism**:
- Each file being processed has a corresponding `.heartbeat` file
- Updated periodically during processing
- On startup, check for stale heartbeats (e.g., >10 minutes since last update)
- Stale heartbeat triggers recovery:
  - Move input file back to `input/`
  - Delete all intermediate state (`landing/`, `work/`, `history/`)
  - Processing restarts from scratch

**Key principle**: Intermediate state is ephemeral and tied to heartbeat liveness. Only completed archives are durable.

## Development

### Run Tests

```bash
cargo test
```

### Build

```bash
cargo build --release
```

### Code Style Guidelines

This project follows specific coding principles for performance and clarity:

1. **Functional Style Preferred**: Use non-mutating operations where performance allows
2. **Self-Documenting Code**: Avoid comments; let code express intent through clear naming
3. **Support Actual Usage**: Avoid defensive programming; implement what the call patterns require
4. **Memory Critical**: Minimize cloning and unnecessary allocations; orthos are 80-900+ bytes each
5. **Test-Driven Development**: Write failing tests first, then implement fixes
6. **Disk-Backed Operations**: Design with streaming/disk storage from the start

## Architecture

### Core Components

- **Interner**: Vocabulary and phrase completion mappings
- **Ortho**: Orthogonal structures with spatial dimensions (80-900+ bytes each)
- **GenerationStore**: Landing zones, work segments, history runs
- **External Sort**: Arena-based run generation + k-way merge
- **Anti-Join**: Streaming merge to find novel orthos

### Directory Structure

```
fold_state/
├── landing/           # Append-only result logs (RAM-bounded)
│   └── b=XX/
│       ├── active.log
│       └── drain-*.log
├── work/              # Unordered work segments
│   └── seg-*.bin
└── history/           # Sorted deduplicated runs
    └── b=XX/
        └── run-*.bin
```

### Documentation

- **CHECKPOINT_DESIGN.md**: Generational frontier model and heartbeat-based file recovery
- **DISK_BACKED_QUEUE_DESIGN.md**: External sort and bucketed compaction
- **SEEN_TRACKER_DESIGN.md**: History store and anti-join correctness
- **MEMORY_OPTIMIZATION.md**: Dynamic RAM policy for leader/follower roles

## DFS Rotation Pruning

When the DFS reaches a base ortho (all dims == 2) with one empty slot, filling that slot triggers `expand_up`, which inserts a new axis at a position determined by `get_insert_position(completion_token)`. The position reflects where the new axis value sorts among the existing axis tokens.

Two different parent frames with different history can both arrive at the same canonical expanded ortho via `expand_up` calls that use different `insert_axis` values. To prevent exploring these rotations multiple times:

Each `SearchFrame` carries a `min_insert_axis` value. When `expand_up` fires, the completion is skipped if its `insert_axis < frame.min_insert_axis`. Child frames are created with `min_insert_axis = insert_axis`, enforcing a non-decreasing sequence of axis expansion positions along every root-to-leaf path. This guarantees exactly one canonical construction path for each unique expanded ortho.

This pruning only applies to the `expand_up` path. The `[2,2]` axis-swap canonicalization (which ensures axis tokens at positions 1 and 2 are always held in sorted order) handles the analogous symmetry within the base shape.

