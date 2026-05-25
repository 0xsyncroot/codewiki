// T-430 — `callers` subcommand: find all callers of a symbol.

use anyhow::Result;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::path::PathBuf;

use crate::commands::util::{open_storage, resolve_root};

pub fn run(name: String, depth: usize, path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = open_storage(&root)?;

    // Step 1: resolve name → node_id via search.
    let results = storage
        .search_nodes(
            &name,
            SearchOptions {
                limit: 1,
                ..Default::default()
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let node = match results.into_iter().next() {
        Some(r) => r.node,
        None => {
            println!("No symbol matching '{}' found.", name);
            return Ok(());
        }
    };

    println!(
        "Callers of `{}` ({}:{}):",
        node.name, node.file_path, node.start_line
    );

    // Step 2: BFS callers up to depth.
    let callers = storage
        .get_callers(&node.id, depth)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if callers.is_empty() {
        println!("  (none)");
        return Ok(());
    }

    for (caller, edge) in &callers {
        let edge_kind = format!("{:?}", edge.kind).to_lowercase();
        println!(
            "  \u{2190} `{}` ({}:{}) --[{}]-->",
            caller.name, caller.file_path, caller.start_line, edge_kind
        );
    }
    Ok(())
}
