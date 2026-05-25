use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// The kind of a graph node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    #[default]
    Function,
    Method,
    Class,
    Interface,
    Enum,
    EnumMember,
    Struct,
    Trait,
    Type,
    Variable,
    Constant,
    Module,
    Namespace,
    Property,
    Field,
    Constructor,
    Getter,
    Setter,
    File,
    Component,
    Route,
    Unknown,
}

/// The kind of a graph edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    #[default]
    Calls,
    Imports,
    Contains,
    Extends,
    Implements,
    Uses,
    References,
    Instantiates,
    Exports,
    Renders,
    Resolves,
    Unknown,
}

/// Programming language enum covering all 18+ supported languages.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    TypeScript,
    JavaScript,
    Python,
    Go,
    Rust,
    Java,
    Cpp,
    C,
    CSharp,
    Php,
    Ruby,
    Swift,
    Kotlin,
    Dart,
    Scala,
    Lua,
    Luau,
    Pascal,
    Svelte,
    Vue,
    Razor,
    Liquid,
    Dfm,
    Unknown,
}

/// A node in the code knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Node {
    /// Globally unique identifier (sha256 of file_path + qualified_name).
    pub id: String,
    /// Simple name (e.g. `processRequest`).
    pub name: String,
    /// Fully qualified name (e.g. `module::Class::processRequest`).
    pub qualified_name: String,
    /// Kind of this node.
    pub kind: NodeKind,
    /// Language of the source file.
    pub language: Language,
    /// Absolute path to the source file.
    pub file_path: String,
    /// 1-indexed start line.
    pub start_line: u32,
    /// 1-indexed end line.
    pub end_line: u32,
    /// 0-indexed start column.
    pub start_col: u32,
    /// 0-indexed end column.
    pub end_col: u32,
    /// Whether the symbol is exported.
    pub is_exported: bool,
    /// Optional documentation string.
    pub docstring: Option<String>,
    /// Optional signature (for functions/methods).
    pub signature: Option<String>,
    /// Arbitrary metadata (JSON-encoded).
    pub metadata: Option<String>,
}

/// An edge between two nodes in the code knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Edge {
    /// Unique identifier for this edge.
    pub id: String,
    /// Source node ID.
    pub source_id: String,
    /// Target node ID.
    pub target_id: String,
    /// Kind of relationship.
    pub kind: EdgeKind,
    /// 1-indexed line number where this relationship occurs.
    pub line: Option<u32>,
    /// 0-indexed column.
    pub col: Option<u32>,
    /// How this edge was determined (e.g. "framework-express", "import-resolver", "name-matcher").
    pub provenance: Option<String>,
    /// Confidence score (0.0–1.0) for resolved edges.
    pub confidence: Option<f32>,
    /// Arbitrary metadata.
    pub metadata: Option<String>,
}

/// A record tracking a source file in the database.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileRecord {
    pub path: PathBuf,
    pub content_hash: String,
    pub language: String,
    pub size: u64,
    pub modified_at: i64, // Unix ms
    pub indexed_at: i64,
    pub node_count: u32,
    pub errors: Vec<String>,
}

/// An unresolved reference that A4 will attempt to resolve.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnresolvedRef {
    pub id: String,
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: String,
    pub file_path: String,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub metadata: Option<String>,
}

/// A subgraph returned by traversal queries.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Subgraph {
    pub nodes: HashMap<String, Node>,
    pub edges: Vec<Edge>,
    pub roots: Vec<String>,
}

/// Aggregate statistics about the graph.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GraphStats {
    pub node_count: u64,
    pub edge_count: u64,
    pub file_count: u64,
    pub unresolved_ref_count: u64,
    pub db_size_bytes: u64,
    pub journal_mode: String,
    pub nodes_by_kind: HashMap<String, u64>,
    pub files_by_language: HashMap<String, u64>,
}

/// A search result from FTS5 or fuzzy search.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchResult {
    pub node: Node,
    /// BM25 relevance score.
    pub score: f64,
    /// Brief snippet showing matching context.
    pub snippet: Option<String>,
}

/// The output of processing a set of files through the extraction pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtractionBatch {
    pub file: FileRecord,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved_refs: Vec<UnresolvedRef>,
}

impl ExtractionBatch {
    /// Create an empty batch for a given file path.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Merge multiple batches into one (for bulk operations).
    pub fn merge(batches: Vec<ExtractionBatch>) -> Vec<ExtractionBatch> {
        batches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_serializes_and_deserializes() {
        let node = Node {
            id: "abc123".to_string(),
            name: "myFunc".to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            ..Default::default()
        };
        let json = serde_json::to_string(&node).expect("serialize");
        let back: Node = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, "abc123");
        assert_eq!(back.name, "myFunc");
    }
}
