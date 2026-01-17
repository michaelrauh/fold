use crate::offloader::OffloadError;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// Entry stored in the cache.
#[derive(Clone, Debug)]
struct CacheEntry {
    path: PathBuf,
    size: u64,
}

/// File cache with byte cap and LRU eviction.
pub struct OffloadCache {
    cache_dir: PathBuf,
    max_bytes: u64,
    current_bytes: u64,
    entries: LruCache<String, CacheEntry>,
}

impl OffloadCache {
    /// Create a cache rooted at `cache_dir` with the given byte cap.
    pub fn new(cache_dir: PathBuf, max_bytes: u64) -> Result<Self, OffloadError> {
        fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            cache_dir,
            max_bytes,
            current_bytes: 0,
            entries: LruCache::new(NonZeroUsize::new(1024).unwrap()), // cap entries at a reasonable number; bytes drive eviction
        })
    }

    /// Insert a file into the cache by copying from `src`. Returns the cached path.
    pub fn insert_copy(&mut self, key: &str, src: &Path) -> Result<PathBuf, OffloadError> {
        let metadata = fs::metadata(src)?;
        let size = metadata.len();

        // Remove any existing entry for this key before adding the new one.
        if let Some(old) = self.entries.pop(key) {
            self.current_bytes = self.current_bytes.saturating_sub(old.size);
            let _ = fs::remove_file(&old.path);
        }

        let dest = self.cache_dir.join(key);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, &dest)?;

        self.entries.put(
            key.to_string(),
            CacheEntry {
                path: dest.clone(),
                size,
            },
        );
        self.current_bytes = self.current_bytes.saturating_add(size);
        self.evict_if_needed()?;
        Ok(dest)
    }

    /// Fetch a cached path if present; updates LRU ordering on hit.
    pub fn get(&mut self, key: &str) -> Option<PathBuf> {
        if let Some(entry) = self.entries.get(key) {
            if entry.path.exists() {
                return Some(entry.path.clone());
            }
        }
        // Clean up missing files.
        if let Some(entry) = self.entries.pop(key) {
            self.current_bytes = self.current_bytes.saturating_sub(entry.size);
        }
        None
    }

    /// Current on-disk usage tracked by the cache manager.
    pub fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    /// For tests: number of cached entries.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn evict_if_needed(&mut self) -> Result<(), OffloadError> {
        while self.current_bytes > self.max_bytes {
            if let Some((_, entry)) = self.entries.pop_lru() {
                self.current_bytes = self.current_bytes.saturating_sub(entry.size);
                let _ = fs::remove_file(&entry.path);
                Self::remove_empty_dirs_upwards(&entry.path, &self.cache_dir)?;
            } else {
                break;
            }
        }
        Ok(())
    }

    fn remove_empty_dirs_upwards(path: &Path, root: &Path) -> Result<(), io::Error> {
        let mut current = path.parent();
        while let Some(dir) = current {
            if dir == root {
                break;
            }
            match fs::remove_dir(dir) {
                Ok(_) => {
                    current = dir.parent();
                }
                Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty => break,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    current = dir.parent();
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn evicts_least_recently_used_when_over_cap() {
        let tmp = tempdir().unwrap();
        let mut cache = OffloadCache::new(tmp.path().join("cache"), 10).unwrap();
        let src_a = write_file(tmp.path(), "a.bin", b"aaaaaa"); // 6 bytes
        let src_b = write_file(tmp.path(), "b.bin", b"bbbbbb"); // 6 bytes
        let src_c = write_file(tmp.path(), "c.bin", b"cccc"); // 4 bytes

        cache.insert_copy("bucket/run_a", &src_a).unwrap(); // total 6
        cache.get("bucket/run_a"); // touch to make MRU
        cache.insert_copy("bucket/run_b", &src_b).unwrap(); // total 12 -> evict run_a

        assert_eq!(cache.len(), 1);
        assert!(cache.get("bucket/run_a").is_none());
        assert!(cache.get("bucket/run_b").is_some());

        cache.insert_copy("bucket/run_c", &src_c).unwrap(); // adds 4, total 10
        assert!(cache.get("bucket/run_b").is_some());
        assert!(cache.get("bucket/run_c").is_some());
    }

    #[test]
    fn returns_cached_path_and_cleans_missing_files() {
        let tmp = tempdir().unwrap();
        let mut cache = OffloadCache::new(tmp.path().join("cache"), 10).unwrap();
        let src = write_file(tmp.path(), "x.bin", b"hello");
        let cached_path = cache.insert_copy("runs/x", &src).unwrap();
        assert_eq!(cache.current_bytes(), 5);

        // Hit returns path
        let fetched = cache.get("runs/x").unwrap();
        assert_eq!(fetched, cached_path);

        // Remove file manually -> cache should drop it on next get
        fs::remove_file(&cached_path).unwrap();
        assert!(cache.get("runs/x").is_none());
        assert_eq!(cache.current_bytes(), 0);
    }
}
