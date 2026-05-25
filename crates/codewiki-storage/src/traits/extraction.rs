/// ExtractionStore trait (T-106) — write path for A1 → A2.
use codewiki_core::{CodeWikiError, ExtractionBatch};
use std::path::Path;

/// Statistics from a bulk store operation.
#[derive(Debug, Default, Clone)]
pub struct BulkStoreStats {
    pub files_written: usize,
    pub files_skipped: usize,
    pub nodes_inserted: usize,
    pub edges_inserted: usize,
}

/// Idempotent write path: hash-check → delete-old → insert-new → upsert file.
pub trait ExtractionStore: Send + Sync {
    /// Persist one file's extraction result.
    ///
    /// Idempotency contract:
    ///   1. Read stored content_hash for `batch.file.path`.
    ///   2. If hash matches and node_count matches → no-op, return Ok(()).
    ///   3. Otherwise, in a single transaction:
    ///      a. DELETE FROM nodes WHERE file_path = ?
    ///      b. INSERT OR REPLACE INTO nodes
    ///      c. INSERT INTO unresolved_refs
    ///      d. UPSERT files
    fn store_extraction_batch(&self, batch: ExtractionBatch) -> Result<(), CodeWikiError>;

    /// Remove all nodes, edges, and unresolved_refs for a deleted file.
    fn delete_file(&self, path: &Path) -> Result<(), CodeWikiError>;

    /// Bulk store for initial `index_all`. Wraps N calls to store_extraction_batch
    /// in a single outer transaction for performance. Returns bulk stats.
    fn store_extraction_batch_bulk(
        &self,
        batches: Vec<ExtractionBatch>,
    ) -> Result<BulkStoreStats, CodeWikiError>;
}
