//! T-209 — TypeScript + TSX language extractor.

use crate::ast_walker::{
    generate_node_id, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::{NodeKind, UnresolvedRef};

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

/// Reduce a heritage type/expression node to a bare type name. Handles the
/// common `class X extends Base implements Iface` shapes:
/// `identifier`/`type_identifier` directly, `member_expression`/
/// `nested_type_identifier` (`A.B` → `B`), and `generic_type` (`Foo<T>` → `Foo`).
fn heritage_name(node: &tree_sitter::Node, source: &[u8]) -> Option<String> {
    let raw = node.utf8_text(source).ok()?.trim();
    // Strip generic args, then take the trailing dotted segment.
    let no_generics = raw.split('<').next().unwrap_or(raw).trim();
    let name = no_generics.rsplit('.').next().unwrap_or(no_generics).trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

impl LanguageExtractor for TypeScriptExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        // C-TS-3: class heritage → implements/extends refs.
        //
        // Return `false` so the generic `extract_class` still emits the class
        // node + recurses (methods, etc.). The class node id is deterministic,
        // so we pre-compute it for the ref's `from_node_id`.
        if node.kind() == "class_declaration" || node.kind() == "abstract_class_declaration" {
            let src = ctx.source.as_bytes();
            if let Some(name_node) = node.child_by_field_name("name") {
                let cname = name_node.utf8_text(src).unwrap_or("").to_string();
                if !cname.is_empty() {
                    let line = node.start_position().row as u32 + 1;
                    let from_id = generate_node_id(ctx.file_path, &NodeKind::Class, &cname, line);

                    // class_heritage is a (non-field) child of the declaration.
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        if child.kind() != "class_heritage" {
                            continue;
                        }
                        let mut hc = child.walk();
                        for clause in child.named_children(&mut hc) {
                            match clause.kind() {
                                "implements_clause" => {
                                    // children: `type` nodes
                                    let mut cc = clause.walk();
                                    for ty in clause.named_children(&mut cc) {
                                        emit_ref(ctx, &from_id, &cname, &ty, "implements");
                                    }
                                }
                                "extends_clause" => {
                                    // `value` field: one or more class expressions.
                                    let mut ec = clause.walk();
                                    for v in clause.children_by_field_name("value", &mut ec) {
                                        emit_ref(ctx, &from_id, &cname, &v, "extends");
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            return false; // let extract_class emit the class node + recurse
        }

        false
    }
}

fn emit_ref(
    ctx: &mut ExtractCtx,
    from_id: &str,
    class_name: &str,
    ty_node: &tree_sitter::Node,
    kind: &str,
) {
    let name = match heritage_name(ty_node, ctx.source.as_bytes()) {
        Some(n) => n,
        None => return,
    };
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
        reference_name: name,
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
    use std::io::Write;

    /// Regression: anonymous arrows must never borrow a name from their
    /// `parameter` or `body` fields. Before the field-aware name fallback,
    /// `xs.find(a => a.ok)` emitted a Function named `a`, and `() => counter`
    /// one named `counter` — junk nodes that also captured every call resolved
    /// by bare name (12,000 `it(...)` edges in one indexed repo).
    #[test]
    fn anonymous_arrows_stay_anonymous() {
        let source = r#"
export function realFn(xs: {ok: boolean}[]): boolean {
    const hit = xs.find(a => a.ok)
    return hit !== undefined
}
let counter = 0
export const readCounter = () => counter
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".ts").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let fn_names: Vec<_> = batch
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, codewiki_core::NodeKind::Function))
            .map(|n| n.name.as_str())
            .collect();
        assert!(fn_names.contains(&"realFn"), "fn nodes: {fn_names:?}");
        assert!(
            !fn_names.contains(&"a") && !fn_names.contains(&"counter"),
            "arrow params/bodies leaked as function names: {fn_names:?}"
        );
    }

    /// Regression: module-scope `const` initialisers are walked, and their
    /// calls attribute to the binding (F6 — previously the whole subtree was
    /// skipped and these calls did not exist anywhere in the graph).
    #[test]
    fn module_scope_initialisers_produce_calls() {
        let source = r#"
export function targetFn(): number { return 1 }
export const arrowCaller = (): number => {
    return targetFn()
}
export const eagerValue = targetFn()
export class Holder {
    readonly fieldCaller = (): number => targetFn()
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".ts").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let callers: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_name == "targetFn")
            .filter_map(|r| {
                batch
                    .nodes
                    .iter()
                    .find(|n| n.id == r.from_node_id)
                    .map(|n| n.name.as_str())
            })
            .collect();
        for expected in ["arrowCaller", "eagerValue", "fieldCaller"] {
            assert!(
                callers.contains(&expected),
                "missing caller {expected}; callers: {callers:?}"
            );
        }
    }

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

    /// C-TS-3: `class Dog implements Animal` must produce an implements ref.
    #[test]
    fn ts_class_implements_ref() {
        let source = r#"
interface Animal {
  speak(): string;
}

export class Dog implements Animal {
  speak(): string {
    return this.bark();
  }
  bark(): string {
    return "woof";
  }
}

export function makeDog(): Dog {
  return new Dog();
}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".ts").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let impls: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "implements")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert_eq!(impls, vec!["Animal"], "implements refs: {impls:?}");
    }

    /// `class B extends A` must produce an extends ref.
    #[test]
    fn ts_class_extends_ref() {
        let source = r#"
class A {}
class B extends A {}
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".ts").unwrap();
        f.write_all(source.as_bytes()).unwrap();
        let batch = extract_file(f.path(), source);
        let ext: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "extends")
            .map(|r| r.reference_name.as_str())
            .collect();
        assert_eq!(ext, vec!["A"], "extends refs: {ext:?}");
    }
}
