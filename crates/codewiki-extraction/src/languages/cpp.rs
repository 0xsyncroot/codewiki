//! T-214 — C++ language extractor.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

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
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingEither {
        block_node_kind: "comment",
        block_prefix: "/**",
        line_node_kind: "comment",
        line_prefix: "///",
    },
};

impl LanguageExtractor for CppExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn extract_cpp_class() {
        let source = r#"
class Calculator {
public:
    int add(int a, int b) { return a + b; }
};
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".cpp").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(batch.nodes.iter().any(|n| n.name == "Calculator"), "nodes: {:?}", batch.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>());
    }
}
