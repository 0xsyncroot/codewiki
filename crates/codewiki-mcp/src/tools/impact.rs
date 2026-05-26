//! T-410 — `codewiki_impact` tool handler.
//!
//! Analyze the impact radius of changing a symbol.
//!
//! Overloaded-name aggregation: a bare symbol name is resolved to its
//! same-name family (all definitions sharing the simple name) and their
//! reverse-reach impact subgraphs are unioned (deduped by node id / edge),
//! capped at a bounded node budget. A fully-qualified symbol (e.g. `Foo::bar`)
//! resolves to exactly that node.

use crate::input_limits::validate_query;
use crate::tools::{search::format_kind, MAX_OUTPUT_LENGTH};
use codewiki_core::CodeWikiError;
use codewiki_storage::QueryHandle;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Upper bound on affected nodes returned to the agent. Keeps the per-call
/// token cost bounded even for highly-connected same-name families.
const IMPACT_NODE_CAP: usize = 100;

#[tracing::instrument(skip(handle), fields(symbol_len = %symbol.len(), depth))]
pub async fn handle_impact(
    handle: Arc<dyn QueryHandle>,
    symbol: String,
    depth: usize,
) -> Result<String, CodeWikiError> {
    validate_query(&symbol)?;

    let depth = depth.clamp(1, 10);

    let agg = handle.get_impact_aggregated(&symbol, depth, IMPACT_NODE_CAP)?;

    if agg.resolved_name.is_empty() {
        return Ok(format!("No symbol found matching '{symbol}'."));
    }

    let all_nodes = &agg.subgraph.nodes;
    let all_edges = &agg.subgraph.edges;

    if all_nodes.is_empty() {
        return Ok(format!(
            "Symbol '{symbol}' has no dependents within {depth} hop(s)."
        ));
    }

    let root = handle.root_path();
    let root_ref = root.as_deref();

    let mut out = format!("## Impact Radius of '{symbol}' (depth {depth})\n\n");
    out.push_str(&crate::tools::root_header(root_ref));
    if agg.family_size > 1 {
        out.push_str(&format!(
            "Aggregated across {} definitions named `{}`.\n\n",
            agg.family_size, agg.resolved_name,
        ));
    }
    out.push_str(&format!(
        "**{} affected symbols** across {} files, {} edges\n\n",
        all_nodes.len(),
        count_unique_files(all_nodes),
        all_edges.len(),
    ));

    // Direct dependants/implementers = sources of an edge whose target is a
    // root (the focal symbol family). These are the highest-value answer nodes
    // for an impl/blast query; their files must render FIRST so that, under the
    // output cap, the genuine 1-hop answers are never truncated away by deeper
    // transitive or test-file nodes.
    let root_ids: HashSet<&str> = agg.subgraph.roots.iter().map(|s| s.as_str()).collect();
    let direct_files: HashSet<String> = all_edges
        .iter()
        .filter(|e| root_ids.contains(e.target_id.as_str()))
        .filter_map(|e| all_nodes.get(&e.source_id))
        .map(|n| n.file_path.clone())
        .collect();

    // Group affected symbols by file
    let mut by_file: HashMap<String, Vec<&codewiki_core::Node>> = HashMap::new();
    for node in all_nodes.values() {
        by_file
            .entry(node.file_path.clone())
            .or_default()
            .push(node);
    }

    // Order files by a value-first key:
    //   1. files holding a DIRECT dependant/implementer of the focal symbol,
    //   2. then production source over test files,
    //   3. then alphabetical for determinism.
    // When the impact set is large enough to hit the output cap, this guarantees
    // the genuine 1-hop answers appear before deeper transitive/test nodes that
    // would otherwise consume the budget and truncate real answers.
    // (General rule for impact, not tuned to any case.)
    let mut files: Vec<_> = by_file.keys().cloned().collect();
    files.sort_by(|a, b| {
        let da = !direct_files.contains(a); // false (direct) sorts first
        let db = !direct_files.contains(b);
        da.cmp(&db)
            .then_with(|| is_test_path(a).cmp(&is_test_path(b)))
            .then_with(|| a.cmp(b))
    });

    for file in &files {
        out.push_str(&format!("### `{}`\n\n", crate::tools::rel(file, root_ref)));
        let mut nodes = by_file[file].clone();
        nodes.sort_by_key(|n| n.start_line);
        for node in nodes {
            out.push_str(&format!(
                "- **{}** ({}) line {}\n",
                node.name,
                format_kind(&node.kind),
                node.start_line,
            ));
        }
        out.push('\n');
    }

    Ok(crate::tools::truncate_output(out, MAX_OUTPUT_LENGTH))
}

fn count_unique_files(nodes: &HashMap<String, codewiki_core::Node>) -> usize {
    let paths: HashSet<_> = nodes.values().map(|n| &n.file_path).collect();
    paths.len()
}

/// Heuristic: is this path a test/spec file? Language-agnostic — matches common
/// test directory and filename conventions across the supported languages.
fn is_test_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);
    file_name.starts_with("test_")
        || file_name.starts_with("test.")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
        || file_name.contains("_test.")
        || file_name.ends_with("test.java")
        || file_name.ends_with("tests.cs")
        || file_name.ends_with("test.go")
        || lower.contains("/__tests__/")
        || lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("/spec/")
}

#[cfg(test)]
mod tests {
    use super::is_test_path;

    #[test]
    fn test_path_detection_is_language_agnostic() {
        // Production source is NOT a test path.
        assert!(!is_test_path(
            "src/main/java/com/google/gson/TypeAdapter.java"
        ));
        assert!(!is_test_path("packages/zod/src/v3/types.ts"));
        assert!(!is_test_path("crates/flags/defs.rs"));
        // Common test conventions across languages ARE detected.
        assert!(is_test_path(
            "gson/src/test/java/com/google/gson/GsonTest.java"
        ));
        assert!(is_test_path("pkg/foo/bar_test.go"));
        assert!(is_test_path("src/__tests__/widget.test.ts"));
        assert!(is_test_path("app/components/foo.spec.ts"));
        assert!(is_test_path("tests/test_app.py"));
        assert!(is_test_path("MyProject/FooTests.cs"));
        // Windows separators are normalized.
        assert!(is_test_path("src\\test\\Thing.java"));
    }
}
