/// ResolutionStore trait (T-108) — A4's interface to storage.
use codewiki_core::{CodeWikiError, Edge, UnresolvedRef};

/// How a reference was resolved.
#[derive(Debug, Clone)]
pub enum ResolvedBy {
    FrameworkResolver(String),
    ImportResolver,
    NameMatcher,
}

/// The original unresolved ref that was resolved.
#[derive(Debug, Clone)]
pub struct ResolvedFromRef {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: String,
    /// Row ID of the `unresolved_refs` record this was resolved from.
    /// Used for bulk `DELETE … WHERE id IN (…)` in `commit_resolved_batch`
    /// (OPT-14).  A value of `0` means the id was not recorded and the
    /// fallback 3-tuple delete will be used instead.
    pub unresolved_ref_id: i64,
}

/// A resolved edge ready to be committed.
#[derive(Debug, Clone)]
pub struct ResolvedEdge {
    pub edge: Edge,
    pub resolved_from: ResolvedFromRef,
    pub confidence: f32,
    pub resolved_by: ResolvedBy,
}

/// A4's interface to storage for committing resolved references.
pub trait ResolutionStore: Send + Sync {
    /// Atomically commit a batch of resolved references.
    fn commit_resolved_batch(&self, batch: Vec<ResolvedEdge>) -> Result<(), CodeWikiError>;

    /// Paginated read of pending unresolved references (OFFSET-based; kept for
    /// the incremental path and tests).
    fn get_unresolved_batch(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<UnresolvedRef>, CodeWikiError>;

    /// W6 cursor-based fetch for the full-index path.
    ///
    /// Returns up to `limit` rows with `id > after_id`, ordered by `id`.
    /// Pass `after_id = 0` to start from the beginning.  The O(log n) B-tree
    /// seek replaces the O(offset) scan, eliminating the O(n²) growth at 100k.
    fn get_unresolved_batch_after(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<UnresolvedRef>, CodeWikiError>;

    /// Count of pending unresolved refs.
    fn get_unresolved_count(&self) -> Result<usize, CodeWikiError>;

    /// Clear all unresolved refs after a full resolution pass.
    fn clear_unresolved_refs(&self) -> Result<(), CodeWikiError>;

    // -----------------------------------------------------------------------
    // OPT-9 — incremental resolution helpers
    // -----------------------------------------------------------------------

    /// Return all unresolved refs whose `file_path` is one of `file_paths`.
    /// Uses `idx_unresolved_file_path`.  Empty input returns empty vec.
    fn get_unresolved_by_files(
        &self,
        file_paths: &[String],
    ) -> Result<Vec<UnresolvedRef>, CodeWikiError>;

    /// Return the set of file paths that contain nodes with edges *targeting*
    /// any node in `changed_file_paths` (reverse dependency lookup).
    /// Uses `idx_edges_target_kind` + `idx_nodes_file_path`.
    fn get_dependent_files(
        &self,
        changed_file_paths: &[String],
    ) -> Result<Vec<String>, CodeWikiError>;

    /// Return all unresolved refs whose `reference_name` matches any of
    /// `names`.  Uses `idx_unresolved_name`.  Finds previously-unresolvable
    /// refs that may now resolve because new symbols were added.
    fn get_unresolved_by_names(
        &self,
        names: &[String],
    ) -> Result<Vec<UnresolvedRef>, CodeWikiError>;

    /// Total count of files in the index (used for the 10 % fallback guard).
    fn get_total_file_count(&self) -> Result<usize, CodeWikiError>;
}
