use crate::FoldError;
use aws_config::Region;
use aws_sdk_s3::{primitives::ByteStream, types::Object, Client};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;

#[derive(Clone)]
pub struct S3State {
    client: Client,
    bucket: String,
    prefix: String,
    worker_id: String,
    runtime: Arc<Runtime>,
}

#[derive(Clone)]
pub struct S3StateConfig {
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub worker_id: String,
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LeaseRecord {
    worker_id: String,
    last_heartbeat: u64,
}

pub struct RemoteJob {
    pub job_key: String,
    pub local_path: PathBuf,
}

pub struct RemoteArchivePair {
    pub archive_a_key: String,
    pub archive_b_key: String,
    pub local_a: PathBuf,
    pub local_b: PathBuf,
}

impl LeaseRecord {
    fn is_stale(&self, now: u64, grace: u64) -> bool {
        now.saturating_sub(self.last_heartbeat) > grace
    }
}

impl S3StateConfig {
    pub fn from_env() -> Result<Option<Self>, FoldError> {
        let bucket = match std::env::var("FOLD_S3_BUCKET") {
            Ok(b) if !b.is_empty() => b,
            _ => return Ok(None),
        };

        let region = std::env::var("FOLD_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());
        let prefix = std::env::var("FOLD_S3_PREFIX").unwrap_or_else(|_| "fold".to_string());
        let worker_id = std::env::var("FOLD_WORKER_ID")
            .unwrap_or_else(|_| format!("{}-{}", std::env::var("HOSTNAME").unwrap_or_else(|_| "worker".to_string()), std::process::id()));
        let endpoint = std::env::var("FOLD_S3_ENDPOINT").ok();

        Ok(Some(Self {
            bucket,
            region,
            prefix,
            worker_id,
            endpoint,
        }))
    }
}

impl S3State {
    pub fn new(config: S3StateConfig) -> Result<Self, FoldError> {
        let runtime = Runtime::new().map_err(|e| FoldError::Other(format!("tokio runtime error: {}", e)))?;
        let region = Region::new(config.region.clone());
        let base_config = runtime.block_on(aws_config::from_env().region(region.clone()).load());

        let mut s3_builder = aws_sdk_s3::config::Builder::from(&base_config).region(region);
        if let Some(endpoint) = &config.endpoint {
            s3_builder = s3_builder.endpoint_url(endpoint);
            s3_builder = s3_builder.force_path_style(true);
        }

        let client = Client::from_conf(s3_builder.build());

        Ok(Self {
            client,
            bucket: config.bucket,
            prefix: config.prefix,
            worker_id: config.worker_id,
            runtime: Arc::new(runtime),
        })
    }

    pub fn try_from_env() -> Result<Option<Self>, FoldError> {
        if let Some(cfg) = S3StateConfig::from_env()? {
            Ok(Some(Self::new(cfg)?))
        } else {
            Ok(None)
        }
    }

    pub fn recover_stale_leases(&self, grace_secs: u64) -> Result<(), FoldError> {
        let objects = self.list_objects("leases/")?;
        let now = now_seconds();

        for object in objects {
            if let Some(key) = object.key() {
                let rel = self.strip_prefix(key);
                if let Some(record) = self.read_lease(rel)? {
                    if record.is_stale(now, grace_secs) {
                        self.delete_object(rel)?;
                    }
                } else {
                    self.delete_object(rel)?;
                }
            }
        }

        Ok(())
    }

    pub fn count_available_txt(&self, grace_secs: u64) -> Result<usize, FoldError> {
        let mut count = 0usize;
        let now = now_seconds();

        for object in self.list_objects("input/")? {
            if let Some(key) = object.key() {
                let rel = self.strip_prefix(key);
                if !rel.starts_with("input/") || !rel.ends_with(".txt") {
                    continue;
                }

                let lease_key = format!("leases/{}", rel);
                let lease = self.read_lease(&lease_key)?;
                if lease
                    .map(|l| l.worker_id == self.worker_id || l.is_stale(now, grace_secs))
                    .unwrap_or(true)
                {
                    count += 1;
                }
            }
        }

        Ok(count)
    }

    pub fn count_active_leases(&self, grace_secs: u64) -> Result<usize, FoldError> {
        let objects = self.list_objects("leases/")?;
        let now = now_seconds();
        let mut count = 0usize;

        for object in objects {
            if let Some(key) = object.key() {
                let rel = self.strip_prefix(key);
                if let Some(record) = self.read_lease(rel)? {
                    if !record.is_stale(now, grace_secs) {
                        count += 1;
                    }
                }
            }
        }

        Ok(count)
    }

    pub fn checkout_next_txt(&self, local_input_dir: &Path, grace_secs: u64) -> Result<Option<RemoteJob>, FoldError> {
        let mut candidates = Vec::new();

        for object in self.list_objects("input/")? {
            if let Some(key) = object.key() {
                let rel = self.strip_prefix(key);
                if !rel.ends_with(".txt") {
                    continue;
                }
                let size = object.size().unwrap_or_default();
                candidates.push((rel.to_string(), size));
            }
        }

        if candidates.is_empty() {
            return Ok(None);
        }

        candidates.sort_by_key(|(_, size)| *size);
        candidates.reverse();

        for (job_key, _) in candidates {
            if self.lease_job(&job_key, grace_secs)? {
                let dest = local_input_dir.join(job_key.trim_start_matches("input/"));
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(FoldError::Io)?;
                }
                self.download_object(&job_key, &dest)?;
                return Ok(Some(RemoteJob {
                    job_key,
                    local_path: dest,
                }));
            }
        }

        Ok(None)
    }

    pub fn checkout_two_archives(
        &self,
        local_input_dir: &Path,
        grace_secs: u64,
    ) -> Result<Option<RemoteArchivePair>, FoldError> {
        let mut archives = Vec::new();

        for object in self.list_objects("input/")? {
            if let Some(key) = object.key() {
                let rel = self.strip_prefix(key);
                if let Some(archive_name) = rel
                    .strip_prefix("input/")
                    .and_then(|s| s.strip_suffix("/metadata.txt"))
                {
                    let job_key = format!("input/{}", archive_name);
                    let lease = self.read_lease(&format!("leases/{}", job_key))?;
                    let now = now_seconds();
                    if lease
                        .map(|l| l.worker_id != self.worker_id && !l.is_stale(now, grace_secs))
                        .unwrap_or(false)
                    {
                        continue;
                    }

                    let meta_path = format!("{}/metadata.txt", job_key);
                    let ortho_count = self.read_metadata(&meta_path)?;
                    archives.push((job_key, ortho_count));
                }
            }
        }

        if archives.len() < 2 {
            return Ok(None);
        }

        archives.sort_by_key(|(_, count)| *count);
        archives.reverse();

        let first = archives[0].clone();
        let second = archives[1].clone();

        if !self.lease_job(&first.0, grace_secs)? {
            return Ok(None);
        }
        if !self.lease_job(&second.0, grace_secs)? {
            self.release_lease(&first.0)?;
            return Ok(None);
        }

        let local_a = self.download_archive(&first.0, local_input_dir)?;
        let local_b = self.download_archive(&second.0, local_input_dir)?;

        Ok(Some(RemoteArchivePair {
            archive_a_key: first.0,
            archive_b_key: second.0,
            local_a,
            local_b,
        }))
    }

    pub fn refresh_lease(&self, job_key: &str, grace_secs: u64) -> Result<(), FoldError> {
        let lease_key = format!("leases/{}", job_key);
        let now = now_seconds();
        if let Some(existing) = self.read_lease(&lease_key)? {
            if existing.worker_id != self.worker_id && !existing.is_stale(now, grace_secs) {
                return Err(FoldError::Other(format!("job {} leased by {}", job_key, existing.worker_id)));
            }
        }

        self.write_lease(&lease_key, now)
    }

    pub fn release_lease(&self, job_key: &str) -> Result<(), FoldError> {
        let lease_key = format!("leases/{}", job_key);
        self.delete_object(&lease_key)?;
        Ok(())
    }

    pub fn upload_archive(&self, archive_path: &Path) -> Result<(), FoldError> {
        let archive_name = archive_path
            .file_name()
            .ok_or_else(|| FoldError::Other("archive missing name".to_string()))?
            .to_string_lossy()
            .to_string();

        for entry in walk(archive_path)? {
            let relative = entry
                .strip_prefix(archive_path)
                .map_err(FoldError::Io)?
                .to_string_lossy()
                .to_string();
            let remote_key = format!("input/{}/{}", archive_name, relative);
            let bytes = fs::read(&entry).map_err(FoldError::Io)?;
            self.put_object(&remote_key, bytes)?;
        }

        Ok(())
    }

    pub fn delete_remote_archive(&self, archive_job_key: &str) -> Result<(), FoldError> {
        let objects = self.list_objects(&format!("{}/", archive_job_key.trim_end_matches('/')))?;
        for object in objects {
            if let Some(key) = object.key() {
                let rel = self.strip_prefix(key);
                self.delete_object(rel)?;
            }
        }
        Ok(())
    }

    pub fn finalize_txt_job(&self, job_key: &str) -> Result<(), FoldError> {
        let _ = self.delete_object(job_key);
        self.release_lease(job_key)
    }

    fn lease_job(&self, job_key: &str, grace_secs: u64) -> Result<bool, FoldError> {
        let lease_key = format!("leases/{}", job_key);
        let now = now_seconds();

        if let Some(existing) = self.read_lease(&lease_key)? {
            if existing.worker_id != self.worker_id && !existing.is_stale(now, grace_secs) {
                return Ok(false);
            }
        }

        self.write_lease(&lease_key, now)?;
        Ok(true)
    }

    fn write_lease(&self, lease_key: &str, heartbeat: u64) -> Result<(), FoldError> {
        let record = LeaseRecord {
            worker_id: self.worker_id.clone(),
            last_heartbeat: heartbeat,
        };
        let data = serde_json::to_vec(&record).map_err(|e| FoldError::Other(format!("lease serialize: {}", e)))?;
        self.put_object(lease_key, data)
    }

    fn read_lease(&self, lease_key: &str) -> Result<Option<LeaseRecord>, FoldError> {
        match self.get_object_bytes(lease_key) {
            Ok(bytes) => {
                if bytes.is_empty() {
                    return Ok(None);
                }
                let record: LeaseRecord = serde_json::from_slice(&bytes)
                    .map_err(|e| FoldError::Other(format!("lease decode: {}", e)))?;
                Ok(Some(record))
            }
            Err(FoldError::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn download_object(&self, key: &str, dest: &Path) -> Result<(), FoldError> {
        let bytes = self.get_object_bytes(key)?;
        fs::write(dest, bytes).map_err(FoldError::Io)
    }

    fn download_archive(&self, job_key: &str, local_input_dir: &Path) -> Result<PathBuf, FoldError> {
        let prefix = format!("{}/", job_key.trim_end_matches('/'));
        let objects = self.list_objects(&prefix)?;

        for object in objects {
            if let Some(full_key) = object.key() {
                let stripped = self.strip_prefix(full_key);
                let Some(rel) = stripped.strip_prefix("input/") else { continue };
                let dest = local_input_dir.join(rel);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(FoldError::Io)?;
                }
                let data = self.get_object_bytes(rel)?;
                fs::write(&dest, data).map_err(FoldError::Io)?;
            }
        }

        Ok(local_input_dir.join(job_key.trim_start_matches("input/")))
    }

    fn read_metadata(&self, meta_path: &str) -> Result<usize, FoldError> {
        let data = self.get_object_bytes(meta_path)?;
        let text = String::from_utf8_lossy(&data);
        text.trim()
            .parse::<usize>()
            .map_err(|e| FoldError::Other(format!("invalid metadata: {}", e)))
    }

    fn put_object(&self, key: &str, bytes: Vec<u8>) -> Result<(), FoldError> {
        let prefixed = self.prefixed(key);
        let body = ByteStream::from(bytes);
        self.runtime
            .block_on(
                self.client
                    .put_object()
                    .bucket(&self.bucket)
                    .key(prefixed)
                    .body(body)
                    .send(),
            )
            .map_err(|e| FoldError::Other(format!("S3 put {}: {}", key, e)))?;
        Ok(())
    }

    fn delete_object(&self, key: &str) -> Result<(), FoldError> {
        let prefixed = self.prefixed(key);
        self.runtime
            .block_on(
                self.client
                    .delete_object()
                    .bucket(&self.bucket)
                    .key(prefixed)
                    .send(),
            )
            .map_err(|e| FoldError::Other(format!("S3 delete {}: {}", key, e)))?;
        Ok(())
    }

    fn get_object_bytes(&self, key: &str) -> Result<Vec<u8>, FoldError> {
        let prefixed = self.prefixed(key);
        let resp = self
            .runtime
            .block_on(self.client.get_object().bucket(&self.bucket).key(prefixed).send())
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("NotFound") {
                    FoldError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, msg))
                } else {
                    FoldError::Other(format!("S3 get {}: {}", key, msg))
                }
            })?;

        let data = self
            .runtime
            .block_on(resp.body.collect())
            .map_err(|e| FoldError::Other(format!("S3 read {}: {}", key, e)))?
            .to_vec();
        Ok(data)
    }

    fn list_objects(&self, prefix: &str) -> Result<Vec<Object>, FoldError> {
        let mut token: Option<String> = None;
        let mut objects = Vec::new();

        let prefixed_prefix = self.prefixed(prefix);

        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefixed_prefix.clone());

            if let Some(ref cont) = token {
                request = request.continuation_token(cont);
            }

            let resp = self
                .runtime
                .block_on(request.send())
                .map_err(|e| FoldError::Other(format!("S3 list {}: {}", prefix, e)))?;

            if let Some(contents) = resp.contents() {
                objects.extend(contents.iter().cloned());
            }

            if resp.is_truncated() {
                token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        Ok(objects)
    }

    fn prefixed(&self, key: &str) -> String {
        let clean = key.trim_start_matches('/');
        if self.prefix.is_empty() {
            clean.to_string()
        } else {
            format!("{}/{}", self.prefix.trim_end_matches('/'), clean)
        }
    }

    fn strip_prefix<'a>(&self, key: &'a str) -> &'a str {
        if self.prefix.is_empty() {
            key
        } else if let Some(rest) = key.strip_prefix(&format!("{}/", self.prefix.trim_end_matches('/'))) {
            rest
        } else {
            key
        }
    }
}

fn walk(path: &Path) -> Result<Vec<PathBuf>, FoldError> {
    let mut files = Vec::new();
    for entry in fs::read_dir(path).map_err(FoldError::Io)? {
        let entry = entry.map_err(FoldError::Io)?;
        let p = entry.path();
        if p.is_dir() {
            files.extend(walk(&p)?);
        } else {
            files.push(p);
        }
    }
    Ok(files)
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
