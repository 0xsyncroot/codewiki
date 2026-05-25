// T-430 — `context` subcommand: AI-focused context subgraph for a task description.

use anyhow::Result;
use codewiki_storage::{FindOpts, QueryHandle};
use std::path::PathBuf;

use crate::commands::util::{open_storage, resolve_root};

pub fn run(task: String, path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = open_storage(&root)?;

    // Use spec-correct defaults (search_limit=3, traversal_depth=1, max_nodes=20).
    // These match the TS DEFAULT_FIND_OPTIONS and the MCP tool opts.
    let opts = FindOpts {
        search_limit: 3,
        traversal_depth: 1,
        max_nodes: 20,
        min_score: 0.3,
    };

    let subgraph = storage
        .find_relevant_context(&task, opts)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if subgraph.nodes.is_empty() {
        println!("No relevant context found for: {}", task);
        return Ok(());
    }

    println!("# Context: {}", task);
    println!();
    println!("**Nodes ({}):**", subgraph.nodes.len());
    println!();

    // Sort nodes for deterministic output.
    let mut nodes: Vec<_> = subgraph.nodes.values().collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));

    for node in nodes {
        let kind = format!("{:?}", node.kind).to_lowercase();
        println!(
            "- `{}` ({}) — `{}:{}`",
            node.name, kind, node.file_path, node.start_line
        );
        if let Some(ref doc) = node.docstring {
            let summary = doc.lines().next().unwrap_or("").trim();
            if !summary.is_empty() {
                println!("  > {}", summary);
            }
        }
    }

    if !subgraph.edges.is_empty() {
        println!();
        println!("**Edges ({}):**", subgraph.edges.len());
        for edge in &subgraph.edges {
            let kind = format!("{:?}", edge.kind).to_lowercase();
            println!("- {} --[{}]--> {}", edge.source_id, kind, edge.target_id);
        }
    }
    Ok(())
}
