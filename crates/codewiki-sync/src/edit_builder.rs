//! T-325 — FileEdit builder from old vs. new source content.
//!
//! Computes a single contiguous changed region by scanning from both ends,
//! then constructs a `FileEdit` for tree-sitter's incremental reparse.
//! Returns `None` when the changed region exceeds 25% of file size or
//! 500 lines, or when the inputs are identical.

use codewiki_extraction::FileEdit;
use std::path::Path;
use tree_sitter::Point;

/// Default threshold: if the changed region is >25% of the old file, fall
/// back to full reparse.
pub const FULL_REPARSE_THRESHOLD: f64 = 0.25;
/// Maximum lines changed before triggering full reparse.
pub const FULL_REPARSE_LINE_LIMIT: usize = 500;

/// Attempt to build a `FileEdit` from cached old source and new file content.
///
/// Returns `None` if:
/// - The files are identical (no edit).
/// - The changed ratio exceeds `full_reparse_threshold`.
/// - The number of changed lines exceeds `FULL_REPARSE_LINE_LIMIT`.
///
/// `full_reparse_threshold` is typically `FULL_REPARSE_THRESHOLD` (0.25).
pub fn try_build_file_edit(
    path: &Path,
    old_source: &str,
    new_source: &str,
    full_reparse_threshold: f64,
) -> Option<FileEdit> {
    let old_bytes = old_source.as_bytes();
    let new_bytes = new_source.as_bytes();

    // If identical, no edit to build.
    if old_bytes == new_bytes {
        return None;
    }

    // Find first differing byte.
    let start_byte = old_bytes
        .iter()
        .zip(new_bytes.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| old_bytes.len().min(new_bytes.len()));

    // Find last differing byte (scan backwards from end on the suffix after start_byte).
    let old_suffix = &old_bytes[start_byte..];
    let new_suffix = &new_bytes[start_byte..];
    let common_suffix = old_suffix
        .iter()
        .rev()
        .zip(new_suffix.iter().rev())
        .position(|(a, b)| a != b)
        .unwrap_or(0);

    let old_end_byte = old_bytes.len() - common_suffix;
    let new_end_byte = new_bytes.len() - common_suffix;

    // Reject if the changed region is > threshold fraction of the old file.
    let old_len = old_bytes.len().max(1);
    let changed_ratio = (old_end_byte.saturating_sub(start_byte)) as f64 / old_len as f64;
    if changed_ratio > full_reparse_threshold {
        return None;
    }

    // Reject if changed region spans more than FULL_REPARSE_LINE_LIMIT lines.
    let start_point = byte_to_point(old_source, start_byte);
    let old_end_point = byte_to_point(old_source, old_end_byte);
    let new_end_point = byte_to_point(new_source, new_end_byte);

    let changed_lines = old_end_point
        .row
        .saturating_sub(start_point.row)
        .max(new_end_point.row.saturating_sub(start_point.row));
    if changed_lines > FULL_REPARSE_LINE_LIMIT {
        return None;
    }

    Some(FileEdit::new(
        path.to_path_buf(),
        start_byte,
        old_end_byte,
        new_end_byte,
        start_point,
        old_end_point,
        new_end_point,
        new_source.to_owned(),
    ))
}

/// Convert a byte offset in `source` to a `tree_sitter::Point` (row, col).
///
/// `row` is 0-indexed (number of newlines before the offset).
/// `col` is the byte offset from the start of the current line.
pub fn byte_to_point(source: &str, byte_offset: usize) -> Point {
    let clamped = byte_offset.min(source.len());
    let prefix = &source[..clamped];
    let row = prefix.bytes().filter(|&b| b == b'\n').count();
    let col = prefix
        .rfind('\n')
        .map(|pos| clamped - pos - 1)
        .unwrap_or(clamped);
    Point::new(row, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p() -> PathBuf {
        PathBuf::from("/tmp/test.ts")
    }

    #[test]
    fn identical_sources_returns_none() {
        let src = "const x = 1;\nconst y = 2;\n";
        assert!(try_build_file_edit(&p(), src, src, 0.25).is_none());
    }

    #[test]
    fn small_edit_produces_file_edit() {
        let old = "const x = 1;\nconst y = 2;\nconst z = 3;\n";
        let new = "const x = 1;\nconst y = 99;\nconst z = 3;\n";
        let edit = try_build_file_edit(&p(), old, new, 0.25);
        assert!(edit.is_some(), "small edit should produce a FileEdit");
        let edit = edit.unwrap();
        // The changed region should be within the second line.
        assert!(edit.start_byte > 0);
        assert!(edit.old_end_byte > edit.start_byte);
    }

    #[test]
    fn large_edit_returns_none() {
        // Rewrite >25% of the file.
        let old = "a".repeat(1000) + "\n";
        let new = "b".repeat(1000) + "\n";
        assert!(try_build_file_edit(&p(), &old, &new, 0.25).is_none());
    }

    #[test]
    fn byte_to_point_first_line() {
        let src = "hello world";
        let pt = byte_to_point(src, 5);
        assert_eq!(pt.row, 0);
        assert_eq!(pt.column, 5);
    }

    #[test]
    fn byte_to_point_second_line() {
        let src = "line1\nline2\n";
        let pt = byte_to_point(src, 6); // 'l' of "line2"
        assert_eq!(pt.row, 1);
        assert_eq!(pt.column, 0);
    }

    #[test]
    fn byte_to_point_clamped() {
        let src = "abc";
        let pt = byte_to_point(src, 1000); // clamp to len
        assert_eq!(pt.row, 0);
        assert_eq!(pt.column, 3);
    }

    #[test]
    fn small_append_produces_edit_with_correct_bytes() {
        let old = "function foo() {\n  return 1;\n}\n";
        let new = "function foo() {\n  return 42;\n}\n";
        let edit = try_build_file_edit(&p(), old, new, 0.25).expect("should produce edit");
        // The changed bytes must be within the return statement.
        assert!(edit.start_byte < edit.old_end_byte);
        assert_eq!(edit.new_source, new);
    }

    #[test]
    fn full_rewrite_returns_none_via_ratio() {
        // Replace entire content — ratio = 1.0 > 0.25.
        let old = "const a = 1;";
        let new = "const b = 2;"; // same length but different throughout
                                  // changed region = entire string, ratio = 1.0
        let result = try_build_file_edit(&p(), old, new, 0.25);
        assert!(result.is_none(), "full rewrite should return None");
    }
}
