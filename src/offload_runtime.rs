use crate::generation_store::{RunDownloader, RunOffloader, set_run_downloader, set_run_offloader};
use crate::offload_cache::OffloadCache;
use crate::offload_config::OffloadConfig;
use crate::offloader::{
    LocalDiskObjectStore, MockObjectStore, OffloadClient, OffloadError, SpacesObjectStore,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn offload_err_to_io(err: OffloadError) -> io::Error {
    match err {
        OffloadError::Io(e) => e,
        OffloadError::Missing(msg) | OffloadError::Other(msg) => {
            io::Error::new(io::ErrorKind::Other, msg)
        }
    }
}

fn object_key(base_path: &Path, path: &Path) -> io::Result<String> {
    let namespace = base_path
        .file_name()
        .map(|part| part.to_string_lossy().into_owned())
        .filter(|part| !part.is_empty())
        .unwrap_or_else(|| "store".to_string());
    let rel = path
        .strip_prefix(base_path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path not under base_path"))?;
    let key = rel
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if key.is_empty() {
        Ok(namespace)
    } else {
        Ok(format!("{}/{}", namespace, key))
    }
}

struct ClientRunOffloader {
    client: OffloadClient,
    base_path: PathBuf,
    min_bytes: Option<u64>,
    batch_bytes: Option<u64>,
    skipped: Mutex<(u64, bool)>, // (accumulated skipped bytes, min_unlocked)
}

impl RunOffloader for ClientRunOffloader {
    fn offload(&self, path: &Path) -> io::Result<bool> {
        if let Some(min) = self.min_bytes {
            if let Ok(meta) = std::fs::metadata(path) {
                let size = meta.len();
                let mut guard = self.skipped.lock().unwrap();
                if !guard.1 && size < min {
                    if let Some(batch) = self.batch_bytes {
                        guard.0 = guard.0.saturating_add(size);
                        if guard.0 >= batch {
                            guard.0 = 0;
                            guard.1 = true; // unlock min threshold after batch reached
                        } else {
                            return Ok(false);
                        }
                    } else {
                        return Ok(false);
                    }
                }
            }
        }
        let key = object_key(&self.base_path, path)?;
        let rel = Path::new(&key);
        self.client
            .upload_file(path, rel)
            .map_err(offload_err_to_io)?;
        Ok(true)
    }
}

struct ClientRunDownloader {
    client: OffloadClient,
    cache: Mutex<OffloadCache>,
    temp_root: PathBuf,
}

impl RunDownloader for ClientRunDownloader {
    fn cache_lookup(&self, key: &str) -> Option<PathBuf> {
        self.cache.lock().unwrap().get(key)
    }

    fn download_to_cache(&self, key: &str) -> io::Result<PathBuf> {
        let tmp_dir = self.temp_root.join("tmp_downloads");
        std::fs::create_dir_all(&tmp_dir)?;
        let tmp_path = tmp_dir.join(key.replace('/', "_"));
        let object_key = self.client.object_key(Path::new(key));
        self.client
            .download_file(&object_key, &tmp_path)
            .map_err(offload_err_to_io)?;
        let mut cache = self.cache.lock().unwrap();
        let cached = cache
            .insert_copy(key, &tmp_path)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let _ = std::fs::remove_file(&tmp_path);
        Ok(cached)
    }
}

/// Guard that clears run offloader/downloader hooks on drop.
pub struct OffloadRuntimeGuard;

impl Drop for OffloadRuntimeGuard {
    fn drop(&mut self) {
        set_run_offloader(None);
        set_run_downloader(None);
    }
}

/// Configure offload hooks for the current thread. Returns a guard that will clear hooks on drop.
/// This currently supports local disk or in-memory object stores for dev/offline runs.
pub fn configure_offload_runtime(
    base_path: &Path,
    cfg: &OffloadConfig,
) -> io::Result<Option<OffloadRuntimeGuard>> {
    if !cfg.enabled {
        return Ok(None);
    }

    let bucket = cfg
        .spaces_bucket
        .clone()
        .unwrap_or_else(|| "local-offload".to_string());
    let prefix = cfg.spaces_prefix.clone();
    let client = if cfg.in_memory_store {
        OffloadClient::new(
            Arc::new(MockObjectStore::new()),
            bucket.clone(),
            prefix.clone(),
        )
    } else if let Some(dir) = cfg.local_store_dir.clone() {
        OffloadClient::new(
            Arc::new(LocalDiskObjectStore::new(dir).map_err(offload_err_to_io)?),
            bucket,
            prefix,
        )
    } else if let (Some(endpoint), Some(access), Some(secret)) = (
        cfg.spaces_endpoint.clone(),
        cfg.spaces_access_key.clone(),
        cfg.spaces_secret_key.clone(),
    ) {
        let region = cfg
            .spaces_region
            .clone()
            .or_else(|| Some("us-east-1".to_string()));
        let store = SpacesObjectStore::new(&bucket, region, Some(endpoint), &access, &secret)
            .map_err(offload_err_to_io)?;
        OffloadClient::new(Arc::new(store), bucket, prefix)
    } else {
        // No supported store configured (missing endpoint/creds or storage selection).
        eprintln!("Offload enabled but no store configured; skipping offload/download hooks");
        return Ok(None);
    };
    let cache = OffloadCache::new(cfg.cache_dir.clone(), cfg.cache_bytes_cap)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    let downloader = ClientRunDownloader {
        client: client.clone(),
        cache: Mutex::new(cache),
        temp_root: cfg.cache_dir.clone(),
    };
    let offloader = ClientRunOffloader {
        client,
        base_path: base_path.to_path_buf(),
        min_bytes: cfg.min_offload_bytes,
        batch_bytes: cfg.batch_offload_bytes,
        skipped: Mutex::new((0, false)),
    };

    set_run_offloader(Some(Arc::new(offloader)));
    set_run_downloader(Some((base_path.to_path_buf(), Arc::new(downloader))));

    Ok(Some(OffloadRuntimeGuard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_store::test_maybe_offload_and_delete;
    use tempfile::tempdir;

    #[test]
    fn configure_local_offload_copies_run() {
        let temp = tempdir().unwrap();
        let base_path = temp.path().join("base");
        std::fs::create_dir_all(base_path.join("runs")).unwrap();
        let run_path = base_path.join("runs").join("b=00-run-0.dat");
        std::fs::write(&run_path, b"hello world").unwrap();

        let local_store = temp.path().join("store");
        let mut cfg = OffloadConfig::with_base_dir(&base_path);
        cfg.enabled = true;
        cfg.local_store_dir = Some(local_store.clone());
        cfg.cache_dir = base_path.join("cache");

        let guard = configure_offload_runtime(&base_path, &cfg)
            .unwrap()
            .expect("offload guard");
        test_maybe_offload_and_delete(&run_path).unwrap();
        assert!(!run_path.exists());

        let expected = local_store
            .join("local-offload")
            .join("runs")
            .join("base")
            .join("runs")
            .join("b=00-run-0.dat");
        assert!(expected.exists());
        drop(guard);
    }
}
