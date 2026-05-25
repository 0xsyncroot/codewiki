//! Bug 2 (Q4) integration test — live-on-save sync.
//!
//! Indexes a temp project, starts the live-sync watcher via `spawn_live_sync`,
//! edits a source file, and asserts the watcher picks up the change and
//! re-indexes it (the DB record's content hash updates). Also verifies the
//! handle stops cleanly on drop.
//!
//! On hosts where the watch policy disables watching (e.g. a WSL2 `/mnt/`
//! drive) the test skips gracefully — there git hooks are the auto-sync path.

use codewiki_core::ExtractionBatch;
use codewiki_extraction::ExtractionOrchestratorImpl;
use codewiki_storage::{open, StorageImpl, SyncStore};
use codewiki_sync::sync_loop::run_sync_cycle;
use codewiki_sync::tree_cache::TreeCache;
use codewiki_sync::{spawn_live_sync, OnSynced, WatcherConfig};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Minimal ExtractionStore adapter so we can build an orchestrator in the test.
struct StoreAdapter(Arc<StorageImpl>);

impl codewiki_extraction::ExtractionStore for StoreAdapter {
    fn store_batch(&self, batch: ExtractionBatch) -> Result<(), String> {
        codewiki_storage::ExtractionStore::store_extraction_batch(self.0.as_ref(), batch)
            .map_err(|e| e.to_string())
    }

    fn delete_file(&self, path: &Path) -> Result<(), String> {
        codewiki_storage::ExtractionStore::delete_file(self.0.as_ref(), path)
            .map_err(|e| e.to_string())
    }
}

fn make_storage(db_path: &Path) -> Arc<StorageImpl> {
    let conn = open(db_path).expect("open db");
    Arc::new(StorageImpl::new(conn, 128))
}

fn setup_project() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".codewiki")).expect("mkdir .codewiki");
    dir
}

fn content_hash_of(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let raw = std::fs::read(path).ok()?;
    let bytes = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&raw);
    Some(hex::encode(Sha256::digest(bytes)))
}

#[test]
fn watcher_reindexes_edited_file() {
    let project = setup_project();
    let root = project.path().to_path_buf();
    let codewiki_dir = root.join(".codewiki");
    let db_path = codewiki_dir.join("codewiki.db");
    let file_path = root.join("app.ts");

    std::fs::write(&file_path, b"export const a = 1;").unwrap();

    // --- Index the project once (so the file is tracked) ---
    let storage = make_storage(&db_path);
    let db: Arc<dyn SyncStore> = Arc::clone(&storage) as Arc<dyn SyncStore>;
    let extractor = Arc::new(ExtractionOrchestratorImpl::new(Arc::new(StoreAdapter(
        Arc::clone(&storage),
    ))));
    let cache = TreeCache::new(16);
    let r0 = run_sync_cycle(&db, &extractor, vec![], &cache, &codewiki_dir, &root).unwrap();
    assert_eq!(r0.files_added, 1, "initial index should track app.ts");

    let original_hash = db
        .get_stale_files()
        .unwrap()
        .into_iter()
        .find(|r| r.path == file_path)
        .unwrap()
        .content_hash;

    // --- Start the live-sync watcher (separate write connection) ---
    let sync_storage = make_storage(&db_path);
    let sync_db: Arc<dyn SyncStore> = Arc::clone(&sync_storage) as Arc<dyn SyncStore>;
    let sync_extractor = Arc::new(ExtractionOrchestratorImpl::new(Arc::new(StoreAdapter(
        Arc::clone(&sync_storage),
    ))));
    // No resolution hook needed for this test.
    let on_synced: OnSynced = Arc::new(|_r| {});

    let config = WatcherConfig { debounce_ms: 100 };
    let handle = match spawn_live_sync(root.clone(), sync_db, sync_extractor, config, on_synced) {
        Ok(h) => h,
        Err(reason) => {
            // Watch policy disabled (e.g. WSL2 /mnt drive) — skip gracefully.
            eprintln!("skipping live-sync test: {reason}");
            return;
        }
    };

    // Give the watcher a moment to register inotify/FSEvents watches.
    std::thread::sleep(Duration::from_millis(150));

    // --- Edit the file; the watcher must reindex it ---
    std::fs::write(&file_path, b"export const a = 1; export const b = 2;").unwrap();
    let expected_hash = content_hash_of(&file_path).unwrap();
    assert_ne!(
        expected_hash, original_hash,
        "test precondition: edited content must hash differently"
    );

    // Poll the DB (via the original read connection) until the hash updates.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut observed = original_hash.clone();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(rec) = db
            .get_stale_files()
            .unwrap()
            .into_iter()
            .find(|r| r.path == file_path)
        {
            observed = rec.content_hash;
            if observed == expected_hash {
                break;
            }
        }
    }

    assert_eq!(
        observed, expected_hash,
        "live-sync watcher must reindex the edited file (content hash should update)"
    );

    // --- Stop cleanly ---
    drop(handle);
}
