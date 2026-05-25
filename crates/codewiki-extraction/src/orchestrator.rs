//! T-221 — ExtractionOrchestratorImpl.
//!
//! Orchestrates file discovery, parallel parsing via rayon, and storage via
//! the `ExtractionStore` trait (A2).

use crate::ast_walker::extract_file;
use crate::config::{EXTRACTION_CHANNEL_DEPTH, MAX_FILE_SIZE_BYTES};
use crate::file_reader::read_source_file;
use crate::language_detector::is_source_file;
use crate::path_norm::normalize_path;
use codewiki_core::ExtractionBatch;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Counts returned by `index_all` — replaces the old `Vec<ExtractionBatch>`
/// accumulation so the full batch set is never held in RAM simultaneously.
#[derive(Debug, Default, Clone, Copy)]
pub struct IndexCounts {
    pub files: usize,
    pub nodes: usize,
    pub edges: usize,
}

/// Trait that the orchestrator uses to persist extraction results.
/// This is a thin wrapper over A2's `ExtractionStore` trait so A1
/// does not hard-depend on the full storage crate.
pub trait ExtractionStore: Send + Sync {
    /// Store a single file's extraction result (used by `process_changes`).
    fn store_batch(&self, batch: ExtractionBatch) -> Result<(), String>;

    /// Remove all data for a deleted file.
    fn delete_file(&self, path: &Path) -> Result<(), String>;

    /// Flush a sub-batch of parsed batches during `index_all`.
    ///
    /// Called repeatedly by the single writer thread with groups of up to
    /// `FLUSH_BULK_SIZE` batches. Implementations that support bulk inserts
    /// (e.g. the CLI's `StorageAdapter`) override this; the default falls back
    /// to calling `store_batch` for each item, preserving compatibility with
    /// `NoopStore` and test stores.
    fn flush_bulk(&self, batches: Vec<ExtractionBatch>) -> Result<(usize, usize, usize), String> {
        let mut nodes = 0usize;
        let mut edges = 0usize;
        let files = batches.len();
        for batch in batches {
            nodes += batch.nodes.len();
            edges += batch.edges.len();
            self.store_batch(batch)?;
        }
        Ok((files, nodes, edges))
    }
}

/// A set of changed files passed by the file-watcher (A5).
#[derive(Default)]
pub struct ChangedFiles {
    pub added: Vec<PathBuf>,
    pub modified: Vec<(PathBuf, Option<crate::types::ParsedTree>)>,
    pub removed: Vec<PathBuf>,
}

/// Orchestrator implementation.
pub struct ExtractionOrchestratorImpl {
    store: Arc<dyn ExtractionStore>,
    rayon_pool: rayon::ThreadPool,
}

impl ExtractionOrchestratorImpl {
    pub fn new(store: Arc<dyn ExtractionStore>) -> Self {
        // W6: use all available cores for parse (was num_cpus/2).
        // Resolution is now fully parallel in its own rayon scope (W5 OPT-11),
        // so there is no longer a need to reserve cores for a serial resolver.
        // On a 28-core machine this doubles the parse thread count from 14→28,
        // cutting the parse phase by up to 2× at 100k scale.
        let num_threads = num_cpus::get().max(2);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .stack_size(2 * 1024 * 1024)
            .build()
            .expect("failed to build rayon thread pool");

        Self {
            store,
            rayon_pool: pool,
        }
    }

    /// Index all files under `root` using git ls-files (fast path) or
    /// a recursive filesystem walk (fallback).
    ///
    /// Returns `IndexCounts` — the number of files, nodes, and edges indexed.
    /// Batches are streamed through a bounded channel to a single writer thread
    /// and are never all held in RAM at once (OPT-5).
    pub fn index_all(&self, root: &Path) -> IndexCounts {
        let files = discover_files(root);
        self.parse_files_and_stream(files)
    }

    /// Process a set of changed files.
    pub fn process_changes(&self, changes: ChangedFiles) -> Vec<ExtractionBatch> {
        // Delete removed files.
        for path in &changes.removed {
            if let Err(e) = self.store.delete_file(path) {
                tracing::warn!(path = %path.display(), err = %e, "failed to delete file from store");
            }
        }

        // Collect all files to re-extract.
        let mut paths: Vec<PathBuf> = changes.added;
        paths.extend(changes.modified.into_iter().map(|(p, _)| p));

        self.parse_files_parallel(paths)
    }

    /// Streaming bulk-index path for `index_all` (OPT-5 + OPT-7 + W6).
    ///
    /// Rayon parse workers send parsed `ExtractionBatch` items through a bounded
    /// channel (`EXTRACTION_CHANNEL_DEPTH`); a single writer thread drains the
    /// channel in sub-batches of `FLUSH_BULK_SIZE` and calls `store.flush_bulk`.
    /// Atomics accumulate the final counts without ever holding all batches in RAM.
    ///
    /// W6: read+parse are now overlapped — `read_source_file` is called inside
    /// each rayon worker rather than in a serial pre-pass.  This eliminates the
    /// serial I/O prefix and avoids allocating ~850 MB of source strings before
    /// parsing starts at 100k scale.  Only a `Vec<PathBuf>` (negligible memory)
    /// is held before the rayon scope opens.
    fn parse_files_and_stream(&self, files: Vec<PathBuf>) -> IndexCounts {
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        const FLUSH_BULK_SIZE: usize = 200;

        // Filter phase — only metadata checks (stat), no I/O for source text.
        // Reading source is deferred into each rayon worker so I/O and CPU parse
        // are overlapped across all available threads.
        let work: Vec<PathBuf> = files
            .into_iter()
            .filter_map(|path| {
                let path = normalize_path(&path);
                if !is_source_file(&path) {
                    tracing::debug!(path = %path.display(), "not a source file; skipping");
                    return None;
                }
                if let Ok(meta) = std::fs::metadata(&path) {
                    if meta.len() > MAX_FILE_SIZE_BYTES {
                        tracing::debug!(
                            path = %path.display(),
                            size = meta.len(),
                            max = MAX_FILE_SIZE_BYTES,
                            "file exceeds size limit; skipping"
                        );
                        return None;
                    }
                }
                Some(path)
            })
            .collect();

        // Set up bounded channel between rayon workers and the writer thread.
        let (tx, rx) = std::sync::mpsc::sync_channel::<ExtractionBatch>(EXTRACTION_CHANNEL_DEPTH);

        let total_files = Arc::new(AtomicUsize::new(0));
        let total_nodes = Arc::new(AtomicUsize::new(0));
        let total_edges = Arc::new(AtomicUsize::new(0));

        let store = Arc::clone(&self.store);
        let tf = Arc::clone(&total_files);
        let tn = Arc::clone(&total_nodes);
        let te = Arc::clone(&total_edges);

        // Spawn the writer thread before starting rayon so it's ready to drain.
        let writer = std::thread::spawn(move || {
            let mut pending: Vec<ExtractionBatch> = Vec::with_capacity(FLUSH_BULK_SIZE);
            for batch in rx {
                pending.push(batch);
                if pending.len() >= FLUSH_BULK_SIZE {
                    let chunk = std::mem::replace(&mut pending, Vec::with_capacity(FLUSH_BULK_SIZE));
                    match store.flush_bulk(chunk) {
                        Ok((f, n, e)) => {
                            tf.fetch_add(f, Ordering::Relaxed);
                            tn.fetch_add(n, Ordering::Relaxed);
                            te.fetch_add(e, Ordering::Relaxed);
                        }
                        Err(err) => {
                            tracing::warn!(err = %err, "flush_bulk error during index_all");
                        }
                    }
                }
            }
            // Drain remainder.
            if !pending.is_empty() {
                match store.flush_bulk(pending) {
                    Ok((f, n, e)) => {
                        tf.fetch_add(f, Ordering::Relaxed);
                        tn.fetch_add(n, Ordering::Relaxed);
                        te.fetch_add(e, Ordering::Relaxed);
                    }
                    Err(err) => {
                        tracing::warn!(err = %err, "flush_bulk error draining remainder");
                    }
                }
            }
        });

        // Parse phase — rayon workers read + parse each file and send the batch.
        // Read is inside the worker so disk I/O and CPU are overlapped across all
        // threads (W6 read-inside-worker).
        self.rayon_pool.install(|| {
            work.into_par_iter().for_each(|path| {
                let source = match read_source_file(&path) {
                    Some(s) => s,
                    None => {
                        tracing::debug!(path = %path.display(), "could not read source; skipping");
                        return;
                    }
                };
                tracing::debug!(path = %path.display(), "parsing file");
                let batch = extract_file(&path, &source);
                // If the channel is full this blocks the rayon worker, providing backpressure.
                if tx.send(batch).is_err() {
                    tracing::warn!(path = %path.display(), "writer thread closed early");
                }
            });
        });

        // Drop tx so the writer thread's `for batch in rx` loop terminates.
        drop(tx);
        // Wait for writer to finish flushing.
        if let Err(e) = writer.join() {
            tracing::warn!(err = ?e, "writer thread panicked");
        }

        IndexCounts {
            files: total_files.load(Ordering::Relaxed),
            nodes: total_nodes.load(Ordering::Relaxed),
            edges: total_edges.load(Ordering::Relaxed),
        }
    }

    /// Parallel parse for incremental sync (`process_changes`).
    ///
    /// Uses the per-file `store_batch` path (individual transactions) — correct
    /// for small incremental updates where the per-file overhead is negligible.
    fn parse_files_parallel(&self, files: Vec<PathBuf>) -> Vec<ExtractionBatch> {
        use rayon::prelude::*;

        // Read + filter phase (I/O, sequential-enough for small lists).
        let work: Vec<(PathBuf, String)> = files
            .into_iter()
            .filter_map(|path| {
                let path = normalize_path(&path);
                if !is_source_file(&path) {
                    tracing::debug!(path = %path.display(), "not a source file; skipping");
                    return None;
                }
                // File size cap (T-223).
                if let Ok(meta) = std::fs::metadata(&path) {
                    if meta.len() > MAX_FILE_SIZE_BYTES {
                        tracing::debug!(
                            path = %path.display(),
                            size = meta.len(),
                            max = MAX_FILE_SIZE_BYTES,
                            "file exceeds size limit; skipping"
                        );
                        return None;
                    }
                }
                let source = read_source_file(&path)?;
                Some((path, source))
            })
            .collect();

        // Parse phase — run in the dedicated rayon pool.
        let store = Arc::clone(&self.store);
        self.rayon_pool.install(|| {
            work.into_par_iter()
                .filter_map(|(path, source)| {
                    tracing::debug!(path = %path.display(), "parsing file");
                    let batch = extract_file(&path, &source);
                    if let Err(e) = store.store_batch(batch.clone()) {
                        tracing::warn!(path = %path.display(), err = %e, "failed to store batch");
                    }
                    Some(batch)
                })
                .collect()
        })
    }
}

/// Discover source files under `root`.
///
/// Uses the `ignore` crate's `WalkBuilder` so `.gitignore`, `.git/info/exclude`,
/// and global gitignore are respected — even when `root` is NOT inside a git
/// repository (`require_git(false)`). This unifies behaviour with the sync
/// watcher's walk (`codewiki-sync::directory`) so `init`/`index`/`sync` all
/// agree on which files exist. `node_modules`, `target`, hidden dirs, and the
/// `.codewiki/` index directory are always skipped.
fn discover_files(root: &Path) -> Vec<PathBuf> {
    use ignore::WalkBuilder;

    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(true) // .gitignore, .git/info/exclude, global gitignore, hidden
        .require_git(false) // apply .gitignore even in a non-git directory
        .parents(true)
        .filter_entry(|entry| {
            // Always skip the index directory and well-known build/dep dirs,
            // regardless of whether a .gitignore lists them.
            let name = entry.file_name().to_string_lossy();
            name != ".codewiki" && name != "node_modules" && name != "target"
        });

    let mut result = Vec::new();
    for dent in builder.build() {
        let dent = match dent {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(err = %e, "walk error; skipping entry");
                continue;
            }
        };
        if dent.file_type().is_some_and(|ft| ft.is_file()) {
            let path = dent.into_path();
            if is_source_file(&path) {
                result.push(path);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopStore;
    impl ExtractionStore for NoopStore {
        fn store_batch(&self, _batch: ExtractionBatch) -> Result<(), String> { Ok(()) }
        fn delete_file(&self, _path: &Path) -> Result<(), String> { Ok(()) }
    }

    #[test]
    fn orchestrator_creates_and_processes_empty_changes() {
        let store = Arc::new(NoopStore);
        let orch = ExtractionOrchestratorImpl::new(store);
        let batches = orch.process_changes(ChangedFiles::default());
        assert!(batches.is_empty());
    }

    #[test]
    fn index_all_on_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ts_path = dir.path().join("hello.ts");
        std::fs::write(&ts_path, b"function hello() {}").unwrap();

        // Use process_changes with an explicit added file to verify the pipeline.
        let store = Arc::new(NoopStore);
        let orch = ExtractionOrchestratorImpl::new(store);
        let changes = ChangedFiles {
            added: vec![ts_path.clone()],
            modified: Vec::new(),
            removed: Vec::new(),
        };
        let batches = orch.process_changes(changes);
        assert!(!batches.is_empty(), "expected at least one batch for {}", ts_path.display());
    }

    #[test]
    fn index_all_returns_counts() {
        let dir = tempfile::tempdir().unwrap();
        let ts_path = dir.path().join("hello.ts");
        std::fs::write(&ts_path, b"function hello() {}").unwrap();

        let store = Arc::new(NoopStore);
        let orch = ExtractionOrchestratorImpl::new(store);
        let counts = orch.index_all(dir.path());
        assert_eq!(counts.files, 1, "expected 1 file indexed");
    }
}
