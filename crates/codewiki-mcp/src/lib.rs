//! codewiki-mcp — MCP server, 9 tool handlers, constants.
//!
//! Public surface:
//! - [`CodeWikiMcpServer`] — rmcp `ServerHandler` impl
//! - [`constants`] — `MCP_SERVE_ARGS_*`, `MCP_PROTOCOL_VERSION`, `MCP_BINARY_NAME`
//! - [`init_tracing`] — idempotent tracing setup (exported from constants)
//! - [`config`] — re-export of `CodeWikiConfig`

pub mod config;
pub mod constants;
pub mod input_limits;
pub mod ppid_watchdog;
pub mod server;
pub mod server_instructions;
pub mod tools;
pub mod transport;

// Re-export tracing init from constants (A6 calls codewiki_mcp::constants::init_tracing)
pub use constants::init_tracing;
pub use server::CodeWikiMcpServer;
