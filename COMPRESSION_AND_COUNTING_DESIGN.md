# Compression And Counting Design Note

## Context

The current storage path uses zstd-compressed ortho segment files for most large sealed data. This has been valuable: during the large `merge_6233` run the dashboard showed compression saving multiple terabytes of logical bytes, with a compression ratio around `7.3x`.

The cost is CPU and extra passes. Live profiling during merge transitions showed zstd compression/decompression as a major runtime component. There are also code paths that write a compressed run and then count records by reading the compressed file back through `count_ortho_records_in_file(..., compressed=true)`. That pattern is safe, but it is exactly the kind of "zip, then unzip to count" work that should be avoided in hot paths.

## Problem

For large transitions, the expensive pattern is:

1. Stream records and write a zstd segment.
2. Finish compression.
3. Re-open the compressed segment.
4. Decompress it to count records or estimate decoded bytes.
5. Commit/import the segment metadata.

That wastes CPU and I/O because the writer already saw every record. The segment should leave the write path with exact metadata attached.

There is also a broader codec-policy issue. Some sealed data is long-lived and worth compressing aggressively, while some data is short-lived and likely to be consumed in the next generation. Using the same compression level everywhere can trade long-term disk savings for avoidable near-term CPU cost.

## Goals

- Keep compression where it materially improves disk/offload cost.
- Avoid all compress-then-decompress-for-count work in hot paths.
- Preserve exact `record_count`, compressed bytes, and decoded/uncompressed bytes in TieredStore metadata.
- Make compression policy explicit and easy to debug.
- Keep segment payload compatibility unless a larger storage-format change is intentionally planned.

## Proposed Metadata Rule

Every sealed segment should be committed with exact metadata produced by the writer:

- `record_count`
- `compressed_bytes`
- `uncompressed_bytes` or decoded-size estimate
- `codec`
- `codec_level`
- `ordering`
- optional checksum

The writer loop should increment `record_count` as each ortho record is written. It should accumulate uncompressed bytes from `write_ortho_record_bytes` or the equivalent encoded-record write call. After the compressor finishes, the compressed byte count comes from the final file metadata.

If a segment is imported from an older path or external archive and no count metadata is available, a full decode count is acceptable as a one-time import cost. It should not be part of normal generation transition or merge processing.

## Candidate Code Changes

- Replace post-write calls to `count_ortho_records_in_file(..., compressed=true)` with counters maintained in the writer loops.
- Use existing catalog `record_count` when adopting/importing TieredStore-managed segments.
- Add `uncompressed_bytes` to `SegmentMeta` if it is not already persisted in enough places.
- Make `own_or_import_segment(...)` require a known `record_count` for normal managed write paths.
- Keep a fallback `count_compressed_records_slow(...)` helper only for old/imported data and make logs explicit when it is used.
- Add a debug counter for slow decode-count calls so regressions are visible.

## Codec Policy Options

Option 1: Keep zstd level 3 everywhere, but remove redundant count passes.

This is the safest first step. It preserves current compression behavior and should still recover time where counts currently force an extra decompress pass.

Option 2: Use a faster codec/level for short-lived work and spill.

For example:

- `work`: zstd level 1 or uncompressed while local disk headroom is healthy.
- `spill`: zstd level 1 unless it is expected to survive multiple generations.
- `seen/history`: zstd level 3 or stronger because these are long-lived.
- `archive/result`: zstd level 3 because they are durable and may be offloaded.

This could improve transition speed, but it increases disk pressure and S3 bytes. It should be tested against whole-run cost, not just CPU speed.

Option 3: Adaptive compression by pressure.

Use faster or no compression while disk is safely above target, then switch to compressed output when disk pressure or offload pressure is high. This is more complex and can make debugging harder, so it should only follow after the simpler metadata/count fixes.

## Recommended V1

Do the metadata/counting fix first:

1. Preserve zstd level 3 behavior.
2. Remove normal hot-path decode-count passes.
3. Add debug metrics for slow decode-count fallback.
4. Reprofile a large merge transition.

Only after that, evaluate codec policy. Compression is saving too much disk to remove casually; the safer initial win is avoiding redundant decompression.

## Tests

- Writing a compressed work segment commits the exact record count without reopening/decompressing the segment.
- Writing a compressed spill/history/seen segment commits exact `record_count` and byte metadata from the writer path.
- Managed segment adoption uses catalog record counts and does not count by decoding.
- Slow decode-count fallback is used only for imported/unmanaged data and increments a debug counter.
- Generation transition output counts remain identical before and after the refactor.
- Perf smoke test: a transition with many compressed runs does not show count-only decompression as a hot path.
