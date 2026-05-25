//! T-409 — `codewiki_callers` tool handler.
//!
//! Find all functions/methods that call a specific symbol.

use crate::input_limits::validate_query;
use crate::tools::{search::format_kind, MAX_OUTPUT_LENGTH};
use codewiki_core::CodeWikiError;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::collections::HashSet;
use std::sync::Arc;

#[tracing::instrument(skip(handle), fields(symbol_len = %symbol.len(), limit))]
pub async fn handle_callers(
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
    let mut caller_nodes = Vec::new();

    for sr in &matches {
        let callers = handle.get_callers(&sr.node.id, 1)?;
        for (node, _edge) in callers {
            if seen_ids.insert(node.id.clone()) {
                caller_nodes.push(node);
            }
        }
    }

    if caller_nodes.is_empty() {
        return Ok(format!("No callers found for '{symbol}'."));
    }

    caller_nodes.truncate(limit);

    let mut out = format!("## Callers of '{}'\n\n", symbol);
    for (i, node) in caller_nodes.iter().enumerate() {
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
