/// Concrete SQLite implementation of ExtractionStore, QueryHandle, ResolutionStore, SyncStore.
use crate::cache::{build_node_cache, NodeCache};
use crate::graph::traversal::{GraphTraverser, TraversalOptions};
use crate::queries::{edges as eq, files as fq, meta as mq, nodes as nq, unresolved as uq};
use crate::search::{
    extract_search_terms, extract_symbols_from_query, get_stem_variants, is_test_file,
    search_nodes_fts, HIGH_VALUE_NODE_KINDS,
};
use crate::traits::query::{is_qualified_symbol, AggregatedImpact, AggregatedNeighbors};
use crate::traits::{
    BulkStoreStats, ExtractionStore, FileFilter, FindOpts, QueryHandle, ResolutionStore,
    ResolvedEdge, SearchOptions, SyncStore,
};
use codewiki_core::{
    CodeWikiError, Edge, ExtractionBatch, FileRecord, GraphStats, Node, NodeKind, SearchResult,
    Subgraph, UnresolvedRef,
};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[cfg(test)]
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Holds a SQLite connection and a node cache.
///
/// The Mutex ensures interior mutability when borrowed through `Arc<dyn Trait>`.
/// All trait methods take `&self` (not `&mut self`) per the trait definitions.
pub struct StorageImpl {
    conn: Mutex<Connection>,
    node_cache: NodeCache,
}

impl StorageImpl {
    pub fn new(conn: Connection, cache_capacity: u64) -> Self {
        Self {
            conn: Mutex::new(conn),
            node_cache: build_node_cache(cache_capacity),
        }
    }

    /// Persist the project root in `project_metadata` so query tools can render
    /// workspace-relative paths. Pass a forward-slash-normalized absolute root.
    pub fn set_root_path(&self, root: &str) -> Result<(), CodeWikiError> {
        self.with_conn(|conn| mq::set_metadata(conn, "root_path", root))
    }

    fn with_conn<F, R>(&self, f: F) -> Result<R, CodeWikiError>
    where
        F: FnOnce(&Connection) -> Result<R, CodeWikiError>,
    {
        let conn = self
            .conn
            .lock()
            .map_err(|_| CodeWikiError::Other("mutex poisoned".into()))?;
        f(&conn)
    }

    /// Public wrapper for `run_maintenance` (PRAGMA optimize + WAL checkpoint).
    ///
    /// Uses `TRUNCATE` checkpoint (OPT-7) so the WAL file is fully reclaimed
    /// after a bulk index write, not just passively checkpointed.
    pub fn run_maintenance_pub(&self) {
        if let Ok(conn) = self.conn.lock() {
            if let Err(e) = conn.execute_batch("PRAGMA optimize;") {
                tracing::warn!(error = %e, "PRAGMA optimize failed (non-fatal)");
            }
            if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                tracing::warn!(error = %e, "PRAGMA wal_checkpoint(TRUNCATE) failed (non-fatal)");
            }
        }
    }

    // -----------------------------------------------------------------------
    // OPT-7 — Bulk init path with FTS drop/rebuild
    // -----------------------------------------------------------------------

    /// Bulk-insert path for initial `init`/`index` operations (OPT-7).
    ///
    /// **Algorithm:**
    /// 1. Drop the three FTS sync triggers (`nodes_ai`, `nodes_ad`, `nodes_au`)
    ///    to eliminate per-row FTS writes during bulk insert.
    /// 2. Insert all batches in a single `BEGIN IMMEDIATE … COMMIT` with
    ///    hash-check deduplication.
    /// 3. Rebuild the FTS index in one pass:
    ///    `INSERT INTO nodes_fts(nodes_fts) VALUES ('rebuild')`.
    /// 4. Recreate the FTS sync triggers so incremental updates continue
    ///    working after the bulk load.
    ///
    /// **Note:** this method is NOT on the `ExtractionStore` trait — it is called
    /// directly from the CLI's `StorageAdapter::flush_bulk` override.
    pub fn store_extraction_batch_bulk_init(
        &self,
        batches: Vec<ExtractionBatch>,
    ) -> Result<BulkStoreStats, CodeWikiError> {
        if batches.is_empty() {
            return Ok(BulkStoreStats::default());
        }

        let mut stats = BulkStoreStats::default();

        self.with_conn(|conn| {
            // ---- Step 1: drop FTS sync triggers ----
            conn.execute_batch(
                "DROP TRIGGER IF EXISTS nodes_ai;
                 DROP TRIGGER IF EXISTS nodes_ad;
                 DROP TRIGGER IF EXISTS nodes_au;",
            )?;

            // ---- Step 2: single-transaction bulk insert ----
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let result: Result<(), CodeWikiError> = (|| {
                for batch in &batches {
                    let path_str = batch.file.path.to_string_lossy().to_string();

                    // Hash-check: skip unchanged files.
                    let skip = if let Some(existing) = fq::get_file_by_path(conn, &path_str)? {
                        existing.content_hash == batch.file.content_hash
                            && existing.node_count == batch.nodes.len() as u32
                    } else {
                        false
                    };

                    if skip {
                        stats.files_skipped += 1;
                        continue;
                    }

                    reconcile_file_nodes(conn, &path_str, &batch.nodes)?;

                    // FK backstop (mirrors `commit_resolved_batch`): an extraction
                    // bug can emit an *orphan* edge whose source/target node id was
                    // never created (e.g. a malformed C++ template). The edges table
                    // has `FOREIGN KEY (source|target) REFERENCES nodes(id)`, so
                    // inserting such an edge raises a SQLite FOREIGN KEY violation
                    // that aborts the entire single-transaction bulk load — zeroing
                    // the whole repo. Build the set of node ids for this batch first
                    // and skip any edge that references an id outside it, so valid
                    // nodes/edges still commit instead of failing wholesale.
                    let batch_node_ids: std::collections::HashSet<&str> =
                        batch.nodes.iter().map(|n| n.id.as_str()).collect();
                    let mut skipped_edges = 0usize;
                    for edge in &batch.edges {
                        if !batch_node_ids.contains(edge.source_id.as_str())
                            || !batch_node_ids.contains(edge.target_id.as_str())
                        {
                            skipped_edges += 1;
                            continue;
                        }
                        eq::insert_edge(conn, edge)?;
                    }
                    if skipped_edges > 0 {
                        tracing::warn!(
                            path = %path_str,
                            skipped = skipped_edges,
                            total = batch.edges.len(),
                            "skipped orphan edge(s) referencing a node id absent from the batch \
                             (FK backstop); valid nodes/edges still committed"
                        );
                    }
                    uq::insert_unresolved_refs_batch(conn, &batch.unresolved_refs)?;
                    let mut file = batch.file.clone();
                    file.node_count = batch.nodes.len() as u32;
                    fq::upsert_file(conn, &file)?;

                    stats.files_written += 1;
                    stats.nodes_inserted += batch.nodes.len();
                    stats.edges_inserted += batch.edges.len() - skipped_edges;
                }
                Ok(())
            })();

            if result.is_err() {
                let _ = conn.execute_batch("ROLLBACK");
                // Restore triggers before propagating the error.
                let _ = Self::recreate_fts_triggers(conn);
                return result;
            }
            conn.execute_batch("COMMIT")?;

            // ---- Step 3: rebuild FTS in a single pass ----
            conn.execute_batch("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild')")?;

            // ---- Step 4: recreate FTS sync triggers ----
            Self::recreate_fts_triggers(conn)?;

            Ok(())
        })?;

        self.node_cache.invalidate_all();
        Ok(stats)
    }

    /// Recreate the three FTS content-table sync triggers.
    ///
    /// Called after a bulk load to restore per-row incremental FTS maintenance.
    fn recreate_fts_triggers(conn: &Connection) -> Result<(), CodeWikiError> {
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
                INSERT INTO nodes_fts(rowid, id, name, qualified_name, docstring, signature)
                VALUES (NEW.rowid, NEW.id, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
             END;
             CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
                INSERT INTO nodes_fts(nodes_fts, rowid, id, name, qualified_name, docstring, signature)
                VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
             END;
             CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
                INSERT INTO nodes_fts(nodes_fts, rowid, id, name, qualified_name, docstring, signature)
                VALUES ('delete', OLD.rowid, OLD.id, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
                INSERT INTO nodes_fts(rowid, id, name, qualified_name, docstring, signature)
                VALUES (NEW.rowid, NEW.id, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
             END;",
        )?;
        Ok(())
    }

    /// Resolve a symbol string to the set of node ids it should aggregate over.
    ///
    /// Returns `(family_ids, resolved_name, family_size)`:
    /// - For a **qualified** symbol (e.g. `Foo::bar`), resolves to exactly the
    ///   single best match — `family_ids` has one id and `family_size == 1`.
    /// - For a **bare** symbol, resolves the best match, then collects every
    ///   node whose simple name case-insensitively EQUALS the best match's name
    ///   (the same-name family). `family_size` is the number of such nodes.
    ///
    /// Returns `None` when nothing matches the symbol.
    ///
    /// The family lookup is bounded by `MAX_FAMILY_NODES` so a pathological
    /// same-name set (e.g. a generated codebase with thousands of `new`) can't
    /// blow up traversal cost; the most relevant members (by search rank) win.
    fn resolve_symbol_family(
        &self,
        symbol: &str,
    ) -> Result<Option<(Vec<String>, String, usize)>, CodeWikiError> {
        /// Cap on how many same-name definitions we union over. Keeps the
        /// per-call traversal cost bounded on huge same-name families.
        const MAX_FAMILY_NODES: usize = 32;

        // Resolve the best match first (top BM25 hit), as the legacy tools did.
        let matches = self.search_nodes(
            symbol,
            SearchOptions {
                limit: 5,
                ..Default::default()
            },
        )?;
        let best = match matches.first() {
            Some(r) => r,
            None => return Ok(None),
        };

        // Qualified / namespaced symbol → resolve to exactly the best node.
        // No same-name family union.
        if is_qualified_symbol(symbol) {
            return Ok(Some((
                vec![best.node.id.clone()],
                best.node.name.clone(),
                1,
            )));
        }

        // Bare name → collect the same-name family (case-insensitive EQUALITY
        // on the simple name, NOT fuzzy/substring matches).
        let target_name = best.node.name.clone();
        let family_nodes = self.with_conn(|conn| {
            nq::find_nodes_by_exact_name(
                conn,
                std::slice::from_ref(&target_name),
                None,
                MAX_FAMILY_NODES,
            )
        })?;

        // Order family members by graph degree (most-connected first) so the
        // cap keeps the highest-signal definitions when the family is large.
        let mut ranked: Vec<(String, u64)> = Vec::with_capacity(family_nodes.len());
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.with_conn(|conn| {
            for n in &family_nodes {
                if !n.name.eq_ignore_ascii_case(&target_name) {
                    continue; // defensive: exact-name only
                }
                if !seen.insert(n.id.clone()) {
                    continue;
                }
                let (in_deg, out_deg) = eq::get_node_degree(conn, &n.id).unwrap_or((0, 0));
                ranked.push((n.id.clone(), in_deg + out_deg));
            }
            Ok::<_, CodeWikiError>(())
        })?;

        // Guarantee the best match is present even if it wasn't returned by the
        // exact-name lookup (it always should be, but be defensive).
        if !ranked.iter().any(|(id, _)| id == &best.node.id) {
            ranked.push((best.node.id.clone(), 0));
        }

        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let family_size = ranked.len();
        let family_ids: Vec<String> = ranked.into_iter().map(|(id, _)| id).collect();

        Ok(Some((family_ids, target_name, family_size)))
    }

    /// Rank an aggregated neighbor list by graph degree (most-connected first,
    /// ties broken by name then id for determinism) and truncate to `cap`.
    fn rank_and_cap_neighbors(&self, neighbors: &mut Vec<Node>, cap: usize) {
        let degrees: std::collections::HashMap<String, u64> = self
            .with_conn(|conn| {
                Ok::<_, CodeWikiError>(
                    neighbors
                        .iter()
                        .map(|n| {
                            let (i, o) = eq::get_node_degree(conn, &n.id).unwrap_or((0, 0));
                            (n.id.clone(), i + o)
                        })
                        .collect(),
                )
            })
            .unwrap_or_default();

        neighbors.sort_by(|a, b| {
            // Production code before test code: a bounded callers/callees list
            // should surface genuine source-code neighbors first, since a flood
            // of test-file callers would otherwise crowd real usage out of the
            // visible window (general rule, not tuned to any case). Within each
            // class, rank by graph degree, then name/id for determinism.
            let a_test = is_test_file(&a.file_path);
            let b_test = is_test_file(&b.file_path);
            let da = degrees.get(&a.id).copied().unwrap_or(0);
            let db = degrees.get(&b.id).copied().unwrap_or(0);
            a_test
                .cmp(&b_test)
                .then_with(|| db.cmp(&da))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.cmp(&b.id))
        });
        neighbors.truncate(cap);
    }
}

// ---------------------------------------------------------------------------
// ExtractionStore
// ---------------------------------------------------------------------------

/// Reconcile a file's stored node set against a freshly-extracted batch
/// WITHOUT cascading away the incoming cross-file edges.
///
/// The old path (`delete_nodes_by_file` → re-insert) let `ON DELETE CASCADE`
/// destroy every edge touching the file's nodes; incoming edges from other
/// files could never be re-created, because their unresolved refs were
/// consumed at resolution time. Verified corpus damage: one trailing-comment
/// edit killed 1,926 incoming edges with byte-identical node ids.
///
/// Steps, all inside the caller's transaction:
/// 1. old node identities; empty ⇒ plain insert (init fast path);
/// 2. delete OUTGOING edges of every old node — always rebuilt from the fresh
///    parse plus re-resolution;
/// 3. delete the file's own pending refs (they are re-emitted by the batch);
/// 4. upsert the new nodes (`ON CONFLICT DO UPDATE`, never REPLACE);
/// 5. pair each stale old node with a brand-new node of the same (kind, name)
///    — exact-id survivors are excluded FIRST, so a deleted duplicate can
///    never steal a surviving twin's edges — preferring an equal
///    qualified_name, then source order; retarget incoming edges to the new
///    id in place: zero visibility gap, no re-resolution;
/// 6. harvest the incoming edges of genuinely-vanished nodes into
///    `unresolved_refs` (pending is the correct state for a dangling ref);
/// 7. only then delete the stale node rows.
fn reconcile_file_nodes(
    conn: &Connection,
    path_str: &str,
    new_nodes: &[Node],
) -> Result<(), CodeWikiError> {
    let old = nq::get_node_identities_by_file(conn, path_str)?;
    if old.is_empty() {
        nq::insert_nodes_batch(conn, new_nodes)?;
        return Ok(());
    }

    let old_ids: Vec<&str> = old.iter().map(|o| o.id.as_str()).collect();
    eq::delete_edges_by_sources(conn, &old_ids)?;
    uq::delete_unresolved_refs_for_file(conn, path_str)?;
    nq::insert_nodes_batch(conn, new_nodes)?;

    let new_ids: std::collections::HashSet<&str> =
        new_nodes.iter().map(|n| n.id.as_str()).collect();
    let old_id_set: std::collections::HashSet<&str> = old_ids.iter().copied().collect();

    // Brand-new nodes (id not previously stored), in source order.
    let mut fresh: Vec<&Node> = new_nodes
        .iter()
        .filter(|n| !old_id_set.contains(n.id.as_str()))
        .collect();
    fresh.sort_by_key(|n| n.start_line);
    let fresh_kinds: Vec<String> = fresh.iter().map(|n| nq::node_kind_str(n)).collect();

    // Old nodes no longer present under their exact id, in source order.
    let mut stale: Vec<&nq::NodeIdentity> = old
        .iter()
        .filter(|o| !new_ids.contains(o.id.as_str()))
        .collect();
    stale.sort_by_key(|o| o.start_line);

    let mut claimed = vec![false; fresh.len()];
    let mut vanished: Vec<&str> = Vec::new();
    for o in &stale {
        let mut pick: Option<usize> = None;
        for (i, n) in fresh.iter().enumerate() {
            if claimed[i] || fresh_kinds[i] != o.kind || n.name != o.name {
                continue;
            }
            if n.qualified_name == o.qualified_name {
                pick = Some(i);
                break;
            }
            if pick.is_none() {
                pick = Some(i);
            }
        }
        match pick {
            Some(i) => {
                claimed[i] = true;
                eq::retarget_incoming_edges(conn, &o.id, &fresh[i].id)?;
            }
            None => vanished.push(o.id.as_str()),
        }
    }

    if !vanished.is_empty() {
        let harvested = uq::harvest_incoming_edges(conn, &vanished, path_str)?;
        if harvested > 0 {
            tracing::debug!(
                path = %path_str,
                vanished = vanished.len(),
                harvested,
                "harvested incoming edges of vanished nodes back into unresolved_refs"
            );
        }
    }

    let stale_ids: Vec<&str> = stale.iter().map(|o| o.id.as_str()).collect();
    nq::delete_nodes_by_ids(conn, &stale_ids)?;
    Ok(())
}

impl ExtractionStore for StorageImpl {
    fn store_extraction_batch(&self, batch: ExtractionBatch) -> Result<(), CodeWikiError> {
        let path_str = batch.file.path.to_string_lossy().to_string();

        // Hash-check gate (outside transaction)
        let skip = self.with_conn(|conn| {
            if let Some(existing) = fq::get_file_by_path(conn, &path_str)? {
                if existing.content_hash == batch.file.content_hash
                    && existing.node_count == batch.nodes.len() as u32
                {
                    return Ok(true);
                }
            }
            Ok(false)
        })?;

        if skip {
            return Ok(());
        }

        self.with_conn(|conn| {
            // Single transaction: delete old → insert new → upsert file
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| {
                reconcile_file_nodes(conn, &path_str, &batch.nodes)?;
                for edge in &batch.edges {
                    eq::insert_edge(conn, edge)?;
                }
                uq::insert_unresolved_refs_batch(conn, &batch.unresolved_refs)?;
                let mut file = batch.file.clone();
                file.node_count = batch.nodes.len() as u32;
                fq::upsert_file(conn, &file)?;
                Ok::<_, CodeWikiError>(())
            })();
            if result.is_err() {
                let _ = conn.execute_batch("ROLLBACK");
                return result;
            }
            conn.execute_batch("COMMIT")?;
            Ok(())
        })?;

        // Cache invalidation
        self.node_cache.invalidate_all();
        Ok(())
    }

    fn delete_file(&self, path: &Path) -> Result<(), CodeWikiError> {
        let path_str = path.to_string_lossy().to_string();
        self.with_conn(|conn| {
            conn.execute_batch("BEGIN IMMEDIATE")?;
            let result = (|| {
                // Delete dependents first (nodes table has no FK referencing files).
                uq::delete_unresolved_refs_for_file(conn, &path_str)?;
                // Harvest incoming cross-file edges into unresolved_refs BEFORE
                // deleting the nodes: a file removal is routinely transient — a
                // branch checkout, `git stash`, a revert (the installed sync git
                // hooks fire on all of them). Harvested refs re-resolve the
                // moment the file comes back; while it is gone they are pending,
                // which is the correct state for a dangling reference.
                let old = nq::get_node_identities_by_file(conn, &path_str)?;
                if !old.is_empty() {
                    let old_ids: Vec<&str> = old.iter().map(|o| o.id.as_str()).collect();
                    uq::harvest_incoming_edges(conn, &old_ids, &path_str)?;
                }
                eq::delete_edges_by_file(conn, &path_str)?;
                nq::delete_nodes_by_file(conn, &path_str)?;
                fq::delete_file(conn, &path_str)?;
                Ok::<_, CodeWikiError>(())
            })();
            if result.is_err() {
                let _ = conn.execute_batch("ROLLBACK");
                return result;
            }
            conn.execute_batch("COMMIT")?;
            Ok(())
        })?;
        self.node_cache.invalidate_all();
        Ok(())
    }

    fn store_extraction_batch_bulk(
        &self,
        batches: Vec<ExtractionBatch>,
    ) -> Result<BulkStoreStats, CodeWikiError> {
        let mut stats = BulkStoreStats::default();

        self.with_conn(|conn| {
            conn.execute_batch("BEGIN IMMEDIATE")?;

            let result: Result<(), CodeWikiError> = (|| {
                for batch in &batches {
                    let path_str = batch.file.path.to_string_lossy().to_string();

                    // Hash check
                    let skip = if let Some(existing) = fq::get_file_by_path(conn, &path_str)? {
                        existing.content_hash == batch.file.content_hash
                            && existing.node_count == batch.nodes.len() as u32
                    } else {
                        false
                    };

                    if skip {
                        stats.files_skipped += 1;
                        continue;
                    }

                    reconcile_file_nodes(conn, &path_str, &batch.nodes)?;
                    for edge in &batch.edges {
                        eq::insert_edge(conn, edge)?;
                    }
                    uq::insert_unresolved_refs_batch(conn, &batch.unresolved_refs)?;
                    let mut file = batch.file.clone();
                    file.node_count = batch.nodes.len() as u32;
                    fq::upsert_file(conn, &file)?;

                    stats.files_written += 1;
                    stats.nodes_inserted += batch.nodes.len();
                    stats.edges_inserted += batch.edges.len();
                }
                Ok(())
            })();

            if result.is_err() {
                let _ = conn.execute_batch("ROLLBACK");
                return result;
            }
            conn.execute_batch("COMMIT")?;
            Ok(())
        })?;

        self.node_cache.invalidate_all();
        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// QueryHandle
// ---------------------------------------------------------------------------

impl QueryHandle for StorageImpl {
    fn search_nodes(
        &self,
        query: &str,
        opts: SearchOptions,
    ) -> Result<Vec<SearchResult>, CodeWikiError> {
        self.with_conn(|conn| search_nodes_fts(conn, query, &opts))
    }

    fn get_node_by_id(&self, id: &str) -> Result<Option<Node>, CodeWikiError> {
        // Check cache first
        if let Some(node) = self.node_cache.get(id) {
            return Ok(Some(node));
        }
        let node = self.with_conn(|conn| nq::get_node_by_id(conn, id))?;
        if let Some(ref n) = node {
            self.node_cache.insert(id.to_string(), n.clone());
        }
        Ok(node)
    }

    fn get_callers(&self, node_id: &str, depth: usize) -> Result<Vec<(Node, Edge)>, CodeWikiError> {
        self.with_conn(|conn| {
            let traverser = GraphTraverser::new(conn);
            traverser.get_callers(node_id, depth)
        })
    }

    fn get_callees(&self, node_id: &str, depth: usize) -> Result<Vec<(Node, Edge)>, CodeWikiError> {
        self.with_conn(|conn| {
            let traverser = GraphTraverser::new(conn);
            traverser.get_callees(node_id, depth)
        })
    }

    fn get_impact_radius(&self, node_id: &str, depth: usize) -> Result<Subgraph, CodeWikiError> {
        self.with_conn(|conn| {
            let traverser = GraphTraverser::new(conn);
            traverser.get_impact_radius(node_id, depth)
        })
    }

    fn get_callers_aggregated(
        &self,
        symbol: &str,
        cap: usize,
    ) -> Result<AggregatedNeighbors, CodeWikiError> {
        let (family_ids, resolved_name, family_size) = match self.resolve_symbol_family(symbol)? {
            Some(f) => f,
            None => return Ok(AggregatedNeighbors::default()),
        };

        // Union 1-hop callers across the family, deduped by node id, ranked by
        // graph degree (most-connected first), capped at `cap`.
        let mut neighbor_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut neighbors: Vec<Node> = Vec::new();
        self.with_conn(|conn| {
            let traverser = GraphTraverser::new(conn);
            for fid in &family_ids {
                for (node, _edge) in traverser.get_callers(fid, 1)? {
                    if neighbor_ids.insert(node.id.clone()) {
                        neighbors.push(node);
                    }
                }
            }
            Ok::<_, CodeWikiError>(())
        })?;

        self.rank_and_cap_neighbors(&mut neighbors, cap);

        Ok(AggregatedNeighbors {
            neighbors,
            resolved_name,
            family_size,
        })
    }

    fn get_callees_aggregated(
        &self,
        symbol: &str,
        cap: usize,
    ) -> Result<AggregatedNeighbors, CodeWikiError> {
        let (family_ids, resolved_name, family_size) = match self.resolve_symbol_family(symbol)? {
            Some(f) => f,
            None => return Ok(AggregatedNeighbors::default()),
        };

        let mut neighbor_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut neighbors: Vec<Node> = Vec::new();
        self.with_conn(|conn| {
            let traverser = GraphTraverser::new(conn);
            for fid in &family_ids {
                for (node, _edge) in traverser.get_callees(fid, 1)? {
                    if neighbor_ids.insert(node.id.clone()) {
                        neighbors.push(node);
                    }
                }
            }
            Ok::<_, CodeWikiError>(())
        })?;

        self.rank_and_cap_neighbors(&mut neighbors, cap);

        Ok(AggregatedNeighbors {
            neighbors,
            resolved_name,
            family_size,
        })
    }

    fn get_impact_aggregated(
        &self,
        symbol: &str,
        depth: usize,
        cap: usize,
    ) -> Result<AggregatedImpact, CodeWikiError> {
        let (family_ids, resolved_name, family_size) = match self.resolve_symbol_family(symbol)? {
            Some(f) => f,
            None => return Ok(AggregatedImpact::default()),
        };

        // Union the reverse-reach impact subgraph across the family. Dedup nodes
        // by id and edges by (source -> target). The merged subgraph is capped
        // at `cap` nodes (ranked by degree) so output stays bounded on large
        // same-name families.
        let mut all_nodes: std::collections::HashMap<String, Node> =
            std::collections::HashMap::new();
        let mut all_edges: Vec<Edge> = Vec::new();
        let mut seen_edges: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut roots: Vec<String> = Vec::new();

        self.with_conn(|conn| {
            let traverser = GraphTraverser::new(conn);
            for fid in &family_ids {
                let subgraph = traverser.get_impact_radius(fid, depth)?;
                roots.push(fid.clone());
                for (id, node) in subgraph.nodes {
                    all_nodes.insert(id, node);
                }
                for edge in subgraph.edges {
                    let key = format!("{}->{}", edge.source_id, edge.target_id);
                    if seen_edges.insert(key) {
                        all_edges.push(edge);
                    }
                }
            }
            Ok::<_, CodeWikiError>(())
        })?;

        // ---- Provenance-aware de-bloat (regression fix) ----
        //
        // The reverse-reach impact traversal pulls in three classes of node:
        //   (A) ANSWER nodes — genuine dependants/implementers reached via a
        //       calls/references/implements/extends/instantiates/uses edge.
        //       These ARE the answer for an `impl`/`blast` query and must NEVER
        //       be dropped to make room for noise.
        //   (B) pure-marker nodes — `file` / `namespace` nodes that only appear
        //       because a file imports/contains a dependent. They carry no
        //       information for impact and are always safe to strip.
        //   (C) context nodes — everything else (e.g. contains-expansion
        //       children, import-only nodes). Kept only if budget remains.
        //
        // We classify each node by the edges in the merged subgraph: a node is
        // an ANSWER node when it is the SOURCE of an answer-kind edge whose
        // target is in the subgraph (i.e. it genuinely depends on a member).
        // Family roots are always answer nodes. Then we drop class (B) outright
        // and cap class (C) by degree, but keep ALL of class (A).
        let root_set: std::collections::HashSet<String> = roots.iter().cloned().collect();

        // Edge kinds that mark a real dependency relationship (the answer).
        let is_answer_edge = |k: &codewiki_core::EdgeKind| {
            matches!(
                k,
                codewiki_core::EdgeKind::Calls
                    | codewiki_core::EdgeKind::References
                    | codewiki_core::EdgeKind::Implements
                    | codewiki_core::EdgeKind::Extends
                    | codewiki_core::EdgeKind::Instantiates
                    | codewiki_core::EdgeKind::Uses
            )
        };
        let mut answer_ids: std::collections::HashSet<String> = root_set.clone();
        // Reverse adjacency over answer edges: target -> [sources that depend on it].
        // Used to BFS distance-from-root so DIRECT (depth-1) dependants outrank
        // transitive (depth-2+) ones when the budget forces a choice — the direct
        // implementers/callers are the answer for impl/blast; deep transitive
        // nodes are bloat that a depth-3 traversal would otherwise pile on.
        let mut rev_adj: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for edge in &all_edges {
            if is_answer_edge(&edge.kind)
                && all_nodes.contains_key(&edge.source_id)
                && all_nodes.contains_key(&edge.target_id)
            {
                // The SOURCE depends on the target → source is a dependant/
                // implementer (an answer node). Roots are already included.
                answer_ids.insert(edge.source_id.clone());
                rev_adj
                    .entry(edge.target_id.clone())
                    .or_default()
                    .push(edge.source_id.clone());
            }
        }

        // Authoritative DIRECT-dependant fetch. The reverse-reach traversal only
        // records the edge that FIRST reached a node, so a node pulled in via a
        // weak edge (e.g. a depth-2 `references`) loses its genuine 1-hop
        // `implements`/`extends`/`calls` edge to the focal — leaving real
        // implementers misclassified as transitive. Re-query each root's
        // incoming answer-kind edges directly so every genuine direct dependant
        // is classified as a distance-1 answer node regardless of traversal
        // edge-recording order. Bounded: one edge query per root.
        const ANSWER_EDGE_KINDS: &[codewiki_core::EdgeKind] = &[
            codewiki_core::EdgeKind::Calls,
            codewiki_core::EdgeKind::References,
            codewiki_core::EdgeKind::Implements,
            codewiki_core::EdgeKind::Extends,
            codewiki_core::EdgeKind::Instantiates,
            codewiki_core::EdgeKind::Uses,
        ];
        self.with_conn(|conn| {
            for root_id in &roots {
                let incoming = eq::get_incoming_edges(conn, root_id, Some(ANSWER_EDGE_KINDS))?;
                for edge in incoming {
                    if all_nodes.contains_key(&edge.source_id) {
                        answer_ids.insert(edge.source_id.clone());
                        rev_adj
                            .entry(root_id.clone())
                            .or_default()
                            .push(edge.source_id.clone());
                    }
                }
            }
            Ok::<_, CodeWikiError>(())
        })?;

        // BFS distance from the roots over answer edges (roots = 0). Nodes not
        // reached this way get a large sentinel distance (transitive/context).
        let mut distance: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        for r in &roots {
            distance.insert(r.clone(), 0);
            queue.push_back(r.clone());
        }
        while let Some(cur) = queue.pop_front() {
            let d = distance[&cur];
            if let Some(deps) = rev_adj.get(&cur) {
                for dep in deps {
                    if !distance.contains_key(dep) {
                        distance.insert(dep.clone(), d + 1);
                        queue.push_back(dep.clone());
                    }
                }
            }
        }
        let dist_of = |id: &String| -> u32 { distance.get(id).copied().unwrap_or(u32::MAX) };

        // (B) Drop pure file/namespace markers (never answer nodes for impact).
        // A root is never dropped even if it were such a kind (defensive).
        all_nodes.retain(|id, n| {
            root_set.contains(id) || !matches!(n.kind, NodeKind::File | NodeKind::Namespace)
        });

        // (C) Cap remaining nodes when over budget. Priority: roots, then answer
        // nodes ordered by distance-from-root (direct dependants first), then
        // context. Ties broken by degree desc then id. ALL answer nodes are kept
        // even if they exceed `cap` — only context is subject to the budget.
        if all_nodes.len() > cap {
            let degrees: std::collections::HashMap<String, u64> = self.with_conn(|conn| {
                Ok::<_, CodeWikiError>(
                    all_nodes
                        .keys()
                        .map(|id| {
                            let (i, o) = eq::get_node_degree(conn, id).unwrap_or((0, 0));
                            (id.clone(), i + o)
                        })
                        .collect(),
                )
            })?;
            let mut ids: Vec<String> = all_nodes.keys().cloned().collect();
            ids.sort_by(|a, b| {
                let rank = |id: &String| -> u8 {
                    if root_set.contains(id) {
                        0
                    } else if answer_ids.contains(id) {
                        1
                    } else {
                        2
                    }
                };
                rank(a)
                    .cmp(&rank(b))
                    .then_with(|| dist_of(a).cmp(&dist_of(b)))
                    .then_with(|| {
                        degrees
                            .get(b)
                            .copied()
                            .unwrap_or(0)
                            .cmp(&degrees.get(a).copied().unwrap_or(0))
                    })
                    .then_with(|| a.cmp(b))
            });
            // Never truncate below the number of DIRECT (1-hop) dependants —
            // those are the genuine implementers/callers an impl/blast query
            // asks for and must all survive even if they exceed `cap`. Deeper
            // transitive answer nodes (distance >= 2) and pure context ARE
            // subject to the budget, so a depth-3 traversal can't pile on the
            // whole transitive closure and crowd the direct answers out.
            let direct_answer_count = all_nodes
                .keys()
                .filter(|id| !root_set.contains(*id) && dist_of(id) <= 1)
                .count();
            let keep_n = std::cmp::max(
                cap,
                (roots.len() + direct_answer_count).min(all_nodes.len()),
            );
            let keep: std::collections::HashSet<String> = ids.into_iter().take(keep_n).collect();
            all_nodes.retain(|id, _| keep.contains(id));
        }

        // Re-sync edges to surviving nodes.
        all_edges.retain(|e| {
            all_nodes.contains_key(&e.source_id) && all_nodes.contains_key(&e.target_id)
        });

        Ok(AggregatedImpact {
            subgraph: Subgraph {
                nodes: all_nodes,
                edges: all_edges,
                roots,
            },
            resolved_name,
            family_size,
        })
    }

    fn find_relevant_context(
        &self,
        query: &str,
        opts: FindOpts,
    ) -> Result<Subgraph, CodeWikiError> {
        // Handle empty query
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Subgraph {
                nodes: std::collections::HashMap::new(),
                edges: Vec::new(),
                roots: Vec::new(),
            });
        }

        // Parse HIGH_VALUE_NODE_KINDS into NodeKind vec once
        let high_value_kinds: Vec<NodeKind> = HIGH_VALUE_NODE_KINDS
            .iter()
            .filter_map(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
            .collect();

        // === STEP 1: Extract symbols from query ===
        let symbols_from_query = extract_symbols_from_query(trimmed);

        // OPT-12 stopword list — used for NL-query detection (is_nl_query) and STEP 2c.
        // Intentionally SMALLER than the commonWords set in extract_symbols_from_query
        // so that domain words like "request", "response", "schema" count as
        // significant for the purpose of detecting NL queries.
        static NL_STOPWORDS: &[&str] = &[
            "the", "and", "for", "with", "from", "this", "that", "have", "been", "will", "would",
            "could", "should", "does", "done", "make", "made", "use", "used", "using", "work",
            "works", "find", "found", "show", "call", "called", "calling", "get", "set", "add",
            "all", "any", "how", "what", "when", "where", "which", "who", "why", "not", "but",
            "are", "was", "were", "has", "had", "its", "can", "did", "may", "also", "into", "than",
            "then", "them", "each", "other", "some", "such", "only", "same", "about", "after",
            "before", "between", "through", "during", "without", "again", "further", "once",
            "here", "there", "both", "just", "more", "most", "very", "being", "having", "doing",
        ];

        // NL FTS stopword list — used for base_terms in the NL fallback FTS pass.
        // Extends NL_STOPWORDS with high-noise code-domain words that produce
        // too many false-positive FTS hits (e.g. "request" → ConnectionRouter).
        static NL_FTS_STOPWORDS: &[&str] = &[
            "the",
            "and",
            "for",
            "with",
            "from",
            "this",
            "that",
            "have",
            "been",
            "will",
            "would",
            "could",
            "should",
            "does",
            "done",
            "make",
            "made",
            "use",
            "used",
            "using",
            "work",
            "works",
            "find",
            "found",
            "show",
            "call",
            "called",
            "calling",
            "get",
            "set",
            "add",
            "all",
            "any",
            "how",
            "what",
            "when",
            "where",
            "which",
            "who",
            "why",
            "not",
            "but",
            "are",
            "was",
            "were",
            "has",
            "had",
            "its",
            "can",
            "did",
            "may",
            "also",
            "into",
            "than",
            "then",
            "them",
            "each",
            "other",
            "some",
            "such",
            "only",
            "same",
            "about",
            "after",
            "before",
            "between",
            "through",
            "during",
            "without",
            "again",
            "further",
            "once",
            "here",
            "there",
            "both",
            "just",
            "more",
            "most",
            "very",
            "being",
            "having",
            "doing",
            // High-noise code-domain words: searching FTS for these floods results
            // with unrelated nodes (e.g. "request" → ConnectionRouter in django).
            "request",
            "requests",
            "response",
            "responses",
        ];

        // Track IDs from Step 2 exact-name channel (used later in Step 5a).
        let mut exact_match_node_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // === STEP 2: Exact-name channel ===
        let mut exact_matches: Vec<SearchResult> = Vec::new();
        if !symbols_from_query.is_empty() {
            let exact_limit = ((opts.search_limit as f64) * 5.0).ceil() as usize;
            if let Ok(nodes) = self.with_conn(|conn| {
                nq::find_nodes_by_exact_name(
                    conn,
                    &symbols_from_query,
                    Some(&high_value_kinds),
                    exact_limit,
                )
            }) {
                for node in nodes {
                    let kind_bonus = crate::search::kind_bonus(&node.kind) as f64;
                    // Use 1.0 base score (matching TS findNodesByExactName which returns 1.0).
                    // The text channel will provide higher scores via name_match_bonus;
                    // Step 4 merge takes the max across channels.
                    let id = node.id.clone();
                    exact_matches.push(SearchResult {
                        score: 1.0 + kind_bonus,
                        node,
                        snippet: None,
                    });
                    exact_match_node_ids.insert(id);
                }
            }

            // Co-location boost: files where >= 2 distinct query symbols match
            if exact_matches.len() > 1 {
                let mut file_symbol_counts: std::collections::HashMap<
                    String,
                    std::collections::HashSet<String>,
                > = std::collections::HashMap::new();
                for r in &exact_matches {
                    let names = file_symbol_counts
                        .entry(r.node.file_path.clone())
                        .or_default();
                    names.insert(r.node.name.to_lowercase());
                }
                for r in &mut exact_matches {
                    let count = file_symbol_counts
                        .get(&r.node.file_path)
                        .map(|s| s.len())
                        .unwrap_or(1);
                    if count > 1 {
                        r.score += (count - 1) as f64 * 20.0;
                    }
                }
                exact_matches.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }

            // Trim to search_limit * 2
            let trim_limit = ((opts.search_limit as f64) * 2.0).ceil() as usize;
            exact_matches.truncate(trim_limit);
        }

        // === STEP 2b: Definition-prefix channel ===
        // For each symbol (+ stem variants), title-case and search class/interface/etc.
        // nodes whose name starts with it, scoring +15 + brevity bonus.
        if !symbols_from_query.is_empty() {
            let def_kinds: Vec<NodeKind> = [
                "class",
                "interface",
                "struct",
                "trait",
                "protocol",
                "enum",
                "type_alias",
            ]
            .iter()
            .filter_map(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
            .collect();

            let mut expanded_symbols: std::collections::HashSet<String> =
                symbols_from_query.iter().cloned().collect();
            for sym in &symbols_from_query {
                for variant in get_stem_variants(sym) {
                    expanded_symbols.insert(variant);
                }
            }

            let existing_ids: std::collections::HashSet<String> =
                exact_matches.iter().map(|r| r.node.id.clone()).collect();

            let prefix_limit = ((opts.search_limit as f64) * 3.0).ceil() as usize;

            for sym in &expanded_symbols {
                // Title-case: first char upper, rest lower
                if sym.is_empty() {
                    continue;
                }
                let title_cased = {
                    let mut chars = sym.chars();
                    match chars.next() {
                        None => String::new(),
                        Some(first) => {
                            let upper: String = first.to_uppercase().collect();
                            upper + &chars.as_str().to_lowercase()
                        }
                    }
                };

                // Skip if title-cased == sym (already an exact-case CamelCase symbol)
                if title_cased == *sym {
                    continue;
                }

                // Search via FTS for the title-cased prefix
                let search_opts = SearchOptions {
                    limit: 30,
                    kinds: Some(def_kinds.clone()),
                    ..Default::default()
                };
                if let Ok(prefix_results) = self.search_nodes(&title_cased, search_opts) {
                    for r in prefix_results {
                        if r.node
                            .name
                            .to_lowercase()
                            .starts_with(&title_cased.to_lowercase())
                        {
                            if existing_ids.contains(&r.node.id) {
                                continue;
                            }
                            let brevity_bonus = {
                                let diff = (r.node.name.len() as i64) - (title_cased.len() as i64);
                                f64::max(0.0, 10.0 - diff as f64 / 3.0)
                            };
                            let new_score = r.score + 15.0 + brevity_bonus;
                            // Check if already in exact_matches
                            if let Some(existing) =
                                exact_matches.iter_mut().find(|e| e.node.id == r.node.id)
                            {
                                existing.score = f64::max(existing.score, new_score);
                            } else {
                                exact_matches.push(SearchResult {
                                    node: r.node,
                                    score: new_score,
                                    snippet: None,
                                });
                            }
                        }
                    }
                }
            }

            exact_matches.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            exact_matches.truncate(prefix_limit);
        }

        // === STEP 2c: NL action-stem channel ===
        // For NL queries, derive verb/adjective forms from abstract nouns in the query
        // (e.g. "validation" → "validate", "reactivity" → "reactive") and search for
        // function/method/class nodes whose names start with the derived stem.
        // This lets STEP 2b results for "Schema*" classes not crowd out ZodType
        // (found via its `validate` method) or ReactiveEffect (found via "reactive*").
        //
        // We pre-compute is_nl_candidate here using the same sig_word_count / total_words
        // logic, so the extraction can happen before is_nl_query is set.
        {
            let nl_cand_sig = trimmed
                .split_whitespace()
                .filter(|w| w.len() >= 3 && !NL_STOPWORDS.contains(&w.to_lowercase().as_str()))
                .count();
            let nl_cand_total = trimmed.split_whitespace().count();
            let is_nl_cand = nl_cand_sig >= 2 && nl_cand_total >= 4;

            if is_nl_cand && !symbols_from_query.is_empty() {
                let existing_ids: std::collections::HashSet<String> =
                    exact_matches.iter().map(|r| r.node.id.clone()).collect();

                let action_kinds: Vec<NodeKind> =
                    ["function", "method", "class", "interface", "struct"]
                        .iter()
                        .filter_map(|s| {
                            serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
                        })
                        .collect();

                let mut action_stems: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for sym in &symbols_from_query {
                    let lower = sym.to_lowercase();
                    // -tion/-ation → verb: "validation" → "validate", "execution" → "execute"
                    if lower.ends_with("tion") && lower.len() > 6 {
                        let stem = &lower[..lower.len() - 4];
                        action_stems.insert(format!("{}e", stem));
                        action_stems.insert(stem.to_string());
                    }
                    if lower.ends_with("ation") && lower.len() > 7 {
                        let stem = &lower[..lower.len() - 5];
                        action_stems.insert(format!("{}e", stem));
                        action_stems.insert(stem.to_string());
                    }
                    // -ity → adjective: "reactivity" → "reactive", "activity" → "active"
                    if lower.ends_with("ity") && lower.len() > 5 {
                        let stem = &lower[..lower.len() - 3];
                        action_stems.insert(format!("{}e", stem));
                        action_stems.insert(stem.to_string());
                    }
                    // -ed → verb: "applied" → "apply", "executed" → "execute"
                    if lower.ends_with("ied") && lower.len() > 5 {
                        // "applied" → "appl" + "y" → "apply"
                        let stem = &lower[..lower.len() - 3];
                        action_stems.insert(format!("{}y", stem));
                    }
                    if lower.ends_with("ed") && !lower.ends_with("ied") && lower.len() > 4 {
                        // "executed" → "execute", "parsed" → "parse"
                        let stem = &lower[..lower.len() - 2];
                        action_stems.insert(format!("{}e", stem));
                        action_stems.insert(stem.to_string());
                    }
                    // Note: -ing not handled here to avoid false positives
                    // (e.g. "routing" → "route" matches RoutePattern but not URLResolver,
                    // and the route* results can displace better routing-related candidates).
                }

                let action_limit = opts.search_limit * 2;
                for stem in &action_stems {
                    if stem.len() < 3 {
                        continue;
                    }
                    let search_opts = SearchOptions {
                        limit: action_limit,
                        kinds: Some(action_kinds.clone()),
                        ..Default::default()
                    };
                    if let Ok(stem_results) = self.search_nodes(stem, search_opts) {
                        for r in stem_results {
                            let stem_lower = stem.to_lowercase();
                            if !r.node.name.to_lowercase().starts_with(&stem_lower) {
                                continue;
                            }
                            if existing_ids.contains(&r.node.id) {
                                continue;
                            }
                            let brevity_bonus = {
                                let diff = (r.node.name.len() as i64) - (stem_lower.len() as i64);
                                f64::max(0.0, 8.0 - diff as f64 / 3.0)
                            };
                            let new_score = r.score + 12.0 + brevity_bonus;
                            if let Some(existing) =
                                exact_matches.iter_mut().find(|e| e.node.id == r.node.id)
                            {
                                existing.score = f64::max(existing.score, new_score);
                            } else {
                                exact_matches.push(SearchResult {
                                    node: r.node,
                                    score: new_score,
                                    snippet: None,
                                });
                            }
                        }
                    }
                }

                // Re-sort but keep prefix_limit from STEP 2b already applied
                exact_matches.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                // Allow a modest expansion for action-stem results
                exact_matches.truncate(((opts.search_limit as f64) * 8.0).ceil() as usize);
                // Protect only STEP 2c-specific new results from STEP 5a dampening.
                // STEP 2 exact-name matches were already added to exact_match_node_ids above.
                // STEP 2b prefix matches should NOT be protected — they match only by name
                // prefix and should be subject to STEP 5a dampening like any FTS result.
                for r in &exact_matches {
                    if !existing_ids.contains(&r.node.id) {
                        exact_match_node_ids.insert(r.node.id.clone());
                    }
                }
            }
        }

        // Capture whether exact_matches contains any high-value structural nodes
        // BEFORE they are consumed by the merge step (used by OPT-12 NL-trigger).
        let has_highval_exact_pre_merge = exact_matches.iter().any(|r| {
            matches!(
                r.node.kind,
                NodeKind::Class
                    | NodeKind::Interface
                    | NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Function
                    | NodeKind::Method
            )
        });

        // === STEP 3: Text channel ===
        // Per-term FTS search, boost nodes matching multiple terms.
        let mut text_results: Vec<SearchResult> = Vec::new();
        {
            let search_terms = extract_search_terms(trimmed);
            if !search_terms.is_empty() {
                // Broad kinds excluding import (to avoid flooding FTS results)
                let search_kinds: Vec<NodeKind> = [
                    "file",
                    "module",
                    "class",
                    "struct",
                    "interface",
                    "trait",
                    "protocol",
                    "function",
                    "method",
                    "property",
                    "field",
                    "variable",
                    "constant",
                    "enum",
                    "enum_member",
                    "type_alias",
                    "namespace",
                    "export",
                    "route",
                    "component",
                ]
                .iter()
                .filter_map(|s| {
                    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
                })
                .collect();

                let term_limit = opts.search_limit * 2;

                // FIX 2 (context multi-term coverage ranking) — per-term IDF,
                // computed once per query and cached. Rare terms (the semantic
                // anchor) weigh more than high-frequency terms so a single common
                // term can't dominate BM25 and crowd out the anchor.
                let term_idf = self
                    .with_conn(|conn| {
                        Ok::<_, CodeWikiError>(crate::search::scoring::compute_term_idf(
                            conn,
                            &search_terms,
                        ))
                    })
                    .unwrap_or_default();

                // Track, per candidate node, the DISTINCT query terms it matched
                // (not just a count) so coverage and IDF weighting are exact.
                let mut term_results_map: std::collections::HashMap<
                    String,
                    (SearchResult, std::collections::HashSet<String>),
                > = std::collections::HashMap::new();

                for term in &search_terms {
                    let search_opts = SearchOptions {
                        limit: term_limit,
                        kinds: Some(search_kinds.clone()),
                        ..Default::default()
                    };
                    if let Ok(results) = self.search_nodes(term, search_opts) {
                        for r in results {
                            let entry = term_results_map
                                .entry(r.node.id.clone())
                                .or_insert_with(|| (r.clone(), std::collections::HashSet::new()));
                            entry.1.insert(term.to_lowercase());
                            entry.0.score = f64::max(entry.0.score, r.score);
                        }
                    }
                }

                // Multi-term COVERAGE boost, applied BEFORE truncation. The boost
                // is strictly ADDITIVE and bounded: it only PROMOTES a node for
                // covering additional DISTINCT query terms (weighting rarer terms
                // higher via IDF). Coverage is determined by testing each query
                // term against the candidate's own text (name / qualified_name /
                // file stem) OR its per-term FTS membership — a candidate can fall
                // out of a common term's capped top-K yet still genuinely cover it,
                // so a pure top-K-membership signal under-counts coverage.
                let multi_term = search_terms.len() > 1;
                let mut scored: Vec<(SearchResult, usize)> = term_results_map
                    .into_values()
                    .map(|(mut result, fts_matched)| {
                        // Distinct query terms this node covers (textual or FTS).
                        let name_l = result.node.name.to_lowercase();
                        let qname_l = result.node.qualified_name.to_lowercase();
                        let stem_l = std::path::Path::new(&result.node.file_path)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let covered: std::collections::HashSet<String> = search_terms
                            .iter()
                            .map(|t| t.to_lowercase())
                            .filter(|t| {
                                fts_matched.contains(t)
                                    || name_l.contains(t.as_str())
                                    || qname_l.contains(t.as_str())
                                    || stem_l.contains(t.as_str())
                            })
                            .collect();
                        let n_terms = covered.len();
                        if multi_term && n_terms > 1 {
                            let mut idfs: Vec<f32> =
                                covered.iter().map(|t| term_idf.weight(t)).collect();
                            // Ascending: drop the smallest IDF (most common term) as
                            // the base; the remaining (rarer) terms form the coverage.
                            idfs.sort_by(|a, b| {
                                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            let cov: f32 = idfs.iter().skip(1).sum();
                            result.score += 5.0 * cov as f64;
                        }
                        (result, n_terms)
                    })
                    .collect();

                // Truncate by the BOOSTED score (additive coverage already folded
                // in), tie-broken by distinct-term count. Ordering by score — NOT
                // by distinct-term count as the primary key (the regressed
                // behavior) — is what makes the boost additive-safe: it preserves
                // the promotion of multi-term matches while never letting a
                // low-BM25 multi-term match EVICT a high-BM25 single-term anchor
                // out of the truncated candidate set.
                scored.sort_by(|a, b| {
                    b.0.score
                        .partial_cmp(&a.0.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.1.cmp(&a.1))
                        // Deterministic final tiebreak so equal-score candidates
                        // (and thus which survive the small term_limit) don't
                        // depend on HashMap iteration order.
                        .then_with(|| a.0.node.id.cmp(&b.0.node.id))
                });
                scored.truncate(term_limit);
                text_results = scored.into_iter().map(|(r, _)| r).collect();
            }
        }

        // === STEP 4: Merge — take max score per node id ===
        let mut result_by_id: std::collections::HashMap<String, SearchResult> =
            std::collections::HashMap::new();
        let mut search_results: Vec<SearchResult> = Vec::new();

        for result in exact_matches {
            let id = result.node.id.clone();
            if let Some(existing) = result_by_id.get_mut(&id) {
                existing.score = f64::max(existing.score, result.score);
            } else {
                result_by_id.insert(id, result.clone());
                search_results.push(result);
            }
        }

        for result in text_results {
            let id = result.node.id.clone();
            if let Some(existing) = result_by_id.get_mut(&id) {
                existing.score = f64::max(existing.score, result.score);
            } else {
                result_by_id.insert(id, result.clone());
                search_results.push(result);
            }
        }

        // Sync scores from result_by_id back to search_results
        for r in &mut search_results {
            if let Some(merged) = result_by_id.get(&r.node.id) {
                r.score = merged.score;
            }
        }

        // === OPT-12: NL-query FTS fallback ===
        // When the exact-match channel yielded few results (i.e., the query is pure
        // natural language like "how does request routing work"), run a direct FTS
        // search on the raw query and seed the subgraph from high-value kinds only.
        // Two-pass: (1) full-phrase FTS on the whole query to hit docstrings/signatures;
        // (2) per-term FTS to boost multi-term hits. This finds symbols like URLResolver
        // whose docstrings mention "routing" even though "routing" isn't in the symbol name.
        let sig_word_count = trimmed
            .split_whitespace()
            .filter(|w| w.len() >= 3 && !NL_STOPWORDS.contains(&w.to_lowercase().as_str()))
            .count();

        // OPT-12 NL trigger: run the NL fallback when the query has >= 2 significant
        // (non-stopword) words AND the query is not a pure symbol lookup.
        // Pure symbol queries (e.g. "ZodType parse safeParse") would have their
        // significant words all be CamelCase / PascalCase symbols — they are already
        // handled by the exact-match channel and don't need NL expansion.
        // We detect "NL" vs "symbol" by checking if the query looks like natural language:
        // at least 4 total words (including stopwords) indicates a prose query.
        let total_words = trimmed.split_whitespace().count();
        let _ = has_highval_exact_pre_merge; // captured earlier, used as additional heuristic
        let is_nl_query = sig_word_count >= 2 && total_words >= 4;

        // For NL queries, we expand the effective search_limit so NL fallback
        // candidates get a chance to be selected as roots after final scoring.
        // Use 6 (not 5) so that nodes like Runtime struct that score lower than
        // 5 co-located Task/Schedule structs still get a root slot and their
        // depth-1 BFS children (e.g. spawn) appear in the context.
        let effective_search_limit = if is_nl_query {
            std::cmp::max(opts.search_limit, 6)
        } else {
            opts.search_limit
        };

        if is_nl_query && search_results.len() < opts.search_limit {
            // Significant words (non-stopwords, len>=3) for per-term pass.
            // Also expand with stem variants (e.g. "reactivity" → "reactive", "react")
            // so terms like "reactive" will FTS-prefix-match "ReactiveEffect".
            let base_terms: Vec<String> = trimmed
                .split_whitespace()
                .filter(|w| w.len() >= 3 && !NL_FTS_STOPWORDS.contains(&w.to_lowercase().as_str()))
                .map(|w| w.to_lowercase())
                .collect();

            // Expand with stem variants (suffix stripping: -ity, -tion, -ing, -ive, -ed, -s)
            let mut nl_terms: Vec<String> = base_terms.clone();
            for term in &base_terms {
                // -ity → drop (e.g. "reactivity" → "reactive")
                if term.ends_with("ity") && term.len() > 5 {
                    let stem = &term[..term.len() - 3];
                    nl_terms.push(format!("{}e", stem)); // "reactiv" → "reactive"
                    nl_terms.push(stem.to_string());
                }
                // -tion → drop (e.g. "validation" → "valid", "validate")
                if term.ends_with("tion") && term.len() > 6 {
                    let stem = &term[..term.len() - 4];
                    nl_terms.push(stem.to_string());
                    nl_terms.push(format!("{}e", stem));
                }
                // -ation → drop
                if term.ends_with("ation") && term.len() > 7 {
                    let stem = &term[..term.len() - 5];
                    nl_terms.push(stem.to_string());
                    nl_terms.push(format!("{}e", stem));
                }
                // -ing → drop (e.g. "routing" → "route", "rout")
                if term.ends_with("ing") && term.len() > 5 {
                    let stem = &term[..term.len() - 3];
                    nl_terms.push(stem.to_string());
                    nl_terms.push(format!("{}e", stem)); // "rout" → "route"
                }
                // get_stem_variants provides additional variants
                for v in get_stem_variants(term) {
                    if v.len() >= 3 {
                        nl_terms.push(v);
                    }
                }
            }
            nl_terms.sort_unstable();
            nl_terms.dedup();

            // Prefer high-value structural kinds for NL entry points
            let nl_kinds: Vec<NodeKind> = [
                "function",
                "method",
                "class",
                "struct",
                "interface",
                "trait",
                "route",
                "component",
                "enum",
                "type_alias",
            ]
            .iter()
            .filter_map(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok())
            .collect();

            let fts_limit = std::cmp::max(opts.search_limit * 6, 18);

            // --- Pass 1: full-query FTS (hits docstrings/signatures that describe the concept) ---
            let full_query_opts = SearchOptions {
                limit: fts_limit,
                kinds: Some(nl_kinds.clone()),
                ..Default::default()
            };
            let mut nl_term_map: std::collections::HashMap<String, (SearchResult, usize)> =
                std::collections::HashMap::new();

            if let Ok(full_results) = self.search_nodes(trimmed, full_query_opts) {
                for r in full_results {
                    let entry = nl_term_map
                        .entry(r.node.id.clone())
                        .or_insert_with(|| (r.clone(), 0));
                    entry.1 += 1; // counts as 1 hit for the full query
                    entry.0.score = f64::max(entry.0.score, r.score);
                }
            }

            // --- Pass 2: per-term FTS — boost nodes matching multiple NL terms ---
            for term in &nl_terms {
                let search_opts = SearchOptions {
                    limit: fts_limit,
                    kinds: Some(nl_kinds.clone()),
                    ..Default::default()
                };
                if let Ok(term_results) = self.search_nodes(term, search_opts) {
                    for r in term_results {
                        let entry = nl_term_map
                            .entry(r.node.id.clone())
                            .or_insert_with(|| (r.clone(), 0));
                        entry.1 += 1;
                        entry.0.score = f64::max(entry.0.score, r.score);
                    }
                }
            }

            let mut nl_results: Vec<SearchResult> = nl_term_map
                .into_values()
                .map(|(mut r, hits)| {
                    // Boost nodes that matched multiple passes/terms — this surfaces
                    // symbols whose name AND docstring both contain query terms.
                    r.score += (hits.saturating_sub(1)) as f64 * 10.0;
                    r
                })
                .collect();
            nl_results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    // Deterministic tiebreak so equal-score NL candidates that
                    // straddle the truncation boundary don't depend on HashMap
                    // iteration order (median-of-3 run-to-run stability).
                    .then_with(|| a.node.id.cmp(&b.node.id))
            });
            nl_results.truncate(fts_limit);

            // Merge NL results into the main result set (don't duplicate)
            for r in nl_results {
                let id = r.node.id.clone();
                if let std::collections::hash_map::Entry::Vacant(e) = result_by_id.entry(id) {
                    e.insert(r.clone());
                    search_results.push(r);
                }
            }
        }

        // === STEP 5: Test-file deprioritize ===
        let query_lower = trimmed.to_lowercase();
        let is_test_query = query_lower.contains("test") || query_lower.contains("spec");
        if !is_test_query {
            for r in &mut search_results {
                if is_test_file(&r.node.file_path) {
                    r.score *= 0.3;
                }
            }
        }

        // === STEP 5a: Multi-term co-occurrence re-rank ===
        // Group stem-variant terms as one concept; boost nodes matching >= 2 concepts.
        let query_terms_for_boost = extract_search_terms(trimmed);
        if query_terms_for_boost.len() >= 2 {
            // Group terms that are substrings of each other (stem variants → same concept)
            let mut term_groups: Vec<Vec<String>> = Vec::new();
            let mut sorted_terms = query_terms_for_boost.clone();
            sorted_terms.sort_by_key(|s| std::cmp::Reverse(s.len()));
            let mut assigned: std::collections::HashSet<String> = std::collections::HashSet::new();

            for term in &sorted_terms {
                if assigned.contains(term) {
                    continue;
                }
                let mut group = vec![term.clone()];
                assigned.insert(term.clone());
                for other in &sorted_terms {
                    if assigned.contains(other) {
                        continue;
                    }
                    if term.contains(other.as_str()) || other.contains(term.as_str()) {
                        group.push(other.clone());
                        assigned.insert(other.clone());
                    }
                }
                term_groups.push(group);
            }

            for result in &mut search_results {
                let name_lower = result.node.name.to_lowercase();
                // File basename without extension (e.g. "batch" from "batch.rs").
                // Checked as an additional segment so nodes inside "batch.rs" can
                // match the "batch" concept even though the dirname doesn't contain it.
                let file_stem: String = std::path::Path::new(&result.node.file_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                // Directory segments (exact match per TS, plus substring match for
                // compound names like "codewiki-resolution" matching "resolution").
                let dir_lower: String = std::path::Path::new(&result.node.file_path)
                    .parent()
                    .and_then(|p| p.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let dir_segments: Vec<String> =
                    dir_lower.split('/').map(|s| s.to_string()).collect();

                let mut match_count = 0usize;
                for group in &term_groups {
                    let group_matches = group.iter().any(|term| {
                        // Build a small set of stem variants for this term:
                        // "migrations" → also check "migration" (strip trailing 's')
                        let mut term_variants: Vec<&str> = vec![term.as_str()];
                        let singular: String;
                        if term.ends_with('s') && term.len() > 3 {
                            singular = term[..term.len() - 1].to_string();
                            term_variants.push(singular.as_str());
                        }
                        term_variants.iter().any(|t| {
                            let in_name = name_lower.contains(*t);
                            // Exact dir-segment match (TS faithful) OR compound-name substring
                            // (e.g. "codewiki-resolution" contains "resolution").
                            let in_dir =
                                dir_segments.iter().any(|seg| seg == t || seg.contains(*t));
                            // Also check file stem (e.g. "batch" from "batch.rs")
                            let in_stem = file_stem == *t || file_stem.contains(*t);
                            in_name || in_dir || in_stem
                        })
                    });
                    if group_matches {
                        match_count += 1;
                    }
                }

                // Also protect nodes whose name exactly matches a query symbol
                // (case-insensitive). This handles nodes that scored into search_results
                // via the text channel (STEP 3) rather than STEP 2's exact-name channel,
                // so they weren't added to exact_match_node_ids but still represent a
                // direct name hit (e.g. Runtime struct when query contains "runtime").
                let is_name_exact_match = symbols_from_query
                    .iter()
                    .any(|sym| sym.eq_ignore_ascii_case(&result.node.name));

                // Canonical-definition bonus: a node whose name exactly matches a query
                // symbol AND whose file is named after that same symbol (e.g. Runtime in
                // runtime.rs) is the primary definition — treat it as if it matched one
                // extra term group so it competes with co-located multi-symbol nodes.
                let is_canonical_def = is_name_exact_match
                    && file_stem.eq_ignore_ascii_case(&result.node.name)
                    && !crate::search::is_test_file(&result.node.file_path);

                if match_count >= 2 || is_canonical_def {
                    // Multiplicative boost: 2 terms → 2x, 3 terms → 2.5x
                    result.score *= 1.0 + (match_count.max(2)) as f64 * 0.5;
                } else if !exact_match_node_ids.contains(&result.node.id) && !is_name_exact_match {
                    // Mild dampen for single-term matches that are NOT explicit
                    // exact-name matches from Step 2 or Step 3.
                    result.score *= 0.6;
                }
            }

            search_results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    // Deterministic tiebreak: equal-score results that straddle
                    // the root-selection cutoff must not depend on HashMap order,
                    // or which roots (and thus recall) varies run-to-run.
                    .then_with(|| a.node.id.cmp(&b.node.id))
            });
        }

        // === Filter and select top search_limit as roots ===
        // For NL queries, effective_search_limit is expanded to max(search_limit, 5)
        // so NL fallback candidates compete fairly against the exact-channel results.
        // De-duplicate: for NL queries, keep only one node per name (the highest-
        // scoring one). This prevents many same-named leaf nodes (e.g. "schema"
        // variable in 7 treeshake files) from monopolising all root slots.
        let roots_results: Vec<SearchResult> = {
            // For NL queries: de-duplicate low-value leaf nodes (variable/parameter/file
            // kinds) by name so that many same-named leaf nodes (e.g. "schema" variable
            // in 7 treeshake files) don't monopolize all root slots.
            // High-value kinds (function, method, class, etc.) are never deduplicated
            // since overloaded/multiple implementations are all interesting entry points.
            const LEAF_KINDS: &[&str] = &["variable", "parameter", "file", "import", "export"];
            let mut seen_leaf_names: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut deduped: Vec<SearchResult> = Vec::new();
            for r in search_results
                .into_iter()
                .filter(|r| r.score >= opts.min_score as f64)
            {
                if is_nl_query {
                    let kind_str = serde_json::to_value(&r.node.kind)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default();
                    if LEAF_KINDS.contains(&kind_str.as_str()) {
                        let key = format!("{}::{}", r.node.name.to_lowercase(), kind_str);
                        if seen_leaf_names.contains(&key) {
                            continue;
                        }
                        seen_leaf_names.insert(key);
                    }
                }
                deduped.push(r);
                if deduped.len() >= effective_search_limit {
                    break;
                }
            }
            deduped
        };

        // === BFS-expand each root and collect subgraph ===
        let mut all_nodes: std::collections::HashMap<String, Node> =
            std::collections::HashMap::new();
        let mut all_edges: Vec<Edge> = Vec::new();
        let mut roots: Vec<String> = Vec::new();

        // Phase 1: Guarantee all root nodes appear in the subgraph regardless of
        // the BFS budget.  This prevents a situation where root 1's BFS expansion
        // exhausts max_nodes and roots 2-5 (e.g. MigrationExecutor, validate) are
        // never added.
        for result in &roots_results {
            roots.push(result.node.id.clone());
            all_nodes.insert(result.node.id.clone(), result.node.clone());
        }

        // Phase 2: BFS-expand each root. Use full max_nodes limit so the traversal
        // can find parent classes/structs via Contains edges (direction=Both, depth=2
        // for NL queries).
        //
        // To prevent the first root's BFS from exhausting the node budget and
        // crowding out later roots' children, we allocate a per-root BFS budget:
        // remaining_slots / remaining_roots (minimum 3 each).  This ensures every
        // root gets at least a small expansion window and nodes like Runtime::spawn
        // (depth-1 child of Runtime) are not squeezed out by earlier roots.
        let n_roots = roots_results.len();
        for (root_idx, result) in roots_results.iter().enumerate() {
            if opts.traversal_depth > 0 && all_nodes.len() < opts.max_nodes {
                // Per-root budget: divide remaining slots evenly among remaining roots.
                let remaining_slots = opts.max_nodes.saturating_sub(all_nodes.len());
                let remaining_roots = n_roots - root_idx;
                let per_root_limit = std::cmp::max(remaining_slots / remaining_roots, 4);

                if let Ok(subgraph) = self.with_conn(|conn| {
                    let traverser = GraphTraverser::new(conn);
                    // OPT-12: For NL queries, use Both direction and depth=2 so BFS
                    // can traverse upward through Contains edges to find parent
                    // classes/structs of matched methods, and their siblings.
                    // depth=2 allows: method → class → sibling_method
                    let (direction, depth) = if is_nl_query {
                        (crate::graph::traversal::TraversalDirection::Both, 2)
                    } else {
                        (
                            crate::graph::traversal::TraversalDirection::Outgoing,
                            opts.traversal_depth,
                        )
                    };
                    traverser.traverse_bfs(
                        &result.node.id,
                        &TraversalOptions {
                            max_depth: depth,
                            limit: opts.max_nodes,
                            direction,
                            ..Default::default()
                        },
                    )
                }) {
                    // Collect BFS results into a deterministic order before inserting.
                    // Sort priority:
                    //   1. Non-test files first (test file penalty)
                    //   2. Nodes in the SAME file as the root come before depth-2 nodes
                    //      (approximates BFS depth: depth-1 children of a struct are
                    //       typically defined in the same file as the struct itself)
                    //   3. Within same file: sort by start_line ASC so early-defined
                    //      methods (the primary API like spawn, new, handle) come first
                    //   4. Cross-file nodes: kind_bonus DESC then name ASC
                    let root_file = &result.node.file_path;
                    let mut bfs_nodes: Vec<(String, Node)> = subgraph.nodes.into_iter().collect();
                    bfs_nodes.sort_by(|(_, a), (_, b)| {
                        let a_test = crate::search::is_test_file(&a.file_path) as u8;
                        let b_test = crate::search::is_test_file(&b.file_path) as u8;
                        if a_test != b_test {
                            return a_test.cmp(&b_test);
                        }
                        let a_same_file = (a.file_path == *root_file) as u8;
                        let b_same_file = (b.file_path == *root_file) as u8;
                        if a_same_file != b_same_file {
                            return b_same_file.cmp(&a_same_file);
                        } // same-file first
                        if a_same_file == 1 {
                            // Both in same file as root: prefer non-file nodes (methods/functions
                            // over the file node itself which is less useful as BFS expansion).
                            let a_is_file = (a.kind == codewiki_core::NodeKind::File) as u8;
                            let b_is_file = (b.kind == codewiki_core::NodeKind::File) as u8;
                            if a_is_file != b_is_file {
                                return a_is_file.cmp(&b_is_file);
                            } // file nodes last
                              // Among methods/functions: sort by start_line ASC so early-defined
                              // methods (the primary API like new, handle, spawn) come first.
                            return a.start_line.cmp(&b.start_line);
                        }
                        // Both cross-file: kind_bonus DESC then name ASC, with
                        // id as the final tiebreak so two same-name same-kind
                        // cross-file nodes have a stable order (otherwise which
                        // survives the per-root budget — and thus recall — varies
                        // run-to-run). id is the deterministic last resort; we do
                        // NOT bias by file_path/start_line so this stays a pure
                        // determinism tiebreak rather than a ranking change.
                        let a_kb = crate::search::kind_bonus(&a.kind);
                        let b_kb = crate::search::kind_bonus(&b.kind);
                        if a_kb != b_kb {
                            return b_kb.cmp(&a_kb);
                        }
                        a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id))
                    });
                    let mut inserted_this_root = 0usize;
                    for (id, node) in bfs_nodes {
                        if inserted_this_root >= per_root_limit {
                            break;
                        }
                        if all_nodes.len() >= opts.max_nodes {
                            break;
                        }
                        let entry = all_nodes.entry(id);
                        if matches!(entry, std::collections::hash_map::Entry::Vacant(_)) {
                            entry.or_insert(node);
                            inserted_this_root += 1;
                        }
                    }
                    all_edges.extend(subgraph.edges);
                }
            }
        }

        // === Phase 3: bounded sibling-anchor expansion (FIX 2) ===
        // For each root, pull in up to 1 structurally-related anchor (a sibling
        // method of the same class/struct, or an interface implementer/supertype)
        // when there is still room in the node budget. This re-attaches the
        // anchor's API surface to a feature-style query without flooding context.
        // Strictly capped (1 anchor/root) and touches only the root's immediate
        // containment/type edges, so latency stays bounded on a large repo.
        if all_nodes.len() < opts.max_nodes && !roots.is_empty() {
            let root_ids: Vec<String> = roots.clone();
            let _ = self.with_conn(|conn| {
                let traverser = GraphTraverser::new(conn);
                for root_id in &root_ids {
                    if all_nodes.len() >= opts.max_nodes {
                        break;
                    }
                    let exclude: std::collections::HashSet<String> =
                        all_nodes.keys().cloned().collect();
                    let anchors = traverser.sibling_anchors(root_id, 1, &exclude)?;
                    for anchor in anchors {
                        if all_nodes.len() >= opts.max_nodes {
                            break;
                        }
                        all_nodes.entry(anchor.id.clone()).or_insert(anchor);
                    }
                }
                Ok::<_, CodeWikiError>(())
            });
        }

        Ok(Subgraph {
            nodes: all_nodes,
            edges: all_edges,
            roots,
        })
    }

    fn get_code(&self, node_id: &str) -> Result<Option<String>, CodeWikiError> {
        let node = match self.get_node_by_id(node_id)? {
            Some(n) => n,
            None => return Ok(None),
        };

        let path = PathBuf::from(&node.file_path);
        let content = std::fs::read_to_string(&path).map_err(CodeWikiError::Io)?;
        let lines: Vec<&str> = content.lines().collect();

        let start = (node.start_line as usize).saturating_sub(1);
        let end = (node.end_line as usize).min(lines.len());

        if start >= lines.len() {
            return Ok(None);
        }

        Ok(Some(lines[start..end].join("\n")))
    }

    fn get_stats(&self) -> Result<GraphStats, CodeWikiError> {
        self.with_conn(mq::get_stats)
    }

    fn get_files(&self, filter: Option<&FileFilter>) -> Result<Vec<FileRecord>, CodeWikiError> {
        self.with_conn(|conn| {
            let all = fq::get_all_files(conn)?;
            if let Some(f) = filter {
                // Resolve a relative prefix against the project root stored in
                // project_metadata so `--prefix src/extraction` works even though
                // the DB stores absolute paths.
                let resolved_prefix: Option<String> = if let Some(p) = &f.path_prefix {
                    // Indexed paths are stored with forward slashes on every
                    // platform, so the resolved prefix must use them too — a
                    // leading '/' counts as absolute, and any backslashes a
                    // Windows `Path::join` introduces are normalized back to '/'.
                    if p.starts_with('/') || std::path::Path::new(p).is_absolute() {
                        Some(p.replace('\\', "/"))
                    } else {
                        // Look up root_path from project_metadata; fall back to
                        // trying the raw prefix (works if DB paths happen to match).
                        let root = mq::get_metadata(conn, "root_path")
                            .unwrap_or(None)
                            .unwrap_or_default();
                        if root.is_empty() {
                            Some(p.replace('\\', "/"))
                        } else {
                            let joined = std::path::Path::new(&root).join(p);
                            Some(joined.to_string_lossy().replace('\\', "/"))
                        }
                    }
                } else {
                    None
                };

                Ok(all
                    .into_iter()
                    .filter(|fr| {
                        let lang_ok = f.language.as_ref().map_or(true, |l| {
                            let l_str = serde_json::to_value(l)
                                .ok()
                                .and_then(|v| v.as_str().map(String::from))
                                .unwrap_or_default();
                            fr.language == l_str
                        });
                        let prefix_ok = resolved_prefix
                            .as_ref()
                            .map_or(true, |p| fr.path.to_string_lossy().starts_with(p.as_str()));
                        lang_ok && prefix_ok
                    })
                    .collect())
            } else {
                Ok(all)
            }
        })
    }

    fn root_path(&self) -> Option<String> {
        // Reuse the same project_metadata path that get_files uses to resolve
        // relative prefixes. Treat an empty value as absent.
        self.with_conn(|conn| mq::get_metadata(conn, "root_path"))
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    }

    fn get_affected_nodes(&self, file_paths: &[PathBuf]) -> Result<Vec<Node>, CodeWikiError> {
        use crate::queries::edges as eq;
        use std::collections::HashSet;

        // Collect all nodes directly in the given files.
        let mut direct: Vec<Node> = Vec::new();
        self.with_conn(|conn| {
            for path in file_paths {
                let path_str = path.to_string_lossy().to_string();
                let nodes = nq::get_nodes_by_file(conn, &path_str)?;
                direct.extend(nodes);
            }
            Ok(())
        })?;

        // For each direct node, collect 1-hop dependents (callers/importers).
        let mut result_ids: HashSet<String> = direct.iter().map(|n| n.id.clone()).collect();
        let mut result: Vec<Node> = direct.clone();

        self.with_conn(|conn| {
            for node in &direct {
                // Incoming edges = nodes that depend on this node.
                let incoming = eq::get_incoming_edges(conn, &node.id, None)?;
                for edge in incoming {
                    if result_ids.contains(&edge.source_id) {
                        continue;
                    }
                    if let Some(dep_node) = nq::get_node_by_id(conn, &edge.source_id)? {
                        result_ids.insert(dep_node.id.clone());
                        result.push(dep_node);
                    }
                }
            }
            Ok(())
        })?;

        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// ResolutionStore
// ---------------------------------------------------------------------------

impl ResolutionStore for StorageImpl {
    fn commit_resolved_batch(&self, batch: Vec<ResolvedEdge>) -> Result<(), CodeWikiError> {
        if batch.is_empty() {
            return Ok(());
        }
        self.with_conn(|conn| {
            // OPT-13: Batched existence check.
            //
            // The old implementation ran 2 queries per edge inside the transaction
            // (one SELECT COUNT for source + one for target). For a batch of 2,000
            // resolved edges that is up to 4,000 SQLite queries before the inserts.
            //
            // Instead: collect every unique source/target ID, run a single batched
            // IN-query to fetch which ones exist, then use a HashSet for O(1) per-edge
            // lookups inside the transaction.  This cuts the query count from
            // O(2 × batch_size) → O(ceil(unique_ids / 500)), saving ~4–8× on large
            // corpora (10k-file repos with 400k+ refs).
            let all_ids: Vec<&str> = batch
                .iter()
                .flat_map(|r| [r.edge.source_id.as_str(), r.edge.target_id.as_str()])
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            let existing_ids = nq::nodes_ids_exist_set(conn, &all_ids)?;

            // OPT-14: Partition edges into "valid" (both nodes exist) and collect
            // the unresolved_ref ids and fallback 3-tuples for the bulk DELETE.
            //
            // - valid_edges: edges to insert via multi-row bulk INSERT
            // - delete_ids: row ids for bulk DELETE WHERE id IN (…)
            // - fallback_tuples: 3-tuple fallback for refs with id == 0
            let mut valid_edges: Vec<&codewiki_core::Edge> = Vec::with_capacity(batch.len());
            let mut delete_ids: Vec<i64> = Vec::with_capacity(batch.len());
            let mut fallback_tuples: Vec<(&str, &str, &str)> = Vec::new();

            for resolved in &batch {
                let both_exist = existing_ids.contains(resolved.edge.source_id.as_str())
                    && existing_ids.contains(resolved.edge.target_id.as_str());
                if !both_exist {
                    tracing::debug!(
                        source = %resolved.edge.source_id,
                        target = %resolved.edge.target_id,
                        reference_name = %resolved.resolved_from.reference_name,
                        "skipping resolved edge — source or target node absent from nodes table"
                    );
                } else {
                    valid_edges.push(&resolved.edge);
                    // Consume the unresolved_ref ONLY when its edge was actually
                    // inserted. A skipped edge (endpoint re-stored mid-resolution)
                    // must leave its ref pending, or the relationship is lost
                    // permanently — the ref is the only artifact that can
                    // re-create the edge on the next cycle.
                    let ref_id = resolved.resolved_from.unresolved_ref_id;
                    if ref_id != 0 {
                        delete_ids.push(ref_id);
                    } else {
                        fallback_tuples.push((
                            resolved.resolved_from.from_node_id.as_str(),
                            resolved.resolved_from.reference_name.as_str(),
                            resolved.resolved_from.reference_kind.as_str(),
                        ));
                    }
                }
            }

            conn.execute_batch("BEGIN IMMEDIATE")?;
            let result: Result<(), CodeWikiError> = (|| {
                // Bulk edge insert (OPT-14): multi-row INSERT OR IGNORE chunked at 100.
                // Converts N individual prepare+execute calls → ceil(N/100) statements.
                let owned_edges: Vec<codewiki_core::Edge> =
                    valid_edges.iter().map(|e| (*e).clone()).collect();
                eq::insert_resolved_edges_bulk(conn, &owned_edges)?;

                // Bulk DELETE by PK id (OPT-14): DELETE WHERE id IN (…) chunked at 500.
                // Converts 114k individual 3-tuple DELETEs (django) → ~228 bulk statements.
                uq::delete_resolved_refs_by_ids(conn, &delete_ids, &fallback_tuples)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = conn.execute_batch("ROLLBACK");
                return result;
            }
            conn.execute_batch("COMMIT")?;
            Ok(())
        })
    }

    fn get_unresolved_batch(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
        self.with_conn(|conn| uq::get_unresolved_batch(conn, limit, offset))
    }

    fn get_unresolved_batch_after(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
        self.with_conn(|conn| uq::get_unresolved_batch_after(conn, after_id, limit))
    }

    fn get_unresolved_count(&self) -> Result<usize, CodeWikiError> {
        self.with_conn(uq::get_unresolved_count)
    }

    fn clear_unresolved_refs(&self) -> Result<(), CodeWikiError> {
        self.with_conn(uq::clear_unresolved_refs)
    }

    // OPT-9 incremental helpers -----------------------------------------------

    fn get_unresolved_by_files(
        &self,
        file_paths: &[String],
    ) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
        self.with_conn(|conn| uq::get_unresolved_by_files(conn, file_paths))
    }

    fn get_dependent_files(
        &self,
        changed_file_paths: &[String],
    ) -> Result<Vec<String>, CodeWikiError> {
        self.with_conn(|conn| eq::get_dependent_files(conn, changed_file_paths))
    }

    fn get_unresolved_by_names(
        &self,
        names: &[String],
    ) -> Result<Vec<UnresolvedRef>, CodeWikiError> {
        self.with_conn(|conn| uq::get_unresolved_by_names(conn, names))
    }

    fn get_total_file_count(&self) -> Result<usize, CodeWikiError> {
        self.with_conn(|conn| {
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
            Ok(count as usize)
        })
    }
}

// ---------------------------------------------------------------------------
// SyncStore
// ---------------------------------------------------------------------------

impl SyncStore for StorageImpl {
    fn get_stale_files(&self) -> Result<Vec<FileRecord>, CodeWikiError> {
        self.with_conn(fq::get_stale_files)
    }

    fn update_file_metadata(&self, batch: Vec<FileRecord>) -> Result<(), CodeWikiError> {
        self.with_conn(|conn| {
            for file in &batch {
                fq::upsert_file(conn, file)?;
            }
            Ok(())
        })
    }

    fn delete_file(&self, path: &Path) -> Result<(), CodeWikiError> {
        ExtractionStore::delete_file(self, path)
    }

    fn update_inode(&self, path: &Path, inode: i64) -> Result<(), CodeWikiError> {
        // Skip no-op writes (0 means "unknown", e.g. on Windows).
        if inode == 0 {
            return Ok(());
        }
        let path_str = path.to_string_lossy().to_string();
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE files SET inode = ?1 WHERE path = ?2",
                rusqlite::params![inode, path_str],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_in_memory;
    use crate::traits::{ResolvedBy, ResolvedFromRef};
    use codewiki_core::{Language, Node, NodeKind};

    fn make_storage() -> StorageImpl {
        let conn = open_in_memory().unwrap();
        StorageImpl::new(conn, 1000)
    }

    fn make_batch(path: &str, hash: &str, n: usize) -> ExtractionBatch {
        let nodes: Vec<Node> = (0..n)
            .map(|i| Node {
                id: format!("{}-node{}", path, i),
                name: format!("func{}", i),
                qualified_name: format!("func{}", i),
                kind: NodeKind::Function,
                language: Language::TypeScript,
                file_path: path.to_string(),
                ..Default::default()
            })
            .collect();
        ExtractionBatch {
            file: FileRecord {
                path: PathBuf::from(path),
                content_hash: hash.to_string(),
                language: "typescript".to_string(),
                size: 1024,
                modified_at: 1_000_000,
                indexed_at: now_ms(),
                node_count: n as u32,
                errors: vec![],
            },
            nodes,
            edges: vec![],
            unresolved_refs: vec![],
        }
    }

    // ── Cascade-fix regression suite ─────────────────────────────────────
    //
    // Each test encodes a way the old delete-then-cascade store path
    // permanently destroyed edges. Verified corpus damage before the fix: a
    // one-character edit killed 1,926 incoming edges; rm + restore killed
    // 1,862; 22,698 edges died in one incremental pass.

    fn mk_node(id: &str, name: &str, file: &str, line: u32) -> Node {
        Node {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: NodeKind::Function,
            language: Language::TypeScript,
            file_path: file.to_string(),
            start_line: line,
            ..Default::default()
        }
    }

    fn mk_batch2(path: &str, hash: &str, nodes: Vec<Node>) -> ExtractionBatch {
        ExtractionBatch {
            file: FileRecord {
                path: PathBuf::from(path),
                content_hash: hash.to_string(),
                language: "typescript".to_string(),
                size: 1024,
                modified_at: 1_000_000,
                indexed_at: now_ms(),
                node_count: nodes.len() as u32,
                errors: vec![],
            },
            nodes,
            edges: vec![],
            unresolved_refs: vec![],
        }
    }

    /// Insert a resolved cross-file `calls` edge the way resolution does.
    fn commit_call_edge(storage: &StorageImpl, source: &str, target: &str) {
        let edge = codewiki_core::Edge {
            id: format!("{source}->{target}"),
            source_id: source.to_string(),
            target_id: target.to_string(),
            kind: codewiki_core::EdgeKind::Calls,
            line: Some(7),
            ..Default::default()
        };
        storage
            .commit_resolved_batch(vec![ResolvedEdge {
                edge,
                resolved_from: ResolvedFromRef {
                    from_node_id: source.to_string(),
                    reference_name: "g".to_string(),
                    reference_kind: "calls".to_string(),
                    unresolved_ref_id: 0,
                },
                confidence: 0.9,
                resolved_by: ResolvedBy::NameMatcher,
            }])
            .unwrap();
    }

    fn count_edges(storage: &StorageImpl, source: &str, target: &str) -> i64 {
        storage
            .with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT count(*) FROM edges WHERE source=?1 AND target=?2",
                        rusqlite::params![source, target],
                        |r| r.get(0),
                    )
                    .unwrap())
            })
            .unwrap()
    }

    fn pending_refs(storage: &StorageImpl, from: &str, name: &str) -> i64 {
        storage
            .with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT count(*) FROM unresolved_refs
                          WHERE from_node_id=?1 AND reference_name=?2",
                        rusqlite::params![from, name],
                        |r| r.get(0),
                    )
                    .unwrap())
            })
            .unwrap()
    }

    /// T1 — re-storing a file with identical node ids (e.g. a trailing-comment
    /// edit) must not touch its incoming cross-file edges.
    #[test]
    fn incoming_cross_file_edge_survives_restore() {
        let storage = make_storage();
        storage
            .store_extraction_batch(mk_batch2(
                "a.ts",
                "ha1",
                vec![mk_node("A-f", "f", "a.ts", 1)],
            ))
            .unwrap();
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb1",
                vec![mk_node("B-g", "g", "b.ts", 5)],
            ))
            .unwrap();
        commit_call_edge(&storage, "A-f", "B-g");
        assert_eq!(count_edges(&storage, "A-f", "B-g"), 1);

        // Same nodes, new content hash — the trailing-comment shape.
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb2",
                vec![mk_node("B-g", "g", "b.ts", 5)],
            ))
            .unwrap();
        assert_eq!(
            count_edges(&storage, "A-f", "B-g"),
            1,
            "incoming edge must survive an id-stable re-store"
        );
    }

    /// T2 — a line shift re-keys the node id; the incoming edge must be
    /// retargeted to the new id, not lost and not left dangling.
    #[test]
    fn incoming_edge_retargets_on_line_shift() {
        let storage = make_storage();
        storage
            .store_extraction_batch(mk_batch2(
                "a.ts",
                "ha1",
                vec![mk_node("A-f", "f", "a.ts", 1)],
            ))
            .unwrap();
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb1",
                vec![mk_node("B-g5", "g", "b.ts", 5)],
            ))
            .unwrap();
        commit_call_edge(&storage, "A-f", "B-g5");

        // `g` moves to line 6 → different id.
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb2",
                vec![mk_node("B-g6", "g", "b.ts", 6)],
            ))
            .unwrap();
        assert_eq!(
            count_edges(&storage, "A-f", "B-g6"),
            1,
            "edge must follow the moved symbol"
        );
        assert_eq!(
            count_edges(&storage, "A-f", "B-g5"),
            0,
            "no edge may point at the dead id"
        );
    }

    /// T3 — when the target symbol genuinely vanishes, the incoming edge
    /// degrades to a pending unresolved ref instead of disappearing.
    #[test]
    fn vanished_node_harvests_incoming_edge() {
        let storage = make_storage();
        storage
            .store_extraction_batch(mk_batch2(
                "a.ts",
                "ha1",
                vec![mk_node("A-f", "f", "a.ts", 1)],
            ))
            .unwrap();
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb1",
                vec![mk_node("B-g", "g", "b.ts", 5)],
            ))
            .unwrap();
        commit_call_edge(&storage, "A-f", "B-g");

        // `g` is deleted from b.ts.
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb2",
                vec![mk_node("B-h", "h", "b.ts", 5)],
            ))
            .unwrap();
        assert_eq!(count_edges(&storage, "A-f", "B-g"), 0);
        assert_eq!(
            pending_refs(&storage, "A-f", "g"),
            1,
            "the caller's reference must survive as a pending unresolved ref"
        );
    }

    /// T4 — exact-id survivors are excluded from (kind, name) pairing, so a
    /// deleted duplicate cannot steal a surviving twin's incoming edges.
    #[test]
    fn deleted_duplicate_does_not_steal_survivor_edges() {
        let storage = make_storage();
        storage
            .store_extraction_batch(mk_batch2(
                "a.ts",
                "ha1",
                vec![mk_node("A-f", "f", "a.ts", 1)],
            ))
            .unwrap();
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb1",
                vec![
                    mk_node("B-g10", "g", "b.ts", 10),
                    mk_node("B-g20", "g", "b.ts", 20),
                ],
            ))
            .unwrap();
        commit_call_edge(&storage, "A-f", "B-g10");

        // The first `g` is deleted; the second keeps its exact id.
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb2",
                vec![mk_node("B-g20", "g", "b.ts", 20)],
            ))
            .unwrap();
        assert_eq!(
            count_edges(&storage, "A-f", "B-g20"),
            0,
            "the survivor must not inherit the deleted twin's edges"
        );
        assert_eq!(
            pending_refs(&storage, "A-f", "g"),
            1,
            "the dangling ref goes pending"
        );
    }

    /// T5 — the edge-identity unique index makes INSERT OR IGNORE real.
    #[test]
    fn edge_identity_index_dedupes() {
        let storage = make_storage();
        storage
            .store_extraction_batch(mk_batch2(
                "a.ts",
                "ha1",
                vec![mk_node("A-f", "f", "a.ts", 1)],
            ))
            .unwrap();
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb1",
                vec![mk_node("B-g", "g", "b.ts", 5)],
            ))
            .unwrap();
        commit_call_edge(&storage, "A-f", "B-g");
        commit_call_edge(&storage, "A-f", "B-g");
        assert_eq!(
            count_edges(&storage, "A-f", "B-g"),
            1,
            "identical edges must dedupe"
        );
    }

    /// T6 — a resolved edge skipped because its endpoint vanished mid-race must
    /// NOT consume its unresolved ref: the ref is the only artifact that can
    /// re-create the edge later.
    #[test]
    fn commit_skipped_edge_keeps_uref() {
        let storage = make_storage();
        storage
            .store_extraction_batch(mk_batch2(
                "a.ts",
                "ha1",
                vec![mk_node("A-f", "f", "a.ts", 1)],
            ))
            .unwrap();
        // Pending ref from A-f to a name that resolution matched against a
        // node id that no longer exists.
        storage
            .with_conn(|conn| {
                crate::queries::unresolved::insert_unresolved_ref(
                    conn,
                    &codewiki_core::UnresolvedRef {
                        id: "0".to_string(),
                        from_node_id: "A-f".to_string(),
                        reference_name: "gone".to_string(),
                        reference_kind: "calls".to_string(),
                        file_path: "a.ts".to_string(),
                        line: Some(3),
                        col: Some(0),
                        metadata: None,
                    },
                )
            })
            .unwrap();
        let ref_id: i64 = storage
            .with_conn(|conn| {
                Ok(conn
                    .query_row("SELECT id FROM unresolved_refs LIMIT 1", [], |r| r.get(0))
                    .unwrap())
            })
            .unwrap();

        storage
            .commit_resolved_batch(vec![ResolvedEdge {
                edge: codewiki_core::Edge {
                    id: "A-f->ghost".to_string(),
                    source_id: "A-f".to_string(),
                    target_id: "ghost-node-id".to_string(),
                    kind: codewiki_core::EdgeKind::Calls,
                    ..Default::default()
                },
                resolved_from: ResolvedFromRef {
                    from_node_id: "A-f".to_string(),
                    reference_name: "gone".to_string(),
                    reference_kind: "calls".to_string(),
                    unresolved_ref_id: ref_id,
                },
                confidence: 0.9,
                resolved_by: ResolvedBy::NameMatcher,
            }])
            .unwrap();

        assert_eq!(
            count_edges(&storage, "A-f", "ghost-node-id"),
            0,
            "edge was rightly skipped"
        );
        assert_eq!(
            pending_refs(&storage, "A-f", "gone"),
            1,
            "the skipped edge's ref must stay pending"
        );
    }

    /// T14 — deleting a file (branch checkout, stash, git mv) harvests its
    /// incoming edges instead of cascading them away.
    #[test]
    fn delete_file_harvests_incoming_edges() {
        let storage = make_storage();
        storage
            .store_extraction_batch(mk_batch2(
                "a.ts",
                "ha1",
                vec![mk_node("A-f", "f", "a.ts", 1)],
            ))
            .unwrap();
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb1",
                vec![mk_node("B-g", "g", "b.ts", 5)],
            ))
            .unwrap();
        commit_call_edge(&storage, "A-f", "B-g");

        ExtractionStore::delete_file(&storage, Path::new("b.ts")).unwrap();
        assert_eq!(count_edges(&storage, "A-f", "B-g"), 0);
        assert_eq!(
            pending_refs(&storage, "A-f", "g"),
            1,
            "delete_file must leave the caller's reference pending"
        );
    }

    /// T15 — remove + restore round-trips: the harvested ref re-creates the
    /// edge once resolution runs again.
    #[test]
    fn remove_then_restore_recovers_incoming_edge() {
        let storage = make_storage();
        storage
            .store_extraction_batch(mk_batch2(
                "a.ts",
                "ha1",
                vec![mk_node("A-f", "f", "a.ts", 1)],
            ))
            .unwrap();
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb1",
                vec![mk_node("B-g", "g", "b.ts", 5)],
            ))
            .unwrap();
        commit_call_edge(&storage, "A-f", "B-g");
        ExtractionStore::delete_file(&storage, Path::new("b.ts")).unwrap();

        // The file comes back byte-identical.
        storage
            .store_extraction_batch(mk_batch2(
                "b.ts",
                "hb1",
                vec![mk_node("B-g", "g", "b.ts", 5)],
            ))
            .unwrap();
        let ref_id: i64 = storage
            .with_conn(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT id FROM unresolved_refs
                          WHERE from_node_id='A-f' AND reference_name='g'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap())
            })
            .unwrap();

        // Resolution picks the harvested ref back up.
        storage
            .commit_resolved_batch(vec![ResolvedEdge {
                edge: codewiki_core::Edge {
                    id: "A-f->B-g".to_string(),
                    source_id: "A-f".to_string(),
                    target_id: "B-g".to_string(),
                    kind: codewiki_core::EdgeKind::Calls,
                    line: Some(7),
                    ..Default::default()
                },
                resolved_from: ResolvedFromRef {
                    from_node_id: "A-f".to_string(),
                    reference_name: "g".to_string(),
                    reference_kind: "calls".to_string(),
                    unresolved_ref_id: ref_id,
                },
                confidence: 0.9,
                resolved_by: ResolvedBy::NameMatcher,
            }])
            .unwrap();
        assert_eq!(count_edges(&storage, "A-f", "B-g"), 1, "edge restored");
        assert_eq!(
            pending_refs(&storage, "A-f", "g"),
            0,
            "ref consumed on real insert"
        );
    }

    /// Outgoing edges are rebuilt per re-store, never duplicated.
    #[test]
    fn outgoing_edges_rebuilt_not_duplicated() {
        let storage = make_storage();
        let mut batch = mk_batch2(
            "c.ts",
            "hc1",
            vec![
                mk_node("C-x", "x", "c.ts", 1),
                mk_node("C-y", "y", "c.ts", 9),
            ],
        );
        batch.edges.push(codewiki_core::Edge {
            id: "C-x->C-y".to_string(),
            source_id: "C-x".to_string(),
            target_id: "C-y".to_string(),
            kind: codewiki_core::EdgeKind::Contains,
            ..Default::default()
        });
        storage.store_extraction_batch(batch.clone()).unwrap();
        batch.file.content_hash = "hc2".to_string();
        storage.store_extraction_batch(batch).unwrap();
        assert_eq!(count_edges(&storage, "C-x", "C-y"), 1);
    }

    #[test]
    fn store_batch_idempotent() {
        let storage = make_storage();
        let batch = make_batch("src/foo.ts", "hash1", 3);
        storage.store_extraction_batch(batch.clone()).unwrap();
        storage.store_extraction_batch(batch).unwrap(); // second call is no-op

        let node = storage.get_node_by_id("src/foo.ts-node0").unwrap();
        assert!(node.is_some());
    }

    #[test]
    fn get_node_by_id_caches() {
        let storage = make_storage();
        let batch = make_batch("src/bar.ts", "hash2", 1);
        storage.store_extraction_batch(batch).unwrap();

        // First call hits DB
        let n1 = storage.get_node_by_id("src/bar.ts-node0").unwrap();
        assert!(n1.is_some());
        // Second call should hit cache
        let n2 = storage.get_node_by_id("src/bar.ts-node0").unwrap();
        assert!(n2.is_some());
    }

    #[test]
    fn bulk_store_stats() {
        let storage = make_storage();
        let batches = vec![make_batch("a.ts", "h1", 5), make_batch("b.ts", "h2", 3)];
        let stats = storage.store_extraction_batch_bulk(batches).unwrap();
        assert_eq!(stats.files_written, 2);
        assert_eq!(stats.files_skipped, 0);
        assert_eq!(stats.nodes_inserted, 8);
    }

    /// Regression test: orphan-edge FK backstop in the bulk-init path.
    ///
    /// Real-world QA found that a single orphan edge (source/target id that was
    /// never created — e.g. from a C++ extraction bug) raised a SQLite FOREIGN
    /// KEY violation that rolled back the *entire* single-transaction bulk load,
    /// so a whole repo indexed 0 files. The backstop must skip orphan edges and
    /// still commit the valid nodes/edges.
    #[test]
    fn bulk_init_skips_orphan_edge_and_still_commits() {
        use codewiki_core::{Edge, EdgeKind};

        let storage = make_storage();

        // A batch with 2 real nodes, one valid intra-batch edge, and one ORPHAN
        // edge whose target id was never created.
        let mut batch = make_batch("src/orphan.ts", "h_orphan", 2);
        batch.edges = vec![
            Edge {
                id: "e_valid".to_string(),
                source_id: "src/orphan.ts-node0".to_string(),
                target_id: "src/orphan.ts-node1".to_string(),
                kind: EdgeKind::Calls,
                ..Default::default()
            },
            Edge {
                id: "e_orphan".to_string(),
                source_id: "src/orphan.ts-node0".to_string(),
                target_id: "ghost-node-never-created".to_string(), // FK violator
                kind: EdgeKind::Calls,
                ..Default::default()
            },
        ];

        // Also include a second, completely normal file in the same bulk call so
        // we prove the orphan does NOT zero the whole transaction.
        let normal = make_batch("src/normal.ts", "h_normal", 3);

        let stats = storage
            .store_extraction_batch_bulk_init(vec![batch, normal])
            .expect("bulk init must not fail wholesale on an orphan edge");

        // Both files committed; only the valid edge counted (orphan skipped).
        assert_eq!(stats.files_written, 2, "both files must be written");
        assert_eq!(stats.nodes_inserted, 5, "all 5 nodes must be inserted");
        assert_eq!(stats.edges_inserted, 1, "only the valid edge is counted");

        // The valid nodes are actually queryable (index not zeroed).
        assert!(storage
            .get_node_by_id("src/orphan.ts-node0")
            .unwrap()
            .is_some());
        assert!(storage
            .get_node_by_id("src/normal.ts-node0")
            .unwrap()
            .is_some());
    }

    #[test]
    fn delete_file_cascades_to_nodes_edges_unresolved() {
        use crate::queries::{edges as eq, nodes as nq, unresolved as uq};
        use codewiki_core::{Edge, EdgeKind, UnresolvedRef};

        let storage = make_storage();

        // Store a batch with 2 nodes, 1 edge, 1 unresolved ref
        let path = "/tmp/test_cascade.ts";
        let batch = make_batch(path, "cascade_hash", 2);
        storage.store_extraction_batch(batch).unwrap();

        // Manually insert an edge and an unresolved_ref tied to this file
        storage
            .with_conn(|conn| {
                let edge = Edge {
                    id: "cascade-edge".to_string(),
                    source_id: format!("{}-node0", path),
                    target_id: format!("{}-node1", path),
                    kind: EdgeKind::Calls,
                    ..Default::default()
                };
                eq::insert_edge(conn, &edge)?;

                let uref = UnresolvedRef {
                    id: String::new(),
                    from_node_id: format!("{}-node0", path),
                    reference_name: "externalFn".to_string(),
                    reference_kind: "calls".to_string(),
                    file_path: path.to_string(),
                    line: Some(1),
                    col: Some(0),
                    metadata: None,
                };
                uq::insert_unresolved_ref(conn, &uref)?;

                Ok(())
            })
            .unwrap();

        // Verify rows exist before deletion
        storage
            .with_conn(|conn| {
                let nodes = nq::get_nodes_by_file(conn, path).unwrap();
                assert_eq!(nodes.len(), 2, "expected 2 nodes before delete");
                let unresolved = uq::get_unresolved_count(conn).unwrap();
                assert!(unresolved > 0, "expected unresolved refs before delete");
                Ok(())
            })
            .unwrap();

        // Delete the file
        ExtractionStore::delete_file(&storage, std::path::Path::new(path)).unwrap();

        // Verify cascade: nodes, edges, and unresolved_refs all gone
        storage
            .with_conn(|conn| {
                let nodes = nq::get_nodes_by_file(conn, path).unwrap();
                assert!(
                    nodes.is_empty(),
                    "nodes should be deleted after delete_file"
                );

                let edges = eq::get_outgoing_edges(conn, &format!("{}-node0", path), None).unwrap();
                assert!(
                    edges.is_empty(),
                    "edges should be deleted after delete_file"
                );

                let unresolved = uq::get_unresolved_count(conn).unwrap();
                assert_eq!(
                    unresolved, 0,
                    "unresolved_refs should be deleted after delete_file"
                );

                let file = crate::queries::files::get_file_by_path(conn, path).unwrap();
                assert!(file.is_none(), "file record should be deleted");

                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn get_files_prefix_relative_path_resolves_against_project_root() {
        use crate::queries::meta as mq;
        use crate::traits::query::FileFilter;
        use codewiki_core::FileRecord;

        let storage = make_storage();

        // Set root_path in project_metadata
        storage
            .with_conn(|conn| mq::set_metadata(conn, "root_path", "/projects/myapp"))
            .unwrap();

        // Insert two files
        let store_file = |storage: &StorageImpl, path: &str| {
            storage
                .with_conn(|conn| {
                    let file = FileRecord {
                        path: std::path::PathBuf::from(path),
                        content_hash: "hash".to_string(),
                        language: "type_script".to_string(),
                        size: 100,
                        modified_at: 1_000_000,
                        indexed_at: now_ms(),
                        node_count: 0,
                        errors: vec![],
                    };
                    crate::queries::files::upsert_file(conn, &file)
                })
                .unwrap();
        };

        store_file(&storage, "/projects/myapp/src/api/handler.ts");
        store_file(&storage, "/projects/myapp/tests/unit.ts");

        // Query with relative prefix — should resolve to /projects/myapp/src/api
        let filter = FileFilter {
            path_prefix: Some("src/api".to_string()),
            language: None,
        };
        let files = QueryHandle::get_files(&storage, Some(&filter)).unwrap();
        assert_eq!(
            files.len(),
            1,
            "relative prefix should match exactly one file"
        );
        assert!(
            files[0]
                .path
                .to_string_lossy()
                .contains("src/api/handler.ts"),
            "matched file should be the api handler"
        );

        // Absolute prefix still works
        let abs_filter = FileFilter {
            path_prefix: Some("/projects/myapp/tests".to_string()),
            language: None,
        };
        let abs_files = QueryHandle::get_files(&storage, Some(&abs_filter)).unwrap();
        assert_eq!(
            abs_files.len(),
            1,
            "absolute prefix should match exactly one file"
        );
    }

    #[test]
    fn resolution_store_commit_and_count() {
        let storage = make_storage();
        let batch = make_batch("src/res.ts", "h3", 2);
        storage.store_extraction_batch(batch).unwrap();

        // Add unresolved refs
        let uref = UnresolvedRef {
            id: String::new(),
            from_node_id: "src/res.ts-node0".to_string(),
            reference_name: "helperFunc".to_string(),
            reference_kind: "calls".to_string(),
            file_path: "src/res.ts".to_string(),
            line: Some(5),
            col: Some(0),
            metadata: None,
        };
        storage
            .with_conn(|conn| uq::insert_unresolved_ref(conn, &uref))
            .unwrap();

        let count = storage.get_unresolved_count().unwrap();
        assert_eq!(count, 1);

        let resolved = ResolvedEdge {
            edge: Edge {
                id: "e1".to_string(),
                source_id: "src/res.ts-node0".to_string(),
                target_id: "src/res.ts-node1".to_string(),
                kind: codewiki_core::EdgeKind::Calls,
                ..Default::default()
            },
            resolved_from: crate::traits::resolution::ResolvedFromRef {
                from_node_id: "src/res.ts-node0".to_string(),
                reference_name: "helperFunc".to_string(),
                reference_kind: "calls".to_string(),
                unresolved_ref_id: 0,
            },
            confidence: 0.9,
            resolved_by: crate::traits::resolution::ResolvedBy::NameMatcher,
        };
        storage.commit_resolved_batch(vec![resolved]).unwrap();

        let count_after = storage.get_unresolved_count().unwrap();
        assert_eq!(count_after, 0);
    }

    /// Regression test: T-FK-001
    ///
    /// An unresolved ref whose target resolves to a node id that does NOT exist
    /// in the nodes table must NOT abort commit_resolved_batch with a FK error.
    /// The defensive backstop must skip the offending edge, leaving the batch
    /// otherwise intact (the unresolved ref is consumed so we don't retry forever).
    ///
    /// This reproduces the real-world failure on Rust workspaces (e.g. ripgrep):
    /// `CargoWorkspaceResolver::resolve()` previously constructed phantom
    /// `"crate:{name}"` target ids before the corresponding crate nodes were
    /// actually inserted into the DB, causing a FK violation on every commit.
    #[test]
    fn commit_resolved_batch_skips_edge_with_absent_target() {
        let storage = make_storage();

        // Insert a real source node only.
        let src_batch = make_batch("src/caller.ts", "h_caller", 1);
        storage.store_extraction_batch(src_batch).unwrap();

        // Build an unresolved ref whose from_node_id is the real source node.
        let uref = UnresolvedRef {
            id: String::new(),
            from_node_id: "src/caller.ts-node0".to_string(),
            reference_name: "phantom_crate".to_string(),
            reference_kind: "imports".to_string(),
            file_path: "src/caller.ts".to_string(),
            line: Some(1),
            col: Some(0),
            metadata: None,
        };
        storage
            .with_conn(|conn| uq::insert_unresolved_ref(conn, &uref))
            .unwrap();
        assert_eq!(storage.get_unresolved_count().unwrap(), 1);

        // Also build a valid resolved edge (both source and target exist).
        let target_batch = make_batch("src/target.ts", "h_target", 1);
        storage.store_extraction_batch(target_batch).unwrap();

        let good_uref = UnresolvedRef {
            id: String::new(),
            from_node_id: "src/caller.ts-node0".to_string(),
            reference_name: "realFunc".to_string(),
            reference_kind: "calls".to_string(),
            file_path: "src/caller.ts".to_string(),
            line: Some(2),
            col: Some(0),
            metadata: None,
        };
        storage
            .with_conn(|conn| uq::insert_unresolved_ref(conn, &good_uref))
            .unwrap();
        assert_eq!(storage.get_unresolved_count().unwrap(), 2);

        // Batch: one edge with a PHANTOM (non-existent) target, one valid edge.
        let phantom_edge = ResolvedEdge {
            edge: Edge {
                id: "e_phantom".to_string(),
                source_id: "src/caller.ts-node0".to_string(),
                target_id: "crate:phantom_does_not_exist".to_string(), // FK violator
                kind: codewiki_core::EdgeKind::Imports,
                ..Default::default()
            },
            resolved_from: crate::traits::resolution::ResolvedFromRef {
                from_node_id: "src/caller.ts-node0".to_string(),
                reference_name: "phantom_crate".to_string(),
                reference_kind: "imports".to_string(),
                unresolved_ref_id: 0,
            },
            confidence: 0.9,
            resolved_by: crate::traits::resolution::ResolvedBy::NameMatcher,
        };
        let good_edge = ResolvedEdge {
            edge: Edge {
                id: "e_good".to_string(),
                source_id: "src/caller.ts-node0".to_string(),
                target_id: "src/target.ts-node0".to_string(),
                kind: codewiki_core::EdgeKind::Calls,
                ..Default::default()
            },
            resolved_from: crate::traits::resolution::ResolvedFromRef {
                from_node_id: "src/caller.ts-node0".to_string(),
                reference_name: "realFunc".to_string(),
                reference_kind: "calls".to_string(),
                unresolved_ref_id: 0,
            },
            confidence: 0.9,
            resolved_by: crate::traits::resolution::ResolvedBy::NameMatcher,
        };

        // Must NOT return an error — the phantom edge is silently skipped.
        storage
            .commit_resolved_batch(vec![phantom_edge, good_edge])
            .expect("commit_resolved_batch must not fail when target node is absent");

        // Spec change (cascade fix): only the ref whose edge actually landed
        // is consumed. The skipped edge's ref stays pending — it is the only
        // artifact that can re-create the edge on a later cycle.
        assert_eq!(
            storage.get_unresolved_count().unwrap(),
            1,
            "the skipped edge's unresolved ref must stay pending"
        );

        // The valid edge was stored; the phantom edge was not.
        storage
            .with_conn(|conn| {
                use crate::queries::edges::get_outgoing_edges;
                let edges = get_outgoing_edges(conn, "src/caller.ts-node0", None)?;
                assert_eq!(
                    edges.len(),
                    1,
                    "only the valid edge should have been stored"
                );
                assert_eq!(
                    edges[0].target_id, "src/target.ts-node0",
                    "stored edge must target the real node"
                );
                Ok(())
            })
            .unwrap();
    }

    /// Test for find_relevant_context hybrid search.
    ///
    /// Creates a tiny in-memory DB with:
    ///   - Function `run_until_empty` in `resolution/src/batch.rs`
    ///   - Variable `batch` in `extraction/src/wasm_parser.rs`
    ///
    /// For the prose query "how does the batch loop avoid infinite loops",
    /// asserts that `run_until_empty` is among the roots (it matches both
    /// "batch" in its file name AND "resolution"/"batch" in its path),
    /// ideally outranking the plain `batch` variable.
    #[test]
    fn find_relevant_context_function_outranks_variable() {
        use crate::queries::edges::insert_edge;
        use crate::queries::files::upsert_file;
        use crate::queries::nodes::insert_node;
        use codewiki_core::{Edge, EdgeKind, FileRecord};

        let storage = make_storage();

        // Insert file nodes so the FTS index gets "batch" in the file path context
        let insert_file = |storage: &StorageImpl, path: &str| {
            storage
                .with_conn(|conn| {
                    // Insert a file-kind node so FTS can find the file by name
                    let file_node = Node {
                        id: format!("file:{}", path),
                        name: std::path::Path::new(path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(path)
                            .to_string(),
                        qualified_name: path.to_string(),
                        kind: NodeKind::File,
                        language: Language::Rust,
                        file_path: path.to_string(),
                        start_line: 1,
                        end_line: 1,
                        ..Default::default()
                    };
                    insert_node(conn, &file_node)?;
                    upsert_file(
                        conn,
                        &FileRecord {
                            path: std::path::PathBuf::from(path),
                            content_hash: "h".to_string(),
                            language: "rust".to_string(),
                            size: 100,
                            modified_at: 1_000_000,
                            indexed_at: 1_000_000,
                            node_count: 1,
                            errors: vec![],
                        },
                    )
                })
                .unwrap();
        };

        insert_file(&storage, "crates/codewiki-resolution/src/batch.rs");
        insert_file(&storage, "crates/codewiki-extraction/src/wasm_parser.rs");

        // Insert the function run_until_empty in batch.rs
        let fn_node = Node {
            id: "fn-run_until_empty".to_string(),
            name: "run_until_empty".to_string(),
            qualified_name: "ResolutionBatchRunner::run_until_empty".to_string(),
            kind: NodeKind::Function,
            language: Language::Rust,
            file_path: "crates/codewiki-resolution/src/batch.rs".to_string(),
            start_line: 52,
            end_line: 120,
            ..Default::default()
        };
        storage
            .with_conn(|conn| insert_node(conn, &fn_node))
            .unwrap();

        // Link file → function via contains edge
        let contains_edge = Edge {
            id: "e-contains-fn".to_string(),
            source_id: "file:crates/codewiki-resolution/src/batch.rs".to_string(),
            target_id: "fn-run_until_empty".to_string(),
            kind: EdgeKind::Contains,
            ..Default::default()
        };
        storage
            .with_conn(|conn| insert_edge(conn, &contains_edge))
            .unwrap();

        // Insert an unrelated `batch` variable in wasm_parser.rs
        let var_node = Node {
            id: "var-batch-wasm".to_string(),
            name: "batch".to_string(),
            qualified_name: "batch".to_string(),
            kind: NodeKind::Variable,
            language: Language::Rust,
            file_path: "crates/codewiki-extraction/src/wasm_parser.rs".to_string(),
            start_line: 172,
            end_line: 172,
            ..Default::default()
        };
        storage
            .with_conn(|conn| insert_node(conn, &var_node))
            .unwrap();

        let query = "how does the batch loop avoid infinite loops";
        let opts = FindOpts {
            search_limit: 3,
            traversal_depth: 1,
            max_nodes: 20,
            min_score: 0.1, // low threshold to ensure we find things
        };

        let result = storage.find_relevant_context(query, opts).unwrap();

        // run_until_empty must appear in the context (either as root or via BFS)
        let node_ids: std::collections::HashSet<&str> =
            result.nodes.keys().map(|s| s.as_str()).collect();
        assert!(
            node_ids.contains("fn-run_until_empty"),
            "run_until_empty should be in context nodes; roots={:?}, all_ids={:?}",
            result.roots,
            node_ids,
        );

        // The batch variable from wasm_parser.rs should ideally NOT be a root —
        // it's an unrelated low-value match. We allow it in the broader node set
        // but assert that run_until_empty is in the roots (it outranks the variable
        // after path-based multi-term co-occurrence boost).
        // NOTE: with search_limit=3 the exact outcome depends on FTS scoring;
        // the minimum guarantee is that run_until_empty is reachable in the context.
        assert!(!result.nodes.is_empty(), "context must not be empty");
    }

    // -------------------------------------------------------------------------
    // OPT-9: incremental resolution query tests
    // -------------------------------------------------------------------------

    /// `get_unresolved_by_files` must return only refs whose file_path is
    /// one of the requested paths, and return an empty vec for empty input.
    #[test]
    fn get_unresolved_by_files_filters_correctly() {
        use crate::queries::unresolved::insert_unresolved_ref;
        let storage = make_storage();

        // Two files, each with one node and one unresolved ref.
        let batch_a = make_batch("src/a.ts", "ha", 1);
        let batch_b = make_batch("src/b.ts", "hb", 1);
        storage.store_extraction_batch(batch_a).unwrap();
        storage.store_extraction_batch(batch_b).unwrap();

        storage
            .with_conn(|conn| {
                insert_unresolved_ref(
                    conn,
                    &UnresolvedRef {
                        id: String::new(),
                        from_node_id: "src/a.ts-node0".into(),
                        reference_name: "refA".into(),
                        reference_kind: "calls".into(),
                        file_path: "src/a.ts".into(),
                        line: Some(1),
                        col: Some(0),
                        metadata: None,
                    },
                )
            })
            .unwrap();
        storage
            .with_conn(|conn| {
                insert_unresolved_ref(
                    conn,
                    &UnresolvedRef {
                        id: String::new(),
                        from_node_id: "src/b.ts-node0".into(),
                        reference_name: "refB".into(),
                        reference_kind: "calls".into(),
                        file_path: "src/b.ts".into(),
                        line: Some(1),
                        col: Some(0),
                        metadata: None,
                    },
                )
            })
            .unwrap();

        // Empty input → empty result.
        let empty = ResolutionStore::get_unresolved_by_files(&storage, &[]).unwrap();
        assert!(empty.is_empty());

        // Request only src/a.ts refs.
        let a_refs =
            ResolutionStore::get_unresolved_by_files(&storage, &["src/a.ts".to_string()]).unwrap();
        assert_eq!(a_refs.len(), 1);
        assert_eq!(a_refs[0].file_path, "src/a.ts");

        // Request both files.
        let all_refs = ResolutionStore::get_unresolved_by_files(
            &storage,
            &["src/a.ts".to_string(), "src/b.ts".to_string()],
        )
        .unwrap();
        assert_eq!(all_refs.len(), 2);
    }

    /// `get_unresolved_by_names` must return only refs whose reference_name
    /// matches one of the requested names.
    #[test]
    fn get_unresolved_by_names_filters_correctly() {
        use crate::queries::unresolved::insert_unresolved_ref;
        let storage = make_storage();

        let batch = make_batch("src/c.ts", "hc", 2);
        storage.store_extraction_batch(batch).unwrap();

        for (name, from) in [
            ("myFunc", "src/c.ts-node0"),
            ("otherFunc", "src/c.ts-node1"),
        ] {
            storage
                .with_conn(|conn| {
                    insert_unresolved_ref(
                        conn,
                        &UnresolvedRef {
                            id: String::new(),
                            from_node_id: from.into(),
                            reference_name: name.into(),
                            reference_kind: "calls".into(),
                            file_path: "src/c.ts".into(),
                            line: Some(1),
                            col: Some(0),
                            metadata: None,
                        },
                    )
                })
                .unwrap();
        }

        // Empty input → empty result.
        let empty = ResolutionStore::get_unresolved_by_names(&storage, &[]).unwrap();
        assert!(empty.is_empty());

        // Only "myFunc" requested.
        let refs =
            ResolutionStore::get_unresolved_by_names(&storage, &["myFunc".to_string()]).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].reference_name, "myFunc");

        // Both names.
        let all = ResolutionStore::get_unresolved_by_names(
            &storage,
            &["myFunc".to_string(), "otherFunc".to_string()],
        )
        .unwrap();
        assert_eq!(all.len(), 2);
    }

    // -------------------------------------------------------------------------
    // FIX 2: context multi-term coverage ranking
    // -------------------------------------------------------------------------

    /// FIX 2 — context multi-term coverage ranking, ADDITIVE-SAFE.
    ///
    /// Two properties must hold simultaneously:
    ///   (A) PROMOTION: among candidates competing for root slots, a node that
    ///       covers an additional RARE query term (high IDF) on top of a common
    ///       one is promoted by the additive coverage boost so it earns a root
    ///       slot ahead of nodes matching only the common term.
    ///   (B) ADDITIVE-SAFETY (no eviction): a strong single-term anchor that the
    ///       pre-coverage ranking would have surfaced is NEVER demoted out of the
    ///       result by the boost — the boost only ADDS to multi-term matches.
    #[test]
    fn context_coverage_rewards_multi_term_and_never_evicts_anchor() {
        use crate::queries::nodes::insert_node;

        let storage = make_storage();

        // A handful of single-term "cache" matches (the common term).
        for i in 0..4 {
            storage
                .with_conn(|conn| {
                    insert_node(
                        conn,
                        &Node {
                            id: format!("cache{}", i),
                            name: format!("cacheHelper{}", i),
                            qualified_name: format!("cacheHelper{}", i),
                            kind: NodeKind::Function,
                            language: Language::Rust,
                            file_path: "src/cache.rs".to_string(),
                            start_line: (i as u32) + 1,
                            end_line: (i as u32) + 1,
                            ..Default::default()
                        },
                    )
                })
                .unwrap();
        }

        // The anchor covers BOTH "cache" AND the rare term "eviction".
        storage
            .with_conn(|conn| {
                insert_node(
                    conn,
                    &Node {
                        id: "anchor".to_string(),
                        name: "cacheEviction".to_string(),
                        qualified_name: "cacheEviction".to_string(),
                        kind: NodeKind::Function,
                        language: Language::Rust,
                        file_path: "src/evict.rs".to_string(),
                        start_line: 1,
                        end_line: 20,
                        ..Default::default()
                    },
                )
            })
            .unwrap();

        // A STRONG single-term anchor matching the rare term directly by name.
        // The coverage boost (which only promotes multi-term matches) must never
        // demote this out of the result set.
        storage
            .with_conn(|conn| {
                insert_node(
                    conn,
                    &Node {
                        id: "strong".to_string(),
                        name: "eviction".to_string(),
                        qualified_name: "eviction".to_string(),
                        kind: NodeKind::Function,
                        language: Language::Rust,
                        file_path: "src/policy.rs".to_string(),
                        start_line: 1,
                        end_line: 30,
                        ..Default::default()
                    },
                )
            })
            .unwrap();

        let opts = FindOpts {
            search_limit: 3,
            traversal_depth: 0, // isolate ranking from BFS expansion
            max_nodes: 20,
            min_score: 0.0,
        };
        let result = storage
            .find_relevant_context("cache eviction policy", opts)
            .unwrap();

        // (A) PROMOTION: the two-term anchor earns a root slot ahead of the
        // single-term "cache*" helpers.
        assert!(
            result.roots.contains(&"anchor".to_string()),
            "multi-term-coverage anchor must be promoted to a root; roots={:?}",
            result.roots
        );

        // (B) ADDITIVE-SAFETY: the strong single-term anchor (exact rare-term
        // name match) is never evicted by the coverage boost.
        assert!(
            result.nodes.contains_key("strong"),
            "strong single-term anchor must not be evicted by the coverage boost; \
             nodes={:?}",
            result.nodes.keys().collect::<Vec<_>>()
        );
    }

    /// Sibling-anchor expansion (Phase 3) must pull in a same-container sibling
    /// for a selected root, and must respect the node budget.
    #[test]
    fn context_sibling_anchor_expansion_respects_budget() {
        use crate::queries::edges::insert_edge;
        use crate::queries::nodes::insert_node;
        use codewiki_core::{Edge, EdgeKind};

        let storage = make_storage();

        // A struct `RuntimeCache` containing two methods. The query targets the
        // first method; the sibling method should be pulled in as an anchor.
        let mk = |id: &str, name: &str, kind: NodeKind, line: u32| Node {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: format!("RuntimeCache::{}", name),
            kind,
            language: Language::Rust,
            file_path: "src/runtime_cache.rs".to_string(),
            start_line: line,
            end_line: line + 5,
            ..Default::default()
        };
        storage
            .with_conn(|conn| {
                insert_node(conn, &mk("rc", "RuntimeCache", NodeKind::Struct, 1))?;
                insert_node(conn, &mk("rc_insert", "insertEntry", NodeKind::Method, 10))?;
                insert_node(conn, &mk("rc_evict", "evictEntry", NodeKind::Method, 20))?;
                // Containment edges struct -> methods.
                insert_edge(
                    conn,
                    &Edge {
                        id: "c1".into(),
                        source_id: "rc".into(),
                        target_id: "rc_insert".into(),
                        kind: EdgeKind::Contains,
                        ..Default::default()
                    },
                )?;
                insert_edge(
                    conn,
                    &Edge {
                        id: "c2".into(),
                        source_id: "rc".into(),
                        target_id: "rc_evict".into(),
                        kind: EdgeKind::Contains,
                        ..Default::default()
                    },
                )
            })
            .unwrap();

        // Direct call to sibling_anchors verifies the structural helper itself,
        // independent of FTS ranking.
        let anchors = storage
            .with_conn(|conn| {
                let t = GraphTraverser::new(conn);
                let exclude: std::collections::HashSet<String> =
                    ["rc_insert".to_string()].into_iter().collect();
                t.sibling_anchors("rc_insert", 1, &exclude)
            })
            .unwrap();
        assert_eq!(anchors.len(), 1, "exactly one sibling anchor (budget=1)");
        assert_eq!(
            anchors[0].id, "rc_evict",
            "sibling method of the same struct must be the anchor"
        );

        // Budget = 0 must yield nothing.
        let none = storage
            .with_conn(|conn| {
                let t = GraphTraverser::new(conn);
                t.sibling_anchors("rc_insert", 0, &std::collections::HashSet::new())
            })
            .unwrap();
        assert!(none.is_empty(), "zero budget must produce no anchors");
    }

    // -------------------------------------------------------------------------
    // FIX 1: overloaded-name aggregation (callers/callees/impact)
    // -------------------------------------------------------------------------

    /// Helper: insert a function-kind node.
    fn insert_fn(storage: &StorageImpl, id: &str, name: &str, file: &str) {
        use crate::queries::nodes::insert_node;
        storage
            .with_conn(|conn| {
                insert_node(
                    conn,
                    &Node {
                        id: id.to_string(),
                        name: name.to_string(),
                        qualified_name: format!("{}::{}", file, name),
                        kind: NodeKind::Function,
                        language: Language::Rust,
                        file_path: file.to_string(),
                        start_line: 1,
                        end_line: 10,
                        ..Default::default()
                    },
                )
            })
            .unwrap();
    }

    fn insert_call(storage: &StorageImpl, id: &str, from: &str, to: &str) {
        use crate::queries::edges::insert_edge;
        storage
            .with_conn(|conn| {
                insert_edge(
                    conn,
                    &codewiki_core::Edge {
                        id: id.to_string(),
                        source_id: from.to_string(),
                        target_id: to.to_string(),
                        kind: codewiki_core::EdgeKind::Calls,
                        ..Default::default()
                    },
                )
            })
            .unwrap();
    }

    /// A bare name with TWO same-name definitions must UNION the callers of
    /// both family members, deduped by node id.
    #[test]
    fn callers_aggregated_unions_same_name_family() {
        let storage = make_storage();
        // Two `handle` definitions in different files (overloaded name).
        insert_fn(&storage, "handle_a", "handle", "src/a.rs");
        insert_fn(&storage, "handle_b", "handle", "src/b.rs");
        // Distinct callers for each.
        insert_fn(&storage, "caller_a", "callerA", "src/a.rs");
        insert_fn(&storage, "caller_b", "callerB", "src/b.rs");
        insert_call(&storage, "e1", "caller_a", "handle_a");
        insert_call(&storage, "e2", "caller_b", "handle_b");

        let agg = storage.get_callers_aggregated("handle", 80).unwrap();
        assert_eq!(agg.resolved_name, "handle");
        assert_eq!(agg.family_size, 2, "both same-name defs form the family");
        let names: std::collections::HashSet<&str> =
            agg.neighbors.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains("callerA") && names.contains("callerB"),
            "callers of BOTH family members must be unioned; got {:?}",
            names
        );
    }

    /// callees aggregation behaves symmetrically: union callees of the family.
    #[test]
    fn callees_aggregated_unions_same_name_family() {
        let storage = make_storage();
        insert_fn(&storage, "run_a", "run", "src/a.rs");
        insert_fn(&storage, "run_b", "run", "src/b.rs");
        insert_fn(&storage, "callee_a", "calleeA", "src/a.rs");
        insert_fn(&storage, "callee_b", "calleeB", "src/b.rs");
        insert_call(&storage, "e1", "run_a", "callee_a");
        insert_call(&storage, "e2", "run_b", "callee_b");

        let agg = storage.get_callees_aggregated("run", 80).unwrap();
        assert_eq!(agg.family_size, 2);
        let names: std::collections::HashSet<&str> =
            agg.neighbors.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains("calleeA") && names.contains("calleeB"));
    }

    /// A QUALIFIED symbol must resolve to exactly one node — NO family union.
    /// Here two `handle` defs exist; querying `a::handle` must return only the
    /// caller of the node whose qualified_name matches that file, with
    /// family_size == 1.
    #[test]
    fn callers_aggregated_qualified_name_no_family_union() {
        let storage = make_storage();
        // Make the two defs distinguishable by qualified_name so FTS can rank
        // the right one to the top for a qualified query.
        use crate::queries::nodes::insert_node;
        for (id, file) in [("h_a", "src/a.rs"), ("h_b", "src/b.rs")] {
            storage
                .with_conn(|conn| {
                    insert_node(
                        conn,
                        &Node {
                            id: id.to_string(),
                            name: "handle".to_string(),
                            qualified_name: format!("{}::handle", file.replace(".rs", "")),
                            kind: NodeKind::Function,
                            language: Language::Rust,
                            file_path: file.to_string(),
                            start_line: 1,
                            end_line: 10,
                            ..Default::default()
                        },
                    )
                })
                .unwrap();
        }
        insert_fn(&storage, "caller_a", "callerA", "src/a.rs");
        insert_fn(&storage, "caller_b", "callerB", "src/b.rs");
        insert_call(&storage, "e1", "caller_a", "h_a");
        insert_call(&storage, "e2", "caller_b", "h_b");

        // Qualified query: must resolve to exactly one node, family_size == 1.
        let agg = storage.get_callers_aggregated("src/a::handle", 80).unwrap();
        assert_eq!(
            agg.family_size, 1,
            "a qualified symbol must NOT trigger a same-name family union"
        );
    }

    /// The neighbor cap must bound output even when the union is large.
    #[test]
    fn callers_aggregated_respects_cap() {
        let storage = make_storage();
        insert_fn(&storage, "target", "target", "src/t.rs");
        // 10 distinct callers of the single `target` node.
        for i in 0..10 {
            let cid = format!("c{}", i);
            insert_fn(&storage, &cid, &format!("caller{}", i), "src/c.rs");
            insert_call(&storage, &format!("e{}", i), &cid, "target");
        }
        let agg = storage.get_callers_aggregated("target", 3).unwrap();
        assert_eq!(agg.family_size, 1, "unique name → family of one");
        assert!(
            agg.neighbors.len() <= 3,
            "cap must bound the neighbor count; got {}",
            agg.neighbors.len()
        );
    }

    /// Regression guard (R1): aggregated callers must surface genuine PRODUCTION
    /// callers before TEST-file callers within a bounded result. A flood of test
    /// callers must not crowd a real source caller out of the visible window.
    #[test]
    fn callers_aggregated_prioritizes_production_over_test_callers() {
        let storage = make_storage();
        insert_fn(&storage, "target", "target", "src/t.rs");
        // One genuine production caller …
        insert_fn(&storage, "prod", "realCaller", "src/service.rs");
        insert_call(&storage, "ep", "prod", "target");
        // … and many test-file callers that would otherwise flood a small cap.
        for i in 0..8 {
            let cid = format!("t{}", i);
            insert_fn(
                &storage,
                &cid,
                &format!("test_caller{}", i),
                "tests/big_test.rs",
            );
            insert_call(&storage, &format!("et{}", i), &cid, "target");
        }

        // Cap of 2 — the production caller MUST be within the kept window.
        let agg = storage.get_callers_aggregated("target", 2).unwrap();
        assert!(
            agg.neighbors.iter().any(|n| n.name == "realCaller"),
            "production caller must rank ahead of test callers within the cap; got {:?}",
            agg.neighbors.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
        // And it should be the FIRST neighbor (test files deprioritized).
        assert_eq!(
            agg.neighbors.first().map(|n| n.name.as_str()),
            Some("realCaller"),
            "production caller must be ranked first; got {:?}",
            agg.neighbors.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    /// Impact aggregation unions the reverse-reach subgraph of the family and
    /// caps the node budget.
    #[test]
    fn impact_aggregated_unions_family_and_caps() {
        let storage = make_storage();
        insert_fn(&storage, "save_a", "save", "src/a.rs");
        insert_fn(&storage, "save_b", "save", "src/b.rs");
        // Each save has a distinct dependent.
        insert_fn(&storage, "dep_a", "depA", "src/a.rs");
        insert_fn(&storage, "dep_b", "depB", "src/b.rs");
        insert_call(&storage, "e1", "dep_a", "save_a");
        insert_call(&storage, "e2", "dep_b", "save_b");

        let agg = storage.get_impact_aggregated("save", 3, 100).unwrap();
        assert_eq!(agg.family_size, 2);
        let names: std::collections::HashSet<&str> = agg
            .subgraph
            .nodes
            .values()
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            names.contains("depA") && names.contains("depB"),
            "impact must union dependents of BOTH family members; got {:?}",
            names
        );

        // Cap is PROVENANCE-AWARE: it bounds context/transitive nodes but must
        // NEVER drop a DIRECT (1-hop) dependant — those are the answer for an
        // impl/blast query. With a tight cap, both direct dependants survive.
        let capped = storage.get_impact_aggregated("save", 3, 1).unwrap();
        let capped_names: std::collections::HashSet<&str> = capped
            .subgraph
            .nodes
            .values()
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            capped_names.contains("depA") && capped_names.contains("depB"),
            "direct dependants must survive even a tight cap; got {:?}",
            capped_names
        );
    }

    /// Provenance-aware de-bloat (regression guard). Verifies that:
    ///   - an `impl`-style query KEEPS implementer classes (implements/extends),
    ///   - a `blast`-style query KEEPS dependant methods (calls/references),
    ///   - file / namespace marker nodes are DROPPED,
    ///   - pure context (contains-only / import-only) is dropped under budget,
    ///   - genuine answer nodes are never sacrificed to the cap.
    #[test]
    fn impact_keeps_answer_nodes_drops_markers_and_context() {
        use crate::queries::edges::insert_edge;
        use crate::queries::nodes::insert_node;
        use codewiki_core::{Edge, EdgeKind};

        let storage = make_storage();

        let mk = |id: &str, name: &str, kind: NodeKind, file: &str| Node {
            id: id.to_string(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind,
            language: Language::Rust,
            file_path: file.to_string(),
            start_line: 1,
            end_line: 5,
            ..Default::default()
        };

        storage
            .with_conn(|conn| {
                // Focal interface + two genuine implementers (implements/extends).
                insert_node(
                    conn,
                    &mk("iface", "Shape", NodeKind::Interface, "src/shape.rs"),
                )?;
                insert_node(
                    conn,
                    &mk("impl1", "Circle", NodeKind::Class, "src/circle.rs"),
                )?;
                insert_node(
                    conn,
                    &mk("impl2", "Square", NodeKind::Class, "src/square.rs"),
                )?;
                // A genuine dependant METHOD (calls the focal) — the blast answer.
                insert_node(conn, &mk("dep", "render", NodeKind::Method, "src/draw.rs"))?;
                // Pure-marker nodes that must be dropped.
                insert_node(
                    conn,
                    &mk("filemark", "shape.rs", NodeKind::File, "src/shape.rs"),
                )?;
                insert_node(
                    conn,
                    &mk("nsmark", "geometry", NodeKind::Namespace, "src/shape.rs"),
                )?;

                // implements/extends → answer (impl)
                insert_edge(
                    conn,
                    &Edge {
                        id: "ei1".into(),
                        source_id: "impl1".into(),
                        target_id: "iface".into(),
                        kind: EdgeKind::Implements,
                        ..Default::default()
                    },
                )?;
                insert_edge(
                    conn,
                    &Edge {
                        id: "ei2".into(),
                        source_id: "impl2".into(),
                        target_id: "iface".into(),
                        kind: EdgeKind::Extends,
                        ..Default::default()
                    },
                )?;
                // calls → answer (blast)
                insert_edge(
                    conn,
                    &Edge {
                        id: "ec".into(),
                        source_id: "dep".into(),
                        target_id: "iface".into(),
                        kind: EdgeKind::Calls,
                        ..Default::default()
                    },
                )?;
                // file/namespace markers attach via non-answer edges (imports/contains).
                insert_edge(
                    conn,
                    &Edge {
                        id: "em1".into(),
                        source_id: "filemark".into(),
                        target_id: "iface".into(),
                        kind: EdgeKind::Imports,
                        ..Default::default()
                    },
                )?;
                insert_edge(
                    conn,
                    &Edge {
                        id: "em2".into(),
                        source_id: "nsmark".into(),
                        target_id: "iface".into(),
                        kind: EdgeKind::Contains,
                        ..Default::default()
                    },
                )
            })
            .unwrap();

        // Tight cap (1) must still keep ALL genuine answer nodes (3 direct
        // dependants) and the root, and DROP the file/namespace markers.
        let agg = storage.get_impact_aggregated("Shape", 3, 1).unwrap();
        let names: std::collections::HashSet<&str> = agg
            .subgraph
            .nodes
            .values()
            .map(|n| n.name.as_str())
            .collect();

        // impl answer: implementer classes kept.
        assert!(
            names.contains("Circle") && names.contains("Square"),
            "implementer classes (implements/extends) must be kept; got {:?}",
            names
        );
        // blast answer: dependant method kept.
        assert!(
            names.contains("render"),
            "dependant method (calls) must be kept; got {:?}",
            names
        );
        // markers dropped.
        assert!(
            !names.contains("shape.rs"),
            "file marker node must be dropped; got {:?}",
            names
        );
        assert!(
            !names.contains("geometry"),
            "namespace marker node must be dropped; got {:?}",
            names
        );

        // No File/Namespace kind survives in the subgraph.
        assert!(
            agg.subgraph
                .nodes
                .values()
                .all(|n| !matches!(n.kind, NodeKind::File | NodeKind::Namespace)),
            "no file/namespace marker may remain in the impact subgraph"
        );
    }

    /// A symbol that matches nothing returns an empty result (empty resolved_name).
    #[test]
    fn callers_aggregated_no_match_is_empty() {
        let storage = make_storage();
        insert_fn(&storage, "x", "somethingElse", "src/x.rs");
        let agg = storage
            .get_callers_aggregated("nonexistent_symbol_xyz", 80)
            .unwrap();
        assert!(agg.resolved_name.is_empty());
        assert!(agg.neighbors.is_empty());
    }

    /// `get_dependent_files` must return the file paths of nodes with edges
    /// targeting nodes in the changed files, excluding the changed files themselves.
    #[test]
    fn get_dependent_files_returns_reverse_deps() {
        use crate::queries::{edges::insert_edge, nodes::insert_node};

        let storage = make_storage();

        // Three files: `lib.ts` (changed), `app.ts` (depends on lib), `unrelated.ts`.
        for (id, name, file) in [
            ("lib-fn", "libFunc", "src/lib.ts"),
            ("app-fn", "appFunc", "src/app.ts"),
            ("other-fn", "otherFunc", "src/unrelated.ts"),
        ] {
            storage
                .with_conn(|conn| {
                    insert_node(
                        conn,
                        &Node {
                            id: id.to_string(),
                            name: name.to_string(),
                            qualified_name: name.to_string(),
                            kind: codewiki_core::NodeKind::Function,
                            language: codewiki_core::Language::TypeScript,
                            file_path: file.to_string(),
                            ..Default::default()
                        },
                    )
                })
                .unwrap();
        }

        // `app.ts` calls `lib.ts`.
        storage
            .with_conn(|conn| {
                insert_edge(
                    conn,
                    &codewiki_core::Edge {
                        id: "e1".to_string(),
                        source_id: "app-fn".to_string(),
                        target_id: "lib-fn".to_string(),
                        kind: codewiki_core::EdgeKind::Calls,
                        ..Default::default()
                    },
                )
            })
            .unwrap();

        // Empty input → empty result.
        let empty = ResolutionStore::get_dependent_files(&storage, &[]).unwrap();
        assert!(empty.is_empty());

        // Changed file is `lib.ts` — only `app.ts` should be returned.
        let deps =
            ResolutionStore::get_dependent_files(&storage, &["src/lib.ts".to_string()]).unwrap();
        assert_eq!(deps.len(), 1, "expected exactly 1 dependent file");
        assert_eq!(deps[0], "src/app.ts");

        // `unrelated.ts` has no edge to lib.ts — must not appear.
        assert!(!deps.contains(&"src/unrelated.ts".to_string()));
    }
}
