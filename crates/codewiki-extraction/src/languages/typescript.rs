//! T-209 — TypeScript + TSX language extractor.

use crate::ast_walker::{DocCommentStyle, LanguageConfig, LanguageExtractor};

pub struct TypeScriptExtractor;

static CONFIG: LanguageConfig = LanguageConfig {
    function_types: &[
        "function_declaration",
        "arrow_function",
        "function_expression",
        "generator_function_declaration",
    ],
    class_types: &["class_declaration", "abstract_class_declaration"],
    method_types: &["method_definition", "public_field_definition"],
    interface_types: &["interface_declaration"],
    struct_types: &[],
    enum_types: &["enum_declaration"],
    enum_member_types: &["property_identifier", "enum_assignment"],
    type_alias_types: &["type_alias_declaration"],
    import_types: &["import_statement"],
    call_types: &["call_expression"],
    variable_types: &["lexical_declaration", "variable_declaration"],
    property_types: &[],
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

impl LanguageExtractor for TypeScriptExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }
}

#[cfg(test)]
mod tests {

    use crate::ast_walker::extract_file;
    use std::io::Write;

    #[test]
    fn extract_function_and_class() {
        let source = r#"
export function greet(name: string): string {
    return `Hello ${name}`;
}

export class Greeter {
    constructor(private name: string) {}
    greet(): string {
        return `Hello ${this.name}`;
    }
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".ts").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let kinds: Vec<_> = batch
            .nodes
            .iter()
            .map(|n| format!("{:?}:{}", n.kind, n.name))
            .collect();
        // Must have file node
        assert!(batch
            .nodes
            .iter()
            .any(|n| matches!(n.kind, codewiki_core::NodeKind::File)));
        // Must have function
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| matches!(n.kind, codewiki_core::NodeKind::Function) && n.name == "greet"),
            "kinds: {kinds:?}"
        );
        // Must have class
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| matches!(n.kind, codewiki_core::NodeKind::Class) && n.name == "Greeter"),
            "kinds: {kinds:?}"
        );
    }

    #[test]
    fn extract_interface_and_enum() {
        let source = r#"
export interface Shape {
    area(): number;
}
export enum Color { Red, Green, Blue }
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".ts").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(batch
            .nodes
            .iter()
            .any(|n| matches!(n.kind, codewiki_core::NodeKind::Interface) && n.name == "Shape"));
        assert!(batch
            .nodes
            .iter()
            .any(|n| matches!(n.kind, codewiki_core::NodeKind::Enum) && n.name == "Color"));
    }

    #[test]
    fn extract_import_call() {
        let source = r#"
import { foo } from './utils';
function main() { foo(); }
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".ts").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        assert!(
            !batch.unresolved_refs.is_empty(),
            "expected unresolved refs"
        );
    }
}
