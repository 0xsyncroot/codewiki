// T-430 — `sync` subcommand: sync stale files into the index.

use anyhow::Result;
use codewiki_extraction::ExtractionOrchestratorImpl;
use codewiki_storage::SyncStore;
use codewiki_sync::{sync_loop::run_sync_cycle, tree_cache::TreeCache};
use std::path::PathBuf;
use std::sync::Arc;

use crate::commands::index::StorageAdapter;
use crate::commands::util::{open_storage, resolve_root, run_resolution_incremental};

pub fn run(path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let codewiki_dir = root.join(".codewiki");

    // Open a single StorageImpl and share it for both sync and resolution.
    let store_impl = Arc::new(open_storage(&root)?);
    let sync_storage: Arc<dyn SyncStore> = Arc::clone(&store_impl) as Arc<dyn SyncStore>;
    let store_adapter = Arc::new(StorageAdapter::new(Arc::clone(&store_impl)));
    let extractor = Arc::new(ExtractionOrchestratorImpl::new(store_adapter));
    let tree_cache = TreeCache::new(128);

    let result = run_sync_cycle(
        &sync_storage,
        &extractor,
        vec![],
        &tree_cache,
        &codewiki_dir,
        &root,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    if result.is_noop() {
        println!("Nothing to sync — index is up to date.");
        return Ok(());
    }

    println!(
        "Sync complete: {} added, {} modified, {} removed ({} ms)",
        result.files_added, result.files_modified, result.files_removed, result.duration_ms
    );

    // OPT-9: Use incremental resolution to scope the resolution pass to only
    // the refs from changed files and their dependents.  Falls back to the
    // full `run_until_empty` when > 10 % of indexed files changed.
    // Framework extraction (OPT-10) is still skipped via `run_resolution`
    // when it is called as the fallback, because `run_resolution_incremental`
    // delegates to `run_resolution(…, Some(changed_paths))` in that case.
    let resolved_count = run_resolution_incremental(&store_impl, &root, &result.changed_paths)?;
    if resolved_count > 0 {
        println!("Resolved {} references", resolved_count);
    }

    Ok(())
}
