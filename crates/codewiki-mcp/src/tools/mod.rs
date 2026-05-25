//! T-407 — MCP tool handlers.
//!
//! Each tool is implemented as an async function taking `Arc<dyn QueryHandle>`
//! and the tool's JSON arguments. The `dispatch` function routes incoming
//! `tools/call` requests to the correct handler.

pub mod callers;
pub mod callees;
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
    "class", "struct", "interface", "trait", "protocol", "enum", "namespace", "module",
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
            "Quick symbol search by name. Returns locations only (no code). \
             Use codewiki_context instead for comprehensive task context.",
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
                        "enum": ["function", "method", "class", "interface", "type",
                                 "variable", "route", "component"]
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
            "Find all functions/methods that call a specific symbol. \
             Useful for understanding usage patterns and impact of changes.",
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
            "Find all functions/methods that a specific symbol calls. \
             Useful for understanding dependencies and code flow.",
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
            "Analyze the impact radius of changing a symbol. \
             Shows what code could be affected by modifications.",
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
            "Get detailed info about ONE symbol (location, signature, docstring). \
             Pass includeCode=true for source: a function/method returns its body; \
             a class/interface/struct/enum returns a compact member OUTLINE.",
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
            "Returns source for SEVERAL related symbols grouped by file, plus a \
             relationship map, in ONE capped call. Query with specific symbol/file/code \
             terms, NOT natural-language sentences.",
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
        args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    };
    let get_u64 = |key: &str| -> Option<u64> {
        args.get(key).and_then(|v| v.as_u64())
    };
    let get_bool = |key: &str| -> Option<bool> {
        args.get(key).and_then(|v| v.as_bool())
    };

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
            node::handle_node(handle, symbol, include_code).await
        }
        "codewiki_explore" => {
            let query = get_str("query").unwrap_or_default();
            let max_files = get_u64("maxFiles").map(|n| n as usize);
            explore::handle_explore(handle, query, max_files).await
        }
        "codewiki_status" => {
            status::handle_status(handle).await
        }
        "codewiki_files" => {
            let path = get_str("path");
            let pattern = get_str("pattern");
            let format = get_str("format").unwrap_or_else(|| "tree".to_string());
            let include_metadata = get_bool("includeMetadata").unwrap_or(true);
            let max_depth = get_u64("maxDepth").map(|n| n as usize);
            files::handle_files(handle, path, pattern, format, include_metadata, max_depth).await
        }
        _other => {
            return Err(rmcp::Error::method_not_found::<rmcp::model::CallToolRequestMethod>());
        }
    };

    match result {
        Ok(text) => Ok(CallToolResult::success(vec![
            rmcp::model::RawContent::text(text).no_annotation(),
        ])),
        Err(e) => {
            let msg = e.to_string();
            tracing::warn!(error = %msg, "Tool handler returned error");
            Ok(CallToolResult::error(vec![
                rmcp::model::RawContent::text(msg).no_annotation(),
            ]))
        }
    }
}

/// Truncate a string to at most `max_chars`, appending an indicator if truncated.
pub fn truncate_output(s: String, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s;
    }
    let truncated = &s[..max_chars];
    format!("{truncated}\n...(output truncated to {max_chars} characters)")
}
