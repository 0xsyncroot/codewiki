// T-430 — `index` subcommand: full re-index of a project.

use anyhow::Result;
use codewiki_extraction::ExtractionOrchestratorImpl;
use codewiki_storage::{ExtractionStore as StorageTrait, QueryHandle, StorageImpl};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::commands::util::{open_storage, resolve_root, run_resolution};
use crate::ui::shimmer::{IndexProgress, Phase, ProgressReporter};

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
pub struct StorageAdapter {
    pub storage: Arc<StorageImpl>,
    /// Live progress sink. The orchestrator has no per-file callback, but it
    /// flushes extraction batches as it goes — we tap that path to stream the
    /// current file + running node/edge counts into the progress UI.
    pub progress: Option<ProgressReporter>,
}

impl StorageAdapter {
    pub fn new(storage: Arc<StorageImpl>) -> Self {
        Self {
            storage,
            progress: None,
        }
    }

    pub fn with_progress(storage: Arc<StorageImpl>, progress: ProgressReporter) -> Self {
        Self {
            storage,
            progress: Some(progress),
        }
    }
}

impl codewiki_extraction::ExtractionStore for StorageAdapter {
    /// Total source files about to stream — size the determinate progress bar.
    fn begin_index(&self, total_files: usize) {
        if let Some(p) = &self.progress {
            p.set_total(total_files as u64);
        }
    }

    fn store_batch(&self, batch: codewiki_core::ExtractionBatch) -> Result<(), String> {
        if let Some(p) = &self.progress {
            p.on_batch(
                &batch.file.path.to_string_lossy(),
                batch.nodes.len() as u64,
                batch.edges.len() as u64,
            );
        }
        self.storage
            .store_extraction_batch(batch)
            .map_err(|e| e.to_string())
    }

    fn delete_file(&self, path: &Path) -> Result<(), String> {
        StorageTrait::delete_file(&*self.storage, path).map_err(|e| e.to_string())
    }

    /// Real source files only — the synthetic `.codewiki/routes/…` manifests
    /// have no disk presence and must never be pruned as "vanished".
    fn known_files(&self) -> Vec<std::path::PathBuf> {
        self.storage
            .get_files(None)
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.path)
            .filter(|p| !p.to_string_lossy().contains("/.codewiki/routes/"))
            .collect()
    }

    /// Bulk-insert path: calls `store_extraction_batch_bulk_init` which drops
    /// FTS triggers, inserts all nodes in one transaction, then rebuilds FTS.
    fn flush_bulk(
        &self,
        batches: Vec<codewiki_core::ExtractionBatch>,
    ) -> Result<(usize, usize, usize), String> {
        // Stream each file in the sub-batch through the progress UI before the
        // bulk insert. The reporter throttles redraws internally, so feeding it
        // every file here is cheap even on very large repos.
        if let Some(p) = &self.progress {
            for b in &batches {
                p.on_batch(
                    &b.file.path.to_string_lossy(),
                    b.nodes.len() as u64,
                    b.edges.len() as u64,
                );
            }
        }
        let stats = self
            .storage
            .store_extraction_batch_bulk_init(batches)
            .map_err(|e| e.to_string())?;
        Ok((
            stats.files_written + stats.files_skipped,
            stats.nodes_inserted,
            stats.edges_inserted,
        ))
    }
}

pub fn run(path: Option<PathBuf>) -> Result<()> {
    let root = resolve_root(path);
    let storage = Arc::new(open_storage(&root)?);

    // Persist the project root so query tools render workspace-relative paths
    // (stored paths are forward-slash normalized on every platform).
    storage.set_root_path(&root.to_string_lossy().replace('\\', "/"))?;

    tracing::info!(root = %root.display(), "starting full index");

    let reporter = ProgressReporter::new();
    reporter.on_progress(&IndexProgress {
        phase: Phase::Scanning,
    });

    let store_adapter = Arc::new(StorageAdapter::with_progress(
        Arc::clone(&storage),
        reporter.clone(),
    ));
    let orchestrator = ExtractionOrchestratorImpl::new(store_adapter);

    let t0 = Instant::now();

    reporter.on_progress(&IndexProgress {
        phase: Phase::Parsing,
    });

    // index_all streams batches → reporter.on_batch fires live during this call.
    let counts = orchestrator.index_all(&root);

    // Guard against silent total data loss (QA-found regression): if the repo
    // contained source files to index but none were persisted, this is a real
    // failure — surface a clear ERROR and exit non-zero instead of printing a
    // misleading "indexed 0 files" success. A genuinely empty repo (no source
    // files discovered) is handled separately as a clean, informational 0.
    let outcome = match check_index_outcome(&counts) {
        Ok(o) => o,
        Err(e) => {
            reporter.abandon();
            return Err(e);
        }
    };
    if !outcome {
        reporter.abandon();
        println!("No source files to index under {}.", root.display());
        return Ok(());
    }

    // Run WAL checkpoint + PRAGMA optimize after bulk insert (OPT-7).
    storage.run_maintenance_pub();

    // Indexing (parse + store + maintenance) is done — leave a ✓ summary line.
    reporter.finish_index(
        counts.files as u64,
        counts.nodes as u64,
        counts.edges as u64,
        t0.elapsed().as_secs_f64(),
    );

    reporter.on_progress(&IndexProgress {
        phase: Phase::Resolving,
    });

    // Run framework extraction + reference resolution to promote unresolved_refs
    // into real import/call/route edges (AUDIT-2/4/7 blocker fix).
    // Pass None → full framework-extract run (init/index always re-extract).
    let resolved_count = run_resolution(&storage, &root, None)?;
    reporter.finish_resolve(resolved_count as u64);
    Ok(())
}

/// Decide whether an index run succeeded, distinguishing three outcomes:
///
/// 1. **No source files found** (`discovered == 0`): a genuinely empty repo, or
///    one with no files in a supported language. This is a clean, informational
///    success — the caller should print a friendly note and exit 0.
/// 2. **Found source files but indexed nothing** (`discovered > 0` yet
///    `files == 0` / `nodes == 0`), or **a store error occurred**
///    (`store_errors > 0`): this is the silent-data-loss failure mode a QA pass
///    found — return an error so the CLI exits non-zero with a clear message.
/// 3. **Indexed something**: normal success.
///
/// Returns `Ok(true)` when source files were discovered and indexed (caller
/// proceeds normally), `Ok(false)` for the clean empty-repo case (caller should
/// emit an informational message and skip the rest), and `Err` for the failure
/// modes.
pub(crate) fn check_index_outcome(counts: &codewiki_extraction::IndexCounts) -> Result<bool> {
    if counts.store_errors > 0 {
        anyhow::bail!(
            "indexing failed: {} batch(es) could not be persisted to the database \
             (see ERROR logs above); the index may be incomplete",
            counts.store_errors
        );
    }

    if counts.discovered == 0 {
        // Nothing to index — not an error.
        return Ok(false);
    }

    // Source files were discovered but nothing landed in the index at all.
    //
    // `counts.files` counts files that were written OR skipped (unchanged on a
    // re-index), so a clean re-index where every file is unchanged still has
    // `files > 0` and is correctly treated as success. Only a total zero —
    // which is exactly what the orphan-edge transaction rollback produced —
    // is the data-loss failure mode. We deliberately do NOT require
    // `nodes > 0`: a repo of empty/comment-only source files legitimately
    // indexes files with zero symbols.
    if counts.files == 0 {
        anyhow::bail!(
            "indexing failed: found {} source file(s) to index but indexed 0 — the index \
             is empty. This usually indicates an extraction or storage error (e.g. a \
             transaction rollback); check the logs above.",
            counts.discovered
        );
    }

    Ok(true)
}
