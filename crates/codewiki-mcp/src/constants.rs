// T-402 — MCP constants exported for use by the CLI installer.
// T-415 — init_tracing() is also housed here (small stable module).

use std::sync::OnceLock;

/// The canonical args written into every IDE MCP config that relies on shell CWD
/// (Claude Code, VSCode extension).
pub const MCP_SERVE_ARGS_GLOBAL: &[&str] = &["serve", "--mcp"];

/// Args written into IDE configs that do NOT inherit shell CWD (Cursor, opencode).
/// Installer appends the absolute project path as a fourth element.
pub const MCP_SERVE_ARGS_WITH_PATH: &[&str] = &["serve", "--mcp", "--path"];

/// The binary name as written into every IDE config.
pub const MCP_BINARY_NAME: &str = "codewiki";

/// MCP protocol version string used in the initialize response.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

// ── Tracing initialisation (T-415) ──────────────────────────────────────────

static TRACING_INIT: OnceLock<()> = OnceLock::new();

/// Initialise the global tracing subscriber exactly once.
///
/// - Reads `CODEWIKI_LOG` env var (falls back to `warn`).
/// - Writes to **stderr** only — stdout is reserved for MCP JSON-RPC.
/// - `ansi = false` keeps the output machine-readable in non-TTY contexts.
/// - Calling this function a second time is a no-op (idempotent).
pub fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        use tracing_subscriber::{fmt, EnvFilter};

        let filter =
            EnvFilter::try_from_env("CODEWIKI_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));

        fmt::Subscriber::builder()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .compact()
            .init();
    });
}
