//! T-412 — `codewiki_explore` tool handler.
//!
//! Deep multi-symbol exploration with adaptive output budget.
//! Returns source for several related symbols grouped by file plus a
//! relationship map, in one capped call.

use crate::input_limits::validate_query;
use crate::tools::search::format_kind;
use codewiki_core::{CodeWikiError, EdgeKind, Node};
use codewiki_storage::{FindOpts, QueryHandle};
use std::collections::HashMap;
use std::sync::Arc;

// ── Adaptive output budget (T-412) ──────────────────────────────────────────

/// Adaptive output budget for `codewiki_explore`, scaled to project size.
#[derive(Debug, Clone)]
pub struct ExploreOutputBudget {
    /// Hard cap on total output characters.
    pub max_output_chars: usize,
    /// Default `maxFiles` when the caller didn't specify one.
    pub default_max_files: usize,
    /// Cap on contiguous source returned per file (across all its clusters).
    pub max_chars_per_file: usize,
    /// Cluster gap threshold in lines — tighter clustering on small projects.
    pub gap_threshold: usize,
    /// Max symbols listed in the per-file header.
    pub max_symbols_in_file_header: usize,
    /// Max edges shown per relationship kind.
    pub max_edges_per_relationship_kind: usize,
    /// Include the "Relationships" section.
    pub include_relationships: bool,
    /// Include the "Additional relevant files (not shown)" trailing list.
    pub include_additional_files: bool,
    /// Include the completeness signal.
    pub include_completeness_signal: bool,
    /// Include the explore-budget reminder at the end.
    pub include_budget_note: bool,
}

/// Compute the adaptive output budget based on the number of indexed files.
///
/// T-412 tier breakpoints (from A3-mcp.md §3.7):
/// - `<500`         → per_file_budget = 3_800, total = 18_000
/// - `500–4999`     → per_file_budget = 2_500, total = 13_000
/// - `5000–14999`   → per_file_budget = 7_000, total = 35_000
/// - `≥15000`       → per_file_budget = 7_000, total = 38_000
///
/// The plan also specifies: `<500 → unlimited` for per_file_budget.
/// The TS source uses concrete values (3_800) for <500 files; we follow the
/// TS source here.
pub fn get_explore_output_budget(file_count: u64) -> ExploreOutputBudget {
    if file_count < 500 {
        ExploreOutputBudget {
            max_output_chars: 18_000,
            default_max_files: 5,
            max_chars_per_file: 3_800,
            gap_threshold: 8,
            max_symbols_in_file_header: 6,
            max_edges_per_relationship_kind: 6,
            include_relationships: true,
            include_additional_files: false,
            include_completeness_signal: false,
            include_budget_note: false,
        }
    } else if file_count < 5_000 {
        ExploreOutputBudget {
            max_output_chars: 13_000,
            default_max_files: 6,
            max_chars_per_file: 2_500,
            gap_threshold: 10,
            max_symbols_in_file_header: 8,
            max_edges_per_relationship_kind: 8,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
        }
    } else if file_count < 15_000 {
        ExploreOutputBudget {
            max_output_chars: 35_000,
            default_max_files: 12,
            max_chars_per_file: 7_000,
            gap_threshold: 15,
            max_symbols_in_file_header: 15,
            max_edges_per_relationship_kind: 15,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
        }
    } else {
        ExploreOutputBudget {
            max_output_chars: 38_000,
            default_max_files: 14,
            max_chars_per_file: 7_000,
            gap_threshold: 15,
            max_symbols_in_file_header: 15,
            max_edges_per_relationship_kind: 15,
            include_relationships: true,
            include_additional_files: true,
            include_completeness_signal: true,
            include_budget_note: true,
        }
    }
}

// ── Node cluster: contiguous lines within a file ─────────────────────────────

#[derive(Debug)]
struct Cluster {
    nodes: Vec<Node>,
    start_line: u32,
    end_line: u32,
    importance: f64,
}

impl Cluster {
    fn new(node: Node, importance: f64) -> Self {
        let start = node.start_line;
        let end = node.end_line;
        Self {
            nodes: vec![node],
            start_line: start,
            end_line: end,
            importance,
        }
    }

    fn merge_with(&mut self, other: Cluster) {
        self.start_line = self.start_line.min(other.start_line);
        self.end_line = self.end_line.max(other.end_line);
        self.importance += other.importance;
        self.nodes.extend(other.nodes);
    }
}

// ── Main handler ─────────────────────────────────────────────────────────────

#[tracing::instrument(skip(handle), fields(query_len = %query.len()))]
pub async fn handle_explore(
    handle: Arc<dyn QueryHandle>,
    query: String,
    max_files: Option<usize>,
) -> Result<String, CodeWikiError> {
    validate_query(&query)?;

    // Get stats for budget calculation
    let stats = handle.get_stats()?;
    let budget = get_explore_output_budget(stats.file_count);

    let max_files = max_files.unwrap_or(budget.default_max_files).clamp(1, 20);

    // 1. Find relevant context
    let opts = FindOpts {
        search_limit: 8,
        traversal_depth: 3,
        max_nodes: 200,
        min_score: 0.2,
    };

    let subgraph = handle.find_relevant_context(&query, opts)?;

    if subgraph.nodes.is_empty() {
        return Ok(format!(
            "## Explore: '{query}'\n\nNo relevant symbols found in the index.\n"
        ));
    }

    // 2. Group by file & score
    let entry_ids: std::collections::HashSet<_> = subgraph.roots.iter().cloned().collect();

    let mut file_nodes: HashMap<String, Vec<(Node, f64)>> = HashMap::new();
    for (id, node) in &subgraph.nodes {
        // Skip import/export nodes (noise)
        if matches!(node.kind, codewiki_core::NodeKind::File) {
            continue;
        }
        let importance = if entry_ids.contains(id) {
            10.0
        } else {
            // Check if directly connected to an entry
            let connected = subgraph.edges.iter().any(|e| {
                (entry_ids.contains(&e.source_id) && e.target_id == *id)
                    || (entry_ids.contains(&e.target_id) && e.source_id == *id)
            });
            if connected {
                3.0
            } else {
                1.0
            }
        };

        file_nodes
            .entry(node.file_path.clone())
            .or_default()
            .push((node.clone(), importance));
    }

    // 3. Sort files by relevance (query terms match filename or symbol names)
    let query_terms: Vec<&str> = query.split_whitespace().collect();

    let mut scored_files: Vec<(String, f64)> = file_nodes
        .iter()
        .map(|(path, nodes)| {
            let mut score: f64 = nodes.iter().map(|(_, imp)| imp).sum();
            // Boost if query terms appear in file path
            let path_lower = path.to_lowercase();
            for term in &query_terms {
                if path_lower.contains(&term.to_lowercase()) {
                    score += 5.0;
                }
            }
            // Deprioritize test/icon/i18n files
            if path.contains("test") || path.contains("spec") {
                score *= 0.7;
            }
            if path.contains("icon") || path.contains("i18n") || path.contains("locale") {
                score *= 0.5;
            }
            (path.clone(), score)
        })
        .collect();

    scored_files.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // 4. Build relationship map
    let mut relationships: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for edge in &subgraph.edges {
        // Skip contains edges (they're structural, not semantic)
        if matches!(edge.kind, EdgeKind::Contains) {
            continue;
        }
        let kind_str = format_edge_kind(&edge.kind);
        let src_name = subgraph
            .nodes
            .get(&edge.source_id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| edge.source_id.clone());
        let dst_name = subgraph
            .nodes
            .get(&edge.target_id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| edge.target_id.clone());
        relationships
            .entry(kind_str.to_string())
            .or_default()
            .push((src_name, dst_name));
    }

    // 5. Format output
    let root = handle.root_path();
    let root_ref = root.as_deref();

    let mut out = format!("## Explore: '{query}'\n\n");
    out.push_str(&crate::tools::root_header(root_ref));

    let selected_files: Vec<&str> = scored_files
        .iter()
        .take(max_files)
        .map(|(p, _)| p.as_str())
        .collect();

    let additional_files: Vec<&str> = scored_files
        .iter()
        .skip(max_files)
        .map(|(p, _)| p.as_str())
        .collect();

    // Relationships section
    if budget.include_relationships && !relationships.is_empty() {
        out.push_str("### Relationships\n\n");
        let mut kinds: Vec<_> = relationships.keys().collect();
        kinds.sort();
        for kind in kinds {
            let edges = &relationships[kind];
            out.push_str(&format!("**{kind}**\n"));
            for (src, dst) in edges.iter().take(budget.max_edges_per_relationship_kind) {
                out.push_str(&format!("- {src} → {dst}\n"));
            }
            out.push('\n');
        }
    }

    // Source code sections
    out.push_str("### Source Code\n\n");

    let mut total_chars = out.len();

    for file_path in &selected_files {
        if total_chars >= budget.max_output_chars {
            break;
        }

        let nodes_with_imp = match file_nodes.get(*file_path) {
            Some(n) => n,
            None => continue,
        };

        // 5a. Cluster symbols by contiguity
        let mut sorted_nodes: Vec<(Node, f64)> = nodes_with_imp.clone();
        sorted_nodes.sort_by_key(|(n, _)| n.start_line);

        let clusters = merge_into_clusters(sorted_nodes, budget.gap_threshold);

        if clusters.is_empty() {
            continue;
        }

        // 5b. Rank clusters (tier 1: entries, tier 2: connected, tier 3: density)
        let mut ranked = clusters;
        ranked.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Symbol header for this file
        let sym_names: Vec<_> = nodes_with_imp
            .iter()
            .take(budget.max_symbols_in_file_header)
            .map(|(n, _)| format!("{} ({})", n.name, format_kind(&n.kind)))
            .collect();

        let file_header = format!(
            "#### `{}` — {}\n\n",
            crate::tools::rel(file_path, root_ref),
            sym_names.join(", ")
        );
        out.push_str(&file_header);

        let mut file_chars = file_header.len();

        for cluster in &ranked {
            if total_chars >= budget.max_output_chars {
                break;
            }
            if file_chars >= budget.max_chars_per_file {
                break;
            }

            // Read the source for the first node of this cluster
            let representative = &cluster.nodes[0];
            let code = match handle.get_code(&representative.id) {
                Ok(Some(c)) => c,
                _ => continue,
            };

            // Prefix lines with line numbers
            let start_line = cluster.start_line;
            let numbered = number_source_lines(&code, start_line);

            let remaining_file = budget.max_chars_per_file.saturating_sub(file_chars);
            let remaining_total = budget.max_output_chars.saturating_sub(total_chars);
            let cap = remaining_file.min(remaining_total);

            let chunk = if numbered.len() > cap {
                format!(
                    "{}\n...(trimmed)...\n",
                    crate::tools::truncate_str(&numbered, cap)
                )
            } else {
                numbered
            };

            let block = format!("```\n{chunk}\n```\n\n");
            file_chars += block.len();
            total_chars += block.len();
            out.push_str(&block);
        }
    }

    // Additional relevant files
    if budget.include_additional_files && !additional_files.is_empty() {
        out.push_str("### Additional Relevant Files (not shown)\n\n");
        for path in additional_files.iter().take(10) {
            out.push_str(&format!("- `{}`\n", crate::tools::rel(path, root_ref)));
        }
        out.push('\n');
    }

    // Completeness signal
    if budget.include_completeness_signal {
        let shown = selected_files.len();
        let total = scored_files.len();
        if shown < total {
            out.push_str(&format!(
                "*Showing {shown} of {total} relevant files. Use `maxFiles` to see more.*\n\n"
            ));
        } else {
            out.push_str("*Complete source code is included above for all relevant files.*\n\n");
        }
    }

    // Budget note
    if budget.include_budget_note {
        out.push_str(&format!(
            "*Output budget: {} chars/file, {} chars total ({} files indexed)*\n",
            budget.max_chars_per_file, budget.max_output_chars, stats.file_count,
        ));
    }

    // Final truncation safety net (char-boundary-safe)
    if out.len() > budget.max_output_chars + 200 {
        out.truncate(crate::tools::floor_char_boundary(
            &out,
            budget.max_output_chars,
        ));
        out.push_str("\n...(output truncated)...\n");
    }

    Ok(out)
}

/// Merge a list of (node, importance) pairs into contiguous clusters.
///
/// Nodes whose line ranges overlap or fall within `gap_threshold` lines of
/// each other are merged into a single cluster.
fn merge_into_clusters(sorted: Vec<(Node, f64)>, gap_threshold: usize) -> Vec<Cluster> {
    let mut clusters: Vec<Cluster> = Vec::new();

    for (node, importance) in sorted {
        // Skip nodes with zero-length spans or obviously bad data
        if node.end_line == 0 {
            clusters.push(Cluster::new(node, importance));
            continue;
        }

        if let Some(last) = clusters.last_mut() {
            let gap = node.start_line.saturating_sub(last.end_line) as usize;
            if gap <= gap_threshold {
                let merge = Cluster::new(node, importance);
                last.merge_with(merge);
                continue;
            }
        }

        clusters.push(Cluster::new(node, importance));
    }

    clusters
}

/// Prefix each line with its 1-based line number (cat -n style).
fn number_source_lines(source: &str, first_line: u32) -> String {
    source
        .lines()
        .enumerate()
        .map(|(i, line)| format!("{}\t{}", first_line + i as u32, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_edge_kind(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Calls => "calls",
        EdgeKind::Imports => "imports",
        EdgeKind::Contains => "contains",
        EdgeKind::Extends => "extends",
        EdgeKind::Implements => "implements",
        EdgeKind::Uses => "uses",
        EdgeKind::References => "references",
        EdgeKind::Instantiates => "instantiates",
        EdgeKind::Exports => "exports",
        EdgeKind::Renders => "renders",
        EdgeKind::Resolves => "resolves",
        EdgeKind::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_tier_small() {
        let b = get_explore_output_budget(50);
        assert_eq!(b.max_output_chars, 18_000);
        assert_eq!(b.max_chars_per_file, 3_800);
        assert_eq!(b.default_max_files, 5);
        assert!(!b.include_completeness_signal);
        assert!(!b.include_budget_note);
    }

    #[test]
    fn budget_tier_medium() {
        let b = get_explore_output_budget(1_000);
        assert_eq!(b.max_output_chars, 13_000);
        assert_eq!(b.max_chars_per_file, 2_500);
        assert_eq!(b.default_max_files, 6);
        assert!(b.include_completeness_signal);
        assert!(b.include_budget_note);
    }

    #[test]
    fn budget_tier_large() {
        let b = get_explore_output_budget(8_000);
        assert_eq!(b.max_output_chars, 35_000);
        assert_eq!(b.max_chars_per_file, 7_000);
        assert_eq!(b.default_max_files, 12);
    }

    #[test]
    fn budget_tier_xl() {
        let b = get_explore_output_budget(20_000);
        assert_eq!(b.max_output_chars, 38_000);
        assert_eq!(b.max_chars_per_file, 7_000);
        assert_eq!(b.default_max_files, 14);
    }

    #[test]
    fn cluster_gap_threshold_merges() {
        let node1 = codewiki_core::Node {
            id: "a".to_string(),
            start_line: 10,
            end_line: 20,
            ..Default::default()
        };
        let node2 = codewiki_core::Node {
            id: "b".to_string(),
            start_line: 25, // gap = 5 lines, threshold = 5
            end_line: 35,
            ..Default::default()
        };
        let clusters = merge_into_clusters(vec![(node1, 1.0), (node2, 1.0)], 5);
        assert_eq!(clusters.len(), 1, "should merge within gap_threshold=5");
        assert_eq!(clusters[0].start_line, 10);
        assert_eq!(clusters[0].end_line, 35);
    }

    #[test]
    fn cluster_gap_threshold_does_not_merge_outside() {
        let node1 = codewiki_core::Node {
            id: "a".to_string(),
            start_line: 10,
            end_line: 20,
            ..Default::default()
        };
        let node2 = codewiki_core::Node {
            id: "b".to_string(),
            start_line: 30, // gap = 10 lines > threshold 5
            end_line: 40,
            ..Default::default()
        };
        let clusters = merge_into_clusters(vec![(node1, 1.0), (node2, 1.0)], 5);
        assert_eq!(clusters.len(), 2, "should NOT merge with gap > threshold");
    }

    #[test]
    fn number_source_lines_correct() {
        let src = "fn foo() {\n    // body\n}";
        let result = number_source_lines(src, 10);
        assert!(result.starts_with("10\t"));
        assert!(result.contains("11\t"));
        assert!(result.contains("12\t"));
    }
}
