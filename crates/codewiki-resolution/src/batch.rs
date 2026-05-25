// T-311 / T-310 — resolution_batch.rs
//
// ResolutionBatchRunner: drains the unresolved-refs queue in configurable
// batch sizes, retrying commit failures up to 3x with 50 ms backoff.
//
// W5 (OPT-11): adds `run_until_empty_parallel` for the full-index path.
// Workers run `resolve_one` concurrently via rayon (READ-ONLY against Arc indexes).
// A single dedicated std::thread is the sole caller of `commit_resolved_batch`
// (SQLite WAL allows only one writer). Workers feed resolved edges to the writer
// over a bounded `std::sync::mpsc::sync_channel`.

use crate::resolver::ReferenceResolver;
use codewiki_core::{CodeWikiError, UnresolvedRef};
use codewiki_storage::traits::{ResolutionStore, ResolvedEdge};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::sync::Arc;

/// Number of unresolved refs fetched per batch in incremental (watch) mode.
pub const BATCH_SIZE_INCREMENTAL: usize = 500;

/// Number of unresolved refs fetched per batch during initial full-index.
///
/// W5 (W5-ANALYSIS §3.1): raised from 2_000 to 10_000 so that django's ~115k refs
/// commit in ~12 transactions rather than ~101, cutting WAL fsync from ~2.8 s to ~0.3 s.
/// WAL fsync is 75% of commit cost at 2k batches; 10k batches amortise that ~8.5×.
/// `BATCH_SIZE_INCREMENTAL` stays at 500 — incremental sync workloads are small.
pub const BATCH_SIZE_FULL_INDEX: usize = 10_000;

/// Channel depth for the parallel writer pipeline (W5 OPT-11).
///
/// 4096 `ResolvedEdge`s ≈ ~800 KB in flight — adequate back-pressure without
/// stalling rayon workers under normal conditions.
const PARALLEL_CHANNEL_DEPTH: usize = 4096;

/// Commit chunk for the parallel writer thread (W5 OPT-11).
///
/// The writer accumulates resolved edges from workers and commits per this many
/// edges, matching `BATCH_SIZE_FULL_INDEX` so both levers (parallel resolve +
/// large batch) fire together.
const PARALLEL_COMMIT_CHUNK: usize = BATCH_SIZE_FULL_INDEX;

/// Maximum retries for a failed `commit_resolved_batch` call.
const COMMIT_RETRIES: u32 = 3;

/// Retry back-off for commit failures (milliseconds).
const COMMIT_BACKOFF_MS: u64 = 50;

pub struct ResolutionBatchRunner {
    store: Arc<dyn ResolutionStore>,
    resolver: ReferenceResolver,
    batch_size: usize,
}

impl ResolutionBatchRunner {
    pub fn new(
        store: Arc<dyn ResolutionStore>,
        resolver: ReferenceResolver,
        full_index_mode: bool,
    ) -> Self {
        Self {
            store,
            resolver,
            batch_size: if full_index_mode {
                BATCH_SIZE_FULL_INDEX
            } else {
                BATCH_SIZE_INCREMENTAL
            },
        }
    }

    /// Drain all unresolved references. Returns total edges committed.
    ///
    /// Strategy: paginated single pass. Unresolvable refs stay in the table
    /// (we don't have a "tried" flag), so we must advance the offset past
    /// them rather than always reading from offset 0 — otherwise we spin
    /// forever once the head of the table is fully unresolvable.
    pub fn run_until_empty(&self) -> Result<usize, CodeWikiError> {
        let mut total_committed = 0usize;
        let mut offset = 0usize;
        // Safety cap to prevent runaway loops if the pagination logic ever drifts.
        let mut iter_count = 0usize;
        const MAX_ITERS: usize = 100_000;

        loop {
            iter_count += 1;
            if iter_count > MAX_ITERS {
                tracing::warn!(
                    offset, total_committed,
                    "resolution batch loop hit safety cap, aborting"
                );
                break;
            }
            let unresolved = self.store.get_unresolved_batch(self.batch_size, offset)?;

            if unresolved.is_empty() {
                break;
            }

            // Resolve each ref; skip and log per-ref failures.
            let resolved: Vec<ResolvedEdge> = unresolved
                .iter()
                .filter_map(|uref| {
                    match self.resolver.resolve_one(uref) {
                        Ok(Some(edge)) => Some(edge),
                        Ok(None) => {
                            tracing::debug!(
                                ref_name = %uref.reference_name,
                                file = %uref.file_path,
                                "unresolvable reference — skipped"
                            );
                            None
                        }
                        Err(e) => {
                            tracing::warn!(
                                ref_name = %uref.reference_name,
                                file = %uref.file_path,
                                error = %e,
                                "resolution error — skipped"
                            );
                            None
                        }
                    }
                })
                .collect();

            let n = resolved.len();
            let batch_len = unresolved.len();

            // Commit with retry on transient DB errors.
            let mut attempts = 0u32;
            loop {
                match self.store.commit_resolved_batch(resolved.clone()) {
                    Ok(()) => break,
                    Err(e) if attempts < COMMIT_RETRIES => {
                        tracing::warn!(
                            attempt = attempts + 1,
                            error = %e,
                            "commit_resolved_batch failed, retrying"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(COMMIT_BACKOFF_MS));
                        attempts += 1;
                    }
                    Err(e) => return Err(e),
                }
            }

            total_committed += n;

            // Successful commits delete the n resolved rows, shifting the
            // remainder forward. Advance the cursor by (batch_len - n) so
            // we skip past the unresolvable refs and don't re-read them.
            offset += batch_len - n;

            // End of table.
            if batch_len < self.batch_size {
                break;
            }
        }

        tracing::info!(total_committed, "resolution batch loop complete");
        Ok(total_committed)
    }

    /// Drain all unresolved refs using parallel `resolve_one` + single-writer commit.
    ///
    /// # W5 OPT-11 design
    ///
    /// **Phase 1 — fetch upfront:** all unresolved refs are loaded from the DB in
    /// one paginated sweep before any workers start.  This decouples the read path
    /// from the write path so the writer's `BEGIN IMMEDIATE` lock never blocks the
    /// reader's `SELECT` on the same `Mutex<Connection>`.
    ///
    /// **Phase 2 — parallel resolve:** the ref list is sharded by
    /// `fnv1a(file_path) % thread_count`.  Each shard is processed by a rayon
    /// worker that holds a `Clone` of the resolver (only Arc pointer copies — zero
    /// data duplication).  Workers call `resolve_one` (READ-ONLY after
    /// `warm_caches`) and send `ResolvedEdge`s over a bounded `sync_channel`.
    ///
    /// **Phase 3 — single-writer commit:** a dedicated `std::thread` (NOT rayon —
    /// avoids blocking the rayon pool on I/O) drains the channel, accumulates edges
    /// into a buffer, and commits per `PARALLEL_COMMIT_CHUNK` (10 000).  This thread
    /// is the ONLY caller of `commit_resolved_batch`; the `Mutex<Connection>` in
    /// `StorageImpl` is never contended for writes.
    ///
    /// Falls back to `run_until_empty` if the name index is not warmed (rare; only
    /// affects test scenarios where `warm_caches` was not called).
    ///
    /// # Arguments
    ///
    /// * `thread_count` — number of rayon worker threads (pass `num_cpus::get()`,
    ///   capped at 16 to avoid diminishing returns from index contention).
    pub fn run_until_empty_parallel(&self, thread_count: usize) -> Result<usize, CodeWikiError> {
        // Safety: require warm indexes.  Without them resolve_one falls back to DB
        // queries that share Mutex<Connection> — not safe to call from rayon workers
        // without a second read-only connection.  Fallback to serial path.
        if self.resolver.name_index().is_none() {
            tracing::warn!(
                "run_until_empty_parallel: name index not warmed — falling back to serial"
            );
            return self.run_until_empty();
        }

        let thread_count = thread_count.clamp(1, 32);

        // ── Phase 1: fetch all unresolved refs upfront ──────────────────────────
        //
        // W6: cursor-based pagination (`WHERE id > last_id ORDER BY id LIMIT ?`)
        // replaces the previous OFFSET scan.  SQLite satisfies each page with an
        // O(log n) B-tree seek on the INTEGER PRIMARY KEY instead of re-scanning
        // from row 0, eliminating the O(n²) growth that would dominate at 100k.
        tracing::debug!("W6: fetching all unresolved refs via cursor pagination");
        let mut all_refs: Vec<UnresolvedRef> = Vec::new();
        let mut last_id: i64 = 0;
        // Generous page size to minimise round-trips (each page is now O(log n)).
        const FETCH_PAGE: usize = 10_000;
        loop {
            let page = self.store.get_unresolved_batch_after(last_id, FETCH_PAGE)?;
            if page.is_empty() {
                break;
            }
            // The last id in the page becomes the next cursor position.
            // `row_to_unresolved_ref` stores the integer id as a string; parse it back.
            last_id = page
                .last()
                .and_then(|r| r.id.parse::<i64>().ok())
                .unwrap_or(last_id);
            let n = page.len();
            all_refs.extend(page);
            if n < FETCH_PAGE {
                break;
            }
        }

        let total_refs = all_refs.len();
        if total_refs == 0 {
            tracing::info!(total_committed = 0, "W5 OPT-11: no unresolved refs — done");
            return Ok(0);
        }

        tracing::debug!(
            total_refs,
            thread_count,
            "W5 OPT-11: sharding refs across rayon workers"
        );

        // ── Phase 2: shard by file_path hash ────────────────────────────────────
        let n_shards = thread_count;
        let mut shards: Vec<Vec<UnresolvedRef>> = (0..n_shards).map(|_| Vec::new()).collect();
        for uref in all_refs {
            let s = file_path_shard(&uref.file_path, n_shards);
            shards[s].push(uref);
        }

        // ── Phase 3: bounded channel to single writer thread ─────────────────────
        let (tx, rx) = std::sync::mpsc::sync_channel::<ResolvedEdge>(PARALLEL_CHANNEL_DEPTH);

        // Clone the store Arc for the writer thread.
        let writer_store = Arc::clone(&self.store);

        let writer_thread = std::thread::spawn(move || -> Result<usize, CodeWikiError> {
            let mut buf: Vec<ResolvedEdge> = Vec::with_capacity(PARALLEL_COMMIT_CHUNK);
            let mut total_committed = 0usize;

            for edge in rx {
                buf.push(edge);
                if buf.len() >= PARALLEL_COMMIT_CHUNK {
                    total_committed += commit_chunk(&writer_store, std::mem::take(&mut buf))?;
                }
            }
            // Flush remainder.
            if !buf.is_empty() {
                total_committed += commit_chunk(&writer_store, buf)?;
            }

            tracing::info!(total_committed, "W5 OPT-11: writer thread done");
            Ok(total_committed)
        });

        // ── Phase 4: rayon scope — N workers resolve their shards ────────────────
        // Wrap the resolver in Arc so each rayon worker clones only the Arc pointer
        // (not the HashMap data) when it takes its shard.
        let resolver = Arc::new(self.resolver.clone());

        rayon::scope(|scope| {
            for shard in shards {
                let tx = tx.clone();
                let resolver = Arc::clone(&resolver);
                scope.spawn(move |_| {
                    for uref in &shard {
                        match resolver.resolve_one(uref) {
                            Ok(Some(edge)) => {
                                // If the writer panicked, send returns Err — ignore it
                                // (the writer error will surface when we join the thread).
                                let _ = tx.send(edge);
                            }
                            Ok(None) => {
                                tracing::debug!(
                                    ref_name = %uref.reference_name,
                                    file = %uref.file_path,
                                    "unresolvable reference — skipped"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    ref_name = %uref.reference_name,
                                    file = %uref.file_path,
                                    error = %e,
                                    "resolve_one error — skipped"
                                );
                            }
                        }
                    }
                    // tx drops here; when the last worker drops its tx clone,
                    // the receiver loop in the writer thread terminates.
                });
            }
            // Drop the original tx so the writer sees EOF once all workers finish.
            drop(tx);
        });

        // ── Phase 5: collect writer result ───────────────────────────────────────
        let total_committed = writer_thread
            .join()
            .map_err(|_| CodeWikiError::Other("W5 OPT-11: writer thread panicked".into()))??;

        tracing::info!(total_committed, total_refs, "W5 OPT-11: parallel resolution complete");
        Ok(total_committed)
    }

    /// Resolve a specific, pre-collected list of unresolved refs.
    ///
    /// OPT-9: used by `run_resolution_incremental` to process only the refs
    /// from changed files + their dependents + newly-nameable refs, instead of
    /// paginating the entire `unresolved_refs` table.
    ///
    /// The commit logic is identical to `run_until_empty`: each batch is
    /// committed atomically with retry on transient DB failures.  Unresolvable
    /// refs are silently skipped (they stay in the table for future syncs).
    pub fn run_for_refs(&self, refs: Vec<UnresolvedRef>) -> Result<usize, CodeWikiError> {
        if refs.is_empty() {
            return Ok(0);
        }

        let mut total_committed = 0usize;

        // Process in batch_size chunks to avoid one giant transaction.
        for chunk in refs.chunks(self.batch_size) {
            let resolved: Vec<ResolvedEdge> = chunk
                .iter()
                .filter_map(|uref| {
                    match self.resolver.resolve_one(uref) {
                        Ok(Some(edge)) => Some(edge),
                        Ok(None) => {
                            tracing::debug!(
                                ref_name = %uref.reference_name,
                                file = %uref.file_path,
                                "unresolvable reference — skipped"
                            );
                            None
                        }
                        Err(e) => {
                            tracing::warn!(
                                ref_name = %uref.reference_name,
                                file = %uref.file_path,
                                error = %e,
                                "resolution error — skipped"
                            );
                            None
                        }
                    }
                })
                .collect();

            let n = resolved.len();

            let mut attempts = 0u32;
            loop {
                match self.store.commit_resolved_batch(resolved.clone()) {
                    Ok(()) => break,
                    Err(e) if attempts < COMMIT_RETRIES => {
                        tracing::warn!(
                            attempt = attempts + 1,
                            error = %e,
                            "commit_resolved_batch failed, retrying"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(COMMIT_BACKOFF_MS));
                        attempts += 1;
                    }
                    Err(e) => return Err(e),
                }
            }

            total_committed += n;
        }

        tracing::info!(total_committed, "run_for_refs complete");
        Ok(total_committed)
    }
}

// ---------------------------------------------------------------------------
// Private helpers (W5 OPT-11)
// ---------------------------------------------------------------------------

/// Shard index for a file path: `fnv1a(file_path) % n`.
///
/// File-hash sharding groups refs from the same file into the same worker shard,
/// maximising cache locality in the moka `file_cache` / `import_mapping_cache`.
#[inline]
fn file_path_shard(file_path: &str, n: usize) -> usize {
    let mut h = DefaultHasher::new();
    file_path.hash(&mut h);
    (h.finish() as usize) % n
}

/// Commit a batch of resolved edges with retry on transient DB errors.
///
/// Returns the number of edges committed (always `batch.len()` on success).
fn commit_chunk(
    store: &Arc<dyn ResolutionStore>,
    batch: Vec<ResolvedEdge>,
) -> Result<usize, CodeWikiError> {
    let n = batch.len();
    if n == 0 {
        return Ok(0);
    }
    let mut attempts = 0u32;
    loop {
        match store.commit_resolved_batch(batch.clone()) {
            Ok(()) => return Ok(n),
            Err(e) if attempts < COMMIT_RETRIES => {
                tracing::warn!(
                    attempt = attempts + 1,
                    error = %e,
                    "commit_chunk failed, retrying"
                );
                std::thread::sleep(std::time::Duration::from_millis(COMMIT_BACKOFF_MS));
                attempts += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::{ReferenceResolver, ResolverQueryHandle};
    use codewiki_core::{UnresolvedRef, Node};
    use codewiki_storage::{open_in_memory, apply_schema, StorageImpl, ExtractionStore};
    use codewiki_storage::traits::ResolutionStore;
    use std::sync::Arc;

    // -------------------------------------------------------------------------
    // Minimal query handle that always returns empty — lets the resolver fall
    // through to "unresolvable" for every ref, which is fine for scoping tests.
    // -------------------------------------------------------------------------
    struct NullQueryHandle;
    impl ResolverQueryHandle for NullQueryHandle {
        fn get_node_by_id(&self, _: &str) -> Option<Node> { None }
        fn get_nodes_by_name(&self, _: &str) -> Vec<Node> { vec![] }
        fn get_nodes_by_lower_name(&self, _: &str) -> Vec<Node> { vec![] }
        fn get_nodes_by_qualified_name(&self, _: &str) -> Vec<Node> { vec![] }
        fn get_nodes_in_file(&self, _: &str) -> Vec<Node> { vec![] }
        fn get_all_node_names(&self) -> Vec<String> { vec![] }
        fn get_all_file_paths(&self) -> Vec<String> { vec![] }
        fn get_all_nodes(&self) -> Vec<Node> { vec![] }
    }

    fn make_storage() -> Arc<StorageImpl> {
        let conn = open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        Arc::new(StorageImpl::new(conn, 1_000))
    }

    /// Build a batch with `n` nodes and optionally an unresolved ref from node0.
    fn make_batch_with_ref(
        path: &str,
        hash: &str,
        n: usize,
        ref_name: Option<&str>,
    ) -> codewiki_core::ExtractionBatch {
        use codewiki_core::{ExtractionBatch, FileRecord, NodeKind, Language};
        use std::path::PathBuf;
        let nodes: Vec<Node> = (0..n)
            .map(|i| Node {
                id: format!("{}-node{}", path, i),
                name: format!("node{}", i),
                qualified_name: format!("{}-node{}", path, i),
                kind: NodeKind::Function,
                language: Language::TypeScript,
                file_path: path.to_string(),
                ..Default::default()
            })
            .collect();
        let unresolved_refs = ref_name
            .map(|name| vec![UnresolvedRef {
                id: String::new(),
                from_node_id: format!("{}-node0", path),
                reference_name: name.to_string(),
                reference_kind: "calls".to_string(),
                file_path: path.to_string(),
                line: Some(1),
                col: Some(0),
                metadata: None,
            }])
            .unwrap_or_default();
        ExtractionBatch {
            file: FileRecord {
                path: PathBuf::from(path),
                content_hash: hash.to_string(),
                language: "typescript".to_string(),
                size: 0,
                modified_at: 0,
                indexed_at: 0,
                node_count: n as u32,
                errors: vec![],
            },
            nodes,
            edges: vec![],
            unresolved_refs,
        }
    }

    fn make_batch(path: &str, hash: &str, n: usize) -> codewiki_core::ExtractionBatch {
        make_batch_with_ref(path, hash, n, None)
    }

    fn make_runner(storage: Arc<StorageImpl>) -> ResolutionBatchRunner {
        let resolver = ReferenceResolver::new(
            std::path::PathBuf::from("/tmp"),
            Arc::new(NullQueryHandle),
            1_000,
        );
        let store_arc: Arc<dyn ResolutionStore> =
            Arc::clone(&storage) as Arc<dyn ResolutionStore>;
        ResolutionBatchRunner::new(store_arc, resolver, false)
    }

    /// OPT-9: `run_for_refs` processes only the supplied refs and leaves
    /// unrelated refs untouched.
    ///
    /// Scenario:
    ///   - `changed.ts` has 1 unresolved ref (can't resolve with NullQueryHandle).
    ///   - `other.ts`   has 1 unresolved ref.
    ///   - We call `run_for_refs` with only the `changed.ts` ref.
    ///   - After the call, `other.ts` ref must still be present.
    #[test]
    fn run_for_refs_scopes_to_provided_refs_only() {
        let storage = make_storage();
        // Embed unresolved refs directly in the extraction batch so FK constraints hold.
        storage
            .store_extraction_batch(make_batch_with_ref("changed.ts", "hc", 1, Some("changedSymbol")))
            .unwrap();
        storage
            .store_extraction_batch(make_batch_with_ref("other.ts", "ho", 1, Some("otherSymbol")))
            .unwrap();

        assert_eq!(storage.get_unresolved_count().unwrap(), 2);

        // Fetch only the changed.ts ref.
        let changed_refs = ResolutionStore::get_unresolved_by_files(
            storage.as_ref(),
            &["changed.ts".to_string()],
        ).unwrap();
        assert_eq!(changed_refs.len(), 1);

        let runner = make_runner(Arc::clone(&storage));
        let committed = runner.run_for_refs(changed_refs).unwrap();
        // NullQueryHandle → nothing resolved.
        assert_eq!(committed, 0);

        // other.ts ref must still be present.
        let other_refs = ResolutionStore::get_unresolved_by_files(
            storage.as_ref(),
            &["other.ts".to_string()],
        ).unwrap();
        assert_eq!(
            other_refs.len(), 1,
            "other.ts ref must remain after run_for_refs scoped to changed.ts"
        );
    }

    /// `run_for_refs` with an empty list returns 0 without error.
    #[test]
    fn run_for_refs_empty_is_noop() {
        let storage = make_storage();
        let runner = make_runner(storage);
        assert_eq!(runner.run_for_refs(vec![]).unwrap(), 0);
    }
}
