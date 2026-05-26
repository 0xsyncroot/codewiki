// T-430 / T-431 — CLI entry point + clap subcommand definitions.
// D5 — added `setup` and `doctor` commands; help grouping via next_help_heading.
// Call init_tracing() at the very top so every downstream tracing::* call has a subscriber.

use clap::{Parser, Subcommand};

mod commands;
mod installer;
mod ui;

/// CodeWiki — tree-sitter knowledge graph for AI coding agents
#[derive(Debug, Parser)]
#[command(
    name = "codewiki",
    version,
    about = "CodeWiki — tree-sitter knowledge graph for AI coding agents",
    long_about = "CodeWiki — tree-sitter knowledge graph for AI coding agents\n\nRun `codewiki <COMMAND> --help` for command-specific help.",
    after_help = "Run `codewiki <COMMAND> --help` for command-specific help.",
    disable_help_subcommand = true
)]
struct Cli {
    /// Reduce output to errors only
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Increase log verbosity
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    // ── Common ────────────────────────────────────────────────────────────────
    /// [START HERE] Index this project and wire your AI agent
    #[command(next_help_heading = "Common")]
    Setup {
        /// Skip all prompts and accept defaults
        #[arg(short, long)]
        yes: bool,

        /// Comma-separated agent targets: auto | all | claude,cursor,…
        #[arg(long, default_value = "auto")]
        target: String,

        /// Path to project root (defaults to cwd)
        #[arg(long)]
        path: Option<std::path::PathBuf>,

        /// Use ASCII glyphs instead of Unicode
        #[arg(long)]
        ascii: bool,
    },

    /// Show index status and statistics
    #[command(next_help_heading = "Common")]
    Status {
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// Run diagnostics and verify your installation
    #[command(next_help_heading = "Common")]
    Doctor {
        /// Path to project root (defaults to cwd)
        #[arg(long)]
        path: Option<std::path::PathBuf>,

        /// Exit with non-zero status if any check fails (useful in CI)
        #[arg(long)]
        strict: bool,
    },

    /// Search for a symbol by name
    #[command(next_help_heading = "Common")]
    Query {
        /// Search term (name, kind:, lang: qualifiers supported)
        query: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// Get AI-focused context for a task description
    #[command(next_help_heading = "Common")]
    Context {
        /// Task or question
        task: String,
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    // ── Advanced ──────────────────────────────────────────────────────────────
    /// Create + index this project's .codewiki/ (DB + git hooks)
    #[command(next_help_heading = "Advanced")]
    Init {
        /// Interactive mode: prompts for options
        #[arg(short = 'i', long)]
        interactive: bool,

        /// Path to project root (defaults to cwd)
        #[arg(long)]
        path: Option<std::path::PathBuf>,

        /// Skip auto-index after initialization
        #[arg(long)]
        no_index: bool,
    },

    /// Rebuild the full index (run `codewiki init` first)
    #[command(next_help_heading = "Advanced")]
    Index {
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// Sync changed files into the index
    #[command(next_help_heading = "Advanced")]
    Sync {
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// Start the MCP server or print setup instructions
    #[command(next_help_heading = "Advanced")]
    Serve {
        /// Start as MCP stdio server (used by IDE integrations)
        #[arg(long)]
        mcp: bool,

        /// Project root (defaults to cwd)
        #[arg(long)]
        path: Option<std::path::PathBuf>,

        /// Disable file watcher
        #[arg(long)]
        no_watch: bool,
    },

    /// List files in the index
    #[command(next_help_heading = "Advanced")]
    Files {
        /// Path prefix filter
        #[arg(long)]
        prefix: Option<String>,
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// List callers of a symbol
    #[command(next_help_heading = "Advanced")]
    Callers {
        /// Symbol name
        name: String,
        /// Traversal depth
        #[arg(short, long, default_value = "2")]
        depth: usize,
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// List callees of a symbol
    #[command(next_help_heading = "Advanced")]
    Callees {
        /// Symbol name
        name: String,
        /// Traversal depth
        #[arg(short, long, default_value = "2")]
        depth: usize,
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// Show impact radius of changing a symbol
    #[command(next_help_heading = "Advanced")]
    Impact {
        /// Symbol name
        name: String,
        /// Traversal depth (1-10)
        #[arg(short, long, default_value = "3")]
        depth: u8,
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// Show symbols affected by a set of files
    #[command(next_help_heading = "Advanced")]
    Affected {
        /// Files to check (space-separated)
        files: Vec<std::path::PathBuf>,
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    // ── Management ────────────────────────────────────────────────────────────
    /// Wire CodeWiki into an AI agent (machine-wide by default; --location local to pin this project)
    #[command(next_help_heading = "Management")]
    Install {
        /// Skip all prompts and use defaults
        #[arg(short, long)]
        yes: bool,

        /// Comma-separated target IDs: auto | all | none | claude,cursor,…
        #[arg(long)]
        target: Option<String>,

        /// Config location: global (user-wide) or local (this project)
        #[arg(long, value_parser = ["global", "local"])]
        location: Option<String>,

        /// Use ASCII glyphs instead of Unicode
        #[arg(long)]
        ascii: bool,
    },

    /// Remove CodeWiki from AI agent configurations
    #[command(next_help_heading = "Management")]
    Uninstall {
        /// Skip all prompts and use defaults
        #[arg(short, long)]
        yes: bool,

        /// Comma-separated target IDs
        #[arg(long)]
        target: Option<String>,

        /// Config location: global | local
        #[arg(long, value_parser = ["global", "local"])]
        location: Option<String>,
    },

    /// Remove the .codewiki/ index from this project
    #[command(next_help_heading = "Management")]
    Uninit {
        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,
        /// Path to project root
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    // ── Graph UI ──────────────────────────────────────────────────────────────
    /// Launch the interactive graph web UI
    #[command(next_help_heading = "Advanced")]
    Graph {
        /// TCP port to bind (default: 7007)
        #[arg(long, default_value = "7007")]
        port: u16,

        /// Path to project root (defaults to cwd)
        #[arg(long)]
        path: Option<std::path::PathBuf>,

        /// Do not open the browser automatically
        #[arg(long)]
        no_open: bool,

        /// Hard cap on nodes returned per subgraph endpoint
        #[arg(long, default_value = "2000")]
        max_nodes: usize,
    },

    /// Export index to a portable SQLite file
    #[command(next_help_heading = "Management")]
    Snapshot {
        /// Output file path
        #[arg(short, long, default_value = "codewiki-snapshot.db")]
        output: std::path::PathBuf,

        /// Project root (defaults to cwd)
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },

    /// Restore index from a snapshot file
    #[command(next_help_heading = "Management")]
    Restore {
        /// Snapshot file to restore from
        file: std::path::PathBuf,

        /// Project root (defaults to cwd)
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },
}

fn main() {
    // T-442: init_tracing MUST be first so every tracing::* call below has a subscriber.
    codewiki_mcp::constants::init_tracing();

    let cli = Cli::parse();

    // Adjust log level for --quiet / --verbose
    // (init_tracing already reads CODEWIKI_LOG; these flags are additive overrides
    // that a future version can wire via a second subscriber init call.)

    let result = run(cli);
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Setup {
            yes,
            target,
            path,
            ascii,
        } => commands::setup::run(yes, target, path, ascii),
        Commands::Doctor { path, strict } => commands::doctor::run(path, strict),
        Commands::Serve {
            mcp,
            path,
            no_watch,
        } => commands::serve::run(mcp, path, no_watch),
        Commands::Install {
            yes,
            target,
            location,
            ascii,
        } => {
            let opts = installer::InstallerOpts {
                yes,
                target: target.unwrap_or_else(|| "auto".to_string()),
                location: location.unwrap_or_else(|| "global".to_string()),
                ascii,
                auto_allow: true,
            };
            installer::run_installer(opts)
        }
        Commands::Uninstall {
            yes,
            target,
            location,
        } => {
            let opts = installer::UninstallOpts {
                yes,
                target: target.unwrap_or_else(|| "all".to_string()),
                location: location.unwrap_or_else(|| "global".to_string()),
            };
            installer::run_uninstaller(opts)
        }
        Commands::Init { path, no_index, .. } => commands::init::run(path, no_index),
        Commands::Uninit { force, path } => commands::uninit::run(force, path),
        Commands::Index { path } => commands::index::run(path),
        Commands::Sync { path } => commands::sync::run(path),
        Commands::Query { query, limit, path } => commands::query::run(query, limit, path),
        Commands::Context { task, path } => commands::context::run(task, path),
        Commands::Callers { name, depth, path } => commands::callers::run(name, depth, path),
        Commands::Callees { name, depth, path } => commands::callees::run(name, depth, path),
        Commands::Impact { name, depth, path } => commands::impact::run(name, depth, path),
        Commands::Affected { files, path } => commands::affected::run(files, path),
        Commands::Files { prefix, path } => commands::files::run(prefix, path),
        Commands::Status { path } => commands::status::run(path),
        Commands::Snapshot { output, path } => commands::snapshot::run_snapshot(output, path),
        Commands::Restore { file, path } => commands::snapshot::run_restore(file, path),
        Commands::Graph {
            port,
            path,
            no_open,
            max_nodes,
        } => commands::graph::run(port, path, no_open, max_nodes),
    }
}
