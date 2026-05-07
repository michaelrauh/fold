use crate::{
    dfs_runner::{DfsConfig, DfsRunner},
    error::FoldError,
    interner::Interner,
    ortho::Ortho,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CHECKPOINT_VERSION: u32 = 6;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointManifest {
    pub version: u32,
    pub input_path: String,
    pub input_fingerprint: u64,
    pub config_fingerprint: u64,
    pub interner_path: String,
    pub state_path: String,
    pub started_unix: u64,
    pub checkpoint_unix: u64,
    pub checkpoint_status: String,
    pub nodes_expanded: u64,
    pub nodes_pruned: u64,
    pub completions_pruned: u64,
    pub current_depth: usize,
    pub max_depth: usize,
    pub open_siblings_total: u64,
    pub frontier_max_bound_volume: Option<usize>,
    pub frontier_max_bound_variance_num: Option<u128>,
    pub frontier_max_bound_variance_den: Option<u128>,
    pub frontier_max_bound_fullness: Option<usize>,
    pub last_improvement_unix: u64,
    pub last_improvement_depth: usize,
    pub incumbent_volume: usize,
    pub incumbent_variance_num: u128,
    pub incumbent_variance_den: u128,
    pub incumbent_fullness: usize,
    pub incumbent_dims: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct LoadedCheckpoint {
    pub manifest: CheckpointManifest,
    pub runner: DfsRunner,
    pub interner: Interner,
}

#[derive(Clone, Debug)]
pub struct CheckpointManager {
    root: PathBuf,
    manifest_path: PathBuf,
    state_path: PathBuf,
    interner_path: PathBuf,
    output_dir: PathBuf,
}

impl CheckpointManager {
    pub fn new(root: PathBuf) -> Result<Self, FoldError> {
        let checkpoint_dir = root.join("checkpoints");
        let output_dir = root.join("output");
        fs::create_dir_all(&checkpoint_dir)?;
        fs::create_dir_all(&output_dir)?;
        Ok(Self {
            root,
            manifest_path: checkpoint_dir.join("current.manifest.json"),
            state_path: checkpoint_dir.join("current.state.bin"),
            interner_path: checkpoint_dir.join("interner.bin"),
            output_dir,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    pub fn interner_path(&self) -> &Path {
        &self.interner_path
    }

    pub fn has_checkpoint(&self) -> bool {
        self.manifest_path.exists() && self.state_path.exists() && self.interner_path.exists()
    }

    pub fn load(&self) -> Result<Option<LoadedCheckpoint>, FoldError> {
        if !self.has_checkpoint() {
            return Ok(None);
        }
        let manifest_bytes = fs::read(&self.manifest_path)?;
        let manifest: CheckpointManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| FoldError::Deserialization(e.to_string()))?;
        if manifest.version == 1 {
            return Err(FoldError::Other(
                "checkpoint format changed; start a fresh DFS run".to_string(),
            ));
        }
        if manifest.version != CHECKPOINT_VERSION {
            return Err(FoldError::Other(format!(
                "unsupported checkpoint version {}",
                manifest.version
            )));
        }
        let state_bytes = fs::read(&self.state_path)?;
        let runner = DfsRunner::from_bytes(&state_bytes)?;
        let interner_bytes = fs::read(&self.interner_path)?;
        let interner = Interner::from_bytes(&interner_bytes)?;
        Ok(Some(LoadedCheckpoint {
            manifest,
            runner,
            interner,
        }))
    }

    pub fn write_interner(&self, interner: &Interner) -> Result<(), FoldError> {
        write_atomic(&self.interner_path, &interner.to_bytes()?)
    }

    pub fn save(
        &self,
        runner: &DfsRunner,
        input_path: &Path,
        input_fingerprint: u64,
        config: &DfsConfig,
        status: &str,
    ) -> Result<CheckpointManifest, FoldError> {
        let snapshot = runner.snapshot();
        let incumbent = snapshot.incumbent;
        let now = now_unix();
        let manifest = CheckpointManifest {
            version: CHECKPOINT_VERSION,
            input_path: input_path.display().to_string(),
            input_fingerprint,
            config_fingerprint: config.fingerprint(),
            interner_path: file_name_string(&self.interner_path),
            state_path: file_name_string(&self.state_path),
            started_unix: snapshot.started_unix,
            checkpoint_unix: now,
            checkpoint_status: status.to_string(),
            nodes_expanded: snapshot.nodes_expanded,
            nodes_pruned: snapshot.nodes_pruned,
            completions_pruned: snapshot.completions_pruned,
            current_depth: snapshot.current_depth,
            max_depth: snapshot.max_depth,
            open_siblings_total: snapshot.open_siblings_total,
            frontier_max_bound_volume: snapshot.frontier_max_bound.map(|bound| bound.volume),
            frontier_max_bound_variance_num: snapshot
                .frontier_max_bound
                .map(|bound| bound.variance_num),
            frontier_max_bound_variance_den: snapshot
                .frontier_max_bound
                .map(|bound| bound.variance_den),
            frontier_max_bound_fullness: snapshot.frontier_max_bound.map(|bound| bound.fullness),
            last_improvement_unix: snapshot.last_improvement_unix,
            last_improvement_depth: snapshot.last_improvement_depth,
            incumbent_volume: incumbent.score().volume,
            incumbent_variance_num: incumbent.score().variance_num,
            incumbent_variance_den: incumbent.score().variance_den,
            incumbent_fullness: incumbent.score().fullness,
            incumbent_dims: incumbent.dims().to_vec(),
        };
        write_atomic(&self.state_path, &runner.to_bytes()?)?;
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| FoldError::Serialization(e.to_string()))?;
        write_atomic(&self.manifest_path, &manifest_bytes)?;
        Ok(manifest)
    }

    pub fn save_optimal(
        &self,
        best: &Ortho,
        display: &str,
        summary: &serde_json::Value,
    ) -> Result<(), FoldError> {
        write_atomic(&self.output_dir.join("optimal.bin"), &best.to_bytes()?)?;
        write_atomic(&self.output_dir.join("optimal.txt"), display.as_bytes())?;
        let summary_bytes = serde_json::to_vec_pretty(summary)
            .map_err(|e| FoldError::Serialization(e.to_string()))?;
        write_atomic(&self.output_dir.join("summary.json"), &summary_bytes)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn file_name_string(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), FoldError> {
    let parent = path.parent().ok_or_else(|| {
        FoldError::Io(std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("path {} has no parent", path.display()),
        ))
    })?;
    fs::create_dir_all(parent)?;
    let tmp_name = format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("state"),
        std::process::id()
    );
    let tmp_path = parent.join(tmp_name);
    fs::write(&tmp_path, bytes)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}
