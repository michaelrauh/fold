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
