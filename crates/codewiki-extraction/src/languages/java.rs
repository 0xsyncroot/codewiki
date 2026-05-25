//! T-213 — Java language extractor.

use crate::ast_walker::{DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor};
use codewiki_core::NodeKind;

pub struct JavaExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &[],
    class_types: &["class_declaration", "interface_declaration", "enum_declaration"],
    method_types: &["method_declaration", "constructor_declaration"],
    interface_types: &[],
    struct_types: &[],
    enum_types: &[],
    enum_member_types: &["enum_constant"],
    type_alias_types: &[],
    import_types: &["import_declaration"],
    call_types: &["method_invocation", "object_creation_expression"],
    variable_types: &[],
    property_types: &[],
    field_types: &["field_declaration"],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingBlockComment {
        node_kind: "block_comment",
        prefix: "/**",
    },
};

impl LanguageExtractor for JavaExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        // Java import_declaration: the qualified class name is the full node text
        // (no named field). Strip the `import ` prefix and trailing `;`.
        if node.kind() == "import_declaration" {
            let raw = node
                .utf8_text(ctx.source.as_bytes())
                .unwrap_or("")
                .trim();
            let module = raw
                .trim_start_matches("import ")
                .trim_start_matches("static ")
                .trim_end_matches(';')
                .trim();
            if !module.is_empty() {
                let from_id = ctx.scope.last().cloned().unwrap_or_default();
                let line = node.start_position().row as u32 + 1;
                ctx.emit_import(&from_id, module, line);
            }
            return true; // suppress default import handling
        }

        // Java field_declaration: the variable name lives inside variable_declarator.name,
        // not in the field_declaration itself (which has the type first).
        if node.kind() == "field_declaration" {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = name_node
                            .utf8_text(ctx.source.as_bytes())
                            .unwrap_or("")
                            .to_string();
                        if !name.is_empty() {
                            ctx.emit_node(NodeKind::Field, &name, &child, false, None, None);
                        }
                    }
                }
            }
            return true; // suppress default field handling
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use codewiki_core::NodeKind;
    use std::io::Write;

    #[test]
    fn extract_java_class_and_method() {
        let source = r#"
public class HelloWorld {
    public static void main(String[] args) {
        System.out.println("Hello");
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".java").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(batch.nodes.iter().any(|n| n.name == "HelloWorld"));
        assert!(batch.nodes.iter().any(|n| n.name == "main"));
    }

    /// Bug 5 fix: Java import_declaration and field variable name extraction.
    #[test]
    fn java_imports_emit_edges_and_field_uses_variable_name() {
        let source = r#"
import java.util.List;
import java.io.IOException;

public class Animal {
    private String name;
    private int age;

    public Animal(String name) {
        this.name = name;
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".java").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);

        // Import edges
        let import_refs: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports")
            .collect();
        let modules: Vec<&str> =
            import_refs.iter().map(|r| r.reference_name.as_str()).collect();
        assert!(
            modules.contains(&"java.util.List"),
            "java.util.List import missing; refs: {modules:?}"
        );
        assert!(
            modules.contains(&"java.io.IOException"),
            "java.io.IOException import missing; refs: {modules:?}"
        );

        // Field names must be variable names, not type names
        let fields: Vec<_> = batch
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Field)
            .collect();
        let field_names: Vec<&str> = fields.iter().map(|n| n.name.as_str()).collect();
        assert!(
            field_names.contains(&"name"),
            "field 'name' missing; fields: {field_names:?}"
        );
        assert!(
            field_names.contains(&"age"),
            "field 'age' missing; fields: {field_names:?}"
        );
        // Type names must NOT appear as field names
        assert!(
            !field_names.contains(&"String"),
            "type 'String' wrongly used as field name; fields: {field_names:?}"
        );
    }
}
