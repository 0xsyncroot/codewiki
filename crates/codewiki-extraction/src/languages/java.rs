//! T-213 — Java language extractor.

use crate::ast_walker::{
    generate_node_id, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::{NodeKind, UnresolvedRef};

pub struct JavaExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &[],
    // C-JAVA-1 fix: interface_declaration / enum_declaration must route to the
    // interface/enum extractors (so they get NodeKind::Interface / Enum), not
    // be lumped in with class_declaration (which emits NodeKind::Class).
    class_types: &["class_declaration"],
    method_types: &["method_declaration", "constructor_declaration"],
    interface_types: &["interface_declaration"],
    struct_types: &[],
    enum_types: &["enum_declaration"],
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
            let raw = node.utf8_text(ctx.source.as_bytes()).unwrap_or("").trim();
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

        // C-JAVA-3: class_declaration heritage → implements/extends refs.
        //
        // We return `false` so the generic `extract_class` still emits the class
        // node and recurses (methods/fields/imports the hook handles stay intact).
        // The class node id is deterministic (NodeKind::Class + name + line), so
        // we can pre-compute it here for use as the ref's `from_node_id`.
        if node.kind() == "class_declaration" {
            let src = ctx.source.as_bytes();
            if let Some(name_node) = node.child_by_field_name("name") {
                let cname = name_node.utf8_text(src).unwrap_or("").to_string();
                if !cname.is_empty() {
                    let line = node.start_position().row as u32 + 1;
                    let from_id = generate_node_id(ctx.file_path, &NodeKind::Class, &cname, line);

                    // implements: super_interfaces → type_list → type_identifier(s)
                    if let Some(ifaces) = node.child_by_field_name("interfaces") {
                        let mut cursor = ifaces.walk();
                        for tl in ifaces.named_children(&mut cursor) {
                            if tl.kind() == "type_list" {
                                let mut tc = tl.walk();
                                for ty in tl.named_children(&mut tc) {
                                    emit_heritage_ref(ctx, &from_id, &cname, &ty, "implements");
                                }
                            }
                        }
                    }

                    // extends: superclass → _type
                    if let Some(superclass) = node.child_by_field_name("superclass") {
                        let mut cursor = superclass.walk();
                        for ty in superclass.named_children(&mut cursor) {
                            emit_heritage_ref(ctx, &from_id, &cname, &ty, "extends");
                        }
                    }
                }
            }
            return false; // let extract_class emit the class node + recurse
        }

        false
    }
}

/// Emit an `implements`/`extends` unresolved ref from `from_id` (a class node)
/// to the type named by `ty_node`. The reference name is the bare type text
/// (e.g. `Animal`, `List`); generic args are stripped so the resolver can match
/// against the declared type's node name.
fn emit_heritage_ref(
    ctx: &mut ExtractCtx,
    from_id: &str,
    class_name: &str,
    ty_node: &tree_sitter::Node,
    kind: &str,
) {
    let raw = ty_node
        .utf8_text(ctx.source.as_bytes())
        .unwrap_or("")
        .trim();
    // Strip generic arguments: `List<String>` → `List`.
    let name = raw.split('<').next().unwrap_or(raw).trim();
    // For scoped/qualified types keep the trailing segment (`a.b.Foo` → `Foo`).
    let name = name.rsplit('.').next().unwrap_or(name);
    if name.is_empty() {
        return;
    }
    let line = ty_node.start_position().row as u32 + 1;
    let ref_id = generate_node_id(
        ctx.file_path,
        &NodeKind::Class,
        &format!("{kind}:{class_name}:{name}"),
        line,
    );
    ctx.unresolved.push(UnresolvedRef {
        id: ref_id,
        from_node_id: from_id.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind.to_string(),
        file_path: ctx.file_path.to_string(),
        line: Some(line),
        col: None,
        metadata: None,
    });
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
        let modules: Vec<&str> = import_refs
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
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

    /// C-JAVA-1/2/3: interface kind, calls edge, and implements ref.
    #[test]
    fn java_interface_kind_calls_and_implements() {
        let source = r#"
package app;

interface Animal {
    String speak();
}

class Dog implements Animal {
    public String speak() {
        return bark();
    }
    private String bark() {
        return "woof";
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".java").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);

        // C-JAVA-1: Animal must be Interface, not Class.
        let animal = batch
            .nodes
            .iter()
            .find(|n| n.name == "Animal")
            .expect("Animal node should exist");
        assert_eq!(
            animal.kind,
            NodeKind::Interface,
            "Animal must be Interface, got {:?}",
            animal.kind
        );

        // C-JAVA-2: speak -> bark calls edge.
        let calls: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "calls")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert!(
            calls.contains(&"bark"),
            "expected speak->bark; calls: {calls:?}"
        );

        // C-JAVA-3: Dog implements Animal.
        let impls: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert_eq!(impls, vec!["Animal"], "implements refs: {impls:?}");
    }
}
