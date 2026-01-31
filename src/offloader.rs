use s3::Bucket;
use s3::creds::Credentials;
use s3::region::Region;
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::Path,
    str::FromStr,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

/// Errors that can occur during offload/download.
#[derive(Debug)]
pub enum OffloadError {
    Io(std::io::Error),
    Missing(String),
    Other(String),
}

impl std::fmt::Display for OffloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OffloadError::Io(e) => write!(f, "io error: {}", e),
            OffloadError::Missing(key) => write!(f, "missing object: {}", key),
            OffloadError::Other(msg) => write!(f, "offload error: {}", msg),
        }
    }
}

impl std::error::Error for OffloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OffloadError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for OffloadError {
    fn from(err: std::io::Error) -> Self {
        OffloadError::Io(err)
    }
}

/// Minimal object-store abstraction for PUT/GET. Real implementation can wrap S3; tests can use the mock.
pub trait ObjectStore: Send + Sync {
    fn put(&self, bucket: &str, key: &str, path: &Path) -> Result<(), OffloadError>;
    fn get(&self, bucket: &str, key: &str, dest_path: &Path) -> Result<(), OffloadError>;
}

/// Client that handles key mapping and retry/backoff.
#[derive(Clone)]
pub struct OffloadClient {
    store: Arc<dyn ObjectStore>,
    bucket: String,
    prefix: String,
    max_retries: usize,
    backoff_base: Duration,
}

impl OffloadClient {
    /// Create a new client for a specific bucket/prefix.
    pub fn new<S: ObjectStore + 'static>(
        store: Arc<S>,
        bucket: impl Into<String>,
        prefix: impl Into<String>,
    ) -> Self {
        let store: Arc<dyn ObjectStore> = store;
        Self {
            store,
            bucket: bucket.into(),
            prefix: prefix.into(),
            max_retries: 3,
            backoff_base: Duration::from_millis(50),
        }
    }

    /// Configure retry behavior (use short delays in tests).
    pub fn with_retry(mut self, max_retries: usize, backoff_base: Duration) -> Self {
        self.max_retries = max_retries;
        self.backoff_base = backoff_base;
        self
    }

    /// Map a local relative path to an object key under the configured prefix.
    pub fn object_key(&self, relative: &Path) -> String {
        let rel_str = relative
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if self.prefix.is_empty() {
            rel_str
        } else {
            let pref = self.prefix.trim_end_matches('/');
            if rel_str.starts_with(pref) {
                rel_str
            } else {
                format!("{}/{}", pref, rel_str)
            }
        }
    }

    /// Upload a file to the configured bucket/prefix. Returns the object key used.
    pub fn upload_file(&self, local_path: &Path, relative: &Path) -> Result<String, OffloadError> {
        let key = self.object_key(relative);
        self.retry("put", &key, || {
            self.store.put(&self.bucket, &key, local_path)
        })?;
        Ok(key)
    }

    /// Download an object to a local path.
    pub fn download_file(&self, key: &str, dest_path: &Path) -> Result<(), OffloadError> {
        self.retry("get", key, || self.store.get(&self.bucket, key, dest_path))
    }

    fn retry<F>(&self, op: &str, key: &str, mut action: F) -> Result<(), OffloadError>
    where
        F: FnMut() -> Result<(), OffloadError>,
    {
        let mut last_err: Option<OffloadError> = None;
        for attempt in 0..=self.max_retries {
            match action() {
                Ok(()) => return Ok(()),
                Err(err) => {
                    last_err = Some(err);
                    if attempt == self.max_retries {
                        break;
                    }
                    let sleep_dur = self.backoff_base.mul_f64((attempt + 1) as f64);
                    thread::sleep(sleep_dur);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            OffloadError::Other(format!("{} failed with no error detail for {}", op, key))
        }))
    }
}

/// Operations supported by the mock for failure injection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MockOp {
    Put,
    Get,
}

/// In-memory object store used for tests and local dev without network.
#[derive(Default)]
pub struct MockObjectStore {
    objects: Mutex<HashMap<String, Vec<u8>>>,
    fail_counts: Mutex<HashMap<(MockOp, String), usize>>,
}

impl MockObjectStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inject N failures for the next calls to the given op/key combo.
    pub fn fail_next(&self, op: MockOp, bucket: &str, key: &str, times: usize) {
        let mut guard = self.fail_counts.lock().unwrap();
        guard.insert((op, Self::composite_key(bucket, key)), times);
    }

    /// Fetch stored object bytes (used by tests to assert writes).
    pub fn get_bytes(&self, bucket: &str, key: &str) -> Option<Vec<u8>> {
        let guard = self.objects.lock().unwrap();
        guard.get(&Self::composite_key(bucket, key)).cloned()
    }

    fn composite_key(bucket: &str, key: &str) -> String {
        format!("{}/{}", bucket, key)
    }

    fn should_fail(&self, op: MockOp, bucket: &str, key: &str) -> bool {
        let mut guard = self.fail_counts.lock().unwrap();
        let comp = (op, Self::composite_key(bucket, key));
        if let Some(remaining) = guard.get_mut(&comp) {
            if *remaining > 0 {
                *remaining -= 1;
                return true;
            }
        }
        false
    }
}

impl ObjectStore for MockObjectStore {
    fn put(&self, bucket: &str, key: &str, path: &Path) -> Result<(), OffloadError> {
        if self.should_fail(MockOp::Put, bucket, key) {
            return Err(OffloadError::Other("injected put failure".to_string()));
        }
        let bytes = fs::read(path)?;
        let comp = Self::composite_key(bucket, key);
        let mut guard = self.objects.lock().unwrap();
        guard.insert(comp, bytes);
        Ok(())
    }

    fn get(&self, bucket: &str, key: &str, dest_path: &Path) -> Result<(), OffloadError> {
        if self.should_fail(MockOp::Get, bucket, key) {
            return Err(OffloadError::Other("injected get failure".to_string()));
        }
        let comp = Self::composite_key(bucket, key);
        let guard = self.objects.lock().unwrap();
        let Some(bytes) = guard.get(&comp) else {
            return Err(OffloadError::Missing(comp));
        };
        let parent = dest_path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        fs::write(dest_path, bytes)?;
        Ok(())
    }
}

/// Local-disk object store for offline/dev usage (writes objects under a root dir).
pub struct LocalDiskObjectStore {
    root: std::path::PathBuf,
}

impl LocalDiskObjectStore {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Result<Self, OffloadError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn object_path(&self, bucket: &str, key: &str) -> std::path::PathBuf {
        self.root.join(bucket).join(key)
    }
}

impl ObjectStore for LocalDiskObjectStore {
    fn put(&self, bucket: &str, key: &str, path: &Path) -> Result<(), OffloadError> {
        let dest = self.object_path(bucket, key);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(path, &dest)?;
        Ok(())
    }

    fn get(&self, bucket: &str, key: &str, dest_path: &Path) -> Result<(), OffloadError> {
        let src = self.object_path(bucket, key);
        if !src.exists() {
            return Err(OffloadError::Missing(key.to_string()));
        }
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dest_path)?;
        Ok(())
    }
}

/// Spaces/S3 object store backed by the `s3` crate.
pub struct SpacesObjectStore {
    bucket: String,
    client: s3::Bucket,
}

impl SpacesObjectStore {
    pub fn new(
        bucket: &str,
        region: Option<String>,
        endpoint: Option<String>,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self, OffloadError> {
        let region = match (region, endpoint) {
            (Some(r), Some(e)) => Region::Custom {
                region: r,
                endpoint: e,
            },
            (Some(r), None) => Region::from_str(&r)
                .map_err(|e| OffloadError::Other(format!("invalid region: {e}")))?,
            _ => {
                return Err(OffloadError::Other(
                    "missing region/endpoint for Spaces client".to_string(),
                ));
            }
        };
        let credentials = Credentials::new(Some(access_key), Some(secret_key), None, None, None)
            .map_err(|e| OffloadError::Other(format!("invalid credentials: {}", e)))?;
        let bucket = *Bucket::new(bucket, region, credentials)
            .map_err(|e| OffloadError::Other(e.to_string()))?
            .with_path_style();
        let client = bucket.clone();
        Ok(Self {
            bucket: bucket.name,
            client,
        })
    }
}

impl ObjectStore for SpacesObjectStore {
    fn put(&self, bucket: &str, key: &str, path: &Path) -> Result<(), OffloadError> {
        if bucket != self.bucket {
            return Err(OffloadError::Other(format!(
                "bucket mismatch: expected {}, got {}",
                self.bucket, bucket
            )));
        }
        let data = fs::read(path)?;
        let response = self
            .client
            .put_object_blocking(key, &data)
            .map_err(|e| OffloadError::Other(e.to_string()))?;
        if response.status_code() / 100 == 2 {
            Ok(())
        } else {
            Err(OffloadError::Other(format!(
                "put_object failed: status {}",
                response.status_code()
            )))
        }
    }

    fn get(&self, bucket: &str, key: &str, dest_path: &Path) -> Result<(), OffloadError> {
        if bucket != self.bucket {
            return Err(OffloadError::Other(format!(
                "bucket mismatch: expected {}, got {}",
                self.bucket, bucket
            )));
        }
        let response = self
            .client
            .get_object_blocking(key)
            .map_err(|e| OffloadError::Other(e.to_string()))?;
        if response.status_code() / 100 != 2 {
            return Err(OffloadError::Missing(key.to_string()));
        }
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::File::create(dest_path)?;
        file.write_all(response.as_slice())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn uploads_and_downloads_with_retry() {
        let bucket = "bucket";
        let prefix = "runs";
        let mock_store = Arc::new(MockObjectStore::new());
        let client = OffloadClient::new(Arc::clone(&mock_store), bucket, prefix)
            .with_retry(2, Duration::from_millis(1));

        let dir = tempdir().unwrap();
        let src = dir.path().join("run.bin");
        fs::write(&src, b"hello world").unwrap();

        // Force the first PUT to fail, ensure retry succeeds.
        mock_store.fail_next(MockOp::Put, bucket, "runs/run.bin", 1);
        let key = client.upload_file(&src, Path::new("run.bin")).unwrap();
        assert_eq!(key, "runs/run.bin");

        // Ensure object is stored under the expected key.
        let stored = mock_store.get_bytes(bucket, &key).unwrap();
        assert_eq!(stored, b"hello world");

        // Force first GET to fail, ensure retry succeeds.
        mock_store.fail_next(MockOp::Get, bucket, &key, 1);
        let dest = dir.path().join("downloaded.bin");
        client.download_file(&key, &dest).unwrap();

        let downloaded = fs::read(&dest).unwrap();
        assert_eq!(downloaded, b"hello world");
    }

    #[test]
    fn object_key_preserves_relative_path_under_prefix() {
        let client = OffloadClient::new(Arc::new(MockObjectStore::new()), "bucket", "prefix/sub");
        let rel = PathBuf::from("a/b/run.bin");
        let key = client.object_key(&rel);
        assert_eq!(key, "prefix/sub/a/b/run.bin");
    }

    #[test]
    fn local_disk_store_puts_and_gets() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("store");
        let store = Arc::new(LocalDiskObjectStore::new(&root).unwrap());
        let client = OffloadClient::new(store, "bucket", "prefix");

        let local = temp.path().join("input.bin");
        std::fs::write(&local, b"hello").unwrap();
        let key = PathBuf::from("runs/file.dat");
        let key_str = client.upload_file(&local, &key).unwrap();
        assert_eq!(key_str, "prefix/runs/file.dat");

        let download = temp.path().join("dl.bin");
        client.download_file(&key_str, &download).unwrap();
        let bytes = std::fs::read(&download).unwrap();
        assert_eq!(bytes, b"hello");

        assert!(
            root.join("bucket")
                .join("prefix")
                .join("runs")
                .join("file.dat")
                .exists()
        );
    }
}
