# Pruning Regression: Early Termination on 8k Run

## What’s happening
- Recent 67 k‑word run (`e.txt`) stops after generation 4 with best ortho `dims=[2,3]`, `score=(2,5)`, `ortho_count≈8.7M` (see `src/temp_history.txt`).
- Historic runs on the same corpus (e.g., 6.7 k words in `stats.txt`) ran 25–27 generations, reached `dims=[2,8]` (`score=(7,15)`), and produced 1.2 B+ orthos.
- Pruning ratios in the short run are modest (≈10–13%), so the stop isn’t explained by aggressive root-span pruning alone.

## Suspected root cause (code-level)
- `upper_bound_score` multiplies only the provided `axis_totals` and **ignores axes that have an empty prefix**.
  - `Ortho::get_requirements` drops empty prefixes (`filter(|p| !p.is_empty())`), so when the current cell sits at coordinate 0 on any axis, that axis is **omitted** from `axis_totals`.
  - `upper_bound_score` then iterates `axis_totals.iter().take(dim_count)`, so missing axes are simply skipped.
- Consequence: the bound assumes no remaining capacity/volume on those axes. Example:
  - `axis_totals=[3]`, `min_volume=2`, `min_fullness=2`, `dim_count=2` → current bound returns `(2,3)`.
  - Even the minimum feasible capacity with the missing axis (len ≥ 2) would be `(volume=2, fullness=6)`, so any `best_score=(2,4)` would wrongly prune this live branch.
- This under-bound appears exactly when the search is shallow (many coordinates still at 0), which matches the generation‑4 stop with a tiny best ortho.

## Evidence from logs
- `src/temp_history.txt` run (67 k words): generations 0–4 only, best `(2,5)`.
- `stats.txt` run (6.7 k words): generations 0–27, best `(7,15)`, showing the corpus can yield much larger shapes.
- Prune counts in the failing run (gens 2–4: 9–13%) aren’t high enough to empty the queue; rather, the bound drives `new_work` to zero by declaring most branches hopeless.

## Recommended fix direction
- When `axis_totals.len() < dim_count`, include placeholders for the missing axes instead of ignoring them. Options, from most context-sensitive to most optimistic:
  1) **Use the candidate token’s own depth for missing axes (preferred):** for each missing axis, set its total to `interner.prefix_stats(&[completion])` (the max_desc_len of the single-token prefix). This is optimistic but tied to the actual token, not the whole corpus.
  2) Use current `dims[i]` for missing axes (safe but pessimistic).
  3) Use a corpus-wide `max_desc_len` (very optimistic; can add work).
- This keeps the bound an *upper* bound and prevents pruning branches whose growth depends on axes that haven’t been touched yet.

## Testing ideas
- Unit: construct a scenario with `axis_totals` shorter than `dim_count` and assert the bound stays ≥ a minimal capacity that assumes length 2 on missing axes.
- Integration: ingest a small corpus where optimal volume > current best but requires growth on an untouched axis; assert `bound_completion` doesn’t prune that path.

## Next steps
- Decide fallback policy for missing axes and adjust `upper_bound_score` (plus callers/tests).
- Add a unit test capturing the missing-axis case to prevent regressions.
- Re-run the 8k ingest after the bound fix to confirm generations extend past 4.

## Worked example (context-sensitive fallback)
Corpus: 3×3 grid “a b c / d e f / g h i” (each row is a phrase). Insert order follows shell distance, not row-major: (0,0) → (0,1) → (1,0) → (0,2) → (1,1) → (2,0) → (1,2) → (2,1) → (2,2).

Step 1 — place `a` at (0,0): requirements empty; no bound.

Step 2 — candidate `b` at (0,1): requirements include row prefix `[a]`; column prefix is empty, so it’s omitted. `axis_totals=[len([a,b,c])]=[3]`, `dim_count=2`.
- Buggy bound: missing column treated as length 1 ⇒ `(volume=2, fullness=3)`; prunes if best is `(2,4)`.
- Context-sensitive fallback: missing column uses `max_desc_len([b])` (column phrase is `b e h`, so 3). Bound becomes `(volume=(3-1)*(3-1)=4, fullness=3*3=9)`; branch is kept.

Step 3 — candidate `c` at (0,2): column still empty, fallback again with `max_desc_len([c])=3`; optimistic bound preserved.

As we continue filling the shell, each axis eventually has a real prefix and no fallback is needed. This approach only prunes when the token itself cannot grow (single-token max_desc_len = 1) and remains optimistic otherwise, avoiding live-branch pruning.
