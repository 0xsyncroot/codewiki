//! T-210 — Python language extractor.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

pub struct PythonExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &["function_definition"],
    class_types: &["class_definition"],
    // Python: methods are function_definitions inside classes — handled by
    // is_in_class_scope() check in the walker.
    method_types: &["function_definition"],
    interface_types: &[],
    struct_types: &[],
    enum_types: &[],
    enum_member_types: &[],
    type_alias_types: &[],
    import_types: &["import_statement", "import_from_statement"],
    call_types: &["call"],
    variable_types: &["assignment"],
    property_types: &[],
    field_types: &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PythonFirstBodyString { body_field: "body" },
};

impl LanguageExtractor for PythonExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }
}

#[cfg(test)]
mod tests {
    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn extract_python_class_and_method() {
        let source = r#"
class Animal:
    def __init__(self, name):
        self.name = name

    def speak(self):
        return self.name
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".py").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(batch.nodes.iter().any(|n| n.name == "Animal"), "nodes: {:?}", batch.nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
        // Methods should be extracted inside class
        assert!(batch.nodes.iter().any(|n| n.name == "speak" || n.name == "__init__"));
    }

    #[test]
    fn extract_python_import() {
        let source = "from os import path\nimport sys\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".py").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(!batch.unresolved_refs.is_empty(), "expected import refs");
    }
}
