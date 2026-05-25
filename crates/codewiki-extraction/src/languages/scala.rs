//! Scala language extractor (via WASM grammar — tree-sitter-scala).
//!
//! Covers Scala 2 and 3.
//!
//! Special handling in `visit_node_hook`:
//! - `trait_definition` → `NodeKind::Trait` (not Class, though it's in class_types).
//! - `val_definition` / `var_definition` → name in `pattern` field, emit Field or Variable.
//! - `enum_case_definitions` children → `NodeKind::EnumMember`.
//! - `extension_definition` → traverse body inline, no container node.

use crate::ast_walker::{
    generate_node_id, is_in_function_scope, walk_node, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::{Edge, EdgeKind, Node, NodeKind};

pub struct ScalaExtractor;

pub static CONFIG: LanguageConfig = LanguageConfig {
    // All functions in Scala are methods; use method_types.
    function_types:    &[],
    class_types:       &["class_definition", "object_definition", "trait_definition"],
    method_types:      &["function_definition", "function_declaration"],
    interface_types:   &[],
    struct_types:      &[],
    enum_types:        &["enum_definition"],
    enum_member_types: &[],
    type_alias_types:  &["type_definition"],
    // S-1: import_declaration handled in visit_node_hook to support selector imports;
    // keep in import_types so the generic path serves as a fallback when hook returns false.
    import_types:      &["import_declaration"],
    call_types:        &["call_expression"],
    // val/var use the `pattern` field — handled via visit_node_hook.
    variable_types:    &[],
    property_types:    &[],
    field_types:       &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field:        "name",
    body_field:        "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingBlockComment {
        node_kind: "block_comment",
        prefix: "/**",
    },
};

// ── Helper: emit a node directly (bypassing emit_node) ───────────────────────

fn emit_node_direct(
    ctx: &mut ExtractCtx,
    kind: NodeKind,
    name: &str,
    ts_node: &tree_sitter::Node,
    is_exported: bool,
) -> Option<String> {
    if name.is_empty() || name == "<anonymous>" {
        return None;
    }
    let line = ts_node.start_position().row as u32 + 1;
    let id = generate_node_id(ctx.file_path, &kind, name, line);

    let node = Node {
        id: id.clone(),
        name: name.to_string(),
        qualified_name: ctx.qualified_name(name),
        kind: kind.clone(),
        language: ctx.language.clone(),
        file_path: ctx.file_path.to_string(),
        start_line: line,
        end_line: ts_node.end_position().row as u32 + 1,
        start_col: ts_node.start_position().column as u32,
        end_col: ts_node.end_position().column as u32,
        is_exported,
        signature: None,
        docstring: None,
        metadata: None,
    };
    ctx.nodes.push(node);

    if let Some(parent_id) = ctx.scope.last() {
        ctx.edges.push(Edge {
            id: format!("{parent_id}->{id}-contains"),
            source_id: parent_id.clone(),
            target_id: id.clone(),
            kind: EdgeKind::Contains,
            line: Some(line),
            col: None,
            provenance: Some("extraction".into()),
            confidence: Some(1.0),
            metadata: None,
        });
    }
    Some(id)
}

impl LanguageExtractor for ScalaExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        let src = ctx.source.as_bytes();

        match node.kind() {
            // S-1: `import foo.{List, Map, _}` — iterate namespace_selectors to emit one ref
            // per selected name, and emit the full dotted path for wildcard / bare imports.
            // Returning true suppresses the generic extract_import (which only captures the
            // first identifier "scala" and drops the rest).
            //
            // Actual grammar shape (verified by tree dump):
            //   import_declaration
            //     identifier = "scala"            ← path components as flat identifiers
            //     identifier = "collection"        ← separated by anonymous "." tokens
            //     namespace_selectors              ← named child containing selectors
            //       identifier = "List"            ← named selector
            //       identifier = "Map"
            //       namespace_wildcard = "_"       ← wildcard selector
            //
            // For `import scala.collection.immutable.List` (no selectors):
            //   import_declaration
            //     identifier = "scala"
            //     identifier = "collection"
            //     identifier = "immutable"
            //     identifier = "List"
            "import_declaration" => {
                let from_id = ctx.scope.last().cloned().unwrap_or_default();
                let line = node.start_position().row as u32 + 1;

                // Collect path identifiers and look for namespace_selectors.
                let mut path_parts: Vec<String> = Vec::new();
                let mut selectors_node: Option<tree_sitter::Node> = None;
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "identifier" => {
                            let text = child.utf8_text(src).unwrap_or("").trim().to_string();
                            if !text.is_empty() {
                                path_parts.push(text);
                            }
                        }
                        "namespace_selectors" => {
                            selectors_node = Some(child);
                        }
                        _ => {}
                    }
                }

                let path_text = path_parts.join(".");

                if let Some(sel_node) = selectors_node {
                    // Selector import: `import foo.bar.{A, B, _}`
                    let mut c2 = sel_node.walk();
                    for selector in sel_node.named_children(&mut c2) {
                        match selector.kind() {
                            "namespace_wildcard" => {
                                // Wildcard — emit the bare package path.
                                if !path_text.is_empty() {
                                    ctx.emit_import(&from_id, &path_text, line);
                                }
                            }
                            "identifier" => {
                                let sel_name = selector.utf8_text(src).unwrap_or("").trim().to_string();
                                if !sel_name.is_empty() {
                                    let qualified = if path_text.is_empty() {
                                        sel_name
                                    } else {
                                        format!("{path_text}::{sel_name}")
                                    };
                                    ctx.emit_import(&from_id, &qualified, line);
                                }
                            }
                            _ => {
                                // Other selector kinds (renamed, etc.) — try identifier child.
                                let mut sc = selector.walk();
                                let children: Vec<_> = selector.named_children(&mut sc).collect();
                                if let Some(ident) = children.iter().find(|c| c.kind() == "identifier") {
                                    let sel_name = ident.utf8_text(src).unwrap_or("").trim().to_string();
                                    if !sel_name.is_empty() {
                                        let qualified = if path_text.is_empty() {
                                            sel_name
                                        } else {
                                            format!("{path_text}::{sel_name}")
                                        };
                                        ctx.emit_import(&from_id, &qualified, line);
                                    }
                                }
                            }
                        }
                    }
                } else if !path_text.is_empty() {
                    // Bare import without selectors: `import foo.bar.Baz`
                    ctx.emit_import(&from_id, &path_text, line);
                }

                true // consumed
            }

            // trait_definition → NodeKind::Trait (override default class extraction).
            "trait_definition" => {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    return false;
                }
                if let Some(id) = emit_node_direct(ctx, NodeKind::Trait, &name, node, false) {
                    ctx.scope.push(id);
                    let mut cursor = node.walk();
                    for child in node.named_children(&mut cursor) {
                        walk_node(&child, ctx, self);
                    }
                    ctx.scope.pop();
                }
                true
            }

            // val_definition / var_definition — name lives in `pattern` field.
            // S-2: at top level, `val` → Constant and `var` → Variable (not both Variable).
            "val_definition" | "var_definition" => {
                let pattern = node.child_by_field_name("pattern")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("")
                    .to_string();
                if pattern.is_empty() {
                    return false;
                }
                // Skip function-local val/var definitions (TS parity: only top-level variables).
                if is_in_function_scope(ctx) {
                    return true; // consumed but not emitted
                }
                // Inside a class scope → Field; at top level: val → Constant, var → Variable.
                let kind = if is_in_class_scope(ctx) {
                    NodeKind::Field
                } else if node.kind() == "val_definition" {
                    NodeKind::Constant
                } else {
                    NodeKind::Variable
                };
                emit_node_direct(ctx, kind, &pattern, node, false);
                true
            }

            // enum_case_definitions — iterate children and emit EnumMember.
            "enum_case_definitions" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "simple_enum_case" || child.kind() == "full_enum_case" {
                        let case_name = child
                            .child_by_field_name("name")
                            .and_then(|n| n.utf8_text(src).ok())
                            .unwrap_or("")
                            .to_string();
                        if !case_name.is_empty() {
                            emit_node_direct(ctx, NodeKind::EnumMember, &case_name, &child, false);
                        }
                    }
                }
                true
            }

            // extension_definition — traverse body children inline, no container node.
            "extension_definition" => {
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        walk_node(&child, ctx, self);
                    }
                }
                true
            }

            _ => false,
        }
    }
}

/// Checks if any node in the current scope chain is a class-like type.
fn is_in_class_scope(ctx: &ExtractCtx) -> bool {
    for id in ctx.scope.iter().rev() {
        if let Some(n) = ctx.nodes.iter().find(|n| &n.id == id) {
            match n.kind {
                NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::Enum
                | NodeKind::Component => return true,
                NodeKind::File => return false,
                _ => {}
            }
        }
    }
    false
}

#[cfg(all(test, feature = "wasmtime-grammars"))]
mod tests {
    use crate::wasm_parser::extract_wasm;
    use crate::wasm_grammars::WasmGrammar;
    use codewiki_core::NodeKind;
    use std::io::Write;

    #[test]
    fn scala_extract_class_object_trait_method_field() {
        let source = r#"
class Animal(name: String) {
  def speak(): String = s"I am $name"
  val legs: Int = 4
}

object Config {
  val version = "1.0"
}

trait Runnable {
  def run(): Unit
}

def topLevel(x: Int): Int = x * 2
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".scala").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Scala)
            .expect("Scala WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        let kinds: Vec<&NodeKind> = batch.nodes.iter().map(|n| &n.kind).collect();
        eprintln!("Scala nodes: {:?}", names);
        eprintln!("Scala kinds: {kinds:?}");

        // File node.
        assert!(
            batch.nodes.iter().any(|n| n.kind == NodeKind::File),
            "no file node; nodes: {names:?}"
        );

        // Animal — class.
        assert!(
            batch.nodes.iter().any(|n| n.name == "Animal" && n.kind == NodeKind::Class),
            "expected Class 'Animal'; nodes: {names:?}"
        );

        // speak — method inside Animal.
        assert!(
            batch.nodes.iter().any(|n| n.name == "speak" && n.kind == NodeKind::Method),
            "expected Method 'speak'; nodes: {names:?}"
        );

        // legs — field inside Animal.
        assert!(
            batch.nodes.iter().any(|n| n.name == "legs" && n.kind == NodeKind::Field),
            "expected Field 'legs'; nodes: {names:?}"
        );

        // Config — object (mapped to Class).
        assert!(
            batch.nodes.iter().any(|n| n.name == "Config" && n.kind == NodeKind::Class),
            "expected Class 'Config' (object); nodes: {names:?}"
        );

        // Runnable — trait.
        assert!(
            batch.nodes.iter().any(|n| n.name == "Runnable" && n.kind == NodeKind::Trait),
            "expected Trait 'Runnable'; nodes: {names:?}"
        );

        // run — method inside Runnable.
        assert!(
            batch.nodes.iter().any(|n| n.name == "run" && n.kind == NodeKind::Method),
            "expected Method 'run'; nodes: {names:?}"
        );

        // At least 7 non-file nodes.
        let non_file = batch.nodes.iter().filter(|n| n.kind != NodeKind::File).count();
        assert!(
            non_file >= 7,
            "expected >= 7 non-file nodes, got {non_file}; nodes: {names:?}"
        );
    }

    /// S-1: `import foo.{Bar, Baz}` should emit 2 import refs.
    /// Wildcard `_` should emit the bare path.
    #[test]
    fn scala_selector_imports() {
        let source = "import scala.collection.{List, Map, _}\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".scala").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Scala)
            .expect("Scala WASM extraction should succeed");

        let import_names: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports")
            .map(|r| r.reference_name.as_str())
            .collect();
        eprintln!("S-1 Scala import refs: {import_names:?}");

        // Named selectors: qualified form
        assert!(
            import_names.iter().any(|n| n.contains("List")),
            "expected import ref containing 'List'; refs: {import_names:?}"
        );
        assert!(
            import_names.iter().any(|n| n.contains("Map")),
            "expected import ref containing 'Map'; refs: {import_names:?}"
        );
        // Wildcard: bare path
        assert!(
            import_names.iter().any(|n| n.contains("scala.collection")),
            "expected import ref for wildcard path 'scala.collection'; refs: {import_names:?}"
        );
    }

    /// S-2: top-level `val` → Constant, top-level `var` → Variable (not both Variable).
    #[test]
    fn scala_val_is_constant_var_is_variable() {
        let source = "val MaxSize: Int = 100\nvar counter: Int = 0\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".scala").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Scala)
            .expect("Scala WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        eprintln!("S-2 Scala nodes: {:?}", batch.nodes.iter().map(|n| (&n.name, &n.kind)).collect::<Vec<_>>());

        assert!(
            batch.nodes.iter().any(|n| n.name == "MaxSize" && n.kind == NodeKind::Constant),
            "expected Constant 'MaxSize'; nodes: {names:?}"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "counter" && n.kind == NodeKind::Variable),
            "expected Variable 'counter'; nodes: {names:?}"
        );
    }
}
