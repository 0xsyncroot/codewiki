//! T-408 — `codewiki_context` tool handler.
//!
//! PRIMARY TOOL — composes search + graph expansion + code snippets in one call.
//!
//! Wave-3 optimisations implemented here:
//! - OPT-12: summary line above Entry Points (N symbols, F files, E relationships)
//! - OPT-13: code blocks for up to 5 non-container non-entry functions/methods
//! - OPT-15: ### Related Files section listing unique file paths not in entry files

use crate::input_limits::validate_query;
use crate::tools::{search::format_kind, MAX_OUTPUT_LENGTH};
use codewiki_core::{CodeWikiError, NodeKind};
use codewiki_storage::{FindOpts, QueryHandle};
use std::sync::Arc;

/// Keywords that suggest this is a feature-request query.
const FEATURE_KEYWORDS: &[&str] = &[
    "add", "create", "implement", "build", "enable", "allow", "new", "support", "want",
];

/// Keywords that disqualify a query from the feature-request reminder.
const EXCLUDE_KEYWORDS: &[&str] = &[
    "fix", "bug", "error", "broken", "crash", "how does", "find",
];

/// Container node kinds — don't include their full code in context output.
const CONTAINER_KINDS: &[NodeKind] = &[
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Enum,
    NodeKind::Namespace,
    NodeKind::Module,
];

#[tracing::instrument(skip(handle), fields(task_len = %task.len(), max_nodes, include_code))]
pub async fn handle_context(
    handle: Arc<dyn QueryHandle>,
    task: String,
    max_nodes: usize,
    include_code: bool,
) -> Result<String, CodeWikiError> {
    validate_query(&task)?;

    let opts = FindOpts {
        search_limit: 3,
        traversal_depth: 1,
        max_nodes,
        min_score: 0.3,
    };

    let subgraph = handle.find_relevant_context(&task, opts)?;

    let mut out = format!("## Context for: {task}\n\n");

    if subgraph.nodes.is_empty() {
        out.push_str("No relevant symbols found in the index for this task.\n");
        out.push_str(
            "\nTip: Make sure the project is indexed with `codewiki index`.\n",
        );
        return Ok(out);
    }

    // Collect entry points (roots) and all nodes
    let entry_ids: std::collections::HashSet<_> = subgraph.roots.iter().collect();
    let mut entry_nodes = Vec::new();
    let mut other_nodes = Vec::new();

    for (id, node) in &subgraph.nodes {
        if entry_ids.contains(id) {
            entry_nodes.push(node.clone());
        } else {
            other_nodes.push(node.clone());
        }
    }

    // Sort by name for deterministic output
    entry_nodes.sort_by(|a, b| a.name.cmp(&b.name));
    other_nodes.sort_by(|a, b| a.name.cmp(&b.name));

    // OPT-12: summary line — "Found N symbols across F files. Key entry points: A, B, C. E relationships."
    {
        let total_nodes = subgraph.nodes.len();
        let total_files: std::collections::HashSet<&str> = subgraph
            .nodes
            .values()
            .map(|n| n.file_path.as_str())
            .collect();
        let total_edges = subgraph.edges.len();
        let entry_names: Vec<&str> = entry_nodes
            .iter()
            .take(3)
            .map(|n| n.name.as_str())
            .collect();
        out.push_str(&format!(
            "Found {} symbols across {} files. Key entry points: {}. {} relationships.\n\n",
            total_nodes,
            total_files.len(),
            if entry_names.is_empty() { "none".to_string() } else { entry_names.join(", ") },
            total_edges,
        ));
    }

    // Entry points section
    if !entry_nodes.is_empty() {
        out.push_str("### Entry Points\n\n");
        for node in &entry_nodes {
            out.push_str(&format!(
                "- **{}** ({}) in `{}:{}` \n",
                node.name,
                format_kind(&node.kind),
                node.file_path,
                node.start_line,
            ));
            if let Some(sig) = &node.signature {
                if !sig.is_empty() {
                    out.push_str(&format!("  Signature: `{sig}`\n"));
                }
            }
            if let Some(doc) = &node.docstring {
                if !doc.is_empty() {
                    let first_line = doc.lines().next().unwrap_or("").trim();
                    out.push_str(&format!("  Doc: {first_line}\n"));
                }
            }
        }
        out.push('\n');
    }

    // Related symbols section
    if !other_nodes.is_empty() {
        out.push_str("### Related Symbols\n\n");
        for node in other_nodes.iter().take(max_nodes.saturating_sub(entry_nodes.len())) {
            out.push_str(&format!(
                "- **{}** ({}) in `{}:{}`\n",
                node.name,
                format_kind(&node.kind),
                node.file_path,
                node.start_line,
            ));
        }
        out.push('\n');
    }

    // OPT-15: Related Files section — unique files in the subgraph, excluding entry-point files
    {
        let entry_files: std::collections::HashSet<&str> = entry_nodes
            .iter()
            .map(|n| n.file_path.as_str())
            .collect();
        let mut related_files: Vec<&str> = subgraph
            .nodes
            .values()
            .map(|n| n.file_path.as_str())
            .filter(|fp| !entry_files.contains(fp))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        related_files.sort_unstable();
        related_files.truncate(5);
        if !related_files.is_empty() {
            out.push_str("### Related Files\n\n");
            for fp in &related_files {
                out.push_str(&format!("- `{fp}`\n"));
            }
            out.push('\n');
        }
    }

    // Edges summary
    if !subgraph.edges.is_empty() {
        let call_count = subgraph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, codewiki_core::EdgeKind::Calls))
            .count();
        let import_count = subgraph
            .edges
            .iter()
            .filter(|e| matches!(e.kind, codewiki_core::EdgeKind::Imports))
            .count();
        if call_count > 0 || import_count > 0 {
            out.push_str("### Relationships\n\n");
            if call_count > 0 {
                out.push_str(&format!("- {call_count} call edges\n"));
            }
            if import_count > 0 {
                out.push_str(&format!("- {import_count} import edges\n"));
            }
            out.push('\n');
        }
    }

    // Code blocks for top-K entry nodes (not containers)
    if include_code {
        let entry_code_nodes: Vec<_> = entry_nodes
            .iter()
            .filter(|n| !CONTAINER_KINDS.contains(&n.kind))
            .take(5)
            .collect();

        // Count how many entry code blocks we'll emit so we know the budget for OPT-13
        let entry_code_count = entry_code_nodes.len();

        if !entry_code_nodes.is_empty() {
            out.push_str("### Key Code\n\n");
            for node in &entry_code_nodes {
                if let Ok(Some(code)) = handle.get_code(&node.id) {
                    let truncated = if code.len() > 1500 {
                        format!("{}\n...(truncated)", &code[..1500])
                    } else {
                        code
                    };
                    out.push_str(&format!(
                        "#### `{}` ({})\n\n```\n{}\n```\n\n",
                        node.name,
                        node.file_path,
                        truncated
                    ));
                }
            }
        }

        // OPT-13: second pass — emit code for non-entry non-container Function/Method nodes
        // from other_nodes, up to (5 - entry_code_count) blocks, capped at 800 chars each.
        let second_pass_budget = 5usize.saturating_sub(entry_code_count);
        if second_pass_budget > 0 {
            let non_entry_code_nodes: Vec<_> = other_nodes
                .iter()
                .filter(|n| {
                    !CONTAINER_KINDS.contains(&n.kind)
                        && matches!(n.kind, NodeKind::Function | NodeKind::Method)
                })
                .take(second_pass_budget)
                .collect();

            if !non_entry_code_nodes.is_empty() {
                // Open the Key Code section if it wasn't opened by entry nodes
                if entry_code_count == 0 {
                    out.push_str("### Key Code\n\n");
                }
                for node in non_entry_code_nodes {
                    if let Ok(Some(code)) = handle.get_code(&node.id) {
                        let truncated = if code.len() > 800 {
                            format!("{}\n...(truncated)", &code[..800])
                        } else {
                            code
                        };
                        out.push_str(&format!(
                            "#### `{}` ({})\n\n```\n{}\n```\n\n",
                            node.name,
                            node.file_path,
                            truncated
                        ));
                    }
                }
            }
        }
    }

    // Feature-request reminder heuristic
    let task_lower = task.to_lowercase();
    let is_excluded = EXCLUDE_KEYWORDS.iter().any(|kw| task_lower.contains(kw));
    if !is_excluded && FEATURE_KEYWORDS.iter().any(|kw| task_lower.contains(kw)) {
        out.push_str(
            "\n> **Note:** This looks like a feature request. Codewiki provides CODE \
             context — make sure to clarify UX requirements, edge cases, and acceptance \
             criteria with the user before implementing.\n",
        );
    }

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}
