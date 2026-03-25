use std::path::{Path, PathBuf};

/// Configuration for local-first spill handling and remote reclaim/offload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffloadConfig {
    pub enabled: bool,
    pub spaces_endpoint: Option<String>,
    pub spaces_region: Option<String>,
    pub spaces_bucket: Option<String>,
    pub spaces_prefix: String,
    pub spaces_access_key: Option<String>,
    pub spaces_secret_key: Option<String>,
    pub local_store_dir: Option<PathBuf>,
    pub in_memory_store: bool,
    pub min_offload_bytes: Option<u64>,
    pub batch_offload_bytes: Option<u64>,
    pub landing_bytes_high_water: Option<u64>,
    pub disk_free_low_water: Option<u64>,
    pub cache_dir: PathBuf,
    pub cache_bytes_cap: u64,
}

impl Default for OffloadConfig {
    fn default() -> Self {
        Self::with_base_dir(PathBuf::from("./fold_state"))
    }
}

impl OffloadConfig {
    /// Build a config anchored to a specific state directory (useful for tests/custom wiring).
    pub fn with_base_dir(base_dir: impl AsRef<Path>) -> Self {
        let base_dir = base_dir.as_ref();
        Self {
            enabled: false,
            spaces_endpoint: None,
            spaces_region: None,
            spaces_bucket: None,
            spaces_prefix: "runs".to_string(),
            spaces_access_key: None,
            spaces_secret_key: None,
            local_store_dir: None,
            in_memory_store: false,
            min_offload_bytes: None,
            batch_offload_bytes: None,
            landing_bytes_high_water: None,
            disk_free_low_water: None,
            cache_dir: base_dir.join("offload_cache"),
            cache_bytes_cap: 20 * 1024 * 1024 * 1024, // 20 GiB cache cap by default
        }
    }

    /// Load configuration from environment variables, defaulting to the configured state dir.
    pub fn from_env() -> Self {
        let base_dir = std::env::var("FOLD_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./fold_state"));
        Self::from_env_with_base(base_dir)
    }

    /// Load configuration with a supplied base dir (used for tests).
    pub fn from_env_with_base(base_dir: impl AsRef<Path>) -> Self {
        let mut cfg = Self::with_base_dir(base_dir.as_ref());

        if let Some(enabled) = env_bool("FOLD_OFFLOAD_ENABLED") {
            cfg.enabled = enabled;
        }

        cfg.spaces_endpoint = env_string("FOLD_OFFLOAD_SPACES_ENDPOINT").or(cfg.spaces_endpoint);
        cfg.spaces_region = env_string("FOLD_OFFLOAD_SPACES_REGION").or(cfg.spaces_region);
        cfg.spaces_bucket = env_string("FOLD_OFFLOAD_SPACES_BUCKET").or(cfg.spaces_bucket);
        if let Some(prefix) = env_string("FOLD_OFFLOAD_SPACES_PREFIX") {
            cfg.spaces_prefix = prefix;
        }
        cfg.spaces_access_key =
            env_string("FOLD_OFFLOAD_SPACES_ACCESS_KEY").or(cfg.spaces_access_key);
        cfg.spaces_secret_key =
            env_string("FOLD_OFFLOAD_SPACES_SECRET_KEY").or(cfg.spaces_secret_key);
        cfg.min_offload_bytes = env_u64("FOLD_OFFLOAD_MIN_FILE_BYTES").or(cfg.min_offload_bytes);
        cfg.batch_offload_bytes = env_u64("FOLD_OFFLOAD_BATCH_BYTES").or(cfg.batch_offload_bytes);
        cfg.local_store_dir = env_string("FOLD_OFFLOAD_LOCAL_STORE_DIR")
            .map(PathBuf::from)
            .or(cfg.local_store_dir);
        if let Some(in_mem) = env_bool("FOLD_OFFLOAD_IN_MEMORY_STORE") {
            cfg.in_memory_store = in_mem;
        }

        cfg.landing_bytes_high_water =
            env_u64("FOLD_OFFLOAD_LANDING_BYTES_HIGH_WATER").or(cfg.landing_bytes_high_water);
        cfg.disk_free_low_water =
            env_u64("FOLD_OFFLOAD_DISK_FREE_LOW_WATER").or(cfg.disk_free_low_water);

        if let Some(cache_dir) = env_string("FOLD_OFFLOAD_CACHE_DIR") {
            cfg.cache_dir = PathBuf::from(cache_dir);
        }
        if let Some(cache_cap) = env_u64("FOLD_OFFLOAD_CACHE_BYTES_CAP") {
            cfg.cache_bytes_cap = cache_cap;
        }

        cfg
    }

    pub fn startup_messages(&self) -> Vec<String> {
        if !self.enabled {
            return vec!["Offload policy: disabled".to_string()];
        }

        let landing = self
            .landing_bytes_high_water
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "disabled".to_string());
        let disk = self
            .disk_free_low_water
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "disabled".to_string());

        let mut messages = vec![format!(
            "Offload policy: landing_high_water={} => local spill only; disk_free_low_water={} => remote reclaim only; cache_cap={}",
            landing, disk, self.cache_bytes_cap
        )];

        if self.min_offload_bytes.is_some() || self.batch_offload_bytes.is_some() {
            messages.push(
                "Offload compatibility: FOLD_OFFLOAD_MIN_FILE_BYTES and FOLD_OFFLOAD_BATCH_BYTES are ignored in local-first mode"
                    .to_string(),
            );
        }

        messages
    }
}

fn env_string(var: &str) -> Option<String> {
    std::env::var(var).ok().and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn env_bool(var: &str) -> Option<bool> {
    std::env::var(var).ok().and_then(|v| {
        let lower = v.trim().to_ascii_lowercase();
        match lower.as_str() {
            "1" | "true" | "yes" | "y" | "on" => Some(true),
            "0" | "false" | "no" | "n" | "off" => Some(false),
            _ => None,
        }
    })
}

fn env_u64(var: &str) -> Option<u64> {
    std::env::var(var)
        .ok()
        .and_then(|v| v.replace('_', "").parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use tempfile::tempdir;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());
    const VARS: &[&str] = &[
        "FOLD_STATE_DIR",
        "FOLD_OFFLOAD_ENABLED",
        "FOLD_OFFLOAD_SPACES_ENDPOINT",
        "FOLD_OFFLOAD_SPACES_REGION",
        "FOLD_OFFLOAD_SPACES_BUCKET",
        "FOLD_OFFLOAD_SPACES_PREFIX",
        "FOLD_OFFLOAD_SPACES_ACCESS_KEY",
        "FOLD_OFFLOAD_SPACES_SECRET_KEY",
        "FOLD_OFFLOAD_LOCAL_STORE_DIR",
        "FOLD_OFFLOAD_IN_MEMORY_STORE",
        "FOLD_OFFLOAD_MIN_FILE_BYTES",
        "FOLD_OFFLOAD_BATCH_BYTES",
        "FOLD_OFFLOAD_LANDING_BYTES_HIGH_WATER",
        "FOLD_OFFLOAD_DISK_FREE_LOW_WATER",
        "FOLD_OFFLOAD_CACHE_DIR",
        "FOLD_OFFLOAD_CACHE_BYTES_CAP",
    ];

    fn set_env(key: &str, val: impl AsRef<std::ffi::OsStr>) {
        // The current toolchain treats env mutation as unsafe; isolate it here.
        unsafe { std::env::set_var(key, val) };
    }

    fn remove_env(key: &str) {
        unsafe { std::env::remove_var(key) };
    }

    struct EnvGuard {
        saved: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let saved = VARS
                .iter()
                .map(|key| (key.to_string(), std::env::var(key).ok()))
                .collect();
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, val) in self.saved.drain(..) {
                match val {
                    Some(v) => set_env(&key, v),
                    None => remove_env(&key),
                }
            }
        }
    }

    fn clear_vars() {
        for key in VARS {
            remove_env(key);
        }
    }

    #[test]
    fn defaults_when_env_missing() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new();
        clear_vars();

        let cfg = OffloadConfig::from_env();

        assert!(!cfg.enabled);
        assert_eq!(cfg.spaces_endpoint, None);
        assert_eq!(cfg.spaces_region, None);
        assert_eq!(cfg.spaces_bucket, None);
        assert_eq!(cfg.spaces_prefix, "runs".to_string());
        assert_eq!(cfg.spaces_access_key, None);
        assert_eq!(cfg.spaces_secret_key, None);
        assert_eq!(cfg.local_store_dir, None);
        assert!(!cfg.in_memory_store);
        assert_eq!(cfg.min_offload_bytes, None);
        assert_eq!(cfg.batch_offload_bytes, None);
        assert_eq!(cfg.landing_bytes_high_water, None);
        assert_eq!(cfg.disk_free_low_water, None);
        assert_eq!(cfg.cache_dir, PathBuf::from("./fold_state/offload_cache"));
        assert_eq!(cfg.cache_bytes_cap, 20 * 1024 * 1024 * 1024);
    }

    #[test]
    fn overrides_from_env() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new();
        clear_vars();

        let base_dir = tempdir().unwrap();
        let cache_override = base_dir.path().join("cache_override");

        set_env("FOLD_OFFLOAD_ENABLED", "yes");
        set_env("FOLD_OFFLOAD_SPACES_ENDPOINT", "https://example.com");
        set_env("FOLD_OFFLOAD_SPACES_REGION", "nyc3");
        set_env("FOLD_OFFLOAD_SPACES_BUCKET", "fold-bucket");
        set_env("FOLD_OFFLOAD_SPACES_PREFIX", "custom/prefix");
        set_env("FOLD_OFFLOAD_SPACES_ACCESS_KEY", "access");
        set_env("FOLD_OFFLOAD_SPACES_SECRET_KEY", "secret");
        set_env("FOLD_OFFLOAD_LOCAL_STORE_DIR", "/tmp/local_store");
        set_env("FOLD_OFFLOAD_IN_MEMORY_STORE", "true");
        set_env("FOLD_OFFLOAD_MIN_FILE_BYTES", "1048576");
        set_env("FOLD_OFFLOAD_BATCH_BYTES", "107374182400");
        set_env("FOLD_OFFLOAD_LANDING_BYTES_HIGH_WATER", "1048576");
        set_env("FOLD_OFFLOAD_DISK_FREE_LOW_WATER", "2097152");
        set_env("FOLD_OFFLOAD_CACHE_DIR", &cache_override);
        set_env("FOLD_OFFLOAD_CACHE_BYTES_CAP", "4096");

        let cfg = OffloadConfig::from_env_with_base(base_dir.path());

        assert!(cfg.enabled);
        assert_eq!(cfg.spaces_endpoint.as_deref(), Some("https://example.com"));
        assert_eq!(cfg.spaces_region.as_deref(), Some("nyc3"));
        assert_eq!(cfg.spaces_bucket.as_deref(), Some("fold-bucket"));
        assert_eq!(cfg.spaces_prefix, "custom/prefix".to_string());
        assert_eq!(cfg.spaces_access_key.as_deref(), Some("access"));
        assert_eq!(cfg.spaces_secret_key.as_deref(), Some("secret"));
        assert_eq!(
            cfg.local_store_dir.as_deref(),
            Some(Path::new("/tmp/local_store"))
        );
        assert!(cfg.in_memory_store);
        assert_eq!(cfg.min_offload_bytes, Some(1_048_576));
        assert_eq!(cfg.batch_offload_bytes, Some(107_374_182_400));
        assert_eq!(cfg.landing_bytes_high_water, Some(1_048_576));
        assert_eq!(cfg.disk_free_low_water, Some(2_097_152));
        assert_eq!(cfg.cache_dir, cache_override);
        assert_eq!(cfg.cache_bytes_cap, 4_096);
    }

    #[test]
    fn state_dir_drives_default_cache_location() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _guard = EnvGuard::new();
        clear_vars();

        let base_dir = tempdir().unwrap();
        let base_path = base_dir.path().join("state");
        set_env("FOLD_STATE_DIR", &base_path);

        let cfg = OffloadConfig::from_env();

        assert_eq!(cfg.cache_dir, base_path.join("offload_cache"));
    }

    #[test]
    fn startup_messages_describe_local_first_policy_and_deprecation() {
        let mut cfg = OffloadConfig::with_base_dir("./fold_state");
        cfg.enabled = true;
        cfg.landing_bytes_high_water = Some(123);
        cfg.disk_free_low_water = Some(456);
        cfg.min_offload_bytes = Some(789);

        let messages = cfg.startup_messages();
        assert!(messages.iter().any(|m| m.contains("local spill only")));
        assert!(messages.iter().any(|m| m.contains("remote reclaim only")));
        assert!(
            messages
                .iter()
                .any(|m| m.contains("ignored in local-first mode"))
        );
    }
}
