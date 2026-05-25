use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodeWikiError {
    // --- Storage layer ---
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Schema migration failed at version {version}: {source}")]
    Migration {
        version: u32,
        #[source]
        source: Box<CodeWikiError>,
    },

    // --- Extraction subsystem (A1) ---
    #[error("Extraction failed for {path}: {message}")]
    Extraction { path: String, message: String },

    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),

    // --- Resolution subsystem (A4) ---
    #[error("Resolution batch failed: {0}")]
    Resolution(String),

    // --- Sync subsystem (A5) ---
    #[error("File watcher error: {0}")]
    Watcher(String),

    #[error("Git operation failed: {0}")]
    Git(String),

    // --- MCP subsystem (A3) ---
    #[error("MCP protocol error: {0}")]
    Mcp(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    // --- Installer subsystem (A6) ---
    #[error("Installer error: {0}")]
    Installer(String),

    // --- General ---
    #[error("Node not found: {id}")]
    NodeNotFound { id: String },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}
