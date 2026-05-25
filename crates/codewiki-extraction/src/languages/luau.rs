//! Luau language extractor (via WASM grammar).
//!
//! Standalone extractor for Luau (polychromatist/tree-sitter-luau commit 71b03e6,
//! Dec 2025).  Does NOT delegate to lua.rs — the new grammar uses completely
//! different node names from the Lua grammar, so any delegation would produce
//! zero symbols.
//!
//! ## Grammar node → extractor mapping
//!
//! | Grammar node       | What we extract                                    |
//! |--------------------|-----------------------------------------------------|
//! | `local_fn_stmt`    | Function (local)      via default dispatch          |
//! | `fn_stmt` (plain)  | Function (global/exported) via visit_node_hook      |
//! | `fn_stmt` (table.) | Method via visit_node_hook                          |
//! | `fn_stmt` (table:) | Method via visit_node_hook                          |
//! | `local_var_stmt`   | Variable(s) and/or anon-fn Function via hook        |
//! | `type_stmt`        | Type (+ export detection) via hook                  |
//! | `call_stmt`        | require() → import ref; other → call ref (default) |
//! | `anon_fn` in local | Function node named after the binding variable      |

use crate::ast_walker::{generate_node_id, is_in_function_scope, DocCommentStyle, ExtractCtx, LanguageConfig, LanguageExtractor};
use codewiki_core::{Edge, EdgeKind, Node, NodeKind};

pub struct LuauExtractor;

/// LanguageConfig for the new Luau grammar (polychromatist 71b03e6).
///
/// `local_fn_stmt` is in `function_types` so the default dispatch handles it
/// (field `function_name` → name).  Everything else is handled in
/// `visit_node_hook` and marked consumed so the default dispatch never sees it.
pub static CONFIG: LanguageConfig = LanguageConfig {
    // local_fn_stmt: handled by default dispatch (function_name field present)
    function_types:    &["local_fn_stmt"],
    class_types:       &[],
    method_types:      &[],       // fn_stmt table methods handled in hook
    interface_types:   &[],
    struct_types:      &[],
    enum_types:        &[],
    enum_member_types: &[],
    type_alias_types:  &[],       // type_stmt fully handled in hook (export check needed)
    import_types:      &[],       // require() handled in hook via call_stmt
    call_types:        &["call_stmt"],
    variable_types:    &[],       // local_var_stmt fully handled in hook
    property_types:    &[],
    field_types:       &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field:        "function_name",  // used by default dispatch for local_fn_stmt
    body_field:        "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::PrecedingLineComment {
        node_kind: "comment",
        prefix: "--!",
    },
};

// ── Private helpers ──────────────────────────────────────────────────────────

/// Extract the module name from a Luau `call_stmt` require() node.
///
/// In the new Luau grammar the callee is in the `invoked` field as a `var` node
/// whose `variable_name` child holds a `name` leaf.  A require call looks like:
/// ```luau
/// require("module.path")
/// ```
/// We only match bare `require` (a plain variable reference, no dot-chain).
fn luau_require_module(call_node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
    // Plain call: `invoked` field must be a `var` node with just a `variable_name`
    // (no `table_name` — that would be a chained index expression).
    let invoked = call_node.child_by_field_name("invoked")?;
    let callee_text = if invoked.kind() == "name" {
        // Direct name node (some grammar versions / edge cases).
        invoked.utf8_text(src).unwrap_or("").to_string()
    } else if invoked.kind() == "var" {
        // `var` node: look for `variable_name` field (plain identifier, no table_name).
        if invoked.child_by_field_name("table_name").is_some() {
            // This is a table index expression like `mod.require` — not a require.
            return None;
        }
        invoked
            .child_by_field_name("variable_name")
            .and_then(|n| n.utf8_text(src).ok())
            .unwrap_or("")
            .to_string()
    } else {
        return None;
    };

    if callee_text != "require" {
        return None;
    }

    // The arglist is a named child with kind "arglist".
    let mut cursor = call_node.walk();
    for child in call_node.named_children(&mut cursor) {
        if child.kind() != "arglist" {
            continue;
        }
        // Walk arglist children looking for a string or exp that contains a string.
        let mut c2 = child.walk();
        for arg in child.named_children(&mut c2) {
            if let Some(s) = extract_string_text(&arg, src) {
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Extract bare string content from a `string` node or an `exp` wrapping one.
fn extract_string_text(node: &tree_sitter::Node, src: &[u8]) -> Option<String> {
    if node.kind() == "string" {
        let raw = node.utf8_text(src).unwrap_or("");
        // Strip surrounding quotes or long-string brackets.
        let stripped = raw
            .trim_start_matches('"')
            .trim_end_matches('"')
            .trim_start_matches('\'')
            .trim_end_matches('\'');
        if !stripped.is_empty() {
            return Some(stripped.to_string());
        }
    }
    // Recurse one level for wrapped expressions.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(s) = extract_string_text(&child, src) {
            return Some(s);
        }
    }
    None
}

/// Recursively scan `node` for `call_stmt` require() calls and emit import refs.
///
/// Parallel to `lua::scan_for_require` but searches for `call_stmt` (not
/// `function_call`) and uses `luau_require_module` (not `lua::require_module`).
fn scan_for_require_luau(node: &tree_sitter::Node, ctx: &mut ExtractCtx) {
    let src = ctx.source.as_bytes();
    if node.kind() == "call_stmt" {
        if let Some(module) = luau_require_module(node, src) {
            let from_id = ctx.scope.last().cloned().unwrap_or_default();
            let line = node.start_position().row as u32 + 1;
            ctx.emit_import(&from_id, &module, line);
            return; // don't recurse inside a require call
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        scan_for_require_luau(&child, ctx);
    }
}

/// Extract all binding names from a `bindinglist` node.
///
/// Returns `Vec<(variable_name, has_anon_fn_rhs)>` where `has_anon_fn_rhs`
/// is true when the corresponding `explist` expression is an `anon_fn`.
///
/// `explist` is optional (e.g. `local x` with no initializer).
fn extract_bindings(
    bindinglist: &tree_sitter::Node,
    explist: Option<&tree_sitter::Node>,
    src: &[u8],
) -> Vec<(String, bool)> {
    // Collect binding variable names in order.
    let mut names: Vec<String> = Vec::new();
    let mut cursor = bindinglist.walk();
    for child in bindinglist.named_children(&mut cursor) {
        if child.kind() == "binding" {
            if let Some(var_name_node) = child.child_by_field_name("variable_name") {
                let name = var_name_node.utf8_text(src).unwrap_or("").to_string();
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
    }

    // Collect expressions in order from explist.
    let mut exprs: Vec<bool> = Vec::new();
    if let Some(el) = explist {
        let mut c2 = el.walk();
        for child in el.named_children(&mut c2) {
            // child.kind() == "exp" — check if its single named child is anon_fn
            let is_anon = is_anon_fn_exp(&child);
            exprs.push(is_anon);
        }
    }

    names
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let is_anon = exprs.get(i).copied().unwrap_or(false);
            (name, is_anon)
        })
        .collect()
}

/// Return true if `exp_node` is an `exp` whose sole named child is `anon_fn`.
fn is_anon_fn_exp(exp_node: &tree_sitter::Node) -> bool {
    if exp_node.kind() == "anon_fn" {
        return true;
    }
    // In this grammar expressions are often wrapped in `exp` nodes.
    if exp_node.kind() == "exp" || exp_node.kind() == "exp_wrap" {
        let mut cursor = exp_node.walk();
        for child in exp_node.named_children(&mut cursor) {
            if child.kind() == "anon_fn" {
                return true;
            }
        }
    }
    false
}

/// Extract the simple callee name from a `var` node (or `name` node).
///
/// For `greet(...)`: invoked is `var { variable_name: name("greet") }` → "greet".
/// For `mod.func(...)`: invoked is `var { table_name: name("mod"), key: ... }` → "func".
fn extract_callee_name(invoked: &tree_sitter::Node, src: &[u8]) -> String {
    if invoked.kind() == "name" {
        return invoked.utf8_text(src).unwrap_or("").to_string();
    }
    if invoked.kind() == "var" {
        // If there's a table_name, the last key child holds the final member name.
        if invoked.child_by_field_name("table_name").is_some() {
            // Walk children for key nodes; take the last one.
            let mut last_key_name = String::new();
            let mut cursor = invoked.walk();
            for child in invoked.named_children(&mut cursor) {
                if child.kind() == "key" {
                    if let Some(fn_node) = child.child_by_field_name("field_name") {
                        last_key_name = fn_node.utf8_text(src).unwrap_or("").to_string();
                    }
                }
            }
            if !last_key_name.is_empty() {
                return last_key_name;
            }
        }
        // Plain variable reference.
        if let Some(vn) = invoked.child_by_field_name("variable_name") {
            return vn.utf8_text(src).unwrap_or("").to_string();
        }
    }
    // Fallback: full text (may include dots; take the last segment).
    let text = invoked.utf8_text(src).unwrap_or("").to_string();
    text.split('.').next_back().unwrap_or(&text).to_string()
}

/// Emit a Method node for a `fn_stmt` table method/function.
///
/// `receiver` = table name text, `method` = method/function name text.
fn emit_method_node(
    node: &tree_sitter::Node,
    receiver: &str,
    method: &str,
    ctx: &mut ExtractCtx,
) {
    if method.is_empty() {
        return;
    }
    let qualified = if receiver.is_empty() {
        method.to_string()
    } else {
        format!("{receiver}::{method}")
    };
    let line = node.start_position().row as u32 + 1;
    let id = generate_node_id(ctx.file_path, &NodeKind::Method, &qualified, line);
    let node_record = Node {
        id: id.clone(),
        name: method.to_string(),
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
}

// ── Extractor impl ────────────────────────────────────────────────────────────

impl LanguageExtractor for LuauExtractor {
    fn config(&self) -> &LanguageConfig {
        &CONFIG
    }

    /// Per-node hook.  Returns `true` (consumed) for all nodes we handle fully
    /// here, preventing the default dispatch from also processing them.
    fn visit_node_hook(&self, node: &tree_sitter::Node, ctx: &mut ExtractCtx) -> bool {
        let src = ctx.source.as_bytes();

        match node.kind() {
            // ── local function ────────────────────────────────────────────────
            // Default dispatch handles `local_fn_stmt` via function_types.
            // No hook needed; return false to let the walker continue.
            "local_fn_stmt" => false,

            // ── global / table function ───────────────────────────────────────
            "fn_stmt" => {
                if let Some(table_name_node) = node.child_by_field_name("table_name") {
                    // Table method or table function.
                    let receiver = table_name_node
                        .utf8_text(src)
                        .unwrap_or("")
                        .to_string();

                    if let Some(method_name_node) = node.child_by_field_name("method_name") {
                        // `function T:m()` — colon syntax.
                        let method = method_name_node
                            .utf8_text(src)
                            .unwrap_or("")
                            .to_string();
                        emit_method_node(node, &receiver, &method, ctx);
                    } else {
                        // `function T.f()` — dot syntax via `key` children.
                        // Walk named children of fn_stmt; key nodes hold field_name.
                        let mut cursor = node.walk();
                        let mut found = false;
                        for child in node.named_children(&mut cursor) {
                            if child.kind() == "key" {
                                if let Some(fn_node) = child.child_by_field_name("field_name") {
                                    let method = fn_node
                                        .utf8_text(src)
                                        .unwrap_or("")
                                        .to_string();
                                    emit_method_node(node, &receiver, &method, ctx);
                                    found = true;
                                    break;
                                }
                            }
                        }
                        if !found {
                            // Fallback: emit with empty method name skipped inside helper.
                        }
                    }
                    true // consumed
                } else {
                    // Plain global function: `function f()` — emit as Function.
                    // Extract name from function_name field.
                    let name = node
                        .child_by_field_name("function_name")
                        .and_then(|n| n.utf8_text(src).ok())
                        .unwrap_or("")
                        .to_string();
                    if !name.is_empty() {
                        ctx.emit_node(NodeKind::Function, &name, node, true, None, None);
                    }
                    true // consumed
                }
            }

            // ── type alias ────────────────────────────────────────────────────
            "type_stmt" => {
                // Detect `export type X = ...` by byte-offset prefix check.
                let start = node.start_byte();
                let end = (start + 7).min(ctx.source.len());
                let is_exported = ctx.source.as_bytes()[start..end] == *b"export ";

                let name = node
                    .child_by_field_name("left")
                    .and_then(|n| n.utf8_text(src).ok())
                    .unwrap_or("")
                    .to_string();

                if !name.is_empty() {
                    ctx.emit_node(NodeKind::Type, &name, node, is_exported, None, None);
                }
                true // consumed
            }

            // ── local variable(s) ─────────────────────────────────────────────
            "local_var_stmt" => {
                // Scan for any require() calls nested inside this statement.
                scan_for_require_luau(node, ctx);

                // Skip variable emission for function-local declarations (TS parity).
                // Anonymous-function bindings inside function bodies are also suppressed
                // (they are local helpers, not top-level callables).
                if is_in_function_scope(ctx) {
                    return true; // consumed — prevent double-emit
                }

                // Find bindinglist and explist children.
                let mut bindinglist: Option<tree_sitter::Node> = None;
                let mut explist: Option<tree_sitter::Node> = None;
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    match child.kind() {
                        "bindinglist" => bindinglist = Some(child),
                        "explist" => explist = Some(child),
                        _ => {}
                    }
                }

                if let Some(bl) = bindinglist {
                    let bindings = extract_bindings(&bl, explist.as_ref(), src);
                    for (name, is_anon_fn) in bindings {
                        let kind = if is_anon_fn {
                            NodeKind::Function
                        } else {
                            NodeKind::Variable
                        };
                        ctx.emit_node(kind, &name, node, false, None, None);
                    }
                }
                true // consumed — prevent double-emit from variable_types
            }

            // ── function call / require ───────────────────────────────────────
            // call_stmt is in call_types, but the default extract_call looks for
            // `function`, `name`, or `callee` fields — none of which exist in
            // the new Luau grammar.  We must handle all call_stmt nodes here.
            "call_stmt" => {
                if let Some(module) = luau_require_module(node, src) {
                    // require() call → emit import ref.
                    let from_id = ctx.scope.last().cloned().unwrap_or_default();
                    let line = node.start_position().row as u32 + 1;
                    ctx.emit_import(&from_id, &module, line);
                } else {
                    // Regular call → emit call ref.
                    // Extract callee name from the `invoked` field (var → variable_name → name).
                    // For method calls, the method_name field holds the name.
                    let callee = if let Some(mn) = node.child_by_field_name("method_name") {
                        mn.utf8_text(src).unwrap_or("").to_string()
                    } else if let Some(inv) = node.child_by_field_name("invoked") {
                        extract_callee_name(&inv, src)
                    } else {
                        String::new()
                    };
                    if !callee.is_empty() {
                        let from_id = ctx.scope.last().cloned().unwrap_or_default();
                        let line = node.start_position().row as u32 + 1;
                        ctx.emit_call(&from_id, &callee, line);
                    }
                }
                true // always consumed — we handle everything above
            }

            _ => false,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(all(test, feature = "wasmtime-grammars"))]
mod tests {
    use crate::wasm_parser::extract_wasm;
    use crate::wasm_grammars::WasmGrammar;
    use codewiki_core::NodeKind;
    use std::io::Write;

    /// Comprehensive extraction test covering all Luau node types.
    ///
    /// Fixture covers: local function, global function, table method (colon),
    /// table function (dot), local variables, anonymous function in local
    /// binding, exported type, non-exported type, require() import, bare
    /// require(), and a regular call.
    #[test]
    fn luau_extract_comprehensive() {
        let source = r#"-- types
type Point = { x: number, y: number }
export type Color = string

-- local function
local function add(a: number, b: number): number
    return a + b
end

-- global function
function greet(name: string): string
    return "Hello " .. name
end

-- table method (colon)
function MyClass:init(value: number)
    self.value = value
end

-- table method (dot)
function MyClass.helper()
    return 42
end

-- local variable
local PI = 3.14159
local greeting: string = "hello"

-- anonymous function in local binding
local double = function(x: number): number
    return x * 2
end

-- require import (in local)
local utils = require("utils.core")

-- bare require
require("init")

-- regular call (for call ref)
greet("world")
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".luau").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_wasm(source, f.path(), WasmGrammar::Luau)
            .expect("Luau WASM extraction should succeed");

        let names: Vec<&str> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        eprintln!("Luau nodes ({}):", batch.nodes.len());
        for n in &batch.nodes {
            eprintln!("  {:?} {:?} exported={}", n.kind, n.name, n.is_exported);
        }
        eprintln!("Luau refs ({}):", batch.unresolved_refs.len());
        for r in &batch.unresolved_refs {
            eprintln!("  {} {:?}", r.reference_kind, r.reference_name);
        }

        // File node exists.
        assert!(
            batch.nodes.iter().any(|n| n.kind == NodeKind::File),
            "no file node; nodes: {names:?}"
        );

        // add — local function, not exported.
        let add = batch.nodes.iter().find(|n| n.name == "add" && n.kind == NodeKind::Function);
        assert!(add.is_some(), "expected Function 'add'; nodes: {names:?}");
        assert!(!add.unwrap().is_exported, "'add' should not be exported");

        // greet — global function, exported.
        let greet = batch.nodes.iter().find(|n| n.name == "greet" && n.kind == NodeKind::Function);
        assert!(greet.is_some(), "expected Function 'greet'; nodes: {names:?}");
        assert!(greet.unwrap().is_exported, "'greet' should be exported");

        // init — table method (colon syntax).
        assert!(
            batch.nodes.iter().any(|n| n.name == "init" && n.kind == NodeKind::Method),
            "expected Method 'init'; nodes: {names:?}"
        );

        // helper — table function (dot syntax).
        assert!(
            batch.nodes.iter().any(|n| n.name == "helper" && n.kind == NodeKind::Method),
            "expected Method 'helper'; nodes: {names:?}"
        );

        // PI — local variable.
        assert!(
            batch.nodes.iter().any(|n| n.name == "PI" && n.kind == NodeKind::Variable),
            "expected Variable 'PI'; nodes: {names:?}"
        );

        // greeting — local variable.
        assert!(
            batch.nodes.iter().any(|n| n.name == "greeting" && n.kind == NodeKind::Variable),
            "expected Variable 'greeting'; nodes: {names:?}"
        );

        // double — anonymous function in local binding.
        let double = batch.nodes.iter().find(|n| n.name == "double" && n.kind == NodeKind::Function);
        assert!(double.is_some(), "expected Function 'double'; nodes: {names:?}");
        assert!(!double.unwrap().is_exported, "'double' should not be exported");

        // Point — type alias, not exported.
        let point = batch.nodes.iter().find(|n| n.name == "Point" && n.kind == NodeKind::Type);
        assert!(point.is_some(), "expected Type 'Point'; nodes: {names:?}");
        assert!(!point.unwrap().is_exported, "'Point' should not be exported");

        // Color — exported type alias.
        let color = batch.nodes.iter().find(|n| n.name == "Color" && n.kind == NodeKind::Type);
        assert!(color.is_some(), "expected Type 'Color'; nodes: {names:?}");
        assert!(color.unwrap().is_exported, "'Color' should be exported");

        // utils.core — import ref from require("utils.core").
        let import_utils = batch
            .unresolved_refs
            .iter()
            .any(|r| r.reference_kind == "imports" && r.reference_name == "utils.core");
        assert!(
            import_utils,
            "expected import ref for 'utils.core'; refs: {:?}",
            batch.unresolved_refs.iter().map(|r| (&r.reference_kind, &r.reference_name)).collect::<Vec<_>>()
        );

        // init — import ref from bare require("init").
        let import_init = batch
            .unresolved_refs
            .iter()
            .any(|r| r.reference_kind == "imports" && r.reference_name == "init");
        assert!(
            import_init,
            "expected import ref for 'init'; refs: {:?}",
            batch.unresolved_refs.iter().map(|r| (&r.reference_kind, &r.reference_name)).collect::<Vec<_>>()
        );

        // greet call ref.
        let call_greet = batch
            .unresolved_refs
            .iter()
            .any(|r| r.reference_kind == "calls" && r.reference_name == "greet");
        assert!(
            call_greet,
            "expected call ref for 'greet'; refs: {:?}",
            batch.unresolved_refs.iter().map(|r| (&r.reference_kind, &r.reference_name)).collect::<Vec<_>>()
        );

        // At least 10 non-file nodes.
        let non_file = batch.nodes.iter().filter(|n| n.kind != NodeKind::File).count();
        assert!(
            non_file >= 10,
            "expected >= 10 non-file nodes, got {non_file}; nodes: {names:?}"
        );
    }
}
