# Generational Frontier System Design

## Overview

Fold uses a **generational frontier model** where each generation processes a work queue to produce results, which are then deduplicated against history to form the next generation's work queue. This architecture uses disk to bound RAM usage, **not for durability**—there is no crash recovery of intermediate state. Resume is only at the **file/heartbeat level**: if processing crashes, heartbeat goes stale, and a later worker moves the input file back and deletes all intermediate state to restart from scratch.

## Core Architecture

### Generational Cycle

```
work(g) → process → results(g)

when work(g) empty:
  1. dedupe results(g) vs results(g) and history
  2. → work(g+1)
```

Each generation:
- Processes work queue items to produce results
- Results land in bucketed append-only logs
- When work empties, results are compacted via external sort
- Anti-join with history produces novel orthos
- Novel orthos become next generation's work queue

### Landing → Compact → Anti-Join Pipeline

```
┌──────────────────────────────────────────────────┐
│ PROCESSING PHASE (work(g) → results(g))         │
│ - Pop work item                                  │
│ - Generate child orthos                          │
│ - Append to landing zone (bucketed)             │
└──────────────────────────────────────────────────┘
                     ↓
┌──────────────────────────────────────────────────┐
│ DRAINING PHASE                                   │
│ - Flush active landing logs                      │
│ - Prepare for compaction                         │
└──────────────────────────────────────────────────┘
                     ↓
┌──────────────────────────────────────────────────┐
│ COMPACTING PHASE (per bucket)                   │
│ - External sort: arena-based runs               │
│ - K-way merge: runs → unique sorted run         │
│ - Dedupe within generation                      │
└──────────────────────────────────────────────────┘
                     ↓
┌──────────────────────────────────────────────────┐
│ ANTI-JOIN PHASE (per bucket)                    │
│ - Stream merge: gen(unique) vs history(sorted)  │
│ - Emit novel orthos → work(g+1)                 │
│ - Add unique run to history                     │
└──────────────────────────────────────────────────┘
```

## Landing Zone (Append-Only, RAM-Bounded)

### Purpose

Landing zones are **append-only logs** used purely to bound RAM during result generation. They accumulate results in memory and spill to disk when buffers fill.

### Structure

```
fold_state/
├── landing/
│   ├── b=00/
│   │   ├── active.log      # Current append target
│   │   ├── drain-001.log   # Drained (immutable)
│   │   └── drain-002.log
│   ├── b=01/
│   │   └── active.log
│   └── ...
```

- **Bucketing**: `bucket = ortho.id() & (B - 1)` where B is power-of-two bucket count
- **Active log**: Receives new orthos via append
- **Drain**: Rename `active.log → drain-N.log` creates immutable snapshot
- **Not durable**: Crash loses all landing/work/history state; heartbeat staleness triggers file-level restart

### Ortho Serialization

- Format: `bincode` encoding of `ortho::Ortho` from `src/ortho.rs`
- Little-endian, no compression
- Dedupe key: `ortho.id()` (hash of payload)
- Collision handling: Equal IDs require struct equality

## Heartbeat-Based File Recovery

**No intermediate state recovery**:
- Disk state (landing/work/history) is **not durable**
- Crash during processing loses all intermediate artifacts
- Heartbeat mechanism (updated periodically during processing) detects stale jobs
- On heartbeat staleness: later worker moves input file back to `input/` and deletes all intermediate state
- Processing restarts from scratch with fresh interner and empty state
- Only completed archives (with successful heartbeat deletion) are preserved

### Heartbeat Mechanism

```
fold_state/
├── input/              # Pending text files
├── in_process/         # Files being processed
│   ├── file1.txt       # Input file (moved from input/)
│   └── file1.heartbeat # Updated periodically (e.g., every 100K orthos)
└── archives/           # Completed archives (heartbeat deleted)
```

**Normal flow**:
1. Move `input/file.txt` → `in_process/file.txt`
2. Create `in_process/file.heartbeat`
3. Process: update heartbeat periodically
4. On completion: save archive, delete `.txt` and `.heartbeat`

**Crash recovery**:
1. On startup, scan `in_process/` for `.heartbeat` files
2. Check last modification time
3. If stale (e.g., >10 minutes since last update):
   - Move `file.txt` back to `input/`
   - Delete `file.heartbeat`
   - Delete all intermediate state (landing/, work/, history/ directories)
4. File will be reprocessed from scratch by this or another worker

**Key invariant**: Intermediate state is ephemeral and tied to heartbeat liveness.

## Work Queue (Unordered Segments)

Work items are stored as **unordered segments**—order is irrelevant:

```
fold_state/
├── work/
│   ├── seg-001.bin    # [count][ortho…]
│   ├── seg-002.bin
│   └── ...
```

- Drain segments sequentially
- Segment format: `[u64 count][ortho bincode…]`
- No ordering guarantee needed

## History Store (Sorted Runs, No Compaction Required)

### Purpose

History maintains all previously seen orthos across generations as **sorted runs**:

```
fold_state/
├── history/
│   ├── b=00/
│   │   ├── run-001.bin
│   │   ├── run-002.bin
│   │   └── ...
│   └── ...
```

### Anti-Join Correctness

- Anti-join streams merge: `gen(unique) ∩ ¬history(sorted)`
- Novel orthos = in gen but not in history
- Accepted count tracks `seen_len_accepted` (monotonic)
- History may optionally compact runs when count > 64 (correctness doesn't depend on this)

## Directory Structure

```
fold_state/
├── input/                # Pending text files
├── in_process/           # Files being processed (with heartbeats)
│   ├── file1.txt
│   └── file1.heartbeat
├── landing/              # Ephemeral result logs (deleted on stale heartbeat)
│   └── b=XX/
│       ├── active.log
│       └── drain-NNN.log
├── work/                 # Ephemeral work segments (deleted on stale heartbeat)
│   └── seg-NNN.bin
├── history/              # Ephemeral dedupe runs (deleted on stale heartbeat)
│   └── b=XX/
│       └── run-NNN.bin
└── archives/             # Completed, durable archives
    └── file1.bin/
        ├── interner.bin
        ├── optimal.txt
        └── lineage.txt
```

**Ephemeral directories**: `landing/`, `work/`, `history/` are tied to active processing and deleted on heartbeat staleness or successful completion.

**Durable directories**: Only `archives/` persists across crashes; everything else restarts from scratch.

## Leader/Follower Roles

The system supports two operational roles with different RAM allocation strategies:

### Leader
- Larger RAM budget (2-6 GB for run generation)
- Aggressive memory usage (targets 65-85% of available RAM)
- Suitable for primary processing nodes

### Follower
- Smaller RAM budget (256 MB - 1 GB for run generation)
- Conservative memory usage (targets 50-70% of available RAM)
- Suitable for resource-constrained nodes or when running multiple instances

RAM allocation is **continuous and dynamic**, adjusting based on global system memory pressure. See MEMORY_OPTIMIZATION.md for detailed RAM policy.

## Ortho Interchange Format

After integer bootstrap proving correctness, all boundaries use orthos:

- **Landing logs**: orthos serialized via `bincode`
- **Sorted runs**: orthos sorted by `ortho.id()`
- **History runs**: orthos with dedupe key = `ortho.id()`
- **Work segments**: orthos in unordered segment files

Dedupe rules: Equal IDs require struct equality; collisions log and keep first.

