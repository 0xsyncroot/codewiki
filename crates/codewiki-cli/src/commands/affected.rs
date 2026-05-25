// T-430 — `affected` subcommand: show nodes affected by a set of files.

use anyhow::Result;
use codewiki_storage::QueryHandle;
use std::path::PathBuf;

use crate::commands::util::{open_storage, resolve_root};

pub fn run(files: Vec<PathBuf>, path: Option<PathBuf>) -> Result<()> {
    if files.is_empty() {
        println!("No files specified.");
        return Ok(());
    }

    let root = resolve_root(path);
    let storage = open_storage(&root)?;

    let affected = storage
        .get_affected_nodes(&files)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if affected.is_empty() {
        println!("No nodes affected by {:?}", files);
        return Ok(());
    }

    println!("Nodes affected by {} file(s):", files.len());
    let mut nodes = affected;
    nodes.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
    });
    for node in &nodes {
        let kind = format!("{:?}", node.kind).to_lowercase();
        println!(
            "  \u{00b7} `{}` ({}) {}:{}",
            node.name, kind, node.file_path, node.start_line
        );
    }
    Ok(())
}
