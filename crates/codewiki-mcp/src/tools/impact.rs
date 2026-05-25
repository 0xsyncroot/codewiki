//! T-410 — `codewiki_impact` tool handler.
//!
//! Analyze the impact radius of changing a symbol.

use crate::input_limits::validate_query;
use crate::tools::{search::format_kind, MAX_OUTPUT_LENGTH};
use codewiki_core::CodeWikiError;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tracing::instrument(skip(handle), fields(symbol_len = %symbol.len(), depth))]
pub async fn handle_impact(
    handle: Arc<dyn QueryHandle>,
    symbol: String,
    depth: usize,
) -> Result<String, CodeWikiError> {
    validate_query(&symbol)?;

    let depth = depth.clamp(1, 10);

    // Resolve symbol name → node ids via search
    let matches = handle.search_nodes(
        &symbol,
        SearchOptions {
            limit: 5,
            ..Default::default()
        },
    )?;

    if matches.is_empty() {
        return Ok(format!("No symbol found matching '{symbol}'."));
    }

    // Merge subgraphs from all matches
    let mut all_nodes: HashMap<String, codewiki_core::Node> = HashMap::new();
    let mut all_edges: Vec<codewiki_core::Edge> = Vec::new();
    let mut seen_edges: HashSet<String> = HashSet::new();

    for sr in &matches {
        let subgraph = handle.get_impact_radius(&sr.node.id, depth)?;
        for (id, node) in subgraph.nodes {
            all_nodes.insert(id, node);
        }
        for edge in subgraph.edges {
            let edge_key = format!("{}->{}", edge.source_id, edge.target_id);
            if seen_edges.insert(edge_key) {
                all_edges.push(edge);
            }
        }
    }

    if all_nodes.is_empty() {
        return Ok(format!(
            "Symbol '{symbol}' has no dependents within {depth} hop(s)."
        ));
    }

    let mut out = format!("## Impact Radius of '{}' (depth {})\n\n", symbol, depth);
    out.push_str(&format!(
        "**{} affected symbols** across {} files, {} edges\n\n",
        all_nodes.len(),
        count_unique_files(&all_nodes),
        all_edges.len(),
    ));

    // Group affected symbols by file
    let mut by_file: HashMap<String, Vec<&codewiki_core::Node>> = HashMap::new();
    for node in all_nodes.values() {
        by_file
            .entry(node.file_path.clone())
            .or_default()
            .push(node);
    }

    let mut files: Vec<_> = by_file.keys().cloned().collect();
    files.sort();

    for file in &files {
        out.push_str(&format!("### `{file}`\n\n"));
        let mut nodes = by_file[file].clone();
        nodes.sort_by_key(|n| n.start_line);
        for node in nodes {
            out.push_str(&format!(
                "- **{}** ({}) line {}\n",
                node.name,
                format_kind(&node.kind),
                node.start_line,
            ));
        }
        out.push('\n');
    }

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}

fn count_unique_files(nodes: &HashMap<String, codewiki_core::Node>) -> usize {
    let paths: HashSet<_> = nodes.values().map(|n| &n.file_path).collect();
    paths.len()
}
