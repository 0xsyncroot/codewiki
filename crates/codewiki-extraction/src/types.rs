//! T-205 — Extraction-specific types: FileEdit, ParsedTree.

use std::path::PathBuf;
use std::sync::Arc;

pub use crate::config::INCREMENTAL_LOC_THRESHOLD;

/// A byte-level description of a file edit, used for incremental reparse.
///
/// `new_source` must contain the **complete** post-edit file contents (not a
/// patch).  A1 does not reconstruct the file from deltas.
///
/// Marked `#[non_exhaustive]` so future fields (e.g. editor hints) can be
/// added without breaking A5's construction code.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct FileEdit {
    pub path: PathBuf,
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_point: tree_sitter::Point,
    pub old_end_point: tree_sitter::Point,
    pub new_end_point: tree_sitter::Point,
    /// Full file content after the edit.
    pub new_source: String,
}

impl FileEdit {
    /// Construct a `FileEdit` from its constituent fields.
    ///
    /// This constructor exists because `FileEdit` is `#[non_exhaustive]`, which
    /// prevents struct-literal construction outside the defining crate.  A5
    /// calls this when building edit descriptors from cached source diffs.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: PathBuf,
        start_byte: usize,
        old_end_byte: usize,
        new_end_byte: usize,
        start_point: tree_sitter::Point,
        old_end_point: tree_sitter::Point,
        new_end_point: tree_sitter::Point,
        new_source: String,
    ) -> Self {
        Self {
            path,
            start_byte,
            old_end_byte,
            new_end_byte,
            start_point,
            old_end_point,
            new_end_point,
            new_source,
        }
    }

    /// Returns `true` when A1 should skip incremental reparse and do a full
    /// reparse instead.
    ///
    /// Two heuristics:
    /// 1. More than `INCREMENTAL_LOC_THRESHOLD` lines changed.
    /// 2. New source length differs from old by more than 50% of the old length.
    pub fn requires_full_reparse(&self, old_source_len: usize) -> bool {
        let changed_lines = self
            .old_end_point
            .row
            .abs_diff(self.start_point.row)
            .max(self.new_end_point.row.abs_diff(self.start_point.row));

        if changed_lines > INCREMENTAL_LOC_THRESHOLD {
            return true;
        }

        // Size-delta heuristic: >50% of file rewritten → full reparse.
        if old_source_len > 0 {
            let size_delta = self.new_source.len().abs_diff(old_source_len);
            if size_delta > old_source_len / 2 {
                return true;
            }
        }

        false
    }
}

/// A cached parse tree together with its source.
#[derive(Clone)]
pub struct ParsedTree {
    pub tree: tree_sitter::Tree,
    pub source: Arc<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_edit(changed_rows: usize, old_len: usize, new_len: usize) -> FileEdit {
        FileEdit {
            path: PathBuf::from("/tmp/test.ts"),
            start_byte: 0,
            old_end_byte: old_len,
            new_end_byte: new_len,
            start_point: tree_sitter::Point { row: 0, column: 0 },
            old_end_point: tree_sitter::Point {
                row: changed_rows,
                column: 0,
            },
            new_end_point: tree_sitter::Point {
                row: changed_rows,
                column: 0,
            },
            new_source: "x".repeat(new_len),
        }
    }

    #[test]
    fn over_threshold_requires_full() {
        let edit = make_edit(101, 1000, 1000);
        assert!(edit.requires_full_reparse(1000));
    }

    #[test]
    fn under_threshold_small_delta_incremental() {
        let edit = make_edit(50, 1000, 1010);
        assert!(!edit.requires_full_reparse(1000));
    }

    #[test]
    fn size_delta_over_50pct_requires_full() {
        // Old = 1000, new = 600, delta = 400 > 500 * 50%? No. Let's try 200→1000
        let edit = make_edit(5, 200, 1000);
        assert!(edit.requires_full_reparse(200));
    }
}
