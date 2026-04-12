use crate::{
    FoldError,
    generation_store::GenerationStore,
    ortho::{Ortho, OrthoScore},
    tiered_store::TieredStore,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const RESUME_SCHEMA_VERSION: u32 = 2;
pub const MANIFEST_FILENAME: &str = "resume_manifest.json";
pub const CHECKPOINTS_DIRNAME: &str = "checkpoints";
pub const CHECKPOINT_PREFIX: &str = "store-";
pub const WORKING_SUFFIX: &str = "-working";
pub const BEST_ORTHO_FILENAME: &str = "best_ortho.bin";
const CONTROL_BLOBS: &str = "control";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumePhase {
    Claimed,
    LargerLoaded,
    SmallerLoaded,
    GenerationCommitted,
    Quiesced,
    Archiving,
    Archived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BestScoreSnapshot {
    pub volume: usize,
    pub variance_num: u128,
    pub variance_den: u128,
    pub fullness: usize,
}

impl From<OrthoScore> for BestScoreSnapshot {
    fn from(value: OrthoScore) -> Self {
        Self {
            volume: value.volume,
            variance_num: value.variance_num,
            variance_den: value.variance_den,
            fullness: value.fullness,
        }
    }
}

impl From<BestScoreSnapshot> for OrthoScore {
    fn from(value: BestScoreSnapshot) -> Self {
        Self {
            volume: value.volume,
            variance_num: value.variance_num,
            variance_den: value.variance_den.max(1),
            fullness: value.fullness,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeResumeManifest {
    pub schema_version: u32,
    pub phase: ResumePhase,
    pub generation: u64,
    pub active_store_dir: Option<String>,
    pub archive_a_path: String,
    pub archive_b_path: String,
    pub a_is_smaller: bool,
    pub merge_policy: String,
    pub best_score: BestScoreSnapshot,
    pub best_ortho_file: Option<String>,
    pub archive_temp_path: Option<String>,
    pub updated_at: u64,
}

impl MergeResumeManifest {
    pub fn new(
        phase: ResumePhase,
        archive_a_path: String,
        archive_b_path: String,
        a_is_smaller: bool,
        merge_policy: String,
        best_score: OrthoScore,
    ) -> Self {
        Self {
            schema_version: RESUME_SCHEMA_VERSION,
            phase,
            generation: 0,
            active_store_dir: None,
            archive_a_path,
            archive_b_path,
            a_is_smaller,
            merge_policy,
            best_score: best_score.into(),
            best_ortho_file: None,
            archive_temp_path: None,
            updated_at: current_timestamp_secs(),
        }
    }

    pub fn best_score(&self) -> OrthoScore {
        self.best_score.into()
    }

    pub fn active_store_namespace(&self) -> Option<&str> {
        self.active_store_dir.as_deref()
    }

    pub fn archive_temp_path(&self, merge_work_dir: &Path) -> Option<PathBuf> {
        self.archive_temp_path
            .as_ref()
            .map(|rel| merge_work_dir.join(rel))
    }
}

#[derive(Clone, Debug)]
pub struct MergeCheckpointPaths {
    pub store_root: PathBuf,
    pub working_namespace: String,
    pub committed_namespace: String,
    pub previous_committed_namespace: Option<String>,
}

pub fn manifest_path(merge_work_dir: &Path) -> PathBuf {
    merge_work_dir.join(MANIFEST_FILENAME)
}

pub fn checkpoints_dir(merge_work_dir: &Path) -> PathBuf {
    merge_work_dir.join(CHECKPOINTS_DIRNAME)
}

pub fn read_manifest(merge_work_dir: &Path) -> Result<MergeResumeManifest, FoldError> {
    let bytes = fs::read(manifest_path(merge_work_dir)).map_err(FoldError::Io)?;
    serde_json::from_slice(&bytes)
        .map_err(|err| FoldError::Other(format!("invalid resume manifest: {}", err)))
}

pub fn write_manifest_atomic(
    merge_work_dir: &Path,
    manifest: &MergeResumeManifest,
) -> Result<(), FoldError> {
    fs::create_dir_all(merge_work_dir).map_err(FoldError::Io)?;
    let path = manifest_path(merge_work_dir);
    let tmp_path = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|err| FoldError::Other(format!("resume manifest encode failed: {}", err)))?;
    fs::write(&tmp_path, bytes).map_err(FoldError::Io)?;
    fs::rename(&tmp_path, &path).map_err(FoldError::Io)?;
    Ok(())
}

pub fn write_best_ortho(
    checkpoint_store_root: &Path,
    namespace: &str,
    ortho: &Ortho,
) -> Result<String, FoldError> {
    let store = TieredStore::open(checkpoint_store_root.to_path_buf()).map_err(FoldError::Io)?;
    if !store.has_namespace(namespace).map_err(FoldError::Io)? {
        return Err(FoldError::Other(format!(
            "missing checkpoint namespace {}",
            namespace
        )));
    }
    let blobs = store
        .open_blob(namespace, CONTROL_BLOBS)
        .map_err(FoldError::Io)?;
    let bytes = ortho.to_bytes()?;
    blobs
        .write_atomic(BEST_ORTHO_FILENAME, &bytes)
        .map_err(FoldError::Io)?;
    Ok(BEST_ORTHO_FILENAME.to_string())
}

pub fn load_best_ortho(
    merge_work_dir: &Path,
    manifest: &MergeResumeManifest,
) -> Result<Option<Ortho>, FoldError> {
    let Some(namespace) = manifest.active_store_namespace() else {
        return Ok(None);
    };
    let store = TieredStore::open(checkpoints_dir(merge_work_dir)).map_err(FoldError::Io)?;
    if !store.has_namespace(namespace).map_err(FoldError::Io)? {
        return Ok(None);
    }
    let blobs = store
        .open_blob(namespace, CONTROL_BLOBS)
        .map_err(FoldError::Io)?;
    let key = manifest
        .best_ortho_file
        .as_deref()
        .unwrap_or(BEST_ORTHO_FILENAME);
    let Some(bytes) = blobs.read(key).map_err(FoldError::Io)? else {
        return Ok(None);
    };
    Ok(Some(Ortho::from_bytes(&bytes)?))
}

pub fn cleanup_transient_checkpoints(merge_work_dir: &Path) -> io::Result<()> {
    let checkpoints = checkpoints_dir(merge_work_dir);
    if !checkpoints.exists() {
        return Ok(());
    }
    let store = TieredStore::open(checkpoints)?;
    for namespace in store.namespaces()? {
        if namespace.starts_with(CHECKPOINT_PREFIX) && namespace.ends_with(WORKING_SUFFIX) {
            store.delete_namespace(&namespace)?;
        }
    }
    Ok(())
}

pub fn validate_manifest(
    merge_work_dir: &Path,
    manifest: &MergeResumeManifest,
    bucket_count: usize,
) -> Result<(), FoldError> {
    if manifest.schema_version != RESUME_SCHEMA_VERSION {
        return Err(FoldError::Other(format!(
            "unsupported resume manifest schema version: {}",
            manifest.schema_version
        )));
    }
    if !Path::new(&manifest.archive_a_path).exists()
        || !Path::new(&manifest.archive_b_path).exists()
    {
        return Err(FoldError::Other(
            "resume manifest references missing source archives".to_string(),
        ));
    }
    if let Some(namespace) = manifest.active_store_namespace() {
        let checkpoint_root = checkpoints_dir(merge_work_dir);
        if !checkpoint_root.exists() {
            return Err(FoldError::Other(format!(
                "resume manifest points at missing checkpoint store {}",
                checkpoint_root.display()
            )));
        }
        let store = TieredStore::open(checkpoint_root.clone()).map_err(FoldError::Io)?;
        if !store.has_namespace(namespace).map_err(FoldError::Io)? {
            return Err(FoldError::Other(format!(
                "resume manifest points at missing checkpoint namespace {}",
                namespace
            )));
        }
        GenerationStore::from_existing_with_namespace(
            checkpoint_root,
            namespace.to_string(),
            bucket_count,
        )
        .map_err(FoldError::Io)
        .map(|_| ())?;
    }
    Ok(())
}

pub fn prepare_working_checkpoint(
    merge_work_dir: &Path,
    active_store_namespace: Option<&str>,
) -> Result<MergeCheckpointPaths, FoldError> {
    cleanup_transient_checkpoints(merge_work_dir).map_err(FoldError::Io)?;
    let store_root = checkpoints_dir(merge_work_dir);
    let store = TieredStore::open(store_root.clone()).map_err(FoldError::Io)?;
    let next_id = next_checkpoint_id(merge_work_dir).map_err(FoldError::Io)?;
    let committed_name = format!("{}{:04}", CHECKPOINT_PREFIX, next_id);
    let working_name = format!("{}{}", committed_name, WORKING_SUFFIX);
    if let Some(active_namespace) = active_store_namespace {
        if !store
            .has_namespace(active_namespace)
            .map_err(FoldError::Io)?
        {
            return Err(FoldError::Other(format!(
                "missing active checkpoint namespace {}",
                active_namespace
            )));
        }
        store
            .snapshot_namespace(active_namespace, &working_name)
            .map_err(FoldError::Io)?;
    } else {
        store
            .delete_namespace(&working_name)
            .map_err(FoldError::Io)?;
        store
            .ensure_namespace(&working_name)
            .map_err(FoldError::Io)?;
    }

    Ok(MergeCheckpointPaths {
        store_root,
        working_namespace: working_name,
        committed_namespace: committed_name,
        previous_committed_namespace: active_store_namespace.map(|name| name.to_string()),
    })
}

pub fn finalize_checkpoint_commit(
    merge_work_dir: &Path,
    mut manifest: MergeResumeManifest,
    checkpoint: MergeCheckpointPaths,
    best_ortho: &Ortho,
) -> Result<MergeResumeManifest, FoldError> {
    let best_ortho_key = write_best_ortho(
        &checkpoint.store_root,
        &checkpoint.working_namespace,
        best_ortho,
    )?;
    let store = TieredStore::open(checkpoint.store_root.clone()).map_err(FoldError::Io)?;
    store
        .promote_namespace(
            &checkpoint.working_namespace,
            &checkpoint.committed_namespace,
        )
        .map_err(FoldError::Io)?;

    manifest.active_store_dir = Some(checkpoint.committed_namespace.clone());
    manifest.best_ortho_file = Some(best_ortho_key);
    manifest.archive_temp_path = None;
    manifest.updated_at = current_timestamp_secs();
    write_manifest_atomic(merge_work_dir, &manifest)?;

    if let Some(previous) = checkpoint.previous_committed_namespace {
        if previous != checkpoint.committed_namespace
            && store.has_namespace(&previous).map_err(FoldError::Io)?
        {
            store.delete_namespace(&previous).map_err(FoldError::Io)?;
        }
    }
    cleanup_transient_checkpoints(merge_work_dir).map_err(FoldError::Io)?;
    Ok(manifest)
}

pub fn current_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn next_checkpoint_id(merge_work_dir: &Path) -> io::Result<usize> {
    let checkpoints = checkpoints_dir(merge_work_dir);
    if !checkpoints.exists() {
        return Ok(0);
    }
    let store = TieredStore::open(checkpoints)?;
    let mut next_id = 0usize;
    for name in store.namespaces()? {
        if !name.starts_with(CHECKPOINT_PREFIX) || name.ends_with(WORKING_SUFFIX) {
            continue;
        }
        if let Some(id) = name
            .strip_prefix(CHECKPOINT_PREFIX)
            .and_then(|rest| rest.parse::<usize>().ok())
        {
            next_id = next_id.max(id.saturating_add(1));
        }
    }
    Ok(next_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn checkpoint_namespace_snapshot_is_isolated() {
        let temp = TempDir::new().unwrap();
        let store = TieredStore::open(checkpoints_dir(temp.path())).unwrap();
        store.ensure_namespace("store-0000").unwrap();
        let blobs = store.open_blob("store-0000", CONTROL_BLOBS).unwrap();
        blobs.write_atomic(BEST_ORTHO_FILENAME, b"alpha").unwrap();

        let checkpoint = prepare_working_checkpoint(temp.path(), Some("store-0000")).unwrap();
        let working_blobs = store
            .open_blob(&checkpoint.working_namespace, CONTROL_BLOBS)
            .unwrap();
        working_blobs
            .write_atomic(BEST_ORTHO_FILENAME, b"beta")
            .unwrap();

        let original = store.open_blob("store-0000", CONTROL_BLOBS).unwrap();
        assert_eq!(
            original.read(BEST_ORTHO_FILENAME).unwrap().unwrap(),
            b"alpha"
        );
    }

    #[test]
    fn manifest_roundtrip() {
        let temp = TempDir::new().unwrap();
        let manifest = MergeResumeManifest::new(
            ResumePhase::Claimed,
            "/a".to_string(),
            "/b".to_string(),
            true,
            "adjacent_balanced".to_string(),
            OrthoScore::zero(),
        );
        write_manifest_atomic(temp.path(), &manifest).unwrap();
        let loaded = read_manifest(temp.path()).unwrap();
        assert_eq!(loaded.phase, ResumePhase::Claimed);
        assert_eq!(loaded.archive_a_path, "/a");
    }
}
