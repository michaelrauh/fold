Offload-on-Pressure Design (DO Spaces + Compaction)
===================================================

Goal
- Keep local disk usage under a configurable cap by triggering an emergency drain/compact and offloading finalized runs to DigitalOcean Spaces (S3-compatible) when landing or free space exceeds thresholds.
- Preserve ingest/merge semantics; add safety valves and resumability.

Triggers
- Track landing bytes (per bucket and total) and disk free space.
- Configurable high-water marks:
  - `landing_bytes_high_water`: total landing (active + drains) exceeds X GB
  - `disk_free_low_water`: free space below Y GB
- Optional per-bucket landing cap to avoid outliers.

Action on trigger
1) Pause ingest loop (stop popping work).
2) Drain all buckets (`drain_bucket`), resetting landing counts.
3) Compact immediately (`compact_landing` → runs, merge/anti-join as normal).
4) Offload newly created runs to DO Spaces; delete local copies after successful upload.
5) Resume ingest when landing is back under threshold (and cache/offload queue is caught up).

Read path
- When a run file is needed and missing locally, download from Spaces into a cache dir.
- Cache manager with byte cap (LRU eviction) to bound local footprint.

Components
- Offloader/downloader: wraps S3 API (Spaces endpoint/bucket/creds). Blocking PUT/GET with retry/backoff.
- Cache manager: tracks downloaded files, enforces size cap, evicts least-recently-used.
- Metadata: map run path → object key (keys can mirror path under a prefix), stored alongside run lists.
- Metrics: bytes offloaded/downloaded, cache hits/misses, trigger events.

Integration points
- `GenerationStore` write sites (after compression):
  - `write_streamed_run` (landing compaction output runs)
  - `merge_unique` / `merge_ortho_chunk`
  - `anti_join_orthos` outputs (seen/new-work)
  - `prune_history_with_bound` rewrites
  - `add_history_run` (after moving into history)
  - Archive artifacts (interner/optimal/history) after `write_archive_artifacts`
- Offload is conditional: only when “pressure mode” is set (triggered).
- `Run::iter` / `HistoryIterator`: if file missing locally, download to cache then stream.
- `run_generation_loop`: watchdog checks landing bytes/free space each housekeeping tick; on trigger, invokes drain+compact+offload cycle and pauses ingest until done.

Config
- Spaces: `endpoint`, `bucket`, `region` (if needed), `access_key`, `secret_key`, `prefix`.
- Thresholds: `landing_bytes_high_water`, `disk_free_low_water`, per-bucket caps.
- Cache: `cache_dir`, `cache_bytes_cap`.
- Flags: enable/disable offload-on-pressure.
- Provisioning: update setup scripts (e.g., `provision_droplet.sh`, `provision_droplet.sh` in repo) to inject Spaces creds, bucket, and thresholds into environment/config; ensure droplet has network egress to Spaces.

Resumability/error handling
- If offload fails: keep local file, log error, retry with backoff; if disk still under pressure, pause ingest or abort cleanly.
- On startup: hydrate missing runs on demand via download; metadata uses deterministic keys (path-based) so lookup is straightforward.
- Checkpoints: generation stats and best ortho already in metrics; consider writing a small checkpoint file after each trigger for resumable state.

Risks/considerations
- Added latency during emergency compaction/offload; ingest stalls briefly.
- Network dependency: Spaces availability; needs robust retry/backoff.
- Cache thrash if cache cap too small vs working set; tune cache size and thresholds.
- Offloading drains vs runs: drains are short-lived; offload runs/history/archives where the space win matters.

Rough flow (when trigger fires)
- Housekeeping detects pressure → set `pressure_mode`.
- Drain all buckets → compact → produce runs.
- For each new run/history file: upload to Spaces, delete local; record in metadata.
- Clear landing state; unset `pressure_mode` when below low-water.
- Ingest resumes; reads auto-download missing runs via cache when needed.

Implementation checklist
- Pattern for each item: implement the code, add/extend a test, run it and verify the result, finish with a full `cargo test`, then mark the box with a note about where the code lives and which test/command covers it.
- [x] Config: add Spaces endpoint/bucket/creds, thresholds, cache dir/size, enable flag. Implemented in `src/offload_config.rs` with `cargo test offload_config` covering defaults/overrides.
- [x] Offloader/downloader module (S3 client, PUT/GET with retry, key mapping). Implemented in `src/offloader.rs` with `cargo test offloader` covering upload/download to a mocked endpoint with retries.
- [x] Cache manager with byte cap + LRU eviction. Implemented in `src/offload_cache.rs` with `cargo test offload_cache` plus full `cargo test` showing eviction at cap and returning cached files.
- [x] `GenerationStore`: hook offload after finalized files (`write_streamed_run`, `merge_unique`, `merge_ortho_chunk`, `anti_join_orthos`, `prune_history_with_bound`, `add_history_run`). Implemented via `RunOffloader` hook/maybe_offload in `src/generation_store.rs`; integration test `generation_store::tests::compact_landing_offloads_and_deletes_runs` with `cargo test` verifies mocked offload deletes local runs.
- [x] `Run::iter`/`HistoryIterator`: download missing files to cache on demand. Implemented via `RunDownloader` hook + cache resolution in `src/generation_store.rs`; test `generation_store::tests::iter_downloads_missing_run_from_cache` removes a run, downloads from the mock store into cache, and iteration succeeds (`cargo test`).
- [x] Watchdog in `run_generation_loop`: monitor landing bytes/disk free; trigger drain+compact+offload; pause/resume ingest. Implemented via `PressureWatchdog` in `src/generation_runner.rs` calling `pressure_compact_and_offload`; test `pressure_watchdog_drains_and_offloads` plus full `cargo test` simulates high landing size and verifies landing drops after offload.
- [x] Archive offload after `write_archive_artifacts`. Implemented via `offload_archive_dir` in `src/main.rs` using the `RunOffloader` hook; test `archive_offload_removes_local_copy` exercises upload to mock/offloader and local archive removal (`cargo test`).
- [x] Metrics/logs: track offloaded bytes/files, cache hits/misses, trigger events. Implemented counters in `Metrics` (offload/download/cache/pressure), wired via generation_store downloader/offloader hooks and watchdog; tests (`compact_landing_offloads_and_deletes_runs`, `iter_downloads_missing_run_from_cache`, `pressure_watchdog_drains_and_offloads`) and full `cargo test` show snapshots increment.
- [x] Error handling/resume: offload failures now bubble up (no local delete), logging via metrics; restart hydrates missing runs from cache/downloader. Covered by `generation_store::tests::offload_failure_retains_local_and_downloads_on_restart` and full `cargo test`.
- [x] Local/in-memory offload mode for dev: added local-disk/mock stores + runtime wiring in `src/offload_runtime.rs` (hooked in `main.rs` and `bin/export_data.rs`), with config in `offload_config.rs`; tests `offloader::local_disk_store_puts_and_gets` and `offload_runtime::configure_local_offload_copies_run` (`cargo test`).
- [x] Real Spaces client + runtime wiring: implemented `SpacesObjectStore` via `rust-s3` (blocking) in `src/offloader.rs`; `configure_offload_runtime` now instantiates it when endpoint/region/creds are set, wiring hooks in `main.rs`/`bin/export_data.rs` (covered by `cargo test` build/runtime path; real Spaces requires env vars).
- [x] Provision/start scripts: `provision_droplet.sh` installs awscli and auto-creates the Spaces bucket (env-driven: SPACES_BUCKET/REGION/ENDPOINT/ACCESS/SECRET); `teardown_droplet.sh` deletes it; `start_fold.sh` now passes through `FOLD_OFFLOAD_*` env so the existing workflow (`SSH_KEY=43081865 SIZE=m-2vcpu-16gb SYNC_MODE=local TEXT_MODE=local ./provision_droplet.sh`, then `start_fold.sh`/`run_doubling.sh`) stays intact.
- [x] TUI visibility: added an offload/cache/pressure line to the header in `src/tui.rs` so offload/download/cache hits/misses and pressure triggers are always visible in the top panel (`cargo test`).
- [x] Runtime gaps: offload disabled by default; `configure_offload_runtime` logs and skips hooks when enabled but misconfigured; cache dir creation is handled via `OffloadCache::new`. Covered by `cargo test` and the runtime wiring in `main.rs`/`bin/export_data.rs`.
