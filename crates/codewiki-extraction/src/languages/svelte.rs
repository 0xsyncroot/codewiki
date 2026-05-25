//! T-222 — Svelte component extractor.
//!
//! Extracts the component node, delegates `<script>` blocks to the TS/JS
//! extractor, offsets line numbers back to .svelte positions, and emits
//! `contains` edges from the component to all contained symbols.

use crate::ast_walker::{extract_file, generate_node_id, timestamps_for_file, DocCommentStyle, LanguageConfig, LanguageExtractor};
use codewiki_core::{EdgeKind, ExtractionBatch, FileRecord, Language, Node, NodeKind, UnresolvedRef};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::OnceLock;

pub struct SvelteExtractor;

pub static EMPTY_CONFIG: LanguageConfig = LanguageConfig {
    function_types: &[],
    class_types: &[],
    method_types: &[],
    interface_types: &[],
    struct_types: &[],
    enum_types: &[],
    enum_member_types: &[],
    type_alias_types: &[],
    import_types: &[],
    call_types: &[],
    variable_types: &[],
    property_types: &[],
    field_types: &[],
    extra_class_types: &[],
    namespace_types: &[],
    name_field: "name",
    body_field: "body",
    methods_are_top_level: false,
    doc_comment_style: DocCommentStyle::None,
};

fn script_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)<script([^>]*)>([\s\S]*?)</script>").unwrap())
}

fn svelte_runes() -> &'static [&'static str] {
    &["$props", "$state", "$derived", "$effect", "$inspect", "$host"]
}

fn is_rune(name: &str) -> bool {
    svelte_runes().iter().any(|r| name == *r || name.starts_with(*r))
}

/// Special extraction for Svelte files — does not use tree-sitter directly.
pub fn extract_special(source: &str, file_path: &Path) -> ExtractionBatch {
    let file_path_str = file_path.to_string_lossy().to_string();
    let component_name = file_path
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("Component");

    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<codewiki_core::Edge> = Vec::new();
    let mut unresolved: Vec<UnresolvedRef> = Vec::new();

    let file_id = format!("file:{file_path_str}");
    let line_count = source.lines().count() as u32;

    nodes.push(Node {
        id: file_id.clone(),
        name: file_path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string(),
        qualified_name: file_path_str.clone(),
        kind: NodeKind::File,
        language: Language::Svelte,
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

    let component_id = generate_node_id(&file_path_str, &NodeKind::Component, component_name, 1);
    nodes.push(Node {
        id: component_id.clone(),
        name: component_name.to_string(),
        qualified_name: component_name.to_string(),
        kind: NodeKind::Component,
        language: Language::Svelte,
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

    // Extract <script> blocks and delegate to TS/JS extractor.
    let re = script_re();
    for cap in re.captures_iter(source) {
        let attrs = &cap[1];
        let script_content = &cap[2];
        let script_start = source[..cap.get(2).unwrap().start()].lines().count() as u32;

        // Detect language from lang="ts" attribute.
        let use_ts = attrs.contains("lang=\"ts\"") || attrs.contains("lang='ts'");
        let ext = if use_ts { ".ts" } else { ".js" };

        // Create a temp virtual path for the sub-extractor.
        let virtual_path = file_path.with_extension(ext.trim_start_matches('.'));
        let sub_batch = extract_file(&virtual_path, script_content);

        // Offset line numbers and re-parent to component.
        for mut node in sub_batch.nodes {
            if matches!(node.kind, NodeKind::File) {
                continue; // Don't re-emit the virtual file node.
            }
            node.file_path = file_path_str.clone();
            node.start_line += script_start;
            node.end_line += script_start;
            node.language = Language::Svelte;

            // Emit contains edge from component.
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
            nodes.push(node);
        }

        // Keep only edges whose source and target are in the actual node set
        // (the virtual file node was skipped, so drop edges referencing it).
        let virtual_file_id = format!("file:{}", virtual_path.to_string_lossy());
        let node_ids: std::collections::HashSet<&str> =
            nodes.iter().map(|n| n.id.as_str()).collect();
        for edge in sub_batch.edges {
            if edge.source_id == virtual_file_id || edge.target_id == virtual_file_id {
                continue; // drop edges referencing the skipped virtual file node
            }
            if !node_ids.contains(edge.source_id.as_str())
                || !node_ids.contains(edge.target_id.as_str())
            {
                continue; // drop dangling edges
            }
            edges.push(edge);
        }

        // Filter unresolved refs — skip Svelte runes, and re-parent any ref
        // whose from_node_id is the virtual file node to the component node.
        // The virtual file node is not in the final batch, so its id as
        // from_node_id would violate the FK on unresolved_refs.from_node_id.
        for mut uref in sub_batch.unresolved_refs {
            if is_rune(&uref.reference_name) {
                continue;
            }
            uref.file_path = file_path_str.clone();
            if let Some(line) = uref.line.as_mut() {
                *line += script_start;
            }
            if uref.from_node_id == virtual_file_id {
                // Re-parent to the component node so the FK is satisfied.
                uref.from_node_id = component_id.clone();
            }
            unresolved.push(uref);
        }
    }

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
            language: "Svelte".to_string(),
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

impl LanguageExtractor for SvelteExtractor {
    fn config(&self) -> &LanguageConfig {
        &EMPTY_CONFIG
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn svelte_component_node_emitted() {
        let source = r#"
<script lang="ts">
  export function greet(name: string) { return `Hi ${name}`; }
</script>
<h1>Hello</h1>
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".svelte").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_special(source, f.path());

        let names: Vec<_> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            batch.nodes.iter().any(|n| matches!(n.kind, NodeKind::Component)),
            "no component node; nodes: {names:?}"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "greet"),
            "no greet function; nodes: {names:?}"
        );
    }

    /// Bug 2 fix: edges from a <script> block must not reference the virtual
    /// file node (which is skipped), or any other node not in the final batch.
    /// This used to cause a FK constraint violation in SQLite.
    #[test]
    fn svelte_script_block_indexes_without_fk_crash() {
        let source = r#"
<script lang="ts">
  import { something } from 'other';
  export function greet(name: string) { return `Hi ${name}`; }
</script>
<p>Hi</p>
"#;
        let mut f = tempfile::NamedTempFile::with_suffix(".svelte").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_special(source, f.path());

        // Collect all node IDs in the batch.
        let node_ids: std::collections::HashSet<&str> =
            batch.nodes.iter().map(|n| n.id.as_str()).collect();

        // Every edge source and target must reference a node that actually exists.
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

        // The component node and greet function must be present.
        assert!(
            batch.nodes.iter().any(|n| matches!(n.kind, NodeKind::Component)),
            "no component node"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "greet"),
            "no greet function"
        );
    }
}
