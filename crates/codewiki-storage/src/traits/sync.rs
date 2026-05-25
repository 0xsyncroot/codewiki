/// SyncStore trait (T-116) — A5's thin interface to storage.
use codewiki_core::{CodeWikiError, FileRecord};
use std::path::Path;

/// A5-facing subset of storage. Avoids A5 depending on the full StorageImpl.
pub trait SyncStore: Send + Sync {
    /// Return all tracked file records so A5 can compare against live FS metadata.
    fn get_stale_files(&self) -> Result<Vec<FileRecord>, CodeWikiError>;

    /// Update file metadata for a batch of files (called after sync cycle).
    fn update_file_metadata(&self, batch: Vec<FileRecord>) -> Result<(), CodeWikiError>;

    /// Remove a file and all its associated data from the database.
    fn delete_file(&self, path: &Path) -> Result<(), CodeWikiError>;

    /// Populate the `inode` column for a tracked file.
    ///
    /// On Windows callers should pass `inode = 0`; the implementation is free
    /// to treat 0 as a no-op.  The default implementation is a no-op so that
    /// test doubles that only implement the core three methods remain valid.
    fn update_inode(&self, path: &Path, inode: i64) -> Result<(), CodeWikiError> {
        let _ = (path, inode);
        Ok(())
    }
}
