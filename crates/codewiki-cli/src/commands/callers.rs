// T-430 — `callers` subcommand: find all callers of a symbol.

use anyhow::Result;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::path::PathBuf;

use crate::commands::render::{render_call_rows, CALL_ROW_LIMIT};
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

    // Step 2: level-order walk; each row is tagged with the hop it was found at.
    let (callers, truncated) = storage
        .get_callers_with_depth(&node.id, depth, CALL_ROW_LIMIT)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    render_call_rows(&callers, truncated, &node.id, true);
    Ok(())
}
