// T-430 — `files` subcommand: list all tracked files in the index.

use anyhow::Result;
use codewiki_storage::{FileFilter, QueryHandle};
use std::path::PathBuf;

use crate::commands::util::{open_storage, resolve_root};

pub fn run(prefix: Option<String>, path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = open_storage(&root)?;

    let filter = prefix.as_deref().map(|p| FileFilter {
        path_prefix: Some(p.to_string()),
        ..Default::default()
    });

    let files = storage
        .get_files(filter.as_ref())
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if files.is_empty() {
        println!("No files indexed.");
        return Ok(());
    }

    println!("{:<12} {:>8} {:>6}  PATH", "LANGUAGE", "SIZE", "NODES");
    println!("{}", "-".repeat(60));
    for f in &files {
        println!(
            "{:<12} {:>8} {:>6}  {}",
            f.language,
            f.size,
            f.node_count,
            f.path.display()
        );
    }
    println!();
    println!("{} file(s) total.", files.len());
    Ok(())
}
