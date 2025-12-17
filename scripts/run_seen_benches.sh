#!/usr/bin/env bash
set -euo pipefail

# Head-to-head on fast in-memory trackers (with/without Bloom), larger workload (10M ids).
BENCHES=(
  hashset_vec_head_to_head_10m
  hashset_vec_bloom_head_to_head_10m
  doubling_vec_head_to_head_10m
  doubling_vec_bloom_head_to_head_10m
)

# Use fewer samples and longer measurement time to avoid Criterion warnings.
CRITERION_ARGS=(--sample-size 20 --measurement-time 10)
if [ "$#" -gt 0 ]; then
  CRITERION_ARGS=("$@")
fi

echo "Running seen-tracker head-to-head (10M ids, ~20% dups, chunk=1024, flush~16k where applicable)"
echo "Criterion args: ${CRITERION_ARGS[*]}"
for name in "${BENCHES[@]}"; do
  echo
  echo "=== $name ==="
  cargo bench --bench seen_tracker_bench -- "${CRITERION_ARGS[@]}" "$name"
done
