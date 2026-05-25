// T-309 / T-310 — reference_resolver.rs
//
// 3-strategy cascade: framework → import → name matcher.
// Pre-filter via knownNames set.
// Edge-kind promotion: extends→implements for interface targets, calls→instantiates for class targets.

use crate::caches::ResolverCaches;
use crate::framework::{FrameworkResolverRegistry, ResolutionContext};
use crate::import_resolver::extract_import_mappings;
use crate::name_matcher::{match_reference, NameMatchContext};
use crate::path_aliases::PathAliasMap;
use codewiki_core::{CodeWikiError, EdgeKind, Language, Node, UnresolvedRef};
use codewiki_storage::queries::nodes::NodeRef;
use codewiki_storage::traits::{ResolvedBy, ResolvedEdge, ResolvedFromRef};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

// ─── Built-in name sets ───────────────────────────────────────────────────────

macro_rules! static_set {
    ($name:ident, $($item:expr),* $(,)?) => {
        fn $name() -> &'static HashSet<&'static str> {
            use std::sync::OnceLock;
            static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
            S.get_or_init(|| [$($item),*].into_iter().collect())
        }
    };
}

static_set!(
    js_builtins,
    "console",
    "window",
    "document",
    "global",
    "process",
    "Promise",
    "Array",
    "Object",
    "String",
    "Number",
    "Boolean",
    "Date",
    "Math",
    "JSON",
    "RegExp",
    "Error",
    "Map",
    "Set",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "fetch",
    "require",
    "module",
    "exports",
    "__dirname",
    "__filename"
);
static_set!(
    react_hooks,
    "useState",
    "useEffect",
    "useContext",
    "useReducer",
    "useCallback",
    "useMemo",
    "useRef",
    "useLayoutEffect",
    "useImperativeHandle",
    "useDebugValue"
);
static_set!(
    python_builtins,
    "print",
    "len",
    "range",
    "str",
    "int",
    "float",
    "list",
    "dict",
    "set",
    "tuple",
    "open",
    "input",
    "type",
    "isinstance",
    "hasattr",
    "getattr",
    "setattr",
    "super",
    "self",
    "cls",
    "None",
    "True",
    "False"
);
static_set!(
    python_builtin_types,
    "list",
    "dict",
    "set",
    "tuple",
    "str",
    "int",
    "float",
    "bool",
    "bytes",
    "bytearray",
    "frozenset",
    "object",
    "super"
);
static_set!(
    python_builtin_methods,
    "append",
    "extend",
    "insert",
    "remove",
    "pop",
    "clear",
    "sort",
    "reverse",
    "copy",
    "update",
    "keys",
    "values",
    "items",
    "get",
    "add",
    "discard",
    "union",
    "intersection",
    "difference",
    "split",
    "join",
    "strip",
    "replace",
    "lower",
    "upper",
    "startswith",
    "endswith",
    "find",
    "index",
    "count",
    "format",
    "encode",
    "decode",
    "read",
    "write",
    "readline",
    "readlines",
    "close",
    "flush"
);
static_set!(
    go_stdlib_packages,
    "fmt",
    "os",
    "io",
    "net",
    "http",
    "log",
    "math",
    "sort",
    "sync",
    "time",
    "path",
    "bytes",
    "strings",
    "strconv",
    "errors",
    "context",
    "json",
    "xml",
    "csv",
    "html",
    "template",
    "regexp",
    "reflect",
    "runtime",
    "testing",
    "flag",
    "bufio",
    "crypto",
    "encoding",
    "filepath",
    "hash",
    "mime",
    "rand",
    "signal",
    "sql",
    "syscall",
    "unicode",
    "unsafe",
    "atomic"
);
static_set!(
    go_builtins,
    "make",
    "new",
    "len",
    "cap",
    "append",
    "copy",
    "delete",
    "close",
    "panic",
    "recover",
    "print",
    "println",
    "nil",
    "true",
    "false",
    "iota",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "float32",
    "float64",
    "string",
    "bool",
    "byte",
    "rune",
    "any",
    "error"
);

// ─── Query handle interface (minimal) ────────────────────────────────────────

/// Minimal query handle needed by the resolver.  Implemented by the caller.
pub trait ResolverQueryHandle: Send + Sync {
    fn get_node_by_id(&self, id: &str) -> Option<Node>;
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node>;
    fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node>;
    fn get_nodes_by_qualified_name(&self, qname: &str) -> Vec<Node>;
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node>;
    fn get_all_node_names(&self) -> Vec<String>;
    fn get_all_file_paths(&self) -> Vec<String>;
    /// Fetch all nodes in one sequential scan (used to build the in-memory name
    /// index during warm_caches — OPT-2).  Implementations that cannot support
    /// this may return an empty Vec, falling back to per-ref DB queries.
    fn get_all_nodes(&self) -> Vec<Node>;
    /// Fetch lean NodeRef rows for all nodes (Wave-3 OPT-12).
    ///
    /// Returns only the fields consumed by the name-matcher, skipping heavy
    /// optional strings (`docstring`, `signature`, `metadata`).  The default
    /// implementation falls back to [`get_all_nodes`] and converts, so existing
    /// implementations remain correct without any changes.
    fn get_all_node_refs(&self) -> Vec<NodeRef> {
        self.get_all_nodes()
            .into_iter()
            .map(|n| NodeRef {
                id: n.id,
                name: n.name,
                qualified_name: n.qualified_name,
                kind: n.kind,
                language: n.language,
                file_path: Arc::from(n.file_path.as_str()),
                start_line: n.start_line,
                is_exported: n.is_exported,
            })
            .collect()
    }
}

// ─── ReferenceResolver ───────────────────────────────────────────────────────

/// In-memory name index built once per resolution run (OPT-2, slimmed in OPT-12).
///
/// Stores `Arc<NodeRef>` instead of full [`Node`] to eliminate both the heavy
/// `docstring`/`signature`/`metadata` optional strings AND the 4× duplication
/// that arose when the same node appeared in name_index, lower_name_index,
/// qualified_name_index, and file_index simultaneously.  With `Arc`, all four
/// maps share a pointer to a single heap-allocated [`NodeRef`]; the only per-map
/// cost is the 16-byte `Arc` pointer increment.
///
/// `file_path` inside [`NodeRef`] is `Arc<str>` so nodes in the same source
/// file share a single heap allocation rather than duplicating the path N×.
///
/// The outer `Arc<HashMap>` wrapper lets Wave-3 parallel workers clone the
/// index handle without any `Mutex` bottleneck (OPT-11).
type NameIndex = Arc<HashMap<String, Vec<Arc<NodeRef>>>>;

/// `Clone` is cheap: all fields are either `Arc<T>`, `PathBuf` (cheap clone),
/// `PathAliasMap` (Vec of small structs), or `ResolverCaches` / `FrameworkResolverRegistry`
/// which themselves clone only Arc pointers.  The four `NameIndex` fields are
/// `Option<Arc<HashMap<…>>>` — cloning copies only the Arc pointer (8 bytes), not
/// the map contents.  This is used by `run_until_empty_parallel` to give each
/// rayon worker its own resolver handle backed by the same shared data.
#[derive(Clone)]
pub struct ReferenceResolver {
    project_root: PathBuf,
    db: Arc<dyn ResolverQueryHandle>,
    caches: ResolverCaches,
    path_aliases: PathAliasMap,
    framework_registry: FrameworkResolverRegistry,
    known_names: Option<HashSet<String>>,
    known_files: Option<Vec<String>>,
    /// name → nodes index (exact match)
    name_index: Option<NameIndex>,
    /// lowercase(name) → nodes index (fuzzy lowercase match)
    lower_name_index: Option<NameIndex>,
    /// qualified_name → nodes index (qualified name match)
    qualified_name_index: Option<NameIndex>,
    /// file_path → nodes index (file-scoped lookups)
    file_index: Option<NameIndex>,
}

impl ReferenceResolver {
    pub fn new(project_root: PathBuf, db: Arc<dyn ResolverQueryHandle>, capacity: u64) -> Self {
        let path_aliases = PathAliasMap::load(&project_root).unwrap_or_default();
        Self {
            project_root,
            db,
            caches: ResolverCaches::new(capacity),
            path_aliases,
            framework_registry: FrameworkResolverRegistry::default(),
            known_names: None,
            known_files: None,
            name_index: None,
            lower_name_index: None,
            qualified_name_index: None,
            file_index: None,
        }
    }

    /// Pre-compute known-names set and in-memory name indexes for fast lookups.
    ///
    /// OPT-2: Fetches all nodes in one sequential DB scan and builds four
    /// `Arc<HashMap>` indexes (name, lower-name, qualified-name, file-path).
    /// Subsequent `resolve_one` calls read these maps instead of issuing
    /// per-ref DB queries, reducing ~686k queries (django) to ~0.
    ///
    /// OPT-12 (Wave-3): indexes now store [`NodeRef`] instead of full [`Node`],
    /// dropping `docstring`/`signature`/`metadata` from every map entry.
    /// `file_path` strings are interned via `Arc<str>` so nodes in the same
    /// file share a single heap allocation rather than duplicating the path N×.
    ///
    /// The indexes are `Arc`-wrapped so Wave-3 parallel resolution (OPT-11)
    /// can clone the `Arc` into each worker thread without any `Mutex`.
    pub fn warm_caches(&mut self) {
        let all_refs = self.db.get_all_node_refs();

        if all_refs.is_empty() {
            // Fall back to the lightweight name-only path when get_all_node_refs
            // returns nothing (e.g. implementations that return empty vec).
            let names: HashSet<String> = self.db.get_all_node_names().into_iter().collect();
            let files = self.db.get_all_file_paths();
            self.known_names = Some(names);
            self.known_files = Some(files);
            return;
        }

        let node_count = all_refs.len();
        tracing::debug!(
            node_count,
            "OPT-12: building lean Arc<NodeRef> name indexes"
        );

        let mut name_map: HashMap<String, Vec<Arc<NodeRef>>> = HashMap::with_capacity(node_count);
        let mut lower_map: HashMap<String, Vec<Arc<NodeRef>>> = HashMap::with_capacity(node_count);
        let mut qname_map: HashMap<String, Vec<Arc<NodeRef>>> =
            HashMap::with_capacity(node_count / 10 + 1);
        let mut file_map: HashMap<String, Vec<Arc<NodeRef>>> =
            HashMap::with_capacity(node_count / 10 + 1);
        let mut known: HashSet<String> = HashSet::with_capacity(node_count);
        // Intern file_path strings: reuse Arc<str> across all nodes in a file.
        let mut path_intern: HashMap<String, Arc<str>> =
            HashMap::with_capacity(node_count / 20 + 1);

        for mut nref in all_refs {
            // Intern the file_path: all nodes in the same file share one Arc<str>.
            let interned: Arc<str> = path_intern
                .entry(nref.file_path.to_string())
                .or_insert_with(|| Arc::clone(&nref.file_path))
                .clone();
            nref.file_path = interned;

            known.insert(nref.name.clone());
            let lower = nref.name.to_lowercase();
            let qname = nref.qualified_name.clone();
            let file_key = nref.file_path.to_string();

            // Wrap in Arc so all four maps share the same heap allocation.
            let nref_arc = Arc::new(nref);
            name_map
                .entry(nref_arc.name.clone())
                .or_default()
                .push(Arc::clone(&nref_arc));
            lower_map
                .entry(lower)
                .or_default()
                .push(Arc::clone(&nref_arc));
            if !qname.is_empty() {
                qname_map
                    .entry(qname)
                    .or_default()
                    .push(Arc::clone(&nref_arc));
            }
            file_map.entry(file_key).or_default().push(nref_arc);
        }

        let files = self.db.get_all_file_paths();

        self.known_names = Some(known);
        self.known_files = Some(files);
        self.name_index = Some(Arc::new(name_map));
        self.lower_name_index = Some(Arc::new(lower_map));
        self.qualified_name_index = Some(Arc::new(qname_map));
        self.file_index = Some(Arc::new(file_map));
    }

    /// Return a clone of the name index Arc (for Wave-3 parallel workers).
    pub fn name_index(&self) -> Option<NameIndex> {
        self.name_index.clone()
    }
    /// Return a clone of the lower-name index Arc.
    pub fn lower_name_index(&self) -> Option<NameIndex> {
        self.lower_name_index.clone()
    }
    /// Return a clone of the qualified-name index Arc.
    pub fn qualified_name_index(&self) -> Option<NameIndex> {
        self.qualified_name_index.clone()
    }
    /// Return a clone of the file index Arc.
    pub fn file_index(&self) -> Option<NameIndex> {
        self.file_index.clone()
    }

    /// Resolve a single reference using the 3-strategy cascade.
    pub fn resolve_one(&self, uref: &UnresolvedRef) -> Result<Option<ResolvedEdge>, CodeWikiError> {
        // Skip built-ins / external
        if self.is_builtin_or_external(uref) {
            return Ok(None);
        }

        // Fast pre-filter
        if !self.has_any_possible_match(&uref.reference_name) && !self.matches_any_import(uref) {
            return Ok(None);
        }

        // OPT-14: borrow known_files instead of cloning the Vec on every call.
        //
        // Previously `known_files` was `Vec<String>` allocated per-call:
        //   `self.known_files.as_deref().map(|f| f.to_vec()).unwrap_or_else(||…)`
        // At django scale this allocates ~114k × 3020 string clones = ~344M allocations.
        // Instead, build the fallback only once and borrow throughout.
        let fallback_files: Vec<String>;
        let known_files_slice: &[String] = if let Some(ref kf) = self.known_files {
            kf.as_slice()
        } else {
            fallback_files = self.db.get_all_file_paths();
            fallback_files.as_slice()
        };

        let ctx = ResolutionContext {
            project_root: &self.project_root,
            project_languages: &[], // populated by caller for framework filtering
            caches: &self.caches,
            path_aliases: &self.path_aliases,
            known_files: known_files_slice,
        };

        let mut candidates: Vec<ResolvedEdge> = Vec::new();

        // Strategy 1: framework resolvers
        for resolver in self.framework_registry.all_resolvers() {
            match resolver.resolve(uref, &ctx) {
                Ok(Some(edge)) => {
                    if edge.confidence >= 0.9 {
                        return Ok(Some(edge));
                    }
                    candidates.push(edge);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(
                        resolver = resolver.name(),
                        ref_name = %uref.reference_name,
                        error = %e,
                        "framework resolver error"
                    );
                }
            }
        }

        // Strategy 2: import-based resolution (stub — uses name cache)
        if let Some(edge) = self.resolve_via_import(uref) {
            if edge.confidence >= 0.9 {
                return Ok(Some(edge));
            }
            candidates.push(edge);
        }

        // Strategy 3: name matching
        if let Some(result) = self.resolve_via_name_matcher(uref) {
            candidates.push(result);
        }

        if candidates.is_empty() {
            return Ok(None);
        }

        // Return highest confidence
        Ok(candidates.into_iter().max_by(|a, b| {
            a.confidence
                .partial_cmp(&b.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        }))
    }

    // ─── Private helpers ──────────────────────────────────────────────────────

    fn has_any_possible_match(&self, name: &str) -> bool {
        if let Some(ref known) = self.known_names {
            if known.contains(name) {
                return true;
            }
            // Check dotted / qualified name parts
            if let Some(dot) = name.find('.') {
                let receiver = &name[..dot];
                let member = &name[dot + 1..];
                if known.contains(receiver) || known.contains(member) {
                    return true;
                }
                let cap = capitalize(receiver);
                if known.contains(cap.as_str()) {
                    return true;
                }
            }
            if let Some(col) = name.find("::") {
                let receiver = &name[..col];
                let member = &name[col + 2..];
                if known.contains(receiver) || known.contains(member) {
                    return true;
                }
            }
            if let Some(slash) = name.rfind('/') {
                let filename = &name[slash + 1..];
                if known.contains(filename) {
                    return true;
                }
            }
            return false;
        }
        // No pre-filter available → allow through
        true
    }

    fn matches_any_import(&self, uref: &UnresolvedRef) -> bool {
        let lang = language_from_file_path(&uref.file_path);
        if let Some(content) = self.caches.file_cache.get(&uref.file_path) {
            let mappings = extract_import_mappings(&uref.file_path, &content, &lang);
            return mappings.iter().any(|m| {
                m.local_name == uref.reference_name
                    || uref
                        .reference_name
                        .starts_with(&format!("{}.", m.local_name))
            });
        }
        false
    }

    fn resolve_via_import(&self, uref: &UnresolvedRef) -> Option<ResolvedEdge> {
        let lang = language_from_file_path(&uref.file_path);
        let content = self.caches.file_cache.get(&uref.file_path)?;
        let mappings = extract_import_mappings(&uref.file_path, &content, &lang);

        for mapping in &mappings {
            if mapping.local_name == uref.reference_name {
                // Try by original name first, fall back to local name
                let target_name = mapping
                    .original_name
                    .as_deref()
                    .unwrap_or(&mapping.local_name);
                let candidates = self.nodes_by_name(target_name);
                if let Some(node) = candidates.iter().find(|n| n.is_exported) {
                    return Some(make_name_match_edge(
                        uref,
                        node,
                        0.9,
                        ResolvedBy::ImportResolver,
                    ));
                }
                // Fallback to any match
                let candidates = self.nodes_by_name(&mapping.local_name);
                if let Some(node) = candidates.first() {
                    return Some(make_name_match_edge(
                        uref,
                        node,
                        0.7,
                        ResolvedBy::ImportResolver,
                    ));
                }
            }
        }
        None
    }

    /// Look up nodes by exact name, preferring the in-memory index (OPT-2/OPT-12)
    /// and falling back to DB only if the index is not yet built.
    #[inline]
    fn nodes_by_name(&self, name: &str) -> Vec<Node> {
        if let Some(idx) = &self.name_index {
            idx.get(name)
                .map(|refs| refs.iter().map(|r| r.to_node()).collect())
                .unwrap_or_default()
        } else {
            self.db.get_nodes_by_name(name)
        }
    }

    fn resolve_via_name_matcher(&self, uref: &UnresolvedRef) -> Option<ResolvedEdge> {
        let ctx = DbNameMatchCtx {
            db: &*self.db,
            name_index: self.name_index.as_ref(),
            lower_name_index: self.lower_name_index.as_ref(),
            qualified_name_index: self.qualified_name_index.as_ref(),
            file_index: self.file_index.as_ref(),
        };
        let result = match_reference(uref, &ctx)?;
        if result.confidence < 0.3 {
            return None;
        }
        Some(make_name_match_edge(
            uref,
            &result.node,
            result.confidence,
            ResolvedBy::NameMatcher,
        ))
    }

    fn is_builtin_or_external(&self, uref: &UnresolvedRef) -> bool {
        let name = &uref.reference_name;
        let is_js = matches!(
            language_from_file_path(&uref.file_path),
            Language::TypeScript | Language::JavaScript
        );

        if is_js && js_builtins().contains(name.as_str()) {
            return true;
        }
        if is_js
            && (name.starts_with("console.")
                || name.starts_with("Math.")
                || name.starts_with("JSON."))
        {
            return true;
        }
        if is_js && react_hooks().contains(name.as_str()) {
            return true;
        }

        if language_from_file_path(&uref.file_path) == Language::Python {
            if python_builtins().contains(name.as_str()) {
                return true;
            }
            if python_builtin_methods().contains(name.as_str()) {
                return true;
            }
            if let Some(dot) = name.find('.') {
                let receiver = &name[..dot];
                let method = &name[dot + 1..];
                if python_builtin_types().contains(receiver) {
                    return true;
                }
                if python_builtin_methods().contains(method) {
                    let cap = capitalize(receiver);
                    if self
                        .known_names
                        .as_ref()
                        .map(|s| !s.contains(cap.as_str()))
                        .unwrap_or(true)
                    {
                        return true;
                    }
                }
            }
        }

        if language_from_file_path(&uref.file_path) == Language::Go {
            if go_builtins().contains(name.as_str()) {
                return true;
            }
            if let Some(dot) = name.find('.') {
                let pkg = &name[..dot];
                if go_stdlib_packages().contains(pkg) {
                    return true;
                }
            }
        }

        false
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
        None => String::new(),
    }
}

fn language_from_file_path(path: &str) -> Language {
    if path.ends_with(".ts") || path.ends_with(".tsx") {
        Language::TypeScript
    } else if path.ends_with(".js") || path.ends_with(".jsx") {
        Language::JavaScript
    } else if path.ends_with(".py") {
        Language::Python
    } else if path.ends_with(".go") {
        Language::Go
    } else if path.ends_with(".rs") {
        Language::Rust
    } else if path.ends_with(".java") {
        Language::Java
    } else if path.ends_with(".rb") {
        Language::Ruby
    } else if path.ends_with(".php") {
        Language::Php
    } else if path.ends_with(".cs") {
        Language::CSharp
    } else {
        Language::Unknown
    }
}

/// Map an extraction-time `reference_kind` string to its semantic [`EdgeKind`].
///
/// GAP-3: name-matched and framework-resolved edges previously hardcoded
/// [`EdgeKind::References`], discarding the relationship the extractor recorded
/// (e.g. C#/.NET `implements` refs from `services.AddScoped<IFoo, Foo>()`). This
/// is the inverse of the canonical `EdgeKind → str` mapping used elsewhere
/// (`codewiki-mcp::tools::explore::format_edge_kind`); keep the two in sync.
///
/// Unknown / generic kinds fall back to [`EdgeKind::References`] rather than
/// inventing a variant. `"inherits"` is accepted as a synonym for `"extends"`
/// because some extractors phrase class inheritance that way.
pub(crate) fn edge_kind_from_reference_kind(reference_kind: &str) -> EdgeKind {
    match reference_kind {
        "calls" => EdgeKind::Calls,
        "imports" => EdgeKind::Imports,
        "contains" => EdgeKind::Contains,
        "extends" | "inherits" => EdgeKind::Extends,
        "implements" => EdgeKind::Implements,
        "uses" => EdgeKind::Uses,
        "instantiates" => EdgeKind::Instantiates,
        "exports" => EdgeKind::Exports,
        "renders" => EdgeKind::Renders,
        "resolves" => EdgeKind::Resolves,
        // "references" and any unrecognised kind stay generic.
        _ => EdgeKind::References,
    }
}

fn make_name_match_edge(
    uref: &UnresolvedRef,
    node: &Node,
    confidence: f32,
    resolved_by: ResolvedBy,
) -> ResolvedEdge {
    use codewiki_core::Edge;
    ResolvedEdge {
        edge: Edge {
            id: format!("{}->{}", uref.from_node_id, node.id),
            source_id: uref.from_node_id.clone(),
            target_id: node.id.clone(),
            // GAP-3: preserve the extractor's relationship instead of forcing
            // every name-matched edge to `References`. Edge orientation is
            // source = referrer (`from_node_id`), target = matched symbol —
            // which matches the implementer→interface / caller→callee
            // convention used by graph traversal (see
            // `codewiki-storage::graph::traversal::type_ancestors`, which walks
            // OUTGOING Implements/Extends edges from a type to its supertypes).
            kind: edge_kind_from_reference_kind(&uref.reference_kind),
            confidence: Some(confidence),
            provenance: Some(format!("{:?}", resolved_by)),
            ..Default::default()
        },
        resolved_from: ResolvedFromRef {
            from_node_id: uref.from_node_id.clone(),
            reference_name: uref.reference_name.clone(),
            reference_kind: uref.reference_kind.clone(),
            unresolved_ref_id: uref.id.parse::<i64>().unwrap_or(0),
        },
        confidence,
        resolved_by,
    }
}

// ─── NameMatchContext bridge ──────────────────────────────────────────────────

/// Bridge from `ReferenceResolver` to the `NameMatchContext` trait used by the
/// 5-strategy name matcher.
///
/// OPT-2: When the in-memory indexes are populated (after `warm_caches()`),
/// all name lookups are served from the `Arc<HashMap>` with zero DB round-trips.
/// Only the file-scoped lookups that are hard to build cheaply fall back to DB,
/// but those are also served from `file_index` when available.
struct DbNameMatchCtx<'a> {
    db: &'a dyn ResolverQueryHandle,
    name_index: Option<&'a NameIndex>,
    lower_name_index: Option<&'a NameIndex>,
    qualified_name_index: Option<&'a NameIndex>,
    file_index: Option<&'a NameIndex>,
}

impl<'a> NameMatchContext for DbNameMatchCtx<'a> {
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        if let Some(idx) = self.name_index {
            return idx
                .get(name)
                .map(|refs| refs.iter().map(|r| r.to_node()).collect())
                .unwrap_or_default();
        }
        self.db.get_nodes_by_name(name)
    }

    fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
        if let Some(idx) = self.lower_name_index {
            return idx
                .get(lower)
                .map(|refs| refs.iter().map(|r| r.to_node()).collect())
                .unwrap_or_default();
        }
        self.db.get_nodes_by_lower_name(lower)
    }

    fn get_nodes_by_qualified_name(&self, qname: &str) -> Vec<Node> {
        if let Some(idx) = self.qualified_name_index {
            return idx
                .get(qname)
                .map(|refs| refs.iter().map(|r| r.to_node()).collect())
                .unwrap_or_default();
        }
        self.db.get_nodes_by_qualified_name(qname)
    }

    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        if let Some(idx) = self.file_index {
            return idx
                .get(file_path)
                .map(|refs| refs.iter().map(|r| r.to_node()).collect())
                .unwrap_or_default();
        }
        self.db.get_nodes_in_file(file_path)
    }

    fn all_file_paths(&self) -> Vec<String> {
        // known_files is already stored on ReferenceResolver; the name_matcher
        // only calls this for Strategy 1 (file-path match).  We serve it from
        // the file_index keys when available to avoid another DB round-trip.
        if let Some(idx) = self.file_index {
            return idx.keys().cloned().collect();
        }
        self.db.get_all_file_paths()
    }

    fn file_path_has_node(&self, file_path: &str) -> bool {
        if let Some(idx) = self.file_index {
            return idx.contains_key(file_path);
        }
        !self.db.get_nodes_in_file(file_path).is_empty()
    }
}

#[cfg(test)]
mod edge_kind_mapping_tests {
    use super::{edge_kind_from_reference_kind, make_name_match_edge};
    use codewiki_core::{EdgeKind, Node, UnresolvedRef};
    use codewiki_storage::traits::ResolvedBy;

    // GAP-3: reference_kind strings must map to their semantic EdgeKind, not be
    // collapsed to References. This is the inverse of format_edge_kind in
    // codewiki-mcp; keep both in sync.
    #[test]
    fn reference_kind_maps_to_semantic_edge_kind() {
        assert_eq!(edge_kind_from_reference_kind("calls"), EdgeKind::Calls);
        assert_eq!(edge_kind_from_reference_kind("imports"), EdgeKind::Imports);
        assert_eq!(
            edge_kind_from_reference_kind("contains"),
            EdgeKind::Contains
        );
        assert_eq!(edge_kind_from_reference_kind("extends"), EdgeKind::Extends);
        assert_eq!(edge_kind_from_reference_kind("inherits"), EdgeKind::Extends);
        assert_eq!(
            edge_kind_from_reference_kind("implements"),
            EdgeKind::Implements
        );
        assert_eq!(edge_kind_from_reference_kind("uses"), EdgeKind::Uses);
        assert_eq!(
            edge_kind_from_reference_kind("instantiates"),
            EdgeKind::Instantiates
        );
        assert_eq!(edge_kind_from_reference_kind("exports"), EdgeKind::Exports);
        assert_eq!(edge_kind_from_reference_kind("renders"), EdgeKind::Renders);
        assert_eq!(
            edge_kind_from_reference_kind("resolves"),
            EdgeKind::Resolves
        );
    }

    #[test]
    fn references_and_unknown_kinds_fall_back_to_references() {
        assert_eq!(
            edge_kind_from_reference_kind("references"),
            EdgeKind::References
        );
        assert_eq!(edge_kind_from_reference_kind(""), EdgeKind::References);
        assert_eq!(
            edge_kind_from_reference_kind("something-bespoke"),
            EdgeKind::References
        );
    }

    // The C#/.NET DI case: an `implements` ref must resolve to an Implements
    // edge, with the implementing-side node as source and the matched symbol as
    // target (the implementer→interface orientation graph traversal expects).
    #[test]
    fn implements_ref_produces_implements_edge_with_correct_direction() {
        let uref = UnresolvedRef {
            id: "1".to_string(),
            from_node_id: "impl-side-node".to_string(),
            reference_name: "UserService".to_string(),
            reference_kind: "implements".to_string(),
            ..Default::default()
        };
        let node = Node {
            id: "interface-side-node".to_string(),
            name: "IUserService".to_string(),
            ..Default::default()
        };
        let resolved = make_name_match_edge(&uref, &node, 0.9, ResolvedBy::NameMatcher);
        assert_eq!(resolved.edge.kind, EdgeKind::Implements);
        assert_eq!(resolved.edge.source_id, "impl-side-node");
        assert_eq!(resolved.edge.target_id, "interface-side-node");
    }

    #[test]
    fn calls_ref_produces_calls_edge() {
        let uref = UnresolvedRef {
            id: "2".to_string(),
            from_node_id: "caller".to_string(),
            reference_name: "doWork".to_string(),
            reference_kind: "calls".to_string(),
            ..Default::default()
        };
        let node = Node {
            id: "callee".to_string(),
            ..Default::default()
        };
        let resolved = make_name_match_edge(&uref, &node, 0.8, ResolvedBy::NameMatcher);
        assert_eq!(resolved.edge.kind, EdgeKind::Calls);
    }
}
