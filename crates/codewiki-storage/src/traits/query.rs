/// QueryHandle trait (T-107) — read API for 9 MCP tools.
use codewiki_core::{
    CodeWikiError, Edge, FileRecord, GraphStats, Language, Node, NodeKind, SearchResult, Subgraph,
};

/// Options for search queries.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub limit: usize,
    pub kinds: Option<Vec<NodeKind>>,
    pub languages: Option<Vec<Language>>,
    pub path_filter: Option<String>,
}

/// Options for find_relevant_context.
#[derive(Debug, Clone)]
pub struct FindOpts {
    pub search_limit: usize,
    pub traversal_depth: usize,
    pub max_nodes: usize,
    pub min_score: f32,
}

impl Default for FindOpts {
    fn default() -> Self {
        // Defaults match the TS reference (DEFAULT_FIND_OPTIONS in context/index.ts).
        Self {
            search_limit: 3,
            traversal_depth: 1,
            max_nodes: 20,
            min_score: 0.3,
        }
    }
}

/// Filter for get_files.
#[derive(Debug, Clone, Default)]
pub struct FileFilter {
    pub language: Option<Language>,
    pub path_prefix: Option<String>,
}

/// Read API for 9 MCP tools.
pub trait QueryHandle: Send + Sync {
    /// BM25 FTS5 search with fallback to LIKE and fuzzy edit-distance.
    fn search_nodes(
        &self,
        query: &str,
        opts: SearchOptions,
    ) -> Result<Vec<SearchResult>, CodeWikiError>;

    /// Single node lookup (checks NodeCache first).
    fn get_node_by_id(&self, id: &str) -> Result<Option<Node>, CodeWikiError>;

    /// Incoming `calls` edges up to `depth` hops.
    fn get_callers(&self, node_id: &str, depth: usize) -> Result<Vec<(Node, Edge)>, CodeWikiError>;

    /// Outgoing `calls` edges up to `depth` hops.
    fn get_callees(&self, node_id: &str, depth: usize) -> Result<Vec<(Node, Edge)>, CodeWikiError>;

    /// Reverse-reach subgraph: all nodes that would be affected if `node_id` changed.
    fn get_impact_radius(&self, node_id: &str, depth: usize) -> Result<Subgraph, CodeWikiError>;

    /// Composite: search → BFS expand → deduplicate → score.
    fn find_relevant_context(&self, query: &str, opts: FindOpts)
        -> Result<Subgraph, CodeWikiError>;

    /// Return raw source text for a node (read from FS, slice start_line..end_line).
    fn get_code(&self, node_id: &str) -> Result<Option<String>, CodeWikiError>;

    /// Aggregate counts: nodes/edges/files by kind and language, DB size bytes.
    fn get_stats(&self) -> Result<GraphStats, CodeWikiError>;

    /// List all tracked files, optionally filtered.
    fn get_files(&self, filter: Option<&FileFilter>) -> Result<Vec<FileRecord>, CodeWikiError>;

    /// Return all nodes whose file_path is in `file_paths`, plus their 1-hop
    /// dependents (incoming edges — i.e. nodes that call/import/use them).
    fn get_affected_nodes(
        &self,
        file_paths: &[std::path::PathBuf],
    ) -> Result<Vec<Node>, CodeWikiError>;
}
