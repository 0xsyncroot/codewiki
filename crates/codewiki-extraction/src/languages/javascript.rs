//! T-209 — JavaScript + JSX language extractor (shares TypeScript config).

use crate::ast_walker::{DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor};

pub struct JavaScriptExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &[
        "function_declaration",
        "arrow_function",
        "function_expression",
        "generator_function_declaration",
    ],
    class_types: &["class_declaration"],
    method_types: &["method_definition", "public_field_definition"],
    interface_types: &[],
    struct_types: &[],
    enum_types: &[],
    enum_member_types: &[],
    type_alias_types: &[],
    import_types: &["import_statement"],
    call_types: &["call_expression"],
    variable_types: &["lexical_declaration", "variable_declaration"],
    property_types: &[],
    field_types: &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingBlockComment {
        node_kind: "comment",
        prefix: "/**",
    },
};

/// Recursively scan a subtree for `require('module')` calls and emit import edges.
fn scan_require_calls(node: &tree_sitter::Node, ctx: &mut ExtractCtx) {
    if node.kind() == "call_expression" {
        if let Some(func) = node.child_by_field_name("function") {
            let func_text = func.utf8_text(ctx.source.as_bytes()).unwrap_or("");
            if func_text == "require" {
                if let Some(args) = node.child_by_field_name("arguments") {
                    let mut cursor = args.walk();
                    for arg in args.named_children(&mut cursor) {
                        let kind = arg.kind();
                        if kind == "string" || kind == "template_string" {
                            // Use string_fragment child (no quotes) if available.
                            let mut c2 = arg.walk();
                            let frag: Option<String> = arg
                                .named_children(&mut c2)
                                .find(|c| {
                                    c.kind() == "string_fragment"
                                        || c.kind() == "template_chars"
                                })
                                .and_then(|f| {
                                    f.utf8_text(ctx.source.as_bytes())
                                        .ok()
                                        .map(|s| s.to_string())
                                });
                            drop(c2);
                            let module = frag.unwrap_or_else(|| {
                                arg.utf8_text(ctx.source.as_bytes())
                                    .unwrap_or("")
                                    .trim_matches(|c: char| {
                                        c == '"' || c == '\'' || c == '`'
                                    })
                                    .to_string()
                            });
                            if !module.is_empty() {
                                let from_id =
                                    ctx.scope.last().cloned().unwrap_or_default();
                                let line = node.start_position().row as u32 + 1;
                                ctx.emit_import(&from_id, &module, line);
                            }
                            break;
                        }
                    }
                }
                return;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        scan_require_calls(&child, ctx);
    }
}

impl LanguageExtractor for JavaScriptExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        // Detect CommonJS require('module') calls embedded inside variable declarations.
        // `lexical_declaration` and `variable_declaration` set skip_children=true in the
        // default walker, so we must scan for require() calls here before they're skipped.
        if node.kind() == "lexical_declaration" || node.kind() == "variable_declaration" {
            scan_require_calls(node, ctx);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn extract_js_function() {
        let source = "function hello() { return 42; }\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".js").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(batch.nodes.iter().any(|n| n.name == "hello"));
    }

    /// Bug 4 fix: CommonJS require('module') must produce an import edge.
    #[test]
    fn js_commonjs_require_emits_import_edge() {
        let source = r#"
const fs = require('fs');
const path = require("path");
function doStuff() {}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".js").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);

        let import_refs: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports")
            .collect();
        let modules: Vec<&str> =
            import_refs.iter().map(|r| r.reference_name.as_str()).collect();

        assert!(
            modules.contains(&"fs"),
            "fs import missing; refs: {modules:?}"
        );
        assert!(
            modules.contains(&"path"),
            "path import missing; refs: {modules:?}"
        );
    }
}
