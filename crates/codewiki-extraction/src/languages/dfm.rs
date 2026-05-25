//! T-222 — Delphi form file (.dfm/.fmx) extractor (regex-based).

use crate::ast_walker::{generate_node_id, LanguageConfig, LanguageExtractor};
use crate::languages::svelte::EMPTY_CONFIG;
use codewiki_core::{
    EdgeKind, ExtractionBatch, FileRecord, Language, Node, NodeKind, UnresolvedRef,
};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::OnceLock;

pub struct DfmExtractor;

fn object_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)^\s*(?:object|inherited|inline)\s+(\w+)\s*:\s*(\w+)").unwrap()
    })
}

fn event_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*On\w+\s*=\s*(\w+)").unwrap())
}

fn end_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^\s*end\s*$").unwrap())
}

/// Special extraction for Delphi form files — regex-based, no tree-sitter.
pub fn extract_special(source: &str, file_path: &Path) -> ExtractionBatch {
    let file_path_str = file_path.to_string_lossy().to_string();
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<codewiki_core::Edge> = Vec::new();
    let mut unresolved: Vec<UnresolvedRef> = Vec::new();

    let file_id = format!("file:{file_path_str}");
    let line_count = source.lines().count() as u32;

    nodes.push(Node {
        id: file_id.clone(),
        name: file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string(),
        qualified_name: file_path_str.clone(),
        kind: NodeKind::File,
        language: Language::Dfm,
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

    // Stack of node_id values representing the containment hierarchy.
    let mut stack: Vec<String> = vec![file_id.clone()];
    let obj_re = object_re();
    let ev_re = event_re();
    let end_r = end_re();

    for (line_idx, line) in source.lines().enumerate() {
        let line_no = line_idx as u32 + 1;

        if let Some(cap) = obj_re.captures(line) {
            let comp_name = cap[1].to_string();
            let comp_type = cap[2].to_string();
            let node_id =
                generate_node_id(&file_path_str, &NodeKind::Component, &comp_name, line_no);

            nodes.push(Node {
                id: node_id.clone(),
                name: comp_name.clone(),
                qualified_name: comp_name,
                kind: NodeKind::Component,
                language: Language::Dfm,
                file_path: file_path_str.clone(),
                start_line: line_no,
                end_line: line_no, // Approximate; updated on 'end'
                start_col: 0,
                end_col: 0,
                is_exported: false,
                signature: Some(comp_type),
                docstring: None,
                metadata: None,
            });

            if let Some(parent_id) = stack.last() {
                edges.push(codewiki_core::Edge {
                    id: format!("{parent_id}->{node_id}-contains"),
                    source_id: parent_id.clone(),
                    target_id: node_id.clone(),
                    kind: EdgeKind::Contains,
                    line: Some(line_no),
                    col: None,
                    provenance: Some("extraction".into()),
                    confidence: Some(1.0),
                    metadata: None,
                });
            }
            stack.push(node_id);
        } else if end_r.is_match(line) {
            if stack.len() > 1 {
                stack.pop();
            }
        } else if let Some(cap) = ev_re.captures(line) {
            let handler = cap[1].to_string();
            let from_id = stack.last().cloned().unwrap_or_else(|| file_id.clone());
            let ref_id = generate_node_id(
                &file_path_str,
                &NodeKind::Method,
                &format!("event:{handler}@{line_no}"),
                line_no,
            );
            unresolved.push(UnresolvedRef {
                id: ref_id,
                from_node_id: from_id,
                reference_name: handler,
                reference_kind: "references".to_string(),
                file_path: file_path_str.clone(),
                line: Some(line_no),
                col: None,
                metadata: None,
            });
        }
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
            language: "Dfm".to_string(),
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

impl LanguageExtractor for DfmExtractor {
    fn config(&self) -> &LanguageConfig {
        &EMPTY_CONFIG
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn dfm_hierarchy_and_events() {
        let source = "\
object Form1 : TForm
  object Button1 : TButton
    OnClick = Button1Click
  end
end
";
        let mut f = tempfile::NamedTempFile::with_suffix(".dfm").unwrap();
        f.write_all(source.as_bytes()).unwrap();

        let batch = extract_special(source, f.path());

        let names: Vec<_> = batch.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            batch.nodes.iter().any(|n| n.name == "Form1"),
            "nodes: {names:?}"
        );
        assert!(
            batch.nodes.iter().any(|n| n.name == "Button1"),
            "nodes: {names:?}"
        );
        assert!(batch
            .unresolved_refs
            .iter()
            .any(|r| r.reference_name == "Button1Click"));
    }
}
