// T-430 — `index` subcommand: full re-index of a project.

use anyhow::Result;
use codewiki_extraction::ExtractionOrchestratorImpl;
use codewiki_storage::{ExtractionStore as StorageTrait, StorageImpl};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::commands::util::{open_storage, resolve_root, run_resolution};
use crate::ui::shimmer::{IndexProgress, Phase, ShimmerProgress};

// ---------------------------------------------------------------------------
// Bridge: codewiki-extraction's ExtractionStore → codewiki-storage StorageImpl
// ---------------------------------------------------------------------------

/// Thin adapter so `StorageImpl` (which implements `codewiki_storage::ExtractionStore`)
/// can be passed to `ExtractionOrchestratorImpl` (which wants
/// `codewiki_extraction::ExtractionStore`).
///
/// The `flush_bulk` override wires the OPT-7 bulk-insert path: for each
/// sub-batch of up to 200 files, FTS triggers are dropped before the insert
/// and rebuilt afterwards so that per-row trigger overhead is eliminated for
/// large initial indexes. `run_maintenance` is called once at the very end.
pub struct StorageAdapter(pub Arc<StorageImpl>);

impl codewiki_extraction::ExtractionStore for StorageAdapter {
    fn store_batch(&self, batch: codewiki_core::ExtractionBatch) -> Result<(), String> {
        self.0
            .store_extraction_batch(batch)
            .map_err(|e| e.to_string())
    }

    fn delete_file(&self, path: &Path) -> Result<(), String> {
        StorageTrait::delete_file(&*self.0, path).map_err(|e| e.to_string())
    }

    /// Bulk-insert path: calls `store_extraction_batch_bulk_init` which drops
    /// FTS triggers, inserts all nodes in one transaction, then rebuilds FTS.
    fn flush_bulk(
        &self,
        batches: Vec<codewiki_core::ExtractionBatch>,
    ) -> Result<(usize, usize, usize), String> {
        let stats = self
            .0
            .store_extraction_batch_bulk_init(batches)
            .map_err(|e| e.to_string())?;
        Ok((stats.files_written + stats.files_skipped, stats.nodes_inserted, stats.edges_inserted))
    }
}

pub fn run(path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = Arc::new(open_storage(&root)?);

    tracing::info!(root = %root.display(), "starting full index");

    let bar = ShimmerProgress::new();
    bar.on_progress(&IndexProgress {
        phase: Phase::Scanning,
        percent: 0,
        count: 0,
    });

    let store_adapter = Arc::new(StorageAdapter(Arc::clone(&storage)));
    let orchestrator = ExtractionOrchestratorImpl::new(store_adapter);

    let t0 = Instant::now();

    bar.on_progress(&IndexProgress {
        phase: Phase::Parsing,
        percent: 30,
        count: 0,
    });

    let counts = orchestrator.index_all(&root);

    bar.on_progress(&IndexProgress {
        phase: Phase::Storing,
        percent: 80,
        count: counts.files as u64,
    });

    bar.finish(counts.files as u64);

    // Run WAL checkpoint + PRAGMA optimize after bulk insert (OPT-7).
    storage.run_maintenance_pub();

    // Run framework extraction + reference resolution to promote unresolved_refs
    // into real import/call/route edges (AUDIT-2/4/7 blocker fix).
    // Pass None → full framework-extract run (init/index always re-extract).
    let resolved_count = run_resolution(&storage, &root, None)?;
    let elapsed = t0.elapsed();

    println!(
        "indexed {} files, {} nodes, {} edges in {:.1}s",
        counts.files,
        counts.nodes,
        counts.edges,
        elapsed.as_secs_f64()
    );
    println!("resolved {} references", resolved_count);
    Ok(())
}
