//! T-402 — Stdio JSON-RPC framing via `rmcp`.
//!
//! The `rmcp` crate handles the low-level framing (one JSON object per line,
//! newline-delimited). This module provides a thin helper to start the server
//! on stdio.
//!
//! Stdout is **exclusively** for JSON-RPC messages.
//! Stderr is **exclusively** for tracing/logging output.

use crate::server::CodeWikiMcpServer;
use codewiki_core::CodeWikiError;
use rmcp::ServiceExt;

/// Start the MCP server on stdio (tokio stdin + stdout).
///
/// This function blocks until the transport closes (client disconnects or
/// watchdog cancels).
pub async fn serve_stdio(server: CodeWikiMcpServer) -> Result<(), CodeWikiError> {
    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let service = server
        .serve(transport)
        .await
        .map_err(|e| CodeWikiError::Mcp(format!("MCP transport error: {e}")))?;

    // Wait for the service to complete (client disconnects).
    service
        .waiting()
        .await
        .map_err(|e| CodeWikiError::Mcp(format!("MCP service error: {e}")))?;

    Ok(())
}
