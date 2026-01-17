Backwards/Goal-Driven Search Ideas
==================================

Context
- Optimal is bounded by the span/depth of the first insertion and each subsequent insertion.
- Forward BFS/DFS writes huge intermediate runs; pruning helps but IO/space still explode.
- We have `prefix_stats.max_desc_len` (per-axis total length after adding a token) and the current best score/shape.

Approaches to reduce intermediates by starting from the end / goal:

1) Root shortlist by potential (forward but pre-filtered)
- Compute optimistic volume for each root completion using `max_desc_len` on empty prefixes; sort by potential volume and only seed the top-K.
- Use a hard bound: drop any root whose max potential volume < current best or < a target volume threshold.
- Cost: low (one interner pass per root); Benefit: cuts root fanout and generations that can never reach optimal.

2) Goal-shape filtering (start from desired dims)
- Given a target volume/shape (e.g., best-so-far dims), filter candidates that can reach those dims:
  - For each axis, require `max_desc_len >= target_dim`.
  - For the root, require branching span >= target width.
- Generate work only for roots/prefixes that satisfy these per-axis minima; skip writing runs for the rest.
- Cost: low-medium (extra checks before enqueuing); Benefit: avoids intermediates that cannot reach the goal shape.

3) Backward fill from deepest position (reverse construction)
- Treat the ortho as a grid of dims (best-so-far or candidate dims).
- Starting from the last slot (max depth on each axis), pick suffix tokens whose prefixes can exist (`max_desc_len` covers remaining depth).
- Move backward, intersecting feasible prefixes per axis; only emit a root when all suffix constraints are satisfied.
- Effectively a constraint-propagation/DFS from the end; avoids writing early generations that will be invalidated later.
- Cost: medium-high (new search order + constraint solver); Benefit: largest reduction in intermediates if feasible sets stay small.

4) Two-phase generation (filter then expand)
- Phase 1: offline enumerate feasible prefix chains using `max_desc_len` only (no payload building), up to a depth/volume bound; drop infeasible chains early.
- Phase 2: actually construct orthos only for feasible chains from phase 1.
- Cost: medium (extra pass) and memory for feasible sets; Benefit: skips writing/running generations for infeasible branches.

5) Beam/top-K per position (bounded width)
- At each depth, keep only the top-N prefixes by optimistic volume (using `max_desc_len` products), discard the rest.
- This approximates “start from the end” by keeping only branches that can plausibly reach large shapes.
- Cost: low-medium; Benefit: caps intermediates; Risk: can prune true optimal if N too small (needs guardrails).

6) Deferred bound for non-root, aggressive root pruning
- Keep only the root span prune inline; defer bound checks until a generation boundary.
- At gen end, prune runs against best-so-far (or target dims) before seeding next gen.
- Cost: low; Benefit: moves bound cost out of hot loop; Risk: more intermediates within a gen (more IO/mem).

Heuristic: prioritize richer areas (high span × high depth)
- Idea: Stay forward but bias search order toward completions/prefixes with high potential (branching × depth), so good shapes surface sooner and later pruning is more effective.
- Scoring: use optimistic excess-volume = product of `(min(max_desc_len, target_dim) - 1)` across axes; penalize single-span chains (span == 1) even if deep.
- Root: sort root completions by this score and process top-N first; drop or defer low-score roots.
- Non-root: when iterating completions for a prefix, sort by potential score and process richer branches first; optionally use a small beam per node.
- Work queue: switch to a priority queue keyed by potential score instead of FIFO/LIFO; or maintain a “rich first” sub-queue for top candidates.
- Code touch points:
  - `generation_runner` completion loop: compute potential per completion (reuse `upper_bound_score` with target dims cap) and sort/partition.
  - `GenerationStore` pop policy: allow a max-heap of work items with cached potential scores.
  - Metrics: track how many items processed from the rich queue vs deferred queue; surface in TUI.
- Risk: sorting overhead per fanout; with very loose `max_desc_len`, many items will look rich unless capped by target dims/span.

What changes to code for a backward fill (reverse/constraint-based) search?
------------------------------------------------------------
- Represent a target shape: use best-so-far dims/volume as the initial goal. Carry both volume and per-axis lengths.
- Add a backward solver:
  - Input: target dims, interner with `max_desc_len`, vocabulary.
  - Start from the deepest position on each axis; for each axis, gather tokens whose `max_desc_len` covers the remaining depth (including the token).
  - Walk backward (depth-first), intersecting feasible token sets across axes; maintain a partial ortho payload/dims consistent with the target.
  - When all positions satisfied, emit the root ortho(s).
- Integrate into the generation loop:
  - Before normal forward generation, run the backward solver to emit a seed set (roots) instead of seeding every completion; or replace forward search entirely with backward enumeration → forward verification.
  - Keep root span prune inline; optionally keep bound checks off in-loop if backward seeds are already constrained.
- Interner helpers:
  - Fast query for “tokens that can reach depth >= d” per prefix: precompute/map `max_desc_len` ≥ threshold.
  - Optional: cache feasible suffix sets per depth to avoid recomputing.
- Ortho construction:
  - Need a way to materialize an `Ortho` from a backward-built assignment (fills payload in reverse). Add a constructor that takes dims + payload vector directly, or use existing `Ortho::new()` + `add` with reversed order.
- Metrics/telemetry:
  - Track how many seeds come from backward solver vs forward; surface in TUI/logs.
- Error handling/resume:
  - If backward solver finds no seeds for a target, fall back to forward search or relax target dims (e.g., shrink one axis) and retry.

Complexity/risk:
- New search path; needs correctness validation that backward-generated roots can still reach optimal via forward expansion (or directly produce optimal).
- With loose `max_desc_len`, feasible sets may remain large; may need additional constraints (span minima, beam width) to keep enumeration tractable.

Notes / integration hooks
- `prefix_stats.max_desc_len` gives total optimistic length including the new token; use products of `(len-1)` as the excess-volume estimate.
- Span at root remains critical: enforce span > 1 and/or min branching to align with target dims.
- To make runs resumable: write per-generation checkpoints (best score, work queue state) so a failed backward/goal search can resume without re-running filtered generations.
