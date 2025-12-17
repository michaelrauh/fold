#!/usr/bin/env bash
set -euo pipefail

BENCHES=(
  hashset_doubling_head_to_head_10m_base16k
  linear_probe_head_to_head_10m
  eytzinger_bloom_head_to_head_10m
  eytzinger_no_bloom_head_to_head_10m
  sorted_vec_bloom_head_to_head_10m
)

# Adjust Criterion args if desired.
CRITERION_ARGS=(--sample-size 10 --measurement-time 10)
if [ "$#" -gt 0 ]; then
  CRITERION_ARGS=("$@")
fi

echo "Running top contenders comparison"
echo "Criterion args: ${CRITERION_ARGS[*]}"

for name in "${BENCHES[@]}"; do
  echo
  echo "=== $name ==="
  cargo bench --bench seen_tracker_bench "$name" -- "${CRITERION_ARGS[@]}"
done
