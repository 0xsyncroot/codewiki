//! T-214 — C language extractor.

use crate::ast_walker::{
    walk_node, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::NodeKind;

pub struct CExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_definition"],
    class_types: &[],
    method_types: &[],
    interface_types: &[],
    struct_types: &["struct_specifier"],
    enum_types: &["enum_specifier"],
    enum_member_types: &["enumerator"],
    type_alias_types: &["type_definition"],
    import_types: &["preproc_include"],
    call_types: &["call_expression"],
    variable_types: &["declaration"],
    property_types: &[],
    field_types: &["field_declaration"],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "declarator",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingBlockComment {
        node_kind: "comment",
        prefix: "/**",
    },
};

impl LanguageExtractor for CExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        // C function_definition: the `declarator` field is a `function_declarator`
        // node whose text includes the parameter list (e.g. "add(int a, int b)").
        // Extract only the bare `identifier` from within the function_declarator.
        if node.kind() == "function_definition" {
            let name = node
                .child_by_field_name("declarator")
                .and_then(|decl| {
                    // function_declarator has a `declarator` field with the name
                    if decl.kind() == "function_declarator" {
                        decl.child_by_field_name("declarator")
                            .and_then(|n| n.utf8_text(ctx.source.as_bytes()).ok())
                            .map(|s| s.to_string())
                    } else {
                        // pointer_declarator or direct identifier
                        let mut cursor = decl.walk();
                        let found = decl
                            .named_children(&mut cursor)
                            .find(|c| c.kind() == "identifier")
                            .and_then(|n| n.utf8_text(ctx.source.as_bytes()).ok())
                            .map(|s| s.to_string());
                        // Drop cursor before the or_else closure borrows `decl` again.
                        drop(cursor);
                        found.or_else(|| {
                            decl.utf8_text(ctx.source.as_bytes())
                                .ok()
                                .map(|s| s.to_string())
                        })
                    }
                })
                .unwrap_or_default();

            if name.is_empty() {
                return false; // let default handle it
            }

            if let Some(id) = ctx.emit_node(NodeKind::Function, &name, node, false, None, None) {
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
        false
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn extract_c_function() {
        let source = "int add(int a, int b) { return a + b; }\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".c").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        // File node + function
        assert!(!batch.nodes.is_empty(), "expected at least a file node");
    }

    /// Bug 6 fix: C function names must not include the parameter list.
    #[test]
    fn c_function_name_excludes_parameter_list() {
        let source = r#"
#include <stdio.h>

int add(int a, int b) { return a + b; }

double distance(double x, double y) { return x + y; }

void greet(void) { printf("hi"); }
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".c").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();

        // Function names must be bare identifiers
        assert!(
            batch.nodes.iter().any(|n| n.name == "add"),
            "add missing; names: {names:?}"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "distance"),
            "distance missing; names: {names:?}"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "greet"),
            "greet missing; names: {names:?}"
        );

        // No node must have a name that includes '('
        for node in &batch.nodes {
            assert!(
                !node.name.contains('('),
                "function name includes parameter list: {:?}",
                node.name
            );
        }
    }
}
