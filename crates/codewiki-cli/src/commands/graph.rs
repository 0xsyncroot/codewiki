// `graph` subcommand — launch the interactive graph web UI.
//
// The `codewiki-graph` crate is always a dependency of codewiki-cli, but the
// server code only compiles when the `web` feature is enabled.  Without `web`,
// calling this command prints an informative error instead of a compile failure.

use std::path::PathBuf;

pub fn run(
    port: u16,
    path: Option<PathBuf>,
    no_open: bool,
    max_nodes: usize,
) -> anyhow::Result<()> {
    #[cfg(feature = "web")]
    {
        run_web(port, path, no_open, max_nodes)
    }
    #[cfg(not(feature = "web"))]
    {
        let _ = (port, path, no_open, max_nodes);
        anyhow::bail!(
            "The graph UI requires the `web` feature. Rebuild with:\n  \
             cargo build --features codewiki-cli/web"
        );
    }
}

#[cfg(feature = "web")]
fn run_web(
    port: u16,
    path: Option<PathBuf>,
    no_open: bool,
    max_nodes: usize,
) -> anyhow::Result<()> {
    let project_root = path
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let db_path = project_root.join(".codewiki").join("codewiki.db");
    if !db_path.exists() {
        anyhow::bail!(
            "No CodeWiki index found at {}.\nRun `codewiki init` first.",
            db_path.display()
        );
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(
        codewiki_graph::GraphServer::new(db_path)
            .port(port)
            .no_open(no_open)
            .max_nodes(max_nodes)
            .serve(),
    )
}
