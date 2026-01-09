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
- New helper (shared by ingest + merge): `fn bound_completion(ortho, completion, interner, best_score) -> Option<MaxPotential>` where `MaxPotential` is an *upper bound* `(volume, fullness)` the branch could ever reach.
- Inputs:
  - `required` prefixes from `ortho.get_requirements()`.
  - Candidate `completion` (usize).
  - Current `best_score` (global optimal from metrics).
- Steps:
1) Build per-axis prefixes after placement: for each required prefix `p`, form `p + completion`.
2) Fetch `PrefixStats` for each `p+completion`. These should always exist for completions; if they don’t (corrupt/intermediate interner), skip pruning for that candidate and log once.
3) Derive per-axis totals:
    - For each `p+completion`, fetch `max_desc_len` (total optimistic length including the new token). Missing stats should panic rather than silently skip.
    - Empty-root special case (no requirements): only consider candidates whose single-token prefix has span > 1; single-span candidates are pruned outright at the root.
4) Bound shape potential from axis totals:
    - Compute an upper-bound excess volume as the saturated product of `(max_desc_len - 1)` across required prefixes (max with current excess volume). This matches the scoring definition (∏(dim−1)).
    - Fullness upper bound can be taken as `excess_volume_upper` (safe over-approximation).
5) If that upper bound cannot beat `best_score`, drop the completion; otherwise proceed.
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

Metric ideas for clarity
- Depth-weighted pruning: weight pruned counts by generation/depth so high-up prunes are visible.
- Show per-gen prune ratio: surface pruned/expanded per generation alongside absolute counts.
- Track “work saved”: estimate avoided expansions from prunes (e.g., sum of axis products) to quantify impact beyond counts.

Implementation tasks (for another LLM; vertical slices with done checks)
- [x] Interner depth stats  
  - Add `HashMap<Vec<usize>, usize>` storing `max_desc_len`; serialize/deserialize and version bump. Done check: interner round-trips with new stats and tests cover build/add/merge paths.
- [x] Bound helper  
  - Implement `bound_completion` using depth-only budgets; prune when optimistic bound cannot beat best score. Done check: unit test that known optimal branches are not pruned and at least one branch is.
- [x] Apply pruning in loops  
  - Wire `bound_completion` into ingest and merge loops before enqueuing children. Done check: integration test shows reduced completions processed with same optimal ortho.
- [x] Impacted seeding pruning  
  - Use `max_desc_len` to skip requeueing impacted prefixes that cannot beat the best archive score; ensure merge seeding uses the same bound logic. Done check: impacted seeding filters out hopeless prefixes in tests.
- [x] Metrics/telemetry  
  - Add counts for pruned vs expanded completions. Done check: metrics snapshot includes these fields and they populate in a dry run.
- [x] Integration tests for prune paths  
  - Add coverage for merge ingest and impacted seeding using the new depth stats, confirming optimal ortho is unchanged and completions processed decrease. Done check: integration test exercising merge + seeding passes with pruning enabled.
- [x] TUI surfacing  
  - Show pruned vs expanded completion counts (and ratio) in the TUI header so operators can see pruning effectiveness in real time. Done check: TUI renders the counts/ratio from metrics.

Notes / watchouts
- Empty-ortho fanout: when starting with no best score and a very large vocabulary, root fanout can explode because nothing prunes; consider seeding a baseline best or capping root fanout if this becomes a problem.
- IDDFS/LIFO tuning: merge ingest is already close to LIFO; revisit full generation caps / strict IDDFS only if fanout/pruning metrics indicate need.
- Prune-driven cleanup: consider dropping archival results that become irrelevant under new pruning/optimal scores when scanning impacted prefixes; non-impacted orthos might never matter once a stronger optimal is found.
- Compaction pass (when best improves): after merge generations, if the best score changed, stream history runs bucket-by-bucket and re-write only orthos whose optimistic bound beats the new best. Use a temp file per run and rename after filtering so disk never more than ~1× a single run. Keep the current optimal even on ties; skip if best_score is zero.
- Symmetric pre-prune: before/while merging, load each archive’s optimal score; prune impacted seeds using the higher of the two best scores so the weaker side is filtered by the stronger best. If the bests tie, no extra pruning beyond normal bounds. Optionally pre-compact only the weaker side with the higher best to avoid rewriting the stronger archive.
