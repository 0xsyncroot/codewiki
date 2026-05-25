//! codewiki-storage — SQLite storage layer for CodeWiki.
//!
//! Provides:
//! - WAL-mode SQLite connection factory (`connection::open`)
//! - Schema application and migrations
//! - FTS5 search with BM25 weights and fuzzy fallback
//! - Graph traversal (BFS, DFS, callers, impact radius, type hierarchy)
//! - Traits: ExtractionStore, QueryHandle, ResolutionStore, SyncStore
//! - Concrete SQLite implementation: StorageImpl

pub mod cache;
pub mod connection;
pub mod graph;
pub mod migrations;
pub mod queries;
pub mod schema;
pub mod search;
pub mod storage_impl;
pub mod traits;

// Public re-exports — all public types visible at crate root.
pub use cache::{build_node_cache, NodeCache};
pub use connection::{open, open_in_memory};
pub use migrations::{run_migrations, CURRENT_SCHEMA_VERSION};
pub use queries::edges::get_node_degree;
pub use queries::meta::{get_edges_by_kind, run_maintenance};
pub use queries::nodes::{get_top_nodes_by_degree, DegreeMetric, NodeRef};
pub use schema::apply_schema;
pub use storage_impl::StorageImpl;
pub use traits::{
    BulkStoreStats, ExtractionStore, FileFilter, FindOpts, QueryHandle, ResolutionStore,
    ResolvedBy, ResolvedEdge, ResolvedFromRef, SearchOptions, SyncStore,
};
