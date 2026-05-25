//! T-202 + T-019 — File reader with UTF-8 BOM stripping and NFC path normalization.

use crate::path_norm::normalize_path;
use std::path::Path;

/// Read a source file as UTF-8.
///
/// Returns `None` if:
/// - The file cannot be read (I/O error).
/// - The content is not valid UTF-8.
///
/// Strips the UTF-8 BOM (`EF BB BF`) if present. CRLF line endings are
/// preserved because tree-sitter reports byte positions into the raw buffer;
/// normalizing `\r\n → \n` would corrupt `FileEdit` byte offsets.
pub fn read_source_file(path: &Path) -> Option<String> {
    let path = normalize_path(path);
    let raw = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(path = %path.display(), err = %e, "failed to read file; skipping");
            return None;
        }
    };

    // Strip UTF-8 BOM (EF BB BF).
    let raw = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&raw);

    match std::str::from_utf8(raw) {
        Ok(s) => Some(s.to_owned()),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                byte_offset = e.valid_up_to(),
                "file is not valid UTF-8; skipping"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn strips_utf8_bom() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // UTF-8 BOM + content
        f.write_all(b"\xEF\xBB\xBFhello world").unwrap();
        let result = read_source_file(f.path()).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn returns_none_for_invalid_utf8() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // UTF-16 LE BOM followed by garbage bytes
        f.write_all(b"\xFF\xFE hello").unwrap();
        let result = read_source_file(f.path());
        assert!(result.is_none());
    }

    #[test]
    fn preserves_crlf() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"line1\r\nline2\r\n").unwrap();
        let result = read_source_file(f.path()).unwrap();
        assert!(result.contains('\r'), "CRLF should be preserved");
    }

    #[test]
    fn normal_ascii_file() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"fn main() {}").unwrap();
        let result = read_source_file(f.path()).unwrap();
        assert_eq!(result, "fn main() {}");
    }
}
