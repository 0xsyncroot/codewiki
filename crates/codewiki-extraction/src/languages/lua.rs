//! Lua language extractor (via WASM grammar).
//!
//! Uses the `tree-sitter-lua` WASM grammar bundled at
//! `grammars/tree-sitter-lua.wasm`.
//!
//! Key grammar notes (ABI-15 tree-sitter-lua):
//! - `function_call` has field `name` (the callee) and field `arguments` (arg list).
//! - String literals: `"utils"` is a `string` node whose first named child is
//!   `string_content` (the bare text without quotes).
//! - `local x = require("x")` is parsed as a `variable_declaration`, which the walker
//!   skips children for — so we must scan for require() inside the hook.

use crate::ast_walker::{generate_node_id, is_in_function_scope, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor};
use codewiki_core::{Edge, EdgeKind, Node, NodeKind};

pub struct LuaExtractor;

pub static CONFIG: LanguageConfig = LanguageConfig {
    function_types:    &["function_declaration"],
    class_types:       &[],
    // `function t:m()` / `function t.f()` handled in visit_node_hook.
    method_types:      &[],
    interface_types:   &[],
    struct_types:      &[],
    enum_types:        &[],
    enum_member_types: &[],
    type_alias_types:  &[],
    // require() is handled in visit_node_hook.
    import_types:      &[],
    call_types:        &["function_call"],
    variable_types:    &["variable_declaration"],
    property_types:    &[],
    field_types:       &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field:        "name",
    body_field:        "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingLineComment {
        node_kind: "comment",
        prefix: "---",
    },
};

/// Extract the module name from a `require(...)` call node.
/// Returns `Some(module_name)` if the call is a require, `None` otherwise.
pub fn require_module(call_node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
    // The callee is the `name` field — must be a bare `identifier` named "require".
    let name_node = call_node.child_by_field_name("name")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let callee = name_node.utf8_text(src).unwrap_or("");
    if callee != "require" {
        return None;
    }

    // The arg list is the `arguments` field.
    let args = call_node.child_by_field_name("arguments")?;

    // Look for a string argument first via `string_content` child.
    let mut cursor = args.walk();
    for arg in args.named_children(&mut cursor) {
        if arg.kind() == "string" {
            // Try string_content child (bare text without quotes).
            let mut c2 = arg.walk();
            for inner in arg.named_children(&mut c2) {
                if inner.kind() == "string_content" {
                    let text = inner.utf8_text(src).unwrap_or("").trim().to_string();
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
            // Fallback: strip quotes from the raw string text.
            let raw = arg.utf8_text(src).unwrap_or("");
            let stripped = raw.trim_matches(|c: char| c == '"' || c == '\'' || c == '[' || c == ']');
            if !stripped.is_empty() {
                return Some(stripped.to_string());
            }
        }
    }
    None
}

/// Recursively scan a node tree for require() calls and emit import refs.
fn scan_for_require(node: &tree_sitter::Node, ctx: &mut ExtractCtx) {
    let src = ctx.source.as_bytes();
    if node.kind() == "function_call" {
        if let Some(module) = require_module(node, src) {
            let from_id = ctx.scope.last().cloned().unwrap_or_default();
            let line = node.start_position().row as u32 + 1;
            ctx.emit_import(&from_id, &module, line);
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        scan_for_require(&child, ctx);
    }
}

impl LanguageExtractor for LuaExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        let src = ctx.source.as_bytes();

        match node.kind() {
            // Detect `function t:m()` / `function t.f()` — table method declarations.
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let kind = name_node.kind();
                    if kind == "dot_index_expression" || kind == "method_index_expression" {
                        let receiver = name_node
                            .child_by_field_name("table")
                            .and_then(|n| n.utf8_text(src).ok())
                            .unwrap_or("")
                            .to_string();
                        let method = name_node
                            .child_by_field_name("field")
                            .or_else(|| name_node.child_by_field_name("method"))
                            .and_then(|n| n.utf8_text(src).ok())
                            .unwrap_or("")
                            .to_string();

                        if !method.is_empty() {
                            let qualified = if receiver.is_empty() {
                                method.clone()
                            } else {
                                format!("{receiver}::{method}")
                            };

                            let line = node.start_position().row as u32 + 1;
                            let id = generate_node_id(
                                ctx.file_path,
                                &NodeKind::Method,
                                &qualified,
                                line,
                            );
                            let node_record = Node {
                                id: id.clone(),
                                name: method,
                                qualified_name: qualified,
                                kind: NodeKind::Method,
                                language: ctx.language.clone(),
                                file_path: ctx.file_path.to_string(),
                                start_line: line,
                                end_line: node.end_position().row as u32 + 1,
                                start_col: node.start_position().column as u32,
                                end_col: node.end_position().column as u32,
                                is_exported: false,
                                signature: None,
                                docstring: None,
                                metadata: None,
                            };
                            if let Some(parent_id) = ctx.scope.last().cloned() {
                                ctx.edges.push(Edge {
                                    id: format!("{parent_id}->{id}-contains"),
                                    source_id: parent_id,
                                    target_id: id.clone(),
                                    kind: EdgeKind::Contains,
                                    line: Some(line),
                                    col: None,
                                    provenance: Some("extraction".into()),
                                    confidence: Some(1.0),
                                    metadata: None,
                                });
                            }
                            ctx.nodes.push(node_record);
                            return true; // consumed
                        }
                    }
                }
                // Plain function — let default dispatch handle it.
                false
            }

            // Detect `require("mod")` calls in bare call position.
            "function_call" => {
                if let Some(module) = require_module(node, src) {
                    let from_id = ctx.scope.last().cloned().unwrap_or_default();
                    let line = node.start_position().row as u32 + 1;
                    ctx.emit_import(&from_id, &module, line);
                    return true; // consumed — don't emit as plain call
                }
                false
            }

            // `M.x = function() end` — assignment-form method.
            //
            // Actual grammar shape (verified by tree dump):
            //   assignment_statement
            //     variable_list          ← named child[0], contains dot_index_expression(s)
            //       dot_index_expression
            //         table: identifier  ← e.g. "M"
            //         field: identifier  ← e.g. "greet"
            //     expression_list        ← named child[1], contains the RHS value(s)
            //       function_definition
            //
            // We check the FIRST element of `expression_list` is a `function_definition`,
            // then emit a Method node for the first `dot_index_expression` in `variable_list`.
            "assignment_statement" => {
                let src = ctx.source.as_bytes();
                // Need at least variable_list and expression_list.
                if node.named_child_count() < 2 {
                    return false;
                }
                // LHS: variable_list (first named child).
                let var_list = match node.named_child(0) {
                    Some(n) if n.kind() == "variable_list" => n,
                    _ => return false,
                };
                // RHS: expression_list (second named child).
                let expr_list = match node.named_child(1) {
                    Some(n) if n.kind() == "expression_list" => n,
                    _ => return false,
                };
                // First value in expression_list must be function_definition.
                let first_value = match expr_list.named_child(0) {
                    Some(n) => n,
                    None => return false,
                };
                if first_value.kind() != "function_definition" {
                    return false;
                }
                // First entry in variable_list must be dot_index_expression.
                let target = match var_list.named_child(0) {
                    Some(n) if n.kind() == "dot_index_expression" => n,
                    _ => return false,
                };

                let receiver = target
                    .child_by_field_name("table")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("")
                    .to_string();
                let field = target
                    .child_by_field_name("field")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("")
                    .to_string();

                if field.is_empty() {
                    return false;
                }

                let qualified = if receiver.is_empty() {
                    field.clone()
                } else {
                    format!("{receiver}::{field}")
                };

                let line = node.start_position().row as u32 + 1;
                let id = generate_node_id(
                    ctx.file_path,
                    &NodeKind::Method,
                    &qualified,
                    line,
                );
                let node_record = Node {
                    id: id.clone(),
                    name: field,
                    qualified_name: qualified,
                    kind: NodeKind::Method,
                    language: ctx.language.clone(),
                    file_path: ctx.file_path.to_string(),
                    start_line: line,
                    end_line: node.end_position().row as u32 + 1,
                    start_col: node.start_position().column as u32,
                    end_col: node.end_position().column as u32,
                    is_exported: false,
                    signature: None,
                    docstring: None,
                    metadata: None,
                };
                if let Some(parent_id) = ctx.scope.last().cloned() {
                    ctx.edges.push(Edge {
                        id: format!("{parent_id}->{id}-contains"),
                        source_id: parent_id,
                        target_id: id.clone(),
                        kind: EdgeKind::Contains,
                        line: Some(line),
                        col: None,
                        provenance: Some("extraction".into()),
                        confidence: Some(1.0),
                        metadata: None,
                    });
                }
                ctx.nodes.push(node_record);
                true // consumed
            }

            // `local x = require("x")` — the walker skips children for variable_declaration
            // (skip_children = true), so we must scan for require() here before it's skipped.
            //
            // Also handles multi-name `local a, b, c = ...`.
            //
            // Actual grammar shape (verified by tree dump):
            //   variable_declaration
            //     assignment_statement           ← named child
            //       variable_list
            //         identifier  "a"
            //         identifier  "b"
            //         identifier  "c"
            //       expression_list
            //         ...
            //
            // For single-name locals the fallback in the walker's extract_variable handles it
            // (finds the first identifier child). We override here only to handle multi-name.
            "variable_declaration" => {
                // Always scan for require() calls even inside function bodies (import refs).
                scan_for_require(node, ctx);
                // Skip variable emission for function-local declarations (TS parity).
                if is_in_function_scope(ctx) {
                    return true; // consumed — suppress default extract_variable
                }
                let src = ctx.source.as_bytes();
                let mut emitted = false;
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "assignment_statement" {
                        // Multi-name form: walk variable_list inside the assignment_statement.
                        if let Some(var_list) = child.named_child(0) {
                            if var_list.kind() == "variable_list" {
                                let mut c2 = var_list.walk();
                                for ident in var_list.named_children(&mut c2) {
                                    if ident.kind() == "identifier" {
                                        let name = ident.utf8_text(src).unwrap_or("").to_string();
                                        if !name.is_empty() {
                                            ctx.emit_node(
                                                NodeKind::Variable,
                                                &name,
                                                &ident,
                                                false,
                                                None,
                                                None,
                                            );
                                            emitted = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Return true only if we handled names ourselves (suppress default extract_variable
                // which would find only the first identifier, causing duplication if we did emit).
                emitted
            }

            _ => false,
        }
    }
}

#[cfg(all(test, feature = "wasmtime-grammars"))]
mod tests {
    use crate::wasm_parser::extract_wasm;
    use crate::wasm_grammars::WasmGrammar;
    use codewiki_core::NodeKind;
    use std::io::Write;

    #[test]
    fn lua_extract_functions_methods_and_imports() {
        let source = r#"
local function greet(name)
    return "Hello " .. name
end

function MyTable:doSomething()
    return 42
end

local M = {}
function M.helper() end
local x = require("utils")
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".lua").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Lua)
            .expect("Lua WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        eprintln!("Lua nodes: {:?}", names);
        eprintln!(
            "Lua refs: {:?}",
            batch.unresolved_refs.iter().map(|r| (&r.reference_name, &r.reference_kind)).collect::<Vec<_>>()
        );

        // File node.
        assert!(
            batch.nodes.iter().any(|n| n.kind == NodeKind::File),
            "no file node; nodes: {names:?}"
        );

        // greet — top-level function.
        assert!(
            batch.nodes.iter().any(|n| n.name == "greet" && n.kind == NodeKind::Function),
            "expected Function 'greet'; nodes: {names:?}"
        );

        // doSomething — table method (MyTable:doSomething).
        assert!(
            batch.nodes.iter().any(|n| n.name == "doSomething" && n.kind == NodeKind::Method),
            "expected Method 'doSomething'; nodes: {names:?}"
        );

        // helper — table method (M.helper).
        assert!(
            batch.nodes.iter().any(|n| n.name == "helper" && n.kind == NodeKind::Method),
            "expected Method 'helper'; nodes: {names:?}"
        );

        // require("utils") — import ref.
        let import_refs: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "imports" && r.reference_name == "utils")
            .collect();
        assert!(
            !import_refs.is_empty(),
            "expected import ref for 'utils'; refs: {:?}",
            batch.unresolved_refs
        );

        // At least 3 non-file nodes.
        let non_file = batch.nodes.iter().filter(|n| n.kind != NodeKind::File).count();
        assert!(
            non_file >= 3,
            "expected >= 3 non-file nodes, got {non_file}; nodes: {names:?}"
        );
    }

    /// L-1: `M.greet = function() end` should emit a Method node.
    #[test]
    fn lua_assignment_form_method() {
        let source = r#"
local M = {}
M.greet = function(self, name) return name end
M.count = 42
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".lua").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Lua)
            .expect("Lua WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        eprintln!("L-1 Lua nodes: {names:?}");

        // greet assigned as function → Method
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "greet" && n.kind == NodeKind::Method),
            "expected Method 'greet'; nodes: {names:?}"
        );

        // count is a plain value assignment, not a function → should NOT be a Method
        assert!(
            !batch
                .nodes
                .iter()
                .any(|n| n.name == "count" && n.kind == NodeKind::Method),
            "unexpected Method 'count'; nodes: {names:?}"
        );
    }

    /// L-3: `local a, b = 1, 2` should emit Variable nodes for both a and b.
    #[test]
    fn lua_multi_name_local() {
        let source = "local a, b, c = 1, 2, 3\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".lua").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Lua)
            .expect("Lua WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        eprintln!("L-3 Lua nodes: {names:?}");

        for var in &["a", "b", "c"] {
            assert!(
                batch
                    .nodes
                    .iter()
                    .any(|n| n.name == *var && n.kind == NodeKind::Variable),
                "expected Variable '{var}'; nodes: {names:?}"
            );
        }
    }
}
