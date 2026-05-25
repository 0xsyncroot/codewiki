// T-430 — `status` subcommand: show index stats.

use anyhow::Result;
use codewiki_storage::QueryHandle;
use std::path::PathBuf;

use crate::commands::util::{open_storage, resolve_root};

pub fn run(path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = open_storage(&root)?;

    let stats = storage
        .get_stats()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let db_size_kb = stats.db_size_bytes / 1024;

    println!("CodeWiki Index Status");
    println!("  Root:              {}", root.display());
    println!("  Nodes:             {}", stats.node_count);
    println!("  Edges:             {}", stats.edge_count);
    println!("  Files:             {}", stats.file_count);
    println!("  Unresolved refs:   {}", stats.unresolved_ref_count);
    println!("  DB size:           {} KB", db_size_kb);
    println!("  Journal mode:      {}", stats.journal_mode);

    if !stats.nodes_by_kind.is_empty() {
        println!();
        println!("  Nodes by kind:");
        let mut by_kind: Vec<_> = stats.nodes_by_kind.iter().collect();
        by_kind.sort_by(|a, b| b.1.cmp(a.1));
        for (kind, count) in by_kind {
            println!("    {:20} {}", kind, count);
        }
    }

    if !stats.files_by_language.is_empty() {
        println!();
        println!("  Files by language:");
        let mut by_lang: Vec<_> = stats.files_by_language.iter().collect();
        by_lang.sort_by(|a, b| b.1.cmp(a.1));
        for (lang, count) in by_lang {
            println!("    {:20} {}", lang, count);
        }
    }
    Ok(())
}
