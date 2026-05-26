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

    /// The absolute workspace root recorded for this project (the `root_path`
    /// key in `project_metadata`), if known.
    ///
    /// MCP renderers strip this prefix to emit workspace-relative paths and
    /// print the absolute root once in a header so paths stay rejoinable.
    /// Returns `None` when the index predates the metadata key or the project
    /// is unindexed; renderers must fall back to the stored (absolute) path.
    ///
    /// Default implementation returns `None`; the SQLite-backed handle
    /// overrides it by reading `project_metadata`.
    fn root_path(&self) -> Option<String> {
        None
    }

    /// Aggregated callers for a bare symbol name (overloaded-name family union).
    ///
    /// Resolves `symbol` to its best match, then — when `symbol` is a *bare*
    /// name (no scope/qualifier separator) — unions the callers of every node
    /// whose simple name case-insensitively EQUALS the best match's name (the
    /// "same-name family"). Results are deduped by node id, ranked by graph
    /// degree (most-connected first), and capped at `cap`.
    ///
    /// When `symbol` is qualified/namespaced (e.g. `Foo::bar`, `mod.Foo`,
    /// `Foo#bar`) it resolves to exactly that one node with NO family union,
    /// preserving the legacy single-node behavior.
    fn get_callers_aggregated(
        &self,
        symbol: &str,
        cap: usize,
    ) -> Result<AggregatedNeighbors, CodeWikiError>;

    /// Aggregated callees for a bare symbol name. See [`Self::get_callers_aggregated`]
    /// for family-union vs qualified-name semantics.
    fn get_callees_aggregated(
        &self,
        symbol: &str,
        cap: usize,
    ) -> Result<AggregatedNeighbors, CodeWikiError>;

    /// Aggregated impact subgraph for a bare symbol name. Unions the
    /// reverse-reach impact radius of every same-name-family member (bare name)
    /// or just the single resolved node (qualified name). Node and edge counts
    /// in the returned subgraph are bounded by `cap` nodes.
    fn get_impact_aggregated(
        &self,
        symbol: &str,
        depth: usize,
        cap: usize,
    ) -> Result<AggregatedImpact, CodeWikiError>;
}

/// Result of an aggregated callers/callees query.
///
/// `resolved_name` is the simple name of the best match (the family key);
/// `family_size` is how many same-name definitions were unioned (1 when the
/// name is unique or a qualified symbol was supplied).
#[derive(Debug, Clone, Default)]
pub struct AggregatedNeighbors {
    pub neighbors: Vec<Node>,
    pub resolved_name: String,
    pub family_size: usize,
}

/// Result of an aggregated impact query.
#[derive(Debug, Clone, Default)]
pub struct AggregatedImpact {
    pub subgraph: Subgraph,
    pub resolved_name: String,
    pub family_size: usize,
}

/// True when `symbol` looks like a fully-qualified / namespaced reference
/// rather than a bare identifier.
///
/// A qualified symbol contains a scope/member separator (`::`, `.`, `/`, `#`,
/// `\`, `->`) flanked by identifier characters — e.g. `Foo::bar`, `mod.Foo`,
/// `pkg/Type`, `Class#method`. Such a symbol must resolve to exactly one node
/// (no same-name family union). A bare identifier (`bar`, `process_request`)
/// is NOT qualified and triggers the family union.
pub fn is_qualified_symbol(symbol: &str) -> bool {
    let s = symbol.trim();
    // Multi-token inputs (e.g. "Foo bar baz") are treated as bare search
    // queries, not a single qualified reference.
    if s.split_whitespace().count() != 1 {
        return false;
    }
    const SEPARATORS: &[&str] = &["::", "->", ".", "/", "#", "\\"];
    for sep in SEPARATORS {
        if let Some(idx) = s.find(sep) {
            // Require an identifier char on BOTH sides so a trailing/leading
            // separator (or a lone `.`) doesn't count as qualification.
            let before = s[..idx].chars().next_back();
            let after = s[idx + sep.len()..].chars().next();
            if matches!(before, Some(c) if c.is_alphanumeric() || c == '_')
                && matches!(after, Some(c) if c.is_alphanumeric() || c == '_')
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod qualified_tests {
    use super::is_qualified_symbol;

    #[test]
    fn bare_names_are_not_qualified() {
        assert!(!is_qualified_symbol("bar"));
        assert!(!is_qualified_symbol("process_request"));
        assert!(!is_qualified_symbol("handleRequest"));
        // Multi-word queries are bare (treated as search text).
        assert!(!is_qualified_symbol("Foo bar"));
        // A lone trailing separator is not qualification.
        assert!(!is_qualified_symbol("foo."));
        assert!(!is_qualified_symbol(".foo"));
    }

    #[test]
    fn qualified_names_are_detected() {
        assert!(is_qualified_symbol("Foo::bar"));
        assert!(is_qualified_symbol("module.Foo"));
        assert!(is_qualified_symbol("pkg/Type"));
        assert!(is_qualified_symbol("Class#method"));
        assert!(is_qualified_symbol("a::b::c"));
        assert!(is_qualified_symbol("obj->method"));
    }
}
