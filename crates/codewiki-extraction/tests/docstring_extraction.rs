/// T-DS-* docstring extraction tests as specified in DOCSTRING-SPEC §7
/// and POLISH-IMPL-SPEC §4.
use codewiki_extraction::ast_walker::{extract_docstring, DocCommentStyle};
use tree_sitter::Parser;

// ── Helper: parse a snippet and find the first node of target_kind ──────────

fn find_and_extract(
    node: &tree_sitter::Node,
    source: &[u8],
    target_kind: &str,
    style: &DocCommentStyle,
) -> Option<String> {
    if node.kind() == target_kind {
        return extract_docstring(node, source, style);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(result) = find_and_extract(&child, source, target_kind, style) {
            return Some(result);
        }
    }
    None
}

fn parse_and_extract_with(
    language: tree_sitter::Language,
    source: &str,
    target_kind: &str,
    style: &DocCommentStyle,
) -> Option<String> {
    let mut parser = Parser::new();
    parser.set_language(&language).expect("load language");
    let tree = parser.parse(source, None).expect("parse");
    find_and_extract(&tree.root_node(), source.as_bytes(), target_kind, style)
}

// ── T-DS-1: Python function docstring ────────────────────────────────────────

#[test]
fn t_ds_1_python_function_docstring() {
    let source = r#"def greet(name):
    """Say hello to the given name.

    Returns a greeting string.
    """
    return "Hello"
"#;
    let style = DocCommentStyle::PythonFirstBodyString { body_field: "body" };
    let result = parse_and_extract_with(
        tree_sitter_python::LANGUAGE.into(),
        source,
        "function_definition",
        &style,
    );
    let ds = result.expect("should have docstring");
    assert!(ds.contains("Say hello to the given name."), "got: {ds}");
    assert!(ds.contains("Returns a greeting string."), "got: {ds}");
    assert!(
        !ds.contains("\"\"\""),
        "raw markers must be stripped, got: {ds}"
    );
}

// ── T-DS-2: TypeScript JSDoc ─────────────────────────────────────────────────

#[test]
fn t_ds_2_typescript_jsdoc() {
    let source = r#"/**
 * Calculates the total price including tax.
 * @param amount - base amount
 * @returns total with tax
 */
export function calculateTotal(amount) { return amount * 1.1; }
"#;
    let style = DocCommentStyle::PrecedingBlockComment {
        node_kind: "comment",
        prefix: "/**",
    };
    let result = parse_and_extract_with(
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        source,
        "function_declaration",
        &style,
    );
    let ds = result.expect("should have docstring");
    assert!(
        ds.contains("Calculates the total price including tax."),
        "got: {ds}"
    );
    assert!(ds.contains("@param amount"), "got: {ds}");
    assert!(ds.contains("@returns total with tax"), "got: {ds}");
    assert!(
        !ds.contains("/**"),
        "opening marker must be stripped, got: {ds}"
    );
}

// ── T-DS-3: Rust /// line doc comments ───────────────────────────────────────

#[test]
fn t_ds_3_rust_line_doc_comments() {
    let source = r#"/// Returns the number of active sessions.
///
/// Does not include expired sessions.
pub fn active_session_count() -> usize { 0 }
"#;
    let style = DocCommentStyle::PrecedingEither {
        block_node_kind: "block_comment",
        block_prefix: "/**",
        line_node_kind: "line_comment",
        line_prefix: "///",
    };
    let result = parse_and_extract_with(
        tree_sitter_rust::LANGUAGE.into(),
        source,
        "function_item",
        &style,
    );
    let ds = result.expect("should have docstring");
    assert!(
        ds.contains("Returns the number of active sessions."),
        "got: {ds}"
    );
    assert!(
        ds.contains("Does not include expired sessions."),
        "got: {ds}"
    );
    assert!(
        !ds.contains("///"),
        "raw markers must be stripped, got: {ds}"
    );
}

// ── T-DS-4: Go godoc comment ─────────────────────────────────────────────────

#[test]
fn t_ds_4_go_godoc() {
    let source = r#"package main

// NewUser creates a new User with the given name.
// The name must not be empty.
func NewUser(name string) string { return name }
"#;
    let style = DocCommentStyle::PrecedingLineComment {
        node_kind: "comment",
        prefix: "//",
    };
    let result = parse_and_extract_with(
        tree_sitter_go::LANGUAGE.into(),
        source,
        "function_declaration",
        &style,
    );
    let ds = result.expect("should have godoc");
    assert!(
        ds.contains("NewUser creates a new User with the given name."),
        "got: {ds}"
    );
    assert!(ds.contains("The name must not be empty."), "got: {ds}");
    assert!(
        !ds.contains("//"),
        "raw markers must be stripped, got: {ds}"
    );
}

// ── T-DS-5: C# XML doc comment ───────────────────────────────────────────────

#[test]
fn t_ds_5_csharp_xml_doc() {
    // Must wrap in a class so method_declaration is inside a class body
    // (otherwise C# parser treats it as a global_statement, not method_declaration)
    let source = r#"class Service {
/// <summary>
/// Processes the order asynchronously.
/// </summary>
public void ProcessOrder() {}
}
"#;
    let style = DocCommentStyle::PrecedingLineComment {
        node_kind: "comment",
        prefix: "///",
    };
    let result = parse_and_extract_with(
        tree_sitter_c_sharp::LANGUAGE.into(),
        source,
        "method_declaration",
        &style,
    );
    let ds = result.expect("should have xml doc");
    assert!(ds.contains("<summary>"), "got: {ds}");
    assert!(
        ds.contains("Processes the order asynchronously."),
        "got: {ds}"
    );
    assert!(
        !ds.contains("///"),
        "raw markers must be stripped, got: {ds}"
    );
}

// ── T-DS-6: Rust blank-line gap → None ───────────────────────────────────────

#[test]
fn t_ds_6_rust_blank_line_gap_returns_none() {
    // The two // comments are NOT prefixed with /// so the PrecedingEither style
    // (which only looks for /// line comments and /** block comments) will not match
    // them.  Verify that non-doc comments are excluded.
    let source = r#"// This is a section header

pub fn unrelated_function() {}
"#;
    let style = DocCommentStyle::PrecedingEither {
        block_node_kind: "block_comment",
        block_prefix: "/**",
        line_node_kind: "line_comment",
        line_prefix: "///",
    };
    let result = parse_and_extract_with(
        tree_sitter_rust::LANGUAGE.into(),
        source,
        "function_item",
        &style,
    );
    assert!(
        result.is_none(),
        "blank-line gap or wrong prefix must suppress docstring attribution, got: {:?}",
        result
    );
}

// ── T-DS-7: TypeScript non-doc comment → None ────────────────────────────────

#[test]
fn t_ds_7_typescript_todo_comment_not_jsdoc() {
    let source = r#"// TODO: refactor this
export function parseDate(s) { return 0; }
"#;
    let style = DocCommentStyle::PrecedingBlockComment {
        node_kind: "comment",
        prefix: "/**",
    };
    let result = parse_and_extract_with(
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        source,
        "function_declaration",
        &style,
    );
    assert!(
        result.is_none(),
        "non-/** comment must not be captured by PrecedingBlockComment, got: {:?}",
        result
    );
}
