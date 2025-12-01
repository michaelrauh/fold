# Fold Architecture

Fold reduces raw text into an "optimal" orthotope (ortho) through a three-part flow:
1) provisioning (expected but not yet implemented), 2) staging, and 3) folding (ingestion + combination).
All coordination in the reference implementation happens via the local `fold_state/` directory.

## Provisioning (expected) vs. current state
- Intended: central storage (e.g., S3) plus colocated compute and noncentral storage; nodes coordinate only through central storage.
- Implemented: local disk only. Directories under `fold_state/` act as shared storage: `input/`, `in_process/`, `results_*/`, `seen_shards/`, `checkpoint/`.
- Heartbeats are the liveness primitive: files in work folders are touched on start and every 100,000 processed orthos; anything older than 10 minutes is treated as stale and recovered.

## Staging
- Use `stage.sh <input_file> [min_length] [state_dir]` to split source text into `fold_state/input/`.
- Splitting matches `splitter.rs`: break on paragraph gaps (`\n\n`), then on `. ? ; ! ,` sentence delimiters; lowercase and strip punctuation except apostrophes.
- Chunks are named `<basename>_chunk_####.txt` (not `<name>-1.txt`), and chunks shorter than `min_length` words (default 2) are deleted.
- Staging keeps the original input file; only the chunks are moved into `input/`.

## Folding modes (src/main.rs)
Each loop iteration:
- Recover stale work in `in_process/` (moves abandoned txt or archives back to `input/` and deletes partial queues/shards).
- Update metrics/TUI; set mode to either text ingestion or archive merge.
- Pick action: if two or more archives exist, merge the two largest by ortho count; otherwise process the next-largest `.txt`.
- Touch heartbeats every 100,000 processed orthos; update charts every 1,000.

### Ingestion (txt → archive)
- Move `<file>.txt` to `in_process/<file>.txt.work/` with `source.txt` and a heartbeat; record text preview and word count.
- Build an interner from the text (see `splitter.rs` rules); derive `MemoryConfig` from the serialized interner size.
- Create per-job paths: `results_<file>/` (results queue), `<work>/queue/` (work queue), `<work>/seen_shards/` (seen tracker). All are isolated per job.
- Seed the work queue with `Ortho::new()` and insert its id into the `SeenTracker`.
- Processing loop per ortho:
  - `get_requirements` on the ortho → required prefix lists and forbidden diagonals.
  - `interner.intersect(required, forbidden)` to get completion token ids.
  - `ortho.add(completion)` may yield multiple children (up/over expansions). Dedup with `SeenTracker`, push to results and work queues, and update best-ortho metrics.
- When the queue empties: flush results, write `optimal.txt`/`optimal.bin`, `interner.bin`, `lineage.txt` (`"filename"`), `metadata.txt` (ortho count), and `text_meta.txt` (word count + preview) into a new archive under `fold_state/input/archive_<...>.bin/`. Remove the work folder (queue, seen shards, heartbeat, source).

### Combination (archive merge → archive)
- Choose the two largest archives by ortho count; move them to `in_process/` and create `merge_<pid>.work/` with a heartbeat.
- Load interners and lineages. The larger vocabulary becomes the base; the smaller side is remapped.
- Detect impacted prefixes via `Interner::impacted_keys` for both sides.
- Build merged interner (larger absorbs smaller); compute `MemoryConfig`.
- Rehydrate state:
  - Read all orthos from the larger archive results; dedup and enqueue if impacted.
  - Read all orthos from the smaller archive results; remap vocab indices into the merged interner; dedup and enqueue if impacted.
  - Seed an empty ortho into the work queue.
- Process the work queue with the same loop as ingestion.
- Save merged archive to `fold_state/input/archive_merged_<...>.bin/` with merged lineage `(lineage_a lineage_b)` and combined text metadata. Delete the two source archives and merge work folder.

## Archive format (actual)
Archives live in `fold_state/input/archive_*.bin/` and contain:
- `interner.bin` – serialized interner for the archive.
- `results/` – bincode-serialized orthos (`queue_XXXXXXXX.bin`).
- `optimal.txt` / `optimal.bin` – best ortho for humans and for recovery.
- `lineage.txt` – S-expression provenance of source files/merges.
- `metadata.txt` – ortho count.
- `text_meta.txt` – word count (line 1) and preview (line 2).
Archives do **not** keep heartbeats, work queues, or seen shards; those live only in the in-process work folders.

## Core components (high level)
- Interner (`src/interner.rs`): deduplicated vocabulary + prefix→completion `FixedBitSet`s; `intersect` ANDs required prefixes and masks out forbidden indices; `merge` unions vocab and completions; `impacted_keys` flags prefixes whose completion sets change between versions.
- Ortho (`src/ortho.rs`) and Spatial (`src/spatial.rs`): orthotope dims/payload/up-axis; `add` fills next slot, expanding "up" (add a new axis) or "over" (grow dims) with canonical reorg patterns; special-case sorting on the third insert into a [2,2] ortho; `get_requirements` uses spatial impacted-phrase locations and diagonals (with parent-filled enrichment) to produce required/forbidden token ids.
- Disk-backed queue (`src/disk_backed_queue.rs`): in-memory `VecDeque` buffer; when `buffer_size` is hit, spill the newest half to `queue_XXXXXXXX.bin`; when the buffer is empty and disk files exist, load the newest file back into memory. `flush` writes the entire buffer. Length counts buffer + an approximate disk count (reset after reload).
- Seen tracker (`src/seen_tracker.rs`): bloom filter (1% FPR) plus hash-sharded maps persisted with bincode. Shard selection uses a hash of the ortho id; LRU keeps up to `max_shards_in_memory` loaded, evicting dirty shards to disk. Initializing a tracker clears any existing shard dir; persistence across runs relies on rehydrating from result queues.
- MemoryConfig (`src/memory_config.rs`): sizes queues, bloom capacity, and shard counts to target ~75% of system RAM given interner size and expected results.
- Checkpointing (`src/checkpoint_manager.rs`): atomic save of `interner.bin` + results backup; load rebuilds tracker by consuming all saved results into a fresh queue/tracker.

## Operational safety and recovery
- Heartbeats: touched on start and every 100,000 processed orthos; stale after 10 minutes.
- Recovery (`file_handler::check_and_recover_stale_work`): moves abandoned txt files or archives from `in_process/` back to `input/`, removes stale work folders, and deletes orphaned `results_merged_*` directories.
- Progress visibility: TUI (src/tui.rs) renders global stats, current operation progress, merge stats, optimal ortho preview, provenance, and recent logs; `q` quits.

## Current gaps and notes
- Provisioning/distributed storage is not implemented; everything assumes shared local disk.
- Stage chunk naming and location follow `stage.sh` (`<base>_chunk_####.txt` in `input/`).
- Archives omit seen shards and work queues, so recovery relies on replaying result queues.
- Heartbeat cadence is 100k orthos (not every 1k); work queues spill newest halves to disk rather than flushing all items when full.
