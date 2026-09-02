//! Shared rendering for the `callers` / `callees` subcommands.
//!
//! Both commands answer a *direct* question ("who calls X", "what does X
//! call") but traverse a graph that can also reach further. Rendering them
//! through one function is what keeps the two commands from drifting apart.

use anyhow::Result;
use codewiki_core::{Edge, Node, NodeKind};
use codewiki_storage::StorageImpl;
use std::collections::HashSet;

/// Maximum rows a single `callers`/`callees` query renders before truncating.
///
/// Traversal itself is uncapped for library consumers; this bound exists so a
/// deep `--depth` cannot dump thousands of unlabelled lines at a terminal.
pub const CALL_ROW_LIMIT: usize = 2_000;

/// Every definition that carries a given bare name.
///
/// A bare identifier usually names a family — 14 `create_app`s in one corpus,
/// 53 `makeFinding`s in another. Resolving it to a single node makes
/// `callers`/`callees` report one definition's neighbours and stay silent about
/// the rest, which reads as a confident `(none)` for a symbol with hundreds of
/// call sites. A qualified name (`Foo::bar`) still resolves to exactly one node.
pub struct SymbolFamily {
    pub ids: Vec<String>,
    pub resolved_name: String,
    pub size: usize,
}

/// Resolve `name` to its family of same-named definitions.
pub fn resolve_family(storage: &StorageImpl, name: &str) -> Result<Option<SymbolFamily>> {
    let resolved = storage
        .resolve_symbol_family(name)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(resolved.map(|(ids, resolved_name, size)| SymbolFamily {
        ids,
        resolved_name,
        size,
    }))
}

/// Render rows produced by `get_callers_with_depth` / `get_callees_with_depth`.
///
/// Direct edges (hop 1) print with a plain arrow; transitive edges print with
/// an elided arrow and an explicit `(depth N)`. That distinction is the whole
/// point — "who calls X" and "what transitively reaches X" are different
/// questions and must not share a glyph.
///
/// Consecutive rows sharing (node, edge kind, call-site line) collapse into a
/// single line carrying a `xN call sites` suffix.
pub fn render_call_rows(
    rows: &[(Node, Edge, usize)],
    truncated: bool,
    focal_ids: &[String],
    incoming: bool,
) {
    let noun = if incoming { "caller" } else { "callee" };

    if rows.is_empty() {
        println!("  (none)");
        return;
    }

    // Rows arrive level-ordered per family member; merging several members can
    // interleave duplicates, so sort before collapsing runs.
    let mut rows: Vec<(Node, Edge, usize)> = rows.to_vec();
    rows.sort_by(|a, b| {
        a.2.cmp(&b.2)
            .then_with(|| a.0.file_path.cmp(&b.0.file_path))
            .then_with(|| a.1.line.cmp(&b.1.line))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });

    // Collapse runs describing the same relationship at the same position.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for (i, (node, edge, depth)) in rows.iter().enumerate() {
        let merged = match groups.last_mut() {
            Some((first, count)) => {
                let (prev_node, prev_edge, prev_depth) = &rows[*first];
                if prev_node.id == node.id
                    && prev_edge.kind == edge.kind
                    && prev_edge.line == edge.line
                    && prev_depth == depth
                {
                    *count += 1;
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        if !merged {
            groups.push((i, 1));
        }
    }

    let mut distinct_direct: HashSet<&str> = HashSet::new();
    for (first, count) in &groups {
        let (node, edge, depth) = &rows[*first];
        let direct = *depth == 1;
        if direct {
            distinct_direct.insert(node.id.as_str());
        }

        let arrow = match (incoming, direct) {
            (true, true) => "\u{2190}  ",
            (true, false) => "\u{2190}\u{2026}\u{2190}",
            (false, true) => "\u{2192}  ",
            (false, false) => "\u{2192}\u{2026}\u{2192}",
        };

        // Prefer the call-site line; fall back to the declaration for edges
        // indexed before the resolver started recording positions.
        let line = edge.line.unwrap_or(node.start_line);

        let mut tags = String::new();
        if node.kind == NodeKind::File {
            // Top-level or anonymous-callback call: no enclosing named symbol.
            tags.push_str(" (file scope)");
        }
        if focal_ids.iter().any(|f| f == &node.id) {
            tags.push_str(" (self)");
        }
        if !direct {
            tags.push_str(&format!(" (depth {depth})"));
        }
        if *count > 1 {
            tags.push_str(&format!(" x{count} call sites"));
        }

        println!(
            "  {} `{}` ({}:{}) --[{}]-->{}",
            arrow,
            node.name,
            node.file_path,
            line,
            format!("{:?}", edge.kind).to_lowercase(),
            tags
        );
    }

    println!();
    let deeper = groups.iter().filter(|(f, _)| rows[*f].2 > 1).count();
    if deeper > 0 {
        println!(
            "{} direct {noun}(s); {} transitive row(s) beyond depth 1.",
            distinct_direct.len(),
            deeper
        );
    } else {
        println!("{} direct {noun}(s).", distinct_direct.len());
    }
    if truncated {
        println!(
            "  (truncated at {CALL_ROW_LIMIT} rows \u{2014} narrow the query or lower --depth)"
        );
    }
}
