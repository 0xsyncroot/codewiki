// T-509 — snapshot / restore subcommands.
//
// `codewiki snapshot --output <file.db>` — uses SQLite online backup API to create
//   a portable snapshot of the current project's index.
//
// `codewiki restore <file.db>` — restores from a snapshot file.
//
// v1 best-effort notes:
// - The snapshot is a plain SQLite database file; it can be opened with any
//   SQLite client for inspection.
// - A restored database should pass `parity-runner compare rust-baseline` against
//   the same golden outputs as the live database.
// - Snapshot does NOT capture in-flight WAL pages; the WAL is checkpointed first.
// - There is no migration guarantee: a snapshot taken on schema v3 restored on a
//   binary that expects v4 will trigger the normal migration path on next open.
//
// NICE-TO-HAVE (T-512, non-gating): see PLAN.md Revision Log #14.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Run `codewiki snapshot --output <file.db>`.
///
/// Locates the project's `.codewiki/codewiki.db`, checkpoints the WAL,
/// then uses SQLite's backup API to write a portable copy to `output_path`.
pub fn run_snapshot(output: PathBuf, project_path: Option<PathBuf>) -> Result<()> {
    let root = project_path
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let db_path = root.join(".codewiki").join("codewiki.db");
    if !db_path.exists() {
        bail!(
            "no index found at {}; run `codewiki init` first",
            db_path.display()
        );
    }

    snapshot_db(&db_path, &output)?;

    println!(
        "snapshot written to {} ({} bytes)",
        output.display(),
        std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0)
    );
    println!("To restore: codewiki restore {}", output.display());
    Ok(())
}

/// Run `codewiki restore <file.db>`.
///
/// Copies the snapshot file into `.codewiki/codewiki.db` (overwriting the
/// existing index after a safety rename).  After restore the caller should
/// run `codewiki index` to catch any changes since the snapshot was taken.
pub fn run_restore(snapshot: PathBuf, project_path: Option<PathBuf>) -> Result<()> {
    if !snapshot.exists() {
        bail!("snapshot file not found: {}", snapshot.display());
    }

    let root = project_path
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let codewiki_dir = root.join(".codewiki");
    std::fs::create_dir_all(&codewiki_dir)?;

    let db_path = codewiki_dir.join("codewiki.db");
    let backup_path = codewiki_dir.join("codewiki.db.pre-restore");

    // Safety: rename existing DB before overwriting.
    if db_path.exists() {
        std::fs::rename(&db_path, &backup_path)?;
        tracing::info!(
            backup = %backup_path.display(),
            "existing index backed up before restore"
        );
    }

    restore_db(&snapshot, &db_path)?;

    println!("restored {} to {}", snapshot.display(), db_path.display());
    if backup_path.exists() {
        println!("previous index backed up at {}", backup_path.display());
    }
    println!("Tip: run `codewiki index` to update the restored index with any recent changes.");
    Ok(())
}

// ── SQLite backup API ────────────────────────────────────────────────────────

/// Copy `src_path` to `dst_path` using SQLite's online backup API.
///
/// The backup is performed page-by-page so the source database remains
/// readable during the operation.
fn snapshot_db(src_path: &Path, dst_path: &Path) -> Result<()> {
    use rusqlite::Connection;

    // Open source.
    let src = Connection::open(src_path)?;

    // Checkpoint WAL before backup so the snapshot contains all committed data.
    src.execute_batch("PRAGMA wal_checkpoint(FULL);")?;

    // Ensure destination directory exists.
    if let Some(parent) = dst_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // SQLite online backup API: src.backup(name, dst_path, progress_fn)
    src.backup(rusqlite::DatabaseName::Main, dst_path, None)?;

    tracing::info!(
        src = %src_path.display(),
        dst = %dst_path.display(),
        "snapshot complete"
    );
    Ok(())
}

/// Restore `src_path` (a snapshot) into `dst_path`.
///
/// Opens the snapshot as source and writes it to dst_path via backup API.
fn restore_db(src_path: &Path, dst_path: &Path) -> Result<()> {
    use rusqlite::Connection;

    // Ensure destination directory exists.
    if let Some(parent) = dst_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open the snapshot as source, then backup it to the destination path.
    let src = Connection::open(src_path)?;
    src.backup(rusqlite::DatabaseName::Main, dst_path, None)?;

    tracing::info!(
        src = %src_path.display(),
        dst = %dst_path.display(),
        "restore complete"
    );
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_test_db(dir: &TempDir) -> PathBuf {
        use rusqlite::Connection;
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE test_nodes (id TEXT PRIMARY KEY, name TEXT);
             INSERT INTO test_nodes VALUES ('n1', 'Node1');
             INSERT INTO test_nodes VALUES ('n2', 'Node2');",
        )
        .unwrap();
        db_path
    }

    #[test]
    fn snapshot_and_restore_roundtrip() {
        let src_dir = TempDir::new().unwrap();
        let dst_dir = TempDir::new().unwrap();
        let restore_dir = TempDir::new().unwrap();

        let src_db = make_test_db(&src_dir);
        let snapshot_path = dst_dir.path().join("snapshot.db");
        let restored_path = restore_dir.path().join("restored.db");

        // Snapshot.
        snapshot_db(&src_db, &snapshot_path).expect("snapshot should succeed");
        assert!(snapshot_path.exists(), "snapshot file should exist");

        // Restore.
        restore_db(&snapshot_path, &restored_path).expect("restore should succeed");
        assert!(restored_path.exists(), "restored DB should exist");

        // Verify content survived the round-trip.
        use rusqlite::Connection;
        let restored = Connection::open(&restored_path).unwrap();
        let count: i64 = restored
            .query_row("SELECT COUNT(*) FROM test_nodes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "restored DB should have 2 rows");
    }

    #[test]
    fn snapshot_nonexistent_source_fails() {
        let tmp = TempDir::new().unwrap();
        let result = snapshot_db(
            Path::new("/nonexistent/db.sqlite"),
            &tmp.path().join("out.db"),
        );
        assert!(result.is_err(), "should fail on missing source");
    }
}
