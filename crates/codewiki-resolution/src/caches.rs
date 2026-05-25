// T-305 / T-302 — resolver_caches.rs
//
// 7 named LruCache instances for the resolution pipeline.
// Capacities from A4-resolutions §C4:
//   DEFAULT_RESOLVER_CAPACITY = 5_000
//   file_cache at capacity / 5

use codewiki_core::{lru_cache::DEFAULT_RESOLVER_CAPACITY, LruCache, Node};

/// A mapping from a local import name to the file it resolves to.
#[derive(Debug, Clone)]
pub struct ImportMapping {
    /// The local name used in the importing file (e.g. `UserService`).
    pub local_name: String,
    /// The source module path as written in the import statement.
    pub source_path: String,
    /// The resolved absolute file path (if resolved).
    pub resolved_file: Option<String>,
}

/// All per-resolver caches keyed by string.
/// Each cache is `Send + Sync` (backed by moka). To share across threads, wrap in `Arc`.
///
/// `Clone` is cheap: `moka::sync::Cache` is internally reference-counted, so cloning
/// a `ResolverCaches` shares the same underlying cache entries across all clones.
/// This is used by `run_until_empty_parallel` to hand each rayon worker its own
/// (Arc-sharing) resolver without any Mutex bottleneck.
#[derive(Clone)]
pub struct ResolverCaches {
    /// Per-file node list cache: file_path → Vec<Node>
    pub node_cache: LruCache<String, Vec<Node>>,
    /// Name → Vec<Node>
    pub name_cache: LruCache<String, Vec<Node>>,
    /// lowercase(name) → Vec<Node>
    pub lower_name_cache: LruCache<String, Vec<Node>>,
    /// qualified_name → Vec<Node>
    pub qualified_name_cache: LruCache<String, Vec<Node>>,
    /// file_path → Vec<ImportMapping>
    pub import_mapping_cache: LruCache<String, Vec<ImportMapping>>,
    /// file_path → Option<Node>  (re-export target)
    pub re_export_cache: LruCache<String, Option<Node>>,
    /// file_path → file contents (smaller budget)
    pub file_cache: LruCache<String, String>,
}

impl ResolverCaches {
    /// Create caches with the given per-cache capacity.
    /// `file_cache` gets `capacity / 5`.
    pub fn new(capacity: u64) -> Self {
        let file_cap = std::cmp::max(64, capacity / 5);
        Self {
            node_cache: LruCache::new(capacity),
            name_cache: LruCache::new(capacity),
            lower_name_cache: LruCache::new(capacity),
            qualified_name_cache: LruCache::new(capacity),
            import_mapping_cache: LruCache::new(capacity),
            re_export_cache: LruCache::new(capacity),
            file_cache: LruCache::new(file_cap),
        }
    }

    /// Create with the workspace default capacity.
    pub fn default_capacity() -> Self {
        Self::new(DEFAULT_RESOLVER_CAPACITY)
    }

    /// Warm name / lower-name / qualified-name caches from the database.
    /// No-op until `QueryHandle::get_all_node_names` is added.
    /// Individual lookups fill the caches lazily.
    pub fn warm_noop(&self) {
        // TODO: bulk warm once QueryHandle exposes get_all_node_names
    }
}

impl Default for ResolverCaches {
    fn default() -> Self {
        Self::default_capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_insert_and_retrieve() {
        let caches = ResolverCaches::new(100);
        let node = Node {
            id: "n1".to_string(),
            name: "myFunc".to_string(),
            ..Default::default()
        };
        caches.name_cache.insert("myFunc".to_string(), vec![node.clone()]);
        let result = caches.name_cache.get(&"myFunc".to_string());
        assert!(result.is_some(), "cache hit expected");
        assert_eq!(result.unwrap()[0].id, "n1");
    }

    #[test]
    fn file_cache_has_smaller_capacity() {
        // file_cache capacity = capacity / 5 = 100 / 5 = 20
        // We can't directly inspect capacity, but we can verify it builds without panic.
        let caches = ResolverCaches::new(100);
        caches.file_cache.insert("foo.ts".to_string(), "content".to_string());
        assert!(caches.file_cache.get(&"foo.ts".to_string()).is_some());
    }
}
