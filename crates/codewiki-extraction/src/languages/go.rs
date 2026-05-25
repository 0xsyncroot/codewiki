//! T-211 — Go language extractor.

use crate::ast_walker::{
    walk_node, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::NodeKind;

pub struct GoExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_declaration"],
    class_types: &[],
    // Go: method_declaration is top-level (has a receiver).
    method_types: &["method_declaration"],
    interface_types: &[],
    struct_types: &[],
    enum_types: &[],
    enum_member_types: &[],
    type_alias_types: &["type_spec"],
    import_types: &["import_declaration", "import_spec"],
    call_types: &["call_expression"],
    variable_types: &[
        "var_declaration",
        "short_var_declaration",
        "const_declaration",
    ],
    property_types: &[],
    field_types: &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: true,
    doc_comment_style: DocCommentStyle::PrecedingLineComment {
        node_kind: "comment",
        prefix: "//",
    },
};

impl GoExtractor {
    /// Resolve a `type_spec` to the correct node kind.
    fn resolve_type_spec_kind(node: &tree_sitter::Node) -> NodeKind {
        if let Some(type_child) = node.child_by_field_name("type") {
            match type_child.kind() {
                "struct_type" => return NodeKind::Struct,
                "interface_type" => return NodeKind::Interface,
                _ => {}
            }
        }
        NodeKind::Type
    }
}

impl LanguageExtractor for GoExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        // Override type_spec handling to emit struct/interface instead of type_alias.
        if node.kind() == "type_spec" {
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(ctx.source.as_bytes()).ok())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                let kind = GoExtractor::resolve_type_spec_kind(node);
                let exported = name
                    .chars()
                    .next()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false);
                if let Some(id) = ctx.emit_node(kind.clone(), &name, node, exported, None, None) {
                    if matches!(kind, NodeKind::Struct | NodeKind::Interface) {
                        ctx.scope.push(id);
                        let mut cursor = node.walk();
                        for child in node.named_children(&mut cursor) {
                            walk_node(&child, ctx, self);
                        }
                        ctx.scope.pop();
                    }
                }
            }
            return true;
        }

        // Override method_declaration to extract receiver type for qualified name.
        if node.kind() == "method_declaration" {
            let receiver_type = node.child_by_field_name("receiver").and_then(|recv| {
                // receiver is a parameter_list; look inside for the type name.
                let text = recv.utf8_text(ctx.source.as_bytes()).ok()?.to_string();
                // Extract type from "(sl *Type)" or "(sl Type)"
                let m = regex::Regex::new(r"\*?\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)").ok()?;
                let cap = m.captures(&text)?;
                Some(cap[1].to_string())
            });

            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(ctx.source.as_bytes()).ok())
                .unwrap_or("")
                .to_string();

            if !name.is_empty() {
                let qualified = if let Some(recv) = &receiver_type {
                    format!("{recv}::{name}")
                } else {
                    name.clone()
                };
                let exported = name
                    .chars()
                    .next()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false);
                let id = crate::ast_walker::generate_node_id(
                    ctx.file_path,
                    &NodeKind::Method,
                    &qualified,
                    node.start_position().row as u32 + 1,
                );
                let node_obj = codewiki_core::Node {
                    id: id.clone(),
                    name,
                    qualified_name: qualified,
                    kind: NodeKind::Method,
                    language: ctx.language.clone(),
                    file_path: ctx.file_path.to_string(),
                    start_line: node.start_position().row as u32 + 1,
                    end_line: node.end_position().row as u32 + 1,
                    start_col: node.start_position().column as u32,
                    end_col: node.end_position().column as u32,
                    is_exported: exported,
                    signature: None,
                    docstring: None,
                    metadata: None,
                };
                // Contains edge from current scope.
                if let Some(parent_id) = ctx.scope.last() {
                    ctx.edges.push(codewiki_core::Edge {
                        id: format!("{parent_id}->{id}-contains"),
                        source_id: parent_id.clone(),
                        target_id: id.clone(),
                        kind: codewiki_core::EdgeKind::Contains,
                        line: Some(node.start_position().row as u32 + 1),
                        col: None,
                        provenance: Some("extraction".into()),
                        confidence: Some(1.0),
                        metadata: None,
                    });
                }
                ctx.nodes.push(node_obj);
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
    fn extract_go_function_and_struct() {
        let source = r#"
package main

type User struct {
    Name string
}

func NewUser(name string) *User {
    return &User{Name: name}
}

func (u *User) Greet() string {
    return "Hello " + u.Name
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".go").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let names: Vec<_> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            batch.nodes.iter().any(|n| n.name == "User"),
            "nodes: {names:?}"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "NewUser"),
            "nodes: {names:?}"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "Greet"),
            "nodes: {names:?}"
        );
    }

    /// Bug 3 fix: Go imports (both single and grouped) must produce unresolved import refs.
    #[test]
    fn go_imports_emit_edges() {
        let source = r#"
package main

import (
    "fmt"
    "net/http"
)

import "os"

func main() {}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".go").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);

        let import_refs: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports")
            .collect();

        let modules: Vec<&str> = import_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(
            modules.contains(&"fmt"),
            "fmt import missing; refs: {modules:?}"
        );
        assert!(
            modules.contains(&"net/http"),
            "net/http import missing; refs: {modules:?}"
        );
        assert!(
            modules.contains(&"os"),
            "os import missing; refs: {modules:?}"
        );
    }
}
