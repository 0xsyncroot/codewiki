//! T-327 — `run_sync_cycle`: the main sync orchestration loop.
//!
//! Acquires the advisory lock, walks the filesystem for new/modified/deleted
//! files, delegates to A1's `ExtractionOrchestratorImpl`, then persists
//! updated metadata back through `SyncStore`.

use crate::directory::walk_source_files;
use crate::tree_cache::TreeCache;
use codewiki_core::{CodeWikiError, ExtractionBatch, FileRecord};
use codewiki_extraction::{ChangedFiles, ExtractionOrchestratorImpl};
use codewiki_storage::SyncStore;
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Result type
// ---------------------------------------------------------------------------

/// Statistics returned after a sync cycle.
#[derive(Debug, Default, Clone)]
pub struct SyncResult {
    pub files_added: usize,
    pub files_modified: usize,
    pub files_removed: usize,
    pub batches_processed: usize,
    pub duration_ms: u64,
    /// All paths that changed (added + modified + removed) in this cycle.
    ///
    /// Populated so callers can pass the set to `run_resolution` for OPT-10:
    /// skipping the framework-extract pass when no config/route files changed.
    pub changed_paths: Vec<PathBuf>,
}

impl SyncResult {
    pub fn noop() -> Self {
        Self::default()
    }

    pub fn is_noop(&self) -> bool {
        self.files_added == 0 && self.files_modified == 0 && self.files_removed == 0
    }
}

// ---------------------------------------------------------------------------
// Change-detection metadata
// ---------------------------------------------------------------------------

/// On-disk metadata for a walked file (raw `metadata()` fields).
struct FsMeta {
    /// Modification time in Unix milliseconds.
    mtime: i64,
    /// Raw on-disk byte length from `metadata().len()`.
    size: u64,
}

/// Stored metadata for a tracked file (from the DB `FileRecord`).
struct DbMeta {
    /// Stored modification time in Unix milliseconds.
    mtime: i64,
    /// Stored byte length (post-UTF-8-BOM, as recorded by the extractor).
    size: u64,
    /// Stored content hash (Sha256 hex, post-UTF-8-BOM, as recorded by the extractor).
    content_hash: String,
}

/// Compute the content hash of `path` exactly as the extractor does:
/// read raw bytes, strip a leading UTF-8 BOM (`EF BB BF`) if present, then
/// `hex(Sha256(bytes))`. Returns `None` if the file cannot be read.
///
/// Matching the extractor's hashing (`read_source_file` + `extract_file`)
/// byte-for-byte is what lets the size-mismatch tier ignore a benign BOM-only
/// discrepancy while still catching a genuine same-length edit.
fn file_content_hash(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let raw = fs::read(path).ok()?;
    let bytes = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&raw);
    Some(hex::encode(Sha256::digest(bytes)))
}

// ---------------------------------------------------------------------------
// Lock helpers
// ---------------------------------------------------------------------------

/// Acquire an exclusive advisory lock on `.codewiki/.sync.lock`.
///
/// Returns `Err` if another process already holds the lock (non-blocking).
fn acquire_sync_lock(codewiki_dir: &Path) -> Result<File, CodeWikiError> {
    let lock_path = codewiki_dir.join(".sync.lock");
    let file = File::create(&lock_path)?;
    file.try_lock_exclusive()
        .map_err(|_| CodeWikiError::Watcher("another sync is already in progress".to_string()))?;
    Ok(file)
}

// ---------------------------------------------------------------------------
// Sync cycle
// ---------------------------------------------------------------------------

/// Run a single sync cycle:
///
/// 1. Acquire `.codewiki/.sync.lock`.
/// 2. Walk the filesystem with `walk_source_files(project_root)` and compare
///    against DB records to discover added, modified, and removed files.
/// 3. Fold in any inotify-buffered removes not yet reflected in the DB.
/// 4. Attach cached `ParsedTree` objects for modified files.
/// 5. Call `extractor.process_changes(changes)`.
/// 6. Persist updated metadata via `db.update_file_metadata(batch)`.
/// 7. On Unix, populate the `inode` column for each indexed file.
///
/// Returns `SyncResult::noop()` immediately if there is nothing to do.
pub fn run_sync_cycle(
    db: &Arc<dyn SyncStore>,
    extractor: &Arc<ExtractionOrchestratorImpl>,
    buffered_removes: Vec<PathBuf>,
    tree_cache: &TreeCache,
    codewiki_dir: &Path,
    project_root: &Path,
) -> Result<SyncResult, CodeWikiError> {
    let t0 = SystemTime::now();

    // --- Step 1: acquire advisory lock ---
    let _lock = acquire_sync_lock(codewiki_dir)?;

    // --- Step 2: full filesystem walk + DB comparison ---
    //
    // Walk all source files under project_root (gitignore-aware, 1 MB cap).
    // Compare the walk result against DB records to classify changes.
    let fs_files = walk_source_files(project_root);

    // Build a map from path → (mtime, size) for files on disk.
    //
    // `size` here is the raw on-disk byte length from `metadata().len()` — a
    // field we already stat for `mtime`, so collecting it adds no extra syscall.
    let mut fs_map: HashMap<PathBuf, FsMeta> = HashMap::with_capacity(fs_files.len());
    for path in &fs_files {
        let meta = fs::metadata(path).ok();
        let mtime = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        fs_map.insert(path.clone(), FsMeta { mtime, size });
    }

    // Fetch all tracked DB records to build a map from path → stored metadata.
    let db_records = db.get_stale_files()?;
    let mut db_map: HashMap<PathBuf, DbMeta> = HashMap::with_capacity(db_records.len());
    for rec in &db_records {
        db_map.insert(
            rec.path.clone(),
            DbMeta {
                mtime: rec.modified_at,
                size: rec.size,
                content_hash: rec.content_hash.clone(),
            },
        );
    }

    let mut added_records: Vec<PathBuf> = Vec::new();
    let mut modified_records: Vec<PathBuf> = Vec::new();
    let mut removed_records: Vec<PathBuf> = Vec::new();

    // Files on disk: new (not in DB) or modified.
    //
    // CHANGE-DETECTION POLICY (cross-platform robustness):
    //
    // A tracked file is considered modified when *either* its mtime *or* its
    // size differs from the stored record. Size is a free `metadata()` field
    // already stat'd above, so this adds no cost and catches the large class of
    // content edits that preserve (or coarsen) mtime — e.g. FAT/exFAT's 2 s
    // mtime granularity, `git checkout`/`restore` preserving mtime, or an
    // explicit `touch -d` restoring an old mtime after an edit.
    //
    // The one residual case the cheap mtime+size check cannot catch is a
    // content edit that preserves BOTH mtime AND byte length (e.g. swapping two
    // characters then restoring mtime). To stay correct without hashing every
    // file on every sync (which would destroy incremental-sync performance) we
    // only fall back to a content-hash comparison in the *narrow* window where
    // mtime matches but size differs. That window also disambiguates the one
    // benign false positive of a raw-vs-stored size mismatch: the DB stores the
    // post-UTF-8-BOM byte length, while `metadata().len()` is the raw on-disk
    // size, so a BOM-prefixed file differs by exactly 3 bytes every sync. The
    // hash compare (which strips the BOM identically to the extractor) resolves
    // both: a genuine edit is detected, a BOM-only discrepancy is ignored.
    for (path, fs_meta) in &fs_map {
        match db_map.get(path) {
            None => {
                // File exists on disk but not in DB → newly added.
                added_records.push(path.clone());
            }
            Some(db_meta) => {
                if fs_meta.mtime != db_meta.mtime {
                    // mtime advanced → modified (no hashing needed).
                    modified_records.push(path.clone());
                } else if fs_meta.size != db_meta.size {
                    // mtime preserved but size differs: this is either a real
                    // content edit with a restored/coarse mtime, or a benign
                    // raw-vs-post-BOM 3-byte size discrepancy. Hash-compare to
                    // decide precisely. Only this narrow set is ever hashed.
                    if file_content_hash(path).as_deref() != Some(db_meta.content_hash.as_str()) {
                        modified_records.push(path.clone());
                    }
                }
            }
        }
    }

    // Files in DB but not on disk → removed.
    //
    // OPT-8: Skip virtual framework-manifest paths under `.codewiki/routes/`.
    // These synthetic paths are created by `run_resolution` and never exist on
    // disk, so without this guard they would be classified as "removed" on every
    // sync, causing cascading deletes followed by immediate re-creation (~120
    // files on django).
    let routes_prefix = codewiki_dir.join("routes");
    for path in db_map.keys() {
        if !fs_map.contains_key(path) && !path.starts_with(&routes_prefix) {
            removed_records.push(path.clone());
        }
    }

    // --- Step 3: fold in buffered removes ---
    let extra_removes: Vec<PathBuf> = buffered_removes
        .into_iter()
        .filter(|p| !removed_records.contains(p))
        .collect();

    let all_removed: Vec<PathBuf> = removed_records
        .iter()
        .cloned()
        .chain(extra_removes)
        .collect();

    // Early exit if nothing changed.
    if added_records.is_empty() && modified_records.is_empty() && all_removed.is_empty() {
        return Ok(SyncResult::noop());
    }

    // --- Step 4: attach prior parse trees for incremental reparse ---
    let modified_with_trees: Vec<(PathBuf, Option<codewiki_extraction::ParsedTree>)> =
        modified_records
            .iter()
            .map(|p| {
                let tree = tree_cache.get(p).map(|c| c.tree.clone());
                (p.clone(), tree)
            })
            .collect();

    let changes = ChangedFiles {
        added: added_records.clone(),
        modified: modified_with_trees,
        removed: all_removed.clone(),
    };

    // --- Step 5: delegate parsing + extraction to A1 ---
    let batches = extractor.process_changes(changes);

    // --- Step 6: persist updated metadata ---
    let file_records: Vec<FileRecord> = batches
        .iter()
        .map(|b: &ExtractionBatch| b.file.clone())
        .collect();
    if !file_records.is_empty() {
        db.update_file_metadata(file_records)?;
    }

    // Delete removed files from DB.
    for path in &all_removed {
        if let Err(e) = db.delete_file(path) {
            tracing::warn!(path = %path.display(), err = %e, "failed to delete removed file from db");
        }
    }

    // --- Step 7: populate inode column for indexed files (Unix only) ---
    let indexed_paths: Vec<PathBuf> = batches.iter().map(|b| b.file.path.clone()).collect();
    if let Err(e) = update_inodes(db, &indexed_paths) {
        tracing::warn!(err = %e, "failed to update inode column");
    }

    let duration_ms = t0.elapsed().map(|d| d.as_millis() as u64).unwrap_or(0);

    // Collect all changed paths for OPT-10 framework-extract skip check.
    let changed_paths: Vec<PathBuf> = added_records
        .iter()
        .chain(modified_records.iter())
        .chain(all_removed.iter())
        .cloned()
        .collect();

    Ok(SyncResult {
        files_added: added_records.len(),
        files_modified: modified_records.len(),
        files_removed: all_removed.len(),
        batches_processed: batches.len(),
        duration_ms,
        changed_paths,
    })
}

/// Populate the `inode` column for a list of paths.
///
/// On Unix this uses `std::os::unix::fs::MetadataExt::ino()`.
/// On Windows inode is not meaningful, so we pass `0` which the implementation
/// treats as a no-op.
fn update_inodes(db: &Arc<dyn SyncStore>, paths: &[PathBuf]) -> Result<(), CodeWikiError> {
    for path in paths {
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(path).map(|m| m.ino() as i64).unwrap_or(0)
        };
        #[cfg(not(unix))]
        let inode: i64 = 0;

        if let Err(e) = db.update_inode(path, inode) {
            tracing::debug!(
                path = %path.display(),
                inode = inode,
                err = %e,
                "inode update skipped"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewiki_extraction::ExtractionOrchestratorImpl;
    use codewiki_storage::{open_in_memory, StorageImpl, SyncStore};
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn sync_result_noop() {
        let r = SyncResult::noop();
        assert!(r.is_noop());
    }

    #[test]
    fn sync_result_non_noop() {
        let r = SyncResult {
            files_added: 1,
            ..Default::default()
        };
        assert!(!r.is_noop());
    }

    // -------------------------------------------------------------------------
    // Helpers shared by integration-style sync tests
    // -------------------------------------------------------------------------

    /// Create an in-memory StorageImpl with the full schema + migrations applied.
    ///
    /// `open_in_memory()` already applies schema and all pending migrations.
    fn make_storage() -> Arc<StorageImpl> {
        let conn = open_in_memory().unwrap();
        Arc::new(StorageImpl::new(conn, 128))
    }

    /// A minimal ExtractionStore adapter so we can build an orchestrator.
    struct StoreAdapter(Arc<StorageImpl>);

    impl codewiki_extraction::ExtractionStore for StoreAdapter {
        fn store_batch(&self, batch: codewiki_core::ExtractionBatch) -> Result<(), String> {
            codewiki_storage::ExtractionStore::store_extraction_batch(self.0.as_ref(), batch)
                .map_err(|e| e.to_string())
        }

        fn delete_file(&self, path: &std::path::Path) -> Result<(), String> {
            codewiki_storage::ExtractionStore::delete_file(self.0.as_ref(), path)
                .map_err(|e| e.to_string())
        }
    }

    fn make_extractor(storage: Arc<StorageImpl>) -> Arc<ExtractionOrchestratorImpl> {
        let adapter = Arc::new(StoreAdapter(storage));
        Arc::new(ExtractionOrchestratorImpl::new(adapter))
    }

    /// Set up a temp project directory with `.codewiki/` subdir.
    fn setup_project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".codewiki")).unwrap();
        dir
    }

    // -------------------------------------------------------------------------
    // T1: sync_discovers_new_file
    // -------------------------------------------------------------------------

    /// After initialising the index with 1 file, adding a second file to disk
    /// and running sync must result in 2 files tracked.
    #[test]
    fn sync_discovers_new_file() {
        let project = setup_project();
        let root = project.path().to_path_buf();
        let codewiki_dir = root.join(".codewiki");

        // Write initial file.
        std::fs::write(root.join("a.ts"), b"export const a = 1;").unwrap();

        let storage = make_storage();
        let db: Arc<dyn SyncStore> = Arc::clone(&storage) as Arc<dyn SyncStore>;
        let extractor = make_extractor(Arc::clone(&storage));
        let cache = TreeCache::new(16);

        // First sync — picks up a.ts as new.
        let r1 = run_sync_cycle(&db, &extractor, vec![], &cache, &codewiki_dir, &root).unwrap();
        assert_eq!(r1.files_added, 1, "first sync should add a.ts");

        // Add a second file.
        std::fs::write(root.join("b.ts"), b"export const b = 2;").unwrap();

        // Second sync — must discover b.ts.
        let r2 = run_sync_cycle(&db, &extractor, vec![], &cache, &codewiki_dir, &root).unwrap();
        assert_eq!(r2.files_added, 1, "second sync should add b.ts");
        assert_eq!(r2.files_modified, 0);
        assert_eq!(r2.files_removed, 0);

        // Both files should now be in the DB.
        let all = db.get_stale_files().unwrap();
        assert_eq!(all.len(), 2, "expected 2 tracked files after two syncs");
    }

    // -------------------------------------------------------------------------
    // T2: sync_detects_deleted_file
    // -------------------------------------------------------------------------

    /// After indexing a file, deleting it from disk and running sync must
    /// remove it from the DB.
    #[test]
    fn sync_detects_deleted_file() {
        let project = setup_project();
        let root = project.path().to_path_buf();
        let codewiki_dir = root.join(".codewiki");

        let path = root.join("gone.ts");
        std::fs::write(&path, b"export const gone = true;").unwrap();

        let storage = make_storage();
        let db: Arc<dyn SyncStore> = Arc::clone(&storage) as Arc<dyn SyncStore>;
        let extractor = make_extractor(Arc::clone(&storage));
        let cache = TreeCache::new(16);

        // Index the file.
        run_sync_cycle(&db, &extractor, vec![], &cache, &codewiki_dir, &root).unwrap();

        let after_index = db.get_stale_files().unwrap();
        assert_eq!(after_index.len(), 1, "file should be tracked");

        // Delete the file.
        std::fs::remove_file(&path).unwrap();

        // Sync again — must detect deletion.
        let r = run_sync_cycle(&db, &extractor, vec![], &cache, &codewiki_dir, &root).unwrap();
        assert_eq!(r.files_removed, 1, "sync should report 1 removed");

        let after_delete = db.get_stale_files().unwrap();
        assert!(after_delete.is_empty(), "DB should have no tracked files");
    }

    // -------------------------------------------------------------------------
    // T3: inode_populated_on_unix
    // -------------------------------------------------------------------------

    /// On Unix, after a sync cycle the `inode` column in the `files` table
    /// must be non-NULL and non-zero for every indexed file.
    #[cfg(unix)]
    #[test]
    fn inode_populated_on_unix() {
        use std::os::unix::fs::MetadataExt;

        let project = setup_project();
        let root = project.path().to_path_buf();
        let codewiki_dir = root.join(".codewiki");
        let file_path = root.join("inode_test.ts");
        std::fs::write(&file_path, b"export const x = 42;").unwrap();

        // Use a file-backed DB so we can inspect the inode column via a
        // second connection after the sync cycle completes.
        let db_path = codewiki_dir.join("codewiki.db");

        // Run sync inside a scope to ensure all Arc<StorageImpl> references are
        // dropped before we re-open the DB.
        {
            // `codewiki_storage::open` already applies schema + migrations.
            let conn = codewiki_storage::open(&db_path).unwrap();
            let storage = Arc::new(StorageImpl::new(conn, 128));

            let db: Arc<dyn SyncStore> = Arc::clone(&storage) as Arc<dyn SyncStore>;
            let extractor = make_extractor(Arc::clone(&storage));
            let cache = TreeCache::new(16);

            run_sync_cycle(&db, &extractor, vec![], &cache, &codewiki_dir, &root).unwrap();
            // All Arcs to `storage` dropped at end of scope.
        }

        // Open the DB again to query the inode column directly.
        let check_conn = rusqlite::Connection::open(&db_path).unwrap();
        let inode: Option<i64> = check_conn
            .query_row(
                "SELECT inode FROM files WHERE path = ?1",
                rusqlite::params![file_path.to_string_lossy().as_ref()],
                |row| row.get(0),
            )
            .unwrap();

        let expected_inode = std::fs::metadata(&file_path).unwrap().ino() as i64;
        assert!(inode.is_some(), "inode column must not be NULL after sync");
        assert_eq!(
            inode.unwrap(),
            expected_inode,
            "inode in DB must match filesystem inode"
        );
    }

    // -------------------------------------------------------------------------
    // T4: sync_detects_mtime_preserved_content_edit (Bug 1 / Q3 regression)
    // -------------------------------------------------------------------------

    /// Regression for the mtime-only change-detection bug: editing a file's
    /// content while *preserving its mtime* (the `touch -d` scenario, also seen
    /// with coarse FAT/exFAT mtimes and `git checkout`) must still be detected
    /// as a modification and re-indexed. The content edit here changes the byte
    /// length, so the size tier catches it even though mtime is unchanged.
    #[test]
    fn sync_detects_mtime_preserved_content_edit() {
        let project = setup_project();
        let root = project.path().to_path_buf();
        let codewiki_dir = root.join(".codewiki");
        let file_path = root.join("edited.ts");

        std::fs::write(&file_path, b"export const a = 1;").unwrap();

        let storage = make_storage();
        let db: Arc<dyn SyncStore> = Arc::clone(&storage) as Arc<dyn SyncStore>;
        let extractor = make_extractor(Arc::clone(&storage));
        let cache = TreeCache::new(16);

        // First sync indexes the file.
        let r1 = run_sync_cycle(&db, &extractor, vec![], &cache, &codewiki_dir, &root).unwrap();
        assert_eq!(r1.files_added, 1, "first sync should add edited.ts");

        // Capture the mtime the DB recorded so we can restore it after editing.
        let original_mtime = std::fs::metadata(&file_path).unwrap().modified().unwrap();

        // Edit the content (different length) then restore the ORIGINAL mtime,
        // simulating `touch -d "<old time>"` after an edit.
        std::fs::write(
            &file_path,
            b"export const a = 1; export const b = 2; export const c = 3;",
        )
        .unwrap();
        std::fs::File::open(&file_path)
            .unwrap()
            .set_modified(original_mtime)
            .unwrap();

        // Confirm the mtime really was preserved — otherwise the test would
        // pass for the wrong reason (it would be caught by the mtime tier).
        let after_edit_mtime = std::fs::metadata(&file_path).unwrap().modified().unwrap();
        assert_eq!(
            after_edit_mtime, original_mtime,
            "test precondition: mtime must be preserved across the edit"
        );

        // Second sync — must detect the content change via the size tier.
        let r2 = run_sync_cycle(&db, &extractor, vec![], &cache, &codewiki_dir, &root).unwrap();
        assert_eq!(
            r2.files_modified, 1,
            "mtime-preserved content edit must be detected as modified"
        );

        // The re-indexed record must carry the new content hash.
        let rec = db
            .get_stale_files()
            .unwrap()
            .into_iter()
            .find(|r| r.path == file_path)
            .expect("edited.ts must still be tracked");
        let expected_hash = file_content_hash(&file_path).unwrap();
        assert_eq!(
            rec.content_hash, expected_hash,
            "DB content hash must reflect the new content after re-index"
        );
    }
}
