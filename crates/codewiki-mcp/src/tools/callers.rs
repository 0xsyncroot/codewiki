//! T-409 — `codewiki_callers` tool handler.
//!
//! Find all functions/methods that call a specific symbol.
//!
//! Overloaded-name aggregation: a bare symbol name is resolved to its
//! same-name family (all definitions sharing the simple name) and their
//! callers are unioned, deduped, ranked by degree, and capped. A
//! fully-qualified symbol (e.g. `Foo::bar`) resolves to exactly that node.

use crate::input_limits::validate_query;
use crate::tools::MAX_OUTPUT_LENGTH;
use codewiki_core::CodeWikiError;
use codewiki_storage::QueryHandle;
use std::sync::Arc;

/// Upper bound on aggregated neighbors returned to the agent. Keeps the
/// per-call token cost bounded even for highly-connected same-name families.
const NEIGHBOR_CAP: usize = 80;

#[tracing::instrument(skip(handle), fields(symbol_len = %symbol.len(), limit))]
pub async fn handle_callers(
    handle: Arc<dyn QueryHandle>,
    symbol: String,
    limit: usize,
) -> Result<String, CodeWikiError> {
    validate_query(&symbol)?;

    let limit = limit.clamp(1, 100);

    let agg = handle.get_callers_aggregated(&symbol, NEIGHBOR_CAP)?;

    if agg.resolved_name.is_empty() {
        return Ok(format!("No symbol found matching '{symbol}'."));
    }

    if agg.neighbors.is_empty() {
        return Ok(format!("No callers found for '{symbol}'."));
    }

    let root = handle.root_path();
    let mut out = format!("## Callers of '{symbol}'\n\n");
    out.push_str(&crate::tools::root_header(root.as_deref()));
    if agg.family_size > 1 {
        out.push_str(&format!(
            "Aggregated across {} definitions named `{}`.\n\n",
            agg.family_size, agg.resolved_name,
        ));
    }
    out.push_str(&crate::tools::render_neighbor_list(
        &agg.neighbors,
        limit,
        root.as_deref(),
    ));

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}
