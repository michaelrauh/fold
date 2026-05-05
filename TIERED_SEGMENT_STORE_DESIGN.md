# Tiered Segment Store Design

## Summary

The long-term target is a real tiered storage facade with amortized IO costs:

- RAM is the hot ephemeral tier.
- Local disk is a bounded working cache and scratch tier.
- S3/Spaces is the cold durable tier.

The system should recover the convenience of the old disk-backed queue while keeping the generational store's merge-friendly behavior. The key change is to make immutable sealed segments the universal unit of storage and movement.

## Design Goals

- Keep all large durable data in one storage model.
- Amortize disk and remote IO over segment-sized batches, not per ortho.
- Make local disk optional as a large capacity tier; it should be a cache, not a correctness requirement for total output size.
- Preserve queue-like semantics for work/spill while retaining efficient sorted-run merges for history and anti-join.
- Move residency decisions into the store so algorithm code no longer reasons about paths, offload markers, or ad hoc reclaim rules.

## Core Model

Everything durable is one of three things:

- Mutable append heads
- Immutable sealed segments
- Small manifests / catalogs

All large payloads move through the system as immutable sealed segments. All heavy IO, batching, upload, download, eviction, and checkpointing happens at segment granularity.

## Logical Views Over One Store

The store exposes several logical collection views over the same segment substrate:

- Queue view
  - For work and spill.
  - Semantics are append-to-tail and consume-from-head by sealed segment.
- RunSet view
  - For history, merge intermediates, unique runs, anti-join inputs, and archive outputs.
  - Semantics are ordered immutable segment sets with merge/replace operations.
- Log view
  - For landing buffers and append-oriented local staging.
- Blob view
  - For small control payloads only.

This restores the old disk-backed queue's amortized behavior without reintroducing a separate queue-specific storage engine.

## Tier Semantics

### RAM

- Append heads
- Small read caches
- Decode scratch
- Current-operation buffers

### Local Disk

- Canonical local copies of active sealed segments
- Upload/download staging
- Working-set cache
- Current-epoch pinned data

### S3 / Spaces

- Durable cold segment tier
- Authoritative backing for remote-resident sealed segments after commit/upload

The key rule is:

**Local disk is a bounded cache and scratch tier, not a requirement to hold the full logical output of the job.**

## Amortized Cost Model

### Write Path

- Append records into an in-memory head.
- Flush the head to a local file only after a segment threshold is reached.
- Seal the segment once threshold or policy requires it.
- Commit one manifest update per sealed segment batch, not per record.
- Upload sealed segments asynchronously after commit.

### Read Path

- Consume or scan data by segment, not by individual file fragments.
- Rehydrate missing segments to canonical local paths only when needed.
- Pin rehydrated segments for the active epoch or operation.
- Evict later at whole-segment granularity.

### Merge Path

- Merge N input segments into a stream of output segments.
- Seal outputs incrementally.
- Upload / evict outputs as policy permits.
- Expose output as a manifest of segment refs, not as one giant required local file.

This is the core correction to the current design: local reservations should be for bounded scratch and pipeline depth, not for total final output size.

## Queue Semantics

The queue view should be implemented as:

- One mutable tail head
- A deque of sealed segment refs

High-level operations:

- `push_batch(records)`
  - Append to tail head
  - Flush/seal when thresholds are met
- `pop_segment_into_ram()`
  - Claim the next sealed head segment
  - Ensure local residency if needed
  - Decode sequentially in memory
- `ack_segment()`
  - Drop or advance the manifest ref once consumed

This reproduces the old disk-backed queue's amortized disk behavior:

- one metadata mutation per segment
- one sequential read per segment
- one rehydrate per segment when cold

## Generation / Merge Flow

The generation layer should orchestrate collections, not own files:

- landing -> log collection
- work / spill -> queue collections
- history / merge outputs -> runset collections

A generation transition becomes:

1. Drain landing into sealed sorted segments.
2. Merge segments into unique output segments.
3. Anti-join unique output against history segments.
4. Emit accepted results directly into next-work queue segments.
5. Append accepted results into history segments.
6. Checkpoint manifests.

This avoids wasteful path-centric workflows such as writing a temporary run only to reopen and rewrite it into a second representation.

## Pinning And Residency

Pinning must be authoritative and segment-oriented:

- Current merge inputs are pinned.
- Current output heads are pinned.
- Current-epoch touched segments are pinned until the epoch policy allows release.
- Reclaim excludes pinned segments.

This allows local disk to stay fluid without risking eviction of data still live in the active operation.

## Reclaim Policy

The reclaim policy should operate entirely over catalog metadata:

- Delete scratch garbage first.
- Evict already-remote unpinned segments next.
- Upload and evict old unpinned local-only sealed segments after commit.
- Keep mutable heads and control metadata local.

The policy should shrink and expand the warm local working set automatically depending on available disk. A small disk should mean more rehydrate activity, not correctness failure.

## Correctness Boundary

The store's correctness boundary should be:

- mutable heads are local-only until sealed and committed
- sealed segments become durable once committed and uploaded according to policy
- manifests are the source of truth

Algorithm code should not depend on local path ownership. It should request segment readers and writers from the store and let the store manage placement.

## Small-Disk Mode

The design should explicitly support smaller local disks:

- local disk holds only current heads, pinned inputs, active scratch, and a warm cache
- large cold segments can stay remote-only
- output should be streamed into manageable sealed segments, not budgeted as one giant local requirement

If S3 is healthy, local pressure should cause more aggressive upload/eviction, not a hard failure because the total future output cannot fit locally.

## Benefits

- Restores queue-like amortized IO behavior
- Keeps merge-friendly sorted-run structure
- Makes remote a real tier, not just post-hoc cleanup
- Lets local disk become a fluid cache
- Bounds metadata churn at segment granularity

## Costs And Complexity

- The store core becomes more important and more complex.
- Manifest and pinning correctness must be very strong.
- Crash recovery must reason about committed segments and in-flight heads.
- Debugging shifts from raw filesystem inspection toward manifests plus segment inspection.

## Implementation Direction

High-level phases:

1. Make all large data segment-backed and manifest-owned.
2. Re-express work/spill as queue views over segment refs.
3. Re-express history and merge intermediates as runset views over segment refs.
4. Move rehydrate/offload/eviction entirely behind the store facade.
5. Replace whole-output local reservations with bounded scratch and streaming segment output.

## Bottom Line

The long-term target is:

**one tiered segment store with queue, runset, log, and blob views, where all heavy operations are amortized per sealed segment and local disk is only a bounded working cache.**

That captures the best properties of both:

- the old disk-backed queue
- the current generational merge model

while making the `RAM -> disk -> S3` hierarchy genuinely fluid enough to support smaller local disks.
