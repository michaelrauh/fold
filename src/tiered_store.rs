use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock, RwLock, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

pub const STORAGE_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_NAMESPACE: &str = "default";
const SEGMENT_REHYDRATE_RETRIES: usize = 3;
#[cfg(not(test))]
const SEGMENT_REHYDRATE_BACKOFF_BASE: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const SEGMENT_REHYDRATE_BACKOFF_CAP: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TierPresence {
    pub ram: bool,
    pub disk: bool,
    pub remote: bool,
}

impl Default for TierPresence {
    fn default() -> Self {
        Self {
            ram: false,
            disk: true,
            remote: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentMeta {
    pub kind: String,
    pub codec: String,
    pub ordering: String,
    pub bytes: u64,
    #[serde(default)]
    pub uncompressed_bytes: u64,
    pub record_count: u64,
    #[serde(default)]
    pub codec_level: i32,
    #[serde(default)]
    pub checksum: Option<String>,
    pub created_epoch: u64,
    pub last_touch_epoch: u64,
    pub ref_count: u64,
    pub tiers: TierPresence,
    pub remote_key: Option<String>,
    pub pinned_until_epoch: u64,
}

impl SegmentMeta {
    fn new(
        kind: impl Into<String>,
        codec: impl Into<String>,
        ordering: impl Into<String>,
        created_epoch: u64,
    ) -> Self {
        Self {
            kind: kind.into(),
            codec: codec.into(),
            ordering: ordering.into(),
            bytes: 0,
            uncompressed_bytes: 0,
            record_count: 0,
            codec_level: 0,
            checksum: None,
            created_epoch,
            last_touch_epoch: created_epoch,
            ref_count: 0,
            tiers: TierPresence::default(),
            remote_key: None,
            pinned_until_epoch: created_epoch.saturating_add(1),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Catalog {
    schema_version: u32,
    next_segment_id: u64,
    current_epoch: u64,
    namespaces: BTreeSet<String>,
    segments: BTreeMap<String, SegmentMeta>,
}

impl Default for Catalog {
    fn default() -> Self {
        let mut namespaces = BTreeSet::new();
        namespaces.insert(DEFAULT_NAMESPACE.to_string());
        Self {
            schema_version: STORAGE_SCHEMA_VERSION,
            next_segment_id: 0,
            current_epoch: 0,
            namespaces,
            segments: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct QueueManifest {
    schema_version: u32,
    kind: String,
    segments: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RunSetManifest {
    schema_version: u32,
    kind: String,
    segments: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct LogManifest {
    schema_version: u32,
    kind: String,
    sealed_segments: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct BlobManifest {
    schema_version: u32,
    kind: String,
    entries: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocatedSegment {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentRef {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub uncompressed_bytes: u64,
    pub record_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TieredStoreReclaimCandidate {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub kind: String,
    pub created_epoch: u64,
    pub last_touch_epoch: u64,
    pub ref_count: u64,
    pub tiers: TierPresence,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TieredStoreReclaimScan {
    pub candidates: Vec<TieredStoreReclaimCandidate>,
    pub skipped_remote: usize,
    pub skipped_missing_local: usize,
    pub skipped_zero_bytes: usize,
    pub skipped_pinned: usize,
    pub skipped_pinned_bytes: u64,
}

pub trait SegmentRemote: Send + Sync {
    fn upload_segment(&self, path: &Path) -> io::Result<String>;
    fn download_segment(&self, remote_key: &str, dest_path: &Path) -> io::Result<()>;
}

#[derive(Debug, Default)]
struct TieredStoreCounters {
    catalog_loads: AtomicU64,
    catalog_flushes: AtomicU64,
    manifest_loads: AtomicU64,
    manifest_flushes: AtomicU64,
    segment_id_lookups: AtomicU64,
}

impl TieredStoreCounters {
    fn snapshot(&self) -> TieredStoreDebugCounters {
        TieredStoreDebugCounters {
            catalog_loads: self.catalog_loads.load(Ordering::Relaxed),
            catalog_flushes: self.catalog_flushes.load(Ordering::Relaxed),
            manifest_loads: self.manifest_loads.load(Ordering::Relaxed),
            manifest_flushes: self.manifest_flushes.load(Ordering::Relaxed),
            segment_id_lookups: self.segment_id_lookups.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TieredStoreDebugCounters {
    pub catalog_loads: u64,
    pub catalog_flushes: u64,
    pub manifest_loads: u64,
    pub manifest_flushes: u64,
    pub segment_id_lookups: u64,
}

#[derive(Debug)]
struct CatalogState {
    catalog: Catalog,
    dirty: bool,
}

impl CatalogState {
    fn new(catalog: Catalog) -> Self {
        Self {
            catalog,
            dirty: false,
        }
    }
}

#[derive(Debug)]
struct TieredStoreInner {
    root: PathBuf,
    catalog: RwLock<CatalogState>,
    pins: Mutex<BTreeMap<String, u64>>,
    counters: TieredStoreCounters,
}

#[derive(Clone, Debug)]
pub struct TieredStore {
    inner: Arc<TieredStoreInner>,
}

#[derive(Debug)]
pub struct SegmentPinGuard {
    store: TieredStore,
    id: String,
    active: bool,
}

impl Drop for SegmentPinGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = self.store.unpin_segment(&self.id, "drop");
            self.active = false;
        }
    }
}

thread_local! {
    static SEGMENT_REMOTE: RefCell<Option<Arc<dyn SegmentRemote>>> = const { RefCell::new(None) };
    static SEGMENT_METRICS_HANDLE: RefCell<Option<crate::metrics::Metrics>> = const { RefCell::new(None) };
}

pub fn configure_segment_remote(remote: Option<Arc<dyn SegmentRemote>>) {
    SEGMENT_REMOTE.with(|slot| *slot.borrow_mut() = remote);
}

pub fn set_segment_metrics_handle(handle: Option<crate::metrics::Metrics>) {
    SEGMENT_METRICS_HANDLE.with(|slot| *slot.borrow_mut() = handle);
}

fn current_segment_remote() -> Option<Arc<dyn SegmentRemote>> {
    SEGMENT_REMOTE.with(|slot| slot.borrow().clone())
}

fn segment_metrics_handle() -> Option<crate::metrics::Metrics> {
    SEGMENT_METRICS_HANDLE.with(|slot| slot.borrow().clone())
}

fn log_segment_event(message: impl Into<String>) {
    if let Some(metrics) = segment_metrics_handle() {
        metrics.add_log(message.into());
    }
}

impl TieredStore {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = normalize_root_path(&root.into())?;
        let mut registry = store_registry_lock()?;
        registry.retain(|_, weak| weak.strong_count() > 0);
        if let Some(existing) = registry.get(&root).and_then(Weak::upgrade) {
            return Ok(Self { inner: existing });
        }

        let counters = TieredStoreCounters::default();
        let catalog_path = root.join("catalog.json");
        let catalog = if !catalog_path.exists() {
            let catalog = Catalog::default();
            write_json_atomic(&catalog_path, &catalog)?;
            counters.catalog_flushes.fetch_add(1, Ordering::Relaxed);
            catalog
        } else {
            let catalog = read_json_file::<Catalog>(&catalog_path)?;
            counters.catalog_loads.fetch_add(1, Ordering::Relaxed);
            if catalog.schema_version != STORAGE_SCHEMA_VERSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "unsupported tiered store schema version {} in {}",
                        catalog.schema_version,
                        root.display()
                    ),
                ));
            }
            catalog
        };

        let inner = Arc::new(TieredStoreInner {
            root: root.clone(),
            catalog: RwLock::new(CatalogState::new(catalog)),
            pins: Mutex::new(BTreeMap::new()),
            counters,
        });
        let store = Self { inner };
        store.cleanup_crashed_transients()?;
        registry.insert(root, Arc::downgrade(&store.inner));
        Ok(store)
    }

    pub fn from_managed_path(path: &Path) -> io::Result<Option<Self>> {
        let Some(root) = storage_root_for_path(path)? else {
            return Ok(None);
        };
        Ok(Some(Self::open(root)?))
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.root().join("catalog.json")
    }

    pub fn begin_epoch(&self, epoch: u64) -> io::Result<()> {
        self.with_catalog_mut(false, |catalog| {
            catalog.current_epoch = epoch;
            Ok(())
        })
    }

    pub fn finish_epoch(&self, epoch: u64) -> io::Result<()> {
        self.with_catalog_mut(false, |catalog| {
            catalog.current_epoch = epoch;
            for meta in catalog.segments.values_mut() {
                if meta.pinned_until_epoch <= epoch {
                    meta.pinned_until_epoch = epoch;
                }
            }
            Ok(())
        })
    }

    pub fn current_epoch(&self) -> io::Result<u64> {
        self.with_catalog(|catalog| Ok(catalog.current_epoch))
    }

    pub fn namespaces(&self) -> io::Result<Vec<String>> {
        let mut namespaces: Vec<String> =
            self.with_catalog(|catalog| Ok(catalog.namespaces.iter().cloned().collect()))?;
        namespaces.sort();
        Ok(namespaces)
    }

    pub fn has_namespace(&self, namespace: &str) -> io::Result<bool> {
        self.with_catalog(|catalog| Ok(catalog.namespaces.contains(namespace)))
    }

    pub fn open_queue(
        &self,
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> io::Result<QueueCollection> {
        let handle = QueueCollection {
            store: self.clone(),
            namespace: namespace.into(),
            name: name.into(),
        };
        handle.ensure_manifest()?;
        Ok(handle)
    }

    pub fn open_runset(
        &self,
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> io::Result<RunSetCollection> {
        let handle = RunSetCollection {
            store: self.clone(),
            namespace: namespace.into(),
            name: name.into(),
        };
        handle.ensure_manifest()?;
        Ok(handle)
    }

    pub fn open_log(
        &self,
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> io::Result<LogCollection> {
        let handle = LogCollection {
            store: self.clone(),
            namespace: namespace.into(),
            name: name.into(),
        };
        handle.ensure_manifest()?;
        Ok(handle)
    }

    pub fn open_blob(
        &self,
        namespace: impl Into<String>,
        name: impl Into<String>,
    ) -> io::Result<BlobCollection> {
        let handle = BlobCollection {
            store: self.clone(),
            namespace: namespace.into(),
            name: name.into(),
        };
        handle.ensure_manifest()?;
        Ok(handle)
    }

    pub fn debug_counters(&self) -> TieredStoreDebugCounters {
        self.inner.counters.snapshot()
    }

    pub fn reclaim_candidates(&self) -> io::Result<Vec<TieredStoreReclaimCandidate>> {
        Ok(self.reclaim_candidates_with_summary()?.candidates)
    }

    pub fn reclaim_candidates_with_summary(&self) -> io::Result<TieredStoreReclaimScan> {
        self.with_catalog(|catalog| {
            let mut scan = TieredStoreReclaimScan::default();
            for (id, meta) in &catalog.segments {
                if meta.bytes == 0 {
                    scan.skipped_zero_bytes = scan.skipped_zero_bytes.saturating_add(1);
                    continue;
                }
                if !meta.tiers.disk {
                    scan.skipped_missing_local = scan.skipped_missing_local.saturating_add(1);
                    continue;
                }
                if self.is_segment_pinned(id)? {
                    scan.skipped_pinned = scan.skipped_pinned.saturating_add(1);
                    scan.skipped_pinned_bytes =
                        scan.skipped_pinned_bytes.saturating_add(meta.bytes);
                    continue;
                }

                let path = self.segment_path_for_id(id);
                if !path.exists() {
                    scan.skipped_missing_local = scan.skipped_missing_local.saturating_add(1);
                    continue;
                }

                scan.candidates.push(TieredStoreReclaimCandidate {
                    id: id.clone(),
                    path,
                    bytes: meta.bytes,
                    kind: meta.kind.clone(),
                    created_epoch: meta.created_epoch,
                    last_touch_epoch: meta.last_touch_epoch,
                    ref_count: meta.ref_count,
                    tiers: meta.tiers.clone(),
                });
            }
            Ok(scan)
        })
    }

    pub fn flush_metadata(&self, _reason: &str) -> io::Result<()> {
        let mut state = self.catalog_write()?;
        if !state.dirty {
            return Ok(());
        }
        write_json_atomic(&self.catalog_path(), &state.catalog)?;
        state.dirty = false;
        self.inner
            .counters
            .catalog_flushes
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn flush_if_dirty(&self) -> io::Result<()> {
        self.flush_metadata("flush_if_dirty")
    }

    pub fn snapshot_namespace(&self, src: &str, dst: &str) -> io::Result<()> {
        if src == dst {
            return Ok(());
        }
        if !self.has_namespace(src)? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("unknown namespace {}", src),
            ));
        }
        self.delete_namespace(dst)?;
        self.ensure_namespace(dst)?;

        let src_collections = self.collections_dir().join(src);
        let dst_collections = self.collections_dir().join(dst);
        let mut refcount_deltas = BTreeMap::new();
        if src_collections.exists() {
            fs::create_dir_all(&dst_collections)?;
            for entry in fs::read_dir(&src_collections)? {
                let entry = entry?;
                let src_path = entry.path();
                let dst_path = dst_collections.join(entry.file_name());
                fs::copy(&src_path, &dst_path)?;
                let ids = manifest_segment_ids(&fs::read(&src_path)?)?;
                for id in ids {
                    *refcount_deltas.entry(id).or_insert(0) += 1;
                }
            }
        }

        let src_heads = self.heads_dir().join(src);
        if src_heads.exists() {
            copy_dir_recursive(&src_heads, &self.heads_dir().join(dst))?;
        }

        let src_blobs = self.blobs_dir().join(src);
        if src_blobs.exists() {
            copy_dir_recursive(&src_blobs, &self.blobs_dir().join(dst))?;
        }

        self.adjust_refcounts_batch(&refcount_deltas, true)?;
        Ok(())
    }

    pub fn export_namespace_to_root(
        &self,
        src_namespace: &str,
        dest_root: impl Into<PathBuf>,
        dest_namespace: &str,
    ) -> io::Result<()> {
        if !self.has_namespace(src_namespace)? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("unknown namespace {}", src_namespace),
            ));
        }
        let dest_root = dest_root.into();
        if dest_root.exists() {
            fs::remove_dir_all(&dest_root)?;
        }
        let dest = TieredStore::open(dest_root)?;
        dest.delete_namespace(dest_namespace)?;
        dest.ensure_namespace(dest_namespace)?;

        let src_catalog = self.read_catalog_snapshot()?;
        let referenced = referenced_segment_counts_for_namespace(self, src_namespace)?;
        let mut segment_metas: BTreeMap<String, SegmentMeta> = BTreeMap::new();
        for (id, count) in &referenced {
            let meta = src_catalog.segments.get(id).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("missing segment metadata for {}", id),
                )
            })?;
            let mut exported = meta.clone();
            exported.ref_count = *count;
            exported.tiers.disk = true;
            segment_metas.insert(id.clone(), exported);
            let src_path = self
                .ensure_local(&self.segment_path_for_id(id))?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("segment {} unavailable locally for export", id),
                    )
                })?;
            let dest_path = dest.segment_path_for_id(id);
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(src_path, dest_path)?;
        }

        let src_collections = self.collections_dir().join(src_namespace);
        let dest_collections = dest.collections_dir().join(dest_namespace);
        if src_collections.exists() {
            fs::create_dir_all(&dest_collections)?;
            for entry in fs::read_dir(&src_collections)? {
                let entry = entry?;
                fs::copy(entry.path(), dest_collections.join(entry.file_name()))?;
            }
        }

        let src_heads = self.heads_dir().join(src_namespace);
        if src_heads.exists() {
            copy_dir_recursive(&src_heads, &dest.heads_dir().join(dest_namespace))?;
        }
        let src_blobs = self.blobs_dir().join(src_namespace);
        if src_blobs.exists() {
            copy_dir_recursive(&src_blobs, &dest.blobs_dir().join(dest_namespace))?;
        }

        let mut dest_catalog = Catalog::default();
        dest_catalog.current_epoch = src_catalog.current_epoch;
        dest_catalog.namespaces.clear();
        dest_catalog.namespaces.insert(dest_namespace.to_string());
        dest_catalog.segments = segment_metas;
        dest_catalog.next_segment_id = next_segment_id_after(&dest_catalog.segments);
        {
            let mut state = dest.catalog_write()?;
            state.catalog = dest_catalog;
            state.dirty = true;
        }
        dest.flush_metadata("export_namespace_to_root")?;
        drop(referenced);
        Ok(())
    }

    pub fn promote_namespace(&self, src: &str, dst: &str) -> io::Result<()> {
        self.snapshot_namespace(src, dst)?;
        self.delete_namespace(src)?;
        Ok(())
    }

    pub fn delete_namespace(&self, namespace: &str) -> io::Result<()> {
        let collection_dir = self.collections_dir().join(namespace);
        let mut refcount_deltas = BTreeMap::new();
        if collection_dir.exists() {
            for entry in fs::read_dir(&collection_dir)? {
                let entry = entry?;
                let bytes = fs::read(entry.path())?;
                for id in manifest_segment_ids(&bytes)? {
                    *refcount_deltas.entry(id).or_insert(0) -= 1;
                }
            }
            fs::remove_dir_all(&collection_dir)?;
        }

        let heads_dir = self.heads_dir().join(namespace);
        if heads_dir.exists() {
            fs::remove_dir_all(heads_dir)?;
        }
        let blobs_dir = self.blobs_dir().join(namespace);
        if blobs_dir.exists() {
            fs::remove_dir_all(blobs_dir)?;
        }

        self.adjust_refcounts_batch(&refcount_deltas, false)?;
        self.with_catalog_mut(false, |catalog| {
            catalog.namespaces.remove(namespace);
            Ok(())
        })?;
        self.flush_metadata("delete_namespace")?;
        self.gc_unreferenced_segments()?;
        Ok(())
    }

    pub fn ensure_namespace(&self, namespace: &str) -> io::Result<()> {
        let already_present = self.has_namespace(namespace)?;
        if !already_present {
            self.with_catalog_mut(false, |catalog| {
                catalog.namespaces.insert(namespace.to_string());
                Ok(())
            })?;
            self.flush_metadata("ensure_namespace")?;
        }
        self.ensure_namespace_dirs(namespace)?;
        Ok(())
    }

    pub fn allocate_segment(
        &self,
        kind: &str,
        codec: &str,
        ordering: &str,
    ) -> io::Result<AllocatedSegment> {
        let (id, path) = self.with_catalog_mut(true, |catalog| {
            let id = format!("{:016x}", catalog.next_segment_id);
            if catalog.segments.contains_key(&id) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "segment allocation collision for id {} in {}",
                        id,
                        self.root().display()
                    ),
                ));
            }
            catalog.next_segment_id = catalog.next_segment_id.saturating_add(1);
            let path = self.segment_path_for_id(&id);
            if path.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "segment allocation would overwrite existing path {}",
                        path.display()
                    ),
                ));
            }
            let meta = SegmentMeta::new(
                kind.to_string(),
                codec.to_string(),
                ordering.to_string(),
                catalog.current_epoch,
            );
            catalog.segments.insert(id.clone(), meta);
            Ok((id, path))
        })?;
        Ok(AllocatedSegment { id, path })
    }

    pub fn commit_allocated_segment(
        &self,
        segment: &AllocatedSegment,
        bytes: u64,
        record_count: u64,
    ) -> io::Result<PathBuf> {
        self.commit_allocated_segment_with_meta(segment, bytes, record_count, 0, 0, None)
    }

    pub fn commit_allocated_segment_with_meta(
        &self,
        segment: &AllocatedSegment,
        bytes: u64,
        record_count: u64,
        uncompressed_bytes: u64,
        codec_level: i32,
        checksum: Option<String>,
    ) -> io::Result<PathBuf> {
        self.with_catalog_mut(true, |catalog| {
            let meta = catalog.segments.get_mut(&segment.id).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("missing allocated segment {}", segment.id),
                )
            })?;
            meta.bytes = bytes;
            meta.uncompressed_bytes = uncompressed_bytes;
            meta.record_count = record_count;
            meta.codec_level = codec_level;
            meta.checksum = checksum;
            meta.last_touch_epoch = catalog.current_epoch;
            meta.pinned_until_epoch = meta
                .pinned_until_epoch
                .max(catalog.current_epoch.saturating_add(1));
            meta.tiers.disk = true;
            Ok(())
        })?;
        Ok(segment.path.clone())
    }

    pub fn import_file_as_segment(
        &self,
        src_path: &Path,
        kind: &str,
        codec: &str,
        ordering: &str,
        record_count: u64,
        move_file: bool,
    ) -> io::Result<PathBuf> {
        let allocated = self.allocate_segment(kind, codec, ordering)?;
        if let Some(parent) = allocated.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if move_file {
            fs::rename(src_path, &allocated.path)?;
        } else {
            fs::copy(src_path, &allocated.path)?;
        }
        let bytes = fs::metadata(&allocated.path)?.len();
        self.commit_allocated_segment(&allocated, bytes, record_count)
    }

    pub fn import_file_as_segment_with_meta(
        &self,
        src_path: &Path,
        kind: &str,
        codec: &str,
        ordering: &str,
        record_count: u64,
        uncompressed_bytes: u64,
        codec_level: i32,
        move_file: bool,
    ) -> io::Result<PathBuf> {
        let allocated = self.allocate_segment(kind, codec, ordering)?;
        if let Some(parent) = allocated.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if move_file {
            fs::rename(src_path, &allocated.path)?;
        } else {
            fs::copy(src_path, &allocated.path)?;
        }
        let bytes = fs::metadata(&allocated.path)?.len();
        self.commit_allocated_segment_with_meta(
            &allocated,
            bytes,
            record_count,
            uncompressed_bytes,
            codec_level,
            None,
        )
    }

    pub fn segment_ref_for_path(&self, path: &Path) -> io::Result<Option<SegmentRef>> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(None);
        };
        self.with_catalog(|catalog| {
            let Some(meta) = catalog.segments.get(&id) else {
                return Ok(None);
            };
            Ok(Some(SegmentRef {
                id: id.clone(),
                path: self.segment_path_for_id(&id),
                bytes: meta.bytes,
                uncompressed_bytes: meta.uncompressed_bytes,
                record_count: meta.record_count,
            }))
        })
    }

    pub fn managed_file_size(&self, path: &Path) -> io::Result<Option<u64>> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(None);
        };
        self.with_catalog(|catalog| Ok(catalog.segments.get(&id).map(|meta| meta.bytes)))
    }

    pub fn ensure_local(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(None);
        };
        self.ensure_local_segment(&id, "ensure_local").map(Some)
    }

    pub fn ensure_local_segment(&self, id: &str, reason: &str) -> io::Result<PathBuf> {
        let canonical = self.segment_path_for_id(id);
        if canonical.exists() {
            self.touch_segment(id, reason)?;
            return Ok(canonical);
        }

        let (remote_key, expected_bytes) = self.with_catalog(|catalog| {
            let meta = catalog.segments.get(id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
            })?;
            if !meta.tiers.remote {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "segment {} missing locally and is not marked remote: {}",
                        id,
                        canonical.display()
                    ),
                ));
            }
            let remote_key = meta.remote_key.clone().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("segment {} missing locally and has no remote key", id),
                )
            })?;
            Ok((remote_key, meta.bytes))
        })?;

        let remote = current_segment_remote().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "segment {} missing locally and no segment remote configured",
                    id
                ),
            )
        })?;

        if let Some(parent) = canonical.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = canonical.with_extension("download_tmp");
        log_segment_event(format!(
            "Segment rehydrate start: segment_id={}, remote_key={}, expected_bytes={}, reason={}",
            id, remote_key, expected_bytes, reason
        ));

        let mut last_error = None;
        for attempt in 0..=SEGMENT_REHYDRATE_RETRIES {
            let _ = fs::remove_file(&tmp);
            let result = remote
                .download_segment(&remote_key, &tmp)
                .and_then(|_| verify_downloaded_segment(id, &remote_key, &tmp, expected_bytes));
            match result {
                Ok(actual_bytes) => {
                    File::open(&tmp)?.sync_all()?;
                    fs::rename(&tmp, &canonical)?;
                    self.set_disk_state_for_id(id, true)?;
                    self.touch_segment(id, reason)?;
                    if let Some(metrics) = segment_metrics_handle() {
                        metrics.record_download(1, actual_bytes);
                        metrics.add_log(format!(
                            "Segment rehydrate success: segment_id={}, remote_key={}, bytes={}, attempts={}",
                            id,
                            remote_key,
                            actual_bytes,
                            attempt.saturating_add(1)
                        ));
                    }
                    return Ok(canonical);
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    let actual_bytes = file_size_or_zero(&tmp);
                    let _ = fs::remove_file(&tmp);
                    log_segment_event(format!(
                        "Segment rehydrate final failure: kind=missing_remote_object, segment_id={}, remote_key={}, expected_bytes={}, actual_bytes={}, attempts={}, error={}",
                        id,
                        remote_key,
                        expected_bytes,
                        actual_bytes,
                        attempt.saturating_add(1),
                        err
                    ));
                    return Err(err);
                }
                Err(err) => {
                    let actual_bytes = file_size_or_zero(&tmp);
                    let message = err.to_string();
                    let _ = fs::remove_file(&tmp);
                    last_error = Some(message.clone());
                    if attempt == SEGMENT_REHYDRATE_RETRIES {
                        break;
                    }
                    log_segment_event(format!(
                        "Segment rehydrate retry: segment_id={}, remote_key={}, expected_bytes={}, actual_bytes={}, attempt={}, error={}",
                        id,
                        remote_key,
                        expected_bytes,
                        actual_bytes,
                        attempt.saturating_add(1),
                        message
                    ));
                    let delay = segment_rehydrate_delay(attempt, &remote_key);
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                }
            }
        }

        let _ = fs::remove_file(&tmp);
        let message = format!(
            "remote_download_exhausted: segment_id={}, remote_key={}, expected_bytes={}, attempts={}, last_error={}",
            id,
            remote_key,
            expected_bytes,
            SEGMENT_REHYDRATE_RETRIES.saturating_add(1),
            last_error.unwrap_or_else(|| "unknown".to_string())
        );
        log_segment_event(format!("Segment rehydrate final failure: {}", message));
        Err(io::Error::other(message))
    }

    pub fn open_segment_reader(
        &self,
        id: &str,
        reason: &str,
    ) -> io::Result<(File, SegmentPinGuard)> {
        let guard = self.pin_segment(id, reason)?;
        let path = self.ensure_local_segment(id, reason)?;
        match File::open(&path) {
            Ok(file) => Ok((file, guard)),
            Err(err) => {
                drop(guard);
                Err(err)
            }
        }
    }

    pub fn pin_segment(&self, id: &str, reason: &str) -> io::Result<SegmentPinGuard> {
        self.with_catalog_mut(false, |catalog| {
            let current_epoch = catalog.current_epoch;
            let meta = catalog.segments.get_mut(id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
            })?;
            meta.last_touch_epoch = current_epoch;
            meta.pinned_until_epoch = meta.pinned_until_epoch.max(current_epoch.saturating_add(1));
            Ok(())
        })?;
        {
            let mut pins = self
                .inner
                .pins
                .lock()
                .map_err(|_| io::Error::other("tiered store pin lock poisoned"))?;
            *pins.entry(id.to_string()).or_insert(0) += 1;
        }
        let _ = reason;
        Ok(SegmentPinGuard {
            store: self.clone(),
            id: id.to_string(),
            active: true,
        })
    }

    pub fn pin_path(&self, path: &Path, reason: &str) -> io::Result<Option<SegmentPinGuard>> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(None);
        };
        Ok(Some(self.pin_segment(&id, reason)?))
    }

    pub fn unpin_segment(&self, id: &str, _reason: &str) -> io::Result<()> {
        let mut pins = self
            .inner
            .pins
            .lock()
            .map_err(|_| io::Error::other("tiered store pin lock poisoned"))?;
        if let Some(count) = pins.get_mut(id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pins.remove(id);
            }
        }
        Ok(())
    }

    pub fn segment_id_for_managed_path(&self, path: &Path) -> io::Result<Option<String>> {
        self.segment_id_for_path(path)
    }

    fn is_segment_pinned(&self, id: &str) -> io::Result<bool> {
        let pins = self
            .inner
            .pins
            .lock()
            .map_err(|_| io::Error::other("tiered store pin lock poisoned"))?;
        Ok(pins.get(id).copied().unwrap_or(0) > 0)
    }

    fn touch_segment(&self, id: &str, _reason: &str) -> io::Result<()> {
        self.with_catalog_mut(false, |catalog| {
            let current_epoch = catalog.current_epoch;
            let meta = catalog.segments.get_mut(id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
            })?;
            meta.last_touch_epoch = current_epoch;
            meta.pinned_until_epoch = meta.pinned_until_epoch.max(current_epoch.saturating_add(1));
            Ok(())
        })
    }

    pub fn set_remote_state_for_path(
        &self,
        path: &Path,
        remote: bool,
        remote_key: Option<String>,
    ) -> io::Result<()> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(());
        };
        self.with_catalog_mut(true, |catalog| {
            let meta = catalog.segments.get_mut(&id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
            })?;
            meta.tiers.remote = remote;
            meta.remote_key = remote_key;
            Ok(())
        })
    }

    pub fn set_disk_state_for_path(&self, path: &Path, disk: bool) -> io::Result<()> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(());
        };
        self.set_disk_state_for_id(&id, disk)
    }

    fn set_disk_state_for_id(&self, id: &str, disk: bool) -> io::Result<()> {
        self.with_catalog_mut(true, |catalog| {
            let meta = catalog.segments.get_mut(id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
            })?;
            meta.tiers.disk = disk;
            Ok(())
        })
    }

    pub fn offload_segment_for_path(&self, path: &Path, reason: &str) -> io::Result<Option<u64>> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(None);
        };
        self.offload_segment(&id, reason).map(Some)
    }

    pub fn offload_segment(&self, id: &str, reason: &str) -> io::Result<u64> {
        if self.is_segment_pinned(id)? {
            return Ok(0);
        }

        let path = self.segment_path_for_id(id);
        let (bytes, already_remote) = self.with_catalog(|catalog| {
            let meta = catalog.segments.get(id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
            })?;
            Ok((meta.bytes, meta.tiers.remote))
        })?;
        if bytes == 0 || !path.exists() {
            return Ok(0);
        }

        if !already_remote {
            let remote = current_segment_remote()
                .ok_or_else(|| io::Error::other("no segment remote configured"))?;
            let remote_key = remote.upload_segment(&path)?;
            self.set_remote_state_for_path(&path, true, Some(remote_key))?;
        }

        // The metadata flush happens inside set_remote_state/set_disk_state before local deletion.
        self.set_disk_state_for_id(id, false)?;
        fs::remove_file(&path)?;
        let _ = reason;
        Ok(bytes)
    }

    pub fn update_segment_stats_for_path(
        &self,
        path: &Path,
        bytes: u64,
        record_count: u64,
    ) -> io::Result<()> {
        self.update_segment_stats_for_path_with_meta(path, bytes, record_count, 0, 0, None)
    }

    pub fn update_segment_stats_for_path_with_meta(
        &self,
        path: &Path,
        bytes: u64,
        record_count: u64,
        uncompressed_bytes: u64,
        codec_level: i32,
        checksum: Option<String>,
    ) -> io::Result<()> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(());
        };
        self.with_catalog_mut(true, |catalog| {
            let current_epoch = catalog.current_epoch;
            let meta = catalog.segments.get_mut(&id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
            })?;
            meta.bytes = bytes;
            meta.uncompressed_bytes = uncompressed_bytes;
            meta.record_count = record_count;
            meta.codec_level = codec_level;
            meta.checksum = checksum;
            meta.last_touch_epoch = current_epoch;
            meta.pinned_until_epoch = meta.pinned_until_epoch.max(current_epoch.saturating_add(1));
            Ok(())
        })
    }

    pub fn touch_segment_for_path(&self, path: &Path, epoch: Option<u64>) -> io::Result<()> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(());
        };
        self.with_catalog_mut(false, |catalog| {
            let current_epoch = epoch.unwrap_or(catalog.current_epoch);
            let meta = catalog.segments.get_mut(&id).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
            })?;
            meta.last_touch_epoch = current_epoch;
            meta.pinned_until_epoch = meta.pinned_until_epoch.max(current_epoch.saturating_add(1));
            Ok(())
        })
    }

    pub fn delete_segment_if_unreferenced(&self, path: &Path) -> io::Result<()> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(());
        };
        let remove = self.with_catalog(|catalog| {
            let Some(meta) = catalog.segments.get(&id) else {
                return Ok(false);
            };
            Ok(meta.ref_count == 0)
        })?;
        if !remove {
            return Ok(());
        }

        let segment_path = self.segment_path_for_id(&id);
        let _ = fs::remove_file(&segment_path);
        self.with_catalog_mut(false, |catalog| {
            catalog.segments.remove(&id);
            Ok(())
        })?;
        self.flush_metadata("delete_segment_if_unreferenced")
    }

    pub fn is_managed_remote_only(&self, path: &Path) -> io::Result<bool> {
        let Some(id) = self.segment_id_for_path(path)? else {
            return Ok(false);
        };
        self.with_catalog(|catalog| {
            let Some(meta) = catalog.segments.get(&id) else {
                return Ok(false);
            };
            Ok(meta.tiers.remote && !meta.tiers.disk)
        })
    }

    fn gc_unreferenced_segments(&self) -> io::Result<()> {
        let stale: Vec<String> = self.with_catalog(|catalog| {
            Ok(catalog
                .segments
                .iter()
                .filter(|(_, meta)| meta.ref_count == 0)
                .map(|(id, _)| id.clone())
                .collect())
        })?;
        if stale.is_empty() {
            return Ok(());
        }
        for id in &stale {
            let path = self.segment_path_for_id(&id);
            let _ = fs::remove_file(&path);
        }
        self.with_catalog_mut(false, |catalog| {
            for id in &stale {
                catalog.segments.remove(id);
            }
            Ok(())
        })?;
        self.flush_metadata("gc_unreferenced_segments")?;
        Ok(())
    }

    fn cleanup_crashed_transients(&self) -> io::Result<()> {
        self.cleanup_download_temps()?;
        let stale: Vec<String> = self.with_catalog(|catalog| {
            Ok(catalog
                .segments
                .iter()
                .filter(|(_, meta)| {
                    if meta.bytes == 0 {
                        return true;
                    }
                    meta.ref_count == 0
                        && matches!(meta.kind.as_str(), "chunk" | "unique" | "seen" | "new-work")
                })
                .map(|(id, _)| id.clone())
                .collect())
        })?;
        if stale.is_empty() {
            return Ok(());
        }
        for id in &stale {
            let path = self.segment_path_for_id(id);
            let _ = fs::remove_file(path);
        }
        self.with_catalog_mut(false, |catalog| {
            for id in &stale {
                catalog.segments.remove(id);
            }
            Ok(())
        })?;
        self.flush_metadata("cleanup_crashed_transients")
    }

    fn cleanup_download_temps(&self) -> io::Result<usize> {
        let segments = self.segments_dir();
        if !segments.exists() {
            return Ok(0);
        }
        let mut removed = 0usize;
        for entry in fs::read_dir(segments)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("download_tmp") {
                match fs::remove_file(&path) {
                    Ok(()) => removed = removed.saturating_add(1),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                }
            }
        }
        if removed > 0 {
            log_segment_event(format!(
                "TieredStore startup scrub removed {} stale download temp files from {}",
                removed,
                self.root().display()
            ));
        }
        Ok(removed)
    }

    fn collections_dir(&self) -> PathBuf {
        self.root().join("collections")
    }

    fn segments_dir(&self) -> PathBuf {
        self.root().join("segments")
    }

    fn heads_dir(&self) -> PathBuf {
        self.root().join("heads")
    }

    fn blobs_dir(&self) -> PathBuf {
        self.root().join("blobs")
    }

    fn segment_id_for_path(&self, path: &Path) -> io::Result<Option<String>> {
        self.inner
            .counters
            .segment_id_lookups
            .fetch_add(1, Ordering::Relaxed);
        let candidate = path
            .file_stem()
            .and_then(|name| name.to_str())
            .map(|s| s.to_string());
        let Some(id) = candidate else {
            return Ok(None);
        };
        if path != self.segment_path_for_id(&id) {
            return Ok(None);
        }
        self.with_catalog(|catalog| Ok(catalog.segments.contains_key(&id).then_some(id)))
    }

    pub fn segment_path_for_id(&self, id: &str) -> PathBuf {
        self.root().join("segments").join(format!("{}.seg", id))
    }

    fn ensure_namespace_dirs(&self, namespace: &str) -> io::Result<()> {
        fs::create_dir_all(self.collections_dir().join(namespace))?;
        fs::create_dir_all(self.heads_dir().join(namespace))?;
        fs::create_dir_all(self.blobs_dir().join(namespace))?;
        Ok(())
    }

    fn read_catalog_snapshot(&self) -> io::Result<Catalog> {
        self.with_catalog(|catalog| Ok(catalog.clone()))
    }

    fn with_catalog_mut<T>(
        &self,
        flush: bool,
        mutator: impl FnOnce(&mut Catalog) -> io::Result<T>,
    ) -> io::Result<T> {
        let out = {
            let mut state = self.catalog_write()?;
            let out = mutator(&mut state.catalog)?;
            state.dirty = true;
            out
        };
        if flush {
            self.flush_metadata("catalog_mutation")?;
        }
        Ok(out)
    }

    fn adjust_refcounts_batch(
        &self,
        deltas: &BTreeMap<String, i64>,
        flush: bool,
    ) -> io::Result<()> {
        if deltas.is_empty() {
            return Ok(());
        }
        self.with_catalog_mut(flush, |catalog| {
            for (id, delta) in deltas {
                let meta = catalog.segments.get_mut(id).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, format!("unknown segment {}", id))
                })?;
                if *delta >= 0 {
                    meta.ref_count = meta.ref_count.saturating_add(*delta as u64);
                } else {
                    meta.ref_count = meta.ref_count.saturating_sub((-*delta) as u64);
                }
            }
            Ok(())
        })
    }

    fn with_catalog<T>(&self, reader: impl FnOnce(&Catalog) -> io::Result<T>) -> io::Result<T> {
        let state = self.catalog_read()?;
        reader(&state.catalog)
    }

    fn catalog_read(&self) -> io::Result<std::sync::RwLockReadGuard<'_, CatalogState>> {
        self.inner
            .catalog
            .read()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "tiered store catalog lock poisoned"))
    }

    fn catalog_write(&self) -> io::Result<std::sync::RwLockWriteGuard<'_, CatalogState>> {
        self.inner
            .catalog
            .write()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "tiered store catalog lock poisoned"))
    }

    fn read_manifest<T: DeserializeOwned>(&self, path: &Path) -> io::Result<T> {
        self.inner
            .counters
            .manifest_loads
            .fetch_add(1, Ordering::Relaxed);
        read_json_file(path)
    }

    fn write_manifest<T: Serialize>(&self, path: &Path, value: &T) -> io::Result<()> {
        write_json_atomic(path, value)?;
        self.inner
            .counters
            .manifest_flushes
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

fn next_segment_id_after(segments: &BTreeMap<String, SegmentMeta>) -> u64 {
    segments
        .keys()
        .filter_map(|id| u64::from_str_radix(id, 16).ok())
        .max()
        .map(|max| max.saturating_add(1))
        .unwrap_or(0)
}

fn store_registry() -> &'static Mutex<BTreeMap<PathBuf, Weak<TieredStoreInner>>> {
    static STORE_REGISTRY: OnceLock<Mutex<BTreeMap<PathBuf, Weak<TieredStoreInner>>>> =
        OnceLock::new();
    STORE_REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn store_registry_lock()
-> io::Result<std::sync::MutexGuard<'static, BTreeMap<PathBuf, Weak<TieredStoreInner>>>> {
    store_registry()
        .lock()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "tiered store registry lock poisoned"))
}

fn normalize_root_path(root: &Path) -> io::Result<PathBuf> {
    ensure_root_layout(root)?;
    match root.canonicalize() {
        Ok(path) => Ok(path),
        Err(_) if root.is_absolute() => Ok(root.to_path_buf()),
        Err(_) => Ok(std::env::current_dir()?.join(root)),
    }
}

fn referenced_segment_counts_for_namespace(
    store: &TieredStore,
    namespace: &str,
) -> io::Result<BTreeMap<String, u64>> {
    let mut counts = BTreeMap::new();
    let collection_dir = store.collections_dir().join(namespace);
    if !collection_dir.exists() {
        return Ok(counts);
    }
    for entry in fs::read_dir(collection_dir)? {
        let entry = entry?;
        let bytes = fs::read(entry.path())?;
        for id in manifest_segment_ids(&bytes)? {
            *counts.entry(id).or_insert(0) += 1;
        }
    }
    Ok(counts)
}

#[derive(Clone, Debug)]
pub struct QueueCollection {
    store: TieredStore,
    namespace: String,
    name: String,
}

impl QueueCollection {
    pub fn segment_paths(&self) -> io::Result<Vec<PathBuf>> {
        Ok(self
            .read_manifest()?
            .segments
            .into_iter()
            .map(|id| self.store.segment_path_for_id(&id))
            .collect())
    }

    pub fn push_back_path(&self, path: &Path) -> io::Result<()> {
        self.with_manifest_mut(|manifest| {
            let id = self.segment_id_for_path(path)?;
            manifest.segments.push(id);
            Ok(())
        })
    }

    pub fn push_front_path(&self, path: &Path) -> io::Result<()> {
        self.with_manifest_mut(|manifest| {
            let id = self.segment_id_for_path(path)?;
            manifest.segments.insert(0, id);
            Ok(())
        })
    }

    pub fn pop_front_path(&self) -> io::Result<Option<PathBuf>> {
        let removed = self.with_manifest_mut(|manifest| Ok(pop_front(&mut manifest.segments)))?;
        Ok(removed.map(|id| self.store.segment_path_for_id(&id)))
    }

    pub fn replace_paths(&self, paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
        let new_ids = paths
            .iter()
            .map(|path| self.segment_id_for_path(path))
            .collect::<io::Result<Vec<_>>>()?;
        let old_ids = self.with_manifest_mut(|manifest| {
            let old = std::mem::replace(&mut manifest.segments, new_ids.clone());
            Ok(old)
        })?;
        Ok(old_ids
            .into_iter()
            .map(|id| self.store.segment_path_for_id(&id))
            .collect())
    }

    pub fn clear(&self) -> io::Result<Vec<PathBuf>> {
        self.replace_paths(&[])
    }

    fn ensure_manifest(&self) -> io::Result<()> {
        self.store.ensure_namespace(&self.namespace)?;
        let path = self.manifest_path();
        if !path.exists() {
            self.store.write_manifest(
                &path,
                &QueueManifest {
                    schema_version: STORAGE_SCHEMA_VERSION,
                    kind: "queue".to_string(),
                    segments: Vec::new(),
                },
            )?;
        }
        Ok(())
    }

    fn manifest_path(&self) -> PathBuf {
        self.store
            .collections_dir()
            .join(&self.namespace)
            .join(format!("{}.json", self.name))
    }

    fn read_manifest(&self) -> io::Result<QueueManifest> {
        self.store.read_manifest(&self.manifest_path())
    }

    fn with_manifest_mut<T>(
        &self,
        mutator: impl FnOnce(&mut QueueManifest) -> io::Result<T>,
    ) -> io::Result<T> {
        let path = self.manifest_path();
        let mut manifest = self.read_manifest()?;
        let before = manifest.segments.clone();
        let out = mutator(&mut manifest)?;
        let deltas = refcount_deltas_for_manifest_change(&before, &manifest.segments);
        self.store.adjust_refcounts_batch(&deltas, false)?;
        if let Err(err) = self.store.write_manifest(&path, &manifest) {
            let rollback = invert_refcount_deltas(&deltas);
            let _ = self.store.adjust_refcounts_batch(&rollback, false);
            return Err(err);
        }
        self.store.flush_metadata("queue_manifest_mutation")?;
        Ok(out)
    }

    fn segment_id_for_path(&self, path: &Path) -> io::Result<String> {
        self.store.segment_id_for_path(path)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path is not a managed segment")
        })
    }
}

#[derive(Clone, Debug)]
pub struct RunSetCollection {
    store: TieredStore,
    namespace: String,
    name: String,
}

impl RunSetCollection {
    pub fn segment_paths(&self) -> io::Result<Vec<PathBuf>> {
        Ok(self
            .read_manifest()?
            .segments
            .into_iter()
            .map(|id| self.store.segment_path_for_id(&id))
            .collect())
    }

    pub fn append_path(&self, path: &Path) -> io::Result<()> {
        self.with_manifest_mut(|manifest| {
            manifest.segments.push(self.segment_id_for_path(path)?);
            Ok(())
        })
    }

    pub fn replace_paths(&self, paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
        let new_ids = paths
            .iter()
            .map(|path| self.segment_id_for_path(path))
            .collect::<io::Result<Vec<_>>>()?;
        let old_ids = self.with_manifest_mut(|manifest| {
            let old = std::mem::replace(&mut manifest.segments, new_ids.clone());
            Ok(old)
        })?;
        Ok(old_ids
            .into_iter()
            .map(|id| self.store.segment_path_for_id(&id))
            .collect())
    }

    pub fn clear(&self) -> io::Result<Vec<PathBuf>> {
        self.replace_paths(&[])
    }

    fn ensure_manifest(&self) -> io::Result<()> {
        self.store.ensure_namespace(&self.namespace)?;
        let path = self.manifest_path();
        if !path.exists() {
            self.store.write_manifest(
                &path,
                &RunSetManifest {
                    schema_version: STORAGE_SCHEMA_VERSION,
                    kind: "runset".to_string(),
                    segments: Vec::new(),
                },
            )?;
        }
        Ok(())
    }

    fn manifest_path(&self) -> PathBuf {
        self.store
            .collections_dir()
            .join(&self.namespace)
            .join(format!("{}.json", self.name))
    }

    fn read_manifest(&self) -> io::Result<RunSetManifest> {
        self.store.read_manifest(&self.manifest_path())
    }

    fn with_manifest_mut<T>(
        &self,
        mutator: impl FnOnce(&mut RunSetManifest) -> io::Result<T>,
    ) -> io::Result<T> {
        let path = self.manifest_path();
        let mut manifest = self.read_manifest()?;
        let before = manifest.segments.clone();
        let out = mutator(&mut manifest)?;
        let deltas = refcount_deltas_for_manifest_change(&before, &manifest.segments);
        self.store.adjust_refcounts_batch(&deltas, false)?;
        if let Err(err) = self.store.write_manifest(&path, &manifest) {
            let rollback = invert_refcount_deltas(&deltas);
            let _ = self.store.adjust_refcounts_batch(&rollback, false);
            return Err(err);
        }
        self.store.flush_metadata("runset_manifest_mutation")?;
        Ok(out)
    }

    fn segment_id_for_path(&self, path: &Path) -> io::Result<String> {
        self.store.segment_id_for_path(path)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path is not a managed segment")
        })
    }
}

#[derive(Clone, Debug)]
pub struct LogCollection {
    store: TieredStore,
    namespace: String,
    name: String,
}

impl LogCollection {
    pub fn head_path(&self) -> io::Result<PathBuf> {
        Ok(self
            .store
            .heads_dir()
            .join(&self.namespace)
            .join(format!("{}.head", self.name)))
    }

    pub fn sealed_paths(&self) -> io::Result<Vec<PathBuf>> {
        Ok(self
            .read_manifest()?
            .sealed_segments
            .into_iter()
            .map(|id| self.store.segment_path_for_id(&id))
            .collect())
    }

    pub fn append_sealed_path(&self, path: &Path) -> io::Result<()> {
        self.with_manifest_mut(|manifest| {
            manifest
                .sealed_segments
                .push(self.segment_id_for_path(path)?);
            Ok(())
        })
    }

    pub fn take_sealed_paths(&self) -> io::Result<Vec<PathBuf>> {
        let removed = self.with_manifest_mut(|manifest| {
            let old = std::mem::take(&mut manifest.sealed_segments);
            Ok(old)
        })?;
        Ok(removed
            .into_iter()
            .map(|id| self.store.segment_path_for_id(&id))
            .collect())
    }

    fn ensure_manifest(&self) -> io::Result<()> {
        self.store.ensure_namespace(&self.namespace)?;
        let path = self.manifest_path();
        if !path.exists() {
            self.store.write_manifest(
                &path,
                &LogManifest {
                    schema_version: STORAGE_SCHEMA_VERSION,
                    kind: "log".to_string(),
                    sealed_segments: Vec::new(),
                },
            )?;
        }
        Ok(())
    }

    fn manifest_path(&self) -> PathBuf {
        self.store
            .collections_dir()
            .join(&self.namespace)
            .join(format!("{}.json", self.name))
    }

    fn read_manifest(&self) -> io::Result<LogManifest> {
        self.store.read_manifest(&self.manifest_path())
    }

    fn with_manifest_mut<T>(
        &self,
        mutator: impl FnOnce(&mut LogManifest) -> io::Result<T>,
    ) -> io::Result<T> {
        let path = self.manifest_path();
        let mut manifest = self.read_manifest()?;
        let before = manifest.sealed_segments.clone();
        let out = mutator(&mut manifest)?;
        let deltas = refcount_deltas_for_manifest_change(&before, &manifest.sealed_segments);
        self.store.adjust_refcounts_batch(&deltas, false)?;
        if let Err(err) = self.store.write_manifest(&path, &manifest) {
            let rollback = invert_refcount_deltas(&deltas);
            let _ = self.store.adjust_refcounts_batch(&rollback, false);
            return Err(err);
        }
        self.store.flush_metadata("log_manifest_mutation")?;
        Ok(out)
    }

    fn segment_id_for_path(&self, path: &Path) -> io::Result<String> {
        self.store.segment_id_for_path(path)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path is not a managed segment")
        })
    }
}

#[derive(Clone, Debug)]
pub struct BlobCollection {
    store: TieredStore,
    namespace: String,
    name: String,
}

impl BlobCollection {
    pub fn write_atomic(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self.blob_path(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        self.with_manifest_mut(|manifest| {
            manifest.entries.insert(key.to_string(), key.to_string());
            Ok(())
        })
    }

    pub fn read(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        let manifest = self.read_manifest()?;
        if !manifest.entries.contains_key(key) {
            return Ok(None);
        }
        Ok(Some(fs::read(self.blob_path(key))?))
    }

    fn ensure_manifest(&self) -> io::Result<()> {
        self.store.ensure_namespace(&self.namespace)?;
        let path = self.manifest_path();
        if !path.exists() {
            self.store.write_manifest(
                &path,
                &BlobManifest {
                    schema_version: STORAGE_SCHEMA_VERSION,
                    kind: "blob".to_string(),
                    entries: BTreeMap::new(),
                },
            )?;
        }
        Ok(())
    }

    fn blob_path(&self, key: &str) -> PathBuf {
        self.store
            .blobs_dir()
            .join(&self.namespace)
            .join(&self.name)
            .join(key)
    }

    fn manifest_path(&self) -> PathBuf {
        self.store
            .collections_dir()
            .join(&self.namespace)
            .join(format!("{}.json", self.name))
    }

    fn read_manifest(&self) -> io::Result<BlobManifest> {
        self.store.read_manifest(&self.manifest_path())
    }

    fn with_manifest_mut<T>(
        &self,
        mutator: impl FnOnce(&mut BlobManifest) -> io::Result<T>,
    ) -> io::Result<T> {
        let path = self.manifest_path();
        let mut manifest = self.read_manifest()?;
        let out = mutator(&mut manifest)?;
        self.store.write_manifest(&path, &manifest)?;
        Ok(out)
    }
}

fn ensure_root_layout(root: &Path) -> io::Result<()> {
    if root.exists() && !root.join("catalog.json").exists() {
        let legacy_entries = ["history", "work", "landing", "runs", "spill"];
        let has_legacy = legacy_entries.iter().any(|entry| root.join(entry).exists());
        if has_legacy {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "legacy store layout at {} is not supported by the tiered store",
                    root.display()
                ),
            ));
        }
    }
    fs::create_dir_all(root.join("segments"))?;
    fs::create_dir_all(root.join("collections"))?;
    fs::create_dir_all(root.join("heads"))?;
    fs::create_dir_all(root.join("blobs"))?;
    Ok(())
}

fn verify_downloaded_segment(
    id: &str,
    remote_key: &str,
    tmp: &Path,
    expected_bytes: u64,
) -> io::Result<u64> {
    let actual_bytes = fs::metadata(tmp)?.len();
    if actual_bytes != expected_bytes {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "remote_download_size_mismatch: segment_id={}, remote_key={}, expected_bytes={}, actual_bytes={}",
                id, remote_key, expected_bytes, actual_bytes
            ),
        ));
    }
    Ok(actual_bytes)
}

fn file_size_or_zero(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn segment_rehydrate_delay(attempt: usize, remote_key: &str) -> Duration {
    #[cfg(test)]
    {
        let _ = (attempt, remote_key);
        Duration::ZERO
    }
    #[cfg(not(test))]
    {
        if SEGMENT_REHYDRATE_BACKOFF_BASE.is_zero() || SEGMENT_REHYDRATE_BACKOFF_CAP.is_zero() {
            return Duration::ZERO;
        }
        let shift = attempt.min(8) as u32;
        let multiplier = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
        let base_ms = SEGMENT_REHYDRATE_BACKOFF_BASE.as_millis() as u64;
        let capped_ms = base_ms
            .saturating_mul(multiplier as u64)
            .min(SEGMENT_REHYDRATE_BACKOFF_CAP.as_millis() as u64);
        let jitter_ms = ((remote_key.len() as u64)
            .saturating_mul(41)
            .saturating_add((attempt as u64).saturating_mul(97)))
            % 250;
        Duration::from_millis(capped_ms.saturating_add(jitter_ms))
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn read_json_file<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
}

fn refcount_deltas_for_manifest_change(
    before: &[String],
    after: &[String],
) -> BTreeMap<String, i64> {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for id in before {
        *counts.entry(id.clone()).or_insert(0) -= 1;
    }
    for id in after {
        *counts.entry(id.clone()).or_insert(0) += 1;
    }
    counts.retain(|_, delta| *delta != 0);
    counts
}

fn invert_refcount_deltas(deltas: &BTreeMap<String, i64>) -> BTreeMap<String, i64> {
    deltas
        .iter()
        .map(|(id, delta)| (id.clone(), -*delta))
        .collect()
}

fn pop_front<T>(items: &mut Vec<T>) -> Option<T> {
    if items.is_empty() {
        None
    } else {
        Some(items.remove(0))
    }
}

fn manifest_segment_ids(bytes: &[u8]) -> io::Result<Vec<String>> {
    if let Ok(manifest) = serde_json::from_slice::<QueueManifest>(bytes) {
        return Ok(manifest.segments);
    }
    if let Ok(manifest) = serde_json::from_slice::<RunSetManifest>(bytes) {
        return Ok(manifest.segments);
    }
    if let Ok(manifest) = serde_json::from_slice::<LogManifest>(bytes) {
        return Ok(manifest.sealed_segments);
    }
    if serde_json::from_slice::<BlobManifest>(bytes).is_ok() {
        return Ok(Vec::new());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "unsupported collection manifest",
    ))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

pub fn storage_root_for_path(path: &Path) -> io::Result<Option<PathBuf>> {
    let mut current = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };
    while let Some(dir) = current {
        if dir.join("catalog.json").exists() {
            return Ok(Some(dir.to_path_buf()));
        }
        current = dir.parent();
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_segment(
        store: &TieredStore,
        kind: &str,
        bytes: &[u8],
        records: u64,
    ) -> AllocatedSegment {
        let segment = store.allocate_segment(kind, "zstd", "sorted").unwrap();
        fs::write(&segment.path, bytes).unwrap();
        store
            .commit_allocated_segment(&segment, bytes.len() as u64, records)
            .unwrap();
        segment
    }

    #[test]
    fn allocates_and_tracks_segment_refs() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let queue = store.open_queue("default", "work").unwrap();

        let segment = store.allocate_segment("work", "raw", "fifo").unwrap();
        fs::write(&segment.path, b"abc").unwrap();
        store.commit_allocated_segment(&segment, 3, 2).unwrap();
        queue.push_back_path(&segment.path).unwrap();

        let refs = queue.segment_paths().unwrap();
        assert_eq!(refs, vec![segment.path.clone()]);
        let meta = store.segment_ref_for_path(&segment.path).unwrap().unwrap();
        assert_eq!(meta.bytes, 3);
        assert_eq!(meta.record_count, 2);
    }

    #[test]
    fn reopening_same_root_shares_live_catalog_state() {
        let temp = tempdir().unwrap();
        let first = TieredStore::open(temp.path()).unwrap();
        let second = TieredStore::open(temp.path()).unwrap();

        assert!(Arc::ptr_eq(&first.inner, &second.inner));

        let first_segment = first.allocate_segment("work", "raw", "fifo").unwrap();
        fs::write(&first_segment.path, b"one").unwrap();
        first
            .commit_allocated_segment(&first_segment, 3, 1)
            .unwrap();

        let second_segment = second.allocate_segment("work", "raw", "fifo").unwrap();
        assert_eq!(first_segment.id, "0000000000000000");
        assert_eq!(second_segment.id, "0000000000000001");
        assert_ne!(first_segment.path, second_segment.path);
    }

    #[test]
    fn from_managed_path_reuses_live_store_instance() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let segment = write_segment(&store, "history", b"hello", 1);

        let reopened = TieredStore::from_managed_path(&segment.path)
            .unwrap()
            .expect("managed store");

        assert!(Arc::ptr_eq(&store.inner, &reopened.inner));
        assert!(
            reopened
                .segment_ref_for_path(&segment.path)
                .unwrap()
                .is_some()
        );

        let next = reopened.allocate_segment("work", "raw", "fifo").unwrap();
        assert_eq!(next.id, "0000000000000001");
    }

    #[test]
    fn snapshot_namespace_references_existing_segments() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let history = store.open_runset("default", "history").unwrap();

        let segment = store.allocate_segment("history", "zstd", "sorted").unwrap();
        fs::write(&segment.path, b"hello").unwrap();
        store.commit_allocated_segment(&segment, 5, 1).unwrap();
        history.append_path(&segment.path).unwrap();

        store.snapshot_namespace("default", "snap").unwrap();
        let snap = store.open_runset("snap", "history").unwrap();
        assert_eq!(snap.segment_paths().unwrap(), vec![segment.path.clone()]);
        let meta = store.segment_ref_for_path(&segment.path).unwrap().unwrap();
        assert_eq!(meta.path, segment.path);
    }

    #[test]
    fn refuses_legacy_layout_without_catalog() {
        let temp = tempdir().unwrap();
        fs::create_dir_all(temp.path().join("history")).unwrap();
        let err = TieredStore::open(temp.path()).unwrap_err();
        assert!(err.to_string().contains("legacy store layout"));
    }

    #[test]
    fn opening_existing_handles_does_not_flush_catalog() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let before = store.debug_counters();

        store.open_queue("default", "work").unwrap();
        store.open_runset("default", "history").unwrap();
        store.open_log("default", "landing").unwrap();
        store.open_blob("default", "control").unwrap();

        let after_first_open = store.debug_counters();
        assert_eq!(after_first_open.catalog_flushes, before.catalog_flushes);

        store.open_queue("default", "work").unwrap();
        store.open_runset("default", "history").unwrap();
        store.open_log("default", "landing").unwrap();
        store.open_blob("default", "control").unwrap();

        let after_second_open = store.debug_counters();
        assert_eq!(
            after_second_open.catalog_flushes,
            after_first_open.catalog_flushes
        );
    }

    #[test]
    fn replace_paths_batches_catalog_flushes() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let history = store.open_runset("default", "history").unwrap();

        let first = write_segment(&store, "history", b"one", 1);
        let second = write_segment(&store, "history", b"two", 1);
        let third = write_segment(&store, "history", b"three", 1);
        let fourth = write_segment(&store, "history", b"four", 1);

        history
            .replace_paths(&[first.path.clone(), second.path.clone(), third.path.clone()])
            .unwrap();

        let before = store.debug_counters();
        history
            .replace_paths(&[second.path.clone(), fourth.path.clone()])
            .unwrap();
        let after = store.debug_counters();

        assert_eq!(after.catalog_flushes, before.catalog_flushes + 1);
        assert_eq!(after.manifest_flushes, before.manifest_flushes + 1);
    }

    #[test]
    fn metadata_reads_use_in_memory_catalog() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let queue = store.open_queue("default", "work").unwrap();

        let segment = write_segment(&store, "work", b"abc", 2);
        queue.push_back_path(&segment.path).unwrap();
        store
            .set_remote_state_for_path(&segment.path, true, Some("remote-key".to_string()))
            .unwrap();
        store.set_disk_state_for_path(&segment.path, false).unwrap();

        let before = store.debug_counters();
        assert_eq!(store.current_epoch().unwrap(), 0);
        assert_eq!(store.managed_file_size(&segment.path).unwrap(), Some(3));
        let segment_ref = store.segment_ref_for_path(&segment.path).unwrap().unwrap();
        assert_eq!(segment_ref.record_count, 2);
        assert!(store.is_managed_remote_only(&segment.path).unwrap());
        let after = store.debug_counters();

        assert_eq!(after.catalog_loads, before.catalog_loads);
        assert!(after.segment_id_lookups > before.segment_id_lookups);
    }

    #[test]
    fn reclaim_candidates_are_committed_local_segments_only() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();

        let local = write_segment(&store, "work", b"local", 1);

        let remote = write_segment(&store, "history", b"remote", 1);
        store
            .set_remote_state_for_path(&remote.path, true, Some("remote-key".to_string()))
            .unwrap();

        let missing = write_segment(&store, "spill", b"missing", 1);
        fs::remove_file(&missing.path).unwrap();

        let _uncommitted = store.allocate_segment("new-work", "raw", "fifo").unwrap();

        let scan = store.reclaim_candidates_with_summary().unwrap();
        let mut ids: Vec<_> = scan
            .candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect();
        ids.sort();
        assert_eq!(ids, vec![local.id.clone(), remote.id.clone()]);
        assert_eq!(scan.skipped_remote, 0);
        assert_eq!(scan.skipped_missing_local, 1);
        assert_eq!(scan.skipped_zero_bytes, 1);
    }

    #[test]
    fn pinned_segments_are_not_reclaim_candidates() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let pinned = write_segment(&store, "chunk", b"pinned", 1);
        let unpinned = write_segment(&store, "history", b"unpinned", 1);

        let _guard = store.pin_segment(&pinned.id, "test").unwrap();
        let scan = store.reclaim_candidates_with_summary().unwrap();

        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.candidates[0].id, unpinned.id);
        assert_eq!(scan.skipped_pinned, 1);
        assert_eq!(scan.skipped_pinned_bytes, 6);
    }

    #[derive(Default)]
    struct MemoryRemote {
        objects: Mutex<BTreeMap<String, Vec<u8>>>,
        download_failures: Mutex<BTreeMap<String, usize>>,
        short_downloads: Mutex<BTreeMap<String, usize>>,
    }

    impl MemoryRemote {
        fn fail_downloads(&self, key: &str, times: usize) {
            self.download_failures
                .lock()
                .unwrap()
                .insert(key.to_string(), times);
        }

        fn short_downloads(&self, key: &str, times: usize) {
            self.short_downloads
                .lock()
                .unwrap()
                .insert(key.to_string(), times);
        }

        fn decrement(map: &Mutex<BTreeMap<String, usize>>, key: &str) -> bool {
            let mut guard = map.lock().unwrap();
            if let Some(remaining) = guard.get_mut(key) {
                if *remaining > 0 {
                    *remaining -= 1;
                    return true;
                }
            }
            false
        }
    }

    impl SegmentRemote for MemoryRemote {
        fn upload_segment(&self, path: &Path) -> io::Result<String> {
            let key = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("segment")
                .to_string();
            self.objects
                .lock()
                .unwrap()
                .insert(key.clone(), fs::read(path)?);
            Ok(key)
        }

        fn download_segment(&self, remote_key: &str, dest_path: &Path) -> io::Result<()> {
            if Self::decrement(&self.download_failures, remote_key) {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "injected transient body EOF",
                ));
            }
            let bytes = self
                .objects
                .lock()
                .unwrap()
                .get(remote_key)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing remote object"))?;
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            if Self::decrement(&self.short_downloads, remote_key) {
                return fs::write(dest_path, &bytes[..bytes.len().saturating_div(2)]);
            }
            fs::write(dest_path, bytes)
        }
    }

    #[test]
    fn remote_only_segment_rehydrates_to_canonical_path() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let remote = Arc::new(MemoryRemote::default());
        configure_segment_remote(Some(remote));

        let segment = write_segment(&store, "chunk", b"payload", 1);
        let bytes = store.offload_segment(&segment.id, "test").unwrap();
        assert_eq!(bytes, 7);
        assert!(!segment.path.exists());

        let local = store.ensure_local_segment(&segment.id, "test").unwrap();
        assert_eq!(local, segment.path);
        assert_eq!(fs::read(local).unwrap(), b"payload");
        assert!(!store.is_managed_remote_only(&segment.path).unwrap());

        configure_segment_remote(None);
    }

    #[test]
    fn remote_only_segment_retries_failed_download_and_records_metrics() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let remote = Arc::new(MemoryRemote::default());
        configure_segment_remote(Some(remote.clone()));
        let metrics = crate::metrics::Metrics::new();
        set_segment_metrics_handle(Some(metrics.clone_handle()));

        let segment = write_segment(&store, "work", b"payload", 1);
        store.offload_segment(&segment.id, "test").unwrap();
        let remote_key = segment
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .to_string();
        remote.fail_downloads(&remote_key, 1);

        let local = store.ensure_local_segment(&segment.id, "test").unwrap();
        assert_eq!(local, segment.path);
        assert_eq!(fs::read(local).unwrap(), b"payload");

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.global.downloaded_files, 1);
        assert_eq!(snapshot.global.downloaded_bytes, 7);
        assert!(
            snapshot
                .logs
                .iter()
                .any(|entry| entry.message.contains("Segment rehydrate retry"))
        );
        assert!(
            snapshot
                .logs
                .iter()
                .any(|entry| entry.message.contains("Segment rehydrate success"))
        );

        set_segment_metrics_handle(None);
        configure_segment_remote(None);
    }

    #[test]
    fn exhausted_rehydrate_deletes_temp_and_leaves_segment_remote_only() {
        let temp = tempdir().unwrap();
        let store = TieredStore::open(temp.path()).unwrap();
        let remote = Arc::new(MemoryRemote::default());
        configure_segment_remote(Some(remote.clone()));

        let segment = write_segment(&store, "work", b"payload", 1);
        store.offload_segment(&segment.id, "test").unwrap();
        let remote_key = segment
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .to_string();
        remote.short_downloads(&remote_key, SEGMENT_REHYDRATE_RETRIES + 1);

        let err = store.ensure_local_segment(&segment.id, "test").unwrap_err();
        assert!(err.to_string().contains("remote_download_exhausted"));
        assert!(!segment.path.exists());
        assert!(!segment.path.with_extension("download_tmp").exists());
        assert!(store.is_managed_remote_only(&segment.path).unwrap());

        configure_segment_remote(None);
    }

    #[test]
    fn open_scrubs_stale_download_temp_files() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("store");
        let tmp = {
            let store = TieredStore::open(&root).unwrap();
            let tmp = store.segments_dir().join("0000000000000000.download_tmp");
            fs::write(&tmp, b"partial").unwrap();
            tmp
        };

        assert!(tmp.exists());
        let _reopened = TieredStore::open(&root).unwrap();
        assert!(!tmp.exists());
    }
}
