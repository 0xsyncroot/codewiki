//! T-215 — PHP language extractor.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

pub struct PhpExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_definition"],
    class_types: &["class_declaration", "interface_declaration", "trait_declaration"],
    method_types: &["method_declaration"],
    interface_types: &[],
    struct_types: &[],
    enum_types: &["enum_declaration"],
    enum_member_types: &["enum_case"],
    type_alias_types: &[],
    import_types: &["namespace_use_declaration"],
    call_types: &["function_call_expression", "member_call_expression"],
    variable_types: &[],
    property_types: &["property_declaration"],
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

impl LanguageExtractor for PhpExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn php_registered() {
        let source = "<?php class Foo { public function bar() {} }\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".php").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        // Language registered and extractor exists
        let batch = extract_file(f.path(), source);
        // At minimum we get a file node
        assert!(!batch.nodes.is_empty());
    }
}
