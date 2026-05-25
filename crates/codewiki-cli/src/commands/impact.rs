// T-430 — `impact` subcommand: show impact radius of a symbol change.

use anyhow::Result;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::path::PathBuf;

use crate::commands::util::{open_storage, resolve_root};

pub fn run(name: String, depth: u8, path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = open_storage(&root)?;

    // Step 1: resolve name → node_id.
    let results = storage
        .search_nodes(&name, SearchOptions { limit: 1, ..Default::default() })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let node = match results.into_iter().next() {
        Some(r) => r.node,
        None => {
            println!("No symbol matching '{}' found.", name);
            return Ok(());
        }
    };

    println!(
        "Impact radius of `{}` ({}:{}) depth={}:",
        node.name, node.file_path, node.start_line, depth
    );

    // Step 2: compute impact subgraph.
    let subgraph = storage
        .get_impact_radius(&node.id, depth as usize)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if subgraph.nodes.is_empty() {
        println!("  (no impacted nodes)");
        return Ok(());
    }

    println!("  {} potentially affected nodes:", subgraph.nodes.len());

    let mut nodes: Vec<_> = subgraph.nodes.values().collect();
    nodes.sort_by(|a, b| a.file_path.cmp(&b.file_path).then(a.start_line.cmp(&b.start_line)));

    for n in nodes {
        let kind = format!("{:?}", n.kind).to_lowercase();
        println!("    \u{00b7} `{}` ({}) {}:{}", n.name, kind, n.file_path, n.start_line);
    }
    Ok(())
}
