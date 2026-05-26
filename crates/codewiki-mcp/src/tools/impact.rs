//! T-410 — `codewiki_impact` tool handler.
//!
//! Analyze the impact radius of changing a symbol.
//!
//! Overloaded-name aggregation: a bare symbol name is resolved to its
//! same-name family (all definitions sharing the simple name) and their
//! reverse-reach impact subgraphs are unioned (deduped by node id / edge),
//! capped at a bounded node budget. A fully-qualified symbol (e.g. `Foo::bar`)
//! resolves to exactly that node.

use crate::input_limits::validate_query;
use crate::tools::{search::format_kind, MAX_OUTPUT_LENGTH};
use codewiki_core::CodeWikiError;
use codewiki_storage::QueryHandle;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Upper bound on affected nodes returned to the agent. Keeps the per-call
/// token cost bounded even for highly-connected same-name families.
const IMPACT_NODE_CAP: usize = 100;

#[tracing::instrument(skip(handle), fields(symbol_len = %symbol.len(), depth))]
pub async fn handle_impact(
    handle: Arc<dyn QueryHandle>,
    symbol: String,
    depth: usize,
) -> Result<String, CodeWikiError> {
    validate_query(&symbol)?;

    let depth = depth.clamp(1, 10);

    let agg = handle.get_impact_aggregated(&symbol, depth, IMPACT_NODE_CAP)?;

    if agg.resolved_name.is_empty() {
        return Ok(format!("No symbol found matching '{symbol}'."));
    }

    let all_nodes = &agg.subgraph.nodes;
    let all_edges = &agg.subgraph.edges;

    if all_nodes.is_empty() {
        return Ok(format!(
            "Symbol '{symbol}' has no dependents within {depth} hop(s)."
        ));
    }

    let root = handle.root_path();
    let root_ref = root.as_deref();

    let mut out = format!("## Impact Radius of '{symbol}' (depth {depth})\n\n");
    out.push_str(&crate::tools::root_header(root_ref));
    if agg.family_size > 1 {
        out.push_str(&format!(
            "Aggregated across {} definitions named `{}`.\n\n",
            agg.family_size, agg.resolved_name,
        ));
    }
    out.push_str(&format!(
        "**{} affected symbols** across {} files, {} edges\n\n",
        all_nodes.len(),
        count_unique_files(all_nodes),
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
        out.push_str(&format!("### `{}`\n\n", crate::tools::rel(file, root_ref)));
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
