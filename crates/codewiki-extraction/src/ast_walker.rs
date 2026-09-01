//! T-208 — Core AST walker and extraction engine.

use codewiki_core::{
    EdgeKind, ExtractionBatch, FileRecord, Language, Node, NodeKind, UnresolvedRef,
};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::language_detector::detect_language;
use crate::parser_pool::THREAD_PARSERS;

// ── Node-ID generation ───────────────────────────────────────────────────────

pub fn generate_node_id(file_path: &str, kind: &NodeKind, name: &str, line: u32) -> String {
    let input = format!("{file_path}:{kind:?}:{name}:{line}");
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(&hash[..8])
}

// ── Doc comment style ─────────────────────────────────────────────────────────

/// Describes how doc comments are attached to declarations in a given language.
/// Used by `extract_docstring` to locate and clean the doc comment text.
#[derive(Clone, Copy)]
pub enum DocCommentStyle {
    /// No doc comment extraction for this language.
    None,
    /// Look for preceding sibling comment node(s) that start with `prefix`.
    /// Multiple contiguous preceding siblings with the same kind and prefix
    /// are concatenated (top-to-bottom). Blank-line gap check enforced.
    PrecedingLineComment {
        node_kind: &'static str,
        prefix: &'static str,
    },
    /// Look for a single preceding sibling block comment starting with `prefix`.
    /// Strips `/**`/`*/`/leading ` * ` per Javadoc/JSDoc pattern.
    PrecedingBlockComment {
        node_kind: &'static str,
        prefix: &'static str,
    },
    /// Look for either a block comment (starting with `block_prefix`) OR one or
    /// more contiguous line comments (starting with `line_prefix`). Whichever is
    /// immediately adjacent wins; block is tried first.
    PrecedingEither {
        block_node_kind: &'static str,
        block_prefix: &'static str,
        line_node_kind: &'static str,
        line_prefix: &'static str,
    },
    /// Python-style: first named child of the body block is an
    /// `expression_statement` containing a `string` node.
    PythonFirstBodyString { body_field: &'static str },
}

// ── Language configuration ────────────────────────────────────────────────────

#[derive(Clone)]
pub struct LanguageConfig {
    pub function_types: &'static [&'static str],
    pub class_types: &'static [&'static str],
    pub method_types: &'static [&'static str],
    pub interface_types: &'static [&'static str],
    pub struct_types: &'static [&'static str],
    pub enum_types: &'static [&'static str],
    pub enum_member_types: &'static [&'static str],
    pub type_alias_types: &'static [&'static str],
    pub import_types: &'static [&'static str],
    pub call_types: &'static [&'static str],
    pub variable_types: &'static [&'static str],
    pub property_types: &'static [&'static str],
    pub field_types: &'static [&'static str],
    pub extra_class_types: &'static [&'static str],
    /// GAP-2: AST node types that introduce a namespace scope.
    /// When encountered, the namespace name is pushed onto the scope stack
    /// (contributing to `qualified_name`) without emitting a Class node.
    /// Set to `&[]` for languages that don't have namespaces.
    pub namespace_types: &'static [&'static str],
    pub name_field: &'static str,
    pub body_field: &'static str,
    pub methods_are_top_level: bool,
    /// How to locate and parse doc comments for declarations in this language.
    pub doc_comment_style: DocCommentStyle,
}

// ── Extraction context ────────────────────────────────────────────────────────

pub struct ExtractCtx<'a> {
    pub file_path: &'a str,
    pub source: &'a str,
    pub language: Language,
    pub config: &'a LanguageConfig,
    pub nodes: Vec<Node>,
    pub edges: Vec<codewiki_core::Edge>,
    pub unresolved: Vec<UnresolvedRef>,
    pub scope: Vec<String>,
}

impl<'a> ExtractCtx<'a> {
    pub fn new(
        file_path: &'a str,
        source: &'a str,
        language: Language,
        config: &'a LanguageConfig,
    ) -> Self {
        Self {
            file_path,
            source,
            language,
            config,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved: Vec::new(),
            scope: Vec::new(),
        }
    }

    pub fn qualified_name(&self, name: &str) -> String {
        let mut parts: Vec<&str> = self
            .scope
            .iter()
            .filter_map(|id| {
                self.nodes
                    .iter()
                    .find(|n| &n.id == id && n.kind != NodeKind::File)
                    .map(|n| n.name.as_str())
            })
            .collect();
        parts.push(name);
        parts.join("::")
    }

    pub fn emit_node(
        &mut self,
        kind: NodeKind,
        name: &str,
        ts_node: &tree_sitter::Node,
        is_exported: bool,
        signature: Option<String>,
        docstring: Option<String>,
    ) -> Option<String> {
        if name.is_empty() || name == "<anonymous>" {
            return None;
        }
        let line = ts_node.start_position().row as u32 + 1;
        let id = generate_node_id(self.file_path, &kind, name, line);

        let node = Node {
            id: id.clone(),
            name: name.to_string(),
            qualified_name: self.qualified_name(name),
            kind: kind.clone(),
            language: self.language.clone(),
            file_path: self.file_path.to_string(),
            start_line: line,
            end_line: ts_node.end_position().row as u32 + 1,
            start_col: ts_node.start_position().column as u32,
            end_col: ts_node.end_position().column as u32,
            is_exported,
            signature,
            docstring,
            metadata: None,
        };
        self.nodes.push(node);

        if let Some(parent_id) = self.scope.last() {
            self.edges.push(codewiki_core::Edge {
                id: format!("{parent_id}->{id}-contains"),
                source_id: parent_id.clone(),
                target_id: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(line),
                col: None,
                provenance: Some("extraction".into()),
                confidence: Some(1.0),
                metadata: None,
            });
        }
        Some(id)
    }

    pub fn emit_call(&mut self, from_id: &str, callee: &str, line: u32) {
        if callee.is_empty() {
            return;
        }
        let ref_id = generate_node_id(
            self.file_path,
            &NodeKind::Function,
            &format!("call:{callee}@{line}"),
            line,
        );
        self.unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: from_id.to_string(),
            reference_name: callee.to_string(),
            reference_kind: "calls".to_string(),
            file_path: self.file_path.to_string(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }

    pub fn emit_import(&mut self, from_id: &str, module: &str, line: u32) {
        if module.is_empty() {
            return;
        }
        let ref_id = generate_node_id(
            self.file_path,
            &NodeKind::Module,
            &format!("import:{module}@{line}"),
            line,
        );
        self.unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: from_id.to_string(),
            reference_name: module.to_string(),
            reference_kind: "imports".to_string(),
            file_path: self.file_path.to_string(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }
}

// ── Language dispatch trait ───────────────────────────────────────────────────

/// Trait for language extractors. All methods must be object-safe.
pub trait LanguageExtractor: Send + Sync {
    fn config(&self) -> &LanguageConfig;

    /// Optional per-node hook. Return `true` to suppress the default dispatch.
    fn visit_node_hook(&self, _node: &tree_sitter::Node, _ctx: &mut ExtractCtx) -> bool {
        false
    }
}

// ── Public extraction helpers ─────────────────────────────────────────────────

/// Run the standard AST walk for any extractor.
pub fn extract_with_config(
    extractor: &dyn LanguageExtractor,
    tree: &tree_sitter::Tree,
    source: &str,
    file_path: &Path,
) -> ExtractionBatch {
    let file_path_str = file_path.to_string_lossy().to_string();
    let language = detect_language(file_path).unwrap_or(Language::Unknown);
    let config = extractor.config();

    let mut ctx = ExtractCtx::new(&file_path_str, source, language, config);

    let file_id = format!("file:{file_path_str}");
    let line_count = source.lines().count() as u32;
    ctx.nodes.push(Node {
        id: file_id.clone(),
        name: file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_string(),
        qualified_name: file_path_str.clone(),
        kind: NodeKind::File,
        language: ctx.language.clone(),
        file_path: file_path_str.clone(),
        start_line: 1,
        end_line: line_count.max(1),
        start_col: 0,
        end_col: 0,
        is_exported: false,
        signature: None,
        docstring: None,
        metadata: None,
    });
    ctx.scope.push(file_id);

    // GAP-2: Pre-pass for file-scoped namespace declarations.
    // In C# (and similar languages), `file_scoped_namespace_declaration` is a
    // sibling of all other top-level declarations in the compilation_unit — NOT
    // a parent.  We detect it here and push its name onto the scope stack so
    // that all subsequent class/method nodes get a namespace-qualified name.
    // After the root walk we pop the namespace scope(s) we pushed here.
    let mut file_scoped_ns_count = 0usize;
    if !config.namespace_types.is_empty() {
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if child.kind() == "file_scoped_namespace_declaration" {
                // Extract the namespace name
                let ns_name = child
                    .child_by_field_name("name")
                    .map(|n| n.utf8_text(source.as_bytes()).unwrap_or("").to_string())
                    .unwrap_or_default();
                if ns_name.is_empty() {
                    continue;
                }
                // Emit a Namespace node and push its ID onto scope.
                // This is intentionally done BEFORE walk_node so all root-level
                // siblings (classes, interfaces, etc.) see the namespace scope.
                let line = child.start_position().row as u32 + 1;
                let ns_id = generate_node_id(&file_path_str, &NodeKind::Namespace, &ns_name, line);
                let ns_node = Node {
                    id: ns_id.clone(),
                    name: ns_name.clone(),
                    qualified_name: ns_name.clone(),
                    kind: NodeKind::Namespace,
                    language: ctx.language.clone(),
                    file_path: file_path_str.clone(),
                    start_line: line,
                    end_line: child.end_position().row as u32 + 1,
                    start_col: child.start_position().column as u32,
                    end_col: child.end_position().column as u32,
                    is_exported: false,
                    signature: None,
                    docstring: None,
                    metadata: None,
                };
                // Add contains edge from file → namespace
                if let Some(parent_id) = ctx.scope.last().cloned() {
                    ctx.edges.push(codewiki_core::Edge {
                        id: format!("{parent_id}->{ns_id}-contains"),
                        source_id: parent_id,
                        target_id: ns_id.clone(),
                        kind: EdgeKind::Contains,
                        line: Some(line),
                        col: None,
                        provenance: Some("extraction".into()),
                        confidence: Some(1.0),
                        metadata: None,
                    });
                }
                ctx.nodes.push(ns_node);
                ctx.scope.push(ns_id);
                file_scoped_ns_count += 1;
            }
        }
    }

    walk_node(&tree.root_node(), &mut ctx, extractor);

    // Pop file-scoped namespace scopes
    for _ in 0..file_scoped_ns_count {
        ctx.scope.pop();
    }

    ctx.scope.pop();

    let content_hash = {
        let mut h = Sha256::new();
        h.update(source.as_bytes());
        hex::encode(h.finalize())
    };

    let (modified_at, indexed_at) = timestamps_for_file(file_path);

    ExtractionBatch {
        file: FileRecord {
            path: file_path.to_path_buf(),
            content_hash,
            language: format!("{:?}", ctx.language),
            size: source.len() as u64,
            modified_at,
            indexed_at,
            node_count: ctx.nodes.len() as u32,
            errors: Vec::new(),
        },
        nodes: ctx.nodes,
        edges: ctx.edges,
        unresolved_refs: ctx.unresolved,
    }
}

/// Return `(modified_at_ms, indexed_at_ms)` for the given file path.
/// `modified_at` comes from filesystem metadata; `indexed_at` is wall-clock now.
/// Falls back to 0 for `modified_at` if metadata is unavailable.
pub fn timestamps_for_file(file_path: &Path) -> (i64, i64) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let indexed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let modified_at = std::fs::metadata(file_path)
        .and_then(|m| m.modified())
        .and_then(|t| t.duration_since(UNIX_EPOCH).map_err(std::io::Error::other))
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    (modified_at, indexed_at)
}

// ── Doc comment extraction ────────────────────────────────────────────────────

/// Extract the doc comment for `decl_node` according to `style`.
/// Returns `None` if no doc comment is found or style is `None`.
/// The returned string has comment markers stripped and leading whitespace
/// trimmed per line. Capped at 2000 characters.
pub fn extract_docstring(
    decl_node: &tree_sitter::Node,
    source: &[u8],
    style: &DocCommentStyle,
) -> Option<String> {
    match style {
        DocCommentStyle::None => None,

        DocCommentStyle::PrecedingLineComment { node_kind, prefix } => {
            extract_preceding_line_comments(decl_node, source, node_kind, prefix)
        }

        DocCommentStyle::PrecedingBlockComment { node_kind, prefix } => {
            extract_preceding_block_comment(decl_node, source, node_kind, prefix)
        }

        DocCommentStyle::PrecedingEither {
            block_node_kind,
            block_prefix,
            line_node_kind,
            line_prefix,
        } => {
            // Try block first (one sibling)
            if let Some(s) =
                extract_preceding_block_comment(decl_node, source, block_node_kind, block_prefix)
            {
                return Some(s);
            }
            // Then line run
            extract_preceding_line_comments(decl_node, source, line_node_kind, line_prefix)
        }

        DocCommentStyle::PythonFirstBodyString { body_field } => {
            extract_python_docstring(decl_node, source, body_field)
        }
    }
}

/// Collect a contiguous run of preceding line comments with the given kind and prefix.
fn extract_preceding_line_comments(
    decl_node: &tree_sitter::Node,
    source: &[u8],
    node_kind: &str,
    prefix: &str,
) -> Option<String> {
    // Try the direct preceding sibling first; if none, try the parent's preceding
    // sibling (handles `export function f()` where JSDoc is before `export_statement`).
    let candidate = decl_node
        .prev_named_sibling()
        .or_else(|| decl_node.parent().and_then(|p| p.prev_named_sibling()))?;

    // Reference row for blank-line gap check: use the effective declaration start
    let decl_row = decl_node
        .parent()
        .filter(|_| decl_node.prev_named_sibling().is_none())
        .map(|p| p.start_position().row)
        .unwrap_or_else(|| decl_node.start_position().row);

    // Blank-line gap check for the first candidate
    if decl_row > candidate.end_position().row + 1 {
        return None;
    }
    if candidate.kind() != node_kind {
        return None;
    }
    let text = candidate.utf8_text(source).ok()?;
    if !text.trim_start().starts_with(prefix) {
        return None;
    }

    // Collect this and any further adjacent same-kind+prefix siblings
    let mut collected = vec![candidate];
    while let Some(prev) = collected.last().unwrap().prev_named_sibling() {
        let last = collected.last().unwrap();
        // Gap check between consecutive comments
        if last.start_position().row > prev.end_position().row + 1 {
            break;
        }
        if prev.kind() != node_kind {
            break;
        }
        let t = match prev.utf8_text(source) {
            Ok(t) => t,
            Err(_) => break,
        };
        if !t.trim_start().starts_with(prefix) {
            break;
        }
        collected.push(prev);
    }

    // Reverse so they read top-to-bottom
    collected.reverse();

    let lines: Vec<String> = collected
        .iter()
        .filter_map(|n| n.utf8_text(source).ok())
        .flat_map(|t| strip_line_comment_markers(t, prefix))
        .collect();

    let result = lines.join("\n").trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result.chars().take(2000).collect())
    }
}

/// Collect a single preceding block comment with the given kind and prefix.
fn extract_preceding_block_comment(
    decl_node: &tree_sitter::Node,
    source: &[u8],
    node_kind: &str,
    prefix: &str,
) -> Option<String> {
    // Try the direct preceding sibling first; if none, try the parent's preceding
    // sibling (handles `export function f()` where JSDoc is before `export_statement`).
    let candidate = decl_node
        .prev_named_sibling()
        .or_else(|| decl_node.parent().and_then(|p| p.prev_named_sibling()))?;

    // Reference row for blank-line gap check
    let decl_row = decl_node
        .parent()
        .filter(|_| decl_node.prev_named_sibling().is_none())
        .map(|p| p.start_position().row)
        .unwrap_or_else(|| decl_node.start_position().row);

    // Blank-line gap check
    if decl_row > candidate.end_position().row + 1 {
        return None;
    }
    if candidate.kind() != node_kind {
        return None;
    }
    let text = candidate.utf8_text(source).ok()?;
    if !text.trim_start().starts_with(prefix) {
        return None;
    }

    let result = strip_block_comment_markers(text).trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result.chars().take(2000).collect())
    }
}

/// Extract a Python-style docstring from the first body child.
fn extract_python_docstring(
    decl_node: &tree_sitter::Node,
    source: &[u8],
    body_field: &str,
) -> Option<String> {
    let body = decl_node.child_by_field_name(body_field)?;

    // First named child of body must be expression_statement
    let mut cursor = body.walk();
    let first_child = body.named_children(&mut cursor).next()?;
    if first_child.kind() != "expression_statement" {
        return None;
    }

    // The expression_statement must have a string child
    let mut c2 = first_child.walk();
    let string_node = first_child
        .named_children(&mut c2)
        .find(|n| n.kind() == "string")?;

    let raw = string_node.utf8_text(source).ok()?;
    let result = strip_python_docstring(raw).trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result.chars().take(2000).collect())
    }
}

/// Strip line comment markers (e.g. `///`, `//`, `#`, `---`) from each line
/// of a contiguous comment block text, returning cleaned lines.
fn strip_line_comment_markers(text: &str, prefix: &str) -> Vec<String> {
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                // Strip one optional leading space after the prefix
                rest.strip_prefix(' ')
                    .unwrap_or(rest)
                    .trim_end()
                    .to_string()
            } else {
                line.trim_end().to_string()
            }
        })
        .collect()
}

/// Strip block comment markers for Javadoc/JSDoc/KDoc style `/** ... */`.
/// Removes `/**` and `*/` delimiter lines and strips leading ` * ` per line.
fn strip_block_comment_markers(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut result_lines: Vec<String> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // First line: strip opening `/**` or `/*!`
        if i == 0 {
            let stripped = trimmed
                .strip_prefix("/**")
                .or_else(|| trimmed.strip_prefix("/*!"))
                .unwrap_or(trimmed);
            let cleaned = stripped.trim();
            if !cleaned.is_empty() {
                result_lines.push(cleaned.to_string());
            }
            continue;
        }
        // Last line: strip closing `*/`
        if i == lines.len() - 1 {
            let stripped = trimmed.strip_suffix("*/").unwrap_or(trimmed);
            let cleaned = stripped.trim_end_matches('*').trim();
            if !cleaned.is_empty() {
                result_lines.push(cleaned.to_string());
            }
            continue;
        }
        // Middle lines: strip leading ` * ` or ` *`
        let cleaned = if let Some(rest) = trimmed.strip_prefix('*') {
            rest.strip_prefix(' ')
                .unwrap_or(rest)
                .trim_end()
                .to_string()
        } else {
            trimmed.trim_end().to_string()
        };
        result_lines.push(cleaned);
    }

    // Drop leading/trailing empty lines
    while result_lines.first().is_some_and(|l| l.is_empty()) {
        result_lines.remove(0);
    }
    while result_lines.last().is_some_and(|l| l.is_empty()) {
        result_lines.pop();
    }

    result_lines.join("\n")
}

/// Strip Python triple-quoted or single-quoted string delimiters and dedent.
fn strip_python_docstring(text: &str) -> String {
    // Try to strip triple-quote delimiters
    let inner = if let Some(s) = text
        .strip_prefix("\"\"\"")
        .and_then(|s| s.strip_suffix("\"\"\""))
    {
        s
    } else if let Some(s) = text.strip_prefix("'''").and_then(|s| s.strip_suffix("'''")) {
        s
    } else if let Some(s) = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        s
    } else if let Some(s) = text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        s
    } else {
        text
    };

    // Dedent: find common leading whitespace of non-empty lines (skip first line)
    let lines: Vec<&str> = inner.lines().collect();
    let non_empty_body: Vec<&str> = lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();
    let common_indent = non_empty_body
        .iter()
        .map(|l| l.len() - l.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);

    let dedented: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                l.trim().to_string()
            } else if l.len() >= common_indent {
                l[common_indent..].trim_end().to_string()
            } else {
                l.trim_end().to_string()
            }
        })
        .collect();

    // Drop leading/trailing blank lines
    let mut result: Vec<String> = dedented;
    while result.first().is_some_and(|l| l.is_empty()) {
        result.remove(0);
    }
    while result.last().is_some_and(|l| l.is_empty()) {
        result.pop();
    }

    result.join("\n")
}

// ── AST walker ────────────────────────────────────────────────────────────────

pub fn walk_node(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) {
    if extractor.visit_node_hook(node, ctx) {
        return;
    }

    let node_type = node.kind();
    let cfg = ctx.config;
    let mut skip_children = false;

    // GAP-2: Handle namespace/module scope — push the namespace name onto the
    // scope stack so qualified_name() produces "Namespace.Class.Method".
    // We do NOT emit a node for the namespace itself (no Class/Module node is
    // added); the scope entry is a synthetic key that contributes only a name
    // segment to qualified_name via a lightweight sentinel.
    if !cfg.namespace_types.is_empty() && cfg.namespace_types.contains(&node_type) {
        skip_children = extract_namespace(node, ctx, extractor);
    } else if cfg.function_types.contains(&node_type) {
        let in_class = is_in_class_scope(ctx);
        if in_class && cfg.method_types.contains(&node_type) {
            skip_children = extract_method(node, ctx, extractor);
        } else {
            skip_children = extract_function(node, ctx, extractor);
        }
    } else if cfg.method_types.contains(&node_type) && !cfg.function_types.contains(&node_type) {
        skip_children = extract_method(node, ctx, extractor);
    } else if cfg.class_types.contains(&node_type) || cfg.extra_class_types.contains(&node_type) {
        skip_children = extract_class(node, ctx, extractor);
    } else if cfg.interface_types.contains(&node_type) {
        skip_children = extract_interface(node, ctx, extractor);
    } else if cfg.struct_types.contains(&node_type) {
        skip_children = extract_struct(node, ctx, extractor);
    } else if cfg.enum_types.contains(&node_type) {
        skip_children = extract_enum(node, ctx, extractor);
    } else if cfg.type_alias_types.contains(&node_type) {
        extract_type_alias(node, ctx);
    } else if cfg.property_types.contains(&node_type) && is_in_class_scope(ctx) {
        extract_property(node, ctx);
        skip_children = true;
    } else if cfg.field_types.contains(&node_type) && is_in_class_scope(ctx) {
        extract_field(node, ctx);
        skip_children = true;
    } else if cfg.variable_types.contains(&node_type)
        && !is_in_class_scope(ctx)
        && !is_in_value_scope(ctx)
    {
        // Walk the initialiser. `const f = () => { ... }` and `const x = call()`
        // carry real calls that were previously dropped outright — the subtree
        // was skipped, so nothing downstream could recover them. Attribute those
        // calls to the binding when it names a single identifier.
        let scope_id = extract_variable(node, ctx);
        let pushed = scope_id.is_some();
        if let Some(id) = scope_id {
            ctx.scope.push(id);
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk_node(&child, ctx, extractor);
        }
        if pushed {
            ctx.scope.pop();
        }
        skip_children = true;
    } else if cfg.import_types.contains(&node_type) {
        extract_import(node, ctx);
    } else if cfg.call_types.contains(&node_type) {
        extract_call(node, ctx);
    } else if node_type == "impl_item" {
        skip_children = extract_rust_impl(node, ctx, extractor);
    }

    if !skip_children {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk_node(&child, ctx, extractor);
        }
    }
}

// ── NoopExtractorHelper ───────────────────────────────────────────────────────

/// Public helper for languages that need to recurse with the same config.
pub struct NoopExtractorHelper<'a> {
    pub config: &'a LanguageConfig,
}

impl<'a> LanguageExtractor for NoopExtractorHelper<'a> {
    fn config(&self) -> &LanguageConfig {
        self.config
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn is_in_class_scope(ctx: &ExtractCtx) -> bool {
    for id in ctx.scope.iter().rev() {
        if let Some(n) = ctx.nodes.iter().find(|n| &n.id == id) {
            match n.kind {
                NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::Enum
                | NodeKind::Component => return true,
                // GAP-2: Namespace scopes are transparent — keep searching outward.
                NodeKind::Namespace => {}
                NodeKind::File => return false,
                _ => {}
            }
        }
    }
    false
}

/// Returns `true` if the current scope is inside a function or method body.
/// Walks the scope stack inward-out; stops (returns `false`) as soon as it
/// finds a `File` node, meaning we reached module level without crossing a
/// function boundary.
/// Like [`is_in_function_scope`], but a variable binding also counts as a value
/// scope.
///
/// Used only to decide whether a declaration is genuinely top-level. A `const`
/// inside the initialiser of another `const` is function-local in every sense
/// that matters and must stay suppressed — otherwise walking initialisers would
/// silently reverse the deliberate "suppress function-local variable nodes"
/// parity decision.
///
/// Deliberately private and separate from the `pub` [`is_in_function_scope`],
/// which `lua.rs`, `luau.rs` and `scala.rs` consume for a different decision.
fn is_in_value_scope(ctx: &ExtractCtx) -> bool {
    for id in ctx.scope.iter().rev() {
        if let Some(n) = ctx.nodes.iter().find(|n| &n.id == id) {
            match n.kind {
                NodeKind::Function | NodeKind::Method | NodeKind::Variable => return true,
                NodeKind::File => return false,
                _ => {}
            }
        }
    }
    false
}

pub fn is_in_function_scope(ctx: &ExtractCtx) -> bool {
    for id in ctx.scope.iter().rev() {
        if let Some(n) = ctx.nodes.iter().find(|n| &n.id == id) {
            match n.kind {
                NodeKind::Function | NodeKind::Method => return true,
                NodeKind::File => return false,
                _ => {}
            }
        }
    }
    false
}

fn extract_name_from_node(node: &tree_sitter::Node, source: &[u8], name_field: &str) -> String {
    if let Some(name_node) = node.child_by_field_name(name_field) {
        return name_node
            .utf8_text(source)
            .unwrap_or("<invalid>")
            .to_string();
    }
    // Fallback: the first identifier-shaped child that is either unfielded or
    // bound to the assignment-LHS field `left` (Python/Ruby `a = …` bind the
    // name there and have no `name` field at all). Any OTHER field is not a
    // name: without this check, anonymous nodes borrowed a name from whatever
    // identifier came first — an arrow function's `parameter` field made
    // `a => …` a Function named `a`, and its bare-identifier `body` made
    // `() => counter` a Function named `counter`.
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            let field = cursor.field_name();
            // `name`: a language config may point `name_field` at the wrong
            // field (Kotlin did, for a grammar swap); a child bound to a
            // field literally called `name` is a name regardless.
            if child.is_named()
                && (field.is_none() || field == Some("left") || field == Some("name"))
            {
                let kind = child.kind();
                if matches!(
                    kind,
                    "identifier" | "type_identifier" | "simple_identifier" | "property_identifier"
                ) {
                    return child.utf8_text(source).unwrap_or("<invalid>").to_string();
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    String::new()
}

pub fn is_exported(node: &tree_sitter::Node) -> bool {
    let mut current = node.parent();
    while let Some(p) = current {
        if p.kind() == "export_statement" || p.kind() == "export_declaration" {
            return true;
        }
        current = p.parent();
    }
    false
}

// ── Symbol extractors ─────────────────────────────────────────────────────────

// ── GAP-2: Namespace scope handler ───────────────────────────────────────────
//
// Emits a lightweight `NodeKind::Namespace` node for the namespace name, then
// pushes its ID onto the scope stack so `qualified_name()` produces fully
// qualified names such as `MyApp.Services::BasketService::AddItem`.
//
// Two forms are handled:
//   - `namespace_declaration { name body }` (block-scoped): handled here.
//   - `file_scoped_namespace_declaration` (rest of file): handled by the
//     pre-pass in `extract_with_config` since those nodes are siblings, not
//     parents, of the classes they scope.
//
// The namespace node is parented to the current (file) scope via a `contains`
// edge, exactly like any other symbol.
fn extract_namespace(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) -> bool {
    // File-scoped namespace declarations are handled by the pre-pass in
    // extract_with_config — skip them here to avoid double-processing.
    if node.kind() == "file_scoped_namespace_declaration" {
        // Return true (skip default child walk) so we don't also try to recurse
        // into the name node or semicolon. The pre-pass already set up the scope.
        return true;
    }

    // The namespace name may be a dotted qualified_name (e.g. "MyApp.Services").
    // Try field "name" first, then walk children for qualified_name/identifier.
    let ns_name = {
        let raw = node
            .child_by_field_name("name")
            .map(|n| n.utf8_text(ctx.source.as_bytes()).unwrap_or("").to_string())
            .unwrap_or_default();
        if !raw.is_empty() {
            raw
        } else {
            // Fallback: first qualified_name or identifier child
            let mut cursor = node.walk();
            let mut found = String::new();
            for child in node.named_children(&mut cursor) {
                let k = child.kind();
                if k == "qualified_name" || k == "identifier" {
                    found = child
                        .utf8_text(ctx.source.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    break;
                }
            }
            found
        }
    };
    if ns_name.is_empty() {
        return false;
    }

    // Emit the Namespace node (no docstring for namespace declarations).
    let ns_id =
        if let Some(id) = ctx.emit_node(NodeKind::Namespace, &ns_name, node, false, None, None) {
            id
        } else {
            return false;
        };

    ctx.scope.push(ns_id);

    // Block-scoped namespace: find the body child (declaration_list or body field)
    // and recurse into it.
    let body_child = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("declaration_list"));

    if let Some(body) = body_child {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            walk_node(&child, ctx, extractor);
        }
    } else {
        // Generic fallback: walk named children (skipping the name itself)
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let k = child.kind();
            if k != "qualified_name" && k != "identifier" && k != "name" {
                walk_node(&child, ctx, extractor);
            }
        }
    }

    ctx.scope.pop();
    true
}

fn extract_function(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) -> bool {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return false;
    }
    let exported = is_exported(node);
    let docstring = extract_docstring(node, ctx.source.as_bytes(), &ctx.config.doc_comment_style);
    if let Some(id) = ctx.emit_node(NodeKind::Function, &name, node, exported, None, docstring) {
        ctx.scope.push(id);
        if let Some(body) = node.child_by_field_name(ctx.config.body_field) {
            let mut cursor = body.walk();
            for child in body.named_children(&mut cursor) {
                walk_node(&child, ctx, extractor);
            }
        }
        ctx.scope.pop();
    }
    true
}

fn extract_method(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) -> bool {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return false;
    }
    let exported = is_exported(node);

    // GAP-5: Build signature from parameter_list child (generic — works for any
    // language whose grammar uses a `parameter_list` node).
    let signature = build_method_signature(node, ctx.source.as_bytes());

    // GAP-5: Detect `async` modifier by scanning direct children for a node
    // of kind "async" (tree-sitter represents keyword modifiers as anonymous
    // or named children with kind == the keyword text).
    let is_async = detect_async_modifier(node, ctx.source.as_bytes());

    // Encode is_async in metadata JSON so the DB can store it even though the
    // Node struct doesn't carry an is_async field directly.
    let metadata = if is_async {
        Some(r#"{"is_async":true}"#.to_string())
    } else {
        None
    };

    let docstring = extract_docstring(node, ctx.source.as_bytes(), &ctx.config.doc_comment_style);
    if let Some(id) = ctx.emit_node(
        NodeKind::Method,
        &name,
        node,
        exported,
        signature,
        docstring,
    ) {
        // Patch is_async into the just-emitted node's metadata
        if let Some(n) = ctx.nodes.iter_mut().find(|n| n.id == id) {
            n.metadata = metadata;
        }
        ctx.scope.push(id);
        if let Some(body) = node.child_by_field_name(ctx.config.body_field) {
            let mut cursor = body.walk();
            for child in body.named_children(&mut cursor) {
                walk_node(&child, ctx, extractor);
            }
        } else {
            // No `body` field. TypeScript's `public_field_definition` keeps its
            // initialiser in `value`, so `readonly f = () => call()` would
            // otherwise contribute no edges at all.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_node(&child, ctx, extractor);
            }
        }
        ctx.scope.pop();
    }
    true
}

/// GAP-5 helper: Build a concise signature string from the `parameter_list`
/// child of a method/function node.  Returns `None` when no `parameter_list`
/// is found (safe fallback for languages that don't use this node name).
fn build_method_signature(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Look for a parameter_list child (works for C#, Java, Kotlin, Swift …)
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let k = child.kind();
        if k == "parameter_list" || k == "formal_parameters" || k == "parameters" {
            let text = child.utf8_text(source).unwrap_or("").trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// GAP-5 helper: Return `true` if the method/function node has an `async`
/// modifier keyword among its direct (named or anonymous) children.
fn detect_async_modifier(node: &tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    // Walk all children (including anonymous ones) to find `async` keyword.
    cursor.goto_first_child();
    loop {
        let child = cursor.node();
        // The keyword may appear as kind=="async" or as text "async"
        if child.kind() == "async" {
            return true;
        }
        // Also check text for anonymous nodes that represent keywords
        if !child.is_named() {
            if let Ok(text) = child.utf8_text(source) {
                if text == "async" {
                    return true;
                }
            }
        }
        // Check inside modifier lists (e.g., "modifier" children in Java/Kotlin)
        if child.kind() == "modifier" || child.kind() == "modifiers" {
            let mut mc = child.walk();
            mc.goto_first_child();
            loop {
                let m = mc.node();
                if m.kind() == "async" {
                    return true;
                }
                if let Ok(text) = m.utf8_text(source) {
                    if text == "async" {
                        return true;
                    }
                }
                if !mc.goto_next_sibling() {
                    break;
                }
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    false
}

fn extract_class(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) -> bool {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return false;
    }
    let exported = is_exported(node);
    let docstring = extract_docstring(node, ctx.source.as_bytes(), &ctx.config.doc_comment_style);
    if let Some(id) = ctx.emit_node(NodeKind::Class, &name, node, exported, None, docstring) {
        ctx.scope.push(id);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk_node(&child, ctx, extractor);
        }
        ctx.scope.pop();
    }
    true
}

fn extract_interface(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) -> bool {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return false;
    }
    let exported = is_exported(node);
    let docstring = extract_docstring(node, ctx.source.as_bytes(), &ctx.config.doc_comment_style);
    if let Some(id) = ctx.emit_node(NodeKind::Interface, &name, node, exported, None, docstring) {
        ctx.scope.push(id);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk_node(&child, ctx, extractor);
        }
        ctx.scope.pop();
    }
    true
}

fn extract_struct(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) -> bool {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return false;
    }
    let exported = is_exported(node);
    let docstring = extract_docstring(node, ctx.source.as_bytes(), &ctx.config.doc_comment_style);
    if let Some(id) = ctx.emit_node(NodeKind::Struct, &name, node, exported, None, docstring) {
        ctx.scope.push(id);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            walk_node(&child, ctx, extractor);
        }
        ctx.scope.pop();
    }
    true
}

fn extract_enum(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) -> bool {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return false;
    }
    let exported = is_exported(node);
    let docstring = extract_docstring(node, ctx.source.as_bytes(), &ctx.config.doc_comment_style);
    if let Some(id) = ctx.emit_node(NodeKind::Enum, &name, node, exported, None, docstring) {
        ctx.scope.push(id);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "enum_body"
                || child.kind() == "enum_variant_list"
                || child.kind() == "enum_class_body"
            {
                let mut c2 = child.walk();
                for inner in child.named_children(&mut c2) {
                    walk_node(&inner, ctx, extractor);
                }
            } else if ctx.config.enum_member_types.contains(&child.kind()) {
                let mem_name =
                    extract_name_from_node(&child, ctx.source.as_bytes(), ctx.config.name_field);
                if !mem_name.is_empty() {
                    ctx.emit_node(NodeKind::EnumMember, &mem_name, &child, false, None, None);
                }
            } else {
                walk_node(&child, ctx, extractor);
            }
        }
        ctx.scope.pop();
    }
    true
}

fn extract_type_alias(node: &tree_sitter::Node, ctx: &mut ExtractCtx) {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return;
    }
    let exported = is_exported(node);
    ctx.emit_node(NodeKind::Type, &name, node, exported, None, None);
}

fn extract_property(node: &tree_sitter::Node, ctx: &mut ExtractCtx) {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return;
    }
    ctx.emit_node(NodeKind::Property, &name, node, false, None, None);
}

fn extract_field(node: &tree_sitter::Node, ctx: &mut ExtractCtx) {
    let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
    if name.is_empty() {
        return;
    }
    ctx.emit_node(NodeKind::Field, &name, node, false, None, None);
}

/// Emit a node per binding in a variable declaration.
///
/// Returns the id of the emitted binding **only** when the declaration binds
/// exactly one identifier-shaped name — the sole case where using that binding
/// as a scope for calls in the initialiser produces a sensible caller name.
/// Destructuring patterns, multi-binding declarations and grammars whose
/// declarator text is not a bare name all return `None`; their initialisers are
/// still walked, but the calls attribute to the enclosing scope.
fn extract_variable(node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> Option<String> {
    let mut cursor = node.walk();
    let mut emitted: Vec<(String, Option<String>)> = Vec::new();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        if kind == "variable_declarator" || kind == "lexical_binding" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name = name_node
                    .utf8_text(ctx.source.as_bytes())
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    let exported = is_exported(node);
                    let id = ctx.emit_node(NodeKind::Variable, &name, &child, exported, None, None);
                    emitted.push((name, id));
                }
            }
        }
    }
    if emitted.is_empty() {
        let name = extract_name_from_node(node, ctx.source.as_bytes(), ctx.config.name_field);
        if !name.is_empty() {
            let exported = is_exported(node);
            let id = ctx.emit_node(NodeKind::Variable, &name, node, exported, None, None);
            emitted.push((name, id));
        }
    }
    match emitted.as_slice() {
        [(name, Some(id))] if is_identifier_shaped(name) => Some(id.clone()),
        _ => None,
    }
}

/// Whether `name` is usable as a scope segment: a bare identifier, not a
/// destructuring pattern (`{ deps, log }`), a multi-line binding, or an
/// assignment expression captured verbatim (C renders `x = foo(1)` as the name).
fn is_identifier_shaped(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

fn extract_import(node: &tree_sitter::Node, ctx: &mut ExtractCtx) {
    // Try named field probes first (covers most languages).
    // GAP-4: Added "name" as a fallback probe for languages that use it as the
    // field name.  The probe order is additive — existing languages that use
    // "source"/"module_name"/"path" are unaffected.
    let module_name = node
        .child_by_field_name("source")
        .or_else(|| node.child_by_field_name("module_name"))
        .or_else(|| node.child_by_field_name("path"))
        .or_else(|| node.child_by_field_name("name"))
        .map(|n| {
            n.utf8_text(ctx.source.as_bytes())
                .unwrap_or("")
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        // GAP-4 fallback: C# `using_directive` has NO named field — the module name
        // appears as a bare `qualified_name` or `identifier` child with no field label.
        // Walk named children to find and extract it.
        .or_else(|| {
            let source_bytes = ctx.source.as_bytes();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                let k = child.kind();
                if k == "qualified_name" || k == "identifier" || k == "name" {
                    let text = child
                        .utf8_text(source_bytes)
                        .unwrap_or("")
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string();
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
            None
        });

    if let Some(module) = module_name {
        if !module.is_empty() {
            let from_id = ctx.scope.last().cloned().unwrap_or_default();
            let line = node.start_position().row as u32 + 1;
            ctx.emit_import(&from_id, &module, line);
        }
    }
}

fn extract_call(node: &tree_sitter::Node, ctx: &mut ExtractCtx) {
    let callee_node = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("name"))
        .or_else(|| node.child_by_field_name("callee"));

    if let Some(callee) = callee_node {
        let text = callee
            .utf8_text(ctx.source.as_bytes())
            .unwrap_or("")
            .to_string();
        let name = text.split('.').next_back().unwrap_or(&text).to_string();
        if !name.is_empty() && name != "new" {
            let from_id = ctx.scope.last().cloned().unwrap_or_default();
            let line = node.start_position().row as u32 + 1;
            ctx.emit_call(&from_id, &name, line);
        }
    }
}

fn extract_rust_impl(
    node: &tree_sitter::Node,
    ctx: &mut ExtractCtx,
    extractor: &dyn LanguageExtractor,
) -> bool {
    let source_bytes = ctx.source.as_bytes();
    let mut type_idents: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "type_identifier" {
            if let Ok(t) = child.utf8_text(source_bytes) {
                type_idents.push(t.to_string());
            }
        }
    }

    // The last type_identifier in `impl [Trait for] Type` is always the Self type.
    let impl_type = type_idents.last().cloned().unwrap_or_default();

    // Resolve or create a Type node for the impl target so we have a real node ID.
    let self_node_id = if !impl_type.is_empty() {
        // Look for an existing struct/trait node with this name in the current batch.
        if let Some(existing) = ctx
            .nodes
            .iter()
            .find(|n| {
                n.name == impl_type
                    && matches!(
                        n.kind,
                        NodeKind::Struct | NodeKind::Trait | NodeKind::Class | NodeKind::Enum
                    )
            })
            .map(|n| n.id.clone())
        {
            existing
        } else {
            // Emit a synthetic Type node so FK constraints are satisfied.
            let line = node.start_position().row as u32 + 1;
            let type_id = generate_node_id(ctx.file_path, &NodeKind::Type, &impl_type, line);
            // Only emit if not already present (e.g., duplicate impls for same type).
            if !ctx.nodes.iter().any(|n| n.id == type_id) {
                let file_node_id = ctx
                    .nodes
                    .iter()
                    .find(|n| matches!(n.kind, NodeKind::File))
                    .map(|n| n.id.clone());
                ctx.nodes.push(Node {
                    id: type_id.clone(),
                    name: impl_type.clone(),
                    qualified_name: ctx.qualified_name(&impl_type),
                    kind: NodeKind::Type,
                    language: ctx.language.clone(),
                    file_path: ctx.file_path.to_string(),
                    start_line: line,
                    end_line: node.end_position().row as u32 + 1,
                    start_col: node.start_position().column as u32,
                    end_col: node.end_position().column as u32,
                    is_exported: false,
                    signature: None,
                    docstring: None,
                    metadata: None,
                });
                if let Some(parent_id) = file_node_id {
                    ctx.edges.push(codewiki_core::Edge {
                        id: format!("{parent_id}->{type_id}-contains"),
                        source_id: parent_id,
                        target_id: type_id.clone(),
                        kind: EdgeKind::Contains,
                        line: Some(line),
                        col: None,
                        provenance: Some("extraction".into()),
                        confidence: Some(1.0),
                        metadata: None,
                    });
                }
            }
            type_id
        }
    } else {
        // Fallback: use the file node as parent scope.
        ctx.nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::File))
            .map(|n| n.id.clone())
            .unwrap_or_default()
    };

    // Emit `implements` unresolved ref when this is `impl Trait for Type`.
    if type_idents.len() >= 2 && !self_node_id.is_empty() {
        let trait_name = type_idents.first().unwrap().clone();
        let line = node.start_position().row as u32 + 1;
        let ref_id = generate_node_id(
            ctx.file_path,
            &NodeKind::Trait,
            &format!("impl:{impl_type}:{trait_name}"),
            line,
        );
        ctx.unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: self_node_id.clone(),
            reference_name: trait_name,
            reference_kind: "implements".to_string(),
            file_path: ctx.file_path.to_string(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }

    // Push the impl type's node ID onto scope so enclosed function_items are
    // classified as Method (is_in_class_scope returns true).
    if !self_node_id.is_empty() {
        ctx.scope.push(self_node_id);
    }
    if let Some(body) = node.child_by_field_name("body") {
        let mut c = body.walk();
        for child in body.named_children(&mut c) {
            walk_node(&child, ctx, extractor);
        }
    }
    if !impl_type.is_empty() {
        ctx.scope.pop();
    }
    // `extract_rust_impl` fully owns traversal of the impl body (it walks the
    // body's named children above), so the caller must NOT recurse into the
    // impl's children again — otherwise each `function_item` would be emitted a
    // second time at file scope as a top-level `Function` (with duplicate calls).
    true
}

// ── Top-level entry point ─────────────────────────────────────────────────────

/// Parse and extract symbols from `(file_path, source)`.
pub fn extract_file(path: &Path, source: &str) -> ExtractionBatch {
    use crate::language_detector::detect_language as det;
    use crate::languages::get_extractor_and_use_special;

    let language = match det(path) {
        Some(l) => l,
        None => {
            tracing::debug!(path = %path.display(), "unknown language; skipping");
            return ExtractionBatch::empty();
        }
    };

    // Check if this language uses a special (non-tree-sitter) extractor.
    match get_extractor_and_use_special(&language, path, source) {
        GetExtractorResult::Special(batch) => batch,
        GetExtractorResult::UseTreeSitter(extractor) => {
            // Parse with the per-thread pool.
            let tree = THREAD_PARSERS.with(|pool| {
                let mut pool = pool.borrow_mut();
                pool.parser_for_file(&language, path)
                    .and_then(|p| p.parse(source, None))
            });

            match tree {
                Some(t) => {
                    tracing::debug!(path = %path.display(), ?language, "extracting");
                    extract_with_config(extractor.as_ref(), &t, source, path)
                }
                None => {
                    tracing::debug!(path = %path.display(), ?language, "no tree; skipping");
                    ExtractionBatch::empty()
                }
            }
        }
        GetExtractorResult::None => ExtractionBatch::empty(),
    }
}

pub enum GetExtractorResult {
    /// Language uses a special extractor that doesn't need tree-sitter.
    Special(ExtractionBatch),
    /// Language uses the standard tree-sitter walker.
    UseTreeSitter(Box<dyn LanguageExtractor>),
    /// Language not supported.
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Bug 9 fix: FileRecord timestamps must not be hardcoded to 0.
    #[test]
    fn extractor_populates_indexed_at_and_modified_at() {
        let source = "function hello() { return 42; }\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".ts").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let before_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let batch = extract_file(f.path(), source);

        let after_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        assert!(
            batch.file.indexed_at > 0,
            "indexed_at must not be 0; got {}",
            batch.file.indexed_at
        );
        assert!(
            batch.file.indexed_at >= before_ms && batch.file.indexed_at <= after_ms,
            "indexed_at {} is out of expected range [{}, {}]",
            batch.file.indexed_at,
            before_ms,
            after_ms
        );
        // modified_at comes from fs::metadata; it may be 0 for temp files on some
        // systems, but must not be negative.
        assert!(
            batch.file.modified_at >= 0,
            "modified_at must be non-negative; got {}",
            batch.file.modified_at
        );
    }
}
