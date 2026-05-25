use moka::sync::Cache;
use std::hash::Hash;
use std::time::Duration;

/// Default capacity for resolver caches (A4).
pub const DEFAULT_RESOLVER_CAPACITY: u64 = 5_000;
/// Default capacity for query result caches (A2).
pub const DEFAULT_QUERY_CAPACITY: u64 = 1_000;
/// Default TTL for cache entries: 600 seconds (10 minutes).
pub const DEFAULT_TTL: Duration = Duration::from_secs(600);

/// A thread-safe LRU cache backed by `moka::sync::Cache`.
///
/// Supports optional TTL-based expiry. All methods are lock-free for reads.
///
/// `Clone` is O(1) and cheap: `moka::sync::Cache` is internally reference-counted,
/// so cloning shares the same underlying store.  This is used by the parallel
/// resolution path to give each rayon worker its own resolver handle without a Mutex.
pub struct LruCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    inner: Cache<K, V>,
}

impl<K, V> Clone for LruCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<K, V> LruCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Create a new cache with the given capacity (entry count).
    pub fn new(capacity: u64) -> Self {
        Self {
            inner: Cache::builder().max_capacity(capacity).build(),
        }
    }

    /// Create a new cache with the given capacity and a time-to-live for entries.
    pub fn with_ttl(capacity: u64, ttl: Duration) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(capacity)
                .time_to_live(ttl)
                .build(),
        }
    }

    /// Retrieve an entry by key. Returns `None` if not present or expired.
    pub fn get(&self, key: &K) -> Option<V> {
        self.inner.get(key)
    }

    /// Insert an entry into the cache.
    pub fn insert(&self, key: K, value: V) {
        self.inner.insert(key, value);
    }

    /// Invalidate (remove) a specific entry.
    pub fn invalidate(&self, key: &K) {
        self.inner.invalidate(key);
    }

    /// Return the number of entries currently in the cache.
    /// Note: this is an approximate count due to moka's async eviction.
    pub fn len(&self) -> u64 {
        self.inner.entry_count()
    }

    /// Returns `true` if the cache contains no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let cache: LruCache<String, u32> = LruCache::new(10);
        cache.insert("foo".to_string(), 42);
        assert_eq!(cache.get(&"foo".to_string()), Some(42));
    }

    #[test]
    fn invalidate_removes_entry() {
        let cache: LruCache<String, u32> = LruCache::new(10);
        cache.insert("bar".to_string(), 99);
        assert_eq!(cache.get(&"bar".to_string()), Some(99));
        cache.invalidate(&"bar".to_string());
        assert_eq!(cache.get(&"bar".to_string()), None);
    }

    #[test]
    fn ttl_cache_creates_successfully() {
        let cache: LruCache<String, u32> = LruCache::with_ttl(100, Duration::from_secs(60));
        cache.insert("key".to_string(), 1);
        assert_eq!(cache.get(&"key".to_string()), Some(1));
    }
}
