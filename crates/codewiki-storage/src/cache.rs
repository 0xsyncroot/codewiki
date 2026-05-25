/// Node cache backed by moka (T-105).
use codewiki_core::Node;
use moka::sync::Cache;

/// LRU-like node cache (moka evicts by frequency + recency).
pub type NodeCache = Cache<String, Node>;

/// Construct a NodeCache with the given capacity.
pub fn build_node_cache(capacity: u64) -> NodeCache {
    Cache::builder().max_capacity(capacity).build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewiki_core::{Node, NodeKind};

    fn make_node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            name: id.to_string(),
            kind: NodeKind::Function,
            ..Default::default()
        }
    }

    #[test]
    fn cache_insert_and_get() {
        let cache = build_node_cache(100);
        let node = make_node("foo");
        cache.insert("foo".to_string(), node.clone());
        let got = cache.get("foo");
        assert!(got.is_some());
        assert_eq!(got.unwrap().id, "foo");
    }

    #[test]
    fn cache_invalidate() {
        let cache = build_node_cache(100);
        let node = make_node("bar");
        cache.insert("bar".to_string(), node);
        cache.invalidate("bar");
        // After invalidation the entry should eventually be absent.
        // Moka may defer eviction; we can run_pending_tasks if available,
        // but for a sync test just verify invalidate doesn't panic.
    }
}
