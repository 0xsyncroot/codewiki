//! T-212 — Rust language extractor.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

pub struct RustExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_item"],
    class_types: &[],
    // Methods inside impl blocks are function_items.
    method_types: &["function_item"],
    interface_types: &["trait_item"],
    struct_types: &["struct_item"],
    enum_types: &["enum_item"],
    enum_member_types: &["enum_variant"],
    type_alias_types: &["type_item"],
    import_types: &["use_declaration"],
    call_types: &["call_expression"],
    variable_types: &["let_declaration", "const_item", "static_item"],
    property_types: &[],
    field_types: &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingEither {
        block_node_kind: "block_comment",
        block_prefix: "/**",
        line_node_kind: "line_comment",
        line_prefix: "///",
    },
};

impl LanguageExtractor for RustExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use codewiki_core::NodeKind;
    use std::io::Write;

    #[test]
    fn extract_rust_struct_and_impl() {
        let source = r#"
pub struct Config {
    pub debug: bool,
}

impl Config {
    pub fn new() -> Self {
        Self { debug: false }
    }
    pub fn is_debug(&self) -> bool {
        self.debug
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".rs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let names: Vec<_> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            batch.nodes.iter().any(|n| n.name == "Config"),
            "nodes: {names:?}"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "new"),
            "nodes: {names:?}"
        );
    }

    #[test]
    fn extract_rust_enum() {
        let source = "pub enum Direction { North, South, East, West }\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".rs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(batch.nodes.iter().any(|n| n.name == "Direction"));
    }

    /// Bug 1 fix: impl Trait for Type must not produce FK-violating fake node IDs.
    /// A real Type (or Struct) node must exist for the impl target.
    #[test]
    fn rust_impl_emits_type_node_and_method_kind() {
        let source = r#"
trait Serialize {}
struct Registry {}
impl Serialize for Registry {
    pub fn serialize(&self) -> String { String::new() }
}
impl Registry {
    pub fn new() -> Self { Registry {} }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".rs").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let names: Vec<_> = batch.nodes.iter().map(|n| n.name.as_str()).collect();

        // There must be a node for Registry (Struct or Type — either satisfies FK).
        assert!(
            batch.nodes.iter().any(|n| n.name == "Registry"),
            "Registry node missing; nodes: {names:?}"
        );

        // Methods inside impl blocks must be Method kind, not Function.
        let serialize = batch.nodes.iter().find(|n| n.name == "serialize");
        assert!(
            serialize.is_some(),
            "serialize node missing; nodes: {names:?}"
        );
        assert_eq!(
            serialize.unwrap().kind,
            NodeKind::Method,
            "serialize should be Method inside impl block"
        );

        let new_node = batch.nodes.iter().find(|n| n.name == "new");
        assert!(new_node.is_some(), "new node missing; nodes: {names:?}");
        assert_eq!(
            new_node.unwrap().kind,
            NodeKind::Method,
            "new should be Method inside impl block"
        );

        // The implements unresolved ref must use a real node ID (not "type:Registry").
        for uref in &batch.unresolved_refs {
            assert!(
                !uref.from_node_id.starts_with("type:"),
                "fake from_node_id found: {:?}",
                uref.from_node_id
            );
        }
    }
}
