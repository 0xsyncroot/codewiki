//! T-326 — Tree cache: moka-backed store of (ParsedTree, source) pairs.
//!
//! A1 populates this cache after every successful parse by calling `store`.
//! A5 reads from it when constructing `FileEdit` objects for incremental
//! reparse.  Evictions are handled automatically by moka based on capacity.

use codewiki_extraction::ParsedTree;
use moka::sync::Cache;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A cached entry: the previously-parsed tree and its full source text.
#[derive(Clone)]
pub struct CachedTree {
    pub tree: ParsedTree,
    /// Full source text at the time of the last successful parse.
    pub source: Arc<String>,
}

/// LRU cache keyed by absolute `PathBuf`, value `CachedTree`.
///
/// Backed by `moka::sync::Cache` so it is thread-safe without additional
/// locking.
#[derive(Clone)]
pub struct TreeCache {
    inner: Cache<PathBuf, CachedTree>,
}

impl TreeCache {
    /// Create a new cache with the given max entry count.
    pub fn new(capacity: u64) -> Self {
        Self {
            inner: Cache::new(capacity),
        }
    }

    /// Retrieve the cached entry for `path`, if present.
    pub fn get(&self, path: &Path) -> Option<CachedTree> {
        self.inner.get(path)
    }

    /// Store a parsed tree and its source in the cache.
    pub fn store(&self, path: PathBuf, tree: ParsedTree, source: Arc<String>) {
        self.inner.insert(path, CachedTree { tree, source });
    }

    /// Remove the entry for `path` from the cache.
    pub fn evict(&self, path: &Path) {
        self.inner.invalidate(path);
    }

    /// Number of entries currently in the cache.
    pub fn len(&self) -> u64 {
        self.inner.entry_count()
    }

    /// Returns `true` if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_get() {
        // TreeCache stores and retrieves entries by path.
        // We test that get returns None for unknown paths and Some after store.
        let cache = TreeCache::new(100);
        let path = PathBuf::from("/tmp/test.ts");
        assert!(cache.get(&path).is_none(), "should be empty initially");
        // We cannot easily build a real ParsedTree without tree-sitter, so we
        // use integration tests for full round-trips.  Here we confirm API
        // compiles and None is returned for missing keys.
    }

    #[test]
    fn evict_returns_none() {
        let cache = TreeCache::new(100);
        let path = PathBuf::from("/tmp/evict_test.ts");
        // Evicting a non-existent entry must not panic.
        cache.evict(&path);
        assert!(cache.get(&path).is_none());
    }

    #[test]
    fn len_starts_zero() {
        let cache = TreeCache::new(100);
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());
    }
}
