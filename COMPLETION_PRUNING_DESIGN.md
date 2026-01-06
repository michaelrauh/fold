# Completion Pruning & Search Order Design

Goals
- Trim fanout by skipping completions that cannot beat the current best score.
- Use the interner to bound “depth” (longest reachable suffix) so we can estimate best-case outcomes before enqueuing work.
- Switch large ingests from BFS to DFS/IDDFS so we surface strong candidates earlier and make the bound meaningful.
- Apply the pruning in both generation paths: normal ingest (`src/generation_runner.rs`) and merge (`src/main.rs::merge_archives`), plus during impacted-key seeding.

Current Flow (baseline)
- Completions come from `interner.intersect(required, forbidden)` inside the generation loop (`src/generation_runner.rs`, ~246) and the merge loop (`src/main.rs`, ~1038). Every completion is expanded via `Ortho::add` and recorded to the store.
- The interner only stores prefix→completions bitsets; no span/depth metadata. `intersect` can’t say how long a prefix can continue.
- The work queue in `GenerationStore` is FIFO, so search is effectively BFS. On large inputs BFS finds good shapes late, so pruning by “best so far” is weak.
- Merge seeding uses `Interner::impacted_keys` + `is_ortho_impacted_fast` to decide what to put back on the work queue; everything impacted is reprocessed.

Interner additions (depth)
- Add a `HashMap<Vec<usize>, usize>` (prefix → max_desc_len), where `max_desc_len` is the length of the longest phrase containing the prefix (including the prefix itself).
- Compute stats during `build_prefix_to_completions`:
  - While iterating phrases, for each prefix slice `prefix[..k]`, set `max_desc_len[prefix[..k]] = max(existing, phrase.len())`.
- Persist stats:
  - Extend `InternerSerializable` to include `prefix_stats` alongside `prefix_to_completions`.
  - Update `add_text` and `merge` to merge stats (max of `max_desc_len`).
- API surface:
  - `fn prefix_stats(&self, prefix: &[usize]) -> Option<PrefixStats>`
  - `fn max_suffix_depth(&self, prefix: &[usize]) -> Option<usize>` returning `max_desc_len.saturating_sub(prefix.len())`.
  - Keep `prefix_entries()` for exporters, but add lightweight queries so the generation loop stays cheap.

Bounding a completion’s potential
- New helper (shared by ingest + merge): `fn bound_completion(ortho, completion, interner, best_score) -> Option<MaxPotential>` where `MaxPotential` is an optimistic `(volume, fullness)` the branch could ever reach.
- Inputs:
  - `required` prefixes from `ortho.get_requirements()`.
  - Candidate `completion` (usize).
  - Current `best_score` (global optimal from metrics).
- Steps:
  1) Build per-axis prefixes after placement: for each required prefix `p`, form `p + completion`.
 2) Fetch `PrefixStats` for each `p+completion`. These should always exist for completions; if they don’t (corrupt/intermediate interner), skip pruning for that candidate and log once.
 3) Derive per-axis budgets:
     - `axis_depth = stats.max_desc_len - (p_len+1)` (suffix tokens available after placing `completion`), clamped ≥0.
 4) Bound shape potential from total length:
     - For each `p+completion`, get `max_desc_len`; remaining budget for that axis = `max_desc_len - (p_len+1)`.
     - Take the minimum remaining budget across all required prefixes (tightest chain).
     - Potential total tokens after taking this completion = `current_fullness + 1 + min_remaining_budget`.
     - Compute the minimal shape that could contain that many tokens given the expansion rules (derive dims/volume directly from required length; no simulated expand-over/expand-up steps). The excess volume of that shape is the optimistic bound.
 5) If that optimistic bound cannot beat `best_score`, drop the completion; otherwise proceed.
- Metrics: count pruned completions and record the worst-case bound that still beat `best_score` (for tuning thresholds).

Pruning sites
- Generation (ingest): in `run_generation_loop`, wrap the `for completion in completions` block with the bound check. Maintain counters so the TUI can show “pruned vs expanded” fanout per gen.
- Merge generations: apply the same bound in the merge loop in `merge_archives` so impacted work produced from archives is also trimmed.
- Impacted seeding: when `Interner::impacted_keys` returns prefixes to requeue, filter them with the same bound idea:
  - For each impacted prefix, use its `max_desc_len` to estimate how far the prefix can extend.
  - If even the optimistic score from that prefix (assuming best-case fills) cannot beat the archive’s best score, skip seeding work that only depends on that prefix.
  - Keep a flag to disable this during debugging to avoid accidentally skipping needed recomputations.

Search order: force IDDFS via generation caps
- Adjust `GenerationStore::pop_work` to pop from the back of the cache (`VecDeque::pop_back`) so the most recently enqueued work is processed first (LIFO) within a generation.
- `push_segments` currently appends to `work_segment_batch` and later writes segments in sequence; ensure `refill_work_cache` pulls the newest segment first by switching `work_segments.remove(0)` to a pop from the end.
- Add a generation cap (by item count and/or wall-clock) so deep descendants don’t spill into a later generation; this effectively yields IDDFS: process all work up to a depth/limit, run the transition, then continue deeper in the next capped pass.
- Impact: IDDFS should surface high-scoring branches earlier and make the pruning bound more effective. Risk: too-small caps could cause overhead from frequent transitions; too-large caps approximates BFS again. Tune caps based on fanout/throughput, and keep logging fanout/pruned counts and best-score improvements per generation to confirm the benefit.

Testing & validation
- Unit: `Interner` stats computation on small corpora; depth math on prefixes with and without completions.
- Property: `bound_completion` never prunes a completion that can produce the known optimal in canned fixtures.
- Integration: run ingest/merge on small archives with pruning on/off and confirm identical optimal ortho + fewer expansions; assert pruned counts > 0.
- Performance: benchmark fanout-heavy corpora to show reduction in completions processed and time-to-best with DFS/IDDFS.

Implementation tasks (for another LLM; vertical slices with done checks)
1) Interner depth stats
   - Add `HashMap<Vec<usize>, usize>` storing `max_desc_len`; serialize/deserialize and version bump. Done check: interner round-trips with new stats and tests cover build/add/merge paths.
2) Bound helper
   - Implement `bound_completion` using depth-only budgets; prune when optimistic bound cannot beat best score. Done check: unit test that known optimal branches are not pruned and at least one branch is.
3) Generation IDDFS
   - Switch `pop_work` to LIFO, read newest segments first, and add generation caps to enforce IDDFS behavior. Done check: generation loop runs with caps, and metrics show bounded fanout/generation sizes.
4) Apply pruning in loops
   - Wire `bound_completion` into ingest and merge loops before enqueuing children. Done check: integration test shows reduced completions processed with same optimal ortho.
5) Metrics/telemetry
   - Add counts for pruned vs expanded completions and generation cap hits. Done check: metrics snapshot includes these fields and they populate in a dry run.
