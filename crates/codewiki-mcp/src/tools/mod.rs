//! T-407 — MCP tool handlers.
//!
//! Each tool is implemented as an async function taking `Arc<dyn QueryHandle>`
//! and the tool's JSON arguments. The `dispatch` function routes incoming
//! `tools/call` requests to the correct handler.

pub mod callees;
pub mod callers;
pub mod context;
pub mod explore;
pub mod files;
pub mod impact;
pub mod node;
pub mod search;
pub mod status;

use codewiki_storage::QueryHandle;
use rmcp::model::{AnnotateAble, CallToolRequestParam, CallToolResult, ListToolsResult, Tool};
use serde_json::Value;
use std::sync::Arc;

/// Maximum output characters for most tools (except explore which is adaptive).
pub const MAX_OUTPUT_LENGTH: usize = 15_000;

/// Container node kinds: for these, `codewiki_node` with `includeCode=true`
/// returns an outline rather than the full body.
pub const CONTAINER_NODE_KINDS: &[&str] = &[
    "class",
    "struct",
    "interface",
    "trait",
    "protocol",
    "enum",
    "namespace",
    "module",
];

/// Build the JSON input schema object from a serde_json Value.
pub fn make_schema(schema: Value) -> Arc<rmcp::model::JsonObject> {
    Arc::new(match schema {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    })
}

/// Return all tool definitions.
pub fn list_tools() -> ListToolsResult {
    let tools = vec![
        Tool::new(
            "codewiki_search",
            "Find symbols by name. Each hit ALREADY includes location, signature, \
             and the first docstring line — so do NOT chain codewiki_node just to \
             read a signature you already have here. For a whole task/area use \
             codewiki_context instead.",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol name or partial name (e.g., \"auth\", \"signIn\", \"UserService\")"
                    },
                    "kind": {
                        "type": "string",
                        "description": "Filter by node kind",
                        "enum": ["function", "method", "class", "struct", "enum",
                                 "trait", "interface", "type", "namespace",
                                 "variable", "constant", "property", "route",
                                 "component"]
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum results (default: 10)",
                        "default": 10
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                },
                "required": ["query"]
            })),
        ),
        Tool::new(
            "codewiki_context",
            "PRIMARY TOOL — call this FIRST for any \"how does X work\", architecture, \
             feature, or bug-context question. Composes search + node + callers + callees \
             and returns entry points, related symbols, and key code in ONE call.",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Description of the task, bug, or feature to build context for"
                    },
                    "maxNodes": {
                        "type": "number",
                        "description": "Maximum symbols to include (default: 20)",
                        "default": 20
                    },
                    "includeCode": {
                        "type": "boolean",
                        "description": "Include code snippets for key symbols (default: true)",
                        "default": true
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                },
                "required": ["task"]
            })),
        ),
        Tool::new(
            "codewiki_callers",
            "Direct (1-hop) callers of a symbol. NOTE: resolves the symbol name to \
             its TOP search match only — on an ambiguous/overloaded name you get \
             callers of one definition. An empty result means \"no callers of THIS \
             match\", NOT \"unused\": to gauge real reach (incl. interface/trait \
             implementors and transitive use) call codewiki_impact instead.",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Name of the function, method, or class to find callers for"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of callers to return (default: 20)",
                        "default": 20
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                },
                "required": ["symbol"]
            })),
        ),
        Tool::new(
            "codewiki_callees",
            "Direct (1-hop) callees a symbol invokes — its outgoing dependencies / \
             code flow. Resolves the symbol name to its TOP search match only, so \
             on an ambiguous name you see one definition's callees.",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Name of the function, method, or class to find callees for"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of callees to return (default: 20)",
                        "default": 20
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                },
                "required": ["symbol"]
            })),
        ),
        Tool::new(
            "codewiki_impact",
            "Blast radius of changing a symbol: the transitive set of dependents \
             (callers, importers, references, interface/trait usage) up to `depth` \
             hops. Prefer this over codewiki_callers when you need the FULL reach \
             of a change or are unsure whether a symbol is actually used.",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Name of the symbol to analyze impact for"
                    },
                    "depth": {
                        "type": "number",
                        "description": "How many levels of dependencies to traverse (default: 2)",
                        "default": 2
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                },
                "required": ["symbol"]
            })),
        ),
        Tool::new(
            "codewiki_node",
            "Deep-dive ONE named symbol you already have. Returns location, \
             signature, and docstring (codewiki_search already gives you these, so \
             don't call node just to re-read a signature). Pass includeCode=true \
             for source: a function/method returns its body, a \
             class/interface/struct/enum returns a compact member OUTLINE. Pass \
             includeCallers/includeCallees=true to fold in the symbol's one-hop \
             neighbors — collapsing a node→callers→callees chain into one call. \
             Resolves to the TOP search match. Note: callers/impact are the \
             accurate path for full reach on ambiguous names.",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Name of the symbol to get details for"
                    },
                    "includeCode": {
                        "type": "boolean",
                        "description": "Include full source code (default: false to minimize context)",
                        "default": false
                    },
                    "includeCallers": {
                        "type": "boolean",
                        "description": "Fold in the symbol's one-hop callers (default: false)",
                        "default": false
                    },
                    "includeCallees": {
                        "type": "boolean",
                        "description": "Fold in the symbol's one-hop callees (default: false)",
                        "default": false
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                },
                "required": ["symbol"]
            })),
        ),
        Tool::new(
            "codewiki_explore",
            "Source for SEVERAL related symbols grouped by file, plus a relationship \
             map, in ONE capped call. Typically the SECOND call after \
             codewiki_context: context maps the area, explore pulls the code. Query \
             with TERMS — specific symbol names, file names, or short code tokens — \
             NOT natural-language sentences (those are for codewiki_context).",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol names, file names, or short code terms to explore"
                    },
                    "maxFiles": {
                        "type": "number",
                        "description": "Maximum number of files to include source code from (default: 12)",
                        "default": 12
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                },
                "required": ["query"]
            })),
        ),
        Tool::new(
            "codewiki_status",
            "Get the status of the CodeWiki index, including statistics about \
             indexed files, nodes, and edges.",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                }
            })),
        ),
        Tool::new(
            "codewiki_files",
            "REQUIRED for file/folder exploration. Get the project file structure \
             from the CodeWiki index. Returns a tree view of all indexed files with \
             metadata (language, symbol count). Much faster than Glob/filesystem scanning.",
            make_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Filter to files under this directory path"
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Filter files matching this glob pattern"
                    },
                    "format": {
                        "type": "string",
                        "description": "Output format: \"tree\" (default), \"flat\", \"grouped\"",
                        "enum": ["tree", "flat", "grouped"],
                        "default": "tree"
                    },
                    "includeMetadata": {
                        "type": "boolean",
                        "description": "Include file metadata like language and symbol count (default: true)",
                        "default": true
                    },
                    "maxDepth": {
                        "type": "number",
                        "description": "Maximum directory depth to show (default: unlimited)"
                    },
                    "projectPath": {
                        "type": "string",
                        "description": "Path to a different project with .codewiki/ initialized."
                    }
                }
            })),
        ),
    ];

    ListToolsResult {
        next_cursor: None,
        tools,
    }
}

/// Dispatch a tool call to the appropriate handler.
#[tracing::instrument(skip(handle, params), fields(tool = %params.name))]
pub async fn dispatch(
    handle: Arc<dyn QueryHandle>,
    params: CallToolRequestParam,
) -> Result<CallToolResult, rmcp::Error> {
    let args = params.arguments.unwrap_or_default();
    let get_str = |key: &str| -> Option<String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let get_u64 = |key: &str| -> Option<u64> { args.get(key).and_then(|v| v.as_u64()) };
    let get_bool = |key: &str| -> Option<bool> { args.get(key).and_then(|v| v.as_bool()) };

    let result = match params.name.as_ref() {
        "codewiki_search" => {
            let query = get_str("query").unwrap_or_default();
            let kind = get_str("kind");
            let limit = get_u64("limit").unwrap_or(10) as usize;
            search::handle_search(handle, query, kind, limit).await
        }
        "codewiki_context" => {
            let task = get_str("task").unwrap_or_default();
            let max_nodes = get_u64("maxNodes").unwrap_or(20) as usize;
            let include_code = get_bool("includeCode").unwrap_or(true);
            context::handle_context(handle, task, max_nodes, include_code).await
        }
        "codewiki_callers" => {
            let symbol = get_str("symbol").unwrap_or_default();
            let limit = get_u64("limit").unwrap_or(20) as usize;
            callers::handle_callers(handle, symbol, limit).await
        }
        "codewiki_callees" => {
            let symbol = get_str("symbol").unwrap_or_default();
            let limit = get_u64("limit").unwrap_or(20) as usize;
            callees::handle_callees(handle, symbol, limit).await
        }
        "codewiki_impact" => {
            let symbol = get_str("symbol").unwrap_or_default();
            let depth = get_u64("depth").unwrap_or(2) as usize;
            let depth = depth.clamp(1, 10);
            impact::handle_impact(handle, symbol, depth).await
        }
        "codewiki_node" => {
            let symbol = get_str("symbol").unwrap_or_default();
            let include_code = get_bool("includeCode").unwrap_or(false);
            let include_callers = get_bool("includeCallers").unwrap_or(false);
            let include_callees = get_bool("includeCallees").unwrap_or(false);
            node::handle_node(
                handle,
                symbol,
                include_code,
                include_callers,
                include_callees,
            )
            .await
        }
        "codewiki_explore" => {
            let query = get_str("query").unwrap_or_default();
            let max_files = get_u64("maxFiles").map(|n| n as usize);
            explore::handle_explore(handle, query, max_files).await
        }
        "codewiki_status" => status::handle_status(handle).await,
        "codewiki_files" => {
            let path = get_str("path");
            let pattern = get_str("pattern");
            let format = get_str("format").unwrap_or_else(|| "tree".to_string());
            let include_metadata = get_bool("includeMetadata").unwrap_or(true);
            let max_depth = get_u64("maxDepth").map(|n| n as usize);
            files::handle_files(handle, path, pattern, format, include_metadata, max_depth).await
        }
        _other => {
            return Err(rmcp::Error::method_not_found::<
                rmcp::model::CallToolRequestMethod,
            >());
        }
    };

    match result {
        Ok(text) => Ok(CallToolResult::success(vec![
            rmcp::model::RawContent::text(text).no_annotation(),
        ])),
        Err(e) => {
            let msg = e.to_string();
            tracing::warn!(error = %msg, "Tool handler returned error");
            Ok(CallToolResult::error(vec![rmcp::model::RawContent::text(
                msg,
            )
            .no_annotation()]))
        }
    }
}

/// Largest char-boundary index `<= max` within `s`.
///
/// `str::floor_char_boundary` is still nightly-only, so we reimplement it.
/// Slicing/truncating a `String` at a non-char-boundary panics; every byte
/// cap in this crate must route through this guard (Wave 1d).
pub fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut idx = max;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Truncate a `&str` to at most `max` bytes, snapping down to a char boundary.
/// Never panics on multibyte UTF-8.
pub fn truncate_str(s: &str, max: usize) -> &str {
    &s[..floor_char_boundary(s, max)]
}

/// Truncate a string to at most `max_chars` bytes, appending an indicator if
/// truncated. Snaps to a UTF-8 char boundary so multibyte output never panics.
pub fn truncate_output(s: String, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s;
    }
    let truncated = truncate_str(&s, max_chars);
    format!("{truncated}\n...(output truncated to {max_chars} characters)")
}

/// Strip a forward-slash-normalized root prefix from `path`, returning a
/// workspace-relative path. Indexed paths are stored '/'-normalized on every
/// platform, so this deliberately does NOT use `std::path` /
/// `MAIN_SEPARATOR` — that would break on Windows where the separator is `\`.
///
/// Returns `path` unchanged when `root` is `None`, empty, or not a prefix
/// (e.g. a `projectPath` outside the root). A leading `/` left after stripping
/// is removed so results are truly relative.
pub fn rel<'a>(path: &'a str, root: Option<&str>) -> &'a str {
    let root = match root {
        Some(r) if !r.is_empty() => r,
        _ => return path,
    };
    let root = root.strip_suffix('/').unwrap_or(root);
    match path.strip_prefix(root) {
        Some(rest) => rest.strip_prefix('/').unwrap_or(rest),
        None => path,
    }
}

/// Render a one-line workspace-root header so agents can rejoin relative
/// paths to absolute ones. Emits nothing when the root is unknown.
pub fn root_header(root: Option<&str>) -> String {
    match root {
        Some(r) if !r.is_empty() => {
            format!("_Workspace root: `{r}` (paths below are relative)_\n\n")
        }
        _ => String::new(),
    }
}

/// Render a numbered list of neighbor nodes (callers/callees) as Markdown
/// bullet lines, capped at `limit` with a "+N more" pointer when truncated.
///
/// Shared by `codewiki_callers`, `codewiki_callees`, and the
/// `includeCallers`/`includeCallees` fold-in of `codewiki_node` so all three
/// emit identical, root-relative formatting (single source of truth).
pub fn render_neighbor_list(
    nodes: &[codewiki_core::Node],
    limit: usize,
    root: Option<&str>,
) -> String {
    use crate::tools::search::format_kind;
    let mut out = String::new();
    let shown = nodes.len().min(limit);
    for (i, node) in nodes.iter().take(shown).enumerate() {
        out.push_str(&format!(
            "{}. **{}** ({}) in `{}:{}`\n",
            i + 1,
            node.name,
            format_kind(&node.kind),
            rel(&node.file_path, root),
            node.start_line,
        ));
    }
    if nodes.len() > shown {
        out.push_str(&format!("...(+{} more)\n", nodes.len() - shown));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_strips_forward_slash_root() {
        let root = Some("/home/u/proj");
        assert_eq!(rel("/home/u/proj/src/main.rs", root), "src/main.rs");
        // Trailing slash on the root is tolerated.
        assert_eq!(
            rel("/home/u/proj/src/main.rs", Some("/home/u/proj/")),
            "src/main.rs"
        );
    }

    #[test]
    fn rel_passthrough_when_no_root_or_no_match() {
        assert_eq!(rel("/a/b/c.rs", None), "/a/b/c.rs");
        assert_eq!(rel("/a/b/c.rs", Some("")), "/a/b/c.rs");
        // projectPath outside the root → unchanged.
        assert_eq!(rel("/other/x.rs", Some("/home/u/proj")), "/other/x.rs");
    }

    #[test]
    fn floor_char_boundary_handles_multibyte() {
        let s = "héllo"; // 'é' is 2 bytes at index 1..3
                         // max=2 lands inside 'é' → must floor to 1.
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(floor_char_boundary(s, s.len() + 10), s.len());
        // truncating there must not panic and must be valid UTF-8.
        let _ = &s[..floor_char_boundary(s, 2)];
    }

    #[test]
    fn truncate_output_no_panic_on_multibyte() {
        // Build a string whose byte-cap would fall mid-character.
        let s: String = "🦀".repeat(100); // each crab is 4 bytes
        let out = truncate_output(s, 10); // 10 is mid-emoji
        assert!(out.contains("output truncated"));
        // If we got here without panicking, the boundary guard worked.
    }

    #[test]
    fn root_header_emitted_only_when_known() {
        assert!(root_header(Some("/x")).contains("Workspace root"));
        assert_eq!(root_header(None), "");
        assert_eq!(root_header(Some("")), "");
    }
}
