//! Pascal language extractor (via WASM grammar — tree-sitter-pascal / Isopod grammar).
//!
//! Covers Pascal/Delphi/FreePascal. The grammar uses node types like
//! `declProc`, `declClass`, `declIntf`, `declEnum`, `declUses`, `exprCall`.
//!
//! Key structural note: In the tree-sitter-pascal grammar, type declarations are
//! wrapped in a `declType` node:
//!   `type TFoo = class ... end;`  →  declType { name: TFoo, body: declClass }
//!   `type TColor = (Red, ...);`   →  declType { name: TFoo, body: declEnum }
//!   `type TMyInt = Integer;`      →  declType { name: TMyInt, (no special body kind) }
//!
//! So `declClass` and `declEnum` never appear at the top level; they are always
//! children of `declType`. We handle this in `visit_node_hook`.
//!
//! Forward-declaration deduplication: Pascal's two-pass structure emits a
//! forward `declProc` in the interface section (no body) and an implementation
//! `declProc` with a body. Both are emitted as separate nodes; deduplication is
//! left for a follow-up (acceptable per the design).

use crate::ast_walker::{
    generate_node_id, walk_node, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor,
};
use codewiki_core::{Edge, EdgeKind, Node, NodeKind};

pub struct PascalExtractor;

pub static CONFIG: LanguageConfig = LanguageConfig {
    // `declProc` covers both `procedure` and `function` in the Pascal AST.
    function_types: &["declProc"],
    // declClass/declIntf/declEnum are INSIDE declType — handled by visit_node_hook.
    class_types: &[],
    // Methods inside a class body are also `declProc`.
    method_types: &["declProc"],
    interface_types: &[],
    struct_types: &[],
    enum_types: &[],
    enum_member_types: &[],
    // declType is the wrapper for all type declarations — see visit_node_hook.
    type_alias_types: &[],
    // `uses` clause imports are handled in visit_node_hook (P-1) — keep out of CONFIG
    // to avoid running the broken generic extract_import path.
    import_types: &[],
    call_types: &["exprCall"],
    variable_types: &["declField", "declConst"],
    // P-4: class properties are emitted via generic property extraction.
    property_types: &["declProp"],
    field_types: &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::None,
};

/// Emit a node directly, with a Contains edge from the current scope.
fn emit_direct(
    ctx: &mut ExtractCtx,
    kind: NodeKind,
    name: &str,
    ts_node: &tree_sitter::Node,
    is_exported: bool,
) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let line = ts_node.start_position().row as u32 + 1;
    let id = generate_node_id(ctx.file_path, &kind, name, line);
    ctx.nodes.push(Node {
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
    });
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

impl LanguageExtractor for PascalExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        let src = ctx.source.as_bytes();

        match node.kind() {
            // P-1 (CRITICAL): `uses SysUtils, Classes;` — the generic extract_import looks for
            // field names that don't exist in the Pascal grammar.  We handle it here.
            //
            // Actual grammar shape (verified by tree dump):
            //   declUses
            //     kUses (anon)
            //     moduleName          ← named child; contains identifier
            //       identifier = "SysUtils"
            //     moduleName
            //       identifier = "Classes"
            //
            // Each unit name lives inside a `moduleName` node as an `identifier` child.
            "declUses" => {
                let from_id = ctx.scope.last().cloned().unwrap_or_default();
                let line = node.start_position().row as u32 + 1;
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    let kind = child.kind();
                    if kind == "moduleName" {
                        // Get the identifier child inside moduleName.
                        let mut c2 = child.walk();
                        for ident in child.named_children(&mut c2) {
                            if ident.kind() == "identifier" {
                                let unit_name =
                                    ident.utf8_text(src).unwrap_or("").trim().to_string();
                                if !unit_name.is_empty() {
                                    ctx.emit_import(&from_id, &unit_name, line);
                                }
                            }
                        }
                    } else if kind == "identifier" || kind == "unitId" {
                        // Fallback: some grammar versions use identifier directly.
                        let unit_name = child.utf8_text(src).unwrap_or("").trim().to_string();
                        if !unit_name.is_empty() {
                            ctx.emit_import(&from_id, &unit_name, line);
                        }
                    }
                }
                true // consumed — prevents broken generic extract_import from running
            }

            // Type declarations: declType wraps declClass, declIntf, declEnum, declRecord, etc.
            "declType" => {
                // Extract the declared name from the `name` field.
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("")
                    .to_string();
                if name.is_empty() {
                    return false;
                }

                // Peek inside to determine the actual kind.
                //
                // Actual grammar shape (verified by tree dump):
                //   declType
                //     identifier          ← type name (no `name` field name)
                //     kEq (anon)
                //     declClass           ← for both `class` and `record`
                //       kClass|kRecord    ← first token distinguishes record from class
                //     OR
                //     declIntf
                //     OR
                //     type                ← wrapper when grammar uses indirect nesting
                //       declEnum
                //         declEnumValue   ← member container
                //           identifier   ← member name
                //
                // NOTE: The grammar maps both `class` and `record` to `declClass`.
                // A record is identified by `kRecord` as a direct child of `declClass`.
                // There is no separate `declRecord` node type.
                let mut body_kind = NodeKind::Type;
                let mut body_node: Option<tree_sitter::Node> = None;

                fn classify_decl_class(child: &tree_sitter::Node) -> NodeKind {
                    // Walk all children (including anonymous) to find kRecord.
                    let mut cc = child.walk();
                    let is_record = child.children(&mut cc).any(|c| c.kind() == "kRecord");
                    if is_record {
                        NodeKind::Struct
                    } else {
                        NodeKind::Class
                    }
                }

                // Check direct named children first.
                let mut cursor = node.walk();
                'outer: for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "declClass" => {
                            // P-3: record uses kRecord token → Struct; class → Class.
                            body_kind = classify_decl_class(&child);
                            body_node = Some(child);
                            break;
                        }
                        "declIntf" => {
                            body_kind = NodeKind::Interface;
                            body_node = Some(child);
                            break;
                        }
                        "declEnum" => {
                            body_kind = NodeKind::Enum;
                            body_node = Some(child);
                            break;
                        }
                        // A `type` wrapper node — look one level deeper.
                        "type" => {
                            let mut c2 = child.walk();
                            for inner in child.named_children(&mut c2) {
                                match inner.kind() {
                                    "declClass" => {
                                        body_kind = classify_decl_class(&inner);
                                        body_node = Some(inner);
                                        break 'outer;
                                    }
                                    "declIntf" => {
                                        body_kind = NodeKind::Interface;
                                        body_node = Some(inner);
                                        break 'outer;
                                    }
                                    "declEnum" => {
                                        body_kind = NodeKind::Enum;
                                        body_node = Some(inner);
                                        break 'outer;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }

                if let Some(id) = emit_direct(ctx, body_kind.clone(), &name, node, false) {
                    match body_kind {
                        NodeKind::Class | NodeKind::Interface | NodeKind::Struct => {
                            ctx.scope.push(id);
                            if let Some(body) = body_node {
                                let mut c = body.walk();
                                for child in body.named_children(&mut c) {
                                    walk_node(&child, ctx, self);
                                }
                            }
                            ctx.scope.pop();
                        }
                        // P-2: emit EnumMember for each declEnumValue inside the enum body.
                        //
                        // Grammar: declEnum → declEnumValue → identifier (member name)
                        NodeKind::Enum => {
                            ctx.scope.push(id);
                            if let Some(body) = body_node {
                                let mut c = body.walk();
                                for child in body.named_children(&mut c) {
                                    if child.kind() == "declEnumValue" {
                                        // Get identifier inside declEnumValue.
                                        let mut c2 = child.walk();
                                        for ident in child.named_children(&mut c2) {
                                            if ident.kind() == "identifier" {
                                                let member_name = ident
                                                    .utf8_text(src)
                                                    .unwrap_or("")
                                                    .trim()
                                                    .to_string();
                                                if !member_name.is_empty() {
                                                    emit_direct(
                                                        ctx,
                                                        NodeKind::EnumMember,
                                                        &member_name,
                                                        &ident,
                                                        false,
                                                    );
                                                }
                                            }
                                        }
                                    } else if child.kind() == "identifier" {
                                        // Fallback: some grammar versions use identifier directly.
                                        let member_name =
                                            child.utf8_text(src).unwrap_or("").trim().to_string();
                                        if !member_name.is_empty() {
                                            emit_direct(
                                                ctx,
                                                NodeKind::EnumMember,
                                                &member_name,
                                                &child,
                                                false,
                                            );
                                        }
                                    } else {
                                        walk_node(&child, ctx, self);
                                    }
                                }
                            }
                            ctx.scope.pop();
                        }
                        _ => {}
                    }
                }

                true // consumed
            }

            _ => false,
        }
    }
}

#[cfg(all(test, feature = "wasmtime-grammars"))]
mod tests {
    use crate::wasm_grammars::WasmGrammar;
    use crate::wasm_parser::extract_wasm;
    use codewiki_core::NodeKind;
    use std::io::Write;

    #[test]
    fn pascal_extract_class_function_enum() {
        let source = r#"unit MyUnit;
interface
  procedure Greet(name: string);
  type TColor = (Red, Green, Blue);
  type TAnimal = class
    procedure Speak; virtual;
  end;
implementation
  procedure Greet(name: string);
  begin
    writeln('Hello ', name);
  end;
end.
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".pas").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Pascal)
            .expect("Pascal WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        eprintln!("Pascal nodes: {names:?}");
        eprintln!(
            "Pascal node kinds: {:?}",
            batch.nodes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );

        // File node.
        assert!(
            batch.nodes.iter().any(|n| n.kind == NodeKind::File),
            "no file node; nodes: {names:?}"
        );

        // At least 4 non-file nodes.
        let non_file = batch
            .nodes
            .iter()
            .filter(|n| n.kind != NodeKind::File)
            .count();
        assert!(
            non_file >= 4,
            "expected >= 4 non-file nodes, got {non_file}; nodes: {names:?}"
        );

        // TColor — should be emitted as Enum.
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "TColor" && n.kind == NodeKind::Enum),
            "expected Enum 'TColor'; nodes: {names:?}"
        );

        // TAnimal — should be emitted as Class.
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "TAnimal" && n.kind == NodeKind::Class),
            "expected Class 'TAnimal'; nodes: {names:?}"
        );

        // Greet — function (appears at least once).
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "Greet"
                    && matches!(n.kind, NodeKind::Function | NodeKind::Method)),
            "expected Function/Method 'Greet'; nodes: {names:?}"
        );
    }

    /// P-1 (critical): `uses SysUtils, Classes;` must emit 2 import refs.
    #[test]
    fn pascal_uses_imports() {
        let source = r#"unit Foo;
interface
uses SysUtils, Classes;
implementation
end.
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".pas").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Pascal)
            .expect("Pascal WASM extraction should succeed");

        eprintln!(
            "P-1 Pascal refs: {:?}",
            batch
                .unresolved_refs
                .iter()
                .map(|r| (&r.reference_name, &r.reference_kind))
                .collect::<Vec<_>>()
        );

        let import_names: Vec<&str> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports")
            .map(|r| r.reference_name.as_str())
            .collect();

        assert!(
            import_names.contains(&"SysUtils"),
            "expected import ref for 'SysUtils'; refs: {import_names:?}"
        );
        assert!(
            import_names.contains(&"Classes"),
            "expected import ref for 'Classes'; refs: {import_names:?}"
        );
    }

    /// P-2: enum members inside `declEnum` body should be emitted as EnumMember.
    #[test]
    fn pascal_enum_members() {
        let source = r#"unit Foo;
interface
type TDir = (North, South, East, West);
implementation
end.
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".pas").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Pascal)
            .expect("Pascal WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        eprintln!("P-2 Pascal nodes: {names:?}");

        // Enum container
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "TDir" && n.kind == NodeKind::Enum),
            "expected Enum 'TDir'; nodes: {names:?}"
        );

        // Enum members
        for member in &["North", "South", "East", "West"] {
            assert!(
                batch
                    .nodes
                    .iter()
                    .any(|n| n.name == *member && n.kind == NodeKind::EnumMember),
                "expected EnumMember '{member}'; nodes: {names:?}"
            );
        }
    }

    /// P-3: `type TPoint = record X, Y: Integer; end;` should emit a Struct.
    #[test]
    fn pascal_record_as_struct() {
        let source = r#"unit Foo;
interface
type TPoint = record
  X, Y: Integer;
end;
implementation
end.
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".pas").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Pascal)
            .expect("Pascal WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        eprintln!("P-3 Pascal nodes: {names:?}");

        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "TPoint" && n.kind == NodeKind::Struct),
            "expected Struct 'TPoint'; nodes: {names:?}"
        );
    }
}
