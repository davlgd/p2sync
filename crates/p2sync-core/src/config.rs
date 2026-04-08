use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Application-wide configuration with sensible defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct P2SyncConfig {
    /// Chunking and indexing settings.
    pub storage: StorageConfig,
    /// Network protocol settings.
    pub network: NetworkConfig,
    /// Filesystem watcher settings.
    pub watcher: WatcherConfig,
    /// TUI display settings.
    pub tui: TuiConfig,
    /// Patterns to always exclude from sync.
    pub default_excludes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Chunk size in bytes (default: 256 KB).
    pub chunk_size: usize,
    /// Maximum chunk index per file (default: 16384).
    pub max_chunk_index: usize,
    /// Maximum file size in bytes that can be received (default: 1 GB).
    pub max_file_size: u64,
    /// How long tombstones are kept before garbage collection, in seconds (default: 3600).
    pub tombstone_ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Maximum request size in bytes for the CBOR codec (default: 1 MiB).
    pub max_request_size: u64,
    /// Maximum response size in bytes for the CBOR codec (default: 100 MiB).
    pub max_response_size: u64,
    /// Request timeout in seconds (default: 120).
    pub request_timeout_secs: u64,
    /// Idle connection timeout in seconds (default: 300).
    pub idle_connection_timeout_secs: u64,
    /// GossipSub heartbeat interval in seconds (default: 5).
    pub gossipsub_heartbeat_secs: u64,
    /// Channel capacity for internal message passing (default: 256).
    pub channel_capacity: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WatcherConfig {
    /// Filesystem poll interval in milliseconds (default: 1000).
    pub poll_interval_ms: u64,
    /// Duration in seconds to suppress watcher events after a sync write (default: 3).
    pub write_guard_ttl_secs: u64,
    /// Debounce window in milliseconds: ignore duplicate events for the same file (default: 500).
    pub debounce_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    /// Maximum number of journal log lines to keep (default: 100).
    pub max_log_lines: usize,
    /// TUI refresh interval in milliseconds (default: 50).
    pub refresh_interval_ms: u64,
}

impl Default for P2SyncConfig {
    fn default() -> Self {
        Self {
            storage: StorageConfig::default(),
            network: NetworkConfig::default(),
            watcher: WatcherConfig::default(),
            tui: TuiConfig::default(),
            default_excludes: vec![".git".into(), ".DS_Store".into(), ".p2sync.toml".into()],
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            chunk_size: 256 * 1024,
            max_chunk_index: 16_384,
            max_file_size: 1024 * 1024 * 1024, // 1 GB
            tombstone_ttl_secs: 3600,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            max_request_size: 1024 * 1024,
            max_response_size: 100 * 1024 * 1024,
            request_timeout_secs: 120,
            idle_connection_timeout_secs: 300,
            gossipsub_heartbeat_secs: 5,
            channel_capacity: 256,
        }
    }
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 1000,
            write_guard_ttl_secs: 3,
            debounce_ms: 500,
        }
    }
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            max_log_lines: 100,
            refresh_interval_ms: 50,
        }
    }
}

impl NetworkConfig {
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    pub fn idle_connection_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_connection_timeout_secs)
    }

    pub fn gossipsub_heartbeat(&self) -> Duration {
        Duration::from_secs(self.gossipsub_heartbeat_secs)
    }
}

impl WatcherConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    pub fn write_guard_ttl(&self) -> Duration {
        Duration::from_secs(self.write_guard_ttl_secs)
    }

    pub fn debounce(&self) -> Duration {
        Duration::from_millis(self.debounce_ms)
    }
}

impl TuiConfig {
    pub fn refresh_interval(&self) -> Duration {
        Duration::from_millis(self.refresh_interval_ms)
    }
}

/// Config file name looked up in the sync root.
pub const CONFIG_FILENAME: &str = ".p2sync.toml";

/// Load config from a `.p2sync.toml` in the given directory, falling back to defaults.
pub fn load(root: &std::path::Path) -> P2SyncConfig {
    let path = root.join(CONFIG_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("invalid {CONFIG_FILENAME}: {e}, using defaults");
            P2SyncConfig::default()
        }),
        Err(_) => P2SyncConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let cfg = P2SyncConfig::default();
        assert_eq!(cfg.storage.chunk_size, 256 * 1024);
        assert_eq!(cfg.network.max_response_size, 100 * 1024 * 1024);
        assert!(cfg.default_excludes.contains(&".git".to_string()));
    }

    #[test]
    fn config_roundtrips_toml() {
        let cfg = P2SyncConfig::default();
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: P2SyncConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.storage.chunk_size, cfg.storage.chunk_size);
        assert_eq!(
            parsed.network.request_timeout_secs,
            cfg.network.request_timeout_secs
        );
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let toml_str = r#"
[storage]
chunk_size = 131072
"#;
        let parsed: P2SyncConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.storage.chunk_size, 131072);
        // Other fields keep defaults
        assert_eq!(parsed.storage.max_chunk_index, 16_384);
        assert_eq!(parsed.network.max_response_size, 100 * 1024 * 1024);
    }

    #[test]
    fn load_returns_default_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load(dir.path());
        assert_eq!(cfg.storage.chunk_size, 256 * 1024);
    }
}
