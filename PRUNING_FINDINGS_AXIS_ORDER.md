# Finding: Axis Selection in Upper Bound Can Under-Bound Potential

## Hypothesis
`upper_bound_score` multiplies the first `dim_count` entries of `axis_totals` as provided. If `axis_totals.len() > dim_count` and the larger axes aren’t first, the bound ignores them, producing a lower-than-possible volume/fullness and allowing over-pruning.

## Reproduction (unit test)
- Added `upper_bound_should_use_largest_axes` (fails today, marked `#[should_panic]` to document the bug).
- Scenario: `axis_totals = [2, 2, 10]`, `dim_count = 2`.
  - Current bound uses 2 and 2 → volume `(2-1)*(2-1)=1`, fullness `2*2=4`.
  - Optimistic bound should pick the two largest axes (10 and 2) → volume `(10-1)*(2-1)=9`, fullness `10*2=20`.
  - The test asserts the higher bound; it panics with message “under-bound: larger axes ignored”, confirming the issue.

## Why it matters
- If requirements return more prefixes than dimensions (e.g., enriched diagonals/up-expansion metadata), the current ordering-dependent take() can drop the largest available axis totals. The bound then understates potential and can prune live branches even with correct prefix_stats.

## What to change
- Sort (or select the top `dim_count`) axis totals before multiplying, both for volume and fullness.
- Keep missing-axis fallback logic as-is.

## Status
- No production change yet; the failing test captures the problem. Once the bound is fixed to pick the largest axes, the test should be flipped to expect success and the `should_panic` removed.
