use serde::Deserialize;

/// Runtime configuration for CodeWiki.
///
/// Values are resolved with the following priority (highest wins):
/// 1. Environment variable
/// 2. `.codewiki/config.toml` in the project root
/// 3. Compiled-in default
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CodeWikiConfig {
    /// File-watcher coalesce window in milliseconds (A5 sync).
    pub debounce_ms: u64,
    /// PPID watchdog interval in milliseconds; 0 disables the watchdog.
    pub ppid_poll_ms: u64,
    /// LRU size for node lookups (A2 moka cache).
    pub node_cache_size: usize,
    /// LRU size for FTS5 result sets.
    pub fts_cache_size: usize,
    /// A4 name/import resolver moka cache size.
    pub resolver_cache_size: usize,
    /// Override per-response char budget; 0 = use adaptive table.
    pub max_output_chars: usize,
    /// Override per-file char budget; 0 = adaptive.
    pub max_chars_per_file: usize,
    /// Max chars in query/task/symbol params.
    pub max_query_len: usize,
    /// Max chars in path/pattern/projectPath params.
    pub max_path_len: usize,
    /// Files larger than this are skipped (bytes).
    pub max_file_size_bytes: u64,
    /// SQLite page cache size passed via `PRAGMA cache_size` (KB).
    pub sqlite_cache_kb: i32,
    /// WAL auto-checkpoint pages.
    pub sqlite_wal_autocheckpoint: i32,
}

impl Default for CodeWikiConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 300,
            ppid_poll_ms: 5_000,
            node_cache_size: 1_000,
            fts_cache_size: 200,
            resolver_cache_size: 5_000,
            max_output_chars: 0,   // adaptive
            max_chars_per_file: 0, // adaptive
            max_query_len: 10_000,
            max_path_len: 4_096,
            max_file_size_bytes: 1 << 20, // 1 MB
            sqlite_cache_kb: 65_536,      // 64 MB
            sqlite_wal_autocheckpoint: 1_000,
        }
    }
}

impl CodeWikiConfig {
    /// Load config: compiled defaults → TOML file → env-var overrides.
    pub fn load(project_root: &std::path::Path) -> Self {
        let mut cfg = Self::default();

        // Layer 1: TOML file (if present)
        let toml_path = project_root.join(".codewiki/config.toml");
        if toml_path.exists() {
            if let Ok(text) = std::fs::read_to_string(&toml_path) {
                if let Ok(from_file) = toml::from_str::<CodeWikiConfig>(&text) {
                    cfg = from_file;
                }
            }
        }

        // Layer 2: env-var overrides (highest priority)
        macro_rules! env_u64 {
            ($field:ident, $var:expr) => {
                if let Ok(v) = std::env::var($var) {
                    if let Ok(n) = v.parse() {
                        cfg.$field = n;
                    }
                }
            };
        }
        macro_rules! env_usize {
            ($field:ident, $var:expr) => {
                if let Ok(v) = std::env::var($var) {
                    if let Ok(n) = v.parse() {
                        cfg.$field = n;
                    }
                }
            };
        }
        macro_rules! env_i32 {
            ($field:ident, $var:expr) => {
                if let Ok(v) = std::env::var($var) {
                    if let Ok(n) = v.parse() {
                        cfg.$field = n;
                    }
                }
            };
        }

        env_u64!(debounce_ms, "CODEWIKI_DEBOUNCE_MS");
        env_u64!(ppid_poll_ms, "CODEWIKI_PPID_POLL_MS");
        env_usize!(node_cache_size, "CODEWIKI_NODE_CACHE_SIZE");
        env_usize!(fts_cache_size, "CODEWIKI_FTS_CACHE_SIZE");
        env_usize!(resolver_cache_size, "CODEWIKI_RESOLVER_CACHE_SIZE");
        env_usize!(max_output_chars, "CODEWIKI_MAX_OUTPUT_CHARS");
        env_usize!(max_chars_per_file, "CODEWIKI_MAX_CHARS_PER_FILE");
        env_usize!(max_query_len, "CODEWIKI_MAX_QUERY_LEN");
        env_usize!(max_path_len, "CODEWIKI_MAX_PATH_LEN");
        env_u64!(max_file_size_bytes, "CODEWIKI_MAX_FILE_SIZE_BYTES");
        env_i32!(sqlite_cache_kb, "CODEWIKI_SQLITE_CACHE_KB");
        env_i32!(sqlite_wal_autocheckpoint, "CODEWIKI_WAL_AUTOCHECKPOINT");

        cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::Path;

    #[test]
    fn config_default_values_match_spec() {
        let cfg = CodeWikiConfig::default();
        assert_eq!(cfg.debounce_ms, 300);
        assert_eq!(cfg.ppid_poll_ms, 5_000);
        assert_eq!(cfg.node_cache_size, 1_000);
        assert_eq!(cfg.fts_cache_size, 200);
        assert_eq!(cfg.resolver_cache_size, 5_000);
        assert_eq!(cfg.max_output_chars, 0);
        assert_eq!(cfg.max_chars_per_file, 0);
        assert_eq!(cfg.max_query_len, 10_000);
        assert_eq!(cfg.max_path_len, 4_096);
        assert_eq!(cfg.max_file_size_bytes, 1 << 20);
        assert_eq!(cfg.sqlite_cache_kb, 65_536);
        assert_eq!(cfg.sqlite_wal_autocheckpoint, 1_000);
    }

    /// Test that defaults hold when there is no TOML file and no relevant env vars set.
    /// Uses a dedicated env-var key to avoid interference with config_env_var_override.
    #[test]
    #[serial]
    fn config_load_non_existent_root_returns_defaults() {
        // Ensure no override is active for this field during this test.
        let _guard = std::env::var("CODEWIKI_NODE_CACHE_SIZE").ok();
        std::env::remove_var("CODEWIKI_NODE_CACHE_SIZE");
        let cfg = CodeWikiConfig::load(Path::new("/tmp/does-not-exist-codewiki-test"));
        assert_eq!(cfg.node_cache_size, 1_000);
    }

    /// Tests env-var override of debounce_ms using a serial lock to avoid races.
    /// NOTE: Rust test runner is multi-threaded by default; env mutation tests
    /// must use `cargo test -- --test-threads=1` for guaranteed isolation.
    #[test]
    #[serial]
    fn config_env_var_override() {
        // Set env var, load config, verify override, then clear.
        std::env::set_var("CODEWIKI_DEBOUNCE_MS", "999");
        let cfg = CodeWikiConfig::load(Path::new("/tmp/does-not-exist-codewiki-test"));
        std::env::remove_var("CODEWIKI_DEBOUNCE_MS");
        assert_eq!(cfg.debounce_ms, 999);
    }
}
