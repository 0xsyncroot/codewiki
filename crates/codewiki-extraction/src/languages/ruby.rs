//! T-215 — Ruby language extractor.

use crate::ast_walker::{DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor};

pub struct RubyExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["method"],
    class_types: &["class"],
    method_types: &["method", "singleton_method"],
    interface_types: &["module"],
    struct_types: &[],
    enum_types: &[],
    enum_member_types: &[],
    type_alias_types: &[],
    import_types: &[], // handled via visit_node_hook
    call_types: &["call", "method_call"],
    variable_types: &["assignment"],
    property_types: &[],
    field_types: &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingLineComment {
        node_kind: "comment",
        prefix: "#",
    },
};

impl LanguageExtractor for RubyExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        // Ruby `call` nodes use field "method" for the callee (not "function"/"name"/"callee").
        // Also handles require/require_relative as import edges.
        if node.kind() == "call" || node.kind() == "method_call" {
            let method_name = node
                .child_by_field_name("method")
                .and_then(|n| n.utf8_text(ctx.source.as_bytes()).ok())
                .unwrap_or("")
                .to_string();

            let from_id = ctx.scope.last().cloned().unwrap_or_default();
            let line = node.start_position().row as u32 + 1;
            let col = node.start_position().column as u32;

            // require / require_relative → import edge
            if method_name == "require" || method_name == "require_relative" {
                // The argument is the first positional argument node.
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    let kind = child.kind();
                    if kind == "argument_list" || kind == "args" {
                        let mut c2 = child.walk();
                        for arg in child.named_children(&mut c2) {
                            let arg_kind = arg.kind();
                            if arg_kind == "string" || arg_kind == "simple_string" {
                                let raw = arg.utf8_text(ctx.source.as_bytes()).unwrap_or("");
                                let module = raw.trim_matches(|c: char| c == '"' || c == '\'');
                                if !module.is_empty() {
                                    ctx.emit_import(&from_id, module, line);
                                }
                                break;
                            }
                        }
                        break;
                    }
                    // Some tree-sitter-ruby versions put the string directly as a child
                    let kind2 = child.kind();
                    if kind2 == "string" || kind2 == "simple_string" {
                        let raw = child.utf8_text(ctx.source.as_bytes()).unwrap_or("");
                        let module = raw.trim_matches(|c: char| c == '"' || c == '\'');
                        if !module.is_empty() {
                            ctx.emit_import(&from_id, module, line);
                        }
                        break;
                    }
                }
                return false; // don't suppress further walking
            }

            // Regular call → call edge
            if !method_name.is_empty() && method_name != "new" {
                ctx.emit_call(&from_id, &method_name, line, col);
            }
            return false; // don't suppress recursion
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn ruby_registered() {
        let source = "class Animal\n  def speak\n    'sound'\n  end\nend\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".rb").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(!batch.nodes.is_empty());
    }

    /// Bug 7 fix: Ruby calls must emit call edges (field is "method", not "function").
    #[test]
    fn ruby_calls_emit_edges() {
        let source = r#"
require 'json'
require_relative 'helper'

class Client
  def fetch
    puts "fetching"
    response = get_data
    response
  end
end
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".rb").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);

        // require should emit import edges
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
            modules.contains(&"json"),
            "json import missing; refs: {modules:?}"
        );
        assert!(
            modules.contains(&"helper"),
            "helper import missing; refs: {modules:?}"
        );

        // puts / get_data should emit call edges
        let call_refs: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "calls")
            .collect();
        let callees: Vec<&str> = call_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(
            callees.contains(&"puts"),
            "puts call missing; callees: {callees:?}"
        );
    }
}
