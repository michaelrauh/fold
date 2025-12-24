GENERATIONAL STORE REWRITE — EXACT AGENT PROMPTS (DETAILED)
GLOBAL RULE (MANDATORY):Scan top-to-bottom.Find the first unchecked task ([ ]).Work on that task only.When complete, mark it [x] and STOP.Do not touch later tasks.

ORTHO REQUIREMENT (READ ME FIRST)
* Use integers to prove correctness and sizing first.
* At the end of every stage (landing, compact, anti-join, history), convert the surfaced artifacts to orthos.
* Final solution must run end-to-end on orthos; integers are only the bootstrap path.
* Leave explicit TODOs where the swap from integers to orthos happens so later tasks enforce it.
* `src/ortho.rs` defines the canonical type; binary format + dedupe rules are in Task 4.

[x] TASK 0 — Rewrite existing Markdown documentation
Objective
Bring all .md documentation in line with the new design.
Instructions
Rewrite docs that currently describe:
* spill-half disk queues
* bloom filters / tiered hashsets
* old memory config / tuning logic
Replace with explicit language describing:
* Generational frontier modelwork(g) → process → results(g)
* when work empty:
*   dedupe results(g) vs results(g) and history
*   → work(g+1)
* 
* Landing → compact → anti-join
* Bucketed external sort
* Disk used to bound RAM, not durability
* Coarse resume only
* Leader/follower RAM dials
* Orthos as the stable interchange type at each boundary after the integer bootstrap
* Make clear that existing memory config/tuning docs are superseded by the RAM policy in Task 10.
* **CRITICAL**: Resume is ONLY at the file/heartbeat level. Crashes during processing result in stale heartbeats that trigger file recovery (move back to input/) and deletion of all intermediate state (landing/, work/, history/). No intermediate state is preserved or resumed.
Deliverable
* Updated .md files committed.
STOP WHEN
* Docs reference only the new model.

[x] TASK 1 — Define new types and state machine (integers only)
Objective
Introduce the new system shape without behavior.
Required types
pub enum Role { Leader, Follower }

pub enum Phase {
    Processing,
    Draining,
    Compacting { bucket: usize },
    AntiJoin { bucket: usize },
    Idle,
}

pub struct Config {
    pub run_budget_bytes: usize,
    pub fan_in: usize,
    pub read_buf_bytes: usize,
    pub allow_compaction: bool,
}

pub struct GenerationStats {
    pub generation: u64,
    pub phase: Phase,
    pub work_len: u64,
    pub seen_len_accepted: u64,
    pub run_budget_bytes: usize,
    pub fan_in: usize,
}

pub struct GenerationStore { /* opaque */ }

pub struct RawStream;     // unsorted drained data
pub struct Run;           // sorted
pub struct UniqueRun;     // sorted + deduped
pub struct WorkSegments; // unordered segments
Constraints
* Integers only (i64 or u64)
* No IO
* No logic
Deliverable
* Code compiles
* TUI renders placeholder stats
STOP WHEN
* App builds.

[x] TASK 2 — Landing zone (append-only, ephemeral)
Objective
Append-only logs used purely to bound RAM. Not durable—deleted on heartbeat staleness.
Required functions
impl GenerationStore {
    pub fn record_result(&mut self, value: i64);
    pub fn drain_bucket(&mut self, bucket: usize) -> RawStream;
}
Behavior
* B = power-of-two bucket_count from config.
* Bucket (bootstrap): bucket = value as u64 & (B - 1)
* Bucket (ortho swap): bucket = ortho.id() as u64 & (B - 1)
* Append to landing/b=XX/active.log
* drain_bucket renames active.log → drain-N.log
* Not durable: crash = heartbeat stale = delete all intermediate state
* No resume of landing state; file restarts from scratch
* After integer proof, regenerate landing with orthos serialized per Task 4.
Tests
* append order preserved
* drain creates immutable file
* restart sees drained + active logs
STOP WHEN
* Disk successfully bounds RAM.

[x] TASK 3 — Work queue as unordered segments (integers)
Required functions
impl GenerationStore {
    pub fn push_segments(&mut self, items: Vec<i64>);
    pub fn pop_work(&mut self) -> Option<i64>;
    pub fn work_len(&self) -> u64;
}
Semantics
* Segment file format: [count][i64…]
* Drain segments sequentially
* Order irrelevant
* Ephemeral: deleted on heartbeat staleness
* After integer proof, rerun with ortho segments: same framing, ortho binary payload from Task 4.
Tests
* push N → pop N
* No restart test needed (ephemeral state, not preserved)
STOP WHEN
* Old disk queue unused.

[x] TASK 4 — Record formats + comparator (int bootstrap → ortho final)
Format
* Integers (bootstrap): raw i64 (8 bytes LE)
* Orthos (final): `bincode` encoding of `ortho::Ortho` from src/ortho.rs; little-endian, no compression.
Comparator
* Integers: a.cmp(&b)
* Orthos: sort/dedupe key = ortho.id(); for equal ids, require struct equality to accept; otherwise treat as collision and keep first, log collision.
Tests
* encode/decode roundtrip
* comparator correctness
* ortho dedupe by id + equality
STOP WHEN
* Sort code can rely on this.

[x] TASK 5 — External sort run generation (arena-based)
RAM rules
* run_budget_bytes default:
    * Leader: 2–6 GB
    * Follower: 256 MB – 1 GB
* Minimum viable: 128 MB
Implementation
fn compact_landing(
    bucket: usize,
    raw: RawStream,
    cfg: &Config,
) -> Vec<Run>;
* Arena: Vec<i64> (bootstrap), then Vec<Ortho> after swap
* Capacity limit (ints): arena.len() * 8 <= cfg.run_budget_bytes
* Capacity limit (orthos): use encoded_size <= cfg.run_budget_bytes (chunking by bincode-encoded len)
* On overflow:
    * sort arena
    * write run file
    * clear arena (reuse capacity)
Tests
* millions of ints
* millions of orthos
* bounded RSS
STOP WHEN
* No OOMs.

[x] TASK 6 — Unique merge → UniqueRun
Required function
fn merge_unique(
    runs: Vec<Run>,
    cfg: &Config,
) -> UniqueRun;
Semantics
* k-way merge
* fan-in ≤ cfg.fan_in
* multi-pass if needed
* drop adjacent duplicates (ints by value, orthos by id+equality)
Example
Input runs:
[1,2,3,5]
[2,3,4]
Output:
[1,2,3,4,5]
STOP WHEN
* UniqueRun correct.

[x] TASK 7 — History store (no compaction yet)
Required functions
impl GenerationStore {
    fn history_iter(&self, bucket: usize) -> impl Iterator<Item=i64>;
    fn add_history_run(&mut self, bucket: usize, run: Run, accepted: u64);
    fn seen_len_accepted(&self) -> u64;
}
Invariant
* seen_len_accepted is monotonic
* No dedupe across runs yet
* After ortho swap, history holds ortho runs and uses ortho id for ordering.
STOP WHEN
* History visible across generations.

[x] TASK 8 — Anti-join (core correctness)
Required function
fn anti_join(
    gen: UniqueRun,
    history: impl Iterator<Item=i64>,
) -> (Vec<i64>, Run, u64);
Semantics
* Streaming merge
* Emit x iff x ∈ gen and x ∉ history (ints by value, orthos by id+equality)
* Return:
    * next-work values
    * new seen run
    * accepted count
Worked example
History:
[1,3,5]
Gen:
[2,3,4,5,6]
Result:
work = [2,4,6]
accepted = 3
STOP WHEN
* Frontier logic proven.

[x] TASK 9 — Optional history compaction
Trigger
* if run count > 64
Function
fn compact_history(bucket: usize, cfg: &Config);
Semantics
* Merge subset of runs
* Drop duplicates
* Correctness must not depend on this
STOP WHEN
* Run counts bounded.

[x] TASK 10 — RAM policy (continuous dials)
Inputs
* Global used %
* Headroom bytes
Outputs
fn compute_config(role: Role) -> Config;
Targets
Role    Aggressive below    Conservative above
Leader  65% 85%
Follower    50% 70%
Numbers
* Global RSS drives decisions (both roles).
* run_budget = 0.7 * budget
* fan_in = clamp(budget / read_buf, 8, 128)
* Follower bail if run_budget < 128MB when already on the lowest allowable budget and global RSS stays above the minimum target.
* Targets replace existing memory config/tuning code/docs; remove the old policy once this is wired.
STOP WHEN
* Config changes smoothly.

[x] TASK 11 — Wire integer pipeline into main loop
Loop
while let Some(x) = store.pop_work() {
    let results = process(x);
    for r in results {
        store.record_result(r);
    }
}
store.on_generation_end();
After integer proof, re-run the loop end-to-end with orthos on all stages (landing, runs, history, anti-join).
STOP WHEN
* 2–3 integer generations run.

[x] TASK 12 — TUI + logging (migrate from old architecture)
Objective
Preserve existing TUI visualizations and metrics but source them from the new generational store instead of the old disk-backed queue and tiered seen tracker.
Remove obsolete components
* Delete `render_queue_depth_chart` and `queue_depth_samples` (old disk queue)
* Delete `render_seen_size_chart` and `seen_size_samples` (old seen tracker)
* Delete `render_tracker_panel` and `TrackerMetrics` (old tiered hashset/bloom architecture)
* Remove from GlobalMetrics: `queue_buffer_size`, `num_shards`, `max_shards_in_memory`, `queue_depth_pk`, `seen_size_pk`
Add equivalent visualizations from generational store
* **Work depth chart**: `work_len` over time (replaces queue_depth_samples)
    * Track work queue depth across generations
    * Show current, peak, rate of change
* **Seen growth chart**: `seen_len_accepted` over time (replaces seen_size_samples)
    * Track accepted orthos/integers across generations
    * Show baseline, current, growth rate
* **History/compaction panel**: Per-bucket metrics (replaces tracker panel)
    * Bucket count (power-of-two buckets)
    * Per-bucket: run count, history size estimate, landing size
    * Current bucket being processed (from Phase enum)
    * History compaction stats (merge count, runs merged, if applicable)
Retain existing TUI fields
* generation (new field)
* phase (new field)
* work_len (new field)
* seen_len_accepted (new field)
* run_budget / fan_in (new fields)
Logging
* bucket start/end
* compact stats (runs generated, data volume, time)
* anti-join counts (accepted vs rejected)
* history compaction events
Implementation notes
* Use MetricsSnapshot pattern: sample work_len and seen_len_accepted into VecDeque<MetricSample> just like old queue_depth_samples
* Expose per-bucket stats through GenerationStore API
* Phase enum variants (Compacting{bucket}, AntiJoin{bucket}) drive "current operation" display
STOP WHEN
* TUI shows same richness of data as before, sourced from generational store
* Old disk queue and seen tracker metrics completely removed
* One full generation run is readable in TUI

[x] TASK 13 — Remove old architecture (DiskBackedQueue, SeenTracker)
Objective
Completely eliminate the old disk-backed queue and seen tracker infrastructure now that the generational store is operational.
Remove components
* Delete or gut `src/disk_backed_queue.rs` - replaced by generational store's work segments
* Delete all `seen_tracker_*.rs` implementations - replaced by history store with anti-join
* Remove `src/memory_config.rs` - replaced by Config::compute_config() in Task 10
* Remove old MemoryConfig and MemoryClaim usage from main loop
* Delete checkpoint_manager if it was only for the old queue/tracker
Implementation
* Replace DiskBackedQueue usage in main.rs with GenerationStore work queue (pop_work/push_segments)
* Replace SeenTracker usage with GenerationStore history (anti-join determines novelty)
* Update worker loop to use: store.pop_work() → process → store.record_result() → store.on_generation_end()
* Remove all references to:
    * work_queue.push() / work_queue.pop()
    * tracker.insert() / tracker.contains()
    * memory_config calculations (queue_buffer_size, num_shards, etc.)
* Keep file_handler.rs, ingestion flow, heartbeat mechanism
Tests
* End-to-end test with GenerationStore only
* Verify no imports of deleted modules remain
* Confirm generation transitions work correctly
STOP WHEN
* Old queue/tracker code deleted
* Main loop uses only GenerationStore
* All tests pass with new architecture
* Build has no warnings about unused code from old architecture

[x] TASK 14 — Benchmarks
Benchmarks
* sort throughput vs RAM
* anti-join vs history size
* full generation with duplicates
* Compare performance with old architecture baseline (if measurements exist)
STOP WHEN
* Performance characteristics documented
* No regressions vs old system

[x] TASK 15 — Full Ortho (remove integer bootstrap)
Remove all integer logic from the code
* Check all integer scaffolding and remove it in favor of plain orthos
* Reduce complexity
* Look for places where tests can be adapted from integer to ortho. If they can't be adapted, remove the integer tests
* Remove i64/u64 codepaths from:
    * record_result (only accept Ortho)
    * landing zone (only serialize Ortho)
    * compact_landing (only sort Ortho)
    * anti_join (only compare Ortho by id)
    * history_iter (only yield Ortho)
STOP WHEN
* integer logic has been removed, results in a complexity reduction and speedup
* All operations work end-to-end with Ortho only

[x] TASK 16 — Documentation
* Document store lifecycle using annotated ascii art in a md document
* Document main flow using annotated ascii art in a md document
* add detail about binary heap handling and how that does not get too large for RAM
STOP WHEN
* all md documentation is up to date

FINAL AGENT REMINDER
Only work on the first unchecked task.Check it off.STOP.
If you want, next I can compress this to a literal 1-page system prompt for an autonomous executor.

