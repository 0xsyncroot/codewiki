// T-430 — `query` subcommand: BM25/FTS search for symbols.

use anyhow::Result;
use codewiki_storage::{QueryHandle, SearchOptions};
use std::path::PathBuf;

use crate::commands::util::{open_storage, resolve_root};

pub fn run(query: String, limit: usize, path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = open_storage(&root)?;

    let opts = SearchOptions {
        limit,
        ..Default::default()
    };

    let results = storage
        .search_nodes(&query, opts)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if results.is_empty() {
        println!("No matches.");
        return Ok(());
    }

    // Print compact table: kind  language  name  path:line
    println!("{:<12} {:<12} {:<40} LOCATION", "KIND", "LANGUAGE", "NAME");
    println!("{}", "-".repeat(80));
    for r in &results {
        let n = &r.node;
        let kind = format!("{:?}", n.kind).to_lowercase();
        let lang = format!("{:?}", n.language).to_lowercase();
        let location = format!("{}:{}", n.file_path, n.start_line);
        println!("{:<12} {:<12} {:<40} {}", kind, lang, n.name, location);
    }
    Ok(())
}
