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

/// Extract the C++ class name (`class_specifier`'s `name` field, which may be a
/// `type_identifier`, `qualified_identifier`, or `template_type`). Mirrors what
/// `extract_name_from_node` will compute so we can pre-derive the (deterministic)
/// node id of the class node that `extract_class` is about to emit.
fn class_name(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    if let Some(name_node) = node.child_by_field_name("name") {
        return name_node.utf8_text(source).ok().map(|s| s.to_string());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "type_identifier" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Extract the base-class name from a `base_class_clause` child node, handling
/// `type_identifier` and `qualified_identifier`.
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

impl LanguageExtractor for CppExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        let src = ctx.source.as_bytes();

        // ── function_definition (free functions AND in-class method bodies) ──
        //
        // The generic `extract_function`/`extract_method` path uses
        // `extract_name_from_node` with name_field="declarator", which yields the
        // function_declarator's *full text* (including the parameter list). We
        // unwrap to the bare identifier here instead, then recurse the body so
        // in-body `call_expression`s still produce `calls` edges.
        if node.kind() == "function_definition" {
            let name = match function_name(node, src) {
                Some(n) if !n.is_empty() => n,
                // Fall back to default dispatch (e.g. unusual declarator shapes).
                _ => return false,
            };

            let kind = if in_class_scope(ctx) {
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

        // ── class_specifier: emit `extends` refs for each base class ──
        //
        // We do NOT suppress default dispatch (return false) so `extract_class`
        // still emits the class node + recurses its body (methods/fields/etc.).
        // The class node id is deterministic, so we can pre-compute it here to
        // use as the `from_node_id` of the extends ref.
        if node.kind() == "class_specifier" || node.kind() == "struct_specifier" {
            if let Some(cname) = class_name(node, src) {
                if !cname.is_empty() {
                    let line = node.start_position().row as u32 + 1;
                    let class_kind = if node.kind() == "class_specifier" {
                        NodeKind::Class
                    } else {
                        NodeKind::Struct
                    };
                    let from_id = generate_node_id(ctx.file_path, &class_kind, &cname, line);

                    // base_class_clause is a (non-field) child of the specifier.
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        if child.kind() == "base_class_clause" {
                            let mut bc = child.walk();
                            for base in child.named_children(&mut bc) {
                                if let Some(bn) = base_name(&base, src) {
                                    if !bn.is_empty() {
                                        let bline = base.start_position().row as u32 + 1;
                                        let ref_id = generate_node_id(
                                            ctx.file_path,
                                            &NodeKind::Class,
                                            &format!("extends:{cname}:{bn}"),
                                            bline,
                                        );
                                        ctx.unresolved.push(UnresolvedRef {
                                            id: ref_id,
                                            from_node_id: from_id.clone(),
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
                    }
                }
            }
            // Let default class/struct extraction emit the node + recurse.
            return false;
        }

        false
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
}
