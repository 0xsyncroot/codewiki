//! T-409 — `codewiki_callees` tool handler.
//!
//! Find all functions/methods that a specific symbol calls.

use crate::input_limits::validate_query;
use crate::tools::{search::format_kind, MAX_OUTPUT_LENGTH};
use codewiki_core::CodeWikiError;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::collections::HashSet;
use std::sync::Arc;

#[tracing::instrument(skip(handle), fields(symbol_len = %symbol.len(), limit))]
pub async fn handle_callees(
    handle: Arc<dyn QueryHandle>,
    symbol: String,
    limit: usize,
) -> Result<String, CodeWikiError> {
    validate_query(&symbol)?;

    let limit = limit.clamp(1, 100);

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

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut callee_nodes = Vec::new();

    for sr in &matches {
        let callees = handle.get_callees(&sr.node.id, 1)?;
        for (node, _edge) in callees {
            if seen_ids.insert(node.id.clone()) {
                callee_nodes.push(node);
            }
        }
    }

    if callee_nodes.is_empty() {
        return Ok(format!("No callees found for '{symbol}'."));
    }

    callee_nodes.truncate(limit);

    let mut out = format!("## Callees of '{}'\n\n", symbol);
    for (i, node) in callee_nodes.iter().enumerate() {
        out.push_str(&format!(
            "{}. **{}** ({}) in `{}:{}`\n",
            i + 1,
            node.name,
            format_kind(&node.kind),
            node.file_path,
            node.start_line,
        ));
    }

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}
