# Potential Remaining Causes of Early Termination (Brainstorm)

1) **Bound is still too tight because prefix_stats are underestimated for common tokens.**  
   Even with axis-order and missing-axis fixes, if `prefix_stats` (max_desc_len) is low for frequent prefixes (e.g., stopwords), the upper bound stays small and branches get pruned. Needs profiling of `prefix_stats` distribution versus actual phrase lengths.

2) **best_score is set too high too early (from small but dense shapes).**  
   A modest-volume shape with high fullness could raise `best_score` quickly; subsequent bounds (volume, fullness) can’t clear it, so most branches prune. Confirm by logging best_score progression and comparing against potential upper bounds per generation.

3) **Impacted seeding pruning is discarding work after merge/transition.**  
   `prune_history_with_bound` runs when best improves; if impacted prefixes are miscomputed or too broad, it may remove seeds needed for deeper growth. Check impacted-prefix filtering and counts of pruned vs kept per bucket.

4) **Work queue is exhausted by anti-join/duplicates, not just pruning.**  
   Large `new_work` reported, but if most entries are duplicates against history, the queue may empty before next gen. Need a metric: `new_work` vs `work_len` after anti-join, and unique/duplicate counts.

5) **Score ordering tie-breaks cause “equal” branches to prune.**  
   The comparison `potential_score <= best_score` prunes on equality; if best_score is reached early, equal-potential branches are dropped even if they could produce a lexicographically better payload or later improvements. Consider whether pruning should require `<` rather than `<=` on fullness when volume ties.
