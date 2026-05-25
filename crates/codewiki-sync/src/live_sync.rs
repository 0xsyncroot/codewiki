//! Live-on-save sync: drive `run_sync_cycle` from debounced `FileWatcher`
//! events on a dedicated background thread.
//!
//! This is the runtime that turns the [`FileWatcher`](crate::watcher::FileWatcher)
//! and [`run_sync_cycle`](crate::sync_loop::run_sync_cycle) from independent
//! building blocks into a working auto-sync mechanism. It is started by the
//! MCP server (`codewiki serve --mcp`) so edits are reflected in the index
//! without the user running `codewiki sync` by hand.
//!
//! ## Threading
//!
//! [`spawn_live_sync`] starts the watcher on a **dedicated OS thread** with its
//! own current-thread Tokio runtime. This is deliberate:
//!
//! - The MCP stdio JSON-RPC serve loop runs on its own runtime and must NEVER
//!   be blocked by the (synchronous, CPU-bound) extraction/sync work.
//! - `run_sync_cycle` is synchronous and can take tens of milliseconds; running
//!   it on the watcher's dedicated thread keeps it off any shared executor.
//!
//! ## Storage connection
//!
//! The caller passes a **separate write connection** (`Arc<dyn SyncStore>`)
//! distinct from the read handles the MCP server serves queries through. SQLite
//! WAL mode permits one writer plus concurrent readers, so the watcher's
//! `run_sync_cycle` writes never block the serve path's reads.
//!
//! ## Watch policy
//!
//! [`FileWatcher::start`] consults [`watch_disabled_reason`](crate::watch_policy::watch_disabled_reason)
//! and returns `Err` when watching is disabled for the path (e.g. a WSL2
//! `/mnt/<drive>` mount, where the installed git hooks remain the auto-sync
//! mechanism). [`spawn_live_sync`] surfaces that as `Err`; the caller logs it
//! and continues serving — it is not a fatal error.

use crate::sync_loop::{run_sync_cycle, SyncResult};
use crate::tree_cache::TreeCache;
use crate::watcher::{FileWatcher, WatcherConfig};
use codewiki_extraction::ExtractionOrchestratorImpl;
use codewiki_storage::SyncStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::{broadcast, oneshot};

/// Callback invoked after each non-noop sync cycle, with the cycle's result.
///
/// The MCP server uses this to run the (CLI-owned) incremental resolution pass,
/// which lives outside this crate. It runs on the watcher thread, so it shares
/// the same single-writer guarantee as `run_sync_cycle`.
pub type OnSynced = Arc<dyn Fn(&SyncResult) + Send + Sync>;

/// A running live-sync background thread.
///
/// Dropping the handle signals the watcher thread to stop and joins it, so the
/// watcher's lifetime is tied to whatever owns this handle (the MCP server).
pub struct LiveSyncHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl LiveSyncHandle {
    /// Signal the watcher thread to stop and join it.
    ///
    /// Called automatically on drop; exposed for explicit shutdown in tests.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            // Best-effort: if the receiver is already gone the thread has
            // exited on its own. Sending wakes the `select!` in the watcher
            // loop even when no file change is pending.
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for LiveSyncHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start the live-sync watcher loop on a dedicated background thread.
///
/// Returns a [`LiveSyncHandle`] whose drop stops the watcher, or `Err(reason)`
/// when the watch policy disables watching for `project_root` or the underlying
/// notify watcher fails to initialise. The caller should log the error and keep
/// running (git hooks cover auto-sync where the watcher is disabled).
///
/// - `project_root` — the project being watched (its `.codewiki/` subdir holds
///   the lock + virtual route manifests).
/// - `db` — a **dedicated write connection** (`SyncStore`), separate from any
///   read handle the caller serves queries through.
/// - `extractor` — extraction orchestrator wired to the same DB.
/// - `config` — debounce window etc.
/// - `on_synced` — post-sync hook (e.g. incremental resolution).
pub fn spawn_live_sync(
    project_root: PathBuf,
    db: Arc<dyn SyncStore>,
    extractor: Arc<ExtractionOrchestratorImpl>,
    config: WatcherConfig,
    on_synced: OnSynced,
) -> Result<LiveSyncHandle, String> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    // Bounded broadcast channel for framework-config events; we don't consume
    // them here (resolution re-detects frameworks), but FileWatcher requires a
    // sender. Keep a subscriber alive inside the thread so sends never error.
    let (framework_tx, _framework_rx) = broadcast::channel(16);

    // Start the watcher up-front (on the calling thread) so policy/init errors
    // are reported synchronously to the caller rather than swallowed in a
    // detached thread.
    let watcher = FileWatcher::start(project_root.clone(), config, framework_tx)?;

    let codewiki_dir = project_root.join(".codewiki");
    let join = std::thread::Builder::new()
        .name("codewiki-live-sync".to_string())
        .spawn(move || {
            watcher_thread(
                watcher,
                db,
                extractor,
                codewiki_dir,
                project_root,
                on_synced,
                shutdown_rx,
            );
        })
        .map_err(|e| format!("failed to spawn live-sync thread: {e}"))?;

    Ok(LiveSyncHandle {
        shutdown_tx: Some(shutdown_tx),
        join: Some(join),
    })
}

/// Body of the dedicated watcher thread.
///
/// Runs a current-thread Tokio runtime to await debounced change batches from
/// the watcher, then runs `run_sync_cycle` synchronously on this thread for
/// each batch. Stops when the shutdown channel fires or the watcher's sender
/// drops.
fn watcher_thread(
    mut watcher: FileWatcher,
    db: Arc<dyn SyncStore>,
    extractor: Arc<ExtractionOrchestratorImpl>,
    codewiki_dir: PathBuf,
    project_root: PathBuf,
    on_synced: OnSynced,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(err = %e, "live-sync: failed to build watcher runtime; auto-sync disabled");
            return;
        }
    };

    let tree_cache = TreeCache::new(128);
    tracing::info!(root = %project_root.display(), "live-sync watcher started");

    rt.block_on(async move {
        let mut shutdown_rx = shutdown_rx;
        loop {
            // Await either the next debounced change batch or a shutdown signal.
            // Using `select!` (rather than polling) means the loop wakes
            // immediately on shutdown even when no file change is pending — so
            // `LiveSyncHandle::stop` joins promptly.
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                batch = watcher.changes_rx.recv() => {
                    // `None` means the watcher's sender dropped (debouncer
                    // gone) → stop.
                    if batch.is_none() {
                        break;
                    }
                }
            }

            // Drain any additional batches that arrived while we were idle, so
            // a burst collapses into a single sync cycle. The actual change set
            // is rediscovered by `run_sync_cycle`'s full walk + DB diff (which
            // also catches removals), so the batch contents are only a trigger.
            while watcher.changes_rx.try_recv().is_ok() {}

            // Mark syncing so overlapping triggers can observe it (the advisory
            // lock in run_sync_cycle is the real mutual-exclusion guarantee).
            watcher
                .syncing
                .store(true, std::sync::atomic::Ordering::SeqCst);

            // run_sync_cycle is synchronous + CPU-bound; on this dedicated
            // thread it never touches the MCP serve runtime.
            let result = run_sync_cycle(
                &db,
                &extractor,
                Vec::new(),
                &tree_cache,
                &codewiki_dir,
                &project_root,
            );

            watcher
                .syncing
                .store(false, std::sync::atomic::Ordering::SeqCst);

            match result {
                Ok(r) if !r.is_noop() => {
                    tracing::info!(
                        added = r.files_added,
                        modified = r.files_modified,
                        removed = r.files_removed,
                        duration_ms = r.duration_ms,
                        "live-sync reindexed changed files"
                    );
                    // Post-sync hook (e.g. incremental resolution). Failures
                    // here are logged by the hook itself; never crash.
                    (on_synced)(&r);
                }
                Ok(_) => {
                    // Noop (e.g. only .codewiki/ churn slipped through) — quiet.
                }
                Err(e) => {
                    // Never crash the watcher on a sync error — log and keep going.
                    tracing::warn!(err = %e, "live-sync cycle failed; will retry on next change");
                }
            }
        }
        tracing::info!("live-sync watcher stopped");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_stop_is_idempotent() {
        // A handle with no live thread (constructed via stop-after-drop path)
        // must tolerate repeated stop() calls without panicking.
        let (tx, _rx) = oneshot::channel::<()>();
        let mut h = LiveSyncHandle {
            shutdown_tx: Some(tx),
            join: None,
        };
        h.stop();
        h.stop();
    }
}
