use crate::disk_safety;
use crate::generation_store::{
    RunDownloader, RunOffloader, set_offload_metrics_handle, set_run_downloader, set_run_offloader,
};
use crate::offload_cache::OffloadCache;
use crate::offload_config::OffloadConfig;
use crate::offloader::{
    LocalDiskObjectStore, MockObjectStore, OffloadClient, OffloadError, SpacesObjectStore,
};
use crate::tiered_store::{SegmentRemote, configure_segment_remote};
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
        .map(PathBuf::from)
        .or_else(|_| {
            let base_canon = base_path
                .canonicalize()
                .unwrap_or_else(|_| base_path.to_path_buf());
            let path_canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            path_canon.strip_prefix(&base_canon).map(PathBuf::from)
        })
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
}

impl RunOffloader for ClientRunOffloader {
    fn offload(&self, path: &Path) -> io::Result<bool> {
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

#[derive(Clone)]
struct ClientSegmentRemote {
    client: OffloadClient,
    base_path: PathBuf,
}

impl SegmentRemote for ClientSegmentRemote {
    fn upload_segment(&self, path: &Path) -> io::Result<String> {
        let key = object_key(&self.base_path, path)?;
        let rel = Path::new(&key);
        self.client
            .upload_file(path, rel)
            .map_err(offload_err_to_io)?;
        Ok(key)
    }

    fn download_segment(&self, remote_key: &str, dest_path: &Path) -> io::Result<()> {
        let object_key = self.client.object_key(Path::new(remote_key));
        self.client
            .download_file(&object_key, dest_path)
            .map_err(offload_err_to_io)
    }
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
        set_offload_metrics_handle(None);
        set_run_offloader(None);
        set_run_downloader(None);
        configure_segment_remote(None);
        disk_safety::clear();
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
    std::fs::create_dir_all(base_path)?;
    let runtime_base_path = base_path
        .canonicalize()
        .unwrap_or_else(|_| base_path.to_path_buf());

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
        let store = SpacesObjectStore::new(
            &bucket,
            region,
            Some(endpoint),
            &access,
            &secret,
            cfg.offload_part_bytes,
        )
        .map_err(offload_err_to_io)?;
        OffloadClient::new(Arc::new(store), bucket, prefix)
    } else {
        return Err(io::Error::other(
            "offload enabled but no usable object store is configured",
        ));
    };
    let cache = OffloadCache::new(cfg.cache_dir.clone(), cfg.cache_bytes_cap)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    let downloader = ClientRunDownloader {
        client: client.clone(),
        cache: Mutex::new(cache),
        temp_root: cfg.cache_dir.clone(),
    };
    let offloader = ClientRunOffloader {
        client: client.clone(),
        base_path: runtime_base_path.clone(),
    };
    let segment_remote = ClientSegmentRemote {
        client,
        base_path: runtime_base_path.clone(),
    };

    set_run_offloader(Some(Arc::new(offloader)));
    set_run_downloader(Some((runtime_base_path, Arc::new(downloader))));
    configure_segment_remote(Some(Arc::new(segment_remote)));
    disk_safety::configure(base_path.to_path_buf(), cfg);

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
        assert!(run_path.exists());
        assert!(crate::generation_store::is_offload_marker(&run_path));

        let expected = local_store
            .join("local-offload")
            .join("runs")
            .join("base")
            .join("runs")
            .join("b=00-run-0.dat");
        assert!(expected.exists());
        drop(guard);
    }

    #[test]
    fn configure_enabled_offload_without_store_fails() {
        let temp = tempdir().unwrap();
        let base_path = temp.path().join("base");
        std::fs::create_dir_all(&base_path).unwrap();

        let mut cfg = OffloadConfig::with_base_dir(&base_path);
        cfg.enabled = true;

        let err = configure_offload_runtime(&base_path, &cfg)
            .err()
            .expect("offload config should fail without a store");
        assert!(err.to_string().contains("no usable object store"));
    }
}
