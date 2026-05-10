# Current run: methodology and runtime estimate

## TL;DR

At the throughput and pruning quality observed on **2026-05-09**, the live
parallel search has an all-in remaining runtime of **~14 years**, with a
defensible 90% interval of **5–25 years**.

This estimate is built up from two independent measurements that converge:
direct path-progress measurement of the four currently-active deep shards,
and the same technique applied to a stratified sample of pending heavies.
Both methods agree that a typical "heavy" shard requires ~230–300 B nodes
to complete; with ~12,000 such shards in the pending queue and 4 workers
sustaining ~6.78 M nodes/sec aggregate, the arithmetic forces multi-year
horizons under the current algorithm and hardware.

The estimate replaces three earlier, less-reliable numbers that this
methodology was specifically designed to refute or supersede:

| earlier estimate | source | what was wrong |
|------------------|--------|----------------|
| ~12 days | linear extrapolation of 87 shards/h average | first-day rate dominated by easy shards; not steady-state |
| ~21 days | L01 dial historical rate | L01 ticked during the easy-shard phase; rate has since collapsed to 0/h |
| ~260 days | replay of currently-active shards at 1 B steps | survivorship bias: those 4 are the worst-case tail, not representative |

## Run state at time of measurement

| field | value |
|-------|-------|
| Run started (`started_unix`) | `1778285929` (2026-05-08, ~23:38 UTC) |
| Latest checkpoint observed (`T4`) | `1778371062` (2026-05-09, ~22:51 UTC) |
| Elapsed at T4 | 23.65 h |
| Workers | 4 |
| Shard depth | 3 |
| Total shards | 32,252 |
| Shards done | 2,057 (6.4%) |
| Shards running | 4 |
| Shards pending | 30,191 |
| `nodes_expanded` total | 595.64 B |
| Aggregate throughput | 6.78 M nodes/sec |
| Per-worker throughput | ~1.7 M nodes/sec |
| Best incumbent | `vol=2`, `fullness=16`, `dims=[2,2,2,3]` |
| `frontier_max_bound` | `vol=31,850,496`, `fullness=40,353,607` |
| Effective prune floor | `vol=8`, `fullness=27` (hardcoded in `parallel_search.rs:1646`) |

The four active shards together account for **496.2 B of the 595.6 B nodes
expanded so far (83%)**. The 2,057 done shards contributed only 99.4 B —
an average of 48.3 M nodes per done shard. The work distribution is
strongly bimodal: easy shards finish near-instantly, deep shards consume
hundreds of billions of nodes.

## Methodology

### Stage 1 — Live polling for steady-state rate

A 60-second poller (`/tmp/fold_poll.sh`) was started against the droplet,
appending `(checkpoint_unix, shards_done, nodes_expanded)` to a CSV. After
2.16 hours of polling, **zero new shards completed**, despite sustained
~7 M nodes/sec throughput. This established that the apparent shards/h
rate from the run's first day was not steady-state and could not be
extrapolated forward.

### Stage 2 — Stratified pending sampling at increasing budget

A snapshot of `state.bin` was copied off the droplet. A new estimator
binary (`src/bin/estimate_parallel.rs`) was extended to dump per-running-shard
state, per-sample `path_progress_by_depth`, and per-level monotonic counters
(`completed_seen_by_depth`, `level_completions`). The sample loop was
parallelized with `std::thread::scope` (8 cores → ~6× speedup over the
original sequential implementation).

Three sampling passes were run, all with `--no-running` (pending only):

| pass | budget per sample | samples | finished | censored | wall time |
|------|-------------------|---------|----------|----------|-----------|
| pass 1 | 10 M | 45 (1/bucket × 45) | 22 | 23 | ~1 min |
| pass 2 | 200 M | 90 (2/bucket × 45) | 53 | 37 (41.1%) | ~30 min |
| pass 3 | 5 B | 90 (2/bucket × 45) | 54 | 36 (40.0%) | ~70 min |

The crucial finding from pass 2 vs. pass 3: bumping the budget 25× (200 M
to 5 B) moved **only 1 sample** from "censored" to "finished". This
established that the heavy distribution has **no moderate band**:
once a shard takes ≥200 M, it almost always takes ≥5 B.

The bimodality:

- **60% of pending shards are essentially trivial** — 43 of 54 finished
  samples in pass 3 finished in <1k nodes (sub-millisecond CPU time).
- **40% of pending shards are heavy** — at least 5 B nodes each, with the
  upper tail extending well beyond.

### Stage 3 — Active-shard down-tree progress measurement

Four state snapshots (`T1`, `T2`, `T3`, `T4`) were captured at 815 s,
1328 s, and 1208 s intervals. For each snapshot, the new
`running_shard_states` field gives per-active-shard `path_progress_by_depth`
— the `(processed, total)` counts at every frame in the runner's stack.

The fraction-of-shard-completed at any frame is given by the recursive
formula:

```
frac(d) = (proc[d] - 1) / total[d] + (1 / total[d]) × frac(d + 1)
```

(where `(proc - 1)` branches are *fully* done and `1` is the in-progress
branch whose interior progress comes from depth `d+1`).

Applied to T4, with the shard root at depth `d = 2`:

| shard | nodes done | path-progress fraction | extrapolated total |
|-------|------------|------------------------|--------------------|
| 15710 | 128.2 B | 0.876 | **146.3 B** |
| 15709 | 141.9 B | 0.643 | **220.5 B** |
| 15713 | 67.6 B | 0.290 | **233.3 B** |
| 15712 | 158.5 B | 0.474 | **334.5 B** |
| **mean** | | | **233.6 B / shard** |

### Stage 4 — Apply the same technique to pending heavies

Each of the 36 censored 5 B-budget samples carries a final
`path_progress_by_depth` showing where the sample's runner ended. Filtering
to the 21 samples with `proc ≥ 2` at the shard-root frame `d=2` (i.e.,
those that completed at least one root-child subtree, giving a clean
measurement):

| | value |
|--|-------|
| min | 13 B |
| p25 | ~70 B |
| median | ~220 B |
| p75 | ~480 B |
| max | 2,048 B |
| **mean** | **~250 B** |

The remaining 15 samples (`proc = 1` at d=2 — still in their first
root-child) give recursion-blow-up estimates that are *upper bounds* under
the uniform-sibling-cost assumption (which is too pessimistic for an
unbalanced tree).

The two methods (active-shard ground truth and pending-sample
extrapolation) **converge on ~230–300 B nodes per heavy shard**.

## All-in estimate

| component | nodes | wall time @ 6.78 M/s |
|-----------|-------|----------------------|
| Active 4 remaining (sum of `nodes × (1−frac)/frac`) | 0.44 T | ~0.8 d |
| Pending easy (18,115 × 48 M done-shard avg) | 0.87 T | ~1.5 d |
| Pending heavy (12,076 × 250 B central) | **3,019 T = 3.02 P** | **~14.1 y** |
| **Total remaining** | **~3.02 P nodes** | **~14 years** |

### Sensitivity to per-heavy size

The largest source of remaining uncertainty is the mean per-heavy size.
Sensitivity analysis:

| if heavy avg = | total wall time |
|----------------|-----------------|
| 100 B (more optimistic than the data supports) | ~5.6 y |
| 200 B | ~11.2 y |
| **250 B (central, calibrated to two methods)** | **~14.0 y** |
| 400 B (allows for tail outliers) | ~22.4 y |

Defensible 90% interval: **5–25 years**.

## Caveats

1. **Throughput is assumed constant**. The aggregate 6.78 M/s was measured
   during a deep-shard phase; if pending heavies cause similar deep DFS
   patterns, this rate is sustainable. If they trigger different cache or
   memory behavior, the rate could drift up or down.
2. **Pending heavies are assumed similar to active 4 in distribution**.
   Validated empirically (the path-progress estimates from pending samples
   with `proc ≥ 2` produce a mean within the active-4 range), but the
   sample is small (n=21).
3. **Recursive fraction formula compounds for `proc=1` samples**.
   ~15 of 36 censored samples returned huge upper-bound estimates because
   the formula multiplies tiny fractions across many depths. These are
   *upper bounds*; the true per-shard cost is between 5 B (sample budget)
   and the recursion estimate. They were excluded from the central-mean
   calculation but the population still contains some genuinely
   pathological tail shards beyond the active 4.
4. **Easy-shard size estimated from done-shard average (48 M)**.
   The 2,057 completed shards averaged 48 M nodes each; pending easies are
   assumed to behave similarly. Easy shards contribute negligibly (~1.5 d
   total wall time) so this assumption has minimal leverage.
5. **The bound function is loose**. The current optimistic-bound function
   contributes to the slow runtime; if it were tightened (see
   `BOUNDS_TIGHTENING_DESIGN.md`), the estimate would shift downward
   without any other change.

## What this estimate means in practice

The 14-year central figure is far beyond any reasonable horizon for the
current setup. The arithmetic is:

- **30,000 shards** × **8% deep rate** × **38 worker-hours/deep** (= 230 B / 1.7 M/s) = 91,200 worker-hours
- **/4 workers** = 22,800 wall hours = **2.6 years just on the deep tail**

(The 14-year figure includes the much-larger non-active heavy population
implied by the 40% censoring rate; the 2.6-year figure above is just a
sanity check using the empirical 8% "active-grade" rate within bucket 3.)

The conclusion is that **brute-force single-pass branch-and-bound at the
current pruning quality is not the right algorithm** for this problem.
Three orthogonal approaches to recover the regime where this run finishes
in *weeks* are documented separately:

- `INCUMBENT_BOOTSTRAP_DESIGN.md` — find a strong incumbent via random or
  MCTS rollouts before searching, multiplying every prune's effectiveness.
- `DUTCH_AUCTION_FLOOR_DESIGN.md` — assert a high synthetic prune floor and
  geometrically lower it; each pass exploits the exponential
  pruning-near-the-top of the BnB tree.
- `BOUNDS_TIGHTENING_DESIGN.md` — five specific changes in the bound
  function to remove visible slack (10–100× tightening, ~30 LoC).

These three are multiplicative, not additive. Together they plausibly
reduce the runtime by 4–6 orders of magnitude, putting completion in days
to weeks rather than years.

## Reproducibility

All measurements are reproducible from the artifacts on the local machine:

| artifact | contents |
|----------|----------|
| `/tmp/fold_poll.csv` | 60s poll of `manifest.json` |
| `/tmp/fold_state_remote/` | T1 state snapshot |
| `/tmp/fold_state_t2/` | T2 state snapshot (T1 + 815 s) |
| `/tmp/fold_state_t3/` | T3 state snapshot (T2 + 1328 s) |
| `/tmp/fold_state_t4/` | T4 state snapshot (T3 + 1208 s) |
| `/tmp/fold_est_bench.json` | 10 M-budget pending sample |
| `/tmp/fold_est_p2.json` | 200 M-budget pending sample |
| `/tmp/fold_est_2b.json` | 2 B-budget pending sample |
| `/tmp/fold_est_5b.json` | 5 B-budget pending sample with path_progress |
| `/tmp/fold_treeprog_*.json` | Per-snapshot running-shard states with path_progress |

Source modifications used to gather the data:

- Added `running_shard_states: Vec<RunningShardState>` to
  `ParallelEstimateReport` with per-shard path_progress and seen-by-depth
  data (`src/parallel_search.rs:259-280`).
- Added `final_path_progress_by_depth`, `final_open_siblings_by_depth`,
  `final_top_bound_volume`, `final_frontier_max_bound_volume` to
  `ShardSampleReport` (`src/parallel_search.rs:298-302`).
- Parallelized the sample loop in `estimate_parallel_checkpoint` with
  `std::thread::scope` and an atomic work-stealing index (~50 LoC, no
  behavioral change beyond ordering).

These changes affect only the offline estimator binary; the live `fold`
binary on the droplet is unchanged.
