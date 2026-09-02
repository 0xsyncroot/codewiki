// T-430 — `callees` subcommand: find all callees of a symbol.

use anyhow::Result;
use std::path::PathBuf;

use crate::commands::render::{render_call_rows, resolve_family, CALL_ROW_LIMIT};
use crate::commands::util::{open_storage, resolve_root};

pub fn run(name: String, depth: usize, path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = open_storage(&root)?;

    // Step 1: resolve the name to every definition that carries it. A bare name
    // is usually a family — resolving to a single node would report one
    // definition's callees and silently omit the rest.
    let family = match resolve_family(&storage, &name)? {
        Some(f) => f,
        None => {
            println!("No symbol matching '{}' found.", name);
            return Ok(());
        }
    };

    println!("Callees of `{}`:", family.resolved_name);
    if family.size > 1 {
        println!(
            "  (aggregated across {} definitions with this name)",
            family.size
        );
    }

    // Step 2: level-order walk per definition; each row is tagged with the hop
    // it was found at, then merged across the family.
    let mut rows = Vec::new();
    let mut truncated = false;
    for id in &family.ids {
        let (mut part, cut) = storage
            .get_callees_with_depth(id, depth, CALL_ROW_LIMIT)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        rows.append(&mut part);
        truncated |= cut;
    }

    render_call_rows(&rows, truncated, &family.ids, false);
    Ok(())
}
