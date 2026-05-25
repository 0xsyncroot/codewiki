//! Blazor / Razor Components extractor.
//!
//! Extracts component, route, inject, using, and component-usage nodes from
//! `.razor` files without a tree-sitter grammar (none is stable/maintained).
//!
//! Follows the same pattern as `svelte.rs`: regex-based special extractor that
//! delegates `@code { }` / `@functions { }` blocks to the existing C# tree-sitter
//! extractor via a virtual `*.__codewiki__.cs` path.
//!
//! Design reference: gaps/DESIGN-B-blazor.md

use crate::ast_walker::{extract_file, generate_node_id, timestamps_for_file, LanguageConfig, LanguageExtractor};
// Re-use the EMPTY_CONFIG defined in svelte.rs to avoid duplication.
use crate::languages::svelte::EMPTY_CONFIG;
use codewiki_core::{EdgeKind, ExtractionBatch, FileRecord, Language, Node, NodeKind, UnresolvedRef};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

// ─── Stub extractor struct ────────────────────────────────────────────────────

pub struct RazorExtractor;

impl LanguageExtractor for RazorExtractor {
    fn config(&self) -> &LanguageConfig {
        &EMPTY_CONFIG
    }
}

// ─── Compiled regexes (OnceLock — compiled exactly once) ─────────────────────

fn page_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)^@page\s+"([^"]+)""#).unwrap()
    })
}

/// Captures the interior of an `@code { }` or `@functions { }` block.
/// Uses a lazy quantifier terminated by `\n}`.
///
/// Limitation: only handles single-level brace nesting reliably (covers 95%+
/// of real Blazor files). Deeply nested braces may terminate the match early.
fn code_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?s)@(?:code|functions)\s*\{(.*?)\n\}").unwrap()
    })
}

fn inject_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)^@inject\s+([A-Za-z_][A-Za-z0-9_<>, ]*)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
    })
}

fn using_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)^@using\s+([A-Za-z_][A-Za-z0-9_.]*)").unwrap()
    })
}

/// Matches self-closing PascalCase component tags: `<MyFoo ... />` or `<MyFoo/>`.
fn component_self_closing_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"<([A-Z][A-Za-z0-9]*)(?:\s[^>]*)?\s*/>").unwrap()
    })
}

/// Matches opening (non-self-closing) PascalCase component tags: `<MyFoo ...>` or `<MyFoo>`.
/// Closing tags are intentionally ignored; we deduplicate by name anyway.
///
/// NOTE: Rust's `regex` crate does not support backreferences (`\2`). We use
/// two separate regexes — one for self-closing and one for opening tags — and
/// deduplicate by component name, as required by B-MUST-3.
fn component_open_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"<([A-Z][A-Za-z0-9]*)(?:\s[^>]*)?>").unwrap()
    })
}

// ─── Blazor built-in component exclusion list ─────────────────────────────────

/// Component names that are part of the Blazor framework and should NOT be
/// emitted as unresolved component references (they won't exist as user files).
static BLAZOR_BUILTINS: &[&str] = &[
    "CascadingValue",
    "AuthorizeView",
    "Virtualize",
    "DynamicComponent",
    "HeadContent",
    "HeadOutlet",
    "PageTitle",
    "SectionContent",
    "SectionOutlet",
    "Router",
    "RouteView",
    "FocusOnNavigate",
    "NavLink",
    "NavMenu",
    "EditForm",
    "ValidationSummary",
    "ValidationMessage",
    "InputText",
    "InputTextArea",
    "InputNumber",
    "InputDate",
    "InputSelect",
    "InputCheckbox",
    "InputFile",
    "InputRadio",
    "InputRadioGroup",
];

fn is_builtin(name: &str) -> bool {
    BLAZOR_BUILTINS.contains(&name)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Count 1-indexed line number for `byte_offset` within `source`.
fn line_of(source: &str, byte_offset: usize) -> u32 {
    source[..byte_offset].lines().count() as u32 + 1
}

// ─── Main extractor ───────────────────────────────────────────────────────────

/// Special extraction for Blazor Razor Component files.
/// Called by `mod.rs::get_extractor_and_use_special` for `Language::Razor`.
pub fn extract_special(source: &str, file_path: &Path) -> ExtractionBatch {
    let file_path_str = file_path.to_string_lossy().to_string();
    let component_name = file_path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("Component")
        .to_string();

    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<codewiki_core::Edge> = Vec::new();
    let mut unresolved: Vec<UnresolvedRef> = Vec::new();

    let line_count = source.lines().count() as u32;

    // ── Step 0: File node ─────────────────────────────────────────────────────
    let file_id = format!("file:{file_path_str}");
    nodes.push(Node {
        id: file_id.clone(),
        name: file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string(),
        qualified_name: file_path_str.clone(),
        kind: NodeKind::File,
        language: Language::Razor,
        file_path: file_path_str.clone(),
        start_line: 1,
        end_line: line_count.max(1),
        start_col: 0,
        end_col: 0,
        is_exported: false,
        signature: None,
        docstring: None,
        metadata: None,
    });

    // ── Step 1: Component node (file-as-component) ────────────────────────────
    let component_id =
        generate_node_id(&file_path_str, &NodeKind::Component, &component_name, 1);
    nodes.push(Node {
        id: component_id.clone(),
        name: component_name.clone(),
        qualified_name: component_name.clone(),
        kind: NodeKind::Component,
        language: Language::Razor,
        file_path: file_path_str.clone(),
        start_line: 1,
        end_line: line_count.max(1),
        start_col: 0,
        end_col: 0,
        is_exported: true,
        signature: None,
        docstring: None,
        metadata: None,
    });
    edges.push(codewiki_core::Edge {
        id: format!("{file_id}->{component_id}-contains"),
        source_id: file_id.clone(),
        target_id: component_id.clone(),
        kind: EdgeKind::Contains,
        line: Some(1),
        col: None,
        provenance: Some("extraction".into()),
        confidence: Some(1.0),
        metadata: None,
    });

    // ── Step 2: @page directives → Route nodes ────────────────────────────────
    for cap in page_re().captures_iter(source) {
        let m = cap.get(0).unwrap();
        let path_str = cap.get(1).unwrap().as_str();
        let line = line_of(source, m.start());

        let route_name = format!("GET {path_str}");
        let route_qualified = format!("{file_path_str}::GET:{path_str}");
        let route_id = format!("route:{file_path_str}:{line}:GET:{path_str}");

        nodes.push(Node {
            id: route_id.clone(),
            name: route_name.clone(),
            qualified_name: route_qualified,
            kind: NodeKind::Route,
            language: Language::Razor,
            file_path: file_path_str.clone(),
            start_line: line,
            end_line: line,
            start_col: 0,
            end_col: 0,
            is_exported: true,
            signature: None,
            docstring: None,
            metadata: None,
        });
        edges.push(codewiki_core::Edge {
            id: format!("{component_id}->{route_id}-contains"),
            source_id: component_id.clone(),
            target_id: route_id.clone(),
            kind: EdgeKind::Contains,
            line: Some(line),
            col: None,
            provenance: Some("extraction".into()),
            confidence: Some(1.0),
            metadata: None,
        });
        // Emit a "renders" ref so the resolver can link route → component.
        let ref_id = format!("{route_id}->renders->{component_name}");
        unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: route_id,
            reference_name: component_name.clone(),
            reference_kind: "renders".into(),
            file_path: file_path_str.clone(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }

    // ── Step 3: @code / @functions blocks — delegate to C# extractor ──────────
    for cap in code_block_re().captures_iter(source) {
        let body_match = cap.get(1).unwrap();
        let code_body = body_match.as_str();

        // Number of complete lines before the opening `{` of the block.
        // source[..body_match.start()] includes everything up to (but not
        // including) the first character of the captured group, i.e. the
        // character immediately after `@code {`. So lines().count() gives us
        // the 0-indexed line of that character, which is the line offset we add
        // to the sub-extractor's 1-indexed line numbers to map back to the
        // .razor file's 1-indexed line space.
        let code_start_line = source[..body_match.start()].lines().count() as u32;

        // Virtual path: ends in .cs so detect_language maps it to CSharp.
        // The `__codewiki__` infix avoids collision with real `.razor.cs`
        // code-behind files.
        let stem = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Component");
        let virtual_name = format!("{stem}.__codewiki__.cs");
        let virtual_path = file_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(&virtual_name);

        // The C# extractor requires methods/fields to be inside a class scope
        // (is_in_class_scope returns true only when a Class/Struct/Component
        // node is in scope). Wrap the @code body in a minimal class wrapper so
        // tree-sitter sees valid C# and method_declaration nodes are emitted.
        //
        // The wrapper header `class __RazorCodeBlock__ {` is line 1 of the
        // synthetic source; code_body follows starting from its own newline.
        // This shifts every code_body line by +1 in the sub-extractor's output,
        // so we subtract 1 from the offset: razor_line = sub_line - 1 + code_start_line.
        // Implemented as: node.start_line += (code_start_line - 1).
        let wrapped = format!("class __RazorCodeBlock__ {{{code_body}\n}}");
        let wrapper_offset = if code_start_line > 0 { code_start_line - 1 } else { 0 };

        let sub_batch = extract_file(&virtual_path, &wrapped);
        let virtual_file_id = format!("file:{}", virtual_path.to_string_lossy());

        // Collect IDs added so far plus the ones we're about to add so that
        // edge-FK pruning works correctly in a single pass.
        let mut added_ids: HashSet<String> =
            nodes.iter().map(|n| n.id.clone()).collect();

        for mut node in sub_batch.nodes {
            if matches!(node.kind, NodeKind::File) {
                continue; // Skip the virtual file node.
            }
            // Skip the synthetic wrapper class node.
            if node.name == "__RazorCodeBlock__" {
                continue;
            }
            // Rewrite to the real .razor file.
            node.file_path = file_path_str.clone();
            // Offset line numbers back to .razor coordinates.
            // sub_line - 1 + code_start_line = sub_line + wrapper_offset
            node.start_line += wrapper_offset;
            node.end_line += wrapper_offset;
            node.language = Language::Razor;

            // Contains edge from component to each C# symbol.
            let edge_id = format!("{}->{}-contains", component_id, node.id);
            edges.push(codewiki_core::Edge {
                id: edge_id,
                source_id: component_id.clone(),
                target_id: node.id.clone(),
                kind: EdgeKind::Contains,
                line: Some(node.start_line),
                col: None,
                provenance: Some("extraction".into()),
                confidence: Some(1.0),
                metadata: None,
            });
            added_ids.insert(node.id.clone());
            nodes.push(node);
        }

        // Forward edges, dropping any that reference the skipped virtual file node
        // or have dangling source/target IDs.
        for edge in sub_batch.edges {
            if edge.source_id == virtual_file_id || edge.target_id == virtual_file_id {
                continue;
            }
            if !added_ids.contains(&edge.source_id) || !added_ids.contains(&edge.target_id) {
                continue;
            }
            edges.push(edge);
        }

        // Forward unresolved refs with corrected line offsets.
        for mut uref in sub_batch.unresolved_refs {
            uref.file_path = file_path_str.clone();
            if let Some(line) = uref.line.as_mut() {
                *line += wrapper_offset;
            }
            unresolved.push(uref);
        }
    }

    // ── Step 4: @inject directives ─────────────────────────────────────────────
    for cap in inject_re().captures_iter(source) {
        let m = cap.get(0).unwrap();
        let identifier = cap.get(2).unwrap().as_str();
        let line = line_of(source, m.start());
        let ref_id = format!("{component_id}->uses->{identifier}@{line}");
        unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: component_id.clone(),
            reference_name: identifier.to_string(),
            reference_kind: "uses".into(),
            file_path: file_path_str.clone(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }

    // ── Step 5: @using directives ─────────────────────────────────────────────
    for cap in using_re().captures_iter(source) {
        let m = cap.get(0).unwrap();
        let namespace = cap.get(1).unwrap().as_str();
        let line = line_of(source, m.start());
        let ref_id = format!("{component_id}->imports->{namespace}@{line}");
        unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: component_id.clone(),
            reference_name: namespace.to_string(),
            reference_kind: "imports".into(),
            file_path: file_path_str.clone(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }

    // ── Step 6: Component usage tags ─────────────────────────────────────────
    // Collect PascalCase component names from BOTH self-closing and open tags,
    // deduplicate by name, skip the file's own component and framework builtins.
    // B-MUST-3: NO backreferences. Two separate regexes; dedup by name.
    let mut seen_components: HashSet<String> = HashSet::new();
    let mut component_refs: Vec<(String, u32)> = Vec::new(); // (name, first_line)

    for cap in component_self_closing_re().captures_iter(source) {
        let name = cap.get(1).unwrap().as_str().to_string();
        if name == component_name || is_builtin(&name) {
            continue;
        }
        if seen_components.insert(name.clone()) {
            let line = line_of(source, cap.get(0).unwrap().start());
            component_refs.push((name, line));
        }
    }
    for cap in component_open_re().captures_iter(source) {
        let name = cap.get(1).unwrap().as_str().to_string();
        if name == component_name || is_builtin(&name) {
            continue;
        }
        if seen_components.insert(name.clone()) {
            let line = line_of(source, cap.get(0).unwrap().start());
            component_refs.push((name, line));
        }
    }

    for (name, line) in component_refs {
        let ref_id = format!("{component_id}->renders->{name}@{line}");
        unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: component_id.clone(),
            reference_name: name,
            reference_kind: "renders".into(),
            file_path: file_path_str.clone(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }

    // ── Step 7: Finalise batch ────────────────────────────────────────────────
    let content_hash = {
        let mut h = Sha256::new();
        h.update(source.as_bytes());
        hex::encode(h.finalize())
    };
    let (modified_at, indexed_at) = timestamps_for_file(file_path);

    ExtractionBatch {
        file: FileRecord {
            path: file_path.to_path_buf(),
            content_hash,
            language: "Razor".to_string(),
            size: source.len() as u64,
            modified_at,
            indexed_at,
            node_count: nodes.len() as u32,
            errors: Vec::new(),
        },
        nodes,
        edges,
        unresolved_refs: unresolved,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Fixture: a realistic Blazor component with @page, @using, @inject,
    /// component tags, and a @code block containing methods and fields.
    fn fixture_source() -> &'static str {
        r#"@page "/counter"
@page "/counter/{start:int}"
@using MyApp.Services
@inject IWeatherService WeatherService
@inject NavigationManager Nav

<h1>Counter</h1>
<MyAlert Message="@message" />
<AnotherComponent />

@code {
    private int currentCount = 0;

    public void IncrementCount()
    {
        currentCount++;
    }

    private string message = "hello";
}
"#
    }

    fn make_tmp_path(name: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .suffix(".razor")
            .tempfile()
            .unwrap();
        f.write_all(fixture_source().as_bytes()).unwrap();
        f
    }

    #[test]
    fn razor_emits_component_node() {
        let f = make_tmp_path("Counter");
        let batch = extract_special(fixture_source(), f.path());

        assert!(
            batch.nodes.iter().any(|n| matches!(n.kind, NodeKind::File)),
            "no file node; nodes: {:?}",
            batch.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        assert!(
            batch.nodes.iter().any(|n| matches!(n.kind, NodeKind::Component)),
            "no component node"
        );
    }

    #[test]
    fn razor_emits_route_nodes() {
        let f = make_tmp_path("Counter");
        let batch = extract_special(fixture_source(), f.path());

        let routes: Vec<_> = batch
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Route))
            .collect();

        assert_eq!(routes.len(), 2, "expected 2 route nodes; got {:?}", routes.iter().map(|r| &r.name).collect::<Vec<_>>());

        let names: Vec<&str> = routes.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"GET /counter"), "missing GET /counter");
        assert!(
            names.contains(&"GET /counter/{start:int}"),
            "missing GET /counter/{{start:int}}"
        );

        // qualified_name format
        let file_str = f.path().to_string_lossy();
        for route in &routes {
            assert!(
                route.language == Language::Razor,
                "route language should be Razor"
            );
            assert!(
                route.qualified_name.contains("::GET:"),
                "qualified_name format unexpected: {}",
                route.qualified_name
            );
        }
        let _ = file_str;
    }

    #[test]
    fn razor_delegates_code_block_to_csharp() {
        let f = make_tmp_path("Counter");
        let batch = extract_special(fixture_source(), f.path());

        // The @code block contains IncrementCount (method) and currentCount / message (fields/variables).
        assert!(
            batch
                .nodes
                .iter()
                .any(|n| n.name == "IncrementCount" && matches!(n.kind, NodeKind::Method | NodeKind::Function)),
            "no IncrementCount method; nodes: {:?}",
            batch.nodes.iter().map(|n| (&n.name, &n.kind)).collect::<Vec<_>>()
        );

        // All delegated nodes must reference the real .razor path, not the virtual .cs path.
        let file_str = f.path().to_string_lossy().to_string();
        for node in &batch.nodes {
            assert!(
                !node.file_path.contains("__codewiki__"),
                "node {:?} still has virtual path: {}",
                node.name,
                node.file_path
            );
            if node.name == "IncrementCount" {
                assert_eq!(node.file_path, file_str, "wrong file_path on IncrementCount");
                // IncrementCount must be on or after the @code line (line 11 in fixture).
                assert!(
                    node.start_line >= 11,
                    "IncrementCount start_line {} should be >= 11",
                    node.start_line
                );
            }
        }
    }

    #[test]
    fn razor_emits_inject_unresolved_refs() {
        let f = make_tmp_path("Counter");
        let batch = extract_special(fixture_source(), f.path());

        let inject_refs: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "uses")
            .collect();

        assert_eq!(inject_refs.len(), 2, "expected 2 @inject refs; got {:?}", inject_refs.iter().map(|r| &r.reference_name).collect::<Vec<_>>());

        let names: Vec<&str> = inject_refs.iter().map(|r| r.reference_name.as_str()).collect();
        assert!(names.contains(&"WeatherService"), "missing WeatherService");
        assert!(names.contains(&"Nav"), "missing Nav");
    }

    #[test]
    fn razor_emits_using_unresolved_refs() {
        let f = make_tmp_path("Counter");
        let batch = extract_special(fixture_source(), f.path());

        assert!(
            batch
                .unresolved_refs
                .iter()
                .any(|r| r.reference_kind == "imports" && r.reference_name == "MyApp.Services"),
            "missing @using MyApp.Services ref"
        );
    }

    #[test]
    fn razor_emits_component_ref_for_pascal_tags() {
        let f = make_tmp_path("Counter");
        let batch = extract_special(fixture_source(), f.path());

        let renders: Vec<_> = batch
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == "renders")
            .collect();

        let names: Vec<&str> = renders.iter().map(|r| r.reference_name.as_str()).collect();

        assert!(names.contains(&"MyAlert"), "missing renders ref for MyAlert");
        assert!(
            names.contains(&"AnotherComponent"),
            "missing renders ref for AnotherComponent"
        );

        // lowercase HTML tags must NOT produce renders refs
        assert!(
            !names.contains(&"h1"),
            "h1 should not produce a renders ref"
        );
    }

    #[test]
    fn razor_no_dangling_edges() {
        let f = make_tmp_path("Counter");
        let batch = extract_special(fixture_source(), f.path());

        let node_ids: std::collections::HashSet<&str> =
            batch.nodes.iter().map(|n| n.id.as_str()).collect();

        for edge in &batch.edges {
            assert!(
                node_ids.contains(edge.source_id.as_str()),
                "edge source_id {:?} not in nodes",
                edge.source_id
            );
            assert!(
                node_ids.contains(edge.target_id.as_str()),
                "edge target_id {:?} not in nodes",
                edge.target_id
            );
        }
    }

    #[test]
    fn razor_codebehind_stays_csharp() {
        // .razor.cs files have final extension .cs → detected as CSharp, never processed by razor extractor.
        use crate::language_detector::detect_language;
        assert_eq!(
            detect_language(std::path::Path::new("Counter.razor.cs")),
            Some(Language::CSharp),
            "Counter.razor.cs should be CSharp (final extension is .cs)"
        );
    }
}
