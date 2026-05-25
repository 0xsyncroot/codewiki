//! T-411 — `codewiki_node` tool handler.
//!
//! Get detailed info about ONE symbol.

use crate::input_limits::validate_query;
use crate::tools::{search::format_kind, truncate_str, CONTAINER_NODE_KINDS, MAX_OUTPUT_LENGTH};
use codewiki_core::CodeWikiError;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::sync::Arc;

/// Soft cap on a leaf node's body before we append a "+N more lines" pointer.
const LEAF_BODY_CAP: usize = 2_500;
/// Cap on a container's member outline.
const CONTAINER_OUTLINE_CAP: usize = 1_500;
/// Cap on one-hop neighbor lists folded in via includeCallers/includeCallees.
const NEIGHBOR_CAP: usize = 10;

#[tracing::instrument(skip(handle), fields(symbol_len = %symbol.len(), include_code))]
pub async fn handle_node(
    handle: Arc<dyn QueryHandle>,
    symbol: String,
    include_code: bool,
    include_callers: bool,
    include_callees: bool,
) -> Result<String, CodeWikiError> {
    validate_query(&symbol)?;

    let matches = handle.search_nodes(
        &symbol,
        SearchOptions {
            limit: 1,
            ..Default::default()
        },
    )?;

    let sr = match matches.into_iter().next() {
        Some(r) => r,
        None => return Ok(format!("No symbol found matching '{symbol}'.")),
    };

    let node = sr.node;
    let kind_str = format_kind(&node.kind);
    let is_container = CONTAINER_NODE_KINDS.contains(&kind_str);

    let root = handle.root_path();
    let root_ref = root.as_deref();

    let mut out = format!("## {} `{}`\n\n", kind_str, node.name);
    out.push_str(&crate::tools::root_header(root_ref));
    out.push_str(&format!(
        "**File:** `{}:{}`\n",
        crate::tools::rel(&node.file_path, root_ref),
        node.start_line
    ));

    // Lead with signature + docstring (cheap, high-signal) before any body.
    if let Some(sig) = &node.signature {
        if !sig.is_empty() {
            out.push_str(&format!("**Signature:** `{sig}`\n"));
        }
    }

    if let Some(doc) = &node.docstring {
        if !doc.is_empty() {
            out.push_str(&format!("\n**Documentation:**\n{doc}\n"));
        }
    }

    // Note: visibility info is stored in metadata if available
    if let Some(meta) = &node.metadata {
        if let Ok(meta_val) = serde_json::from_str::<serde_json::Value>(meta) {
            if let Some(vis) = meta_val.get("visibility").and_then(|v| v.as_str()) {
                out.push_str(&format!("**Visibility:** {vis}\n"));
            }
        }
    }

    if include_code {
        if is_container {
            // Return outline: child nodes' names + signatures + lines
            out.push_str("\n**Members (outline):**\n\n");
            if let Ok(Some(code)) = handle.get_code(&node.id) {
                out.push_str(&format!(
                    "```\n{}\n```\n",
                    cap_source(&code, CONTAINER_OUTLINE_CAP)
                ));
            } else {
                out.push_str("(source not available)\n");
            }
        } else {
            // Return source for leaf nodes (softened cap + line-count pointer).
            if let Ok(Some(code)) = handle.get_code(&node.id) {
                out.push_str(&format!(
                    "\n**Source:**\n\n```\n{}\n```\n",
                    cap_source(&code, LEAF_BODY_CAP)
                ));
            } else {
                out.push_str("\n(source not available)\n");
            }
        }
    }

    // Fold in one-hop neighbors, reusing the shared callers/callees renderer.
    if include_callers {
        let mut nodes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (n, _edge) in handle.get_callers(&node.id, 1)? {
            if seen.insert(n.id.clone()) {
                nodes.push(n);
            }
        }
        out.push_str("\n**Callers (1-hop):**\n\n");
        if nodes.is_empty() {
            out.push_str("(none found for this match — use codewiki_impact for full reach)\n");
        } else {
            out.push_str(&crate::tools::render_neighbor_list(
                &nodes,
                NEIGHBOR_CAP,
                root_ref,
            ));
        }
    }

    if include_callees {
        let mut nodes = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (n, _edge) in handle.get_callees(&node.id, 1)? {
            if seen.insert(n.id.clone()) {
                nodes.push(n);
            }
        }
        out.push_str("\n**Callees (1-hop):**\n\n");
        if nodes.is_empty() {
            out.push_str("(none)\n");
        } else {
            out.push_str(&crate::tools::render_neighbor_list(
                &nodes,
                NEIGHBOR_CAP,
                root_ref,
            ));
        }
    }

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}

/// Cap source at `max` bytes (char-boundary-safe), appending a pointer to the
/// number of additional lines that were elided.
fn cap_source(code: &str, max: usize) -> String {
    if code.len() <= max {
        return code.to_string();
    }
    let head = truncate_str(code, max);
    let shown_lines = head.lines().count();
    let total_lines = code.lines().count();
    let more = total_lines.saturating_sub(shown_lines);
    format!("{head}\n...(+{more} more lines — codewiki_explore for full source)")
}
