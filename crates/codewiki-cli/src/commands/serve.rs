// T-442 — `serve` subcommand handler.
// --mcp: starts the MCP stdio server.
// Without --mcp: prints setup instructions.

use anyhow::Result;
use codewiki_extraction::ExtractionOrchestratorImpl;
use codewiki_storage::{QueryHandle, SyncStore};
use codewiki_sync::{spawn_live_sync, LiveSyncHandle, OnSynced, WatcherConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::commands::index::StorageAdapter;
use crate::commands::util::{open_storage, run_resolution_incremental};

/// Run the `serve` subcommand.
///
/// When `mcp = true`:
///   - On Unix, installs SIGPIPE → IGN so a closed reader doesn't crash the process.
///   - Calls `codewiki_mcp::constants::init_tracing()` (already called by main but
///     this is idempotent so safe to call again here for clarity).
///   - Starts the live-on-save file watcher unless `no_watch` is set.
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

    tracing::info!(root = %project_root.display(), no_watch, "codewiki MCP server starting");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_mcp_real(project_root, no_watch))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

async fn run_mcp_real(
    project_root: PathBuf,
    no_watch: bool,
) -> Result<(), codewiki_core::CodeWikiError> {
    // §4-A — Graceful unindexed mode. With a single machine-wide MCP
    // registration (args `serve --mcp`), this server is launched in EVERY
    // project the agent opens — including ones never `codewiki init`'d. We must
    // NOT crash there. When the resolved root has no `.codewiki/codewiki.db`,
    // start the server in "unindexed" mode: the handshake still succeeds and
    // every tool call returns a friendly "run `codewiki init`" message. We do
    // NOT auto-index (silently indexing a huge/wrong cwd would be worse).
    let db_path = project_root.join(".codewiki").join("codewiki.db");

    // `_watcher` is held for the lifetime of `serve_stdio`; dropping it after
    // serve returns stops the watcher. It only runs in indexed mode — there is
    // nothing to sync in an unindexed project.
    let mut _watcher: Option<LiveSyncHandle> = None;

    let server = if index_exists(&db_path) {
        // Open the storage. This read handle is what the MCP server serves
        // queries through.
        let storage = open_storage(&project_root)
            .map_err(|e| codewiki_core::CodeWikiError::Other(e.to_string()))?;
        let query_handle: Arc<dyn QueryHandle> = Arc::new(storage);

        // Start the live-on-save watcher unless --no-watch. The watcher runs on
        // its own dedicated background thread (see `spawn_live_sync`), so it
        // never blocks the JSON-RPC serve loop below.
        //
        // The watcher is started here (around the transport) rather than from a
        // server-handler hook because `rmcp`'s server transport consumes the
        // `initialize` handshake internally and never dispatches it to the
        // handler.
        if no_watch {
            tracing::info!(
                "live-sync watcher disabled (--no-watch); index updates require manual `codewiki sync` or git hooks"
            );
        } else {
            _watcher = start_live_sync(&project_root);
        }

        codewiki_mcp::server::CodeWikiMcpServer::new(query_handle)
    } else {
        tracing::info!(
            root = %project_root.display(),
            "no CodeWiki index found — serving in unindexed mode (tools will prompt to run `codewiki init`)"
        );
        codewiki_mcp::server::CodeWikiMcpServer::new_unindexed(project_root)
    };

    // Run the MCP server on stdio.
    // The rmcp transport exits cleanly when stdin closes (host disconnect).
    codewiki_mcp::transport::serve_stdio(server).await
}

/// §4-A — Whether a usable CodeWiki index exists at `db_path`
/// (`<root>/.codewiki/codewiki.db`). When this returns `false`, the MCP server
/// is started in unindexed mode instead of crashing.
fn index_exists(db_path: &Path) -> bool {
    db_path.exists()
}

/// Build and start the live-on-save sync loop for `project_root`.
///
/// Opens a **dedicated write connection** for the watcher — separate from the
/// read handle the MCP server serves queries through — so SQLite WAL's
/// single-writer rule never collides with the serve path's concurrent readers.
/// Returns `None` (after logging) when the write connection can't be opened or
/// the watch policy disables watching (e.g. a WSL2 `/mnt/` drive, where the
/// installed git hooks remain the auto-sync mechanism).
fn start_live_sync(project_root: &Path) -> Option<LiveSyncHandle> {
    // Dedicated write connection for the watcher's sync cycles.
    let store_impl = match open_storage(project_root) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!(err = %e, "live-sync: failed to open write connection; auto-sync disabled");
            return None;
        }
    };
    let sync_db: Arc<dyn SyncStore> = Arc::clone(&store_impl) as Arc<dyn SyncStore>;
    let extractor = Arc::new(ExtractionOrchestratorImpl::new(Arc::new(
        StorageAdapter::new(Arc::clone(&store_impl)),
    )));

    // Post-sync hook: run incremental resolution over the changed paths. Runs
    // on the watcher thread (single writer), so it reuses the same connection
    // without contending with the serve path's readers.
    let resolution_root = project_root.to_path_buf();
    let resolution_store = Arc::clone(&store_impl);
    let on_synced: OnSynced = Arc::new(move |result| {
        match run_resolution_incremental(&resolution_store, &resolution_root, &result.changed_paths)
        {
            Ok(n) if n > 0 => tracing::info!(resolved = n, "live-sync resolved references"),
            Ok(_) => {}
            Err(e) => tracing::warn!(err = %e, "live-sync incremental resolution failed"),
        }
    });

    match spawn_live_sync(
        project_root.to_path_buf(),
        sync_db,
        extractor,
        WatcherConfig::default(),
        on_synced,
    ) {
        Ok(handle) => {
            tracing::info!(root = %project_root.display(), "live-sync watcher active");
            Some(handle)
        }
        Err(reason) => {
            // Watch policy disabled (e.g. WSL2 /mnt drive) or init failed.
            // Not fatal — git hooks remain the auto-sync mechanism there.
            tracing::info!(reason = %reason, "live-sync watcher not started (git hooks remain the auto-sync path)");
            None
        }
    }
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

    /// §4-A — a directory with no `.codewiki/` index is detected as unindexed,
    /// so `serve --mcp` starts in graceful unindexed mode rather than crashing.
    /// (The unindexed tool-call message itself is asserted in
    /// `codewiki-mcp/src/server.rs`, which can reach the `pub(crate)` internals.)
    #[test]
    fn unindexed_dir_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(".codewiki").join("codewiki.db");
        assert!(
            !index_exists(&db_path),
            "a fresh dir with no .codewiki/ must be reported unindexed"
        );

        // The server `serve --mcp` constructs for this root in unindexed mode is
        // buildable and its handshake builder works — i.e. the server stays
        // usable instead of crashing on a missing DB.
        let _server =
            codewiki_mcp::server::CodeWikiMcpServer::new_unindexed(dir.path().to_path_buf());
        let init = codewiki_mcp::server::CodeWikiMcpServer::build_initialize_result();
        assert_eq!(init.server_info.name, "codewiki");
    }
}
