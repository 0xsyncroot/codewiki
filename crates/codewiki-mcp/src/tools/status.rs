//! T-413 — `codewiki_status` tool handler.
//!
//! Get index statistics: file count, node count, edge count, DB size, etc.

use codewiki_core::CodeWikiError;
use codewiki_storage::QueryHandle;
use std::sync::Arc;

#[tracing::instrument(skip(handle))]
pub async fn handle_status(
    handle: Arc<dyn QueryHandle>,
) -> Result<String, CodeWikiError> {
    let stats = handle.get_stats()?;

    let db_size_mb = stats.db_size_bytes as f64 / (1024.0 * 1024.0);

    let mut out = format!(
        "## CodeWiki Index Status\n\n\
         - **Files:** {}\n\
         - **Nodes:** {}\n\
         - **Edges:** {}\n\
         - **Unresolved refs:** {}\n\
         - **DB size:** {:.2} MB\n\
         - **Journal mode:** {}\n\n",
        stats.file_count,
        stats.node_count,
        stats.edge_count,
        stats.unresolved_ref_count,
        db_size_mb,
        stats.journal_mode,
    );

    if !stats.nodes_by_kind.is_empty() {
        out.push_str("### Nodes by Kind\n\n");
        let mut kinds: Vec<_> = stats.nodes_by_kind.iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(a.1));
        for (kind, count) in kinds {
            out.push_str(&format!("- {kind}: {count}\n"));
        }
        out.push('\n');
    }

    if !stats.files_by_language.is_empty() {
        out.push_str("### Files by Language\n\n");
        let mut langs: Vec<_> = stats.files_by_language.iter().collect();
        langs.sort_by(|a, b| b.1.cmp(a.1));
        for (lang, count) in langs {
            out.push_str(&format!("- {lang}: {count}\n"));
        }
        out.push('\n');
    }

    Ok(out)
}
