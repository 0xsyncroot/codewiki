//! T-214 — C++ language extractor.

use crate::ast_walker::{
    generate_node_id, walk_node, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::{NodeKind, UnresolvedRef};

pub struct CppExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_definition"],
    class_types: &["class_specifier"],
    method_types: &["function_definition"],
    interface_types: &[],
    struct_types: &["struct_specifier"],
    enum_types: &["enum_specifier"],
    enum_member_types: &["enumerator"],
    type_alias_types: &["type_definition", "alias_declaration"],
    import_types: &["preproc_include"],
    call_types: &["call_expression"],
    variable_types: &["declaration"],
    property_types: &[],
    field_types: &["field_declaration"],
    extra_class_types: &["namespace_definition"],
    namespace_types: &[],
    // C++ function/method names live inside the `declarator` field (a
    // `function_declarator`), not a `name` field. `class_specifier` keeps its
    // name in the `name` field, but `extract_name_from_node` falls back to the
    // first `type_identifier` child, so class extraction still works.
    name_field: "declarator",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingEither {
        block_node_kind: "comment",
        block_prefix: "/**",
        line_node_kind: "comment",
        line_prefix: "///",
    },
};

/// Whether the current scope is inside a class/struct (so a `function_definition`
/// should be classified as a Method rather than a free Function).
///
/// Replicates `ast_walker::is_in_class_scope` (which is private to the walker
/// module) so the hook can make the same Method-vs-Function decision without
/// touching the shared walker.
fn in_class_scope(ctx: &ExtractCtx) -> bool {
    for id in ctx.scope.iter().rev() {
        if let Some(n) = ctx.nodes.iter().find(|n| &n.id == id) {
            match n.kind {
                NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::Enum
                | NodeKind::Component => return true,
                NodeKind::Namespace => {}
                NodeKind::File => return false,
                _ => {}
            }
        }
    }
    false
}

/// Extract the bare identifier text from a C++ name node, handling the cases
/// that can appear as the innermost `declarator` of a `function_declarator`:
/// `identifier`, `field_identifier`, `qualified_identifier` (`Foo::bar`,
/// where we want the trailing `bar`), `destructor_name` (`~Foo`), and
/// `operator_name` (`operator+`).
fn name_from_decl_node(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => node.utf8_text(source).ok().map(|s| s.to_string()),
        "destructor_name" | "operator_name" => {
            // Use the full text (e.g. "~Foo", "operator+").
            node.utf8_text(source).ok().map(|s| s.to_string())
        }
        "qualified_identifier" => {
            // `A::B::bar` — take the trailing `name` segment (recursing through
            // nested qualified_identifiers).
            if let Some(name_node) = node.child_by_field_name("name") {
                name_from_decl_node(&name_node, source)
            } else {
                node.utf8_text(source).ok().map(|s| s.to_string())
            }
        }
        _ => None,
    }
}

/// Walk a `function_definition`'s `declarator` chain down to the
/// `function_declarator`, unwrapping pointer/reference declarators, and return
/// the bare function/method name.
fn function_name(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut decl = node.child_by_field_name("declarator")?;
    // Unwrap pointer_declarator / reference_declarator wrappers around the
    // function_declarator (e.g. `int* foo()` -> pointer_declarator).
    loop {
        match decl.kind() {
            "function_declarator" => break,
            "pointer_declarator" | "reference_declarator" => {
                decl = decl.child_by_field_name("declarator")?;
            }
            _ => return None,
        }
    }
    // function_declarator.declarator holds the name node.
    let name_node = decl.child_by_field_name("declarator")?;
    name_from_decl_node(&name_node, source)
}

/// Resolve a class/struct *name node* to a bare, usable identifier.
///
/// In tree-sitter-cpp a `class_specifier`/`struct_specifier`'s `name` field may
/// be one of:
///   - `type_identifier` — `Foo`
///   - `qualified_identifier` — `ns::Foo` (we want the trailing `Foo`)
///   - `template_type` — `A<int>` (a template specialization; we want the
///     underlying template name `A`, never the `<...>` argument list — otherwise
///     `extract_name_from_node` cannot recover a name and the struct node is
///     dropped, orphaning its `extends` edge).
fn name_from_class_name_node(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" => node.utf8_text(source).ok().map(|s| s.to_string()),
        "qualified_identifier" => node
            .child_by_field_name("name")
            .and_then(|n| name_from_class_name_node(&n, source))
            .or_else(|| node.utf8_text(source).ok().map(|s| s.to_string())),
        "template_type" => node
            .child_by_field_name("name")
            .and_then(|n| name_from_class_name_node(&n, source)),
        _ => None,
    }
}

/// Extract the C++ class/struct name from a `class_specifier`/`struct_specifier`.
///
/// Reads the `name` field (a `type_identifier`, `qualified_identifier`, or
/// `template_type`) and resolves it to a bare identifier. This is the single
/// source of truth for the class node's name AND the `from_node_id` of its
/// `extends` refs, so the two can never disagree (which would orphan the edge).
fn class_name(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    if let Some(name_node) = node.child_by_field_name("name") {
        if let Some(n) = name_from_class_name_node(&name_node, source) {
            if !n.is_empty() {
                return Some(n);
            }
        }
    }
    // Fallback: first type_identifier among direct children.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "type_identifier" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Extract the base-class name from a `base_class_clause` child node, handling
/// `type_identifier`, `qualified_identifier`, and `template_type`.
fn base_name(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => node.utf8_text(source).ok().map(|s| s.to_string()),
        "qualified_identifier" => node.utf8_text(source).ok().map(|s| s.to_string()),
        "template_type" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Emit `extends` refs for each base class of a class/struct specifier, parented
/// to the node id that was actually emitted for the owning class/struct.
///
/// Crucially this is called with the *real* emitted node id (not a precomputed
/// guess), so if the class node was never emitted no heritage edge is produced —
/// the edge can never be orphaned.
fn emit_heritage(node: &tree_sitter::Node, ctx: &mut ExtractCtx, from_id: &str, cname: &str) {
    let src = ctx.source.as_bytes();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "base_class_clause" {
            continue;
        }
        let mut bc = child.walk();
        for base in child.named_children(&mut bc) {
            if let Some(bn) = base_name(&base, src) {
                if bn.is_empty() {
                    continue;
                }
                let bline = base.start_position().row as u32 + 1;
                let ref_id = generate_node_id(
                    ctx.file_path,
                    &NodeKind::Class,
                    &format!("extends:{cname}:{bn}"),
                    bline,
                );
                ctx.unresolved.push(UnresolvedRef {
                    id: ref_id,
                    from_node_id: from_id.to_string(),
                    reference_name: bn,
                    reference_kind: "extends".to_string(),
                    file_path: ctx.file_path.to_string(),
                    line: Some(bline),
                    col: None,
                    metadata: None,
                });
            }
        }
    }
}

/// Detect a macro-prefixed class that tree-sitter mis-parsed as a
/// `function_definition`.
///
/// `class SPDLOG_API base_sink : public sink { ... };` (where `SPDLOG_API` is an
/// unknown export macro) does NOT parse as a `class_specifier`. Instead it
/// becomes a `function_definition` whose:
///   - `type` field is a *body-less* `class_specifier`/`struct_specifier` whose
///     own `name` field is the macro (`SPDLOG_API`),
///   - `declarator` field is a bare `identifier` (the REAL class name,
///     `base_sink`),
///   - `body` field is the class body (`compound_statement`),
///   - optional `ERROR` sibling holds the dropped `: public sink` base clause.
///
/// Returns `(real_name, inner_class_specifier, is_struct)` when this shape is
/// detected, else `None`.
fn detect_macro_class<'t>(
    node: &tree_sitter::Node<'t>,
    source: &[u8],
) -> Option<(String, tree_sitter::Node<'t>, bool)> {
    let type_node = node.child_by_field_name("type")?;
    let is_struct = match type_node.kind() {
        "class_specifier" => false,
        "struct_specifier" => true,
        _ => return None,
    };
    // A real class definition has a body; the mis-parsed stub does not.
    if type_node.child_by_field_name("body").is_some() {
        return None;
    }
    // The body of the *function_definition* must be a compound_statement (the
    // class body), and the declarator must be a bare identifier (the class name).
    let decl = node.child_by_field_name("declarator")?;
    if decl.kind() != "identifier" {
        return None;
    }
    let real_name = decl.utf8_text(source).ok()?.to_string();
    if real_name.is_empty() {
        return None;
    }
    Some((real_name, type_node, is_struct))
}

/// Extract base-class names from the `ERROR` recovery node that tree-sitter
/// produces for a mis-parsed macro class's base clause (`: public sink`).
/// Any `identifier`/`type_identifier`/`qualified_identifier` inside is taken as
/// a base name (access specifiers `public`/`private`/`protected` are unnamed
/// keyword tokens and are skipped).
fn macro_base_names(node: &tree_sitter::Node, source: &[u8]) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "ERROR" {
            let mut ec = child.walk();
            for e in child.named_children(&mut ec) {
                let text = match e.kind() {
                    "identifier" | "type_identifier" | "qualified_identifier" => {
                        e.utf8_text(source).ok().map(|s| s.to_string())
                    }
                    "template_type" => e
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string()),
                    _ => None,
                };
                // `final`/`override` can leak into the ERROR node as identifiers;
                // skip those — they are not base classes.
                if let Some(t) = text {
                    if !t.is_empty() && t != "final" && t != "override" {
                        out.push((t, e.start_position().row as u32 + 1));
                    }
                }
            }
        }
    }
    out
}

impl LanguageExtractor for CppExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        let src = ctx.source.as_bytes();

        // ── function_definition ──────────────────────────────────────────────
        if node.kind() == "function_definition" {
            // (D2) Macro-prefixed class mis-parsed as a function_definition:
            //   `class SPDLOG_API base_sink : public sink { ... };`
            // Re-interpret it as the class/struct it really is. The class name is
            // the declarator identifier (`base_sink`), NOT the macro that landed
            // in the inner class_specifier's `name` field (`SPDLOG_API`).
            if let Some((cname, _spec, is_struct)) = detect_macro_class(node, src) {
                let class_kind = if is_struct {
                    NodeKind::Struct
                } else {
                    NodeKind::Class
                };
                let docstring =
                    crate::ast_walker::extract_docstring(node, src, &CONFIG.doc_comment_style);
                if let Some(id) = ctx.emit_node(class_kind, &cname, node, false, None, docstring) {
                    // Base classes leaked into a sibling ERROR node.
                    for (bn, bline) in macro_base_names(node, src) {
                        let ref_id = generate_node_id(
                            ctx.file_path,
                            &NodeKind::Class,
                            &format!("extends:{cname}:{bn}"),
                            bline,
                        );
                        ctx.unresolved.push(UnresolvedRef {
                            id: ref_id,
                            from_node_id: id.clone(),
                            reference_name: bn,
                            reference_kind: "extends".to_string(),
                            file_path: ctx.file_path.to_string(),
                            line: Some(bline),
                            col: None,
                            metadata: None,
                        });
                    }
                    // Recurse the class body (a compound_statement) so members
                    // and in-body calls are extracted within the class scope.
                    ctx.scope.push(id);
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut cursor = body.walk();
                        for child in body.named_children(&mut cursor) {
                            walk_node(&child, ctx, self);
                        }
                    }
                    ctx.scope.pop();
                }
                return true;
            }

            // (D3 + normal) Free functions, in-class method bodies, and out-of-line
            // method definitions.
            //
            // The generic path uses `extract_name_from_node` with
            // name_field="declarator", which yields the function_declarator's
            // *full text* (with the parameter list). We unwrap to the bare name
            // here, then recurse the body so in-body `call_expression`s still
            // produce `calls` edges.
            let name = match function_name(node, src) {
                Some(n) if !n.is_empty() => n,
                // Fall back to default dispatch (e.g. unusual declarator shapes).
                _ => return false,
            };

            // (D3) An out-of-line definition `void logger::foo() {}` has a
            // `qualified_identifier` declarator (`logger::foo`). Classify it as a
            // Method even at file scope (the qualifier names the owning type).
            let kind = if in_class_scope(ctx) || has_qualified_declarator(node) {
                NodeKind::Method
            } else {
                NodeKind::Function
            };

            if let Some(id) = ctx.emit_node(kind, &name, node, false, None, None) {
                ctx.scope.push(id);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        walk_node(&child, ctx, self);
                    }
                }
                ctx.scope.pop();
            }
            return true;
        }

        // ── class_specifier / struct_specifier ───────────────────────────────
        //
        // We OWN class/struct extraction (rather than deferring to the default
        // dispatch) so the emitted node name and the `from_node_id` of its
        // `extends` refs are derived from the SAME `class_name()` call. A heritage
        // edge is only emitted when the class node was actually emitted, so an
        // edge can never be orphaned (root-cause fix for the FK crash).
        if node.kind() == "class_specifier" || node.kind() == "struct_specifier" {
            // Only true *definitions* (which carry a body) are real symbols.
            // Skip:
            //   - forward declarations: `class Foo;` (no body),
            //   - explicit template instantiations:
            //     `template class SPDLOG_API spdlog::sinks::base_sink<...>;`
            //     — these parse as a body-less `class_specifier` whose `name`
            //     field is the export MACRO (`SPDLOG_API`), so without this guard
            //     they would leak bogus `SPDLOG_API` class nodes.
            if node.child_by_field_name("body").is_none() {
                return true;
            }
            let cname = match class_name(node, src) {
                Some(n) if !n.is_empty() => n,
                // Anonymous class/struct (e.g. `struct { ... } x;`): let the
                // default dispatch handle/skip it; it emits nothing, so emitting
                // no heritage ref here is correct.
                _ => return false,
            };
            let class_kind = if node.kind() == "class_specifier" {
                NodeKind::Class
            } else {
                NodeKind::Struct
            };
            let exported = crate::ast_walker::is_exported(node);
            let docstring =
                crate::ast_walker::extract_docstring(node, src, &CONFIG.doc_comment_style);
            if let Some(id) = ctx.emit_node(class_kind, &cname, node, exported, None, docstring) {
                emit_heritage(node, ctx, &id, &cname);
                ctx.scope.push(id);
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    walk_node(&child, ctx, self);
                }
                ctx.scope.pop();
            }
            return true;
        }

        false
    }
}

/// Whether a `function_definition`'s declarator (after unwrapping pointer/
/// reference wrappers) names a `qualified_identifier` such as `logger::foo` —
/// i.e. an out-of-line member definition.
fn has_qualified_declarator(node: &tree_sitter::Node) -> bool {
    let Some(mut decl) = node.child_by_field_name("declarator") else {
        return false;
    };
    loop {
        match decl.kind() {
            "function_declarator" => {
                return decl
                    .child_by_field_name("declarator")
                    .map(|d| d.kind() == "qualified_identifier")
                    .unwrap_or(false);
            }
            "pointer_declarator" | "reference_declarator" => {
                match decl.child_by_field_name("declarator") {
                    Some(d) => decl = d,
                    None => return false,
                }
            }
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use codewiki_core::NodeKind;
    use std::io::Write;

    fn extract(src: &str) -> codewiki_core::ExtractionBatch {
        let mut f = tempfile::NamedTempFile::with_suffix(".cpp").unwrap();
        f.write_all(src.as_bytes()).unwrap();
        extract_file(f.path(), src)
    }

    #[test]
    fn extract_cpp_class() {
        let source = r#"
class Calculator {
public:
    int add(int a, int b) { return a + b; }
};
"#;
        let batch = extract(source);
        assert!(
            batch.nodes.iter().any(|n| n.name == "Calculator"),
            "nodes: {:?}",
            batch
                .nodes
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// C-CPP-1/2/3 + D3: methods, free functions, extends and in-body calls.
    #[test]
    fn cpp_methods_free_fn_extends_and_calls() {
        let source = r#"
class Base {
public:
    virtual int value() const { return compute(); }
    int compute() const { return 42; }
};

class Derived : public Base {
public:
    int value() const override { return 7; }
};

int makeIt() {
    Derived d;
    return d.value();
}
"#;
        let batch = extract(source);
        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();

        // Class nodes
        assert!(batch
            .nodes
            .iter()
            .any(|n| n.name == "Base" && matches!(n.kind, NodeKind::Class)));
        assert!(batch
            .nodes
            .iter()
            .any(|n| n.name == "Derived" && matches!(n.kind, NodeKind::Class)));

        // Methods (value ×2, compute) — bare names, no parameter lists.
        let methods: Vec<&str> = batch
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Method))
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            methods.iter().filter(|&&n| n == "value").count() == 2,
            "expected two `value` methods; methods: {methods:?}"
        );
        assert!(methods.contains(&"compute"), "methods: {methods:?}");

        // Free function makeIt
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "makeIt" && matches!(n.kind, NodeKind::Function)),
            "makeIt missing; names: {names:?}"
        );

        // No name should include a parameter list.
        for n in &batch.nodes {
            assert!(!n.name.contains('('), "name includes params: {:?}", n.name);
        }

        // extends ref: Derived -> Base
        let ext: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "extends")
            .collect();
        assert_eq!(ext.len(), 1, "expected one extends ref: {ext:?}");
        assert_eq!(ext[0].reference_name, "Base");

        // calls ref: value() -> compute()
        let calls: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "calls")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(
            calls.contains(&"compute"),
            "expected a call to compute; calls: {calls:?}"
        );
    }

    /// Helper: every `extends`/`calls`/heritage ref must point at a node that was
    /// actually emitted (no orphan edges → no FK violation on persist).
    fn assert_no_orphan_refs(batch: &codewiki_core::ExtractionBatch) {
        use std::collections::HashSet;
        let ids: HashSet<&str> = batch.nodes.iter().map(|n| n.id.as_str()).collect();
        for r in &batch.unresolved_refs {
            assert!(
                ids.contains(r.from_node_id.as_str()),
                "orphan ref ({}) from missing node {}: refs={:?} node ids={:?}",
                r.reference_kind,
                r.from_node_id,
                batch
                    .unresolved_refs
                    .iter()
                    .map(|x| (&x.reference_kind, &x.from_node_id))
                    .collect::<Vec<_>>(),
                ids
            );
        }
    }

    /// D1 (P0): template specialization with a base class must NOT crash / orphan
    /// an `extends` edge. The struct node is emitted under the template name `A`.
    #[test]
    fn cpp_template_specialization_with_base() {
        let source = r#"
struct B { int x; };
template <> struct A<int> : B { void f(){} };
"#;
        let batch = extract(source);
        assert_no_orphan_refs(&batch);

        // The specialization is emitted as a struct named `A` (not `A<int>`).
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "A" && matches!(n.kind, NodeKind::Struct)),
            "expected struct named A; nodes: {:?}",
            batch
                .nodes
                .iter()
                .map(|n| (n.name.as_str(), &n.kind))
                .collect::<Vec<_>>()
        );
        // extends A -> B is present and not orphaned.
        let ext: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "extends")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(ext.contains(&"B"), "expected extends -> B; got {ext:?}");
        // Method f extracted.
        assert!(batch
            .nodes
            .iter()
            .any(|n| n.name == "f" && matches!(n.kind, NodeKind::Method)));
    }

    /// D1 variant: a regular templated class (`template<T> class Holder : Base<T>`)
    /// keeps name `Holder` and emits a non-orphan extends to `Base`.
    #[test]
    fn cpp_templated_class_with_template_base() {
        let source = r#"
template <typename T> class Holder : public Base<T> { T val; };
"#;
        let batch = extract(source);
        assert_no_orphan_refs(&batch);
        assert!(batch
            .nodes
            .iter()
            .any(|n| n.name == "Holder" && matches!(n.kind, NodeKind::Class)));
        let ext: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "extends")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(ext.contains(&"Base"), "got {ext:?}");
    }

    /// D2 (P1): an export macro before the class name must not become the class
    /// name; the real name and base class must be recovered.
    #[test]
    fn cpp_export_macro_class() {
        let source = r#"
class SPDLOG_API base_sink : public sink {
public:
    void log();
};
"#;
        let batch = extract(source);
        assert_no_orphan_refs(&batch);
        // Real class name recovered; macro is NOT a node name.
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "base_sink" && matches!(n.kind, NodeKind::Class)),
            "expected class base_sink; nodes: {:?}",
            batch
                .nodes
                .iter()
                .map(|n| (n.name.as_str(), &n.kind))
                .collect::<Vec<_>>()
        );
        assert!(
            !batch.nodes.iter().any(|n| n.name == "SPDLOG_API"),
            "SPDLOG_API leaked as a node name"
        );
        // Base class detected.
        let ext: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "extends")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(
            ext.contains(&"sink"),
            "expected extends -> sink; got {ext:?}"
        );
    }

    /// D2 variant: macro class with `final` and a template base, wrapped in a
    /// `template<...>` (the real spdlog `base_sink` shape).
    #[test]
    fn cpp_export_macro_templated_class() {
        let source = r#"
template <typename Mutex>
class SPDLOG_API base_sink : public sink {
public:
    void log();
};
"#;
        let batch = extract(source);
        assert_no_orphan_refs(&batch);
        assert!(batch
            .nodes
            .iter()
            .any(|n| n.name == "base_sink" && matches!(n.kind, NodeKind::Class)));
        assert!(!batch.nodes.iter().any(|n| n.name == "SPDLOG_API"));
    }

    /// D3 (P1): an out-of-line method definition `void logger::foo() {}` in a .cpp
    /// must be classified as a Method (not a free Function), and still extract
    /// its in-body calls.
    #[test]
    fn cpp_out_of_line_method() {
        let source = r#"
void logger::foo() {
    helper();
}
int freeFn() { return 0; }
"#;
        let batch = extract(source);
        assert_no_orphan_refs(&batch);
        // foo is a Method (qualified declarator).
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "foo" && matches!(n.kind, NodeKind::Method)),
            "expected foo as Method; nodes: {:?}",
            batch
                .nodes
                .iter()
                .map(|n| (n.name.as_str(), &n.kind))
                .collect::<Vec<_>>()
        );
        // freeFn stays a Function.
        assert!(batch
            .nodes
            .iter()
            .any(|n| n.name == "freeFn" && matches!(n.kind, NodeKind::Function)));
        // In-body call preserved.
        let calls: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "calls")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(calls.contains(&"helper"), "calls: {calls:?}");
    }
}
