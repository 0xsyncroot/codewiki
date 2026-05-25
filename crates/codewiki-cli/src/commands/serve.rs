// T-442 — `serve` subcommand handler.
// --mcp: starts the MCP stdio server.
// Without --mcp: prints setup instructions.

use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use codewiki_storage::QueryHandle;

/// Run the `serve` subcommand.
///
/// When `mcp = true`:
///   - On Unix, installs SIGPIPE → IGN so a closed reader doesn't crash the process.
///   - Calls `codewiki_mcp::constants::init_tracing()` (already called by main but
///     this is idempotent so safe to call again here for clarity).
///   - Runs the real `CodeWikiMcpServer` on stdio via `codewiki_mcp::transport::serve_stdio`.
///
/// When `mcp = false`:
///   - Prints instructions for adding codewiki as an MCP server.
pub fn run(mcp: bool, path: Option<PathBuf>, no_watch: bool) -> Result<()> {
    codewiki_mcp::constants::init_tracing();

    if !mcp {
        println!("To use CodeWiki as an MCP server, add the following to your IDE config:");
        println!();
        println!("  Claude Code:  codewiki install --target claude");
        println!("  Cursor:       codewiki install --target cursor");
        println!("  Codex CLI:    codewiki install --target codex");
        println!("  opencode:     codewiki install --target opencode");
        println!("  Hermes:       codewiki install --target hermes");
        println!();
        println!("Or run: codewiki serve --mcp [--path <project-root>]");
        return Ok(());
    }

    // Install SIGPIPE → IGN on Unix so a closed MCP host doesn't kill us.
    codewiki_mcp::ppid_watchdog::ignore_sigpipe();

    let project_root = path
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    tracing::info!(root = %project_root.display(), "codewiki MCP server starting");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_mcp_real(project_root, no_watch))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

async fn run_mcp_real(
    project_root: PathBuf,
    _no_watch: bool,
) -> Result<(), codewiki_core::CodeWikiError> {
    // Open the storage (requires `codewiki init` to have been run).
    let storage = crate::commands::util::open_storage(&project_root)
        .map_err(|e| codewiki_core::CodeWikiError::Other(e.to_string()))?;
    let query_handle: Arc<dyn QueryHandle> = Arc::new(storage);

    // Build and run the real MCP server on stdio.
    // The rmcp transport exits cleanly when stdin closes (host disconnect).
    let server = codewiki_mcp::server::CodeWikiMcpServer::new(query_handle);
    codewiki_mcp::transport::serve_stdio(server).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_without_mcp_returns_ok() {
        // The non-MCP path just prints text and exits; no side effects.
        let result = run(false, None, false);
        assert!(result.is_ok());
    }
}
