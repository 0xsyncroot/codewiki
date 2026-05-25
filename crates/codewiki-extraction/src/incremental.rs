//! T-205/T-222 — IncrementalParser trait.

use crate::types::FileEdit;
use codewiki_core::ExtractionBatch;

/// Trait for incremental reparse support.
///
/// Implementors can try to reparse a file incrementally using a cached
/// tree-sitter tree. If the heuristic decides a full reparse is needed, `None`
/// is returned and the caller must fall back to `extract_file`.
pub trait IncrementalParser: Send + Sync {
    /// Attempt to incrementally reparse `edit.path` using the `cached_tree`.
    ///
    /// Returns `None` if:
    /// - `edit.requires_full_reparse()` is true (>100 LOC changed or >50% size delta).
    /// - The parser is not available for the language.
    fn try_incremental_reparse(
        &self,
        edit: FileEdit,
        cached_tree: &tree_sitter::Tree,
    ) -> Option<ExtractionBatch>;
}
