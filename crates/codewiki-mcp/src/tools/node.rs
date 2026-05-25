//! T-411 — `codewiki_node` tool handler.
//!
//! Get detailed info about ONE symbol.

use crate::input_limits::validate_query;
use crate::tools::{search::format_kind, CONTAINER_NODE_KINDS, MAX_OUTPUT_LENGTH};
use codewiki_core::CodeWikiError;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::sync::Arc;

#[tracing::instrument(skip(handle), fields(symbol_len = %symbol.len(), include_code))]
pub async fn handle_node(
    handle: Arc<dyn QueryHandle>,
    symbol: String,
    include_code: bool,
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

    let mut out = format!("## {} `{}`\n\n", kind_str, node.name);
    out.push_str(&format!("**File:** `{}:{}`\n", node.file_path, node.start_line));

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
            // Get outgoing contains edges to find child nodes
            // For now, we use the code block and extract it
            if let Ok(Some(code)) = handle.get_code(&node.id) {
                let truncated = if code.len() > 1500 {
                    format!("{}\n...(truncated)", &code[..1500])
                } else {
                    code.clone()
                };
                out.push_str(&format!("```\n{truncated}\n```\n"));
            } else {
                out.push_str("(source not available)\n");
            }
        } else {
            // Return full source for leaf nodes
            if let Ok(Some(code)) = handle.get_code(&node.id) {
                let truncated = if code.len() > 5000 {
                    format!("{}\n...(truncated)", &code[..5000])
                } else {
                    code
                };
                out.push_str(&format!("\n**Source:**\n\n```\n{truncated}\n```\n"));
            } else {
                out.push_str("\n(source not available)\n");
            }
        }
    }

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}
