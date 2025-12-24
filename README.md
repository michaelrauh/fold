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
