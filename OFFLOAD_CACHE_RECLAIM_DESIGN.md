# Eager Tiered Reclaim Design Note

## Context

TieredStore is intended to behave like a three-tier residency system:

1. RAM for the hot mutable/working set.
2. Local disk for the near-term working set and canonical sealed segment files.
3. Remote object storage for cold sealed segments and overflow.

Local disk residency should be a performance hint, not a correctness boundary. Once a segment is committed, sealed, and safely present remotely, the local copy should be freely evictable unless it is pinned for the current read/write epoch.

The large `merge_6233` run on `143.198.12.155` exposed a mismatch with that intent. Reclaim kept the filesystem hovering just above the disk threshold by offloading one small segment at a time. At the same time, `fold_state/offload_cache` held roughly `200 GB`, including about `148.5 GiB` of stale cache files whose segment IDs were no longer present in the active merge catalog. This was safe, but it was not the intended locality policy: disposable cache bytes crowded out newly created canonical `work` segments.

## Problem

The current behavior is too incremental and too cache-tolerant:

1. A generation transition creates new canonical `work` segments.
2. Disk free space drops just below the threshold.
3. Reclaim prunes the offload cache only down to its configured cap.
4. Reclaim offloads just enough cataloged segment bytes to cross the threshold.
5. The next write drops disk below the threshold again.
6. The cycle repeats, often offloading fresh work segments that are likely to be needed in the next generation.

This is safe when remote offload works, but it creates pressure churn. It also violates the spirit of the tiered design: cache data should be very short-lived, and local disk should be drained aggressively when it is no longer the right tier for the workload.

## Long-Term Policy

When disk pressure is reached, TieredStore should enter eager reclaim mode:

1. Flush mutable heads.
2. Seal any safe-to-seal append heads.
3. Commit manifests/catalog metadata.
4. Delete stale and duplicate cache files.
5. Upload and evict all unpinned sealed segments until substantial headroom is restored.
6. Continue only after local disk has real breathing room, not just enough for the next segment write.

In short: if disk is the constrained resource, push unpinned sealed data up to remote storage eagerly.

## Reclaim Target

Do not reclaim only to `low_water`.

Use a large target such as:

`target_free = low_water + eager_hysteresis`

where `eager_hysteresis` is intentionally large enough to avoid repeated one-segment reclaim cycles. The exact value should be configurable, but the default should be measured in tens or hundreds of GB for large runs.

The existing small hysteresis is useful for ordinary checks, but eager mode should be allowed to reclaim much more aggressively.

## Offloadable Data

Offloadable:

- Any committed sealed segment with `ref_count > 0`.
- `work`, `spill`, `run`, `seen`, `new-work`, `history`, archive/result, and merge-intermediate segments.
- Downloaded canonical segment copies after their pin/epoch expires.
- Current-generation data once it is sealed and not pinned.

Not offloadable:

- Mutable append heads.
- Uncommitted segment allocations.
- Manifest/catalog/control files.
- Upload/download scratch files.
- Process coordination files such as locks, heartbeats, and claim files.
- Segments pinned for the current epoch/read.

## Cache Policy

The persistent offload cache should not be a large independent disk tier.

Preferred long-term direction:

- Remove the long-lived `fold_state/offload_cache` as a separate residency layer.
- Download remote segments directly to their canonical `store/segments/<id>.seg` path using a temporary file and atomic rename.
- Mark `disk=true` only after the canonical local file is complete.
- Pin the segment for the current epoch/read.
- Let normal TieredStore reclaim evict it after the pin expires.
- Keep only a small scratch area for partial downloads and uploads.

If a separate cache remains temporarily, reclaim order should be:

1. Delete cache files whose segment ID is no longer present in the owning catalog.
2. Delete cache duplicates where the canonical segment is already local.
3. Evict cache files for remote-only segments by LRU/oldest order.
4. Only then offload canonical cataloged segments.

Cache files are disposable. Cataloged sealed segments are the real data.

## Epoch And Pinning Policy

Eager reclaim only works if pinning is real.

Required behavior:

- Use `begin_epoch(epoch)` / `finish_epoch(epoch)` around generation processing and transitions.
- Touch segments when they are read or written.
- Pin downloaded/read segments through the current epoch.
- Do not evict pinned segments.
- Prefer evicting oldest unpinned segments by `last_touch_epoch`.
- Use size as a tie-breaker so reclaim can restore headroom quickly.

The live run showed many active segments with `created_epoch=0` and `last_touch_epoch=0`, which makes least-used ordering weak. A real eager policy needs meaningful touch/epoch metadata.

## Safety Rules

- Never delete a canonical `store/segments/<id>.seg` file unless the catalog first records `remote=true`.
- Flush the catalog state before deleting local canonical bytes.
- Cache/scratch files may be deleted without catalog mutation because they are not authoritative.
- A remote-only segment may be rehydrated later, but should be pinned while the current operation uses it.
- If remote upload fails, keep the local canonical file and either retry or fail cleanly before accepting more writes that cannot be reserved.

## Implementation Sketch

- Move cache pruning into TieredStore/disk reclaim as a first-class step.
- Add an eager reclaim mode that targets `low_water + eager_hysteresis`, not just `low_water`.
- Make reclaim collect candidates from catalog metadata only.
- Exclude pinned/current mutable data.
- Sort candidates by `last_touch_epoch`, then larger files first.
- Upload candidates that are not already remote.
- Flush metadata after `remote=true`.
- Delete the local canonical file and set `disk=false`.
- Log eager reclaim cycles with:
  - reason,
  - free before/after,
  - target free,
  - cache bytes pruned,
  - canonical files offloaded,
  - canonical bytes offloaded,
  - candidates skipped due to pinning.

## Recommended Staging

V1:

- Keep the existing cache, but aggressively prune stale/duplicate cache before canonical offload.
- Increase reclaim target during pressure cycles so reclaim does not offload only one segment at a time.
- Add logs for stale cache bytes and pinned-skipped segment bytes.

V2:

- Remove the long-lived offload cache as a separate disk tier.
- Rehydrate remote segments directly to canonical TieredStore segment paths.
- Make epochs/touches/pinning authoritative.

V3:

- Tune eviction policy using measured hit/miss and rehydrate rates.
- Consider prefetching or generation-aware pinning only if profiles show repeated rehydrate misses.

## Tests

- Eager reclaim frees to `low_water + eager_hysteresis`, not merely `low_water`.
- Stale cache entries are deleted before canonical segments are offloaded.
- Cache duplicates for local canonical segments are deleted without changing catalog tier state.
- Remote-only cache entries are evicted before fresh canonical `work` segments are offloaded.
- Pinned sealed segments are not evicted.
- Unpinned sealed `work`, `spill`, `run`, `seen`, `new-work`, `history`, archive/result, and merge-intermediate segments are all eligible.
- Canonical segment offload records `remote=true` and flushes metadata before deleting local bytes.
- Rehydrated remote segments are written to the canonical path and pinned for the current epoch.
- A store under heavy disk pressure does not enter repeated one-segment reclaim cycles when enough unpinned sealed data exists.
