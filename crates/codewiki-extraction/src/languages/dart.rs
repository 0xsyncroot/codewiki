//! T-215 — Dart language extractor.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

pub struct DartExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_signature", "function_declaration"],
    class_types: &["class_declaration"],
    method_types: &["method_signature", "function_signature"],
    interface_types: &[],
    struct_types: &[],
    enum_types: &["enum_declaration"],
    enum_member_types: &["enum_constant"],
    type_alias_types: &["type_alias"],
    import_types: &["import_or_export"],
    call_types: &["function_expression_invocation", "invocation_expression"],
    variable_types: &["initialized_variable_definition"],
    property_types: &[],
    field_types: &[],
    extra_class_types: &["mixin_declaration", "extension_declaration"],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingLineComment {
        node_kind: "documentation_comment",
        prefix: "///",
    },
};

impl LanguageExtractor for DartExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn dart_registered() {
        let source = "class Foo { void bar() {} }\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".dart").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(!batch.nodes.is_empty());
    }
}
