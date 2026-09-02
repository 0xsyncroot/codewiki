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
        assert!(
            batch.nodes.iter().any(|n| n.name == "Animal"),
            "nodes: {:?}",
            batch.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        // Methods should be extracted inside class
        assert!(batch
            .nodes
            .iter()
            .any(|n| n.name == "speak" || n.name == "__init__"));
    }

    #[test]
    fn extract_python_import() {
        let source = "from os import path\nimport sys\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".py").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(!batch.unresolved_refs.is_empty(), "expected import refs");
    }

    /// Regression: Python's `assignment` binds its LHS identifier to the
    /// `left` grammar field (there is no `name` field), so the field-aware
    /// name fallback must accept `left` — otherwise every module-level
    /// `bp = Blueprint(...)` singleton loses its Variable node entirely
    /// (observed: 136 -> 0 on pallets/flask).
    #[test]
    fn module_assignment_keeps_its_variable_node_and_calls() {
        let source = r#"
def make_thing():
    return 1

thing = make_thing()
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".py").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let var = batch
            .nodes
            .iter()
            .find(|n| matches!(n.kind, codewiki_core::NodeKind::Variable) && n.name == "thing");
        assert!(
            var.is_some(),
            "module-level assignment must emit its Variable node"
        );
        // The initialiser call attributes to the binding.
        let var_id = &var.unwrap().id;
        assert!(
            batch
                .unresolved_refs
                .iter()
                .any(|r| r.reference_name == "make_thing" && &r.from_node_id == var_id),
            "the initialiser call must attribute to `thing`; refs: {:?}",
            batch
                .unresolved_refs
                .iter()
                .map(|r| (&r.reference_name, &r.from_node_id))
                .collect::<Vec<_>>()
        );
    }
}
