//! T-215 — Kotlin language extractor.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

pub struct KotlinExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_declaration"],
    class_types: &["class_declaration", "object_declaration"],
    method_types: &["function_declaration"],
    interface_types: &["interface_declaration"],
    struct_types: &[],
    enum_types: &["enum_class_body"],
    enum_member_types: &["enum_entry"],
    type_alias_types: &["type_alias"],
    import_types: &["import_header"],
    call_types: &["call_expression", "postfix_expression"],
    variable_types: &["property_declaration"],
    property_types: &[],
    field_types: &[],
    extra_class_types: &[],
    namespace_types: &[],
    // kotlin-ng binds the declaration name as `identifier [field=name]`;
    // "simple_identifier" was a node KIND from the older fwcd grammar and never
    // matched a field, so naming silently relied on the walker's fallback.
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingBlockComment {
        node_kind: "multiline_comment",
        prefix: "/**",
    },
};

impl LanguageExtractor for KotlinExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn kotlin_registered() {
        let source = "class Foo {\n    fun bar(): Int = 42\n}\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".kt").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(!batch.nodes.is_empty());
    }

    /// Regression: with `name_field` pointing at a node kind instead of the
    /// grammar's `name` field, Kotlin symbol naming silently relied on the
    /// walker's permissive fallback; tightening that fallback made Kotlin
    /// extract zero symbols. Both the config and the fallback now name these.
    #[test]
    fn declarations_are_named() {
        use crate::ast_walker::extract_file;
        use std::io::Write;
        let source = r#"
fun target(): Int = 1
class Holder {
    fun method(): Int = 2
}
object Single {
    fun run(): Int = 3
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".kt").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let names: Vec<String> = batch.nodes.iter().map(|n| n.name.clone()).collect();
        for expected in ["target", "Holder", "method", "Single", "run"] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing symbol {expected}; got {names:?}"
            );
        }
    }
}
