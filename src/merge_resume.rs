use crate::{
    FoldError,
    generation_store::GenerationStore,
    ortho::{Ortho, OrthoScore},
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const RESUME_SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_FILENAME: &str = "resume_manifest.json";
pub const CHECKPOINTS_DIRNAME: &str = "checkpoints";
pub const CHECKPOINT_PREFIX: &str = "store-";
pub const WORKING_SUFFIX: &str = "-working";
pub const BEST_ORTHO_FILENAME: &str = "best_ortho.bin";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumePhase {
    Claimed,
    LargerLoaded,
    SmallerLoaded,
    GenerationCommitted,
    Quiesced,
    Pruned,
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

    pub fn active_store_path(&self, merge_work_dir: &Path) -> Option<PathBuf> {
        self.active_store_dir
            .as_ref()
            .map(|rel| merge_work_dir.join(rel))
    }

    pub fn best_ortho_path(&self, merge_work_dir: &Path) -> Option<PathBuf> {
        self.best_ortho_file
            .as_ref()
            .map(|rel| merge_work_dir.join(rel))
    }

    pub fn archive_temp_path(&self, merge_work_dir: &Path) -> Option<PathBuf> {
        self.archive_temp_path
            .as_ref()
            .map(|rel| merge_work_dir.join(rel))
    }
}

#[derive(Clone, Debug)]
pub struct MergeCheckpointPaths {
    pub working_dir: PathBuf,
    pub committed_dir: PathBuf,
    pub committed_rel: String,
    pub previous_committed_dir: Option<PathBuf>,
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

pub fn write_best_ortho(store_dir: &Path, ortho: &Ortho) -> Result<String, FoldError> {
    let path = store_dir.join(BEST_ORTHO_FILENAME);
    let tmp_path = path.with_extension("bin.tmp");
    let bytes = ortho.to_bytes()?;
    fs::write(&tmp_path, bytes).map_err(FoldError::Io)?;
    fs::rename(&tmp_path, &path).map_err(FoldError::Io)?;
    Ok(relative_store_entry(store_dir, BEST_ORTHO_FILENAME))
}

pub fn load_best_ortho(
    merge_work_dir: &Path,
    manifest: &MergeResumeManifest,
) -> Result<Option<Ortho>, FoldError> {
    let Some(path) = manifest.best_ortho_path(merge_work_dir) else {
        return Ok(None);
    };
    let bytes = fs::read(path).map_err(FoldError::Io)?;
    Ok(Some(Ortho::from_bytes(&bytes)?))
}

pub fn cleanup_transient_checkpoints(merge_work_dir: &Path) -> io::Result<()> {
    let checkpoints = checkpoints_dir(merge_work_dir);
    if !checkpoints.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(&checkpoints)? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() && name.starts_with(CHECKPOINT_PREFIX) && name.ends_with(WORKING_SUFFIX) {
            fs::remove_dir_all(path)?;
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
    if let Some(active_store) = manifest.active_store_path(merge_work_dir) {
        if !active_store.exists() {
            return Err(FoldError::Other(format!(
                "resume manifest points at missing checkpoint {}",
                active_store.display()
            )));
        }
        GenerationStore::from_existing(active_store, bucket_count)
            .map_err(FoldError::Io)
            .map(|_| ())?;
    }
    Ok(())
}

pub fn prepare_working_checkpoint(
    merge_work_dir: &Path,
    active_store_dir: Option<&Path>,
) -> Result<MergeCheckpointPaths, FoldError> {
    fs::create_dir_all(checkpoints_dir(merge_work_dir)).map_err(FoldError::Io)?;
    cleanup_transient_checkpoints(merge_work_dir).map_err(FoldError::Io)?;

    let next_id = next_checkpoint_id(merge_work_dir).map_err(FoldError::Io)?;
    let committed_name = format!("{}{:04}", CHECKPOINT_PREFIX, next_id);
    let working_name = format!("{}{}", committed_name, WORKING_SUFFIX);
    let checkpoints = checkpoints_dir(merge_work_dir);
    let committed_dir = checkpoints.join(&committed_name);
    let working_dir = checkpoints.join(&working_name);

    if working_dir.exists() {
        fs::remove_dir_all(&working_dir).map_err(FoldError::Io)?;
    }

    if let Some(active) = active_store_dir {
        clone_dir_hardlinked(active, &working_dir).map_err(FoldError::Io)?;
    }

    let committed_rel = format!("{}/{}", CHECKPOINTS_DIRNAME, committed_name);
    Ok(MergeCheckpointPaths {
        working_dir,
        committed_dir,
        committed_rel,
        previous_committed_dir: active_store_dir.map(|path| path.to_path_buf()),
    })
}

pub fn finalize_checkpoint_commit(
    merge_work_dir: &Path,
    mut manifest: MergeResumeManifest,
    checkpoint: MergeCheckpointPaths,
    best_ortho: &Ortho,
) -> Result<MergeResumeManifest, FoldError> {
    let best_ortho_rel = write_best_ortho(&checkpoint.working_dir, best_ortho)?;
    if checkpoint.committed_dir.exists() {
        fs::remove_dir_all(&checkpoint.committed_dir).map_err(FoldError::Io)?;
    }
    fs::rename(&checkpoint.working_dir, &checkpoint.committed_dir).map_err(FoldError::Io)?;

    manifest.active_store_dir = Some(checkpoint.committed_rel);
    manifest.best_ortho_file = Some(
        best_ortho_rel.replace(
            &format!(
                "{}/{}",
                CHECKPOINTS_DIRNAME,
                checkpoint
                    .working_dir
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
            ),
            manifest.active_store_dir.as_ref().unwrap(),
        ),
    );
    manifest.archive_temp_path = None;
    manifest.updated_at = current_timestamp_secs();
    write_manifest_atomic(merge_work_dir, &manifest)?;

    if let Some(previous) = checkpoint.previous_committed_dir {
        if previous.exists() {
            fs::remove_dir_all(previous).map_err(FoldError::Io)?;
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

pub fn checkpoint_relative_path(merge_work_dir: &Path, path: &Path) -> Result<String, FoldError> {
    let relative = path.strip_prefix(merge_work_dir).map_err(|err| {
        FoldError::Other(format!("checkpoint path not under merge work dir: {}", err))
    })?;
    Ok(relative.to_string_lossy().to_string())
}

fn next_checkpoint_id(merge_work_dir: &Path) -> io::Result<usize> {
    let checkpoints = checkpoints_dir(merge_work_dir);
    if !checkpoints.exists() {
        return Ok(0);
    }
    let mut next_id = 0usize;
    for entry in fs::read_dir(&checkpoints)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
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

fn clone_dir_hardlinked(src: &Path, dst: &Path) -> io::Result<()> {
    if dst.exists() {
        fs::remove_dir_all(dst)?;
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            clone_dir_hardlinked(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            clone_file_for_checkpoint(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn clone_file_for_checkpoint(src: &Path, dst: &Path) -> io::Result<()> {
    if is_mutable_checkpoint_file(src) {
        let _ = fs::copy(src, dst)?;
        return Ok(());
    }
    match fs::hard_link(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = fs::copy(src, dst)?;
            Ok(())
        }
    }
}

fn is_mutable_checkpoint_file(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if file_name == "active.log" || file_name == "manifest.txt" || file_name == BEST_ORTHO_FILENAME
    {
        return true;
    }
    path.components()
        .any(|component| component.as_os_str() == "landing")
}

fn relative_store_entry(store_dir: &Path, filename: &str) -> String {
    let checkpoints_root = store_dir
        .parent()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| CHECKPOINTS_DIRNAME.to_string());
    let store_name = store_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| CHECKPOINT_PREFIX.to_string());
    format!("{}/{}/{}", checkpoints_root, store_name, filename)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn checkpoint_clone_copies_mutable_landing_files() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("src");
        let dst = temp.path().join("dst");
        fs::create_dir_all(src.join("landing").join("b=00")).unwrap();
        fs::create_dir_all(src.join("history").join("b=00")).unwrap();
        let landing = src.join("landing").join("b=00").join("active.log");
        let history = src.join("history").join("b=00").join("history-0.dat");
        fs::write(&landing, b"landing").unwrap();
        fs::write(&history, b"history").unwrap();

        clone_dir_hardlinked(&src, &dst).unwrap();
        fs::write(
            &dst.join("landing").join("b=00").join("active.log"),
            b"changed",
        )
        .unwrap();

        assert_eq!(fs::read(&landing).unwrap(), b"landing");
        assert_eq!(
            fs::read(dst.join("history").join("b=00").join("history-0.dat")).unwrap(),
            b"history"
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
