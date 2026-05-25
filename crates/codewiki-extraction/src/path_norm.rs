//! T-019 — NFC/NFD path normalization.
//!
//! On macOS the HFS+ filesystem stores filenames in NFD (decomposed) form while
//! most callers supply NFC strings. This module normalizes paths to NFC on macOS
//! so that DB lookups are consistent regardless of how the path was constructed.
//! On Linux and Windows paths are returned unchanged (Linux filesystems are
//! byte-transparent; Windows NTFS is NFC-normalized by the kernel).

use std::path::{Path, PathBuf};

/// Normalize `path` to NFC on macOS; return it unchanged on Linux/Windows.
pub fn normalize_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        use unicode_normalization::UnicodeNormalization;
        let s = path.to_string_lossy();
        let nfc: String = s.nfc().collect();
        PathBuf::from(nfc)
    }
    #[cfg(not(target_os = "macos"))]
    {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_on_ascii() {
        let p = Path::new("/tmp/foo/bar.ts");
        assert_eq!(normalize_path(p), PathBuf::from("/tmp/foo/bar.ts"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn nfd_to_nfc_on_macos() {
        use unicode_normalization::UnicodeNormalization;
        // NFD form of "é" (e + combining accent)
        let nfd_e: String = "e\u{0301}".to_string();
        let nfd_path = PathBuf::from(format!("/tmp/{}dir/file.ts", nfd_e));
        let normalized = normalize_path(&nfd_path);
        // NFC form should have the pre-composed é
        let expected: String = "é".nfc().collect();
        assert!(normalized.to_string_lossy().contains(&expected));
    }
}
