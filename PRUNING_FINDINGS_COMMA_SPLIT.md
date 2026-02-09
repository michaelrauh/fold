# Finding: Comma-Splitting Collapses Phrase Depth and Starves Search

## Hypothesis
The splitter treats commas as sentence boundaries, so comma-heavy prose (like the 67k-word book) is shredded into 1–3 word “sentences.” That caps `prefix_stats` (max_desc_len) at ~1–3 for most tokens, which in turn:
- Forces very small `axis_totals` into pruning bounds.
- Produces a tiny best score `(2,5)` and stops after ~5 generations (no new work).

## Evidence
- New unit test `commas_shrink_phrase_depth_drastically` (in `src/interner.rs`) shows:
  - Continuous text “a b c d e f g” → `prefix_stats([a]) == 7`.
  - Comma-split text “a, b, c, d, e f g” → `prefix_stats([a]) == 1` (only single-word “sentences”); only the tail chunk keeps depth 3.
- Your latest `temp_history.txt` run still halts at generation 4/5 with best `(2,5)` and only ~8.7M orthos, far below historical runs in `stats.txt` that reached `(7,15)` with billions of orthos.

## Why this starves the search
- `prefix_stats` drives `axis_totals`, which drive `upper_bound_score`. If `max_desc_len` is near 1–3, the optimistic bound is tiny; completions and seeds look hopeless, and anti-join quickly exhausts new work.
- Longer structures (needed for higher volume/fullness) become impossible because the interner never records long prefixes once commas split sentences apart.

## Next steps (if we choose to change behavior)
- Revisit `Splitter::split_into_sentences`: stop treating commas (and maybe semicolons) as hard sentence breaks; keep commas as in-sentence tokens or soft delimiters.
- Re-run the 67k ingest with commas preserved to confirm generation depth recovers and best score climbs.

## Files touched for validation
- `src/interner.rs` — added unit test `commas_shrink_phrase_depth_drastically` demonstrating the depth collapse.
