//! T-403 — `CodeWikiMcpServer` — rmcp `ServerHandler` implementation.
//!
//! Initialize flow (per spec §2):
//! 1. Client sends `initialize` request.
//! 2. Server immediately returns `InitializeResult` with `MCP_PROTOCOL_VERSION`
//!    WITHOUT awaiting any background work (so the host doesn't time out).
//! 3. The live-on-save file watcher is started via `tokio::spawn` (when a
//!    `WatcherStarter` was wired and watching is enabled). Starting it is
//!    non-blocking: the watcher runs on its own dedicated background thread, so
//!    the JSON-RPC serve path is never blocked by extraction/sync work.
//!
//! T-414 — Roots protocol: if the client advertises `capabilities.roots`,
//! and no explicit project path was provided, the server requests `roots/list`
//! with a 5-second timeout on the first tool call, then falls back to cwd.
//!
//! OPT-14 — `projectPath` parameter: when a tool call includes `projectPath`,
//! the server opens (and caches) a handle to that project's `.codewiki/codewiki.db`
//! and routes the call through it instead of the default handle.

use crate::server_instructions::SERVER_INSTRUCTIONS;
use crate::tools;
use codewiki_storage::{open as open_db, QueryHandle, StorageImpl};
use rmcp::{
    handler::server::ServerHandler,
    model::{
        AnnotateAble, CallToolRequestParam, CallToolResult, Implementation, InitializeRequestParam,
        InitializeResult, ListToolsResult, PaginatedRequestParam, ProtocolVersion,
        ServerCapabilities, ToolsCapability,
    },
    service::RequestContext,
    Error as McpError, RoleServer,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// The primary MCP server struct.
///
/// Holds a reference to the shared, read-only query handle.
///
/// §4-A — Graceful "unindexed" mode: with a single machine-wide MCP
/// registration (args `serve --mcp`), the server is launched in EVERY project
/// the agent opens — including ones that were never `codewiki init`'d. Rather
/// than crash there (a dead server in those projects), the server starts with
/// `db = None`. The `initialize` handshake and `tools/list` still succeed, and
/// every `tools/call` short-circuits with a clear, structured message pointing
/// the user at `codewiki init`. A tool call that supplies an explicit
/// `projectPath` to a *different*, indexed project still works.
///
/// Live-on-save auto-sync is **not** owned here: `rmcp`'s server transport
/// consumes the `initialize` handshake internally and replies via `get_info()`
/// rather than dispatching to a handler method, so there is no
/// server-side hook from which to start a watcher. The file watcher is instead
/// started by the serve entrypoint (`codewiki serve --mcp`) for the duration of
/// the transport — see `codewiki-cli/src/commands/serve.rs`.
#[derive(Clone)]
pub struct CodeWikiMcpServer {
    /// Shared, read-only query handle (SQLite reads are thread-safe in WAL mode).
    ///
    /// `None` when the resolved project root has no `.codewiki/` index yet
    /// (§4-A unindexed mode); tool calls without an explicit `projectPath`
    /// then return the friendly "run `codewiki init`" message.
    pub(crate) db: Option<Arc<dyn QueryHandle>>,
    /// Resolved project root, used to render the unindexed-mode hint.
    project_root: PathBuf,
    /// OPT-14: Cache of per-path handles opened on demand when `projectPath` is provided.
    /// Key: canonical absolute project root path string.
    handle_cache: Arc<Mutex<HashMap<String, Arc<dyn QueryHandle>>>>,
    /// Peer handle set by rmcp after `serve` is called (allows server-initiated requests).
    peer: Arc<Mutex<Option<rmcp::Peer<RoleServer>>>>,
}

impl CodeWikiMcpServer {
    /// Create a new server wrapping the given query handle for an indexed project.
    pub fn new(db: Arc<dyn QueryHandle>) -> Self {
        Self {
            db: Some(db),
            // `project_root` is only read by `unindexed_message()`, which the
            // indexed server never reaches — this placeholder is unused here.
            project_root: PathBuf::from("."),
            handle_cache: Arc::new(Mutex::new(HashMap::new())),
            peer: Arc::new(Mutex::new(None)),
        }
    }

    /// §4-A — Create a server for a project that has no `.codewiki/` index yet.
    ///
    /// The server stays alive and responsive: the handshake succeeds and tool
    /// calls (without an explicit `projectPath`) return the friendly message
    /// produced by [`Self::unindexed_message`].
    pub fn new_unindexed(project_root: PathBuf) -> Self {
        Self {
            db: None,
            project_root,
            handle_cache: Arc::new(Mutex::new(HashMap::new())),
            peer: Arc::new(Mutex::new(None)),
        }
    }

    /// The structured, user-facing message returned by every tool when the
    /// resolved project has no index (§4-A).
    pub(crate) fn unindexed_message(&self) -> String {
        format!(
            "This project has no CodeWiki index yet. \
             Run `codewiki init` in {} to enable code intelligence.",
            self.project_root.display()
        )
    }

    /// OPT-14: Resolve the query handle to use for a given optional `projectPath`.
    ///
    /// - If `project_path` is `None` or empty, returns the default handle — or
    ///   the §4-A unindexed message when this server started without an index.
    /// - Otherwise, opens (or returns a cached handle to) the project's
    ///   `.codewiki/codewiki.db`.  Returns an error string if the path has no index.
    pub(crate) async fn resolve_handle(
        &self,
        project_path: Option<&str>,
    ) -> Result<Arc<dyn QueryHandle>, String> {
        let path_str = match project_path {
            None | Some("") => {
                return match &self.db {
                    Some(db) => Ok(db.clone()),
                    None => Err(self.unindexed_message()),
                };
            }
            Some(p) => p,
        };

        let canonical = PathBuf::from(path_str);
        let key = canonical.to_string_lossy().to_string();

        // Check cache first (non-blocking fast path)
        {
            let cache = self.handle_cache.lock().await;
            if let Some(h) = cache.get(&key) {
                return Ok(h.clone());
            }
        }

        // Build the path to the DB
        let db_path = canonical.join(".codewiki").join("codewiki.db");
        if !db_path.exists() {
            return Err(format!(
                "projectPath `{}` has no CodeWiki index. Run `codewiki init` inside that directory first.",
                path_str
            ));
        }

        let conn = open_db(&db_path)
            .map_err(|e| format!("Failed to open DB at {}: {e}", db_path.display()))?;
        let handle: Arc<dyn QueryHandle> = Arc::new(StorageImpl::new(conn, 10_000));

        // Store in cache
        {
            let mut cache = self.handle_cache.lock().await;
            // Insert-or-keep (another task may have raced us)
            let h = cache.entry(key).or_insert_with(|| handle.clone());
            Ok(h.clone())
        }
    }

    /// Build the `InitializeResult` sent to the client.
    pub fn build_initialize_result() -> InitializeResult {
        InitializeResult {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(false),
                }),
                ..Default::default()
            },
            server_info: Implementation {
                name: "codewiki".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: Some(SERVER_INSTRUCTIONS.to_string()),
        }
    }
}

impl ServerHandler for CodeWikiMcpServer {
    /// Handle a redundant in-session `initialize` request.
    ///
    /// Note: `rmcp`'s server transport handles the *startup* handshake itself
    /// (it consumes the first `initialize` message and replies via `get_info`),
    /// so this method only fires if a client re-sends `initialize` mid-session.
    /// It returns the same `InitializeResult` either path produces.
    async fn initialize(
        &self,
        _request: InitializeRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        Ok(Self::build_initialize_result())
    }

    /// Handle `tools/list` — return all 9 tool definitions.
    async fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(tools::list_tools())
    }

    /// Handle `tools/call` — dispatch to the appropriate tool handler.
    ///
    /// OPT-14: If `projectPath` is present in the arguments, resolve a per-project
    /// handle before dispatching.
    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Extract optional projectPath from arguments before dispatch
        let project_path: Option<String> = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("projectPath"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // §4-A — Unindexed mode: when this server has no default index and the
        // call did not target a different, indexed project via `projectPath`,
        // short-circuit every tool with the friendly "run `codewiki init`"
        // message. Returned as a successful (non-error) result so agents render
        // it as normal guidance rather than a tool failure.
        let targets_default = project_path.as_deref().map(str::is_empty).unwrap_or(true);
        if self.db.is_none() && targets_default {
            return Ok(rmcp::model::CallToolResult::success(vec![
                rmcp::model::RawContent::text(self.unindexed_message()).no_annotation(),
            ]));
        }

        // Deliberate asymmetry: the default-root unindexed case above returns
        // `success` (expected first-run state → guidance, not failure), whereas an
        // explicit `projectPath` that can't be resolved is a caller error and is
        // returned as `error` here.
        let db = match self.resolve_handle(project_path.as_deref()).await {
            Ok(h) => h,
            Err(msg) => {
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::RawContent::text(msg).no_annotation(),
                ]));
            }
        };

        tools::dispatch(db, request).await
    }

    fn get_peer(&self) -> Option<rmcp::Peer<RoleServer>> {
        self.peer.try_lock().ok().and_then(|g| g.clone())
    }

    fn set_peer(&mut self, peer: rmcp::Peer<RoleServer>) {
        if let Ok(mut guard) = self.peer.try_lock() {
            *guard = Some(peer);
        }
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        Self::build_initialize_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub QueryHandle for testing
    struct StubHandle;

    impl QueryHandle for StubHandle {
        fn search_nodes(
            &self,
            _query: &str,
            _opts: codewiki_storage::SearchOptions,
        ) -> Result<Vec<codewiki_core::SearchResult>, codewiki_core::CodeWikiError> {
            Ok(vec![])
        }

        fn get_node_by_id(
            &self,
            _id: &str,
        ) -> Result<Option<codewiki_core::Node>, codewiki_core::CodeWikiError> {
            Ok(None)
        }

        fn get_callers(
            &self,
            _node_id: &str,
            _depth: usize,
        ) -> Result<Vec<(codewiki_core::Node, codewiki_core::Edge)>, codewiki_core::CodeWikiError>
        {
            Ok(vec![])
        }

        fn get_callees(
            &self,
            _node_id: &str,
            _depth: usize,
        ) -> Result<Vec<(codewiki_core::Node, codewiki_core::Edge)>, codewiki_core::CodeWikiError>
        {
            Ok(vec![])
        }

        fn get_impact_radius(
            &self,
            _node_id: &str,
            _depth: usize,
        ) -> Result<codewiki_core::Subgraph, codewiki_core::CodeWikiError> {
            Ok(codewiki_core::Subgraph::default())
        }

        fn find_relevant_context(
            &self,
            _query: &str,
            _opts: codewiki_storage::FindOpts,
        ) -> Result<codewiki_core::Subgraph, codewiki_core::CodeWikiError> {
            Ok(codewiki_core::Subgraph::default())
        }

        fn get_code(&self, _node_id: &str) -> Result<Option<String>, codewiki_core::CodeWikiError> {
            Ok(None)
        }

        fn get_stats(&self) -> Result<codewiki_core::GraphStats, codewiki_core::CodeWikiError> {
            Ok(codewiki_core::GraphStats {
                journal_mode: "wal".to_string(),
                ..Default::default()
            })
        }

        fn get_files(
            &self,
            _filter: Option<&codewiki_storage::FileFilter>,
        ) -> Result<Vec<codewiki_core::FileRecord>, codewiki_core::CodeWikiError> {
            Ok(vec![])
        }

        fn get_affected_nodes(
            &self,
            _file_paths: &[std::path::PathBuf],
        ) -> Result<Vec<codewiki_core::Node>, codewiki_core::CodeWikiError> {
            Ok(vec![])
        }

        fn get_callers_aggregated(
            &self,
            _symbol: &str,
            _cap: usize,
        ) -> Result<
            codewiki_storage::traits::query::AggregatedNeighbors,
            codewiki_core::CodeWikiError,
        > {
            Ok(codewiki_storage::traits::query::AggregatedNeighbors::default())
        }

        fn get_callees_aggregated(
            &self,
            _symbol: &str,
            _cap: usize,
        ) -> Result<
            codewiki_storage::traits::query::AggregatedNeighbors,
            codewiki_core::CodeWikiError,
        > {
            Ok(codewiki_storage::traits::query::AggregatedNeighbors::default())
        }

        fn get_impact_aggregated(
            &self,
            _symbol: &str,
            _depth: usize,
            _cap: usize,
        ) -> Result<codewiki_storage::traits::query::AggregatedImpact, codewiki_core::CodeWikiError>
        {
            Ok(codewiki_storage::traits::query::AggregatedImpact::default())
        }
    }

    fn extract_text(result: &CallToolResult) -> String {
        match &result.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        }
    }

    #[tokio::test]
    async fn initialize_result_has_correct_protocol_version() {
        // Test the static builder function directly
        let result = CodeWikiMcpServer::build_initialize_result();
        assert_eq!(
            result.protocol_version,
            ProtocolVersion::V_2024_11_05,
            "initialize must return protocolVersion 2024-11-05"
        );
        assert!(
            result.instructions.is_some(),
            "initialize must include server instructions"
        );
        assert_eq!(result.server_info.name, "codewiki");
    }

    #[tokio::test]
    async fn list_tools_returns_9_tools() {
        let result = tools::list_tools();
        assert_eq!(result.tools.len(), 9, "must expose exactly 9 tools");
        let names: Vec<&str> = result.tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains(&"codewiki_search"));
        assert!(names.contains(&"codewiki_context"));
        assert!(names.contains(&"codewiki_callers"));
        assert!(names.contains(&"codewiki_callees"));
        assert!(names.contains(&"codewiki_impact"));
        assert!(names.contains(&"codewiki_node"));
        assert!(names.contains(&"codewiki_explore"));
        assert!(names.contains(&"codewiki_status"));
        assert!(names.contains(&"codewiki_files"));
    }

    #[tokio::test]
    async fn dispatch_search_empty_returns_no_results_message() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_search".into(),
            arguments: Some({
                let mut map = serde_json::Map::new();
                map.insert(
                    "query".to_string(),
                    serde_json::Value::String("test".to_string()),
                );
                map
            }),
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        assert!(result.is_error != Some(true));
        let text = extract_text(&result);
        assert!(text.contains("No results") || text.contains("Search Results"));
    }

    #[tokio::test]
    async fn dispatch_status_includes_wal_journal_mode() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_status".into(),
            arguments: None,
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        let text = extract_text(&result);
        assert!(
            text.contains("wal"),
            "status should include 'wal' journal mode"
        );
    }

    #[tokio::test]
    async fn dispatch_callers_returns_no_callers_message() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_callers".into(),
            arguments: Some({
                let mut map = serde_json::Map::new();
                map.insert(
                    "symbol".to_string(),
                    serde_json::Value::String("myFunc".to_string()),
                );
                map
            }),
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("No symbol found") || text.contains("No callers found"));
    }

    #[tokio::test]
    async fn dispatch_callees_returns_no_callees_message() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_callees".into(),
            arguments: Some({
                let mut map = serde_json::Map::new();
                map.insert(
                    "symbol".to_string(),
                    serde_json::Value::String("myFunc".to_string()),
                );
                map
            }),
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("No symbol found") || text.contains("No callees found"));
    }

    #[tokio::test]
    async fn dispatch_impact_returns_no_symbol_message() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_impact".into(),
            arguments: Some({
                let mut map = serde_json::Map::new();
                map.insert(
                    "symbol".to_string(),
                    serde_json::Value::String("CoreType".to_string()),
                );
                map
            }),
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("No symbol found") || text.contains("no dependents"));
    }

    #[tokio::test]
    async fn dispatch_files_returns_no_files_message() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_files".into(),
            arguments: None,
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("No indexed files"));
    }

    #[tokio::test]
    async fn dispatch_explore_returns_no_results_message() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_explore".into(),
            arguments: Some({
                let mut map = serde_json::Map::new();
                map.insert(
                    "query".to_string(),
                    serde_json::Value::String("AuthService".to_string()),
                );
                map
            }),
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("No relevant symbols") || text.contains("Explore:"));
    }

    #[tokio::test]
    async fn dispatch_node_returns_no_symbol_message() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_node".into(),
            arguments: Some({
                let mut map = serde_json::Map::new();
                map.insert(
                    "symbol".to_string(),
                    serde_json::Value::String("handleRequest".to_string()),
                );
                map
            }),
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        let text = extract_text(&result);
        assert!(text.contains("No symbol found"));
    }

    #[tokio::test]
    async fn dispatch_context_returns_no_symbols_message() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_context".into(),
            arguments: Some({
                let mut map = serde_json::Map::new();
                map.insert(
                    "task".to_string(),
                    serde_json::Value::String("add user authentication".to_string()),
                );
                map
            }),
        };
        let result = tools::dispatch(handle, params).await.unwrap();
        // Should include the feature-request reminder since "add" is a keyword
        let text = extract_text(&result);
        assert!(text.contains("Context for:"));
    }

    #[tokio::test]
    async fn unindexed_server_resolve_handle_returns_friendly_message() {
        // §4-A — a server started without an index must NOT crash. Resolving the
        // default handle (no projectPath) yields the friendly hint string that
        // `call_tool` surfaces to the agent.
        let server = CodeWikiMcpServer::new_unindexed(PathBuf::from("/tmp/unindexed-proj"));

        let msg = match server.resolve_handle(None).await {
            Ok(_) => panic!("default handle must be absent in unindexed mode"),
            Err(msg) => msg,
        };
        assert!(
            msg.contains("no CodeWiki index") && msg.contains("codewiki init"),
            "unindexed message must guide the user to `codewiki init`, got: {msg}"
        );
        assert!(
            msg.contains("/tmp/unindexed-proj"),
            "message must name the resolved project root, got: {msg}"
        );

        // Empty-string projectPath also routes to the default (unindexed) handle.
        assert!(server.resolve_handle(Some("")).await.is_err());

        // The handshake builder still works (server stays usable) and
        // tools/list returns the full tool set in unindexed mode.
        let init = CodeWikiMcpServer::build_initialize_result();
        assert_eq!(init.server_info.name, "codewiki");
        assert_eq!(tools::list_tools().tools.len(), 9);
    }

    #[test]
    fn unindexed_message_names_root_and_init() {
        let server = CodeWikiMcpServer::new_unindexed(PathBuf::from("/some/project"));
        let msg = server.unindexed_message();
        assert!(msg.contains("/some/project"));
        assert!(msg.contains("codewiki init"));
        assert!(msg.contains("code intelligence"));
    }

    #[tokio::test]
    async fn dispatch_unknown_tool_returns_error() {
        let handle: Arc<dyn QueryHandle> = Arc::new(StubHandle);
        let params = CallToolRequestParam {
            name: "codewiki_nonexistent".into(),
            arguments: None,
        };
        let result = tools::dispatch(handle, params).await;
        assert!(result.is_err(), "unknown tool must return error");
    }
}
