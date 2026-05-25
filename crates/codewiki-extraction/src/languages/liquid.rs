//! T-222 — Liquid template extractor (regex-based).

use crate::ast_walker::{generate_node_id, LanguageConfig, LanguageExtractor};
use crate::languages::svelte::EMPTY_CONFIG;
use codewiki_core::{EdgeKind, ExtractionBatch, FileRecord, Language, Node, NodeKind, UnresolvedRef};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::OnceLock;

pub struct LiquidExtractor;

fn render_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\{%-?\s*(?:render|include)\s+['"]([^'"]+)['"]\s*"#).unwrap())
}

fn section_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\{%-?\s*section\s+['"]([^'"]+)['"]\s*"#).unwrap())
}

fn assign_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{%-?\s*assign\s+(\w+)\s*=").unwrap())
}

fn line_of(source: &str, byte_offset: usize) -> usize {
    source[..byte_offset].chars().filter(|&c| c == '\n').count() + 1
}

/// Special extraction for Liquid template files — regex-based, no tree-sitter.
pub fn extract_special(source: &str, file_path: &Path) -> ExtractionBatch {
    let file_path_str = file_path.to_string_lossy().to_string();
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
        language: Language::Liquid,
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

    // render/include → unresolved references
    for cap in render_re().captures_iter(source) {
        let name = cap[1].to_string();
        let line = line_of(source, cap.get(0).unwrap().start()) as u32;
        let ref_id = generate_node_id(
            &file_path_str,
            &NodeKind::Component,
            &format!("ref:{name}@{line}"),
            line,
        );
        unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: file_id.clone(),
            reference_name: name,
            reference_kind: "references".to_string(),
            file_path: file_path_str.clone(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }

    // section → unresolved references
    for cap in section_re().captures_iter(source) {
        let name = cap[1].to_string();
        let line = line_of(source, cap.get(0).unwrap().start()) as u32;
        let ref_id = generate_node_id(
            &file_path_str,
            &NodeKind::Component,
            &format!("section:{name}@{line}"),
            line,
        );
        unresolved.push(UnresolvedRef {
            id: ref_id,
            from_node_id: file_id.clone(),
            reference_name: name,
            reference_kind: "references".to_string(),
            file_path: file_path_str.clone(),
            line: Some(line),
            col: None,
            metadata: None,
        });
    }

    // assign → variable nodes
    for cap in assign_re().captures_iter(source) {
        let name = cap[1].to_string();
        let line = line_of(source, cap.get(0).unwrap().start()) as u32 + 1;
        let node_id = generate_node_id(&file_path_str, &NodeKind::Variable, &name, line);
        nodes.push(Node {
            id: node_id.clone(),
            name: name.clone(),
            qualified_name: name,
            kind: NodeKind::Variable,
            language: Language::Liquid,
            file_path: file_path_str.clone(),
            start_line: line,
            end_line: line,
            start_col: 0,
            end_col: 0,
            is_exported: false,
            signature: None,
            docstring: None,
            metadata: None,
        });
        edges.push(codewiki_core::Edge {
            id: format!("{file_id}->{node_id}-contains"),
            source_id: file_id.clone(),
            target_id: node_id,
            kind: EdgeKind::Contains,
            line: Some(line),
            col: None,
            provenance: Some("extraction".into()),
            confidence: Some(1.0),
            metadata: None,
        });
    }

    let content_hash = {
        let mut h = Sha256::new();
        h.update(source.as_bytes());
        hex::encode(h.finalize())
    };

    ExtractionBatch {
        file: FileRecord {
            path: file_path.to_path_buf(),
            content_hash,
            language: "Liquid".to_string(),
            size: source.len() as u64,
            modified_at: 0,
            indexed_at: 0,
            node_count: nodes.len() as u32,
            errors: Vec::new(),
        },
        nodes,
        edges,
        unresolved_refs: unresolved,
    }
}

impl LanguageExtractor for LiquidExtractor {
    fn config(&self) -> &LanguageConfig {
        &EMPTY_CONFIG
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn liquid_render_and_assign() {
        let source =
            "{% render 'header' %}\n{% assign title = 'Hello' %}\n{% section 'footer' %}\n";
        let mut f = tempfile::NamedTempFile::with_suffix(".liquid").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_special(source, f.path());

        // File node + variable node
        assert!(batch.nodes.iter().any(|n| matches!(n.kind, NodeKind::File)));
        assert!(
            batch.nodes.iter().any(|n| n.name == "title"),
            "expected title variable"
        );
        // Unresolved refs for render and section
        assert!(batch.unresolved_refs.iter().any(|r| r.reference_name == "header"));
        assert!(batch.unresolved_refs.iter().any(|r| r.reference_name == "footer"));
    }
}
