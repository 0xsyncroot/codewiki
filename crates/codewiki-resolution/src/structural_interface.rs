//! Go structural-interface `implements` synthesis (recall fix).
//!
//! Go satisfies interfaces *structurally* — there is no `implements` keyword and
//! the extractor therefore emits no `implements` edge for Go. As a result,
//! impact / impl queries over a Go interface see none of its implementers.
//!
//! This pass closes that gap. For every exported Go interface `I` it computes
//! `I`'s method set (method name + parameter-arity fingerprint), then for every
//! exported concrete Go type `T` (struct / enum / named type) whose EXPORTED
//! method set is a superset of `I`'s, it synthesises an `EdgeKind::Implements`
//! edge `T → I` (implementer → interface, matching the orientation graph
//! traversal expects).
//!
//! ## Why source parsing
//!
//! The Go extractor (correctly) does not materialise interface method *specs* as
//! graph nodes, and concrete-method nodes carry no signature, so neither the
//! interface's required method set nor any method's parameter arity is available
//! from the graph alone. This pass therefore parses the Go *source* with the
//! same regex-driven convention the framework resolvers use (e.g.
//! `framework/go.rs`) rather than re-deriving the AST. Each file is read at most
//! once and the interface inventory is computed a single time per full index.
//!
//! ## Soundness guards (avoid false positives)
//!
//! - **Arity-aware**: a method matches only when name AND parameter count agree,
//!   so a type's `String()` does not spuriously satisfy an interface that
//!   happens to declare a different `String(int)`.
//! - **Exported-only**: only exported interfaces, types, and methods participate
//!   (Go capitalisation rule). Unexported methods never satisfy a requirement.
//! - **Empty interfaces are skipped**: `interface{}` (and marker interfaces with
//!   no methods) match everything and carry no signal.
//! - **Confidence scales with specificity**: a 1-method interface is matched at
//!   a lower confidence than a multi-method interface, because single-method
//!   coincidences are far more likely.
//!
//! ## Complexity
//!
//! Inventory build is `O(files)` source scans. Matching is
//! `O(interfaces × types)` with each comparison an `O(methods)` subset check
//! over small `HashSet`s. On kubernetes-scale repos the interface and exported
//! concrete-type counts are a small fraction of total nodes, so this is run-once
//! acceptable.

use codewiki_core::{Edge, EdgeKind, Language, Node, NodeKind};
use codewiki_storage::queries::nodes::NodeRef;
use codewiki_storage::traits::{ResolvedBy, ResolvedEdge, ResolvedFromRef};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Provenance string stamped on every synthesised structural edge.
pub const STRUCTURAL_GO_PROVENANCE: &str = "structural-interface-go";

/// Confidence for a match against a multi-method interface (≥ 2 methods).
const CONFIDENCE_MULTI_METHOD: f32 = 0.75;

/// Confidence for a match against a single-method interface. Lower because a
/// lone method-name+arity coincidence is much more likely to be accidental.
const CONFIDENCE_SINGLE_METHOD: f32 = 0.55;

/// A method requirement / capability fingerprint: name + parameter arity.
///
/// Arity is the count of *parameters*, not results — Go interface satisfaction
/// is by the full signature, but name + parameter arity is a cheap, robust
/// discriminator that rejects the common `String()` / `Error()` false positives
/// without needing full type resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MethodSig {
    pub name: String,
    pub arity: usize,
}

/// An exported Go interface and its required exported method set.
#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    pub node_id: String,
    pub name: String,
    /// File the interface is declared in (used only for diagnostics).
    pub file_path: String,
    pub methods: HashSet<MethodSig>,
}

/// An exported concrete Go type and the exported method set it provides.
#[derive(Debug, Clone)]
pub struct ConcreteTypeInfo {
    pub node_id: String,
    pub name: String,
    pub methods: HashSet<MethodSig>,
}

/// Precomputed inventory of Go interfaces + concrete types, ready for matching.
///
/// Build once per full index (`StructuralInterfaceIndex::build`), then call
/// [`synthesize`](StructuralInterfaceIndex::synthesize) to produce the additive
/// `Implements` edges.
#[derive(Debug, Default, Clone)]
pub struct StructuralInterfaceIndex {
    interfaces: Vec<InterfaceInfo>,
    concrete_types: Vec<ConcreteTypeInfo>,
}

impl StructuralInterfaceIndex {
    /// Build the inventory from the full node list and a source-reader.
    ///
    /// `read_source(file_path)` returns the file contents (or `None` if it
    /// cannot be read); each Go file is read at most once.
    ///
    /// Method sets are matched within the same *package* (approximated by source
    /// directory) to avoid cross-package name collisions producing spurious
    /// matches. Interface method requirements and concrete method capabilities
    /// are parsed from source via [`parse_go_file`].
    pub fn build<F>(nodes: &[NodeRef], mut read_source: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        // Gather the Go declaration nodes we care about.
        let mut interface_nodes: Vec<&NodeRef> = Vec::new();
        let mut concrete_nodes: Vec<&NodeRef> = Vec::new();
        let mut go_files: HashSet<String> = HashSet::new();

        for n in nodes {
            if n.language != Language::Go {
                continue;
            }
            // Read EVERY Go source file in the graph, not only those that declare
            // a type: in Go a receiver method may live in a different file from
            // its type (those files contribute `Method` nodes but no type node),
            // and the package aggregation below must see them.
            go_files.insert(n.file_path.to_string());
            match n.kind {
                NodeKind::Interface if n.is_exported => {
                    interface_nodes.push(n);
                }
                // Concrete types that can carry a method set in Go.
                NodeKind::Struct | NodeKind::Enum | NodeKind::Type if n.is_exported => {
                    concrete_nodes.push(n);
                }
                _ => {}
            }
        }

        if interface_nodes.is_empty() || concrete_nodes.is_empty() {
            return Self::default();
        }

        // Parse each relevant Go file once.
        // package_dir → ParsedGoFile aggregation: interface method sets keyed by
        // type name, and concrete method sets keyed by receiver type name.
        let mut parsed_by_file: HashMap<String, ParsedGoFile> = HashMap::new();
        for file in &go_files {
            if let Some(src) = read_source(file) {
                parsed_by_file.insert(file.clone(), parse_go_file(&src));
            }
        }

        // Concrete method sets are package-scoped: a type's methods may be split
        // across multiple files in the same directory. Aggregate by package dir.
        let mut methods_by_pkg_type: HashMap<(String, String), HashSet<MethodSig>> = HashMap::new();
        let mut iface_methods_by_pkg_type: HashMap<(String, String), HashSet<MethodSig>> =
            HashMap::new();
        for (file, parsed) in &parsed_by_file {
            let pkg = package_dir(file);
            for (ty, sigs) in &parsed.receiver_methods {
                methods_by_pkg_type
                    .entry((pkg.clone(), ty.clone()))
                    .or_default()
                    .extend(sigs.iter().cloned());
            }
            for (ty, sigs) in &parsed.interface_methods {
                iface_methods_by_pkg_type
                    .entry((pkg.clone(), ty.clone()))
                    .or_default()
                    .extend(sigs.iter().cloned());
            }
        }

        let interfaces: Vec<InterfaceInfo> = interface_nodes
            .iter()
            .filter_map(|n| {
                let pkg = package_dir(&n.file_path);
                let methods = iface_methods_by_pkg_type
                    .get(&(pkg, n.name.clone()))
                    .cloned()
                    .unwrap_or_default();
                // Skip empty / marker interfaces — they match everything.
                if methods.is_empty() {
                    return None;
                }
                Some(InterfaceInfo {
                    node_id: n.id.clone(),
                    name: n.name.clone(),
                    file_path: n.file_path.to_string(),
                    methods,
                })
            })
            .collect();

        let concrete_types: Vec<ConcreteTypeInfo> = concrete_nodes
            .iter()
            .filter_map(|n| {
                let pkg = package_dir(&n.file_path);
                let methods = methods_by_pkg_type
                    .get(&(pkg, n.name.clone()))
                    .cloned()
                    .unwrap_or_default();
                // A type with no exported methods can satisfy no (non-empty)
                // interface; drop it to keep the match loop tight.
                if methods.is_empty() {
                    return None;
                }
                Some(ConcreteTypeInfo {
                    node_id: n.id.clone(),
                    name: n.name.clone(),
                    methods,
                })
            })
            .collect();

        Self {
            interfaces,
            concrete_types,
        }
    }

    /// Synthesise additive `Implements` edges for every structural satisfaction.
    ///
    /// An edge `T → I` is produced when `T`'s exported method set is a superset
    /// of `I`'s required method set, comparing by name + parameter arity. The
    /// edge confidence scales with the interface's method count. A type is never
    /// matched to itself.
    pub fn synthesize(&self) -> Vec<ResolvedEdge> {
        let mut edges = Vec::new();
        for iface in &self.interfaces {
            let confidence = if iface.methods.len() >= 2 {
                CONFIDENCE_MULTI_METHOD
            } else {
                CONFIDENCE_SINGLE_METHOD
            };
            for ty in &self.concrete_types {
                if ty.node_id == iface.node_id {
                    continue;
                }
                // Superset check: every interface method must be provided.
                if iface.methods.is_subset(&ty.methods) {
                    edges.push(make_structural_edge(
                        &ty.node_id,
                        &iface.node_id,
                        &ty.name,
                        &iface.name,
                        confidence,
                    ));
                }
            }
        }
        edges
    }

    /// Number of interfaces in the inventory (for diagnostics / tests).
    pub fn interface_count(&self) -> usize {
        self.interfaces.len()
    }

    /// Number of concrete types in the inventory (for diagnostics / tests).
    pub fn concrete_type_count(&self) -> usize {
        self.concrete_types.len()
    }
}

/// Build a `ResolvedEdge` for a structural `Implements` match.
///
/// `resolved_by` is reported as a [`ResolvedBy::FrameworkResolver`] carrying the
/// structural provenance tag — the storage `ResolvedBy` enum is owned by another
/// crate and has no `StructuralTyping` variant, so the distinguishing signal is
/// the `provenance` string (`structural-interface-go`) which downstream tooling
/// can match on.
fn make_structural_edge(
    type_node_id: &str,
    interface_node_id: &str,
    type_name: &str,
    interface_name: &str,
    confidence: f32,
) -> ResolvedEdge {
    ResolvedEdge {
        edge: Edge {
            id: format!("{type_node_id}->{interface_node_id}"),
            source_id: type_node_id.to_string(),
            target_id: interface_node_id.to_string(),
            kind: EdgeKind::Implements,
            confidence: Some(confidence),
            provenance: Some(STRUCTURAL_GO_PROVENANCE.to_string()),
            ..Default::default()
        },
        resolved_from: ResolvedFromRef {
            from_node_id: type_node_id.to_string(),
            // There is no originating unresolved ref; record the synthesised
            // relationship so the commit path has a stable description.
            reference_name: interface_name.to_string(),
            reference_kind: "implements".to_string(),
            unresolved_ref_id: 0,
        },
        confidence,
        resolved_by: ResolvedBy::FrameworkResolver(format!(
            "{STRUCTURAL_GO_PROVENANCE}:{type_name}"
        )),
    }
}

/// Convenience entry point used by the resolver: build the inventory from the
/// full [`Node`] list (reading sources from disk) and return the synthesised
/// edges. Kept generic over the node slice so callers can pass either owned
/// [`Node`]s or pre-fetched [`NodeRef`]s.
pub fn synthesize_from_nodes<F>(nodes: &[NodeRef], read_source: F) -> Vec<ResolvedEdge>
where
    F: FnMut(&str) -> Option<String>,
{
    StructuralInterfaceIndex::build(nodes, read_source).synthesize()
}

/// Lightweight conversion so callers holding [`Node`]s can reuse the [`NodeRef`]
/// based API without depending on storage internals.
pub fn node_to_ref(n: &Node) -> NodeRef {
    NodeRef {
        id: n.id.clone(),
        name: n.name.clone(),
        qualified_name: n.qualified_name.clone(),
        kind: n.kind.clone(),
        language: n.language.clone(),
        file_path: std::sync::Arc::from(n.file_path.as_str()),
        start_line: n.start_line,
        is_exported: n.is_exported,
    }
}

// ─── Go source parsing (regex, matching framework-resolver convention) ─────────

/// Parsed method sets from a single Go source file.
#[derive(Debug, Default)]
struct ParsedGoFile {
    /// receiver type name → exported method signatures it provides.
    receiver_methods: HashMap<String, HashSet<MethodSig>>,
    /// interface type name → exported method signatures it requires.
    interface_methods: HashMap<String, HashSet<MethodSig>>,
}

/// `func (r *Recv) Name(params) ...` / `func (r Recv) Name(params) ...`.
///
/// Captures: 1 = receiver type (sans `*`), 2 = method name, 3 = raw parameter
/// list. The receiver may be generic (`Recv[T]`); the bracket is stripped.
fn method_decl_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)^\s*func\s*\(\s*\w+\s+\*?\s*([A-Za-z_]\w*)(?:\[[^\]]*\])?\s*\)\s*([A-Za-z_]\w*)\s*\(([^)]*)\)"#,
        )
        .expect("go method_decl_regex")
    })
}

/// `type Name interface {` — opens an interface body whose method specs follow.
fn interface_open_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)\btype\s+([A-Za-z_]\w*)(?:\[[^\]]*\])?\s+interface\s*\{"#)
            .expect("go interface_open_regex")
    })
}

/// A method spec line inside an interface body: `Name(params) results`.
///
/// Anchored to the start of a (trimmed) line so embedded interfaces / comments
/// do not match. Captures: 1 = method name, 2 = raw parameter list.
fn interface_method_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)^\s*([A-Za-z_]\w*)\s*\(([^)]*)\)"#).expect("go interface_method_regex")
    })
}

/// Parse a Go source file into receiver / interface method sets.
fn parse_go_file(src: &str) -> ParsedGoFile {
    let mut out = ParsedGoFile::default();

    // ── Concrete (receiver) methods ──────────────────────────────────────────
    for cap in method_decl_regex().captures_iter(src) {
        let recv = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let name = cap.get(2).map(|m| m.as_str()).unwrap_or("");
        let params = cap.get(3).map(|m| m.as_str()).unwrap_or("");
        if recv.is_empty() || name.is_empty() {
            continue;
        }
        // Only exported methods count toward interface satisfaction.
        if !is_exported_ident(name) {
            continue;
        }
        out.receiver_methods
            .entry(recv.to_string())
            .or_default()
            .insert(MethodSig {
                name: name.to_string(),
                arity: param_arity(params),
            });
    }

    // ── Interface method requirements ──────────────────────────────────────────
    for open in interface_open_regex().captures_iter(src) {
        let iface_name = match open.get(1) {
            Some(m) => m.as_str().to_string(),
            None => continue,
        };
        // The brace opens at the end of the whole match.
        let brace_idx = open.get(0).map(|m| m.end() - 1).unwrap_or(0);
        let body = match read_balanced_braces(src, brace_idx) {
            Some(b) => b,
            None => continue,
        };
        let mut methods: HashSet<MethodSig> = HashSet::new();
        for mcap in interface_method_regex().captures_iter(&body) {
            let name = mcap.get(1).map(|m| m.as_str()).unwrap_or("");
            let params = mcap.get(2).map(|m| m.as_str()).unwrap_or("");
            // Interface methods are implicitly exported when the interface is,
            // but Go also allows lowercase methods (package-private interfaces).
            // We only require exported names since only exported concrete methods
            // can satisfy them and exported interfaces are the matching target.
            if name.is_empty() || !is_exported_ident(name) {
                continue;
            }
            methods.insert(MethodSig {
                name: name.to_string(),
                arity: param_arity(params),
            });
        }
        if !methods.is_empty() {
            out.interface_methods
                .entry(iface_name)
                .or_default()
                .extend(methods);
        }
    }

    out
}

/// Count the parameters declared in a Go parameter list.
///
/// Handles grouped params (`a, b int` → 2) and empty lists (`` → 0). Splits on
/// top-level commas only (so `map[string]int` and `func(int) error` params are
/// counted as one each). A name-less type list (`int, error`) counts each
/// comma-separated entry. A grouped declaration (`a, b int`) is counted by the
/// number of leading identifiers sharing the trailing type.
fn param_arity(params: &str) -> usize {
    let trimmed = params.trim();
    if trimmed.is_empty() {
        return 0;
    }
    // Split on top-level commas (ignoring commas nested in (), [], {}).
    let groups = split_top_level_commas(trimmed);
    let mut count = 0usize;
    for g in groups {
        let g = g.trim();
        if g.is_empty() {
            continue;
        }
        // A group like `a, b int` would already be split by the comma into
        // `a` and `b int`; the first is a bare identifier (no type) meaning it
        // shares the next group's type. Each comma-separated entry is exactly
        // one parameter slot, so simply count non-empty groups.
        count += 1;
    }
    count
}

/// Split a string on commas that are not nested inside `()`, `[]`, `{}`.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(s[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].to_string());
    out
}

/// Read a balanced `{ … }` block starting at `open_index` (the byte index of the
/// opening brace). Returns the inner text (without the braces) or `None` if the
/// braces are unbalanced. String / rune literals are skipped so braces inside
/// them do not affect depth.
fn read_balanced_braces(s: &str, open_index: usize) -> Option<String> {
    let bytes = s.as_bytes();
    if open_index >= bytes.len() || bytes[open_index] != b'{' {
        return None;
    }
    let mut depth = 0usize;
    let mut i = open_index;
    let start = open_index + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..i].to_string());
                }
                i += 1;
            }
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && q != b'`' {
                        i += 2;
                    } else if bytes[i] == q {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Go export rule: an identifier is exported iff its first letter is uppercase.
fn is_exported_ident(name: &str) -> bool {
    name.chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
}

/// Approximate a Go package by its source directory (everything up to the last
/// path separator). Methods for a type may be split across files in the same
/// directory; matching within a package avoids cross-package name collisions.
fn package_dir(file_path: &str) -> String {
    match file_path.rfind(['/', '\\']) {
        Some(idx) => file_path[..idx].to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn go_ref(id: &str, name: &str, kind: NodeKind, file: &str, exported: bool) -> NodeRef {
        NodeRef {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind,
            language: Language::Go,
            file_path: Arc::from(file),
            start_line: 1,
            is_exported: exported,
        }
    }

    /// A struct whose exported method set covers an interface (arity-aware) is
    /// matched; the edge is Implements with the type as source.
    #[test]
    fn matches_type_covering_interface() {
        let iface_src = r#"
package io
type Reader interface {
    Read(p []byte) (int, error)
    Close() error
}
"#;
        let impl_src = r#"
package io
type File struct { name string }
func (f *File) Read(p []byte) (int, error) { return 0, nil }
func (f *File) Close() error { return nil }
func (f *File) secret() {}
"#;
        let nodes = vec![
            go_ref(
                "iface",
                "Reader",
                NodeKind::Interface,
                "pkg/io/reader.go",
                true,
            ),
            go_ref("file", "File", NodeKind::Struct, "pkg/io/file.go", true),
        ];
        let mut srcs: HashMap<&str, &str> = HashMap::new();
        srcs.insert("pkg/io/reader.go", iface_src);
        srcs.insert("pkg/io/file.go", impl_src);
        let edges = synthesize_from_nodes(&nodes, |f| srcs.get(f).map(|s| s.to_string()));
        assert_eq!(edges.len(), 1, "expected exactly one implements edge");
        let e = &edges[0];
        assert_eq!(e.edge.kind, EdgeKind::Implements);
        assert_eq!(e.edge.source_id, "file");
        assert_eq!(e.edge.target_id, "iface");
        assert_eq!(e.edge.provenance.as_deref(), Some(STRUCTURAL_GO_PROVENANCE));
        assert!((e.confidence - CONFIDENCE_MULTI_METHOD).abs() < 1e-6);
    }

    /// Wrong parameter arity must NOT match even with the same method name.
    #[test]
    fn rejects_wrong_arity() {
        let iface_src = r#"
package p
type Writer interface {
    Write(p []byte) (int, error)
}
"#;
        // Write takes ZERO params here → arity mismatch.
        let impl_src = r#"
package p
type Bad struct {}
func (b *Bad) Write() (int, error) { return 0, nil }
"#;
        let nodes = vec![
            go_ref("iface", "Writer", NodeKind::Interface, "p/w.go", true),
            go_ref("bad", "Bad", NodeKind::Struct, "p/bad.go", true),
        ];
        let mut srcs: HashMap<&str, &str> = HashMap::new();
        srcs.insert("p/w.go", iface_src);
        srcs.insert("p/bad.go", impl_src);
        let edges = synthesize_from_nodes(&nodes, |f| srcs.get(f).map(|s| s.to_string()));
        assert!(edges.is_empty(), "wrong-arity must not match: {edges:?}");
    }

    /// Unexported methods do not satisfy interface requirements.
    #[test]
    fn rejects_unexported_methods() {
        let iface_src = r#"
package p
type Doer interface {
    Do(x int) error
}
"#;
        // `do` is lowercase → unexported, cannot satisfy `Do`.
        let impl_src = r#"
package p
type T struct {}
func (t *T) do(x int) error { return nil }
"#;
        let nodes = vec![
            go_ref("iface", "Doer", NodeKind::Interface, "p/d.go", true),
            go_ref("t", "T", NodeKind::Struct, "p/t.go", true),
        ];
        let mut srcs: HashMap<&str, &str> = HashMap::new();
        srcs.insert("p/d.go", iface_src);
        srcs.insert("p/t.go", impl_src);
        let edges = synthesize_from_nodes(&nodes, |f| srcs.get(f).map(|s| s.to_string()));
        assert!(
            edges.is_empty(),
            "unexported method must not satisfy: {edges:?}"
        );
    }

    /// Single-method interfaces match at the lower confidence tier.
    #[test]
    fn tiny_interface_lower_confidence() {
        let src = r#"
package p
type Stringer interface { String() string }
type Thing struct {}
func (t Thing) String() string { return "" }
"#;
        let nodes = vec![
            go_ref("iface", "Stringer", NodeKind::Interface, "p/s.go", true),
            go_ref("thing", "Thing", NodeKind::Struct, "p/s.go", true),
        ];
        let edges = synthesize_from_nodes(&nodes, |_| Some(src.to_string()));
        assert_eq!(edges.len(), 1);
        assert!(
            (edges[0].confidence - CONFIDENCE_SINGLE_METHOD).abs() < 1e-6,
            "single-method interface should use lower confidence, got {}",
            edges[0].confidence
        );
    }

    /// Empty / marker interfaces match nothing (no signal).
    #[test]
    fn empty_interface_matches_nothing() {
        let src = r#"
package p
type Any interface {}
type Thing struct {}
func (t Thing) Foo() {}
"#;
        let nodes = vec![
            go_ref("iface", "Any", NodeKind::Interface, "p/a.go", true),
            go_ref("thing", "Thing", NodeKind::Struct, "p/a.go", true),
        ];
        let edges = synthesize_from_nodes(&nodes, |_| Some(src.to_string()));
        assert!(edges.is_empty(), "empty interface must match nothing");
    }

    /// A partial method set (covers some but not all requirements) is rejected.
    #[test]
    fn partial_coverage_rejected() {
        let src = r#"
package p
type RW interface {
    Read(p []byte) (int, error)
    Write(p []byte) (int, error)
}
type ReadOnly struct {}
func (r *ReadOnly) Read(p []byte) (int, error) { return 0, nil }
"#;
        let nodes = vec![
            go_ref("iface", "RW", NodeKind::Interface, "p/rw.go", true),
            go_ref("ro", "ReadOnly", NodeKind::Struct, "p/rw.go", true),
        ];
        let edges = synthesize_from_nodes(&nodes, |_| Some(src.to_string()));
        assert!(
            edges.is_empty(),
            "partial coverage must not match: {edges:?}"
        );
    }

    /// Cross-package satisfaction IS valid Go (a type in package `b` may
    /// implement an interface declared in package `a`, e.g. `io.Reader`). The
    /// matcher must allow it — multi-method method-set supersets make accidental
    /// cross-package collisions unlikely. (Package keying is used only to
    /// AGGREGATE a single type's methods that may be split across files, not to
    /// restrict which interface a type may satisfy.)
    #[test]
    fn cross_package_satisfaction_allowed() {
        let iface_src = r#"
package a
type Doer interface {
    Do(x int) error
    Stop() error
}
"#;
        let impl_src = r#"
package b
type T struct {}
func (t *T) Do(x int) error { return nil }
func (t *T) Stop() error { return nil }
"#;
        let nodes = vec![
            go_ref("iface", "Doer", NodeKind::Interface, "pkg/a/d.go", true),
            go_ref("t", "T", NodeKind::Struct, "pkg/b/t.go", true),
        ];
        let mut srcs: HashMap<&str, &str> = HashMap::new();
        srcs.insert("pkg/a/d.go", iface_src);
        srcs.insert("pkg/b/t.go", impl_src);
        let edges = synthesize_from_nodes(&nodes, |f| srcs.get(f).map(|s| s.to_string()));
        assert_eq!(
            edges.len(),
            1,
            "cross-package implementer must match: {edges:?}"
        );
        assert_eq!(edges[0].edge.source_id, "t");
        assert_eq!(edges[0].edge.target_id, "iface");
    }

    /// A type's methods split across multiple files in the SAME package are
    /// aggregated, so the union satisfies an interface even though no single
    /// file does.
    #[test]
    fn methods_aggregated_across_files_in_package() {
        let iface_src = r#"
package p
type RW interface {
    Read(p []byte) (int, error)
    Write(p []byte) (int, error)
}
"#;
        let file_a = r#"
package p
type T struct {}
func (t *T) Read(p []byte) (int, error) { return 0, nil }
"#;
        let file_b = r#"
package p
func (t *T) Write(p []byte) (int, error) { return 0, nil }
"#;
        let nodes = vec![
            go_ref("iface", "RW", NodeKind::Interface, "pkg/p/i.go", true),
            go_ref("t", "T", NodeKind::Struct, "pkg/p/a.go", true),
            // b.go declares no type but does declare a method node `T::Write`,
            // so the file is part of the Go inventory and gets parsed.
            go_ref("twrite", "Write", NodeKind::Method, "pkg/p/b.go", true),
        ];
        let mut srcs: HashMap<&str, &str> = HashMap::new();
        srcs.insert("pkg/p/i.go", iface_src);
        srcs.insert("pkg/p/a.go", file_a);
        srcs.insert("pkg/p/b.go", file_b);
        let edges = synthesize_from_nodes(&nodes, |f| srcs.get(f).map(|s| s.to_string()));
        assert_eq!(
            edges.len(),
            1,
            "Read (a.go) + Write (b.go) must aggregate to satisfy RW: {edges:?}"
        );
        assert_eq!(edges[0].edge.source_id, "t");
        assert_eq!(edges[0].edge.target_id, "iface");
    }

    /// Non-Go languages with explicit `implements` (Java/Rust) are untouched:
    /// the pass only considers Go nodes, so a Java interface/class yields nothing.
    #[test]
    fn non_go_languages_unaffected() {
        let mut nodes = vec![
            NodeRef {
                id: "ji".to_string(),
                name: "IService".to_string(),
                qualified_name: "IService".to_string(),
                kind: NodeKind::Interface,
                language: Language::Java,
                file_path: Arc::from("Svc.java"),
                start_line: 1,
                is_exported: true,
            },
            NodeRef {
                id: "jc".to_string(),
                name: "Service".to_string(),
                qualified_name: "Service".to_string(),
                kind: NodeKind::Class,
                language: Language::Java,
                file_path: Arc::from("Svc.java"),
                start_line: 1,
                is_exported: true,
            },
        ];
        // Even if a Rust trait/impl is present, it is ignored.
        nodes.push(NodeRef {
            id: "rt".to_string(),
            name: "Trait".to_string(),
            qualified_name: "Trait".to_string(),
            kind: NodeKind::Trait,
            language: Language::Rust,
            file_path: Arc::from("lib.rs"),
            start_line: 1,
            is_exported: true,
        });
        let edges = synthesize_from_nodes(&nodes, |_| {
            Some("interface IService { void handle(); }".to_string())
        });
        assert!(edges.is_empty(), "non-Go languages must be unaffected");
    }

    /// Multiple types satisfying one interface each get an edge; a type
    /// satisfying multiple interfaces gets one edge per interface.
    #[test]
    fn many_to_many_matches() {
        let src = r#"
package p
type Closer interface { Close() error }
type Flusher interface { Flush() error }
type A struct {}
func (a *A) Close() error { return nil }
type B struct {}
func (b *B) Close() error { return nil }
func (b *B) Flush() error { return nil }
"#;
        let nodes = vec![
            go_ref("c", "Closer", NodeKind::Interface, "p/i.go", true),
            go_ref("fl", "Flusher", NodeKind::Interface, "p/i.go", true),
            go_ref("a", "A", NodeKind::Struct, "p/i.go", true),
            go_ref("b", "B", NodeKind::Struct, "p/i.go", true),
        ];
        let edges = synthesize_from_nodes(&nodes, |_| Some(src.to_string()));
        // A → Closer; B → Closer; B → Flusher = 3 edges.
        assert_eq!(edges.len(), 3, "edges: {edges:?}");
        let pairs: HashSet<(String, String)> = edges
            .iter()
            .map(|e| (e.edge.source_id.clone(), e.edge.target_id.clone()))
            .collect();
        assert!(pairs.contains(&("a".to_string(), "c".to_string())));
        assert!(pairs.contains(&("b".to_string(), "c".to_string())));
        assert!(pairs.contains(&("b".to_string(), "fl".to_string())));
    }

    #[test]
    fn param_arity_counts_correctly() {
        assert_eq!(param_arity(""), 0);
        assert_eq!(param_arity("x int"), 1);
        assert_eq!(param_arity("a, b int"), 2);
        assert_eq!(param_arity("p []byte"), 1);
        assert_eq!(param_arity("m map[string]int, e error"), 2);
        assert_eq!(param_arity("fn func(int) error"), 1);
    }
}
