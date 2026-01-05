GT/Interner & Ortho Plan
========================

Current state (done)
--------------------
- Interner export: `data/interner_export/` contains `interner_index.json` + `interner_orders_XXXX.bin` sidecars + `interner_keys_XXXX.json` chunks (word buckets, 500-word stride).
- GT InternerModel: loads index/sidecars, shows keys with #, Δ, keys≥1, bucket +/- and min-length filter; counts/delta respect bucket and length; completions filtered by cutoff.
- Data hygiene: exports copied out of `target/` and gitignored.
- Substring filters for keys/completions exist in the interner UI.

Linear plan
-----------
1) Ortho export (no GT intersects):
   - For small strides (e.g., 500, 1000 words), export ortho archives per stride:
     ortho_id, required_keys (decoded), candidates [{word, min_input, child_id}], optional forbidden/summary.
   - Optional sidecars: id→offset index if archives are large.
2) OrthoModel (GT):
   - Load one stride’s ortho archive; show required keys, candidates (min_input shown), click word → load child ortho by child_id.
   - Add stride selector (500/1000) to swap archives.
   - Share cutoff/bucket with interner (or pass explicitly) so candidates are aligned to stride.
   - Add Lepiter examples for ortho navigation.
3) Cross-links (interner ↔ ortho):
   - Make required keys in Ortho view clickable → select in InternerModel.
   - Show interner context (counts/Δ/length) for ortho’s required keys.
   - Optionally highlight if selected interner key is part of current ortho.
4) Reverse lookup (words → keys):
   - Export inverted index: `word -> {counts_by_bucket, chunk of keys+first_word_pos}`, with sidecar orders.
   - Add ReverseInternerModel view: list words with #keys/Δ, detail keys containing word (filtered by cutoff).
   - Integrate with shared controls and link from Ortho candidates if desired.
5) Runtime/stats models (optional):
   - Build GT views over stats.txt/multistats.txt for timing/growth trends.
6) Comprehensive Lepiter docs:
   - End-to-end pages: data formats/paths, interner views, ortho explorer, cross-links, examples.

Ortho expansions (all children)
-------------------------------
- Export change: in `export_data.rs` emit one entry per child (loop all `ortho.add(...)` results) so `candidates` is a flat list of {word, min_input, child_id} for every expansion, not just the first.
- GT change: existing Ortho view already lists `candidates`; no code change needed unless you want to rename the header (“Expansions”) or add grouping/sorting. Each row will show duplicated words with distinct child_ids.

Pruning exploration (prefix → long key reachability)
----------------------------------------------------
- Export addition: for each prefix/key, compute `max_descendant_len` (longest key that has this prefix; include itself) and optionally a small histogram of descendant lengths.
- GT: add a “min descendant length” slider to the interner view; show only keys with `max_descendant_len >= slider`, and summarize surviving keys/completions. Optionally annotate required keys in Ortho with whether they survive the threshold.

Pruning Exploration (branching factor)
----------------------------------------------------
(flesh this out more)

Extract generation loop for reuse (export & tooling)
----------------------------------------------------
1) Map dependencies: list what the current generation loop in `main.rs` captures (Interner, GenerationStore, Metrics handle, Role/config, mem_claim, file_handler helpers, ingestion/merge metadata, best_ortho/best_score, optimal_dirty).
2) Factor ingest/merge setup: move “ingest text” and “ingest merge” into helpers that return (interner, store, lineage/meta, config, role, mem_claim, metrics handle).
3) Isolate loop body: extract the `while let Some(ortho)` loop plus generation-end transition into a function that takes the prepared context and returns (best_ortho, stats, maybe archive paths).
4) Decouple side-effects: push TUI/threading, heartbeat/mem-claim touches, and metrics UI updates behind callbacks so the loop can run headless (for exports/tests).
5) Rewire `main`: call the new helpers/functions so behavior stays identical.
6) Repoint exporter: have `export_data.rs` call the extracted generation runner instead of its own BFS.
7) Test & measure: cargo test/export; verify archives match prior behavior and performance is acceptable.
