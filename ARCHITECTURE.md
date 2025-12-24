# Fold Architecture Documentation

This document describes the generational store lifecycle, main processing flow, and memory management through detailed ASCII diagrams.

## Store Lifecycle: Generation Processing

The GenerationStore manages a generational frontier model where work(g) → process → results(g), and when work is empty, results are deduplicated to create work(g+1).

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        GENERATIONAL STORE LIFECYCLE                          │
└─────────────────────────────────────────────────────────────────────────────┘

Generation G Processing:
────────────────────────

                          ┌─────────────────┐
                          │  Work Queue (G) │  ← pop_work() iteratively
                          │  (segments)     │     retrieves orthos to process
                          └────────┬────────┘
                                   │
                                   ↓ process each ortho
                          ┌────────────────┐
                          │  Main Loop     │  ← interner.intersect() + ortho.add()
                          │  (process)     │     generates child orthos
                          └────────┬───────┘
                                   │
                                   ↓ record_result() for each child
                    ┌──────────────┴──────────────┐
                    │   Landing Zone (ephemeral)   │  ← Bucketed append-only logs
                    │   Per-bucket active.log      │     Hash ortho.id() → bucket
                    │   B=8 buckets (power-of-2)   │     NOT durable: crash = delete all
                    └──────────────┬──────────────┘
                                   │
                                   │ when work(G) empty → on_generation_end()
                                   ↓

────────────────────────────────────────────────────────────────────────────────
Generation Transition (on_generation_end):
────────────────────────────────────────────────────────────────────────────────

For each bucket b in [0..B):

    ┌──────────────┐
    │  1. DRAIN    │  drain_bucket(b) → RawStream
    └──────┬───────┘     • rename active.log → drain-N.log
           │             • unsorted orthos from landing zone
           ↓             • ephemeral file (deleted after compact)

    ┌──────────────┐
    │ 2. COMPACT   │  compact_landing(b, raw, cfg) → Vec<Run>
    └──────┬───────┘     • External sort with arena (Vec<Ortho>)
           │             • Budget: cfg.run_budget_bytes (2-6GB leader, 256MB-1GB follower)
           │             • On overflow: sort arena → write run → clear arena
           │             • Multiple runs created if data > budget
           ↓             • Each run is sorted by ortho.id()

    ┌──────────────┐
    │  3. MERGE    │  merge_unique(runs, cfg) → UniqueRun
    └──────┬───────┘     • K-way merge using BinaryHeap (min-heap by id)
           │             • Multi-pass if runs.len() > cfg.fan_in (8-128)
           │             • Deduplicates by ortho.id() + struct equality
           │             • Result: single sorted deduplicated run
           ↓

    ┌──────────────┐
    │ 4. ANTI-JOIN │  anti_join_orthos(unique_run, history) → (work, seen_run, accepted)
    └──────┬───────┘     • Streaming merge: emit x iff x ∈ gen AND x ∉ history
           │             • Compare by ortho.id() + equality
           │             • Returns: novel orthos (new work), seen run, count accepted
           │
           ├─→ new_work: Vec<Ortho> → push_segments() → Work Queue (G+1)
           │
           └─→ seen_run: Run → add_history_run(b, seen_run, accepted)
                                    │
                                    ↓
                         ┌─────────────────────┐
                         │  History Store      │  • Per-bucket sorted runs
                         │  Per bucket: runs[] │  • Monotonic seen_len_accepted
                         │  Immutable logs     │  • Used for anti-join in next gen
                         └─────────┬───────────┘
                                   │
                                   ↓ optional (if run_count > 64)
                         ┌─────────────────────┐
                         │ 5. HISTORY COMPACT  │  • Merge oldest runs
                         │    (optional)       │  • Drop duplicates
                         └─────────────────────┘  • Keep recent runs separate


After all buckets processed:

    Work Queue (G+1) filled with novel orthos
              ↓
    If work_len > 0: continue to Generation G+1
    If work_len = 0: processing complete


────────────────────────────────────────────────────────────────────────────────
Directory Structure on Disk:
────────────────────────────────────────────────────────────────────────────────

fold_state/
├── in_process/
│   └── <file>/
│       └── generation_store/
│           ├── landing/
│           │   ├── b=00/
│           │   │   ├── active.log      ← Current append target
│           │   │   └── drain-*.log     ← Drained (ephemeral)
│           │   ├── b=01/
│           │   └── ... (8 buckets)
│           │
│           ├── work/
│           │   └── segment-*.dat       ← Unordered work segments
│           │
│           ├── runs/
│           │   ├── run-*.dat           ← Sorted runs from compact
│           │   ├── unique-*.dat        ← Deduplicated merge results
│           │   ├── chunk-*.dat         ← Intermediate merge chunks
│           │   └── seen-*.dat          ← Anti-join output (seen runs)
│           │
│           └── history/
│               ├── b=00/
│               │   └── history-*.dat   ← Immutable seen runs (per bucket)
│               ├── b=01/
│               └── ... (8 buckets)
│
└── cached_runs/
    └── <archive>.dat                   ← Final archived results


Key Properties:
──────────────
• Bucketing: ortho.id() & (B-1) distributes orthos across 8 buckets
• Ephemeral state: landing/, work/, runs/ deleted on heartbeat staleness
• Durable state: history/ and cached_runs/ survive crashes
• Resume: ONLY at file/heartbeat level (no mid-generation resume)
• Deduplication: Within each bucket's generation via merge_unique
                 Across generations via anti-join against history
• Memory bound: All stages use cfg.run_budget_bytes to prevent OOM
```

## Main Processing Flow

The main loop orchestrates file ingestion, ortho generation, and archiving across multiple generations.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          MAIN PROCESSING FLOW                                │
└─────────────────────────────────────────────────────────────────────────────┘

Outer Loop: Role-based Work Selection
──────────────────────────────────────

    ┌──────────────────┐
    │ Determine Role   │  • Leader: claim leader lock (first worker)
    └────────┬─────────┘  • Follower: all other workers
             │
             ↓
    ┌─────────────────────────────────────────────┐
    │  Role: LEADER                               │  Role: FOLLOWER
    │  Priority: merge archives, then text        │  Priority: text, then small merges
    ├─────────────────────────────────────────────┤
    │  1. Get two LARGEST archives                │  1. Get txt file
    │     → merge_archives()                      │     → process_txt_file()
    │                                             │
    │  2. If no archives, get txt file            │  2. If no txt, get two SMALLEST archives
    │     → process_txt_file()                    │     → merge_archives()
    │                                             │
    │  3. If nothing, exit                        │  3. If nothing, exit
    └─────────────────────────────────────────────┘


Process Text File Flow:
───────────────────────

    ┌──────────────────┐
    │ 1. Ingest Text   │  file_handler::ingest_txt_file()
    └────────┬─────────┘  • Move txt → in_process/<file>/
             │             • Create heartbeat file
             │             • Split into chunks
             ↓
    ┌──────────────────┐
    │ 2. Build Interner│  Interner::from_text()
    └────────┬─────────┘  • Extract vocabulary from text
             │             • Build phrase prefix mappings
             │             • Version tracking for changes
             ↓
    ┌──────────────────┐
    │ 3. Compute Config│  Config::compute_config(role)
    └────────┬─────────┘  • Adaptive RAM policy based on system memory %
             │             • Leader: 65% aggressive / 85% conservative
             │             • Follower: 50% aggressive / 70% conservative
             │             • Returns: run_budget_bytes, fan_in, read_buf_bytes
             │             • Follower may bail if insufficient memory
             ↓
    ┌──────────────────┐
    │ 4. Init Store    │  GenerationStore::new()
    └────────┬─────────┘  • 8 buckets (power-of-2)
             │             • Initialize landing/, work/, runs/, history/
             │
             ↓
    ┌──────────────────┐
    │ 5. Seed Work     │  push_segments(vec![Ortho::new()])
    └────────┬─────────┘  • Start with empty ortho
             │
             ↓
    ┌─────────────────────────────────────────────────────────────────────────┐
    │                    GENERATIONAL PROCESSING LOOP                          │
    └─────────────────────────────────────────────────────────────────────────┘

    Generation G:
    ────────────

        ┌──────────────────┐
        │ While work_len>0 │  ← store.work_len() > 0
        └────────┬─────────┘
                 │
                 ↓ pop_work() → ortho
        ┌──────────────────────────────────────────┐
        │  Process Ortho:                          │
        │                                          │
        │  1. Get requirements:                    │
        │     (forbidden, required) =              │
        │       ortho.get_requirements()           │
        │                                          │
        │  2. Get completions:                     │
        │     completions =                        │
        │       interner.intersect(required,       │
        │                         forbidden)       │
        │                                          │
        │  3. Generate children:                   │
        │     for completion in completions:       │
        │       children = ortho.add(completion)   │
        │       for child in children:             │
        │         • Check if best ortho            │
        │         • store.record_result(child)     │  ← To landing zone
        │                                          │
        └──────────────────┬───────────────────────┘
                           │
                           ↓ Repeat until work empty
        ┌──────────────────────────────────────────┐
        │  End of Generation:                      │
        │                                          │
        │  new_work = store.on_generation_end(cfg) │  ← Drain/compact/merge/anti-join
        │                                          │     Pushes novel orthos to work(G+1)
        │  generation += 1                         │
        └──────────────────┬───────────────────────┘
                           │
                           ↓
                    ┌──────────────┐
                    │ Loop or Exit │  If new_work > 0: continue
                    └──────────────┘  If new_work = 0: done


    ┌──────────────────┐
    │ 6. Collect All   │  • Iterate history_iter() for all 8 buckets
    │    Results       │  • Accumulate all seen orthos
    └────────┬─────────┘
             │
             ↓
    ┌──────────────────┐
    │ 7. Save Archive  │  save_archive_from_vec()
    └────────┬─────────┘  • Write orthos + interner + optimal ortho
             │             • Move to cached_runs/<archive>.dat
             │             • Delete in_process/<file>/
             │             • Remove heartbeat
             ↓
    ┌──────────────────┐
    │ 8. Update Optimal│  • Update global optimal ortho if improved
    │    Ortho         │  • Metrics tracking for TUI
    └──────────────────┘


Merge Archives Flow (for completeness):
────────────────────────────────────────

    ┌──────────────────┐
    │ 1. Load Archives │  • Load orthos + interners from both archives
    └────────┬─────────┘
             │
             ↓
    ┌──────────────────┐
    │ 2. Merge Interners│ • Combine vocabularies
    └────────┬─────────┘  • New version if vocab changed
             │
             ↓
    ┌──────────────────┐
    │ 3. Rewind Orthos │  • Rewind impacted orthos to earlier state
    └────────┬─────────┘  • Based on interner version changes
             │
             ↓
    ┌──────────────────┐
    │ 4. Dedupe + Sort │  • Combine ortho sets
    └────────┬─────────┘  • Deduplicate by ortho.id()
             │             • Sort by id for efficient storage
             ↓
    ┌──────────────────┐
    │ 5. Save Archive  │  • Write merged result
    └──────────────────┘  • Delete source archives


Periodic Operations:
────────────────────

Every 1,000 orthos:
  • Update metrics (optimal volume, progress)
  • Check system RAM
  • Follower bail-out if RAM > 85%

Every 50,000 orthos:
  • Log progress

Every 100,000 orthos:
  • Print optimal ortho
  • Touch heartbeat (prevent staleness)
  • Touch memory claim
  • Touch leader lock (if leader)


Key Flow Properties:
────────────────────
• Generational: Clear separation between generations, no mixing
• Streaming: Orthos processed one-by-one, results batched to landing
• Memory-bounded: All stages respect cfg.run_budget_bytes
• Incremental: Work queue allows resuming (though only at file level)
• Adaptive: Config adjusts to system memory pressure in real-time
```

## Binary Heap K-Way Merge: RAM Management

The k-way merge is critical for preventing OOM when merging many sorted runs into a single deduplicated output.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                   BINARY HEAP K-WAY MERGE (merge_unique)                    │
└─────────────────────────────────────────────────────────────────────────────┘

Problem:
────────
We have N sorted runs on disk, each potentially gigabytes in size. We need to
merge them into a single sorted, deduplicated run WITHOUT loading all data
into RAM simultaneously.

Solution: K-way merge with binary heap
───────────────────────────────────────

The key insight: Only keep ONE ortho per run in memory at a time, plus a small
heap to track which ortho should be emitted next.


Data Structure:
───────────────

    Runs on Disk (N files):
    ───────────────────────
    run-0.dat: [o1, o3, o5, o7, o9, ...]     (sorted by id)
    run-1.dat: [o2, o3, o6, o8, o10, ...]    (sorted by id)
    run-2.dat: [o1, o4, o5, o9, o11, ...]    (sorted by id)
    ...
    run-K.dat: [o2, o6, o7, o12, ...]        (sorted by id)


    Memory Structure (BOUNDED):
    ───────────────────────────

    ┌─────────────────────────────────────────────┐
    │  BinaryHeap (min-heap by ortho.id)         │  ← Size: O(K) where K = fan_in
    │  ┌─────────────────────────────────────┐   │    Typically 8-128 entries
    │  │ HeapItem { id: 1, run_idx: 0 }      │ ← Top (minimum id)
    │  │ HeapItem { id: 1, run_idx: 2 }      │
    │  │ HeapItem { id: 2, run_idx: 1 }      │   Total heap memory:
    │  │ HeapItem { id: 2, run_idx: 4 }      │   K * (8 bytes id + 8 bytes idx)
    │  │ ...                                 │   = 128 * 16 = ~2KB max
    │  └─────────────────────────────────────┘   │
    └─────────────────────────────────────────────┘

    ┌─────────────────────────────────────────────┐
    │  Current Orthos: Vec<Option<Ortho>>        │  ← Size: O(K) orthos
    │  ┌─────────────────────────────────────┐   │    One ortho per run
    │  │ [0]: Some(Ortho { id=1, ... })      │   │
    │  │ [1]: Some(Ortho { id=2, ... })      │   │   Each ortho: 80-900+ bytes
    │  │ [2]: Some(Ortho { id=1, ... })      │   │   Total: K * ~400 bytes avg
    │  │ [3]: None  (exhausted run)          │   │   = 128 * 400 = ~50KB max
    │  │ ...                                 │   │
    │  └─────────────────────────────────────┘   │
    └─────────────────────────────────────────────┘

    Total memory for merge state: ~52KB (negligible vs run_budget_bytes)


Algorithm Flow:
───────────────

    INITIALIZATION:
    ─────────────

    1. Open file iterator for each run (no data loaded yet)
       iterators: Vec<RunIterator>  ← N file handles, minimal RAM

    2. Prime the heap: read FIRST ortho from each run
       for each run i in [0..N):
           ortho = iterators[i].next()          ← Decode one ortho
           current_orthos[i] = Some(ortho)      ← Store in slot
           heap.push(HeapItem {                 ← Add to heap
               id: ortho.id(),
               run_idx: i
           })

       Heap now contains N entries (one per run), tracking minimum id.


    MERGE LOOP:
    ─────────

    while heap is not empty:

        ┌──────────────────────────────────────────┐
        │ 1. Pop minimum from heap                 │
        │    item = heap.pop()                     │  ← O(log K)
        │    → item = { id: 5, run_idx: 2 }        │
        └────────────────┬─────────────────────────┘
                         │
                         ↓
        ┌──────────────────────────────────────────┐
        │ 2. Retrieve ortho from current_orthos    │
        │    ortho = current_orthos[2].take()      │  ← Move ortho out
        │    → ortho = Ortho { id=5, ... }         │
        └────────────────┬─────────────────────────┘
                         │
                         ↓
        ┌──────────────────────────────────────────┐
        │ 3. Write to output (with deduplication)  │
        │    if id != last_written_id:             │
        │        encode(ortho) → output file       │  ← Dedupe by id
        │        last_written_id = id              │
        └────────────────┬─────────────────────────┘
                         │
                         ↓
        ┌──────────────────────────────────────────┐
        │ 4. Refill from same run                  │
        │    next_ortho = iterators[2].next()      │  ← Read next from run 2
        │                                          │
        │    if Some(next):                        │
        │        current_orthos[2] = Some(next)    │  ← Refill slot
        │        heap.push(HeapItem {               │  ← Re-add to heap
        │            id: next.id(),                │
        │            run_idx: 2                    │
        │        })                                │
        │    else:                                 │
        │        run 2 is exhausted                │  ← Slot stays None
        └──────────────────────────────────────────┘

        Repeat until heap empty (all runs exhausted)


    ┌────────────────────────────────────────────────────────────────────┐
    │  KEY INSIGHT: RAM Usage is O(K) not O(N)                           │
    │                                                                     │
    │  • N = total orthos across all runs (could be millions/billions)  │
    │  • K = fan_in (number of runs being merged, 8-128)                │
    │                                                                     │
    │  Memory usage: K orthos + K heap entries ≈ 50KB                   │
    │                                                                     │
    │  Compare to naive approach (load all N orthos): GBs → OOM!        │
    └────────────────────────────────────────────────────────────────────┘


Multi-Pass Merge (when N > fan_in):
────────────────────────────────────

If we have more runs than fan_in allows (e.g., 1000 runs but fan_in=128):

    Pass 1: Merge runs in chunks of 128
    ───────────────────────────────────
    Runs [0..127]   → merge → intermediate-0.dat
    Runs [128..255] → merge → intermediate-1.dat
    Runs [256..383] → merge → intermediate-2.dat
    ...

    Result: 8 intermediate runs (1000 / 128 ≈ 8)

    Pass 2: Merge intermediate runs
    ────────────────────────────────
    intermediate-[0..7] → merge → unique-final.dat

Each pass maintains O(K) memory usage, not O(N).


Why Binary Heap is Perfect Here:
─────────────────────────────────

1. Min-heap property: Always gives us the smallest id next → sorted output
2. Efficient operations:
   • pop(): O(log K) ← Remove minimum
   • push(): O(log K) ← Insert next item from same run
3. Small memory footprint: O(K) entries, each tiny (16 bytes)
4. No need to compare full orthos, just ids (cheap comparison)


RAM Budget Enforcement:
────────────────────────

The merge itself is always O(K) ≈ 50KB, but we control K via cfg.fan_in:

    fan_in = clamp(run_budget_bytes / read_buf_bytes, 8, 128)

    Example with 2GB budget:
    ─────────────────────────
    fan_in = 2_000_000_000 / 65536 = 30,517 → clamped to 128

    With 128 fan_in:
    • Heap: 128 entries = 2KB
    • Current orthos: 128 * 400 bytes = 50KB
    • Read buffers: 128 * 64KB = 8MB
    • Total merge overhead: ~8MB (well within budget)

    Remaining budget available for arena in compact_landing stage.


Deduplication During Merge:
────────────────────────────

    ┌─────────────────────────────────────────┐
    │  Merge naturally handles duplicates:    │
    │                                         │
    │  Run 0: [o1, o3, o5]                    │
    │  Run 1: [o2, o3, o6]                    │
    │  Run 2: [o1, o4, o5]                    │
    │                                         │
    │  Heap at some point:                    │
    │    { id:3, run:0 }  ← min               │
    │    { id:3, run:1 }                      │
    │    { id:4, run:2 }                      │
    │                                         │
    │  Pop id:3 from run 0 → write            │
    │  last_written_id = 3                    │
    │                                         │
    │  Pop id:3 from run 1 → SKIP             │
    │  (id == last_written_id)                │
    │                                         │
    │  Result: only ONE ortho with id=3       │
    └─────────────────────────────────────────┘

    Dedupe is O(1) per item: just compare current id to last_written_id.
    No hash table needed!


Summary:
────────
• K-way merge keeps memory usage at O(K) orthos, not O(N)
• Binary heap efficiently tracks minimum across K streams
• Multi-pass strategy handles arbitrary N runs
• Deduplication is free (streaming comparison)
• Fan-in dynamically adjusts to available RAM
• Result: Merge multi-GB datasets with <10MB RAM overhead
