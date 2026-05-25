//! T-215 — Swift language extractor.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

pub struct SwiftExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_declaration"],
    class_types: &["class_declaration", "actor_declaration"],
    method_types: &["function_declaration"],
    interface_types: &["protocol_declaration"],
    struct_types: &["struct_declaration"],
    enum_types: &["enum_declaration"],
    enum_member_types: &["enum_case"],
    type_alias_types: &["typealias_declaration"],
    import_types: &["import_statement"],
    call_types: &["call_expression", "function_call_expression"],
    variable_types: &["property_declaration", "variable_declaration"],
    property_types: &[],
    field_types: &[],
    extra_class_types: &["extension_declaration"],
    namespace_types: &[],
    name_field: "name",
    body_field: "statements",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingEither {
        block_node_kind: "multiline_comment",
        block_prefix: "/**",
        line_node_kind: "comment",
        line_prefix: "///",
    },
};

impl LanguageExtractor for SwiftExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn swift_registered() {
        let source = "class Foo { func bar() {} }\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".swift").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(!batch.nodes.is_empty());
    }
}
